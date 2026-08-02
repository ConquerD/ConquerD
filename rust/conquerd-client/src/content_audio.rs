//! Content-audio wire format: timestamped Opus that video is synchronised to.
//!
//! This is the audio that belongs *with* the video — a game, a browser tab, a
//! track — as opposed to the call microphone, which keeps its own untouched
//! path. See the media-layer item in `backlog.md` for why the two are separate
//! streams rather than one mixed one.
//!
//! # Wire format
//!
//! This module owns everything after the channel tag; the transport prepends
//! [`CONTENT_AUDIO_TAG`](conquerd_features::channel_frame::CONTENT_AUDIO_TAG)
//! or [`ROOM_CONTENT_AUDIO_TAG`](conquerd_features::channel_frame::ROOM_CONTENT_AUDIO_TAG).
//!
//! ```text
//! [ver:u8]                0x01
//! [flags:u8]              reserved, must be 0
//! [pts:u64 BE]            microseconds since the sender's session start
//! [seq:u32 BE]            per-stream frame counter
//! [sender_len:u8][sender] base64url peer id
//! [sig:64]                Ed25519 over the frame
//! [payload]               room: [epoch][nonce][AES-GCM(opus)]; direct: raw opus
//! ```
//!
//! # Why not fragmented like video
//!
//! One Opus frame is at most a couple of hundred bytes, so it always fits a
//! single datagram. Fragmentation exists for video because an encoded picture
//! does not; borrowing that machinery here would add reassembly state and a
//! failure mode for no benefit.
//!
//! # Why signed per frame
//!
//! Same reason room video is: the room group key is *shared*, so GCM alone
//! cannot tell one member from another, and without a per-frame signature any
//! member could seal a frame claiming to be someone else. At 50 frames/second
//! this is the same signing rate room voice already sustains.
//!
//! The timestamp is inside the signed bytes, so a relay cannot shift a stream
//! in time — that would desynchronise a peer's audio and video without
//! corrupting either, which no integrity check that ignored timing would catch.

use sha2::{Digest, Sha256};

/// Wire format version.
pub const CONTENT_AUDIO_VERSION: u8 = 0x01;

/// Length of the Ed25519 signature carried on every frame.
pub const SIGNATURE_LEN: usize = 64;

/// Bytes before the variable-length sender id: ver + flags + pts(8) + seq(4).
const FIXED_PREFIX_LEN: usize = 15;

/// Upper bound on a parsed sender id, so a hostile length byte cannot make us
/// index far past the header.
const MAX_SENDER_LEN: usize = 64;

/// Domain tag for the per-frame signature, so a content-audio signature can
/// never be replayed as a signature over anything else this identity signs —
/// including a *video* frame, which is the neighbouring format most likely to
/// be confused with it.
const CONTENT_AUDIO_SIG_DOMAIN: &[u8] = b"conquerd.content.audio.frame.v1";

/// One parsed content-audio frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentAudioFrame {
    /// Sender's base64url peer id, as carried on the wire.
    pub sender: String,
    /// Capture time, microseconds since the sender's session start.
    pub pts_us: u64,
    /// Per-stream frame counter, for replay detection and loss estimation.
    pub seq: u32,
    /// Ed25519 signature over the frame. Verify **before** opening `payload`.
    pub signature: [u8; SIGNATURE_LEN],
    /// Sealed (room) or raw (direct) Opus bytes.
    pub payload: Vec<u8>,
}

/// Bytes the per-frame Ed25519 signature is computed over.
///
/// The payload is hashed rather than signed directly so signing cost does not
/// grow with frame size, mirroring the video path.
pub fn content_audio_signing_bytes(
    conv_id: &str,
    sender: &str,
    seq: u32,
    pts_us: u64,
    payload: &[u8],
) -> Vec<u8> {
    let digest = Sha256::digest(payload);
    let mut out = Vec::with_capacity(
        CONTENT_AUDIO_SIG_DOMAIN.len()
            + 4
            + conv_id.len()
            + 4
            + sender.len()
            + 4
            + 8
            + digest.len(),
    );
    out.extend_from_slice(CONTENT_AUDIO_SIG_DOMAIN);
    // Length-prefixed so the variable-length fields cannot be re-partitioned
    // between conv_id and sender.
    out.extend_from_slice(&(conv_id.len() as u32).to_be_bytes());
    out.extend_from_slice(conv_id.as_bytes());
    out.extend_from_slice(&(sender.len() as u32).to_be_bytes());
    out.extend_from_slice(sender.as_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&pts_us.to_be_bytes());
    out.extend_from_slice(&digest);
    out
}

