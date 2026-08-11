//! Well-known capability descriptors for first-party features.
//!
//! These constructors document the existing on-wire formats that today's
//! Conquerd client and supernode already speak. They are the seed of the
//! capability catalogue and let Phase 1 advertise the current behavior
//! without changing any byte on the wire.

use serde_json::json;

use crate::descriptor::{AuthTier, CapabilityDescriptor, ChannelKind};
use crate::video_codec::{self, VideoCodec};

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
///
/// Byte quota is sized for the **signed JSON wire frame**, not raw Opus:
/// each 20 ms frame is E2E-sealed, base64'd, and wrapped in an Ed25519-signed
/// `SfuAudio` envelope (`[ROOM_AUDIO_TAG][json]`). At the default 128 kbps
/// ceiling that is ~40 KiB/s continuous; 128 KiB/s leaves ~3× headroom for
/// ABR overshoot and bursty speech without the supernode dropping frames
/// (which clients report as 6–30% "packet loss" even when ICMP is clean).
pub fn room_audio_sfu() -> CapabilityDescriptor {
    CapabilityDescriptor::new("room.audio.sfu", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "codec": "opus",
            "quota_bytes_per_sec": 128 * 1024,
            "quota_datagrams_per_sec": 200,
            "allow_public_rooms": false,
            "allow_private_rooms": true,
        }))
        .with_auth(AuthTier::RoomMember)
}

/// Byte quota for **one** video sender, inbound.
///
/// Sized from what a client can actually be configured to emit rather than from
/// a preset: the Video settings page allows a hand-set bitrate up to 8 Mbps, and
/// a quota below that would let the UI offer a rate the relay silently shreds.
/// 8 Mbps of payload plus roughly 6% of fragment header and relay framing is
/// ~1.03 MB/s, so 1.25 MB/s covers the whole settable range with room for the
/// keyframe spike inside the bucket's one-second burst.
///
/// For reference at the auto bitrate, measured with the Media Foundation
/// encoder: 720p30 ≈ 220 KB/s, 1080p30 ≈ 404 KB/s, 1080p60 ≈ 679 KB/s.
///
/// This is the cap that matters for abuse: it is enforced by the supernode on
/// what one peer may push in, and it stays per-sender no matter how large rooms
/// get. See [`ROOM_VIDEO_RECIPIENT_BYTES_PER_SEC`] for the other direction.
pub const VIDEO_SENDER_BYTES_PER_SEC: u32 = 1280 * 1024;

/// Datagram quota for one video sender, inbound.
///
/// [`VIDEO_SENDER_BYTES_PER_SEC`] over the ~1070-byte usable fragment payload is
/// ~1223 datagrams/s; 2000 leaves room for the partial fragment that ends every
/// frame, which inflates the count above what the byte rate alone implies.
pub const VIDEO_SENDER_DATAGRAMS_PER_SEC: u32 = 2000;

/// Concurrent video senders a room is expected to carry.
///
/// Video is opt-in — a member chooses to share and others choose to watch — so
/// this is a deliberate ceiling on a feature people join, not a limit that
/// silently binds an ordinary call. It exists to size the fan-out quota below;
/// nothing enforces the count itself.
pub const ROOM_VIDEO_CONCURRENT_SENDERS: u32 = 5;

/// Byte quota for what the supernode fans **at one room member**.
///
/// Keyed on the recipient, so unlike [`VIDEO_SENDER_BYTES_PER_SEC`] this one
/// bucket carries every *other* sender at once — `ROOM_VIDEO_CONCURRENT_SENDERS
/// - 1` streams, since the relay never echoes a sender their own frames.
///
/// Sized from the 1080p60 auto bitrate (~679 KB/s on the wire) rather than from
/// the 8 Mbps hand-set ceiling: the intent is that every streamer gets a full
/// 1080p stream through, not that four people simultaneously maxing the manual
/// slider are underwritten. A room that does the latter sheds, which is the
/// right answer for that configuration.
///
/// It is a ceiling, not a reservation — nothing is consumed until traffic
/// actually arrives — but it does bound what one room can cost a supernode:
/// five senders fanning to four members each is ~65 Mbps of egress at the
/// 1080p30 auto rate.
pub const ROOM_VIDEO_RECIPIENT_BYTES_PER_SEC: u32 =
    ROOM_VIDEO_STREAM_BYTES_PER_SEC * (ROOM_VIDEO_CONCURRENT_SENDERS - 1);

/// What one received 1080p stream is allowed, before the fan-out multiplier.
///
/// 768 KB/s against a measured 1080p60 wire rate of ~679 KB/s — enough headroom
/// for ABR overshoot and the keyframe spike without underwriting a hand-set
/// bitrate several times larger.
const ROOM_VIDEO_STREAM_BYTES_PER_SEC: u32 = 768 * 1024;

