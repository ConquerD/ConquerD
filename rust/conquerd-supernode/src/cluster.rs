//! Supernode clustering — linking several supernodes into one logical node.
//!
//! A *cluster* is a set of supernodes, identified by their Ed25519 identity
//! keys, that present as a single logical supernode to peers. A peer connects
//! to any one member; members replicate room ciphertext among themselves so a
//! room scales beyond a single node. Members never see plaintext (E2E is the
//! client's concern) — clustering is purely a fan-out/replication fabric.
//!
//! This module owns the **membership** layer (B.1): the operator-declared
//! `[cluster]` section of `supernode.toml`, the runtime membership queries used
//! to authenticate peer members and route replication, and a signed descriptor
//! advertised to clients so they can treat any member as equivalent. The
//! intra-cluster transport, subscription table, and replication build on top of
//! this in later steps.
//!
//! On-disk shape (`<data_dir>/supernode.toml`):
//!
//! ```toml
//! [cluster]
//! cluster_id = "acme-us"
//!
//! [[cluster.member]]
//! identity_pub = "BASE64URL_ED25519_PUBKEY"
//! relay_addr   = "node-a.acme.example:3478"
//! ws_addr      = "node-a.acme.example:34935"   # optional
//! ```
//!
//! Every member lists the **full** member set (operators coordinate the list);
//! a member authenticates an inbound supernode↔supernode link by checking the
//! peer's identity key is in this set. The list is identical on every member,
//! so any member can sign and advertise it to clients.

use serde::{Deserialize, Serialize};

use crate::crypto::b64url_encode;
use crate::identity::Identity;

/// One supernode in a cluster, as declared in `supernode.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterMember {
    /// base64url Ed25519 identity key — the member's trust anchor and the CN of
    /// its mTLS cert on the intra-cluster QUIC link.
    pub identity_pub: String,
    /// QUIC relay address (`host:port`) advertised to clients as an attach point.
    pub relay_addr: String,
    /// Dedicated QUIC address (`host:port`) for the supernode↔supernode cluster
    /// link. Absent ⇒ this member cannot be dialed for replication (a member
    /// that only accepts client traffic). Kept separate from `relay_addr` so
    /// cluster control traffic and client relay traffic don't share a listener.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_addr: Option<String>,
    /// Optional WebSocket signaling address (`host:port`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_addr: Option<String>,
}

impl ClusterMember {
    /// Identity key with any base64url padding stripped, matching the un-padded
    /// form the relay and signaling layers use for peer lookups.
    fn normalized_id(&self) -> &str {
        self.identity_pub.trim_end_matches('=')
    }
}

/// The `[cluster]` section of the manifest. Absent ⇒ standalone supernode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterConfig {
    /// Stable cluster identifier, shared by every member.
    pub cluster_id: String,
    /// Full member set, including this node. Order is not significant.
    #[serde(default, rename = "member")]
    pub members: Vec<ClusterMember>,
}

/// Normalize a base64url identity key for membership comparison (strip padding).
fn norm(id: &str) -> &str {
    id.trim_end_matches('=')
}

impl ClusterConfig {
    /// Validate operator config: non-empty id, no duplicate members, and (when a
    /// member set is given) that this node appears in it.
    pub fn validate(&self, self_identity_pub: &str) -> Result<(), String> {
        if self.cluster_id.trim().is_empty() {
            return Err("cluster.cluster_id must not be empty".into());
        }
        let mut seen = std::collections::HashSet::new();
        for m in &self.members {
            if m.relay_addr.trim().is_empty() {
                return Err(format!(
                    "cluster member {} has empty relay_addr",
                    m.identity_pub
                ));
            }
            if !seen.insert(m.normalized_id()) {
                return Err(format!("duplicate cluster member: {}", m.identity_pub));
            }
        }
        if !self.members.is_empty() && !self.contains(self_identity_pub) {
            return Err("this node's identity is not listed in cluster.member".into());
        }
        Ok(())
    }

