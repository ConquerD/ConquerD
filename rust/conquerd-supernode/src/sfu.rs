// ConquerD Supernode — sfu.rs
// SFU room management: room lifecycle, participant tracking, room types.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::crypto::generate_nonce_hex;

/// Max participants per SFU room.
pub const MAX_ROOM_SIZE: usize = 32;
/// Default room ID (always-present "Public Voice" room).
pub const DEFAULT_ROOM_ID: &str = "default";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomType {
    Public,
    Private,
}

/// A single SFU room.
#[derive(Debug, Clone)]
pub struct SFURoom {
    pub room_id: String,
    pub room_name: String,
    pub room_type: RoomType,
    pub creator_id: String,
    /// identity_pub → participant index
    participants: HashMap<String, u8>,
    /// Text-chat subscribers (not voice-joined)
    subscribers: std::collections::HashSet<String>,
    /// Allowed peers for private rooms
    allowed: std::collections::HashSet<String>,
    /// Invite tokens: token → InviteToken
    invite_tokens: HashMap<String, InviteToken>,
    #[allow(dead_code)]
    pub created_at: f64,
    next_index: u8,
}

#[derive(Debug, Clone)]
struct InviteToken {
    #[allow(dead_code)]
    created_by: String,
    uses: u32,
    max_uses: u32,
}

impl SFURoom {
    pub fn new(room_id: &str, room_name: &str, room_type: RoomType, creator_id: &str) -> Self {
        Self {
            room_id: room_id.to_string(),
            room_name: room_name.to_string(),
            room_type,
            creator_id: creator_id.to_string(),
            participants: HashMap::new(),
            subscribers: std::collections::HashSet::new(),
            allowed: std::collections::HashSet::new(),
            invite_tokens: HashMap::new(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
            next_index: 1,
        }
    }

    pub fn is_public(&self) -> bool {
        self.room_type == RoomType::Public
    }

    pub fn is_peer_allowed(&self, peer_id: &str) -> bool {
        self.is_public() || self.allowed.contains(peer_id) || self.creator_id == peer_id
    }

    #[allow(dead_code)]
    pub fn allow_peer(&mut self, peer_id: &str) {
        self.allowed.insert(peer_id.to_string());
    }

    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    pub fn is_full(&self) -> bool {
        self.participants.len() >= MAX_ROOM_SIZE
    }

    pub fn participant_ids(&self) -> Vec<String> {
        self.participants.keys().cloned().collect()
    }

    pub fn add_participant(&mut self, peer_id: &str) -> bool {
        // Idempotent: if the peer is already a voice participant (common
        // reconnect race where the old WS hasn't been cleaned up yet), return
        // true so the caller still sends a fresh SfuMembers snapshot back to
        // the joiner.  This prevents the "don't see myself in the room" bug.
        if self.participants.contains_key(peer_id) {
            return true;
        }
        if self.is_full() {
            return false;
        }
        let idx = self.next_index;
        self.next_index = self.next_index.wrapping_add(1).max(1);
        self.participants.insert(peer_id.to_string(), idx);
        // Promoted to full participant — remove from text-only subscribers.
        self.subscribers.remove(peer_id);
        true
    }

    pub fn remove_participant(&mut self, peer_id: &str) -> bool {
        self.participants.remove(peer_id).is_some()
    }

    /// Subscribe a peer to text chat (without voice join).
    pub fn subscribe(&mut self, peer_id: &str) {
        // No-op if already a voice participant (they already receive chat).
        if !self.participants.contains_key(peer_id) {
            self.subscribers.insert(peer_id.to_string());
        }
    }

    /// Unsubscribe a peer from text chat.
    pub fn unsubscribe(&mut self, peer_id: &str) {
        self.subscribers.remove(peer_id);
    }

    /// Remove a peer from both participants and subscribers.
    pub fn remove_peer_entirely(&mut self, peer_id: &str) -> bool {
        let was_participant = self.participants.remove(peer_id).is_some();
        self.subscribers.remove(peer_id);
        was_participant
    }

    /// All peers who should receive text chat: participants + subscribers.
    pub fn chat_recipient_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.participants.keys().cloned().collect();
        for sub in &self.subscribers {
            ids.push(sub.clone());
        }
        ids
    }

    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    pub fn generate_invite_token(&mut self, created_by: &str, max_uses: u32) -> String {
        let token = generate_nonce_hex(16);
        self.invite_tokens.insert(
            token.clone(),
            InviteToken {
                created_by: created_by.to_string(),
                uses: 0,
                max_uses,
            },
        );
        token
    }