/// Datagram quota for what the supernode fans at one room member. Scales with
/// [`ROOM_VIDEO_RECIPIENT_BYTES_PER_SEC`] on the same reasoning as
/// [`VIDEO_SENDER_DATAGRAMS_PER_SEC`]: ~735 datagrams/s of payload per stream,
/// rounded up to absorb each frame's trailing partial fragment.
pub const ROOM_VIDEO_RECIPIENT_DATAGRAMS_PER_SEC: u32 = 1200 * (ROOM_VIDEO_CONCURRENT_SENDERS - 1);

// The whole point of splitting the two rates is that fan-out gets more than one
// sender does. Checked at compile time rather than in a test: if a later tuning
// pass inverts them, every watcher is silently capped at less than one stream,
// which looks like packet loss rather than a misconfiguration.
const _: () = assert!(ROOM_VIDEO_RECIPIENT_BYTES_PER_SEC > VIDEO_SENDER_BYTES_PER_SEC);
const _: () = assert!(ROOM_VIDEO_RECIPIENT_DATAGRAMS_PER_SEC > VIDEO_SENDER_DATAGRAMS_PER_SEC);

/// `core.video.v1` — direct peer video over QUIC datagrams.
///
/// Unlike audio, one encoded frame spans many datagrams: the sender fragments
/// each frame to fit the 1200-byte relay ceiling. At the 640x360 / 30 fps /
/// ~600 kbps default that is roughly 4 fragments per frame (~120 datagrams/s),
/// spiking to ~25 for a keyframe.
///
/// Sized to [`VIDEO_SENDER_BYTES_PER_SEC`], which covers the whole range the
/// settings page can produce — including 1080p. Direct video has no fan-out, so
/// there is nothing for an outbound override to do here.
///
/// The id names no codec: which codecs this build can actually run is a
/// platform and build-time fact, advertised in `params.codecs` and resolved by
/// [`negotiate`](crate::video_codec::negotiate). See [`crate::video_codec`] for
/// why the codec cannot live in the id.
///
/// This constructor advertises the full known codec set and is what the
/// supernode registers for quota classification (it forwards video opaquely and
/// encodes nothing). A client **must** advertise only what it can really run —
/// use [`core_video_v1_for`].
pub fn core_video_v1() -> CapabilityDescriptor {
    core_video_v1_for(&video_codec::PREFERENCE)
}

/// [`core_video_v1`] restricted to the codecs this build can actually run.
pub fn core_video_v1_for(codecs: &[VideoCodec]) -> CapabilityDescriptor {
    CapabilityDescriptor::new("core.video.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "codecs": video_codec::codec_names(&video_codec::advertised_codecs(codecs)),
            "quota_bytes_per_sec": VIDEO_SENDER_BYTES_PER_SEC,
            "quota_datagrams_per_sec": VIDEO_SENDER_DATAGRAMS_PER_SEC,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `room.video.sfu` — SFU room video via QUIC relay.
///
/// Byte quota covers the **lean binary fragment wire format**, not a JSON
/// envelope: room video deliberately does not reuse the signed-JSON framing
/// that room audio uses. At video frame rates the base64 + envelope + per
/// datagram signature overhead consumes more than half of each datagram and
/// forces a full JSON parse per datagram on the supernode. Each fragment
/// instead carries a compact binary header (`FRAGMENT_VERSION` `0x03`: codec +
/// `pts_us`, ~64 bytes before the signature for a typical sender id), and
/// authenticity comes from one Ed25519 signature per *frame* (carried in
/// fragment 0) rather than one per datagram.
///
/// Codec selection works as for [`core_video_v1`], with one difference that
/// matters: a room sender fans out to every member at once, so there may be no
/// single codec all of them decode. The sender stamps the codec on each frame
/// and a member that cannot decode it drops the frame — video degrades for that
/// member only, rather than the room failing to negotiate.
pub fn room_video_sfu() -> CapabilityDescriptor {
    room_video_sfu_for(&video_codec::PREFERENCE)
}

/// [`room_video_sfu`] restricted to the codecs this build can actually run.
pub fn room_video_sfu_for(codecs: &[VideoCodec]) -> CapabilityDescriptor {
    CapabilityDescriptor::new("room.video.sfu", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "codecs": video_codec::codec_names(&video_codec::advertised_codecs(codecs)),
            // Inbound is per *sender*: one peer's stream, and the cap that
            // stops one client flooding the relay.
            "quota_bytes_per_sec": VIDEO_SENDER_BYTES_PER_SEC,
            "quota_datagrams_per_sec": VIDEO_SENDER_DATAGRAMS_PER_SEC,
            // Outbound is per *recipient*: every other sender at once. Raising
            // the inbound number to cover fan-out instead would have handed
            // each individual sender the whole room's allowance.
            "quota_bytes_per_sec_outbound": ROOM_VIDEO_RECIPIENT_BYTES_PER_SEC,
            "quota_datagrams_per_sec_outbound": ROOM_VIDEO_RECIPIENT_DATAGRAMS_PER_SEC,
            "allow_public_rooms": false,
            "allow_private_rooms": true,
        }))
        .with_auth(AuthTier::RoomMember)
}

