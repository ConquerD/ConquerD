//! Peer store — local, client-owned storage for trusted peers.
//!
//! Data is stored on disk as an AES-256-GCM envelope keyed by an HKDF
//! subkey of the user's Identity. Existing `peers.dat` files can be read
//! without migration.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

use crate::avatar_config::AvatarConfig;
use crate::crypto::{decrypt_blob, encrypt_blob};
use crate::error::Result;
use crate::identity::Identity;

pub const PEER_STORE_FILE: &str = "peers.dat";
pub const PEER_STORE_LABEL: &str = "conquerd-store/peers/v1";

// ---------------------------------------------------------------------------
// PeerRecord
// ---------------------------------------------------------------------------

/// A single trusted-peer entry. Existing `peers.dat` files deserialise
/// without migration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PeerRecord {
    pub peer_id: String,
    pub identity_pub: String,
    #[serde(default)]
    pub relay_hints: Vec<String>,
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub is_supernode: bool,
    /// Set when the invite payload explicitly advertised `is_supernode`.
    #[serde(default)]
    pub supernode_from_invite: bool,
    #[serde(default)]
    pub transcript_hash: String,
    #[serde(default)]
    pub created_at: f64,
    #[serde(default)]
    pub last_seen_at: f64,
    #[serde(default)]
    pub last_ice: Option<serde_json::Value>,
    #[serde(default)]
    pub quic_port: u16,
    #[serde(default)]
    pub peer_version: String,
    #[serde(default)]
    pub peer_build_hash: String,
    /// Content hash of the sources the peer was built from (computed at build time
    /// by hashing relevant files). Helps detect source modifications even if
    /// the git-based build_id is spoofed via environment variable.
    #[serde(default)]
    pub peer_source_hash: String,
    #[serde(default)]
    pub peer_protocol_hash: String,
    #[serde(default)]
    pub peer_attestation_sig: String,
    #[serde(default)]
    pub last_attestation_at: f64,
    #[serde(default = "default_attestation_status")]
    pub attestation_status: String,
    #[serde(default)]
    pub last_nonce_challenge: String,
    /// Avatar visual config received from this peer after handshake.
    /// `None` = peer connected but hasn't sent config yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_config: Option<AvatarConfig>,
}

fn default_attestation_status() -> String {
    "unknown".to_owned()
}

impl PeerRecord {
    /// Human-friendly label: handle if set, otherwise truncated peer_id.
    pub fn display_name(&self) -> String {
        if !self.handle.is_empty() {
            self.handle.clone()
        } else {
            format!("{}…", &self.peer_id[..self.peer_id.len().min(12)])
        }
    }
}

// ---------------------------------------------------------------------------
// PeerStore
// ---------------------------------------------------------------------------

pub struct PeerStore {
    file_path: PathBuf,
    key: [u8; 32],
    records: HashMap<String, PeerRecord>,
}

