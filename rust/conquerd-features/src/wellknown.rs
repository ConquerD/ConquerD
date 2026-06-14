//! Well-known capability descriptors for first-party features.
//!
//! These constructors document the existing on-wire formats that today's
//! Conquerd client and supernode already speak. They are the seed of the
//! capability catalogue and let Phase 1 advertise the current behavior
//! without changing any byte on the wire.

use serde_json::json;

use crate::descriptor::{AuthTier, CapabilityDescriptor, ChannelKind};

// ── Reserved namespace prefixes (documented for tooling/lints). ──────────────
pub const NS_CORE: &str = "core";
pub const NS_TRANSPORT: &str = "transport";
pub const NS_ROOM: &str = "room";
pub const NS_WEB: &str = "web";
pub const NS_GAME: &str = "game";
pub const NS_VENDOR: &str = "x";

// ── Transport-layer capabilities (describe existing wire formats). ───────────

/// `transport.quic.audio.v1` — direct-peer audio datagram framing
/// (`[u16 BE seq][opus...]`) currently implemented in
/// `conquerd-quic::wire::encode_audio_datagram`.
pub fn transport_quic_audio_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.audio.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "framing": "u16be_seq + opaque",
            "seq_bytes": 2,
            "max_payload_hint": 1200,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.relay.v1` — supernode broadcast/forward datagram
/// framing (`[peer_index][opus...]`, `0xFF` = broadcast) implemented in
/// `conquerd-quic::wire::{encode_relay_broadcast,decode_relay_datagram}`.
pub fn transport_quic_relay_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.relay.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "framing": "u8_peer_index + opaque",
            "broadcast_index": 0xFF,
        }))
        .with_auth(AuthTier::RoomMember)
}

/// `transport.quic.stream.v1` — generic length-prefixed stream framing
/// (`[u32 BE len][data]`) implemented in `conquerd-quic::wire::StreamBuffer`.
pub fn transport_quic_stream_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.stream.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "framing": "u32be_len + opaque",
            "max_frame_bytes": 16 * 1024 * 1024,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.feature_datagram.v1` — tagged datagram framing
/// (`[u8 tag][payload]`) used by the channel multiplexer to share one
/// QUIC connection between multiple datagram features. The tag space is
/// owned by [`crate::ChannelTagRegistry`]: `0x10..=0xEF` are dynamically
/// allocated per session, `0xFF` remains reserved for the legacy
/// `transport.quic.relay.v1` broadcast indicator.
pub fn transport_quic_feature_datagram_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new(
        "transport.quic.feature_datagram.v1",
        "1.0",
        ChannelKind::Datagram,
    )
    .with_params(json!({
        "framing": "u8_tag + opaque",
        "tag_dynamic_start": 0x10,
        "tag_dynamic_end": 0xEF,
        "tag_broadcast": 0xFF,
    }))
    .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.uni_stream.v1` — tagged unidirectional stream framing
/// (`[u8 tag][payload...]`) for reliable ordered messages on the generic
/// channel multiplexer. The tag space is shared with
/// `transport.quic.feature_datagram.v1`; both allocate from
/// [`crate::ChannelTagRegistry`] (range `0x10..=0xEF`).
pub fn transport_quic_uni_stream_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.uni_stream.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "framing": "u8_tag + opaque",
            "tag_dynamic_start": 0x10,
            "tag_dynamic_end": 0xEF,
            "direction": "unidirectional",
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.stream_priority.v1` — advisory stream priority hints for
/// outbound streams. Lower numbers mean higher priority (quinn convention).
/// Typical values: `-100` (voice / real-time) through `0` (default) to `100`
/// (bulk file transfer).
pub fn transport_quic_stream_priority_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new(
        "transport.quic.stream_priority.v1",
        "1.0",
        ChannelKind::Stream,
    )
    .with_params(json!({
        "hint_type": "i32",
        "lower_is_higher_priority": true,
    }))
    .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.zero_rtt.v1` — 0-RTT session resumption support.
///
/// The client caches TLS session tickets (via PSK resumption) and attempts
/// to open 0-RTT connections on reconnect. The server accepts early data
/// when `max_early_data_size` is non-zero. If the server rejects early data,
/// quinn transparently retransmits it in the 1-RTT handshake.
/// Stats are tracked in `QUICTransport.stats["zero_rtt_attempted"]` and
/// `QUICTransport.stats["zero_rtt_accepted"]`.
pub fn transport_quic_zero_rtt_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.zero_rtt.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "client_session_cache": 64,
            "server_early_data": true,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.pmtud.v1` — path MTU discovery via quinn's built-in PMTUD.
