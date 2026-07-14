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

impl ChatPath {
    /// Banner / `connection_mode` wire string.
    pub fn as_mode_str(self) -> &'static str {
        match self {
            ChatPath::Direct => "direct",
            ChatPath::Relay => "relay",
            ChatPath::None => "offline",
        }
    }
}

/// Derive path + health from live transport facts (pure helper for CM + tests).
pub fn derive_session_path_health(
    direct_connected: bool,
    direct_connecting: bool,
    relay_available: bool,
    rtt_ms: Option<f64>,
    packet_loss_pct: f64,
) -> (ChatPath, ChatHealth) {
    if direct_connected {
        let health = classify_link_health(rtt_ms, packet_loss_pct);
        return (ChatPath::Direct, health);
    }
    if direct_connecting {
        return (ChatPath::Direct, ChatHealth::Connecting);
    }
    if relay_available {
        // Relay path: use loss/RTT when we have supernode ping stats, else Connected.
        let health = match rtt_ms {
            Some(rtt) => classify_link_health(Some(rtt), packet_loss_pct),
            None => ChatHealth::Connected,
        };
        return (ChatPath::Relay, health);
    }
    (ChatPath::None, ChatHealth::Disconnected)
}

/// Map RTT + loss into a coarse health band.
pub fn classify_link_health(rtt_ms: Option<f64>, packet_loss_pct: f64) -> ChatHealth {
    let rtt = rtt_ms.unwrap_or(0.0);
    if packet_loss_pct >= 15.0 || rtt >= 400.0 {
        ChatHealth::Degraded
    } else {
        ChatHealth::Connected
    }
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

    /// Build a snapshot for a direct peer or supernode id.
    ///
    /// Each argument is one independent transport fact (direct QUIC state,
    /// relay availability, link stats, voice mode); bundling them into a
    /// struct would just move the same field list one level down.
    #[allow(clippy::too_many_arguments)]
    pub fn from_transport(
        peer_id: impl Into<String>,
        direct_connected: bool,
        direct_connecting: bool,
        relay_available: bool,
        rtt_ms: Option<f64>,
        packet_loss: f64,
        jitter_ms: f64,
        voice_mode: VoiceMode,
    ) -> Self {
        let peer_id = peer_id.into();
        let (chat_path, chat_health) = derive_session_path_health(
            direct_connected,
            direct_connecting,
            relay_available,
            rtt_ms,
            packet_loss,
        );
        let voice_quality = match chat_health {
            ChatHealth::Connected => VoiceQuality::Good,
            ChatHealth::Degraded => VoiceQuality::Poor,
            ChatHealth::Connecting => VoiceQuality::Fair,
            ChatHealth::Disconnected => VoiceQuality::Unknown,
        };
        Self {
            peer_id,
            chat_path,
            chat_health,
            voice_mode,
            voice_quality,
            in_call: voice_mode != VoiceMode::None,
            muted: false,
            speaking: false,
            rtt_ms,
            packet_loss,
            jitter_ms,
            relay_url: None,
            relay_index: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_prefers_direct_over_relay() {
        let (p, h) = derive_session_path_health(true, false, true, Some(40.0), 0.0);
        assert_eq!(p, ChatPath::Direct);
        assert_eq!(h, ChatHealth::Connected);
    }

    #[test]
    fn connecting_direct_reports_connecting() {
        let (p, h) = derive_session_path_health(false, true, true, None, 0.0);
        assert_eq!(p, ChatPath::Direct);
        assert_eq!(h, ChatHealth::Connecting);
    }

    #[test]
    fn relay_when_no_direct() {
        let (p, h) = derive_session_path_health(false, false, true, Some(80.0), 1.0);
        assert_eq!(p, ChatPath::Relay);
        assert_eq!(h, ChatHealth::Connected);
    }

    #[test]
    fn high_loss_is_degraded() {
        assert_eq!(classify_link_health(Some(50.0), 20.0), ChatHealth::Degraded);
        assert_eq!(classify_link_health(Some(500.0), 0.0), ChatHealth::Degraded);
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