/// `core.audio.content.v1` — direct peer content audio.
///
/// System or application audio that belongs *with* the video — a game, a
/// browser tab, a track — carried separately from the call microphone rather
/// than mixed into it. Two reasons it is its own stream:
///
/// * **It is synchronised.** Every frame carries a presentation timestamp on
///   the sender's session clock, and video is slaved to it. The mic path has no
///   timestamp and is deliberately left alone.
/// * **It is encoded differently.** Speech settings — `Application::Voip`, a
///   noise gate, voice-activity gating — mangle or suppress music and game
///   audio. This stream uses `Application::Audio` at a higher rate, and one
///   Opus encoder cannot be in both modes at once.
///
/// Quota sits above voice because the bitrate is higher and frames are not
/// gated by voice activity, so the stream is continuous rather than bursty.
pub fn core_audio_content_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("core.audio.content.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "codec": "opus",
            "opus_application": "audio",
            "av_sync": 1,
            "pts_unit": "us",
            "quota_bytes_per_sec": 64 * 1024,
            "quota_datagrams_per_sec": 200,
        }))
        .with_auth(AuthTier::TrustedPeer)
}

/// `room.audio.content.sfu` — content audio relayed through a supernode.
///
/// See [`core_audio_content_v1`]. The supernode forwards these frames opaquely
/// and never parses the timestamp: teaching the SFU a media timeline would give
/// it a reason to inspect content it is not trusted with.
pub fn room_audio_content_sfu() -> CapabilityDescriptor {
    CapabilityDescriptor::new("room.audio.content.sfu", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "codec": "opus",
            "opus_application": "audio",
            "av_sync": 1,
            "pts_unit": "us",
            "quota_bytes_per_sec": 64 * 1024,
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

/// `game.relay.v1` — opaque datagram relay for in-app portal games.
/// The supernode fans inbound QUIC-relay datagrams (fixed channel tag
/// `GAME_RELAY_TAG`) to peers that joined the same portal game session via
/// `GameRelayJoin`, without interpreting the payload.
pub fn game_relay_v1() -> CapabilityDescriptor {
    CapabilityDescriptor::new("game.relay.v1", "1.0", ChannelKind::Datagram)
        .with_params(json!({
            "relay": "opaque",
            "scope": "session",
            "channel_tag": "fixed",
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
        core_video_v1(),
        core_audio_content_v1(),
        room_audio_content_sfu(),
        room_video_sfu(),
        room_chat_v1(),
        room_file_v1(),
        web_host_app_v1(),
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

    /// Wire-protocol feature-ID stability guard.
    ///
    /// These strings are sent on the wire during capability negotiation.
    /// Renaming or removing one silently drops interoperability with peers
    /// that already know the old name. To make a breaking change:
    ///   1. Add the new ID alongside the old one in `local_capabilities()`.
    ///   2. Keep the old ID until all peers have migrated.
    ///   3. Remove the old ID and update this list in the same PR.
    #[test]
    fn feature_ids_are_stable() {
        let caps = super::local_capabilities();
        let ids: Vec<&str> = caps.iter().map(|c| c.id.as_str()).collect();
        let expected = [
            "transport.quic.audio.v1",
            "transport.quic.relay.v1",
            "transport.quic.stream.v1",
            "transport.quic.feature_datagram.v1",
            "transport.quic.uni_stream.v1",
            "transport.quic.stream_priority.v1",
            "transport.quic.zero_rtt.v1",
            "transport.quic.pmtud.v1",
            "transport.quic.migration.v1",
            "transport.quic.flow_control.v1",
            "core.chat.v1",
            "core.file.v1",
            "core.audio.opus",
            "room.audio.sfu",
            // Added for video calling. Purely additive: peers that predate
            // these simply won't advertise them, and negotiation degrades to
            // audio-only, so no migration window is needed.
            "core.video.v1",
            "core.audio.content.v1",
            "room.audio.content.sfu",
            "room.video.sfu",
            "room.chat.v1",
            "room.file.v1",
            "web.host.app.v1",
            "game.relay.v1",
        ];
        assert_eq!(
            ids, expected,
            "local_capabilities() ID list changed — update this test AND \
             coordinate the change with all peers before shipping"
        );
    }
}
