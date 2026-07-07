//! Room store — client-owned, encrypted persistence for voice room definitions.
//!
//! The supernode never persists rooms; rooms are client-owned data. When the
//! client connects to a supernode, saved rooms are sent via `SFU_ROOM_CREATE`
//! to be recreated on the fly.
//!
//! File: `~/.conquerd/my_rooms.dat` — AES-256-GCM envelope keyed by HKDF
//! subkey of the user's Identity.
//!
//! ## Schema versioning
//!
//! The inner JSON carries `"schema": 1`. Bump to `2` (and add a migration
//! in `load`) if a field is renamed, removed, or changes type. New optional
//! fields with `#[serde(default)]` do NOT require a bump.

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
    /// Space tree fields (Layer 1). `""` = legacy flat room (no Space).
    #[serde(default)]
    pub space_id: String,
    /// Parent node id in the Space; `""` = top-level / legacy.
    #[serde(default)]
    pub parent_id: String,
    /// Owner-controlled invite policy: `""` (inherit) | `"owner"` | `"members"`.
    #[serde(default)]
    pub invite_policy: String,
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
            space_id: String::new(),
            parent_id: String::new(),
            invite_policy: String::new(),
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

    /// Set the room's invite-mint policy (`"owner"` or `"members"`). Empty
    /// stays empty (interpreted as "unset" — the supernode defaults to
    /// `"owner"`); any non-`"members"` value is left as-is here (normalization
    /// to a known value happens supernode-side).
    pub fn with_invite_policy(mut self, policy: impl Into<String>) -> Self {
        self.invite_policy = policy.into();
        self
    }
}

// ---------------------------------------------------------------------------
// Serialized format
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct StoreData {
    #[serde(default = "default_schema")]
    schema: u32,
    rooms: Vec<RoomEntry>,
    #[serde(default)]
    deleted_ids: Vec<String>,
    /// Owner-held Space trees (Layer 1), keyed on disk by insertion order.
    /// Additive `#[serde(default)]` field — no schema bump per the file's rule.
    #[serde(default)]
    spaces: Vec<crate::space::Space>,
}

