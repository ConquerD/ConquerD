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
/// Rooms are keyed by `(supernode_id, room_id)`. Sidebar hide keys use the
/// same composite format so removals stay scoped per supernode.
pub struct RoomStore {
    file_path: PathBuf,
    key: [u8; 32],
    rooms: HashMap<String, RoomEntry>,
    deleted_ids: HashSet<String>,
}

impl RoomStore {
    /// Composite storage key for a room on a specific supernode.
    pub fn entry_key(supernode_id: &str, room_id: &str) -> String {
        format!("{supernode_id}:{room_id}")
    }

    /// Open and load the room store. Creates an empty store if the file does
    /// not exist yet.
    pub fn open(identity: &Identity, file_path: Option<&Path>) -> Result<Self> {
        let default_dir = Identity::default_key_dir();
        let path = file_path
            .map(Path::to_path_buf)
            .unwrap_or_else(|| default_dir.join(ROOM_STORE_FILE));
        let key = identity.derive_store_key(ROOM_STORE_LABEL)?;
        let mut store = Self {
            file_path: path,
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
            if entry.supernode_id.is_empty() {
                warn!(
                    "RoomStore: skipping legacy entry without supernode_id ({})",
                    &entry.room_id[..entry.room_id.len().min(12)]
                );
                continue;
            }
            let key = Self::entry_key(&entry.supernode_id, &entry.room_id);
            self.rooms.insert(key, entry);
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
                v.sort_by(|a, b| {
                    a.supernode_id
                        .cmp(&b.supernode_id)
                        .then_with(|| a.room_name.cmp(&b.room_name))
                });
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

    /// Add or replace a room entry and persist.
    pub fn add(&mut self, entry: RoomEntry) -> Result<()> {
        if entry.supernode_id.is_empty() {
            return Err(crate::error::ClientError::Store(
                "room entry missing supernode_id".to_owned(),
            ));
        }
        let hide_key = Self::sidebar_hide_key(&entry.supernode_id, &entry.room_id);
        self.deleted_ids.remove(&hide_key);
        info!(
            "RoomStore: saving room '{}' ({}) on supernode {}",
            entry.room_name,
            &entry.room_id[..entry.room_id.len().min(12)],
            &entry.supernode_id[..entry.supernode_id.len().min(12)]
        );
        let key = Self::entry_key(&entry.supernode_id, &entry.room_id);
        self.rooms.insert(key, entry);
        self.save()
    }

    /// Merge a room entry with any existing record (keeps non-empty prior fields).
    pub fn upsert(&mut self, entry: RoomEntry) -> Result<()> {
        if entry.supernode_id.is_empty() {
            return Err(crate::error::ClientError::Store(
                "room entry missing supernode_id".to_owned(),
            ));
        }
        let key = Self::entry_key(&entry.supernode_id, &entry.room_id);
        let merged = if let Some(existing) = self.rooms.get(&key) {
            let mut m = entry;
            let incoming_name_is_id = m.room_name == m.room_id;
            if m.room_name.is_empty()
                || (incoming_name_is_id
                    && !existing.room_name.is_empty()
                    && existing.room_name != existing.room_id)
            {
                m.room_name = existing.room_name.clone();
            }
            if m.room_type.is_empty() {
                m.room_type = existing.room_type.clone();
            }
            if m.creator_id.is_empty() {
                m.creator_id = existing.creator_id.clone();
            }
            if m.invite_token.is_empty() {
                m.invite_token = existing.invite_token.clone();
            }
            m.is_creator = m.is_creator || existing.is_creator;
            m
        } else {
            entry
        };
        self.add(merged)
    }

    /// Remove a room by supernode + id, record as deleted, and persist.
    pub fn remove(&mut self, supernode_id: &str, room_id: &str) -> Result<()> {
        let key = Self::entry_key(supernode_id, room_id);
        self.rooms.remove(&key);
        self.deleted_ids.insert(key);
        info!(
            "RoomStore: removed room {} on supernode {}",
            &room_id[..room_id.len().min(12)],
            &supernode_id[..supernode_id.len().min(12)]
        );
        self.save()
    }

    /// Return `true` if this room was explicitly deleted by the user.
    pub fn is_deleted(&self, supernode_id: &str, room_id: &str) -> bool {
        self.deleted_ids
            .contains(&Self::entry_key(supernode_id, room_id))
    }

    /// Composite key for hiding a supernode room from the local sidebar only.
    pub fn sidebar_hide_key(supernode_id: &str, room_id: &str) -> String {
        Self::entry_key(supernode_id, room_id)
    }

    /// Hide a room from the local Rooms sidebar (does not delete on the supernode).
    pub fn hide_from_sidebar(&mut self, supernode_id: &str, room_id: &str) -> Result<()> {
        let key = Self::sidebar_hide_key(supernode_id, room_id);
        self.deleted_ids.insert(key);
        info!(
            "RoomStore: hid room {} on supernode {}",
            &room_id[..room_id.len().min(12)],
            &supernode_id[..supernode_id.len().min(12)]
        );
        self.save()
    }

    /// Return `true` when the user hid this room from the local sidebar.
    pub fn is_hidden_from_sidebar(&self, supernode_id: &str, room_id: &str) -> bool {
        self.deleted_ids
            .contains(&Self::sidebar_hide_key(supernode_id, room_id))
    }

    /// Remove a room from the deleted set (e.g. re-invited).
    pub fn undelete(&mut self, supernode_id: &str, room_id: &str) {
        self.deleted_ids
            .remove(&Self::entry_key(supernode_id, room_id));
    }

    // -- Queries ------------------------------------------------------------

    pub fn get(&self, supernode_id: &str, room_id: &str) -> Option<&RoomEntry> {
        self.rooms.get(&Self::entry_key(supernode_id, room_id))
    }

    pub fn list(&self) -> Vec<&RoomEntry> {
        let mut v: Vec<&RoomEntry> = self.rooms.values().collect();
        v.sort_by(|a, b| {
            a.supernode_id
                .cmp(&b.supernode_id)
                .then_with(|| a.room_name.cmp(&b.room_name))
        });
        v
    }

    /// Saved room definitions for one supernode (for replay on reconnect).
    pub fn list_for_supernode(&self, supernode_id: &str) -> Vec<&RoomEntry> {
        let mut v: Vec<&RoomEntry> = self
            .rooms
            .values()
            .filter(|e| e.supernode_id == supernode_id)
            .collect();
        v.sort_by(|a, b| a.room_name.cmp(&b.room_name));
        v
    }

    /// Like [`list_for_supernode`], but matches hex `peer_id` aliases via `peer_store`.
    pub fn list_for_supernode_resolved(
        &self,
        peer_store: &crate::peer_store::PeerStore,
        supernode_id: &str,
    ) -> Vec<RoomEntry> {
        let canon = peer_store
            .resolve_supernode_identity_pub(supernode_id)
            .unwrap_or_else(|| supernode_id.to_owned());
        let mut v: Vec<RoomEntry> = self
            .rooms
            .values()
            .filter(|e| {
                peer_store
                    .resolve_supernode_identity_pub(&e.supernode_id)
                    .unwrap_or_else(|| e.supernode_id.clone())
                    == canon
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| a.room_name.cmp(&b.room_name));
        v
    }

    /// Rewrite room rows keyed by hex `peer_id` to canonical `identity_pub`.
    pub fn normalize_supernode_ids(
        &mut self,
        peer_store: &crate::peer_store::PeerStore,
    ) -> Result<bool> {
        let mut changed = false;
        let entries: Vec<RoomEntry> = self.rooms.values().cloned().collect();
        for entry in entries {
            let Some(canon) = peer_store.resolve_supernode_identity_pub(&entry.supernode_id) else {
                continue;
            };
            if canon == entry.supernode_id {
                continue;
            }
            let old_key = Self::entry_key(&entry.supernode_id, &entry.room_id);
            let new_key = Self::entry_key(&canon, &entry.room_id);
            let mut moved = entry;
            moved.supernode_id = canon;
            self.rooms.remove(&old_key);
            self.rooms.insert(new_key, moved);
            changed = true;
        }
        let old_deleted: Vec<String> = self.deleted_ids.iter().cloned().collect();
        for key in old_deleted {
            let Some((sn, rid)) = key.split_once(':') else {
                continue;
            };
            let Some(canon) = peer_store.resolve_supernode_identity_pub(sn) else {
                continue;
            };
            if canon == sn {
                continue;
            }
            self.deleted_ids.remove(&key);
            self.deleted_ids.insert(Self::entry_key(&canon, rid));
            changed = true;
        }
        if changed {
            self.save()?;
        }
        Ok(changed)
    }

    pub fn len(&self) -> usize {
        self.rooms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
    }

    pub fn contains(&self, supernode_id: &str, room_id: &str) -> bool {
        self.rooms
            .contains_key(&Self::entry_key(supernode_id, room_id))
    }

    /// Rooms created by this peer on any supernode.
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
        let store = RoomStore::open(&id, Some(&path)).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn add_and_reload() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);

        {
            let mut store = RoomStore::open(&id, Some(&path)).unwrap();
            store
                .add(
                    RoomEntry::new("room-123", "Test Room")
                        .with_type("public")
                        .with_supernode("sn-abc")
                        .with_creator("creator-abc", true),
                )
                .unwrap();
            assert_eq!(store.len(), 1);
        }

        let store2 = RoomStore::open(&id, Some(&path)).unwrap();
        let entry = store2.get("sn-abc", "room-123").unwrap();
        assert_eq!(entry.room_name, "Test Room");
        assert!(entry.is_creator);
    }

    #[test]
    fn list_for_supernode_filters() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        store
            .add(
                RoomEntry::new("r1", "A")
                    .with_supernode("sn-a")
                    .with_creator("me", true),
            )
            .unwrap();
        store
            .add(
                RoomEntry::new("r2", "B")
                    .with_supernode("sn-b")
                    .with_creator("me", true),
            )
            .unwrap();
        assert_eq!(store.list_for_supernode("sn-a").len(), 1);
        assert_eq!(store.list_for_supernode("sn-a")[0].room_id, "r1");
    }

