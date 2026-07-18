// ConquerD Supernode — peer_store.rs
// JSON-based trusted peer persistence.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::normalize_public_id;

/// A single peer record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRecord {
    pub peer_id: String,      // SHA-256 hex
    pub identity_pub: String, // base64url Ed25519 public key
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
    pub quic_port: u16,
}

/// Container for peers.json file format.
#[derive(Debug, Serialize, Deserialize)]
struct PeersFile {
    version: u32,
    updated_at: f64,
    peers: Vec<PeerRecord>,
}

/// In-memory peer store with JSON persistence.
pub struct PeerStore {
    /// identity_pub → PeerRecord
    peers: HashMap<String, PeerRecord>,
    path: std::path::PathBuf,
}

impl PeerStore {
    /// Create a new peer store backed by the given file.
    pub fn new(path: &Path) -> Self {
        let mut store = Self {
            peers: HashMap::new(),
            path: path.to_path_buf(),
        };
        store.load();
        store
    }

    /// Load peers from disk.
    fn load(&mut self) {
        if !self.path.exists() {
            return;
        }
        let Ok(data) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(file) = serde_json::from_str::<PeersFile>(&data) else {
            return;
        };
        for peer in file.peers {
            let mut peer = peer;
            // Collapse pad variants so load-time keys match live lookups.
            peer.identity_pub = normalize_public_id(&peer.identity_pub);
            self.peers.insert(peer.identity_pub.clone(), peer);
        }
    }