fn default_schema() -> u32 {
    1
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
    /// Owner-held Space trees keyed by `space_id`.
    spaces: HashMap<String, crate::space::Space>,
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
            spaces: HashMap::new(),
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
        if data.schema != 1 {
            warn!(
                "Room store schema version {} is newer than supported (1); \
                 some fields may be ignored",
                data.schema
            );
        }
        for entry in data.rooms {
            if entry.supernode_id.is_empty() {
                warn!(
                    "RoomStore: skipping malformed entry with missing supernode_id ({})",
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
        for space in data.spaces {
            self.spaces.insert(space.space_id.clone(), space);
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let data = StoreData {
            schema: 1,
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
            spaces: {
                let mut v: Vec<crate::space::Space> = self.spaces.values().cloned().collect();
                v.sort_by(|a, b| a.space_id.cmp(&b.space_id));
                v
            },
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

    // -- Space tree (Layer 1) ----------------------------------------------

    /// The `space_id` (Server node id) for a Space owned by `owner_pub` on
    /// `supernode_id`. Deterministic, so a room can be adopted before its Space
    /// exists in the store.
    pub fn space_id_for(owner_pub: &str, supernode_id: &str) -> String {
        crate::space::derive_node_id("", owner_pub, supernode_id)
    }

    /// A snapshot of an owned Space by id, if present.
    pub fn get_space(&self, space_id: &str) -> Option<crate::space::Space> {
        self.spaces.get(space_id).cloned()
    }

    /// All owned Spaces (sorted by id), for building invite proofs / roots.
    pub fn all_spaces(&self) -> Vec<crate::space::Space> {
        let mut v: Vec<crate::space::Space> = self.spaces.values().cloned().collect();
        v.sort_by(|a, b| a.space_id.cmp(&b.space_id));
        v
    }

    /// Adopt `room_id` as a Room node in the owner's Space for `supernode_id`,
    /// creating the Space (Server root) on first use. The room leaf's `node_id`
    /// is the existing `room_id` (design §3.3) so live room state and sidebar
    /// keys don't churn.
    ///
    /// `parent_node_id` selects where the room nests: `""` → directly under the
    /// Server node (a top-level room); otherwise the given node id (a nested
    /// sub-room under another room). Bumps the epoch, re-signs the root with
    /// `sign` (e.g. `|b| identity.sign(b)`), persists, stamps the stored
    /// `RoomEntry`, and returns the new signed root.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt_room_into_space(
        &mut self,
        owner_pub: &str,
        supernode_id: &str,
        room_id: &str,
        room_name: &str,
        room_type: &str,
        parent_node_id: &str,
        issued_at: u64,
        sign: impl Fn(&[u8]) -> Vec<u8>,
    ) -> Result<crate::space::SignedSpaceRoot> {
        let space_id = Self::space_id_for(owner_pub, supernode_id);
        // Empty / self-referential parent → nest directly under the Server node.
        let parent_id = if parent_node_id.is_empty() || parent_node_id == room_id {
            space_id.clone()
        } else {
            parent_node_id.to_owned()
        };
        let space = self
            .spaces
            .entry(space_id.clone())
            .or_insert_with(|| crate::space::Space::new_server(owner_pub, supernode_id));
        let node_type = if room_type.eq_ignore_ascii_case("private") {
            "private"
        } else {
            "public"
        };
        space.upsert_node(crate::space::SpaceNode {
            node_id: room_id.to_owned(),
            parent_id: parent_id.clone(),
            kind: "room".to_owned(),
            name: room_name.to_owned(),
            node_type: node_type.to_owned(),
            owner_pub: owner_pub.to_owned(),
            invite_policy: String::new(),
            inherit: false,
            key_commit: String::new(),
        });
        let root = space.signed_root(issued_at, sign);
        // Stamp the stored RoomEntry (if it exists) so the sidebar can render
        // the nesting: `space_id` = the tree, `parent_id` = Server or parent room.
        if let Some(entry) = self.rooms.get_mut(&Self::entry_key(supernode_id, room_id)) {
            entry.space_id = space_id.clone();
            entry.parent_id = parent_id;
        }
        self.save()?;
        Ok(root)
    }

    /// Stamp Space-tree linkage onto an already-stored room (e.g. from an
    /// accepted invite's inclusion proof) so the sidebar can nest it under its
    /// parent. Only overwrites with non-empty values; no-op if the room isn't
    /// stored or nothing changes. Persists when it does.
    pub fn set_space_linkage(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        parent_id: &str,
        space_id: &str,
    ) -> Result<()> {
        let Some(entry) = self.rooms.get_mut(&Self::entry_key(supernode_id, room_id)) else {
            return Ok(());
        };
        let mut changed = false;
        if !parent_id.is_empty() && entry.parent_id != parent_id {
            entry.parent_id = parent_id.to_owned();
            changed = true;
        }
        if !space_id.is_empty() && entry.space_id != space_id {
            entry.space_id = space_id.to_owned();
            changed = true;
        }
        if changed {
            self.save()?;
        }
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
            } else if existing.room_type == "private"
                && m.room_type == "public"
                && (existing.is_creator || !existing.invite_token.is_empty())
            {
                // Passive room-list sync must not downgrade a stored private room
                // back to public — that flips join_room off the invite path and the
                // supernode silently denies SfuJoin for non-members.
                m.room_type = existing.room_type.clone();
            }
            if m.creator_id.is_empty() {
                m.creator_id = existing.creator_id.clone();
            }
            if m.invite_token.is_empty() {
                m.invite_token = existing.invite_token.clone();
            }
            // Preserve client-only Space-tree fields: the supernode room list
            // (synced via `sync_saved_rooms_from_list`) carries none of these, so
            // without this a sub-room's parent linkage would be wiped on the next
            // room-list update and the sidebar tree would flatten.
            if m.space_id.is_empty() {
                m.space_id = existing.space_id.clone();
            }
            if m.parent_id.is_empty() {
                m.parent_id = existing.parent_id.clone();
            }
            if m.invite_policy.is_empty() {
                m.invite_policy = existing.invite_policy.clone();
            }
            m.is_creator = m.is_creator || existing.is_creator;
            m
        } else {
            entry
        };
        self.add(merged)
    }

    /// Passive-sync variant of [`upsert`] for the supernode's room list.
    ///
    /// A room the supernode still lists but the user hid locally must NOT be
    /// resurrected: [`add`] clears the hide tombstone, so upserting every listed
    /// room would un-hide them all the moment a room list arrives (e.g. right
    /// after accepting an invite). When the room is currently hidden this is a
    /// no-op — the tombstone and stored entry are left untouched. Explicit user
    /// actions (create / join / accept-invite) still go through [`upsert`] and
    /// intentionally un-hide.
    pub fn upsert_from_remote(&mut self, entry: RoomEntry) -> Result<()> {
        if !entry.supernode_id.is_empty()
            && self.is_hidden_from_sidebar(&entry.supernode_id, &entry.room_id)
        {
            return Ok(());
        }
        self.upsert(entry)
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
    fn upsert_does_not_downgrade_private_to_public() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();

        store
            .add(
                RoomEntry::new("priv", "Secret")
                    .with_type("private")
                    .with_supernode("sn-a")
                    .with_invite_token("tok-abc"),
            )
            .unwrap();

        // Simulates a remote room-list entry that omits type and defaults to public.
        store
            .upsert(
                RoomEntry::new("priv", "Secret")
                    .with_type("public")
                    .with_supernode("sn-a"),
            )
            .unwrap();

        let e = store.get("sn-a", "priv").unwrap();
        assert_eq!(
            e.room_type, "private",
            "stored private rooms must not be downgraded to public"
        );
        assert_eq!(e.invite_token, "tok-abc");
    }

    #[test]
    fn upsert_preserves_space_tree_fields() {
        // A sub-room whose parent linkage lives only in the local store must not
        // be wiped when the supernode room list re-syncs a parent-less entry.
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();

        let mut nested = RoomEntry::new("child", "Closet").with_supernode("sn-a");
        nested.space_id = "space-1".to_owned();
        nested.parent_id = "parent-room".to_owned();
        nested.invite_policy = "members".to_owned();
        store.add(nested).unwrap();

        // Simulates `sync_saved_rooms_from_list`: a fresh entry from the remote
        // room list with no Space fields.
        store
            .upsert(RoomEntry::new("child", "Closet").with_supernode("sn-a"))
            .unwrap();

        let e = store.get("sn-a", "child").unwrap();
        assert_eq!(e.parent_id, "parent-room", "parent_id must survive re-sync");
        assert_eq!(e.space_id, "space-1");
        assert_eq!(e.invite_policy, "members");
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
    fn upsert_from_remote_does_not_unhide() {
        // A passive room-list sync must not resurrect a room the user hid: the
        // regression where accepting an invite pulled a fresh list and un-hid
        // every previously hidden room.
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();

        store
            .add(RoomEntry::new("r1", "Test Room").with_supernode("sn-a"))
            .unwrap();
        store.hide_from_sidebar("sn-a", "r1").unwrap();
        assert!(store.is_hidden_from_sidebar("sn-a", "r1"));

        // Supernode re-lists the room (as `sync_saved_rooms_from_list` does).
        store
            .upsert_from_remote(RoomEntry::new("r1", "r1").with_supernode("sn-a"))
            .unwrap();
        assert!(
            store.is_hidden_from_sidebar("sn-a", "r1"),
            "passive sync must not un-hide a room the user hid"
        );

        // An explicit re-add / join still un-hides intentionally.
        store
            .upsert(RoomEntry::new("r1", "r1").with_supernode("sn-a"))
            .unwrap();
        assert!(!store.is_hidden_from_sidebar("sn-a", "r1"));
    }

    #[test]
    fn set_space_linkage_stamps_and_nests_invited_room() {
        // The invite-accept path stamps the room's parent/space from the proof so
        // the sidebar nests it. Empty values must not clobber an existing stamp.
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();

        store
            .add(RoomEntry::new("child", "test").with_supernode("sn-a"))
            .unwrap();
        // No-op for an unknown room.
        store
            .set_space_linkage("sn-a", "missing", "default", "space-1")
            .unwrap();

        store
            .set_space_linkage("sn-a", "child", "default", "space-1")
            .unwrap();
        let e = store.get("sn-a", "child").unwrap();
        assert_eq!(e.parent_id, "default");
        assert_eq!(e.space_id, "space-1");

        // Empty args leave the existing linkage intact.
        store.set_space_linkage("sn-a", "child", "", "").unwrap();
        let e = store.get("sn-a", "child").unwrap();
        assert_eq!(e.parent_id, "default");
        assert_eq!(e.space_id, "space-1");

        // Survives a reload.
        let store2 = RoomStore::open(&id, Some(&path)).unwrap();
        assert_eq!(store2.get("sn-a", "child").unwrap().parent_id, "default");
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

    /// Wire-format field stability guard.
    ///
    /// If you rename a field in `RoomEntry`, add `#[serde(rename = "old_name")]`
    /// to keep the on-disk key stable, then update this list.
    /// If you intentionally change the wire name, bump `schema` in `StoreData`
    /// and add a migration in `load()`.
    #[test]
    fn room_entry_wire_fields_are_stable() {
        let entry = RoomEntry {
            room_id: "room-abc".into(),
            room_name: "Test Room".into(),
            room_type: "public".into(),
            supernode_id: "sn-xyz".into(),
            creator_id: "creator-abc".into(),
            invite_token: "tok123".into(),
            is_creator: true,
            space_id: "space-abc".into(),
            parent_id: "parent-abc".into(),
            invite_policy: "owner".into(),
        };
        let json = serde_json::to_value(&entry).unwrap();
        let obj = json.as_object().unwrap();
        let required = [
            "room_id",
            "room_name",
            "room_type",
            "supernode_id",
            "creator_id",
            "invite_token",
            "is_creator",
            "space_id",
            "parent_id",
            "invite_policy",
        ];
        for key in required {
            assert!(
                obj.contains_key(key),
                "RoomEntry wire field missing or renamed: `{key}`"
            );
        }
    }

    #[test]
    fn spaces_round_trip_and_adopt_room() {
        use crate::crypto::ed25519_sign;
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let key = SigningKey::generate(&mut OsRng);
        let owner = crate::crypto::b64url_encode(key.verifying_key().as_bytes());

        let root = {
            let mut store = RoomStore::open(&id, Some(&path)).unwrap();
            // The room entry exists first (as created rooms are remembered),
            // then it is adopted into the Space and stamped.
            store
                .add(RoomEntry::new("room-1", "General").with_supernode("sn-a"))
                .unwrap();
            let root = store
                .adopt_room_into_space(
                    &owner,
                    "sn-a",
                    "room-1",
                    "General",
                    "public",
                    "",
                    100,
                    |b| ed25519_sign(&key.to_bytes(), b).unwrap(),
                )
                .unwrap();
            assert!(root.verify());
            assert_eq!(root.space_id, RoomStore::space_id_for(&owner, "sn-a"));
            // A top-level room (empty parent) is stamped with the Server node.
            let entry = store.get("sn-a", "room-1").expect("entry present");
            assert_eq!(entry.space_id, root.space_id);
            assert_eq!(entry.parent_id, root.space_id);
            root
        };

        // Reopen: the Space persisted and rebuilds to the same root/proofs.
        let store = RoomStore::open(&id, Some(&path)).unwrap();
        let space = store.get_space(&root.space_id).expect("space persisted");
        assert_eq!(space.root_hash(), root.root_hash);
        let proof = space.prove("room-1").expect("room node present");
        assert!(proof.verify_against(&root));
        // The stamp survived the round-trip.
        assert_eq!(store.get("sn-a", "room-1").unwrap().space_id, root.space_id);
    }

    #[test]
    fn adopt_nested_sub_room_parents_to_room() {
        use crate::crypto::ed25519_sign;
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let key = SigningKey::generate(&mut OsRng);
        let owner = crate::crypto::b64url_encode(key.verifying_key().as_bytes());
        let sign = |b: &[u8]| ed25519_sign(&key.to_bytes(), b).unwrap();

        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        store
            .add(RoomEntry::new("parent-room", "Lobby").with_supernode("sn-a"))
            .unwrap();
        store
            .add(RoomEntry::new("child-room", "Closet").with_supernode("sn-a"))
            .unwrap();

        // Parent room nests under the Server node.
        let r1 = store
            .adopt_room_into_space(
                &owner,
                "sn-a",
                "parent-room",
                "Lobby",
                "public",
                "",
                1,
                &sign,
            )
            .unwrap();
        assert_eq!(
            store.get("sn-a", "parent-room").unwrap().parent_id,
            r1.space_id
        );

        // Child room nests under the parent room's node id, not the Server.
        let r2 = store
            .adopt_room_into_space(
                &owner,
                "sn-a",
                "child-room",
                "Closet",
                "private",
                "parent-room",
                2,
                &sign,
            )
            .unwrap();
        let child = store.get("sn-a", "child-room").unwrap();
        assert_eq!(child.parent_id, "parent-room");
        assert_eq!(child.space_id, r2.space_id);

        // Both rooms are provable leaves of the same signed root.
        let space = store.get_space(&r2.space_id).unwrap();
        assert!(space.prove("parent-room").unwrap().verify_against(&r2));
        assert!(space.prove("child-room").unwrap().verify_against(&r2));
        // The child leaf records the parent linkage in the tree itself.
        let child_node = space
            .nodes
            .iter()
            .find(|n| n.node_id == "child-room")
            .unwrap();
        assert_eq!(child_node.parent_id, "parent-room");
    }

    #[test]
    fn store_data_carries_schema_version() {
        let dir = tempdir().unwrap();
        let id = make_identity();
        let path = dir.path().join(ROOM_STORE_FILE);
        let mut store = RoomStore::open(&id, Some(&path)).unwrap();
        store
            .add(RoomEntry::new("room-v", "Version Check").with_supernode("sn-a"))
            .unwrap();
        // Read the raw decrypted bytes and verify schema field is present
        let key = id.derive_store_key(ROOM_STORE_LABEL).unwrap();
        let envelope = std::fs::read(&path).unwrap();
        let plaintext = crate::crypto::decrypt_blob(&key, &envelope).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
        assert_eq!(
            doc["schema"].as_u64(),
            Some(1),
            "StoreData must carry schema version 1"
        );
    }
}