    pub fn validate_and_consume_token(&mut self, token: &str, peer_id: &str) -> bool {
        let Some(it) = self.invite_tokens.get_mut(token) else {
            return false;
        };
        if it.max_uses > 0 && it.uses >= it.max_uses {
            return false;
        }
        it.uses += 1;
        self.allowed.insert(peer_id.to_string());
        if it.max_uses > 0 && it.uses >= it.max_uses {
            self.invite_tokens.remove(token);
        }
        true
    }

    /// Serialize for persistence.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::json!({
            "room_id": self.room_id,
            "room_name": self.room_name,
            "room_type": self.room_type,
            "creator_id": self.creator_id,
        })
    }
}

/// Manages all SFU rooms.
pub struct SFURoomManager {
    rooms: HashMap<String, SFURoom>,
}

impl SFURoomManager {
    pub fn new() -> Self {
        let mut mgr = Self {
            rooms: HashMap::new(),
        };
        // Create default "Public Voice" room
        mgr.rooms.insert(
            DEFAULT_ROOM_ID.to_string(),
            SFURoom::new(DEFAULT_ROOM_ID, "Public Voice", RoomType::Public, ""),
        );
        mgr
    }

    /// Create a new room. Returns the room if created, or None if exists.
    pub fn create_room(
        &mut self,
        room_id: Option<&str>,
        room_name: &str,
        room_type: RoomType,
        creator_id: &str,
    ) -> Option<&SFURoom> {
        let id = room_id
            .map(String::from)
            .unwrap_or_else(|| crate::crypto::derive_room_id(creator_id, room_name));
        if self.rooms.contains_key(&id) {
            return self.rooms.get(&id);
        }
        let room = SFURoom::new(&id, room_name, room_type, creator_id);
        info!("Created SFU room: {} ({})", room_name, &id);
        self.rooms.insert(id.clone(), room);
        self.rooms.get(&id)
    }

