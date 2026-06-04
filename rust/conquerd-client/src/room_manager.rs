//! Room manager — per-participant state for voice rooms.
//!
//! Tracks connection status, speaking state, audio level, and connection
//! quality for up to MAX_ROOM_SIZE participants. No I/O — pure in-memory
//! state managed by the owning task.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

pub const MAX_ROOM_SIZE: usize = 8;

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantState {
    Joining,
    Connected,
    Speaking,
    Muted,
    Disconnected,
}

impl ParticipantState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Joining => "joining",
            Self::Connected => "connected",
            Self::Speaking => "speaking",
            Self::Muted => "muted",
            Self::Disconnected => "disconnected",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionQuality {
    Excellent,
    Good,
    Fair,
    Poor,
    Unknown,
}

impl ConnectionQuality {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Excellent => "excellent",
            Self::Good => "good",
            Self::Fair => "fair",
            Self::Poor => "poor",
            Self::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Participant
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Participant {
    pub peer_id: String,
    pub display_name: String,
    pub state: ParticipantState,
    pub speaking: bool,
    /// Normalized audio level 0.0–1.0.
    pub audio_level: f32,
    pub muted: bool,
    pub quality: ConnectionQuality,
    pub is_self: bool,
    pub joined_at: f64,
}

impl Participant {
    pub fn new(peer_id: impl Into<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Self {
            peer_id: peer_id.into(),
            display_name: String::new(),
            state: ParticipantState::Joining,
            speaking: false,
            audio_level: 0.0,
            muted: false,
            quality: ConnectionQuality::Unknown,
            is_self: false,
            joined_at: now,
        }
    }

    pub fn short_id(&self) -> &str {
        &self.peer_id[..self.peer_id.len().min(16)]
    }

    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            self.short_id()
        } else {
            &self.display_name
        }
    }
}

// ---------------------------------------------------------------------------
// RoomManager
// ---------------------------------------------------------------------------

/// In-memory state machine for a single active room session.
///
/// Call [`RoomManager::new`] when joining a room and drop when leaving.
/// All methods are synchronous — designed to be owned by a single async task
/// and updated from inbound signaling events.
pub struct RoomManager {
    pub room_id: String,
    pub room_name: String,
    local_peer_id: String,
    participants: HashMap<String, Participant>,
}

impl RoomManager {
    pub fn new(
        room_id: impl Into<String>,
        room_name: impl Into<String>,
        local_peer_id: impl Into<String>,
    ) -> Self {
        Self {
            room_id: room_id.into(),
            room_name: room_name.into(),
            local_peer_id: local_peer_id.into(),
            participants: HashMap::new(),
        }
    }

    // -- Participant management ---------------------------------------------

    pub fn add_participant(&mut self, peer_id: impl Into<String>, display_name: &str) {
        let peer_id = peer_id.into();
        if self.participants.len() >= MAX_ROOM_SIZE {
            warn!(
                "Room '{}' is at capacity ({})",
                self.room_name, MAX_ROOM_SIZE
            );
        }
        let mut p = Participant::new(&peer_id);
        p.display_name = display_name.to_string();
        p.is_self = peer_id == self.local_peer_id;
        info!(
            "Participant joined room '{}': {} ({})",
            self.room_name,
            display_name,
            &peer_id[..peer_id.len().min(12)]
        );
        self.participants.insert(peer_id, p);
    }

    pub fn remove_participant(&mut self, peer_id: &str) -> Option<Participant> {
        let removed = self.participants.remove(peer_id);
        if removed.is_some() {
            info!(
                "Participant left room '{}': {}",
                self.room_name,
                &peer_id[..peer_id.len().min(12)]
            );
        }
        removed
    }

    pub fn get_participant(&self, peer_id: &str) -> Option<&Participant> {
        self.participants.get(peer_id)
    }

