//! Client-side view of a supernode cluster.
//!
//! A clustered supernode advertises a signed roster of its sibling members in
//! `SUPERNODE_INFO` (see `conquerd-supernode/src/cluster.rs`). The client parses
//! and **verifies** that roster here: the signature must bind the member list to
//! the supernode identity we are already connected to and trust. Verified member
//! endpoints are the basis for failing over to another member if our current one
//! becomes unreachable.
//!
//! The wire shape and the signed canonical bytes are kept byte-for-byte in sync
//! with the supernode's `SignedClusterDescriptor`.

use serde::Deserialize;

use crate::crypto::{b64url_decode, ed25519_verify};

/// One supernode in a cluster, as advertised to clients.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClusterMember {
    /// base64url Ed25519 identity key of the member.
    pub identity_pub: String,
    /// QUIC relay address (`host:port`) — a client attach point.
    pub relay_addr: String,
    /// Dedicated supernode↔supernode address (not used by clients).
    #[serde(default)]
    pub cluster_addr: Option<String>,
    /// WebSocket signaling address (`host:port`) — the failover attach point.
    #[serde(default)]
    pub ws_addr: Option<String>,
}

/// A cluster roster signed by one member, as carried in `SUPERNODE_INFO`.
#[derive(Debug, Clone, Deserialize)]
pub struct SignedClusterDescriptor {
    pub cluster_id: String,
    /// Serialized under the `member` key (matches the supernode).
    #[serde(rename = "member")]
    pub members: Vec<ClusterMember>,
    /// base64url Ed25519 identity of the signing member.
    pub signer: String,
    /// base64url Ed25519 signature over [`Self::canonical_bytes`].
    pub signature: String,
}

/// Normalize a base64url identity key for comparison (strip padding).
fn norm(id: &str) -> &str {
    id.trim_end_matches('=')
}

impl ClusterMember {
    /// Build a WebSocket failover URL for this member from its `ws_addr`,
    /// reusing `scheme` (`ws`/`wss`) from the supernode we're connected to.
    /// Returns `None` when the member advertises no signaling address.
    pub fn ws_url(&self, scheme: &str) -> Option<String> {
        let addr = self.ws_addr.as_deref()?.trim();
        if addr.is_empty() {
            return None;
        }
        if addr.contains("://") {
            Some(addr.to_string())
        } else {
            Some(format!("{scheme}://{addr}"))
        }
    }
}

impl SignedClusterDescriptor {
    /// Deterministic bytes the signature covers. Must match the supernode's
    /// `ClusterMembership::canonical_bytes` exactly.
    fn canonical_bytes(&self) -> Vec<u8> {
        let mut ids: Vec<&str> = self.members.iter().map(|m| norm(&m.identity_pub)).collect();
        ids.sort_unstable();
        let mut out = Vec::new();
        out.extend_from_slice(b"conquerd-cluster-v1|");
        out.extend_from_slice(self.cluster_id.as_bytes());
        for id in ids {
            out.push(b'|');
            out.extend_from_slice(id.as_bytes());
        }
        out
    }

    /// Verify the signature binds this roster to `signer`.
    pub fn verify(&self) -> bool {
        let (Ok(pk), Ok(sig)) = (b64url_decode(&self.signer), b64url_decode(&self.signature))
        else {
            return false;
        };
        ed25519_verify(&pk, &sig, &self.canonical_bytes())
    }

    /// Verify the descriptor *and* that it was signed by `expected_signer` — the
    /// supernode we're connected to and already trust. Returns the member list
    /// (excluding `expected_signer` itself) on success.
    pub fn verified_members(&self, expected_signer: &str) -> Option<Vec<ClusterMember>> {
        if norm(&self.signer) != norm(expected_signer) || !self.verify() {
            return None;
        }
        Some(
            self.members
                .iter()
                .filter(|m| norm(&m.identity_pub) != norm(expected_signer))
                .cloned()
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{b64url_encode, ed25519_sign};
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn member(id: &str) -> ClusterMember {
        ClusterMember {
            identity_pub: id.to_string(),
            relay_addr: "host:3478".to_string(),
            cluster_addr: None,
            ws_addr: Some("host:34935".to_string()),
        }
    }

    /// Sign a roster the way the supernode does, for verification tests.
    fn signed(
        cluster_id: &str,
        members: Vec<ClusterMember>,
        key: &SigningKey,
    ) -> SignedClusterDescriptor {
        let signer = b64url_encode(key.verifying_key().as_bytes());
        let mut desc = SignedClusterDescriptor {
            cluster_id: cluster_id.to_string(),
            members,
            signer,
            signature: String::new(),
        };
        let sig = ed25519_sign(&key.to_bytes(), &desc.canonical_bytes()).unwrap();
        desc.signature = b64url_encode(&sig);
        desc
    }

    #[test]
    fn verifies_valid_descriptor() {
        let key = SigningKey::generate(&mut OsRng);
        let signer_id = b64url_encode(key.verifying_key().as_bytes());
        let desc = signed("c1", vec![member(&signer_id), member("PEER_B")], &key);
        assert!(desc.verify());
        let members = desc.verified_members(&signer_id).expect("verified");
        // The signer itself is excluded from the failover candidates.
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].identity_pub, "PEER_B");
    }

    #[test]
    fn rejects_wrong_expected_signer() {
        let key = SigningKey::generate(&mut OsRng);
        let signer_id = b64url_encode(key.verifying_key().as_bytes());
        let desc = signed("c1", vec![member(&signer_id)], &key);
        assert!(desc.verified_members("SOMEONE_ELSE").is_none());
    }

    #[test]
    fn rejects_tampered_roster() {
        let key = SigningKey::generate(&mut OsRng);
        let signer_id = b64url_encode(key.verifying_key().as_bytes());
        let mut desc = signed("c1", vec![member(&signer_id)], &key);
        desc.members.push(member("INJECTED"));
        assert!(!desc.verify());
    }

    #[test]
    fn ws_url_builds_from_addr_and_scheme() {
        let mut m = member("X");
        m.ws_addr = Some("node.example:34935".into());
        assert_eq!(
            m.ws_url("wss"),
            Some("wss://node.example:34935".to_string())
        );
        // Already a URL — left as-is.
        m.ws_addr = Some("ws://other:1234".into());
        assert_eq!(m.ws_url("wss"), Some("ws://other:1234".to_string()));
        // No address — no URL.
        m.ws_addr = None;
        assert_eq!(m.ws_url("ws"), None);
    }

    #[test]
    fn order_independent_verification() {
        let key = SigningKey::generate(&mut OsRng);
        let signer_id = b64url_encode(key.verifying_key().as_bytes());
        let mut desc = signed(
            "c1",
            vec![member(&signer_id), member("PEER_B"), member("PEER_C")],
            &key,
        );
        desc.members.reverse(); // signature is over sorted ids, so still valid
        assert!(desc.verify());
    }
}