    /// Whether `identity_pub` (padded or not) is a declared cluster member.
    pub fn contains(&self, identity_pub: &str) -> bool {
        let want = norm(identity_pub);
        self.members.iter().any(|m| m.normalized_id() == want)
    }
}

/// Runtime view of this node's place in its cluster.
#[derive(Debug, Clone)]
pub struct ClusterMembership {
    config: ClusterConfig,
    /// This node's own base64url identity (normalized).
    self_id: String,
}

impl ClusterMembership {
    /// Build from validated config and this node's identity.
    pub fn new(config: ClusterConfig, self_identity_pub: &str) -> Self {
        Self {
            config,
            self_id: norm(self_identity_pub).to_string(),
        }
    }

    pub fn cluster_id(&self) -> &str {
        &self.config.cluster_id
    }

    /// Total members declared (includes self).
    pub fn member_count(&self) -> usize {
        self.config.members.len()
    }

    /// Whether `identity_pub` is a member (authn gate for an inbound peer link).
    #[allow(dead_code)] // intra-cluster link authentication (B.2)
    pub fn is_member(&self, identity_pub: &str) -> bool {
        self.config.contains(identity_pub)
    }

    /// Whether `identity_pub` is *another* member (not this node).
    #[allow(dead_code)] // intra-cluster link authentication (B.2)
    pub fn is_peer_member(&self, identity_pub: &str) -> bool {
        let id = norm(identity_pub);
        id != self.self_id && self.config.contains(id)
    }

    /// The other members this node should open replication links to.
    #[allow(dead_code)] // dialed when intra-cluster links are established (B.2)
    pub fn peers(&self) -> impl Iterator<Item = &ClusterMember> {
        let self_id = self.self_id.clone();
        self.config
            .members
            .iter()
            .filter(move |m| m.normalized_id() != self_id)
    }

    /// This node's own entry in the roster, if listed.
    #[allow(dead_code)] // used to resolve the local cluster_addr (B.2)
    pub fn self_member(&self) -> Option<&ClusterMember> {
        self.config
            .members
            .iter()
            .find(|m| m.normalized_id() == self.self_id)
    }

    /// Deterministic bytes committed by a [`SignedClusterDescriptor`]: the
    /// cluster id followed by each member's identity sorted, so every member
    /// signs the identical roster regardless of declaration order.
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut ids: Vec<&str> = self
            .config
            .members
            .iter()
            .map(|m| m.normalized_id())
            .collect();
        ids.sort_unstable();
        let mut out = Vec::new();
        out.extend_from_slice(b"conquerd-cluster-v1|");
        out.extend_from_slice(self.config.cluster_id.as_bytes());
        for id in ids {
            out.push(b'|');
            out.extend_from_slice(id.as_bytes());
        }
        out
    }

    /// Sign this node's view of the roster with its identity key.
    pub fn sign(&self, identity: &Identity) -> SignedClusterDescriptor {
        let sig = identity.sign(&self.canonical_bytes());
        SignedClusterDescriptor {
            cluster_id: self.config.cluster_id.clone(),
            members: self.config.members.clone(),
            signer: identity.public_id(),
            signature: b64url_encode(&sig),
        }
    }
}

/// A cluster roster signed by one member, advertised to clients. The client
/// already trusts the supernode identity it connected to, so verifying the
/// signature against `signer` is sufficient to accept the member list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedClusterDescriptor {
    pub cluster_id: String,
    #[serde(rename = "member")]
    pub members: Vec<ClusterMember>,
    /// base64url Ed25519 identity of the signing member.
    pub signer: String,
    /// base64url Ed25519 signature over the canonical roster bytes.
    pub signature: String,
}

