//! Peer store — local, client-owned storage for trusted peers.
//!
//! Data is stored on disk as an AES-256-GCM envelope keyed by an HKDF
//! subkey of the user's Identity.
//!
//! ## Schema versioning
//!
//! The on-disk envelope contains JSON with a top-level `"version": 1` field.
//! Bump this to `2` (and add a migration in `load`) whenever a field is
//! renamed, removed, or changes type. New optional fields (with `#[serde(default)]`)
//! do NOT require a version bump.

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
        if self.file_path.exists() {
            match std::fs::read(&self.file_path) {
                Ok(envelope) => match decrypt_blob(&self.key, &envelope) {
                    Ok(plaintext) => {
                        match serde_json::from_slice::<serde_json::Value>(&plaintext) {
                            Ok(doc) => {
                                if let Some(v) = doc["version"].as_u64() {
                                    if v != 1 {
                                        warn!("Peer store schema version {v} is newer than supported (1); \
                                               some fields may be ignored");
                                    }
                                }
                                if let Some(peers) = doc["peers"].as_array() {
                                    for entry in peers {
                                        match serde_json::from_value::<PeerRecord>(entry.clone()) {
                                            Ok(rec) => {
                                                self.records.insert(rec.peer_id.clone(), rec);
                                            }
                                            Err(e) => {
                                                warn!("Skipping malformed peer record: {e}");
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => warn!("Malformed peer store JSON: {e}"),
                        }
                    }
                    Err(e) => warn!("Failed to decrypt peer store: {e}"),
                },
                Err(e) => {
                    warn!(
                        "Failed to read peer store {}: {}",
                        self.file_path.display(),
                        e
                    );
                }
            }
        }
        if Self::repair_all_supernode_flags(&mut self.records) {
            if let Err(e) = self.save() {
                warn!("Failed to persist repaired supernode flags: {e}");
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

    /// Insert or replace a peer accepted via invite without clobbering an
    /// unrelated record that happens to share the same `peer_id` HashMap key.
    pub fn upsert_from_invite(&mut self, mut record: PeerRecord) {
        let stale_keys: Vec<String> = self
            .records
            .iter()
            .filter(|(_, r)| r.identity_pub == record.identity_pub)
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale_keys {
            self.records.remove(&k);
        }
        if let Some(existing) = self.records.get(&record.peer_id) {
            if existing.identity_pub != record.identity_pub {
                record.peer_id = record.identity_pub.clone();
            }
        }
        self.records.insert(record.peer_id.clone(), record);
    }

    /// When a supernode invite carries a ws signaling URL already used by
    /// another trusted supernode, mark those rows invite-flagged so load-time
    /// repair does not demote them on the next restart.
    pub fn grandfather_supernode_ws_hint_sharing(&mut self, ws_hint: &str) {
        let h = ws_hint.trim();
        if !h.starts_with("ws://") && !h.starts_with("wss://") {
            return;
        }
        for record in self.records.values_mut() {
            if !record.is_supernode {
                continue;
            }
            if record.relay_hints.iter().any(|hint| hint.trim() == h) {
                record.supernode_from_invite = true;
            }
        }
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

    /// True when `id` is a trusted supernode the user explicitly joined via an
    /// invite (as opposed to one only learned from a cluster roster). Used to
    /// pin a cluster's single display identity to the member the user actually
    /// connected through, which is also guaranteed to be a known supernode.
    pub fn is_invite_supernode_id(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        self.records.values().any(|r| {
            !r.blocked
                && r.is_supernode
                && r.supernode_from_invite
                && (r.identity_pub == id || r.peer_id == id)
        })
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

    /// Handles that look like infrastructure nodes rather than personal peers.
    fn looks_like_supernode_handle(handle: &str) -> bool {
        if handle.is_empty() || Self::looks_like_operator_handle(handle) {
            return true;
        }
        let lower = handle.to_ascii_lowercase();
        lower.contains("server")
            || lower.contains("node")
            || lower.contains("relay")
            || lower.contains("supernode")
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
            if record.is_supernode
                && Self::relay_hint_targets_other_supernode(record, &owners)
                && !Self::looks_like_supernode_handle(&record.handle)
            {
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

        // Pass 3 — restore infrastructure rows that were incorrectly demoted when
        // another supernode shared the same ws signaling URL (pre-2026-06 fix).
        for record in records.values_mut() {
            if record.is_supernode || record.quic_port > 0 {
                continue;
            }
            if Self::has_supernode_signaling_hint(record)
                && Self::looks_like_supernode_handle(&record.handle)
            {
                record.is_supernode = true;
                record.supernode_from_invite = true;
                changed = true;
            }
        }

        changed
    }

    /// Re-promote trusted peers referenced by saved room definitions. Used on
    /// startup when a supernode was demoted from the Rooms sidebar but room
    /// history still points at its `identity_pub`.
    pub fn restore_supernodes_referenced_by_ids(&mut self, supernode_ids: &[String]) -> bool {
        let mut changed = false;
        for id in supernode_ids {
            if id.is_empty() {
                continue;
            }
            if self.is_supernode_id(id) {
                continue;
            }
            let keys: Vec<String> = self
                .records
                .iter()
                .filter(|(_, r)| r.identity_pub == *id || r.peer_id == *id)
                .map(|(k, _)| k.clone())
                .collect();
            for key in keys {
                let Some(record) = self.records.get_mut(&key) else {
                    continue;
                };
                if record.blocked || record.revoked {
                    continue;
                }
                if !record.is_supernode {
                    warn!(
                        "Restored demoted supernode {} from room-store reference",
                        &record.identity_pub[..record.identity_pub.len().min(12)]
                    );
                    record.is_supernode = true;
                    record.supernode_from_invite = true;
                    changed = true;
                }
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
    fn two_custom_supernodes_sharing_ws_hint_both_survive_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();
        let shared_hint = "ws://relay.example.com:34935/ws".to_owned();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_a_hex".to_owned(),
            identity_pub: "sn_a_b64".to_owned(),
            relay_hints: vec![shared_hint.clone()],
            handle: "My Home Server".to_owned(),
            is_supernode: true,
            ..Default::default()
        });
        store.upsert(PeerRecord {
            peer_id: "sn_b_hex".to_owned(),
            identity_pub: "sn_b_b64".to_owned(),
            relay_hints: vec![shared_hint],
            handle: "Backup Node".to_owned(),
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        assert_eq!(store2.supernodes().len(), 2);
        assert!(store2.is_supernode_id("sn_a_b64"));
        assert!(store2.is_supernode_id("sn_b_b64"));
    }

    #[test]
    fn upsert_from_invite_does_not_clobber_unrelated_peer_id_key() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        store.upsert(PeerRecord {
            peer_id: "shared_hex".to_owned(),
            identity_pub: "sn_a_b64".to_owned(),
            handle: "Node A".to_owned(),
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });

        store.upsert_from_invite(PeerRecord {
            peer_id: "shared_hex".to_owned(),
            identity_pub: "sn_b_b64".to_owned(),
            handle: "Node B".to_owned(),
            relay_hints: vec!["ws://relay:34935/ws".to_owned()],
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });

        assert_eq!(store.supernodes().len(), 2);
        assert!(store.is_supernode_id("sn_a_b64"));
        assert!(store.is_supernode_id("sn_b_b64"));
    }

    #[test]
    fn demoted_infrastructure_supernode_restored_on_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(PEER_STORE_FILE);
        let id = make_identity();
        let shared_hint = "ws://relay.example.com:34935/ws".to_owned();

        let mut store = PeerStore::open(&id, Some(&path)).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_a_hex".to_owned(),
            identity_pub: "sn_a_b64".to_owned(),
            relay_hints: vec![shared_hint.clone()],
            handle: "Node A".to_owned(),
            is_supernode: false,
            ..Default::default()
        });
        store.upsert(PeerRecord {
            peer_id: "sn_b_hex".to_owned(),
            identity_pub: "sn_b_b64".to_owned(),
            relay_hints: vec![shared_hint],
            handle: "Node B".to_owned(),
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });
        store.save().unwrap();

        let store2 = PeerStore::open(&id, Some(&path)).unwrap();
        assert_eq!(store2.supernodes().len(), 2);
        assert!(store2.is_supernode_id("sn_a_b64"));
        assert!(store2.is_supernode_id("sn_b_b64"));
    }

    #[test]
    fn restore_supernodes_referenced_by_ids_promotes_demoted_row() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        store.upsert(PeerRecord {
            peer_id: "sn_a_hex".to_owned(),
            identity_pub: "sn_a_b64".to_owned(),
            relay_hints: vec!["ws://relay:34935/ws".to_owned()],
            handle: "Alice Node".to_owned(),
            is_supernode: false,
            ..Default::default()
        });

        assert!(store.restore_supernodes_referenced_by_ids(&["sn_a_b64".to_owned()]));
        assert!(store.is_supernode_id("sn_a_b64"));
    }

    #[test]
    fn grandfather_supernode_ws_hint_sharing_flags_existing_rows() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let mut store = PeerStore::open(&id, Some(&dir.path().join(PEER_STORE_FILE))).unwrap();
        let shared_hint = "ws://relay.example.com:34935/ws".to_owned();
        store.upsert(PeerRecord {
            peer_id: "sn_a_hex".to_owned(),
            identity_pub: "sn_a_b64".to_owned(),
            relay_hints: vec![shared_hint.clone()],
            handle: "My Home Server".to_owned(),
            is_supernode: true,
            ..Default::default()
        });

        store.grandfather_supernode_ws_hint_sharing(&shared_hint);
        assert!(store.get("sn_a_hex").unwrap().supernode_from_invite);
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

    /// Manual diagnostic for a live profile (uses OS keyring unlock).
    /// Run: `cargo test -p conquerd-client --features manual-diagnostics dump_live_profile -- --nocapture`
    /// Optional: `CONQUERD_HOME=C:\Users\YOU\.conquerd` to target a specific profile.
    #[cfg(feature = "manual-diagnostics")]
    #[test]
    fn dump_live_profile() {
        use std::path::PathBuf;
        let key_dir = std::env::var("CONQUERD_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".conquerd")
            });
        eprintln!("CONQUERD_HOME={}", key_dir.display());
        let (identity, _) =
            crate::identity::Identity::load_with_keyring_or_passphrase(b"", &key_dir)
                .expect("unlock identity");
        eprintln!("my_public_id={}", identity.public_id());
        let peer_path = key_dir.join(PEER_STORE_FILE);
        let store = PeerStore::open(&identity, Some(&peer_path)).expect("open peers");
        eprintln!("total_records={}", store.len());
        for p in store.list_peers() {
            eprintln!(
                "peer_id={} identity_pub={} handle={} is_supernode={} supernode_from_invite={} relay_hints={:?}",
                p.peer_id, p.identity_pub, p.handle, p.is_supernode, p.supernode_from_invite, p.relay_hints
            );
        }
        eprintln!("--- supernodes ---");
        for sn in store.supernodes() {
            eprintln!(
                "  {} handle={} hints={:?}",
                sn.identity_pub, sn.handle, sn.relay_hints
            );
        }
        let room_path = key_dir.join(crate::room_store::ROOM_STORE_FILE);
        if room_path.exists() {
            let rs =
                crate::room_store::RoomStore::open(&identity, Some(&room_path)).expect("rooms");
            eprintln!("--- rooms ---");
            for r in rs.list() {
                eprintln!(
                    "  supernode_id={} room_id={} name={}",
                    r.supernode_id, r.room_id, r.room_name
                );
            }
        }
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

    /// Wire-format field stability guard.
    ///
    /// If you rename a field in `PeerRecord`, add `#[serde(rename = "old_name")]`
    /// to keep the on-disk key stable, then update this list.
    /// If you intentionally change the wire name, bump `"version"` in `save()`
    /// and add a migration in `load()`.
    #[test]
    fn peer_record_wire_fields_are_stable() {
        let rec = PeerRecord {
            peer_id: "pid".into(),
            identity_pub: "pub_b64".into(),
            relay_hints: vec!["ws://relay:34935/ws".into()],
            handle: "Alice".into(),
            blocked: true,
            revoked: false,
            auto_connect: true,
            is_supernode: false,
            supernode_from_invite: false,
            transcript_hash: "abc".into(),
            created_at: 1.0,
            last_seen_at: 2.0,
            last_ice: Some(serde_json::json!({"candidate": "test"})),
            quic_port: 34934,
            peer_version: "1.0".into(),
            peer_build_hash: "bld".into(),
            peer_source_hash: "src".into(),
            peer_protocol_hash: "prot".into(),
            peer_attestation_sig: "sig".into(),
            last_attestation_at: 3.0,
            attestation_status: "verified".into(),
            last_nonce_challenge: "nonce".into(),
            avatar_config: None,
        };
        let json = serde_json::to_value(&rec).unwrap();
        let obj = json.as_object().unwrap();
        // These are the canonical on-disk field names. Changing any of these
        // without a serde rename breaks backward-read of existing stores.
        let required = [
            "peer_id",
            "identity_pub",
            "relay_hints",
            "handle",
            "blocked",
            "revoked",
            "auto_connect",
            "is_supernode",
            "supernode_from_invite",
            "transcript_hash",
            "created_at",
            "last_seen_at",
            "last_ice",
            "quic_port",
            "peer_version",
            "peer_build_hash",
            "peer_source_hash",
            "peer_protocol_hash",
            "peer_attestation_sig",
            "last_attestation_at",
            "attestation_status",
            "last_nonce_challenge",
        ];
        for key in required {
            assert!(
                obj.contains_key(key),
                "PeerRecord wire field missing or renamed: `{key}`"
            );
        }
        // avatar_config uses skip_serializing_if = "Option::is_none"; absent when None
        assert!(
            !obj.contains_key("avatar_config"),
            "avatar_config should be absent when None"
        );
    }
}
