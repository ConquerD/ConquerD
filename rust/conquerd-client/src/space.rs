//! Space Merkle tree — Layer 1 authenticated room tree (client-side).
//!
//! A Space is a tree of nodes (Server → Room in v1) replacing the flat
//! per-supernode room namespace. The node set hashes into a single signed
//! **epoch root** ([`SignedSpaceRoot`]); any leaf is provable with a compact
//! [`SpaceInclusionProof`]. This module is Layer 1 only — node records, RFC 6962
//! hashing, owner-signed roots, and proof verification. Layer 2 key hierarchy
//! and remaining Space work are tracked in `backlog.md`.
//!
//! Leaves are sorted by `node_id` so the root is a pure function of the node
//! set — two parties with the same set compute the same root (a reconciliation
//! digest), unlike an append-only log.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::crypto::{b64url_decode, ed25519_verify, sha256, sha256_hex};

/// Domain-separation label for leaf hashing. Bumping it (v2, …) is a breaking
/// change to every stored proof; adding/removing/retyping a leaf field requires
/// the bump (hash input cannot absorb unknown fields).
pub const SPACE_LEAF_LABEL: &str = "conquerd-space-leaf-v1";
/// Domain-separation label for the signed root.
pub const SPACE_ROOT_LABEL: &str = "conquerd-space-root-v1";
/// Domain-separation label for an owner-signed admission grant.
pub const SPACE_GRANT_LABEL: &str = "conquerd-space-grant-v1";
/// Defensive verifier bound on inclusion-proof depth (≈ 4 billion nodes).
pub const MAX_PROOF_DEPTH: usize = 32;
/// Schema version carried by wire structs.
pub const SPACE_SCHEMA: u32 = 1;

// ---------------------------------------------------------------------------
// Node record
// ---------------------------------------------------------------------------

/// One node in a Space tree. `inherit` / `key_commit` are reserved for the
/// Layer-2 key hierarchy and stay `false` / `""` in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceNode {
    /// Content-derived id, unique per position in the tree ([`derive_node_id`]).
    pub node_id: String,
    /// Parent node id; `""` for the Space root (the Server node).
    pub parent_id: String,
    /// `"server"` | `"room"` (v1); `"closet"` is a later additive value.
    pub kind: String,
    pub name: String,
    /// `"public"` | `"private"` — same vocabulary as `RoomType`.
    pub node_type: String,
    /// base64url Ed25519 of the node creator.
    pub owner_pub: String,
    /// `""` (inherit) | `"owner"` | `"members"` — owner-controlled invite policy.
    #[serde(default)]
    pub invite_policy: String,
    /// Layer 2: derive this node's key from the parent's. Reserved.
    #[serde(default)]
    pub inherit: bool,
    /// Layer 2: commitment to the node's epoch key. Reserved (`""`).
    #[serde(default)]
    pub key_commit: String,
}

impl SpaceNode {
    /// Alphabetically-sorted JSON of every field — the canonical leaf body.
    /// Built from an explicit `BTreeMap` so the ordering never depends on
    /// serde_json's map feature flags.
    fn canonical_json(&self) -> Vec<u8> {
        let mut m: BTreeMap<&str, Value> = BTreeMap::new();
        m.insert("node_id", json!(self.node_id));
        m.insert("parent_id", json!(self.parent_id));
        m.insert("kind", json!(self.kind));
        m.insert("name", json!(self.name));
        m.insert("node_type", json!(self.node_type));
        m.insert("owner_pub", json!(self.owner_pub));
        m.insert("invite_policy", json!(self.invite_policy));
        m.insert("inherit", json!(self.inherit));
        m.insert("key_commit", json!(self.key_commit));
        serde_json::to_vec(&m).unwrap_or_default()
    }

    /// RFC 6962 leaf hash: `SHA-256(0x00 ‖ "label|" ‖ sorted_json)`.
    pub fn leaf_hash(&self) -> [u8; 32] {
        let canonical = self.canonical_json();
        let mut input = Vec::with_capacity(1 + SPACE_LEAF_LABEL.len() + 1 + canonical.len());
        input.push(0x00); // RFC 6962 leaf prefix
        input.extend_from_slice(SPACE_LEAF_LABEL.as_bytes());
        input.push(b'|');
        input.extend_from_slice(&canonical);
        sha256(&input)
    }
}

