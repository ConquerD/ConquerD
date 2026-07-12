//! Fixed first-party channel tags and the tagged-frame codec.
//!
//! The QUIC peer fabric multiplexes several logical channels over one
//! physical path (a bidirectional stream for reliable features, datagrams
//! for unreliable ones). Every payload is prefixed with a single channel
//! tag so the receiver can route it without re-parsing the body.
//!
//! [`ChannelTagRegistry`](crate::ChannelTagRegistry) owns the *dynamic*
//! range (`0x10..=0xEF`) used by negotiated and bespoke features. This
//! module owns the *fixed* reserved tags (`0x00..=0x0F`) for first-party
//! core channels, so both ends — native peer, relay client, supernode,
//! and the web SDK — agree on them statically with no negotiation
//! round-trip:
//!
//! | tag    | channel                | feature id        |
//! |--------|------------------------|-------------------|
//! | `0x00` | control / signaling    | — (handshake etc) |
//! | `0x01` | direct audio           | `core.audio.opus` |
//! | `0x02` | text chat              | `core.chat.v1`    |
//! | `0x03` | file transfer          | `core.file.v1`    |
//! | `0x04` | room (SFU) audio       | `room.audio.sfu`  |
//!
//! The room-audio tag (`0x04`) only appears on the *relayed* datagram path
//! (a peer's QUIC relay session to a supernode that fans the frame out to
//! room members). It never rides the direct peer fabric, so [`classify`]
//! leaves it in [`FrameClass::Other`]; the relay client decodes it manually.
//! It exists as a fixed tag purely so relay quota accounting attributes the
//! frame to `room.audio.sfu` rather than the direct `core.audio.opus`.
//! Frame layout on a reliable stream is `[tag:u8][payload…]` *inside* the
//! transport's existing length-prefixed envelope. On a datagram it is the
//! same `[tag:u8][payload…]` with the datagram boundary as the frame
//! boundary.
//!
//! Control frames (tag `0x00`) carry signed `SignalingMessage` JSON. Every
//! peer-stream frame is tagged; untagged leading-`{` payloads are rejected.

/// Control / signaling channel (handshake, capability announce, presence).
pub const CONTROL_TAG: u8 = 0x00;
/// Direct peer audio (`core.audio.opus`) — unreliable datagrams.
pub const AUDIO_TAG: u8 = 0x01;
/// Text chat (`core.chat.v1`) — reliable stream frames.
pub const CHAT_TAG: u8 = 0x02;
/// File transfer (`core.file.v1`) — reliable stream frames.
pub const FILE_TAG: u8 = 0x03;
/// Room (SFU) audio (`room.audio.sfu`) — unreliable datagrams on a relay
/// session. Distinct from [`AUDIO_TAG`] so the relay attributes the frame to
/// the room feature's quota bucket rather than the direct-call one.
pub const ROOM_AUDIO_TAG: u8 = 0x04;

/// Highest fixed first-party tag. Tags above this up to
/// [`DYNAMIC_TAG_START`](crate::channel_tag::DYNAMIC_TAG_START) stay
/// reserved for future first-party channels.
pub const MAX_FIRST_PARTY_TAG: u8 = 0x0F;

/// Sentinel u32 (big-endian) a client writes as the **first frame** of a
/// QUIC relay *bidirectional* stream to mark it as the reliable signaling
/// channel (`room.chat.v1` / `room.file.v1` broadcasts), as opposed to a
/// `web.host.app.v1` request whose first u32 is a length-prefix.
///
/// The value is far above [`web_app::WEB_APP_MAX_FRAME_BYTES`](crate::web_app::WEB_APP_MAX_FRAME_BYTES)
/// (16 KiB), so a valid web-app request length can never collide with it and
/// the supernode can disambiguate the two stream kinds by reading 4 bytes.
pub const RELAY_SIGNAL_STREAM_MAGIC: u32 = 0x5147_4E4C; // "QGNL"

/// The fixed channel tag for a first-party feature id, if one exists.
///
/// Only `core.*` features that ride a dedicated channel have a fixed tag;
/// everything else is allocated dynamically via
/// [`ChannelTagRegistry`](crate::ChannelTagRegistry).
pub fn fixed_tag_for(feature_id: &str) -> Option<u8> {
    match feature_id {
        "core.audio.opus" => Some(AUDIO_TAG),
        "core.chat.v1" => Some(CHAT_TAG),
        "core.file.v1" => Some(FILE_TAG),
        "room.audio.sfu" => Some(ROOM_AUDIO_TAG),
        _ => None,
    }
}

/// The first-party feature id bound to a fixed channel tag, if any.
pub fn feature_for_fixed_tag(tag: u8) -> Option<&'static str> {
    match tag {
        AUDIO_TAG => Some("core.audio.opus"),
        CHAT_TAG => Some("core.chat.v1"),
        FILE_TAG => Some("core.file.v1"),
        ROOM_AUDIO_TAG => Some("room.audio.sfu"),
        _ => None,
    }
}