    #[test]
    fn upsert_preserves_display_name_when_join_passes_room_id() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        let room_id = "a1b2c3d4e5f67890";
        store
            .add(
                RoomEntry::new(room_id, "My Private Room")
                    .with_type("private")
                    .with_supernode("sn-a")
                    .with_creator("creator-x", true),
            )
            .unwrap();
        // join_room/subscribe_room_chat used to pass room_id as the display name.
        store
            .upsert(
                RoomEntry::new(room_id, room_id)
                    .with_type("private")
                    .with_supernode("sn-a"),
            )
            .unwrap();
        let e = store.get("sn-a", room_id).unwrap();
        assert_eq!(e.room_name, "My Private Room");
    }

    #[test]
    fn upsert_merges_fields() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        store
            .add(
                RoomEntry::new("r1", "Full Name")
                    .with_supernode("sn-a")
                    .with_creator("creator-x", true)
                    .with_invite_token("tok"),
            )
            .unwrap();
        store
            .upsert(
                RoomEntry::new("r1", "")
                    .with_supernode("sn-a")
                    .with_creator("", false),
            )
            .unwrap();
        let e = store.get("sn-a", "r1").unwrap();
        assert_eq!(e.room_name, "Full Name");
        assert_eq!(e.creator_id, "creator-x");
        assert_eq!(e.invite_token, "tok");
        assert!(e.is_creator);
    }

    #[test]
    fn normalize_supernode_ids_rekeys_hex_peer_id_to_identity_pub() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let peer_path = dir.path().join(crate::peer_store::PEER_STORE_FILE);
        let room_path = dir.path().join(ROOM_STORE_FILE);

        let mut peer_store = crate::peer_store::PeerStore::open(&id, Some(&peer_path)).unwrap();
        peer_store.upsert(crate::peer_store::PeerRecord {
            peer_id: "hex_sn".to_owned(),
            identity_pub: "b64_sn".to_owned(),
            is_supernode: true,
            supernode_from_invite: true,
            ..Default::default()
        });
        peer_store.save().unwrap();

        let mut room_store = RoomStore::open(&id, Some(&room_path)).unwrap();
        room_store
            .add(
                RoomEntry::new("room-1", "yes")
                    .with_supernode("hex_sn")
                    .with_type("public"),
            )
            .unwrap();

        room_store.normalize_supernode_ids(&peer_store).unwrap();
        let rooms = room_store.list_for_supernode("b64_sn");
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].room_id, "room-1");
    }

    #[test]
    fn hide_from_sidebar_is_scoped_by_supernode() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        store.hide_from_sidebar("sn-a", "room-1").unwrap();
        assert!(store.is_hidden_from_sidebar("sn-a", "room-1"));
        assert!(!store.is_hidden_from_sidebar("sn-b", "room-1"));

        let store2 = RoomStore::open(&id, Some(&path)).unwrap();
        assert!(store2.is_hidden_from_sidebar("sn-a", "room-1"));
    }

    #[test]
    fn remove_marks_deleted() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);

        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        store
            .add(RoomEntry::new("room-del", "Delete Me").with_supernode("sn-a"))
            .unwrap();
        store.remove("sn-a", "room-del").unwrap();
        assert!(!store.contains("sn-a", "room-del"));
        assert!(store.is_deleted("sn-a", "room-del"));
    }
}