/// Content-derived node id: `SHA-256(parent ‖ ":" ‖ owner ‖ ":" ‖ name)[..16]`.
/// Extends `derive_room_id` with the parent so ids are unique per tree position
/// and stable across supernodes.
pub fn derive_node_id(parent_id: &str, owner_pub: &str, name: &str) -> String {
    let input = format!("{parent_id}:{owner_pub}:{name}");
    sha256_hex(input.as_bytes())[..16].to_string()
}

// ---------------------------------------------------------------------------
// Merkle hashing (RFC 6962)
// ---------------------------------------------------------------------------

/// RFC 6962 interior hash: `SHA-256(0x01 ‖ left ‖ right)`.
fn interior_hash(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut input = Vec::with_capacity(1 + 64);
    input.push(0x01);
    input.extend_from_slice(left);
    input.extend_from_slice(right);
    sha256(&input)
}

/// Largest power of two strictly less than `n` (n ≥ 2).
fn largest_pow2_below(n: usize) -> usize {
    debug_assert!(n >= 2);
    let mut k = 1;
    while k << 1 < n {
        k <<= 1;
    }
    k
}

/// RFC 6962 Merkle Tree Hash over already-`leaf_hash`ed, sorted leaves.
fn merkle_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves.len() {
        0 => sha256(&[]), // empty tree = SHA-256("")
        1 => leaves[0],
        n => {
            let k = largest_pow2_below(n);
            let left = merkle_root(&leaves[..k]);
            let right = merkle_root(&leaves[k..]);
            interior_hash(&left, &right)
        }
    }
}

/// RFC 6962 audit path for the leaf at `m` in a tree of `n` leaves, ordered
/// leaf → root (deepest sibling first).
fn audit_path(m: usize, leaves: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let n = leaves.len();
    if n <= 1 {
        return Vec::new();
    }
    let k = largest_pow2_below(n);
    if m < k {
        let mut p = audit_path(m, &leaves[..k]);
        p.push(merkle_root(&leaves[k..]));
        p
    } else {
        let mut p = audit_path(m - k, &leaves[k..]);
        p.push(merkle_root(&leaves[..k]));
        p
    }
}

/// Fold an audit `path` (leaf → root) back to a root hash. Mirrors [`audit_path`]:
/// the top-level sibling is consumed last.
fn root_from_audit(leaf: [u8; 32], m: usize, n: usize, path: &[[u8; 32]]) -> Option<[u8; 32]> {
    if n == 0 || m >= n {
        return None;
    }
    if n == 1 {
        return if path.is_empty() { Some(leaf) } else { None };
    }
    let (top, rest) = path.split_last()?;
    let k = largest_pow2_below(n);
    if m < k {
        let left = root_from_audit(leaf, m, k, rest)?;
        Some(interior_hash(&left, top))
    } else {
        let right = root_from_audit(leaf, m - k, n - k, rest)?;
        Some(interior_hash(top, &right))
    }
}

// ---------------------------------------------------------------------------
// Signed root + inclusion proof
// ---------------------------------------------------------------------------

/// An owner-signed Space treehead for one epoch. Mirrors the
/// `SignedClusterDescriptor` pattern: domain-separated deterministic bytes,
/// Ed25519 over them, verified with the owner's public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedSpaceRoot {
    pub schema: u32,
    /// `node_id` of the Server (root) node.
    pub space_id: String,
    pub epoch: u64,
    /// hex SHA-256 Merkle root.
    pub root_hash: String,
    pub node_count: u32,
    /// unix seconds.
    pub issued_at: u64,
    /// base64url Ed25519 of the Space owner.
    pub signer: String,
    /// base64url Ed25519 over [`Self::signing_bytes`].
    #[serde(default)]
    pub signature: String,
}

impl SignedSpaceRoot {
    /// Deterministic bytes the signature covers: `"label|" ‖ sorted_json(all
    /// fields except signature)`.
    fn signing_bytes(&self) -> Vec<u8> {
        let mut m: BTreeMap<&str, Value> = BTreeMap::new();
        m.insert("schema", json!(self.schema));
        m.insert("space_id", json!(self.space_id));
        m.insert("epoch", json!(self.epoch));
        m.insert("root_hash", json!(self.root_hash));
        m.insert("node_count", json!(self.node_count));
        m.insert("issued_at", json!(self.issued_at));
        m.insert("signer", json!(self.signer));
        let body = serde_json::to_vec(&m).unwrap_or_default();
        let mut out = Vec::with_capacity(SPACE_ROOT_LABEL.len() + 1 + body.len());
        out.extend_from_slice(SPACE_ROOT_LABEL.as_bytes());
        out.push(b'|');
        out.extend_from_slice(&body);
        out
    }

