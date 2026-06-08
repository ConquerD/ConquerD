//! Banner formatter — pure helpers for the session status banner.
//!
//! All functions are pure (no I/O, no async). The UI layer calls these to
//! compose the banner HTML from live connection state.

// ---------------------------------------------------------------------------
// Color palette
// ---------------------------------------------------------------------------

pub const COLOR_GREEN: &str = "#57F287";
pub const COLOR_YELLOW: &str = "#FEE75C";
pub const COLOR_RED: &str = "#ED4245";
pub const COLOR_BLUE: &str = "#5865F2";
pub const COLOR_GRAY: &str = "#B9BBBE";

// ---------------------------------------------------------------------------
// Color helpers
// ---------------------------------------------------------------------------

pub fn color_for_chat_health(value: &str) -> &'static str {
    match value {
        "healthy" => COLOR_GREEN,
        "degraded" => COLOR_YELLOW,
        "offline" => COLOR_RED,
        _ => COLOR_GRAY,
    }
}

pub fn color_for_chat_path(value: &str) -> &'static str {
    match value {
        "direct" => COLOR_GREEN,
        "relay" => COLOR_BLUE,
        _ => COLOR_GRAY,
    }
}

pub fn color_for_data(value: &str) -> &'static str {
    match value {
        "ready" => COLOR_GREEN,
        "ws-only" => COLOR_YELLOW,
        "closed" => COLOR_RED,
        _ => COLOR_GRAY,
    }
}

pub fn color_for_voice(state: &str, quality: &str) -> &'static str {
    match state {
        "connected" => match quality {
            "excellent" | "good" => COLOR_GREEN,
            "fair" => COLOR_YELLOW,
            "poor" => COLOR_RED,
            _ => COLOR_GRAY,
        },
        "connecting" | "new" | "checking" => COLOR_YELLOW,
        "failed" | "disconnected" | "closed" => COLOR_RED,
        _ => COLOR_GRAY,
    }
}

// ---------------------------------------------------------------------------
// VoiceStatus
// ---------------------------------------------------------------------------

/// A secondary line in the banner: `(text, Option<color>)`.
/// `color == None` means the text is already a fully-formatted HTML fragment.
pub type SecondaryLine = (String, Option<&'static str>);

/// Resolved voice status for the call panel.
#[derive(Debug, Clone)]
pub struct VoiceStatus {
    /// Primary status string, e.g. `"connected/quic-relay"`.
    pub voice_part: String,
    /// Matching banner color.
    pub voice_color: &'static str,
    /// Optional secondary lines to append.
    pub secondary_lines: Vec<SecondaryLine>,
}

// ---------------------------------------------------------------------------
// Room voice status
// ---------------------------------------------------------------------------

/// Compose the call-panel status for a *room* call.
///
/// Room calls flow through a supernode using either the QUIC relay
/// (`"quic-relay"`) or the WebSocket relay fallback (`"ws-relay"`).
pub fn resolve_room_voice_status(
    call_state_in_call: bool,
    call_state_connecting: bool,
    quic_relay_connected: bool,
    supernode_label: &str,
    transport_note: &str,
) -> VoiceStatus {
    let (state, color) = if call_state_in_call {
        ("connected", COLOR_GREEN)
    } else if call_state_connecting {
        ("connecting", COLOR_YELLOW)
    } else {
        ("idle", COLOR_GRAY)
    };

    let transport = if quic_relay_connected {
        "quic-relay"
    } else {
        "ws-relay"
    };
    let voice_part = if state == "connected" {
        format!("connected/{transport}")
    } else {
        state.to_string()
    };

    let mut secondary: Vec<SecondaryLine> = Vec::new();
    if !supernode_label.is_empty() {
        secondary.push((format!("SN: {supernode_label}"), Some(COLOR_BLUE)));
    }
    if !transport_note.is_empty() {
        secondary.push((transport_note.to_string(), Some(COLOR_RED)));
    }

    VoiceStatus {
        voice_part,
        voice_color: color,
        secondary_lines: secondary,
    }
}

// ---------------------------------------------------------------------------
// Direct peer voice status
// ---------------------------------------------------------------------------

/// Compose the call-panel status for a *direct* peer-to-peer call.
pub fn resolve_direct_voice_status(voice_state: &str, voice_quality: &str) -> VoiceStatus {
    let (voice_part, color) = if voice_state == "connected" {
        (
            format!("connected/{voice_quality}"),
            color_for_voice("connected", voice_quality),
        )
    } else if matches!(voice_state, "connecting" | "new" | "checking") {
        (voice_state.to_string(), COLOR_YELLOW)
    } else if matches!(voice_state, "failed" | "disconnected" | "closed") {
        (voice_state.to_string(), COLOR_RED)
    } else {
        ("idle".to_string(), COLOR_GRAY)
    };

    VoiceStatus {
        voice_part,
        voice_color: color,
        secondary_lines: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Banner summary
// ---------------------------------------------------------------------------

/// Full banner summary used by the session header.
#[derive(Debug, Clone)]
pub struct BannerSummary {
    /// Connection path text, e.g. `"direct"` or `"relay"`.
    pub path: String,
    pub path_color: &'static str,
    /// Health text, e.g. `"healthy"` / `"degraded"` / `"offline"`.
    pub health: String,
    pub health_color: &'static str,
    /// Voice mode string, e.g. `"connected/excellent"`.
    pub voice: String,
    pub voice_color: &'static str,
}

/// Build a [`BannerSummary`] from raw session state values.
pub fn compose_banner(
    chat_path: &str,   // "direct" | "relay" | ""
    chat_health: &str, // "healthy" | "degraded" | "offline" | ""
    voice_state: &str,
    voice_quality: &str,
    in_room: bool,
    quic_relay_connected: bool,
) -> BannerSummary {
    let voice_status = if in_room {
        resolve_room_voice_status(
            voice_state == "connected",
            voice_state == "connecting",
            quic_relay_connected,
            "",
            "",
        )
    } else {
        resolve_direct_voice_status(voice_state, voice_quality)
    };

    BannerSummary {
        path: chat_path.to_string(),
        path_color: color_for_chat_path(chat_path),
        health: chat_health.to_string(),
        health_color: color_for_chat_health(chat_health),
        voice: voice_status.voice_part,
        voice_color: voice_status.voice_color,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_excellent_is_green() {
        let s = resolve_direct_voice_status("connected", "excellent");
        assert_eq!(s.voice_part, "connected/excellent");
        assert_eq!(s.voice_color, COLOR_GREEN);
    }

    #[test]
    fn direct_poor_is_red() {
        let s = resolve_direct_voice_status("connected", "poor");
        assert_eq!(s.voice_color, COLOR_RED);
    }

    #[test]
    fn room_quic_relay_label() {
        let s = resolve_room_voice_status(true, false, true, "Node-1", "");
        assert_eq!(s.voice_part, "connected/quic-relay");
        assert_eq!(s.voice_color, COLOR_GREEN);
        assert_eq!(s.secondary_lines[0].0, "SN: Node-1");
    }

    #[test]
    fn room_ws_fallback() {
        let s = resolve_room_voice_status(true, false, false, "", "");
        assert_eq!(s.voice_part, "connected/ws-relay");
    }

    #[test]
    fn compose_banner_direct_healthy() {
        let b = compose_banner("direct", "healthy", "connected", "good", false, false);
        assert_eq!(b.path_color, COLOR_GREEN);
        assert_eq!(b.health_color, COLOR_GREEN);
        assert!(b.voice.starts_with("connected/"));
    }
}