///
/// The effective maximum datagram payload size starts at `MAX_DATAGRAM_SIZE`
/// (1 200 B) and grows as larger paths are probed and confirmed. Callers
/// should read the current value via `QUICTransport.get_max_datagram_size(peer_id)`
/// and adapt their payload sizes accordingly.
pub fn transport_quic_pmtud_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.pmtud.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "min_payload_bytes": 1200,
            "query_api": "get_max_datagram_size",
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.migration.v1` — QUIC connection migration support.
///
/// Quinn handles path migration automatically: when the local or remote
/// network address changes (e.g. Wi-Fi → cellular), the QUIC connection
/// survives and traffic resumes on the new path. The current live remote
/// address is readable via `QUICTransport.get_connection_path(peer_id)`.
pub fn transport_quic_migration_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.migration.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "automatic": true,
            "query_api": "get_connection_path",
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `transport.quic.flow_control.v1` — tuned QUIC flow-control windows.
///
/// All three quinn config variants (server, P2P client, relay client) are
/// configured with explicit windows: 8 MB connection-level receive/send,
/// 2 MB per-stream receive. This allows high-throughput asset streaming
/// without needing per-connection tuning.
pub fn transport_quic_flow_control_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("transport.quic.flow_control.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "receive_window_bytes": 8 * 1024 * 1024,
            "stream_receive_window_bytes": 2 * 1024 * 1024,
            "send_window_bytes": 8 * 1024 * 1024,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

// ── First-party feature capabilities. ────────────────────────────────────────

/// `core.chat.v1` — signed chat envelope on the signaling channel.
pub fn core_chat_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream)
        // Chat is bursty but tiny: ~32 KB/s, ~50 messages/s is plenty.
        .with_params(json!({
            "quota_bytes_per_sec": 32 * 1024,
            "quota_datagrams_per_sec": 50,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `core.file.v1` — chunked file transfer.
pub fn core_file_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("core.file.v1", "1.0", ChannelKind::Stream)
        // File transfer is throughput-bound; allow 8 MB/s headroom.
        .with_params(json!({
            "quota_bytes_per_sec": 8 * 1024 * 1024,
            "quota_datagrams_per_sec": 4096,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `core.audio.opus` — direct peer voice (Opus over `transport.quic.audio.v1`).
pub fn core_audio_opus() -> CapabilityDescriptor {
    CapabilityDescriptor::new("core.audio.opus", "1.0", ChannelKind::Datagram)
        // 64 kbps Opus + overhead, 50 frames/s (20 ms). Cap at ~3×.
        .with_params(json!({
            "codec": "opus",
            "quota_bytes_per_sec": 32 * 1024,
            "quota_datagrams_per_sec": 200,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `room.audio.sfu` — SFU room voice via QUIC relay.
pub fn room_audio_sfu() -> CapabilityDescriptor {
    CapabilityDescriptor::new("room.audio.sfu", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "codec": "opus",
            "quota_bytes_per_sec": 32 * 1024,
            "quota_datagrams_per_sec": 200,
            "allow_public_rooms": false,
            "allow_private_rooms": true,
        }))
        .with_auth(AuthTier::RoomMember)
}

/// `room.chat.v1` — SFU room text chat.
pub fn room_chat_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("room.chat.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "quota_bytes_per_sec": 32 * 1024,
            "quota_datagrams_per_sec": 50,
        }))
        .with_auth(AuthTier::RoomMember)
}

/// `room.file.v1` — chunked room file broadcast via an SFU supernode.
pub fn room_file_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("room.file.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "quota_bytes_per_sec": 8 * 1024 * 1024,
            "quota_datagrams_per_sec": 4096,
        }))
        .with_auth(AuthTier::RoomMember)
}

/// `web.host.app.v1` — supernode in-app portal hosted over QUIC reliable
/// streams. This surface is *not* reachable from a standard browser: the
/// desktop client opens the portal in an embedded Chromium view via the
/// custom `conquerd://<supernode_pub>/<path>` URL scheme, and the scheme
/// handler issues GET requests over a QUIC bidirectional stream tagged
/// with `web.host.app.v1`.
///
/// Wire shape (see `web_app.rs`):
///
/// * Client → supernode: one length-prefixed `WebAppRequest` JSON frame
///   carrying `{ path, method = "GET" }`. The QUIC connection is already
///   identity-verified (the supernode knows which Ed25519 pub key opened
///   the stream), so individual requests are *not* re-signed.
/// * Supernode → client: one `WebAppResponseHeader` JSON frame
///   (`{ status, content_type, total_len }`), followed by zero or more
///   length-prefixed binary body chunks, terminated by a zero-length
///   chunk.
///
/// Auth is `AuthTier::Public` because the QUIC handshake itself is the
/// identity gate. Quotas default to a generous 4 MB/s / 256 stream-frames
/// per second to keep pages snappy without letting a runaway page exhaust
/// the supernode.
pub fn web_host_app_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("web.host.app.v1", "1.0", ChannelKind::Stream)
        .with_params(json!({
            "scheme": "conquerd",
            "framing": "u32be_len + json | u32be_len + bytes",
            "methods": ["GET"],
            "quota_bytes_per_sec": 4 * 1024 * 1024,
            "quota_datagrams_per_sec": 256,
        }))
        .with_auth(AuthTier::Public)
}