    /// Save peers to disk.
    pub fn save(&self) -> std::io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let file = PeersFile {
            version: 1,
            updated_at: now,
            peers: self.peers.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, json)
    }

    /// Resolve a map key for `identity_pub`, trying the canonical padded form
    /// and common pad variants so WS / relay / cluster encodings all hit.
    fn resolve_key(&self, identity_pub: &str) -> Option<String> {
        let canon = normalize_public_id(identity_pub);
        if self.peers.contains_key(&canon) {
            return Some(canon);
        }
        if self.peers.contains_key(identity_pub) {
            return Some(identity_pub.to_string());
        }
        let bare = identity_pub.trim_end_matches('=');
        if bare != identity_pub && self.peers.contains_key(bare) {
            return Some(bare.to_string());
        }
        None
    }

    /// Add or update a trusted peer.
    pub fn add_peer(&mut self, mut record: PeerRecord) {
        record.identity_pub = normalize_public_id(&record.identity_pub);
        // Drop any legacy pad-variant key so we never hold two entries for one peer.
        let bare = record.identity_pub.trim_end_matches('=').to_string();
        if bare != record.identity_pub {
            self.peers.remove(&bare);
        }
        self.peers.insert(record.identity_pub.clone(), record);
    }

    /// Get peer by identity_pub.
    pub fn get_peer(&self, identity_pub: &str) -> Option<&PeerRecord> {
        self.resolve_key(identity_pub)
            .and_then(|k| self.peers.get(&k))
    }

    /// Check if a peer is trusted (exists and not revoked/blocked).
    pub fn is_trusted(&self, identity_pub: &str) -> bool {
        self.get_peer(identity_pub)
            .is_some_and(|p| !p.revoked && !p.blocked)
    }

    /// Revoke a peer.
    #[allow(dead_code)]
    pub fn revoke_peer(&mut self, identity_pub: &str) {
        if let Some(key) = self.resolve_key(identity_pub) {
            if let Some(peer) = self.peers.get_mut(&key) {
                peer.revoked = true;
            }
        }
    }

    /// Remove a peer entirely.
    #[allow(dead_code)]
    pub fn remove_peer(&mut self, identity_pub: &str) {
        if let Some(key) = self.resolve_key(identity_pub) {
            self.peers.remove(&key);
        }
    }

    /// Get all trusted peer identity_pubs.
    pub fn trusted_peer_ids(&self) -> Vec<String> {
        self.peers
            .iter()
            .filter(|(_, p)| !p.revoked && !p.blocked)
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// Update last_seen_at for a peer.
    pub fn touch_peer(&mut self, identity_pub: &str) {
        if let Some(key) = self.resolve_key(identity_pub) {
            if let Some(peer) = self.peers.get_mut(&key) {
                peer.last_seen_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
            }
        }
    }

    /// Total number of peers (including revoked).
    #[allow(dead_code)]
    pub fn total_count(&self) -> usize {
        self.peers.len()
    }

    /// Number of non-revoked, non-blocked peers.
    pub fn trusted_count(&self) -> usize {
        self.peers
            .values()
            .filter(|p| !p.revoked && !p.blocked)
            .count()
    }

    /// Get all peer records.
    #[allow(dead_code)]
    pub fn all_peers(&self) -> impl Iterator<Item = &PeerRecord> {
        self.peers.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_store_basic() {
        let dir = std::env::temp_dir().join("conquerd_test_store");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("peers.json");

        let mut store = PeerStore::new(&path);
        assert_eq!(store.total_count(), 0);

        store.add_peer(PeerRecord {
            peer_id: "abc".into(),
            identity_pub: "pub1".into(),
            relay_hints: vec![],
            handle: "Alice".into(),
            blocked: false,
            revoked: false,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: String::new(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        assert!(store.is_trusted("pub1"));
        assert_eq!(store.trusted_count(), 1);

        store.save().unwrap();
        let store2 = PeerStore::new(&path);
        assert!(store2.is_trusted("pub1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn make_store_with_peer(path: &std::path::Path, identity_pub: &str) -> PeerStore {
        let mut store = PeerStore::new(path);
        store.add_peer(PeerRecord {
            peer_id: "pid".into(),
            identity_pub: identity_pub.into(),
            relay_hints: vec![],
            handle: "Test".into(),
            blocked: false,
            revoked: false,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: String::new(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        store
    }

    #[test]
    fn is_trusted_returns_false_for_unknown_peer() {
        let dir = std::env::temp_dir().join("conquerd_test_unknown");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = PeerStore::new(&dir.join("peers.json"));
        assert!(!store.is_trusted("nobody"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn is_trusted_returns_false_for_blocked_peer() {
        let dir = std::env::temp_dir().join("conquerd_test_blocked");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = make_store_with_peer(&dir.join("peers.json"), "pub-blocked");
        store.peers.get_mut("pub-blocked").unwrap().blocked = true;
        assert!(!store.is_trusted("pub-blocked"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revoke_peer_marks_as_revoked_and_untrusted() {
        let dir = std::env::temp_dir().join("conquerd_test_revoke");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = make_store_with_peer(&dir.join("peers.json"), "pub-r");
        assert!(store.is_trusted("pub-r"));
        store.revoke_peer("pub-r");
        assert!(!store.is_trusted("pub-r"));
        assert_eq!(store.trusted_count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn revoke_nonexistent_peer_is_noop() {
        let dir = std::env::temp_dir().join("conquerd_test_revoke_noop");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = PeerStore::new(&dir.join("peers.json"));
        store.revoke_peer("ghost"); // must not panic
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_peer_deletes_entry() {
        let dir = std::env::temp_dir().join("conquerd_test_remove");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = make_store_with_peer(&dir.join("peers.json"), "pub-del");
        assert_eq!(store.total_count(), 1);
        store.remove_peer("pub-del");
        assert_eq!(store.total_count(), 0);
        assert!(!store.is_trusted("pub-del"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trusted_peer_ids_excludes_revoked_and_blocked() {
        let dir = std::env::temp_dir().join("conquerd_test_ids");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("peers.json");
        let mut store = PeerStore::new(&path);
        store.add_peer(PeerRecord {
            peer_id: "1".into(),
            identity_pub: "ok".into(),
            relay_hints: vec![],
            handle: String::new(),
            blocked: false,
            revoked: false,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: String::new(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        store.add_peer(PeerRecord {
            peer_id: "2".into(),
            identity_pub: "rev".into(),
            relay_hints: vec![],
            handle: String::new(),
            blocked: false,
            revoked: true,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: String::new(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        store.add_peer(PeerRecord {
            peer_id: "3".into(),
            identity_pub: "blk".into(),
            relay_hints: vec![],
            handle: String::new(),
            blocked: true,
            revoked: false,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: String::new(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        let ids = store.trusted_peer_ids();
        assert_eq!(ids, vec!["ok".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn touch_peer_updates_last_seen() {
        let dir = std::env::temp_dir().join("conquerd_test_touch");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = make_store_with_peer(&dir.join("peers.json"), "pub-t");
        store.touch_peer("pub-t");
        let last = store.get_peer("pub-t").unwrap().last_seen_at;
        assert!(last > 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn touch_nonexistent_peer_is_noop() {
        let dir = std::env::temp_dir().join("conquerd_test_touch_noop");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = PeerStore::new(&dir.join("peers.json"));
        store.touch_peer("ghost"); // must not panic
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_peers_returns_all_including_revoked() {
        let dir = std::env::temp_dir().join("conquerd_test_all");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = make_store_with_peer(&dir.join("peers.json"), "pub-a");
        store.revoke_peer("pub-a");
        let count = store.all_peers().count();
        assert_eq!(count, 1); // still present, just revoked
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_creates_parent_dirs_and_persists() {
        let dir = std::env::temp_dir().join("conquerd_test_save_deep");
        let _ = std::fs::remove_dir_all(&dir);
        let nested_path = dir.join("a").join("b").join("peers.json");
        let mut store = PeerStore::new(&nested_path);
        store.add_peer(PeerRecord {
            peer_id: "1".into(),
            identity_pub: "deep".into(),
            relay_hints: vec![],
            handle: String::new(),
            blocked: false,
            revoked: false,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: String::new(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        store.save().unwrap();
        let store2 = PeerStore::new(&nested_path);
        assert!(store2.is_trusted("deep"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pad_variants_resolve_to_same_trusted_peer() {
        let dir = std::env::temp_dir().join("conquerd_test_pad_peers");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 43-char unpadded URL-safe base64 of a 32-byte key.
        let bare: String = std::iter::repeat_n('A', 43).collect();
        assert_eq!(bare.len(), 43);
        let padded = format!("{bare}=");
        let mut store = PeerStore::new(&dir.join("peers.json"));
        store.add_peer(PeerRecord {
            peer_id: "pid".into(),
            identity_pub: bare.clone(),
            relay_hints: vec![],
            handle: "Pad".into(),
            blocked: false,
            revoked: false,
            auto_connect: false,
            is_supernode: false,
            transcript_hash: "abc".into(),
            created_at: 0.0,
            last_seen_at: 0.0,
            quic_port: 0,
        });
        assert!(store.is_trusted(&bare));
        assert!(store.is_trusted(&padded));
        assert_eq!(
            store.get_peer(&bare).unwrap().identity_pub,
            normalize_public_id(&bare)
        );
        assert_eq!(store.total_count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