    /// Join a peer to a room. Returns (success, member_list).
    pub fn join_room(&mut self, peer_id: &str, room_id: &str) -> (bool, Vec<String>) {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return (false, vec![]);
        };
        if !room.is_peer_allowed(peer_id) {
            return (false, vec![]);
        }
        let ok = room.add_participant(peer_id);
        let members = room.participant_ids();
        (ok, members)
    }

    /// Leave a room. Returns remaining member list.
    pub fn leave_room(&mut self, peer_id: &str, room_id: &str) -> Vec<String> {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return vec![];
        };
        room.remove_participant(peer_id);
        let members = room.participant_ids();
        // GC anonymous rooms (non-default, no creator) when empty
        if room.is_empty() && room.creator_id.is_empty() && room_id != DEFAULT_ROOM_ID {
            debug!("GC-ing empty anonymous room: {}", room_id);
            self.rooms.remove(room_id);
        }
        members
    }

    /// Remove peer from ALL rooms (participants and subscribers).
    pub fn remove_peer_from_all(&mut self, peer_id: &str) -> Vec<(String, Vec<String>)> {
        let mut results = vec![];
        let room_ids: Vec<String> = self
            .rooms
            .iter()
            .filter(|(_, r)| {
                r.participants.contains_key(peer_id) || r.subscribers.contains(peer_id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for room_id in room_ids {
            if let Some(room) = self.rooms.get_mut(&room_id) {
                let was_participant = room.remove_peer_entirely(peer_id);
                if was_participant {
                    let members = room.participant_ids();
                    // GC anonymous rooms when empty
                    if room.is_empty() && room.creator_id.is_empty() && room_id != DEFAULT_ROOM_ID {
                        debug!("GC-ing empty anonymous room: {}", room_id);
                        self.rooms.remove(&room_id);
                    }
                    results.push((room_id, members));
                }
            }
        }
        results
    }

    pub fn get_room(&self, room_id: &str) -> Option<&SFURoom> {
        self.rooms.get(room_id)
    }

    pub fn get_room_members(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .get(room_id)
            .map(|r| r.participant_ids())
            .unwrap_or_default()
    }

    /// Get all peers who should receive text chat (participants + subscribers).
    pub fn get_chat_recipients(&self, room_id: &str) -> Vec<String> {
        self.rooms
            .get(room_id)
            .map(|r| r.chat_recipient_ids())
            .unwrap_or_default()
    }

    /// Subscribe a peer to a room's text chat without voice join.
    pub fn subscribe(&mut self, peer_id: &str, room_id: &str) -> bool {
        if let Some(room) = self.rooms.get_mut(room_id) {
            if room.is_peer_allowed(peer_id) {
                room.subscribe(peer_id);
                return true;
            }
        }
        false
    }

    /// Unsubscribe a peer from a room's text chat.
    pub fn unsubscribe(&mut self, peer_id: &str, room_id: &str) {
        if let Some(room) = self.rooms.get_mut(room_id) {
            room.unsubscribe(peer_id);
        }
    }

    /// Get rooms visible to a peer: all public + private rooms they're in.
    pub fn get_rooms_for_peer(&self, peer_id: &str) -> Vec<serde_json::Value> {
        self.rooms
            .values()
            .filter(|r| r.is_public() || r.is_peer_allowed(peer_id))
            .map(|r| {
                serde_json::json!({
                    "room_id": r.room_id,
                    "name": r.room_name,
                    "member_count": r.participant_count(),
                    "room_type": r.room_type,
                })
            })
            .collect()
    }

    /// Validate and consume a room invite token.
    pub fn validate_room_invite(&mut self, room_id: &str, token: &str, peer_id: &str) -> bool {
        let Some(room) = self.rooms.get_mut(room_id) else {
            return false;
        };
        room.validate_and_consume_token(token, peer_id)
    }

    /// Generate an invite token for a room.
    pub fn generate_invite_token(&mut self, room_id: &str, created_by: &str) -> Option<String> {
        self.rooms
            .get_mut(room_id)
            .map(|r| r.generate_invite_token(created_by, 1))
    }

    /// Save user-created rooms to JSON.
    pub fn save_rooms(&self, path: &std::path::Path) -> std::io::Result<()> {
        let rooms: Vec<serde_json::Value> = self
            .rooms
            .values()
            .filter(|r| !r.creator_id.is_empty())
            .map(|r| r.to_value())
            .collect();
        let json = serde_json::to_string_pretty(&rooms).map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Load rooms from JSON. Returns count loaded.
    pub fn load_rooms(&mut self, path: &std::path::Path) -> usize {
        let Ok(data) = std::fs::read_to_string(path) else {
            return 0;
        };
        let Ok(rooms) = serde_json::from_str::<Vec<serde_json::Value>>(&data) else {
            return 0;
        };
        let mut count = 0;
        for r in rooms {
            let room_id = r.get("room_id").and_then(|v| v.as_str()).unwrap_or("");
            let room_name = r.get("room_name").and_then(|v| v.as_str()).unwrap_or("");
            let room_type: RoomType =
                serde_json::from_value(r.get("room_type").cloned().unwrap_or_default())
                    .unwrap_or(RoomType::Public);
            let creator = r.get("creator_id").and_then(|v| v.as_str()).unwrap_or("");
            if !room_id.is_empty() && !self.rooms.contains_key(room_id) {
                self.rooms.insert(
                    room_id.to_string(),
                    SFURoom::new(room_id, room_name, room_type, creator),
                );
                count += 1;
            }
        }
        count
    }

    /// Stats snapshot.
    pub(crate) fn stats(&self) -> SFUStats {
        SFUStats {
            rooms_total: self.rooms.len(),
            participants_total: self.rooms.values().map(|r| r.participant_count()).sum(),
            rooms: self
                .rooms
                .values()
                .map(|r| SFURoomStats {
                    room_id: r.room_id.clone(),
                    name: r.room_name.clone(),
                    room_type: r.room_type,
                    participants: r.participant_count(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SFUStats {
    pub rooms_total: usize,
    pub participants_total: usize,
    pub rooms: Vec<SFURoomStats>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SFURoomStats {
    pub room_id: String,
    pub name: String,
    pub room_type: RoomType,
    pub participants: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_room_lifecycle() {
        let mut mgr = SFURoomManager::new();
        assert!(mgr.get_room(DEFAULT_ROOM_ID).is_some());

        mgr.create_room(Some("test"), "Test Room", RoomType::Public, "creator1");
        let (ok, members) = mgr.join_room("peer1", "test");
        assert!(ok);
        assert_eq!(members.len(), 1);

        let (ok2, members2) = mgr.join_room("peer2", "test");
        assert!(ok2);
        assert_eq!(members2.len(), 2);

        let remaining = mgr.leave_room("peer1", "test");
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_private_room() {
        let mut mgr = SFURoomManager::new();
        mgr.create_room(Some("priv"), "Private", RoomType::Private, "creator");

        // Unauthorized peer can't join
        let (ok, _) = mgr.join_room("stranger", "priv");
        assert!(!ok);

        // Creator can join
        let (ok, _) = mgr.join_room("creator", "priv");
        assert!(ok);

        // Generate invite token
        let token = mgr.generate_invite_token("priv", "creator").unwrap();
        assert!(mgr.validate_room_invite("priv", &token, "friend"));

        let (ok, _) = mgr.join_room("friend", "priv");
        assert!(ok);
    }

    #[test]
    fn test_room_gc() {
        let mut mgr = SFURoomManager::new();
        // Anonymous room (no creator)
        mgr.rooms.insert(
            "anon".to_string(),
            SFURoom::new("anon", "Anon", RoomType::Public, ""),
        );
        mgr.join_room("p1", "anon");
        mgr.leave_room("p1", "anon");
        // Should be GC'd
        assert!(mgr.get_room("anon").is_none());
    }
}
