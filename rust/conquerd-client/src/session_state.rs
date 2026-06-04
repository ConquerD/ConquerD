//! Session state — consolidated per-peer and per-voice session scalars.

use std::time::Instant;

// ---------------------------------------------------------------------------
// ChatPath / ChatHealth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatPath {
    #[default]
    None,
    Direct,
    Relay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatHealth {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Degraded,
}

// ---------------------------------------------------------------------------
// VoiceMode / VoiceQuality
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceMode {
    #[default]
    None,
    DirectPeer,
    Room,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceQuality {
    #[default]
    Unknown,
    Good,
    Fair,
    Poor,
}

// ---------------------------------------------------------------------------
// PeerSessionState
// ---------------------------------------------------------------------------

/// Consolidated state for a single peer session.
#[derive(Debug, Clone, Default)]
pub struct PeerSessionState {
    pub peer_id: String,

    // Chat path
    pub chat_path: ChatPath,
    pub chat_health: ChatHealth,

    // Voice
    pub voice_mode: VoiceMode,
    pub voice_quality: VoiceQuality,
    pub in_call: bool,
    pub muted: bool,
    pub speaking: bool,

    // Data
    pub rtt_ms: Option<f64>,
    pub packet_loss: f64,
    pub jitter_ms: f64,

    // Relay
    pub relay_url: Option<String>,
    pub relay_index: Option<u8>,
}

impl PeerSessionState {
    pub fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// VoiceSessionState
// ---------------------------------------------------------------------------

/// Voice-call session scalars (direct peer or room).
#[derive(Debug, Clone, Default)]
pub struct VoiceSessionState {
    pub active: bool,
    pub mode: VoiceMode,
    pub room_id: Option<String>,
    pub peer_id: Option<String>,
    pub muted: bool,
    pub deafened: bool,
    pub started_at: Option<Instant>,
}

impl VoiceSessionState {
    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn duration_secs(&self) -> Option<u64> {
        self.started_at.map(|t| t.elapsed().as_secs())
    }
}
