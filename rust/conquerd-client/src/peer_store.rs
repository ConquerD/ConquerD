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

    pub fn supernodes(&self) -> Vec<&PeerRecord> {
        self.records
            .values()
            .filter(|r| r.is_supernode && !r.blocked)
            .collect()
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
