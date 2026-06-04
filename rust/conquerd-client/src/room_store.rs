//! Room store — client-owned, encrypted persistence for voice room definitions.
//!
//! The supernode never persists rooms; rooms are client-owned data. When the
//! client connects to a supernode, saved rooms are sent via `SFU_ROOM_CREATE`
//! to be recreated on the fly.
//!
//! File: `~/.conquerd/my_rooms.dat` — AES-256-GCM envelope keyed by HKDF
//! subkey of the user's Identity.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::crypto::{decrypt_blob, encrypt_blob};
use crate::error::Result;
use crate::identity::Identity;

pub const ROOM_STORE_FILE: &str = "my_rooms.dat";
pub const ROOM_STORE_LABEL: &str = "conquerd-store/rooms/v1";

// ---------------------------------------------------------------------------
// RoomEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomEntry {
    pub room_id: String,
    pub room_name: String,
    #[serde(default = "default_room_type")]
    pub room_type: String,
    #[serde(default)]
    pub supernode_id: String,
    #[serde(default)]
    pub creator_id: String,
    #[serde(default)]
    pub invite_token: String,
    #[serde(default)]
    pub is_creator: bool,
}

fn default_room_type() -> String {
    "public".to_string()
}

impl RoomEntry {
    pub fn new(room_id: impl Into<String>, room_name: impl Into<String>) -> Self {
        Self {
            room_id: room_id.into(),
            room_name: room_name.into(),
            room_type: "public".to_string(),
            supernode_id: String::new(),
            creator_id: String::new(),
            invite_token: String::new(),
            is_creator: false,
        }
    }

    pub fn with_type(mut self, room_type: impl Into<String>) -> Self {
        self.room_type = room_type.into();
        self
    }

    pub fn with_supernode(mut self, supernode_id: impl Into<String>) -> Self {
        self.supernode_id = supernode_id.into();
        self
    }

    pub fn with_creator(mut self, creator_id: impl Into<String>, is_creator: bool) -> Self {
        self.creator_id = creator_id.into();
        self.is_creator = is_creator;
        self
    }

    pub fn with_invite_token(mut self, token: impl Into<String>) -> Self {
        self.invite_token = token.into();
        self
    }
}

// ---------------------------------------------------------------------------
// Serialized format
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoreData {
    rooms: Vec<RoomEntry>,
    #[serde(default)]
    deleted_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// RoomStore
// ---------------------------------------------------------------------------

/// Thread-safe encrypted store for the peer's room definitions.
///
/// Rooms are keyed by `room_id`. Deleted room IDs are tracked in a separate
/// set so that supernode broadcasts don't resurrect them.
pub struct RoomStore {
    file_path: PathBuf,
    key: [u8; 32],
    rooms: HashMap<String, RoomEntry>,
    deleted_ids: HashSet<String>,
}

impl RoomStore {
    /// Open and load the room store. Creates an empty store if the file does
    /// not exist yet.
    pub fn open(identity: &Identity, file_path: impl AsRef<Path>) -> Result<Self> {
        let key = identity.derive_store_key(ROOM_STORE_LABEL)?;
        let mut store = Self {
            file_path: file_path.as_ref().to_path_buf(),
            key,
            rooms: HashMap::new(),
            deleted_ids: HashSet::new(),
        };
        store.load()?;
        Ok(store)
    }

    fn load(&mut self) -> Result<()> {
        if !self.file_path.exists() {
            return Ok(());
        }
        let envelope = std::fs::read(&self.file_path).map_err(crate::error::ClientError::Io)?;
        let plaintext = decrypt_blob(&self.key, &envelope)?;
        let data: StoreData = serde_json::from_slice(&plaintext)
            .map_err(|e| crate::error::ClientError::Store(e.to_string()))?;
        for entry in data.rooms {
            self.rooms.insert(entry.room_id.clone(), entry);
        }
        for id in data.deleted_ids {
            self.deleted_ids.insert(id);
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let data = StoreData {
            rooms: {
                let mut v: Vec<RoomEntry> = self.rooms.values().cloned().collect();
                v.sort_by(|a, b| a.room_name.cmp(&b.room_name));
                v
            },
            deleted_ids: self.deleted_ids.iter().cloned().collect(),
        };
        let plaintext = serde_json::to_vec(&data)
            .map_err(|e| crate::error::ClientError::Store(e.to_string()))?;
        let envelope = encrypt_blob(&self.key, &plaintext)?;
        if let Some(parent) = self.file_path.parent() {
            std::fs::create_dir_all(parent).map_err(crate::error::ClientError::Io)?;
        }
        std::fs::write(&self.file_path, &envelope).map_err(crate::error::ClientError::Io)?;
        Ok(())
    }

    // -- CRUD ---------------------------------------------------------------

    /// Add or update a room entry and persist.
    pub fn add(&mut self, entry: RoomEntry) -> Result<()> {
        self.deleted_ids.remove(&entry.room_id);
        info!(
            "RoomStore: saving room '{}' ({})",
            entry.room_name,
            &entry.room_id[..entry.room_id.len().min(12)]
        );
        self.rooms.insert(entry.room_id.clone(), entry);
        self.save()
    }

    /// Remove a room by id, record as deleted, and persist.
    pub fn remove(&mut self, room_id: &str) -> Result<()> {
        self.rooms.remove(room_id);
        self.deleted_ids.insert(room_id.to_string());
        info!(
            "RoomStore: removed room {}",
            &room_id[..room_id.len().min(12)]
        );
        self.save()
    }

    /// Return `true` if this room was explicitly deleted by the user.
    pub fn is_deleted(&self, room_id: &str) -> bool {
        self.deleted_ids.contains(room_id)
    }

    /// Remove a room from the deleted set (e.g. re-invited).
    pub fn undelete(&mut self, room_id: &str) {
        self.deleted_ids.remove(room_id);
    }

    // -- Queries ------------------------------------------------------------

    pub fn get(&self, room_id: &str) -> Option<&RoomEntry> {
        self.rooms.get(room_id)
    }

    pub fn list(&self) -> Vec<&RoomEntry> {
        let mut v: Vec<&RoomEntry> = self.rooms.values().collect();
        v.sort_by(|a, b| a.room_name.cmp(&b.room_name));
        v
    }

    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    pub fn contains(&self, room_id: &str) -> bool {
        self.rooms.contains_key(room_id)
    }

    /// Rooms owned by this peer.
    pub fn owned_rooms(&self) -> Vec<&RoomEntry> {
        self.rooms.values().filter(|r| r.is_creator).collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use tempfile::tempdir;

    fn make_identity() -> Identity {
        Identity::generate()
    }

    #[test]
    fn roundtrip_empty() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let store = RoomStore::open(&id, &path).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn add_and_reload() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);

        {
            let mut store = RoomStore::open(&id, &path).unwrap();
            store
                .add(
                    RoomEntry::new("room-123", "Test Room")
                        .with_type("public")
                        .with_creator("creator-abc", true),
                )
                .unwrap();
            assert_eq!(store.len(), 1);
        }

        // Reload
        let store2 = RoomStore::open(&id, &path).unwrap();
        let entry = store2.get("room-123").unwrap();
        assert_eq!(entry.room_name, "Test Room");
        assert!(entry.is_creator);
    }

    #[test]
    fn remove_marks_deleted() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);

        let mut store = RoomStore::open(&id, &path).unwrap();
        store.add(RoomEntry::new("room-del", "Delete Me")).unwrap();
        store.remove("room-del").unwrap();
        assert!(!store.contains("room-del"));
        assert!(store.is_deleted("room-del"));
    }
}