/// `web.host.h3.v1` — supernode HTTP/3 + WebTransport host. Browsers
/// connect over WebTransport and bridge their feature datagrams onto the
/// supernode's channel-tag fabric. TLS cert is loaded from
/// `<data_dir>/web_cert.pem` / `web_key.pem`; `port` defaults to the
/// existing `supernode_web_port` env var.
pub fn web_host_h3_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("web.host.h3.v1", "1.0", ChannelKind::Datagram)
        .with_params(serde_json::json!({
            "transport": "webtransport",
            "alpn": "h3",
            "datagram_framing": "u8_tag + opaque",
        }))
        .with_auth(AuthTier::Public)
}

/// `game.relay.v1` — opaque datagram relay for game state. The supernode
/// fans inbound datagrams from each browser (or native) peer to every other
/// participant in the same game session (identified by the `room` parameter
/// in the WebTransport request path) without interpreting the payload.
/// Games control their own serialization; the relay only enforces:
///
/// * tag is in the dynamic range `0x10..=0xEF`
/// * source peer has a verified identity (handshake completed)
/// * source peer has advertised `game.relay.v1`
/// * participants share the same session id
pub fn game_relay_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("game.relay.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "relay": "opaque",
            "scope": "session",
        }))
        .with_auth(AuthTier::RoomMember)
}

// ── Public catalogue ──────────────────────────────────────────────────────────

/// The canonical list of well-known capabilities this client advertises after
/// handshake. **Both sides MUST be updated in lock-step when the well-known list changes.**
pub fn local_capabilities() -> Vec<CapabilityDescriptor> {
    vec![
        transport_quic_audio_v1(),
        transport_quic_relay_v1(),
        transport_quic_stream_v1(),
        transport_quic_feature_datagram_v1(),
        transport_quic_uni_stream_v1(),
        transport_quic_stream_priority_v1(),
        transport_quic_zero_rtt_v1(),
        transport_quic_pmtud_v1(),
        transport_quic_migration_v1(),
        transport_quic_flow_control_v1(),
        core_chat_v1(),
        core_file_v1(),
        core_audio_opus(),
        room_audio_sfu(),
        room_chat_v1(),
        room_file_v1(),
        web_host_app_v1(),
        web_host_h3_v1(),
        game_relay_v1(),
    ]
}

#[cfg(test)]
mod tests {
    #[test]
    fn all_well_known_descriptors_validate() {
        for cap in super::local_capabilities() {
            cap.validate()
                .unwrap_or_else(|e| panic!("{}: {}", cap.id, e));
        }
    }
}