impl SignedClusterDescriptor {
    /// Verify the signature binds this roster to `signer`. This is the
    /// canonical, tested verifier; the supernode only ever *signs*, so the
    /// check is exercised by clients (and these tests) rather than the bin.
    #[allow(dead_code)]
    pub fn verify(&self) -> bool {
        let membership = ClusterMembership::new(
            ClusterConfig {
                cluster_id: self.cluster_id.clone(),
                members: self.members.clone(),
            },
            &self.signer,
        );
        let (Ok(pk), Ok(sig)) = (
            crate::crypto::b64url_decode(&self.signer),
            crate::crypto::b64url_decode(&self.signature),
        ) else {
            return false;
        };
        Identity::verify_with_pub(&pk, &sig, &membership.canonical_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, addr: &str) -> ClusterMember {
        ClusterMember {
            identity_pub: id.to_string(),
            relay_addr: addr.to_string(),
            cluster_addr: Some(addr.to_string()),
            ws_addr: None,
        }
    }

    fn config(self_id: &str) -> ClusterConfig {
        ClusterConfig {
            cluster_id: "test-cluster".to_string(),
            members: vec![
                member(self_id, "a.example:3478"),
                member("PEER_B", "b.example:3478"),
                member("PEER_C", "c.example:3478"),
            ],
        }
    }

    #[test]
    fn membership_queries() {
        let m = ClusterMembership::new(config("SELF"), "SELF");
        assert_eq!(m.member_count(), 3);
        assert!(m.is_member("SELF"));
        assert!(m.is_member("PEER_B"));
        assert!(!m.is_member("STRANGER"));
        assert!(m.is_peer_member("PEER_B"));
        assert!(!m.is_peer_member("SELF"));
        let peers: Vec<&str> = m.peers().map(|p| p.identity_pub.as_str()).collect();
        assert_eq!(peers, vec!["PEER_B", "PEER_C"]);
    }

    #[test]
    fn membership_ignores_base64url_padding() {
        // Relay uses un-padded ids; config may carry padded ones (or vice-versa).
        let cfg = ClusterConfig {
            cluster_id: "c".to_string(),
            members: vec![member("ABC==", "a:1"), member("DEF", "b:2")],
        };
        let m = ClusterMembership::new(cfg, "ABC");
        assert!(m.is_member("ABC"));
        assert!(m.is_member("ABC=="));
        assert!(m.is_peer_member("DEF=="));
    }

    #[test]
    fn validate_rejects_empty_id_and_dupes_and_missing_self() {
        assert!(ClusterConfig {
            cluster_id: " ".into(),
            members: vec![]
        }
        .validate("SELF")
        .is_err());

        let dupe = ClusterConfig {
            cluster_id: "c".into(),
            members: vec![member("X", "a:1"), member("X=", "b:2")],
        };
        assert!(dupe.validate("X").is_err());

        let missing_self = config("SELF");
        assert!(missing_self.validate("NOT_A_MEMBER").is_err());

        assert!(config("SELF").validate("SELF").is_ok());
    }

    #[test]
    fn empty_member_list_is_valid_standalone_bootstrap() {
        // A cluster_id with no members yet (single-node bootstrap) is allowed;
        // the self-membership check only applies once a roster is declared.
        let cfg = ClusterConfig {
            cluster_id: "solo".into(),
            members: vec![],
        };
        assert!(cfg.validate("SELF").is_ok());
    }

    #[test]
    fn signed_descriptor_round_trips_and_is_order_independent() {
        let id = Identity::generate();
        let self_pub = id.public_id();

        let mut cfg = config(&self_pub);
        let m1 = ClusterMembership::new(cfg.clone(), &self_pub);
        let signed = m1.sign(&id);
        assert!(signed.verify());

        // Reordering the roster yields the same signed commitment.
        cfg.members.reverse();
        let m2 = ClusterMembership::new(cfg, &self_pub);
        assert_eq!(m1.canonical_bytes(), m2.canonical_bytes());
        assert!(m2.sign(&id).verify());
    }

    #[test]
    fn tampered_descriptor_fails_verification() {
        let id = Identity::generate();
        let self_pub = id.public_id();
        let mut signed = ClusterMembership::new(config(&self_pub), &self_pub).sign(&id);
        signed.members.push(member("INJECTED", "evil:1"));
        assert!(!signed.verify());

        let mut signed2 = ClusterMembership::new(config(&self_pub), &self_pub).sign(&id);
        signed2.cluster_id = "other".into();
        assert!(!signed2.verify());
    }
}