    pub fn get_participant_mut(&mut self, peer_id: &str) -> Option<&mut Participant> {
        self.participants.get_mut(peer_id)
    }

    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.participants.is_empty()
    }

    pub fn list_participants(&self) -> Vec<&Participant> {
        let mut v: Vec<&Participant> = self.participants.values().collect();
        v.sort_by(|a, b| {
            a.joined_at
                .partial_cmp(&b.joined_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    // -- State updates ------------------------------------------------------

    pub fn set_participant_state(&mut self, peer_id: &str, state: ParticipantState) {
        if let Some(p) = self.participants.get_mut(peer_id) {
            debug!(
                "Room '{}' peer {} state: {:?}",
                self.room_name,
                p.short_id(),
                state
            );
            p.state = state;
        }
    }

    pub fn set_speaking(&mut self, peer_id: &str, speaking: bool) {
        if let Some(p) = self.participants.get_mut(peer_id) {
            p.speaking = speaking;
            if speaking {
                p.state = ParticipantState::Speaking;
            } else if p.muted {
                p.state = ParticipantState::Muted;
            } else {
                p.state = ParticipantState::Connected;
            }
        }
    }

    pub fn set_audio_level(&mut self, peer_id: &str, level: f32) {
        if let Some(p) = self.participants.get_mut(peer_id) {
            p.audio_level = level.clamp(0.0, 1.0);
        }
    }

    pub fn set_muted(&mut self, peer_id: &str, muted: bool) {
        if let Some(p) = self.participants.get_mut(peer_id) {
            p.muted = muted;
            if muted {
                p.state = ParticipantState::Muted;
                p.speaking = false;
            } else if p.state == ParticipantState::Muted {
                p.state = ParticipantState::Connected;
            }
        }
    }

    pub fn set_quality(&mut self, peer_id: &str, quality: ConnectionQuality) {
        if let Some(p) = self.participants.get_mut(peer_id) {
            p.quality = quality;
        }
    }

    // -- Queries ------------------------------------------------------------

    pub fn speaking_participants(&self) -> Vec<&Participant> {
        self.participants.values().filter(|p| p.speaking).collect()
    }

    pub fn connected_peers(&self) -> Vec<&str> {
        self.participants
            .values()
            .filter(|p| {
                matches!(
                    p.state,
                    ParticipantState::Connected | ParticipantState::Speaking
                )
            })
            .map(|p| p.peer_id.as_str())
            .collect()
    }

    pub fn is_full(&self) -> bool {
        self.participants.len() >= MAX_ROOM_SIZE
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_remove_participant() {
        let mut mgr = RoomManager::new("room-1", "Test Room", "local-peer");
        mgr.add_participant("peer-a", "Alice");
        mgr.add_participant("peer-b", "Bob");
        assert_eq!(mgr.participant_count(), 2);

        mgr.remove_participant("peer-a");
        assert_eq!(mgr.participant_count(), 1);
        assert!(mgr.get_participant("peer-a").is_none());
    }

    #[test]
    fn speaking_state_transitions() {
        let mut mgr = RoomManager::new("room-2", "Voice Room", "local");
        mgr.add_participant("peer-x", "Xavier");

        mgr.set_speaking("peer-x", true);
        assert_eq!(
            mgr.get_participant("peer-x").unwrap().state,
            ParticipantState::Speaking
        );

        mgr.set_speaking("peer-x", false);
        assert_eq!(
            mgr.get_participant("peer-x").unwrap().state,
            ParticipantState::Connected
        );

        mgr.set_muted("peer-x", true);
        assert_eq!(
            mgr.get_participant("peer-x").unwrap().state,
            ParticipantState::Muted
        );
        assert!(!mgr.get_participant("peer-x").unwrap().speaking);
    }

    #[test]
    fn list_is_sorted_by_join_time() {
        let mut mgr = RoomManager::new("room-3", "R", "local");
        mgr.add_participant("p1", "First");
        // Small sleep to ensure different timestamps in test
        std::thread::sleep(std::time::Duration::from_millis(5));
        mgr.add_participant("p2", "Second");

        let list = mgr.list_participants();
        assert_eq!(list[0].peer_id, "p1");
        assert_eq!(list[1].peer_id, "p2");
    }
}