    /// Verify the signature binds this root to `signer`.
    pub fn verify(&self) -> bool {
        let (Ok(pk), Ok(sig)) = (b64url_decode(&self.signer), b64url_decode(&self.signature))
        else {
            return false;
        };
        ed25519_verify(&pk, &sig, &self.signing_bytes())
    }
}

/// A compact proof that `node` is a leaf of a Space at a given epoch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceInclusionProof {
    pub schema: u32,
    /// The leaf being proven (the verifier recomputes its `leaf_hash`).
    pub node: SpaceNode,
    /// Position in the sorted-leaf order.
    pub leaf_index: u32,
    /// hex sibling hashes, leaf → root.
    pub path: Vec<String>,
    /// Root epoch this proof was built against.
    pub epoch: u64,
}

impl SpaceInclusionProof {
    /// Verify this proof against a `root` the caller already trusts (i.e. whose
    /// owner signature it has checked). Confirms the epoch matches, the depth is
    /// bounded, and the recomputed root equals `root.root_hash`.
    pub fn verify_against(&self, root: &SignedSpaceRoot) -> bool {
        if self.epoch != root.epoch {
            return false;
        }
        if self.path.len() > MAX_PROOF_DEPTH {
            return false;
        }
        let n = root.node_count as usize;
        let m = self.leaf_index as usize;
        if m >= n {
            return false;
        }
        let path: Option<Vec<[u8; 32]>> = self.path.iter().map(|h| decode_hash(h)).collect();
        let Some(path) = path else {
            return false;
        };
        let leaf = self.node.leaf_hash();
        match root_from_audit(leaf, m, n, &path) {
            Some(computed) => hex::encode(computed) == root.root_hash,
            None => false,
        }
    }
}

/// Decode a 32-byte hex hash.
fn decode_hash(hex_str: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_str).ok()?;
    bytes.try_into().ok()
}

/// An owner-signed capability admitting `grantee_pub` to a specific node.
///
/// Presented with a [`SpaceInclusionProof`] on a private-node join: the proof
/// shows the node exists in the signed Space, the grant shows this peer may
/// enter it. One owner signature per grant (Layer 1); the Layer-2 upgrade swaps
/// the Ed25519 signature for a token MAC'd under the node key without changing
/// the admission call shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceGrant {
    pub schema: u32,
    /// Node this grant admits to.
    pub node_id: String,
    /// Grant is valid while the root epoch is ≥ this and the node still included.
    pub epoch: u64,
    /// base64url Ed25519 of the admitted peer (checked against the connection).
    pub grantee_pub: String,
    /// unix seconds; `0` = until revoked by epoch exclusion.
    pub expires_at: u64,
    /// base64url Ed25519 of the Space owner over [`Self::signing_bytes`].
    #[serde(default)]
    pub signature: String,
}

impl SpaceGrant {
    /// Deterministic bytes the signature covers: `"label|" ‖ sorted_json(all
    /// fields except signature)`. Mirrors [`SignedSpaceRoot::signing_bytes`].
    fn signing_bytes(&self) -> Vec<u8> {
        let mut m: BTreeMap<&str, Value> = BTreeMap::new();
        m.insert("schema", json!(self.schema));
        m.insert("node_id", json!(self.node_id));
        m.insert("epoch", json!(self.epoch));
        m.insert("grantee_pub", json!(self.grantee_pub));
        m.insert("expires_at", json!(self.expires_at));
        let body = serde_json::to_vec(&m).unwrap_or_default();
        let mut out = Vec::with_capacity(SPACE_GRANT_LABEL.len() + 1 + body.len());
        out.extend_from_slice(SPACE_GRANT_LABEL.as_bytes());
        out.push(b'|');
        out.extend_from_slice(&body);
        out
    }

    /// Verify the grant's signature binds it to `owner_pub` (the Space signer).
    /// Does **not** check expiry/epoch/grantee — the admitting node does that
    /// against the connection identity and the current root.
    pub fn verify(&self, owner_pub: &str) -> bool {
        let (Ok(pk), Ok(sig)) = (b64url_decode(owner_pub), b64url_decode(&self.signature)) else {
            return false;
        };
        ed25519_verify(&pk, &sig, &self.signing_bytes())
    }
}