/// Serialise one frame. Returns `None` for an unusable sender id — a caller
/// error rather than a runtime condition.
pub fn encode_frame(
    sender: &str,
    seq: u32,
    pts_us: u64,
    signature: &[u8; SIGNATURE_LEN],
    payload: &[u8],
) -> Option<Vec<u8>> {
    if sender.is_empty() || sender.len() > MAX_SENDER_LEN {
        return None;
    }
    let mut buf =
        Vec::with_capacity(FIXED_PREFIX_LEN + sender.len() + SIGNATURE_LEN + payload.len());
    buf.push(CONTENT_AUDIO_VERSION);
    buf.push(0); // flags, reserved
    buf.extend_from_slice(&pts_us.to_be_bytes());
    buf.extend_from_slice(&seq.to_be_bytes());
    buf.push(sender.len() as u8);
    buf.extend_from_slice(sender.as_bytes());
    buf.extend_from_slice(signature);
    buf.extend_from_slice(payload);
    Some(buf)
}

/// Parse one frame. `None` for anything malformed; every length is validated
/// before it is used to index or allocate, because this is the piece most
/// exposed to hostile input.
pub fn parse_frame(buf: &[u8]) -> Option<ContentAudioFrame> {
    if buf.len() < FIXED_PREFIX_LEN {
        return None;
    }
    if buf[0] != CONTENT_AUDIO_VERSION {
        return None;
    }
    // Reserved flags must be zero: accepting unknown bits now would make them
    // unusable later, since a peer could not tell an old sender from a new one.
    if buf[1] != 0 {
        return None;
    }
    let pts_us = u64::from_be_bytes(buf[2..10].try_into().ok()?);
    let seq = u32::from_be_bytes(buf[10..14].try_into().ok()?);
    let sender_len = buf[14] as usize;
    if sender_len == 0 || sender_len > MAX_SENDER_LEN {
        return None;
    }

    let sender_end = FIXED_PREFIX_LEN + sender_len;
    let sig_end = sender_end + SIGNATURE_LEN;
    if buf.len() < sig_end {
        return None;
    }
    let sender = std::str::from_utf8(&buf[FIXED_PREFIX_LEN..sender_end]).ok()?;

    let mut signature = [0u8; SIGNATURE_LEN];
    signature.copy_from_slice(&buf[sender_end..sig_end]);

    Some(ContentAudioFrame {
        sender: sender.to_owned(),
        pts_us,
        seq,
        signature,
        payload: buf[sig_end..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENDER: &str = "alice-public-id-base64url-xxxxxxxxxxxxxxxxxxx";
    const SIG: [u8; SIGNATURE_LEN] = [7u8; SIGNATURE_LEN];

    #[test]
    fn round_trips() {
        let payload = b"opus-frame-bytes";
        let buf = encode_frame(SENDER, 42, 1_234_567, &SIG, payload).unwrap();
        let f = parse_frame(&buf).unwrap();
        assert_eq!(f.sender, SENDER);
        assert_eq!(f.seq, 42);
        assert_eq!(f.pts_us, 1_234_567);
        assert_eq!(f.signature, SIG);
        assert_eq!(f.payload, payload);
    }

    /// GOLDEN: the header layout. A silent shift desynchronises every field
    /// after it, and the symptom is unplayable audio rather than a parse error.
    #[test]
    fn header_fields_sit_where_the_format_says() {
        let buf = encode_frame(SENDER, 9, 0x0123_4567_89AB_CDEF, &SIG, b"x").unwrap();
        assert_eq!(buf[0], CONTENT_AUDIO_VERSION);
        assert_eq!(buf[1], 0, "flags are reserved and must be zero");
        assert_eq!(
            u64::from_be_bytes(buf[2..10].try_into().unwrap()),
            0x0123_4567_89AB_CDEF
        );
        assert_eq!(u32::from_be_bytes(buf[10..14].try_into().unwrap()), 9);
        assert_eq!(buf[14], SENDER.len() as u8);
    }

    /// A full-range value catches a truncated or byte-swapped field that a
    /// small number would let pass by luck.
    #[test]
    fn a_full_range_pts_survives() {
        let pts = u64::MAX - 12345;
        let buf = encode_frame(SENDER, 1, pts, &SIG, b"x").unwrap();
        assert_eq!(parse_frame(&buf).unwrap().pts_us, pts);
    }

    #[test]
    fn an_empty_payload_is_valid() {
        // A frame with no Opus bytes is odd but well-formed; the receiver, not
        // the parser, decides what to do with it.
        let buf = encode_frame(SENDER, 1, 0, &SIG, b"").unwrap();
        let f = parse_frame(&buf).unwrap();
        assert!(f.payload.is_empty());
    }

    #[test]
    fn truncated_input_is_rejected() {
        let buf = encode_frame(SENDER, 1, 0, &SIG, b"payload").unwrap();
        for cut in 0..buf.len() - 1 {
            // Every prefix short of the full header+signature must be refused
            // rather than parsed into a frame with a truncated signature.
            if cut < FIXED_PREFIX_LEN + SENDER.len() + SIGNATURE_LEN {
                assert!(parse_frame(&buf[..cut]).is_none(), "accepted {cut} bytes");
            }
        }
    }

    #[test]
    fn a_wrong_version_is_rejected() {
        let mut buf = encode_frame(SENDER, 1, 0, &SIG, b"x").unwrap();
        buf[0] = CONTENT_AUDIO_VERSION + 1;
        assert!(parse_frame(&buf).is_none());
    }

    /// Reserved bits must stay reserved: accepting them now would make them
    /// unusable later, since a receiver could not distinguish an old sender
    /// from one using a new flag.
    #[test]
    fn unknown_flag_bits_are_rejected() {
        let mut buf = encode_frame(SENDER, 1, 0, &SIG, b"x").unwrap();
        buf[1] = 0b0000_0001;
        assert!(parse_frame(&buf).is_none());
    }

    #[test]
    fn a_hostile_sender_length_cannot_over_read() {
        let mut buf = encode_frame(SENDER, 1, 0, &SIG, b"x").unwrap();
        buf[14] = 255; // far beyond both MAX_SENDER_LEN and the buffer
        assert!(parse_frame(&buf).is_none());

        let mut zero = encode_frame(SENDER, 1, 0, &SIG, b"x").unwrap();
        zero[14] = 0;
        assert!(parse_frame(&zero).is_none());
    }

    #[test]
    fn encode_refuses_an_unusable_sender() {
        assert!(encode_frame("", 1, 0, &SIG, b"x").is_none());
        let too_long = "a".repeat(MAX_SENDER_LEN + 1);
        assert!(encode_frame(&too_long, 1, 0, &SIG, b"x").is_none());
    }

    #[test]
    fn signing_bytes_bind_every_field() {
        let base = content_audio_signing_bytes("room", "alice", 1, 100, b"opus");
        for other in [
            content_audio_signing_bytes("ROOM", "alice", 1, 100, b"opus"),
            content_audio_signing_bytes("room", "mallory", 1, 100, b"opus"),
            content_audio_signing_bytes("room", "alice", 2, 100, b"opus"),
            content_audio_signing_bytes("room", "alice", 1, 101, b"opus"),
            content_audio_signing_bytes("room", "alice", 1, 100, b"tampered"),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn signing_bytes_are_length_prefixed_not_concatenated() {
        assert_ne!(
            content_audio_signing_bytes("ab", "c", 1, 0, b"x"),
            content_audio_signing_bytes("a", "bc", 1, 0, b"x")
        );
    }

    /// The domain tag must keep a content-audio signature from ever being
    /// accepted as a video-frame signature, since both bind a conversation, a
    /// sender, a sequence and a timestamp over hashed bytes.
    #[test]
    fn domain_is_separate_from_video() {
        let audio = content_audio_signing_bytes("room", "alice", 1, 100, b"same");
        let video = crate::video::video_frame_signing_bytes(
            "room",
            "alice",
            1,
            conquerd_features::video_codec::VideoCodec::Stub,
            100,
            b"same",
        );
        assert_ne!(audio, video);
        assert!(audio.starts_with(CONTENT_AUDIO_SIG_DOMAIN));
    }

    /// The whole send-to-receive contract in one place: seal, sign, encode,
    /// then parse, verify, unseal — and get the original bytes back.
    ///
    /// Worth asserting end to end because each half lives in a different module
    /// and only their *composition* is the thing that has to hold.
    #[test]
    fn a_sealed_signed_frame_round_trips_through_the_wire_format() {
        use crate::group_key::{open_media_frame, seal_media_frame, MediaKind, SenderKeysGroup};
        use crate::identity::Identity;

        const ROOM: &str = "room-content-audio";
        let identity = Identity::generate();
        let sender = identity.public_id();
        let mut keys = SenderKeysGroup::new();
        keys.new_owner_epoch(ROOM);

        let opus = b"pretend-opus-frame".to_vec();
        let (seq, pts) = (7u32, 140_000u64);

        let sealed = seal_media_frame(
            &keys,
            MediaKind::ContentAudio,
            ROOM,
            &sender,
            u64::from(seq),
            &opus,
        )
        .unwrap();
        let sig_bytes = content_audio_signing_bytes(ROOM, &sender, seq, pts, &sealed);
        let signature = <[u8; SIGNATURE_LEN]>::try_from(&identity.sign(&sig_bytes)[..]).unwrap();
        let wire = encode_frame(&sender, seq, pts, &signature, &sealed).unwrap();

        // ---- receiver ----
        let got = parse_frame(&wire).expect("parses");
        assert_eq!(got.pts_us, pts);
        assert_eq!(got.seq, seq);

        let key = base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE, &got.sender)
            .unwrap();
        let check =
            content_audio_signing_bytes(ROOM, &got.sender, got.seq, got.pts_us, &got.payload);
        assert!(Identity::verify_with_public_key(
            &key,
            &got.signature,
            &check
        ));

        let opened = open_media_frame(
            &keys,
            MediaKind::ContentAudio,
            ROOM,
            &got.sender,
            u64::from(got.seq),
            &got.payload,
        )
        .expect("unseals");
        assert_eq!(opened, opus);
    }

    /// A content frame must not open as a voice frame at the same sequence.
    /// Both carry Opus under the same room key with independent counters, so
    /// without the AAD domain byte one could be replayed as the other.
    #[test]
    fn a_content_frame_does_not_open_as_voice() {
        use crate::group_key::{open_media_frame, seal_media_frame, MediaKind, SenderKeysGroup};

        const ROOM: &str = "room-domain-check";
        let mut keys = SenderKeysGroup::new();
        keys.new_owner_epoch(ROOM);

        let sealed =
            seal_media_frame(&keys, MediaKind::ContentAudio, ROOM, "alice", 3, b"payload").unwrap();

        assert!(
            open_media_frame(&keys, MediaKind::Voice, ROOM, "alice", 3, &sealed).is_none(),
            "content audio must not open under the voice domain"
        );
        assert!(
            open_media_frame(&keys, MediaKind::Video, ROOM, "alice", 3, &sealed).is_none(),
            "content audio must not open under the video domain"
        );
        assert!(
            open_media_frame(&keys, MediaKind::ContentAudio, ROOM, "alice", 3, &sealed).is_some()
        );
    }

    #[test]
    fn signing_bytes_are_fixed_size_regardless_of_payload() {
        let small = content_audio_signing_bytes("room", "alice", 1, 0, b"x");
        let large = content_audio_signing_bytes("room", "alice", 1, 0, &vec![0u8; 100_000]);
        assert_eq!(small.len(), large.len());
    }
}