impl PeerStore {
    /// Open the peer store associated with `identity`.
    pub fn open(identity: &Identity, file_path: Option<&Path>) -> Result<Self> {
        let default_dir = Identity::default_key_dir();
        let path = file_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_dir.join(PEER_STORE_FILE));
        let key = identity.derive_store_key(PEER_STORE_LABEL)?;
        let mut store = Self {
            file_path: path,
            key,
            records: HashMap::new(),
        };
        store.load();
        Ok(store)
    }

    /// Load records from disk; silently resets to empty on corruption.
    pub fn load(&mut self) {
        if !self.file_path.exists() {
            return;
        }
        let envelope = match std::fs::read(&self.file_path) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    "Failed to read peer store {}: {}",
                    self.file_path.display(),
                    e
                );
                return;
            }
        };
        let plaintext = match decrypt_blob(&self.key, &envelope) {
            Ok(p) => p,
            Err(e) => {
                warn!("Failed to decrypt peer store: {}", e);
                return;
            }
        };
        let doc: serde_json::Value = match serde_json::from_slice(&plaintext) {
            Ok(v) => v,
            Err(e) => {
                warn!("Malformed peer store JSON: {}", e);
                return;
            }
        };
        let peers = match doc["peers"].as_array() {
            Some(a) => a,
            None => return,
        };
        for entry in peers {
            match serde_json::from_value::<PeerRecord>(entry.clone()) {
                Ok(rec) => {
                    self.records.insert(rec.peer_id.clone(), rec);
                }
                Err(e) => {
                    warn!("Skipping malformed peer record: {}", e);
                }
            }
        }
        let repaired = Self::repair_all_supernode_flags(&mut self.records);
        if repaired {
            if let Err(e) = self.save() {
                warn!("Failed to persist repaired supernode flags: {}", e);
            }
        }
    }

    /// Persist records to disk as an encrypted envelope.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut sorted_peers: Vec<&PeerRecord> = self.records.values().collect();
        sorted_peers.sort_by(|a, b| {
            a.created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let payload = serde_json::json!({
            "version": 1,
            "updated_at": unix_now(),
            "peers": sorted_peers,
        });
        let plaintext = serde_json::to_vec(&payload)?;
        let envelope = encrypt_blob(&self.key, &plaintext)?;
        std::fs::write(&self.file_path, &envelope)?;
        Ok(())
    }

    // -- accessors ----------------------------------------------------------

    pub fn list_peers(&self) -> Vec<&PeerRecord> {
        let mut v: Vec<&PeerRecord> = self.records.values().collect();
        v.sort_by(|a, b| {
            a.created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Trusted peers for the navigation rail — excludes supernodes.
    pub fn list_non_supernode_peers(&self) -> Vec<&PeerRecord> {
        let mut v: Vec<&PeerRecord> = self.records.values().filter(|r| !r.is_supernode).collect();
        v.sort_by(|a, b| {
            a.created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    pub fn get(&self, peer_id: &str) -> Option<&PeerRecord> {
        self.records.get(peer_id)
    }

    pub fn get_by_identity(&self, identity_pub: &str) -> Option<&PeerRecord> {
        self.records
            .values()
            .find(|r| r.identity_pub == identity_pub)
    }

    pub fn get_mut(&mut self, peer_id: &str) -> Option<&mut PeerRecord> {
        self.records.get_mut(peer_id)
    }

    pub fn upsert(&mut self, record: PeerRecord) {
        self.records.insert(record.peer_id.clone(), record);
    }

    pub fn remove(&mut self, peer_id: &str) -> Option<PeerRecord> {
        self.records.remove(peer_id)
    }

    /// Remove by hex `peer_id` or base64url `identity_pub`.
    pub fn remove_by_any_id(&mut self, id: &str) -> Option<PeerRecord> {
        if let Some(rec) = self.records.remove(id) {
            return Some(rec);
        }
        let key = self
            .records
            .iter()
            .find(|(_, r)| r.identity_pub == id)
            .map(|(k, _)| k.clone());
        key.and_then(|k| self.records.remove(&k))
    }

    pub fn contains(&self, peer_id: &str) -> bool {
        self.records.contains_key(peer_id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn auto_connect_peers(&self) -> Vec<&PeerRecord> {
        self.records
            .values()
            .filter(|r| r.auto_connect && !r.blocked && !r.revoked)
            .collect()
    }

    /// Returns trusted supernode records eligible for WS auto-reconnect.
    pub fn supernodes(&self) -> Vec<&PeerRecord> {
        self.records
            .values()
            .filter(|r| !r.blocked && r.is_supernode)
            .collect()
    }

    /// True when `id` matches a trusted supernode (`peer_id` or `identity_pub`).
    pub fn is_supernode_id(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        self.records
            .values()
            .any(|r| !r.blocked && r.is_supernode && (r.identity_pub == id || r.peer_id == id))
    }

    /// Resolve a supernode sidebar / signaling id to the canonical
    /// `identity_pub` (base64url Ed25519 key). Returns `None` for ordinary peers.
    pub fn resolve_supernode_identity_pub(&self, id: &str) -> Option<String> {
        if id.is_empty() {
            return None;
        }
        for record in self.records.values() {
            if record.blocked || !record.is_supernode {
                continue;
            }
            if record.identity_pub == id || record.peer_id == id {
                return Some(record.identity_pub.clone());
            }
        }
        None
    }

    fn has_supernode_signaling_hint(record: &PeerRecord) -> bool {
        record.relay_hints.iter().any(|h| {
            let h = h.trim();
            h.starts_with("ws://") || h.starts_with("wss://")
        })
    }

    /// Default / operator titles supernodes advertise during handshake.
    fn looks_like_operator_handle(handle: &str) -> bool {
        matches!(
            handle,
            "Relay Node" | "Supernode" | "My Relay Node" | "My Community Node"
        ) || handle.ends_with(" Relay Node")
    }

    /// Supernodes eligible to own a ws signaling URL for demotion checks.
    /// Excludes mis-tagged peers (`is_supernode` without invite proof or operator title).
    fn is_trusted_supernode_record(record: &PeerRecord) -> bool {
        record.is_supernode
            && (record.supernode_from_invite
                || record.handle.is_empty()
                || Self::looks_like_operator_handle(&record.handle))
    }

    fn supernode_ws_hint_owners(records: &HashMap<String, PeerRecord>) -> HashMap<String, String> {
        let mut owners = HashMap::new();
        for record in records.values() {
            if !Self::is_trusted_supernode_record(record) {
                continue;
            }
            for hint in &record.relay_hints {
                let h = hint.trim();
                if h.starts_with("ws://") || h.starts_with("wss://") {
                    owners
                        .entry(h.to_owned())
                        .or_insert_with(|| record.identity_pub.clone());
                }
            }
        }
        owners
    }

    /// True when a ws/wss relay hint belongs to a different trusted supernode.
    fn relay_hint_targets_other_supernode(
        record: &PeerRecord,
        owners: &HashMap<String, String>,
    ) -> bool {
        for hint in &record.relay_hints {
            let h = hint.trim();
            if !h.starts_with("ws://") && !h.starts_with("wss://") {
                continue;
            }
            if let Some(owner) = owners.get(h) {
                if owner != &record.identity_pub {
                    return true;
                }
            }
        }
        false
    }

    /// Fix mis-tagged peers from the old ws-relay-hint heuristic and promote
    /// legacy supernodes saved before `is_supernode` was persisted.
    fn repair_all_supernode_flags(records: &mut HashMap<String, PeerRecord>) -> bool {
        let mut changed = false;
        let mut owners = Self::supernode_ws_hint_owners(records);

        // Pass 1 — demote false positives (regular peers borrowing a supernode ws URL).
        for record in records.values_mut() {
            if record.supernode_from_invite {
                continue;
            }
            if record.is_supernode && Self::relay_hint_targets_other_supernode(record, &owners) {
                record.is_supernode = false;
                changed = true;
                continue;
            }
            if record.is_supernode
                && record.quic_port > 0
                && !Self::looks_like_operator_handle(&record.handle)
            {
                record.is_supernode = false;
                changed = true;
            }
        }
        if changed {
            owners = Self::supernode_ws_hint_owners(records);
        }

        // Pass 2 — trust invite-flagged or unique-ws supernodes; restore invite-tagged rows.
        for record in records.values_mut() {
            if record.is_supernode
                && !record.supernode_from_invite
                && Self::has_supernode_signaling_hint(record)
                && record.quic_port == 0
                && !Self::relay_hint_targets_other_supernode(record, &owners)
            {
                record.supernode_from_invite = true;
                changed = true;
            }
            if !record.is_supernode && record.supernode_from_invite {
                record.is_supernode = true;
                changed = true;
            }
        }

        // Pass 3 — legacy promotion (operator/default title only).
        for record in records.values_mut() {
            if !record.is_supernode
                && Self::has_supernode_signaling_hint(record)
                && record.quic_port == 0
                && (record.handle.is_empty() || Self::looks_like_operator_handle(&record.handle))
            {
                record.is_supernode = true;
                record.supernode_from_invite = true;
                changed = true;
            }
        }

        changed
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_identity() -> Identity {
        Identity::generate()
    }

    #[test]
    fn empty_store_loads_cleanly() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn upsert_save_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        let rec = PeerRecord {
            peer_id: "abc123".to_owned(),
            identity_pub: "pubkey_b64".to_owned(),
            handle: "Alice".to_owned(),
            auto_connect: true,
            created_at: 1000.0,
            ..Default::default()
        };
        store.upsert(rec);
        store.save().unwrap();

        // Reload from disk
        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        let loaded = store2.get("abc123").unwrap();
        assert_eq!(loaded.handle, "Alice");
        assert!(loaded.auto_connect);
    }

    #[test]
    fn resolve_supernode_identity_pub_accepts_peer_id_or_identity_pub() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        store.upsert(PeerRecord {
            peer_id: "hex_peer".to_owned(),
            identity_pub: "b64_identity".to_owned(),
            relay_hints: vec!["ws://relay:34935/ws".to_owned()],
            is_supernode: true,
            ..Default::default()
        });

        assert_eq!(
            store.resolve_supernode_identity_pub("hex_peer"),
            Some("b64_identity".to_owned())
        );
        assert_eq!(
            store.resolve_supernode_identity_pub("b64_identity"),
            Some("b64_identity".to_owned())
        );
        assert!(store.resolve_supernode_identity_pub("unknown").is_none());
    }

    #[test]
    fn legacy_supernode_promoted_on_load_from_ws_hint_and_operator_title() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_hex".to_owned(),
            identity_pub: "sn_b64".to_owned(),
            relay_hints: vec!["ws://relay.example.com:34935/ws".to_owned()],
            handle: "Relay Node".to_owned(),
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        let sns = store2.supernodes();
        assert_eq!(sns.len(), 1);
        assert_eq!(sns[0].identity_pub, "sn_b64");
        assert!(store2.is_supernode_id("sn_b64"));
    }

    #[test]
    fn regular_peer_with_ws_relay_hint_and_quic_port_is_not_a_supernode() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        store.upsert(PeerRecord {
            peer_id: "peer_hex".to_owned(),
            identity_pub: "peer_b64".to_owned(),
            relay_hints: vec!["ws://relay.example.com:34935/ws".to_owned()],
            handle: "Alice".to_owned(),
            quic_port: 34934,
            is_supernode: true,
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        assert!(store2.supernodes().is_empty());
        assert!(!store2.is_supernode_id("peer_b64"));
    }

    #[test]
    fn custom_titled_supernode_is_not_demoted_on_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_hex".to_owned(),
            identity_pub: "sn_b64".to_owned(),
            relay_hints: vec!["ws://relay.example.com:34935/ws".to_owned()],
            handle: "My Home Server".to_owned(),
            is_supernode: true,
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        assert_eq!(store2.supernodes().len(), 1);
        assert!(store2.is_supernode_id("sn_b64"));
    }

    #[test]
    fn invite_flagged_supernode_restored_on_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_hex".to_owned(),
            identity_pub: "sn_b64".to_owned(),
            relay_hints: vec!["ws://relay.example.com:34935/ws".to_owned()],
            handle: "My Home Server".to_owned(),
            supernode_from_invite: true,
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        assert_eq!(store2.supernodes().len(), 1);
        assert!(store2.is_supernode_id("sn_b64"));
    }

    #[test]
    fn regular_peer_sharing_supernode_ws_hint_is_demoted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();
        let shared_hint = "ws://relay.example.com:34935/ws".to_owned();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_hex".to_owned(),
            identity_pub: "sn_b64".to_owned(),
            relay_hints: vec![shared_hint.clone()],
            handle: "Relay Node".to_owned(),
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });
        store.upsert(PeerRecord {
            peer_id: "peer_hex".to_owned(),
            identity_pub: "HQrq9wyKpjmsp0DvT0H3si_lgQ5nKoYpBjVxbDFkugQ=".to_owned(),
            relay_hints: vec![shared_hint],
            handle: "Friend".to_owned(),
            is_supernode: true,
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        assert_eq!(store2.supernodes().len(), 1);
        assert!(store2.is_supernode_id("sn_b64"));
        assert!(!store2.is_supernode_id("HQrq9wyKpjmsp0DvT0H3si_lgQ5nKoYpBjVxbDFkugQ="));
        let peers = store2.list_non_supernode_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].identity_pub,
            "HQrq9wyKpjmsp0DvT0H3si_lgQ5nKoYpBjVxbDFkugQ="
        );
    }

    #[test]
    fn remove_by_any_id_accepts_peer_id_or_identity_pub() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        store.upsert(PeerRecord {
            peer_id: "hex_peer".to_owned(),
            identity_pub: "b64_identity".to_owned(),
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });

        assert!(store.remove_by_any_id("b64_identity").is_some());
        assert!(store.is_empty());
    }

    #[test]
    fn list_non_supernode_peers_excludes_supernodes() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        store.upsert(PeerRecord {
            peer_id: "peer_hex".to_owned(),
            identity_pub: "peer_b64".to_owned(),
            handle: "Alice".to_owned(),
            ..Default::default()
        });
        store.upsert(PeerRecord {
            peer_id: "sn_hex".to_owned(),
            identity_pub: "sn_b64".to_owned(),
            relay_hints: vec!["ws://relay:34935/ws".to_owned()],
            is_supernode: true,
            ..Default::default()
        });

        let peers = store.list_non_supernode_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].identity_pub, "peer_b64");
    }

    #[test]
    fn wrong_key_does_not_panic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "x".to_owned(),
            identity_pub: "y".to_owned(),
            ..Default::default()
        });
        store.save().unwrap();

        // Different identity → wrong decryption key; should load empty silently
        let id2 = Identity::generate();
        let store2 = PeerStore::open(&id2, Some(&path)).unwrap();
        assert!(store2.is_empty());
    }
}