// ---------------------------------------------------------------------------
// Space builder (owner side)
// ---------------------------------------------------------------------------

/// The owner's mutable view of a Space: the node set plus the current epoch.
/// Holds no key material (Layer 1). Persisted in the room store so the owner can
/// rebuild and re-sign roots across sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Space {
    /// `node_id` of the Server (root) node — also the Space id.
    pub space_id: String,
    /// base64url Ed25519 of the owner.
    pub owner_pub: String,
    pub epoch: u64,
    /// All nodes; order is irrelevant (leaves are sorted by `node_id` on build).
    pub nodes: Vec<SpaceNode>,
}

impl Space {
    /// Start a Space rooted at a Server node owned by `owner_pub`. `server_name`
    /// is usually the supernode id or a display name.
    pub fn new_server(owner_pub: &str, server_name: &str) -> Self {
        let space_id = derive_node_id("", owner_pub, server_name);
        let server = SpaceNode {
            node_id: space_id.clone(),
            parent_id: String::new(),
            kind: "server".to_owned(),
            name: server_name.to_owned(),
            node_type: "public".to_owned(),
            owner_pub: owner_pub.to_owned(),
            invite_policy: "owner".to_owned(),
            inherit: false,
            key_commit: String::new(),
        };
        Self {
            space_id,
            owner_pub: owner_pub.to_owned(),
            epoch: 0,
            nodes: vec![server],
        }
    }

    /// Add or replace a node and bump the epoch. Returns the node id.
    pub fn upsert_node(&mut self, node: SpaceNode) -> String {
        let id = node.node_id.clone();
        if let Some(existing) = self.nodes.iter_mut().find(|n| n.node_id == id) {
            *existing = node;
        } else {
            self.nodes.push(node);
        }
        self.epoch += 1;
        id
    }

    /// Remove a node by id and bump the epoch. Returns `true` if it existed.
    pub fn remove_node(&mut self, node_id: &str) -> bool {
        let before = self.nodes.len();
        self.nodes.retain(|n| n.node_id != node_id);
        let removed = self.nodes.len() != before;
        if removed {
            self.epoch += 1;
        }
        removed
    }

    /// Leaf hashes in the canonical (node_id-sorted) order.
    fn sorted_leaves(&self) -> Vec<[u8; 32]> {
        let mut sorted: Vec<&SpaceNode> = self.nodes.iter().collect();
        sorted.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        sorted.iter().map(|n| n.leaf_hash()).collect()
    }

    /// hex Merkle root of the current node set.
    pub fn root_hash(&self) -> String {
        hex::encode(merkle_root(&self.sorted_leaves()))
    }

    /// Build a [`SignedSpaceRoot`] for the current epoch. `sign` produces an
    /// Ed25519 signature over the given bytes (e.g. `|b| identity.sign(b)`);
    /// keeping it a closure avoids coupling this module to `Identity`.
    pub fn signed_root(&self, issued_at: u64, sign: impl Fn(&[u8]) -> Vec<u8>) -> SignedSpaceRoot {
        let mut root = SignedSpaceRoot {
            schema: SPACE_SCHEMA,
            space_id: self.space_id.clone(),
            epoch: self.epoch,
            root_hash: self.root_hash(),
            node_count: self.nodes.len() as u32,
            issued_at,
            signer: self.owner_pub.clone(),
            signature: String::new(),
        };
        let sig = sign(&root.signing_bytes());
        root.signature = crate::crypto::b64url_encode(&sig);
        root
    }

    /// Build an inclusion proof for `node_id` against the current epoch, or
    /// `None` if the node is absent.
    pub fn prove(&self, node_id: &str) -> Option<SpaceInclusionProof> {
        let mut sorted: Vec<&SpaceNode> = self.nodes.iter().collect();
        sorted.sort_by(|a, b| a.node_id.cmp(&b.node_id));
        let idx = sorted.iter().position(|n| n.node_id == node_id)?;
        let leaves: Vec<[u8; 32]> = sorted.iter().map(|n| n.leaf_hash()).collect();
        let path = audit_path(idx, &leaves)
            .into_iter()
            .map(hex::encode)
            .collect();
        Some(SpaceInclusionProof {
            schema: SPACE_SCHEMA,
            node: sorted[idx].clone(),
            leaf_index: idx as u32,
            path,
            epoch: self.epoch,
        })
    }