/// Encode a tagged frame: `[tag][payload…]`.
pub fn encode_frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + payload.len());
    out.push(tag);
    out.extend_from_slice(payload);
    out
}

/// Split a tagged frame into `(tag, payload)`. Returns `None` for an
/// empty frame.
pub fn decode_frame(frame: &[u8]) -> Option<(u8, &[u8])> {
    frame.split_first().map(|(&tag, rest)| (tag, rest))
}

/// How an inbound frame on the QUIC peer fabric should be interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameClass<'a> {
    /// A tagged control frame (`0x00`); payload is the signed JSON body.
    Control(&'a [u8]),
    /// Audio datagram (`0x01`); payload is the audio frame body.
    Audio(&'a [u8]),
    /// Chat frame (`0x02`); payload is the signed `core.chat.v1` JSON.
    Chat(&'a [u8]),
    /// File frame (`0x03`); payload is the signed `core.file.v1` JSON.
    File(&'a [u8]),
    /// Any other reserved/dynamic tag; carries the raw `(tag, payload)`.
    Other(u8, &'a [u8]),
}

/// Classify an inbound tagged frame. The leading byte is always the channel
/// tag; untagged JSON is not accepted.
pub fn classify(frame: &[u8]) -> Option<FrameClass<'_>> {
    let (&first, rest) = frame.split_first()?;
    Some(match first {
        CONTROL_TAG => FrameClass::Control(rest),
        AUDIO_TAG => FrameClass::Audio(rest),
        CHAT_TAG => FrameClass::Chat(rest),
        FILE_TAG => FrameClass::File(rest),
        other => FrameClass::Other(other, rest),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_tag_mapping_round_trips() {
        for (fid, tag) in [
            ("core.audio.opus", AUDIO_TAG),
            ("core.chat.v1", CHAT_TAG),
            ("core.file.v1", FILE_TAG),
            ("room.audio.sfu", ROOM_AUDIO_TAG),
        ] {
            assert_eq!(fixed_tag_for(fid), Some(tag));
            assert_eq!(feature_for_fixed_tag(tag), Some(fid));
        }
        assert_eq!(fixed_tag_for("x.acme.thing"), None);
        assert_eq!(feature_for_fixed_tag(0x42), None);
    }

    #[test]
    fn fixed_tags_are_in_reserved_range() {
        for tag in [CONTROL_TAG, AUDIO_TAG, CHAT_TAG, FILE_TAG, ROOM_AUDIO_TAG] {
            assert!(tag <= MAX_FIRST_PARTY_TAG);
            assert!(tag < crate::channel_tag::DYNAMIC_TAG_START);
        }
    }

    #[test]
    fn encode_decode_round_trips() {
        let f = encode_frame(CHAT_TAG, b"hello");
        assert_eq!(f, vec![CHAT_TAG, b'h', b'e', b'l', b'l', b'o']);
        assert_eq!(decode_frame(&f), Some((CHAT_TAG, &b"hello"[..])));
    }

    #[test]
    fn encode_empty_payload_is_just_tag() {
        let f = encode_frame(FILE_TAG, b"");
        assert_eq!(f, vec![FILE_TAG]);
        assert_eq!(decode_frame(&f), Some((FILE_TAG, &b""[..])));
    }

    #[test]
    fn decode_empty_frame_is_none() {
        assert_eq!(decode_frame(&[]), None);
        assert_eq!(classify(&[]), None);
    }

    #[test]
    fn classify_untagged_json_is_other_not_control() {
        // Leading `{` is not a reserved tag — pre-release peers must tag control.
        let json = br#"{"type":"chat_message"}"#;
        assert_eq!(classify(json), Some(FrameClass::Other(b'{', &json[1..])));
    }

    #[test]
    fn classify_tagged_channels() {
        assert_eq!(
            classify(&encode_frame(CONTROL_TAG, b"x")),
            Some(FrameClass::Control(b"x"))
        );
        assert_eq!(
            classify(&encode_frame(AUDIO_TAG, b"a")),
            Some(FrameClass::Audio(b"a"))
        );
        assert_eq!(
            classify(&encode_frame(CHAT_TAG, b"c")),
            Some(FrameClass::Chat(b"c"))
        );
        assert_eq!(
            classify(&encode_frame(FILE_TAG, b"f")),
            Some(FrameClass::File(b"f"))
        );
    }

    #[test]
    fn classify_dynamic_tag_is_other() {
        let frame = encode_frame(0x10, b"payload");
        assert_eq!(classify(&frame), Some(FrameClass::Other(0x10, b"payload")));
    }

    #[test]
    fn control_tag_with_json_payload_decodes_as_control_not_untagged() {
        // A tagged control frame: [0x00]{...}. The leading byte is the tag,
        // not '{', so it classifies as Control with the JSON as payload.
        let frame = encode_frame(CONTROL_TAG, br#"{"type":"hello"}"#);
        match classify(&frame) {
            Some(FrameClass::Control(body)) => {
                assert_eq!(body, br#"{"type":"hello"}"#);
            }
            other => panic!("expected Control, got {other:?}"),
        }
    }
}