    /// Owner: sign a [`SpaceGrant`] admitting `grantee_pub` to `node_id` at the
    /// current epoch. `expires_at` is unix seconds (`0` = until epoch exclusion).
    /// `sign` is the owner's Ed25519 signer (e.g. `|b| identity.sign(b)`).
    pub fn grant(
        &self,
        node_id: &str,
        grantee_pub: &str,
        expires_at: u64,
        sign: impl Fn(&[u8]) -> Vec<u8>,
    ) -> SpaceGrant {
        let mut grant = SpaceGrant {
            schema: SPACE_SCHEMA,
            node_id: node_id.to_owned(),
            epoch: self.epoch,
            grantee_pub: grantee_pub.to_owned(),
            expires_at,
            signature: String::new(),
        };
        let sig = sign(&grant.signing_bytes());
        grant.signature = crate::crypto::b64url_encode(&sig);
        grant
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{b64url_encode, ed25519_sign};
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn node(parent: &str, owner: &str, name: &str, kind: &str) -> SpaceNode {
        SpaceNode {
            node_id: derive_node_id(parent, owner, name),
            parent_id: parent.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            node_type: "private".to_owned(),
            owner_pub: owner.to_owned(),
            invite_policy: String::new(),
            inherit: false,
            key_commit: String::new(),
        }
    }

    /// A fully-literal node set (no derivation) so the canonical encoding is
    /// identical across the client and supernode `space.rs` copies. The KAT
    /// below must assert the SAME hex in both crates — that is what proves the
    /// two independent implementations can never silently diverge.
    fn kat_nodes() -> Vec<SpaceNode> {
        let mk = |id: &str, parent: &str, kind: &str, name: &str, ntype: &str| SpaceNode {
            node_id: id.to_owned(),
            parent_id: parent.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            node_type: ntype.to_owned(),
            owner_pub: "KAT-OWNER-PUB".to_owned(),
            invite_policy: String::new(),
            inherit: false,
            key_commit: String::new(),
        };
        vec![
            mk("srv0", "", "server", "Srv", "public"),
            mk("r001", "srv0", "room", "Alpha", "public"),
            mk("r002", "srv0", "room", "Beta", "private"),
        ]
    }

    #[test]
    fn kat_canonical_leaf_and_root_hashes() {
        // Cross-crate known-answer test. If either hash changes, the leaf/root
        // canonical encoding changed — bump the label and update BOTH crates.
        let nodes = kat_nodes();
        assert_eq!(
            hex::encode(nodes[0].leaf_hash()),
            "fda231a1e7510baeb453f78438324bb5943834643f01fca2b6fe5b9cf1c8fcc6",
            "canonical leaf hash drifted"
        );
        let space = Space {
            space_id: "srv0".to_owned(),
            owner_pub: "KAT-OWNER-PUB".to_owned(),
            epoch: 7,
            nodes,
        };
        assert_eq!(
            space.root_hash(),
            "4f083fc161505e259b9b1397bc1b181368cff87ea411366c4dd43ee73f4a452d",
            "canonical root hash drifted"
        );
    }

    #[test]
    fn space_grant_signs_and_verifies() {
        let key = SigningKey::generate(&mut OsRng);
        let owner = b64url_encode(key.verifying_key().as_bytes());
        let space = Space::new_server(&owner, "srv");
        let grant = space.grant("room-1", "grantee-pub", 0, |b| {
            ed25519_sign(&key.to_bytes(), b).unwrap()
        });
        assert!(grant.verify(&owner));
        assert_eq!(grant.node_id, "room-1");
        assert_eq!(grant.grantee_pub, "grantee-pub");

        // Signed by a different owner → fails against the claimed owner.
        let attacker = SigningKey::generate(&mut OsRng);
        let bad = space.grant("room-1", "grantee-pub", 0, |b| {
            ed25519_sign(&attacker.to_bytes(), b).unwrap()
        });
        assert!(!bad.verify(&owner));

        // Tampering any signed field breaks verification.
        let mut tampered = grant.clone();
        tampered.grantee_pub = "someone-else".to_owned();
        assert!(!tampered.verify(&owner));
        let mut tampered2 = grant.clone();
        tampered2.node_id = "room-2".to_owned();
        assert!(!tampered2.verify(&owner));
    }

    #[test]
    fn node_id_is_deterministic_and_position_bound() {
        let a = derive_node_id("srv", "ownerpub", "General");
        let b = derive_node_id("srv", "ownerpub", "General");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        // Different parent / owner / name each change the id.
        assert_ne!(a, derive_node_id("other", "ownerpub", "General"));
        assert_ne!(a, derive_node_id("srv", "other", "General"));
        assert_ne!(a, derive_node_id("srv", "ownerpub", "Random"));
    }

    #[test]
    fn root_is_order_independent() {
        let owner = "owner-pub";
        let server = derive_node_id("", owner, "srv");
        let n1 = node(&server, owner, "General", "room");
        let n2 = node(&server, owner, "Random", "room");
        let n3 = node(&server, owner, "Voice", "room");

        let mut a = Space::new_server(owner, "srv");
        a.nodes.extend([n1.clone(), n2.clone(), n3.clone()]);
        let mut b = Space::new_server(owner, "srv");
        b.nodes.extend([n3, n1, n2]); // different insertion order
        assert_eq!(a.root_hash(), b.root_hash());
    }

    #[test]
    fn single_and_empty_tree_roots() {
        // Empty = SHA-256("").
        assert_eq!(merkle_root(&[]), sha256(&[]));
        // Single leaf's tree hash is the leaf hash itself.
        let owner = "o";
        let n = node("", owner, "srv", "server");
        assert_eq!(merkle_root(&[n.leaf_hash()]), n.leaf_hash());
    }

    #[test]
    fn inclusion_proof_verifies_for_every_node() {
        let key = SigningKey::generate(&mut OsRng);
        let owner = b64url_encode(key.verifying_key().as_bytes());

        let mut space = Space::new_server(&owner, "srv");
        // Add a handful of rooms so the tree is >1 level deep.
        for name in ["General", "Random", "Voice", "Dev", "Off-topic"] {
            let n = node(&space.space_id, &owner, name, "room");
            space.upsert_node(n);
        }
        let root = space.signed_root(1000, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        assert!(root.verify());
        assert_eq!(root.node_count as usize, space.nodes.len());

        for n in &space.nodes {
            let proof = space.prove(&n.node_id).expect("node present");
            assert!(proof.verify_against(&root), "proof failed for {}", n.name);
        }
    }

    #[test]
    fn tampered_proof_and_root_are_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let owner = b64url_encode(key.verifying_key().as_bytes());
        let mut space = Space::new_server(&owner, "srv");
        for name in ["General", "Random", "Voice"] {
            let n = node(&space.space_id, &owner, name, "room");
            space.upsert_node(n);
        }
        let root = space.signed_root(1, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        let mut proof = space.prove(&space.space_id).unwrap();

        // A tampered leaf field breaks the recomputed root.
        let mut bad_node = proof.clone();
        bad_node.node.name = "Hacked".to_owned();
        assert!(!bad_node.verify_against(&root));

        // A wrong epoch is rejected.
        proof.epoch = root.epoch + 1;
        assert!(!proof.verify_against(&root));

        // A flipped signature fails verification.
        let mut bad_root = root.clone();
        bad_root.root_hash = "00".repeat(32);
        assert!(!space
            .prove(&space.space_id)
            .unwrap()
            .verify_against(&bad_root));
    }

    #[test]
    fn signed_root_rejects_foreign_signer() {
        let key = SigningKey::generate(&mut OsRng);
        let attacker = SigningKey::generate(&mut OsRng);
        let owner = b64url_encode(key.verifying_key().as_bytes());
        let space = Space::new_server(&owner, "srv");
        // Signed by the attacker but claiming the owner as signer → fails.
        let mut root = space.signed_root(1, |b| ed25519_sign(&attacker.to_bytes(), b).unwrap());
        assert!(!root.verify());
        // Correctly signed verifies.
        root = space.signed_root(1, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        assert!(root.verify());
    }

    #[test]
    fn over_deep_proof_is_rejected() {
        let key = SigningKey::generate(&mut OsRng);
        let owner = b64url_encode(key.verifying_key().as_bytes());
        let space = Space::new_server(&owner, "srv");
        let root = space.signed_root(1, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        let mut proof = space.prove(&space.space_id).unwrap();
        proof.path = vec!["00".repeat(32); MAX_PROOF_DEPTH + 1];
        assert!(!proof.verify_against(&root));
    }
}
