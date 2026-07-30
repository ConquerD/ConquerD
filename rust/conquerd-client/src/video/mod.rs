//! Video capture, encoding, transport framing, and decode.
//!
//! The pipeline mirrors the audio one in [`crate::call_controller`], with one
//! structural difference: an encoded video frame does not fit a single QUIC
//! datagram, so [`fragment`] sits between the codec and the transport on both
//! ends.
//!
//! ```text
//! camera -> encode -> seal (whole frame) -> fragment -> datagrams
//! datagrams -> reassemble -> verify signature -> open seal -> decode -> QVideoSink
//! ```
//!
//! "Camera" there may be several devices at once: a fragment names its stream by
//! sender alone, so a peer gets exactly one video stream, and picture-in-picture
//! is therefore done by merging sources into one frame before the encoder. See
//! [`composite`].
//!
//! Sealing happens once per frame rather than per fragment: a partial frame
//! cannot be decoded, so per-fragment sealing would cost 28 bytes and a GCM
//! operation each for no gain. Authenticity likewise rides one Ed25519
//! signature per frame, carried on fragment 0 — see [`fragment`] for why the
//! signed-JSON envelope used by room audio is not viable at video frame rates.

pub mod camera;
pub mod codec;
pub mod composite;
pub mod fragment;
pub mod frame;
#[cfg(target_os = "windows")]
pub mod mediafoundation;
#[cfg(target_os = "windows")]
pub mod mf_async;
pub mod nv12;
pub mod receiver;
pub mod scale;
#[cfg(target_os = "windows")]
pub mod screen;
pub mod sender;
pub mod sink;

use sha2::{Digest, Sha256};

/// Fallback datagram budget when quinn has not yet surfaced a negotiated size.
///
/// Matches the supernode's `MAX_DATAGRAM_SIZE`, so a fragment sized against
/// this assumption is still forwardable once the real value is known.
pub const DEFAULT_MAX_DATAGRAM: usize = 1200;

/// Domain tag mixed into the per-frame signature so a video frame signature can
/// never be replayed as a signature over anything else this identity signs.
const VIDEO_SIG_DOMAIN: &[u8] = b"conquerd.video.frame.v1";

/// Bytes an Ed25519 frame signature is computed over.
///
/// The frame body is hashed rather than signed directly so signing cost is
/// independent of frame size, and so the signed bytes fit comfortably in the
/// 64-byte fragment-0 budget regardless of how large the frame is.
///
/// Binding `conv_id` and `sender` is what makes this useful at all: the room
/// group key is *shared*, so GCM alone cannot distinguish members. Without the
/// sender bound into a signature only that member can produce, any room member
/// could seal a frame claiming to be someone else. `sequence` is bound so a
/// captured frame cannot be replayed at a later position in the stream.
///
/// `codec` is bound because it travels in the cleartext fragment header, where
/// a relay could otherwise flip it. On its own that is only a nuisance — the
/// receiver would hand the bytes to the wrong decoder, which errors and asks
/// for a keyframe — but an advisory codec is not worth having when binding it
/// costs one byte in the hash.
pub fn video_frame_signing_bytes(
    conv_id: &str,
    sender: &str,
    sequence: u32,
    codec: conquerd_features::video_codec::VideoCodec,
    sealed: &[u8],
) -> Vec<u8> {
    let digest = Sha256::digest(sealed);
    let mut out = Vec::with_capacity(
        VIDEO_SIG_DOMAIN.len() + 4 + conv_id.len() + 4 + sender.len() + 4 + 1 + digest.len(),
    );
    out.extend_from_slice(VIDEO_SIG_DOMAIN);
    // Length-prefixed like `group_key::voice_aad`, so the variable-length
    // fields can't be re-partitioned between conv_id and sender.
    out.extend_from_slice(&(conv_id.len() as u32).to_be_bytes());
    out.extend_from_slice(conv_id.as_bytes());
    out.extend_from_slice(&(sender.len() as u32).to_be_bytes());
    out.extend_from_slice(sender.as_bytes());
    out.extend_from_slice(&sequence.to_be_bytes());
    out.push(codec.as_wire());
    out.extend_from_slice(&digest);
    out
}

/// Conversation id for a direct 1:1 call: the two peer ids sorted and joined.
///
/// Sorting is what makes both ends derive the same string without exchanging
/// anything. Matches the convention documented on
/// [`GroupKeySource`](crate::group_key::GroupKeySource), which uses the room id
/// for rooms and this pair id for direct calls.
pub fn direct_conv_id(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}|{b}")
    } else {
        format!("{b}|{a}")
    }
}

/// Where captured video should be sent.
///
/// `None` means the room fan-out; `Some(peer)` means that peer's direct
/// session. Matches the shape of
/// [`SendVideoState::direct_peer`](crate::connection_manager::events::ConnectionCommand::SendVideoState)
/// so the answer can be handed straight to the connection manager.
///
/// A joined room always wins. A direct call that fell back to a temporary room
/// still has its original `direct_call_peer_id` set, but by then the call runs
/// through the room and the peer's direct QUIC session no longer carries it —
/// so preferring the peer there would send video down a path nobody is reading.
pub fn video_route(voice_room_id: &str, direct_call_peer_id: &str) -> Option<String> {
    if !voice_room_id.trim().is_empty() {
        return None;
    }
    let peer = direct_call_peer_id.trim();
    (!peer.is_empty()).then(|| peer.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    use conquerd_features::video_codec::VideoCodec;

    const CODEC: VideoCodec = VideoCodec::Stub;

    #[test]
    fn a_joined_room_routes_video_to_the_room() {
        assert_eq!(video_route("room-1", ""), None);
    }

    #[test]
    fn a_direct_call_routes_video_to_that_peer() {
        assert_eq!(video_route("", "peer-abc"), Some("peer-abc".to_owned()));
    }

    /// The precedence that matters: a direct call that fell back to a temporary
    /// room leaves *both* ids set. Sending to the peer then would push frames
    /// down a direct session the call no longer uses, and the room would see
    /// nothing — video silently dead while audio worked.
    #[test]
    fn a_room_wins_when_a_stale_direct_peer_is_also_set() {
        assert_eq!(video_route("fallback-room", "peer-abc"), None);
    }

    #[test]
    fn no_session_routes_nowhere_in_particular() {
        // Falls through to the room path, which no-ops when not in a room.
        assert_eq!(video_route("", ""), None);
        assert_eq!(video_route("   ", "   "), None);
    }

    #[test]
    fn direct_conv_id_is_order_independent() {
        assert_eq!(
            direct_conv_id("alice", "bob"),
            direct_conv_id("bob", "alice")
        );
        assert_ne!(
            direct_conv_id("alice", "bob"),
            direct_conv_id("alice", "carol")
        );
    }

    #[test]
    fn signing_bytes_bind_every_field() {
        let base = video_frame_signing_bytes("room", "alice", 1, CODEC, b"sealed");
        for other in [
            video_frame_signing_bytes("ROOM", "alice", 1, CODEC, b"sealed"),
            video_frame_signing_bytes("room", "mallory", 1, CODEC, b"sealed"),
            video_frame_signing_bytes("room", "alice", 2, CODEC, b"sealed"),
            video_frame_signing_bytes("room", "alice", 1, CODEC, b"tampered"),
        ] {
            assert_ne!(base, other);
        }
    }

    /// The codec travels in cleartext in the fragment header, so it must be
    /// bound into the signature — otherwise a relay could redirect a frame to
    /// the wrong decoder without invalidating anything.
    #[test]
    fn signing_bytes_bind_the_codec() {
        let h264 = video_frame_signing_bytes("room", "alice", 1, VideoCodec::H264, b"sealed");
        let vp8 = video_frame_signing_bytes("room", "alice", 1, VideoCodec::Vp8, b"sealed");
        assert_ne!(h264, vp8);
    }

    /// End to end: flipping the codec byte on the wire must make the frame fail
    /// verification, not merely decode oddly.
    #[test]
    fn tampering_with_the_codec_byte_fails_verification() {
        const ROOM: &str = "room-codec-tamper";
        let identity = Identity::generate();
        let sender = identity.public_id();
        let seq = 5u32;
        let sealed = b"sealed-frame-bytes".to_vec();

        let sig_bytes = video_frame_signing_bytes(ROOM, &sender, seq, VideoCodec::H264, &sealed);
        let signature = <[u8; SIGNATURE_LEN]>::try_from(&identity.sign(&sig_bytes)[..]).unwrap();
        let mut parts = fragment::fragment_frame(
            &sender,
            seq,
            true,
            VideoCodec::H264,
            &signature,
            &sealed,
            1198,
        )
        .unwrap();
        assert_eq!(parts.len(), 1, "small frame should be a single fragment");

        // A relay flips H.264 to VP8 in the cleartext header.
        parts[0][2] = VideoCodec::Vp8.as_wire();

        let mut rx = Reassembler::new();
        let got = rx
            .push(&parts[0], std::time::Instant::now())
            .expect("the tampered fragment still parses");
        assert_eq!(got.codec, VideoCodec::Vp8, "the lie is carried up as-is");

        // ...and dies here, because the receiver verifies against the codec
        // the frame claims.
        let public_key =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE, &got.sender)
                .unwrap();
        let check =
            video_frame_signing_bytes(ROOM, &got.sender, got.frame_seq, got.codec, &got.sealed);
        assert!(
            !Identity::verify_with_public_key(&public_key, &got.signature, &check),
            "a flipped codec byte must fail signature verification"
        );
    }

    #[test]
    fn signing_bytes_are_length_prefixed_not_concatenated() {
        // "ab" + "c" must not collide with "a" + "bc".
        assert_ne!(
            video_frame_signing_bytes("ab", "c", 1, CODEC, b"x"),
            video_frame_signing_bytes("a", "bc", 1, CODEC, b"x")
        );
    }

    #[test]
    fn signing_bytes_are_fixed_size_regardless_of_frame_size() {
        let small = video_frame_signing_bytes("room", "alice", 1, CODEC, b"x");
        let large = video_frame_signing_bytes("room", "alice", 1, CODEC, &vec![0u8; 100_000]);
        assert_eq!(small.len(), large.len());
    }

    // ── End-to-end: the whole room-video pipeline, minus the network ─────────

    use crate::group_key::{open_media_frame, seal_media_frame, MediaKind, SenderKeysGroup};
    use crate::identity::Identity;
    use codec::{StubDecoder, StubEncoder, VideoDecoder, VideoEncoder, STUB_HEIGHT, STUB_WIDTH};
    use fragment::{Reassembler, SIGNATURE_LEN};
    use frame::RawFrame;

    /// encode -> seal -> sign -> fragment -> (wire) -> reassemble -> verify
    /// -> open -> decode, and the pixels must come back identical.
    #[test]
    fn room_video_round_trips_end_to_end() {
        const ROOM: &str = "room-e2e";
        let identity = Identity::generate();
        let sender = identity.public_id();
        let mut keys = SenderKeysGroup::new();
        keys.new_owner_epoch(ROOM);

        let original = RawFrame::test_pattern(STUB_WIDTH, STUB_HEIGHT, 11);
        let (encoded, keyframe) = StubEncoder.encode(&original).unwrap();
        let seq = 42u32;

        let sealed = seal_media_frame(
            &keys,
            MediaKind::Video,
            ROOM,
            &sender,
            u64::from(seq),
            &encoded,
        )
        .unwrap();
        let sig_bytes = video_frame_signing_bytes(ROOM, &sender, seq, CODEC, &sealed);
        let signature = <[u8; SIGNATURE_LEN]>::try_from(&identity.sign(&sig_bytes)[..]).unwrap();

        let fragments =
            fragment::fragment_frame(&sender, seq, keyframe, CODEC, &signature, &sealed, 1198)
                .unwrap();
        assert!(
            fragments.len() > 20,
            "the stub frame should genuinely exercise fragmentation"
        );

        let mut rx = Reassembler::new();
        let now = std::time::Instant::now();
        let mut done = None;
        for f in &fragments {
            if let Some(frame) = rx.push(f, now) {
                done = Some(frame);
            }
        }
        let got = done.expect("all fragments delivered");

        assert_eq!(got.sender, sender);
        assert_eq!(got.frame_seq, seq);
        assert_eq!(got.sealed, sealed, "reassembly must be byte-identical");

        // Verify before decrypting — the shared group key cannot distinguish
        // room members, so this signature is what binds the sender.
        let public_key =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE, &got.sender)
                .unwrap();
        let check =
            video_frame_signing_bytes(ROOM, &got.sender, got.frame_seq, got.codec, &got.sealed);
        assert!(Identity::verify_with_public_key(
            &public_key,
            &got.signature,
            &check
        ));

        let opened = open_media_frame(
            &keys,
            MediaKind::Video,
            ROOM,
            &got.sender,
            u64::from(got.frame_seq),
            &got.sealed,
        )
        .unwrap();
        assert_eq!(opened, encoded);
        assert_eq!(StubDecoder.decode(&opened).unwrap(), original);
    }

    /// The impersonation the per-frame signature exists to stop: a room member
    /// holding the same group key seals a frame naming someone else. GCM alone
    /// accepts it; the signature check must not.
    #[test]
    fn another_member_cannot_impersonate_a_sender() {
        const ROOM: &str = "room-impersonation";
        let victim = Identity::generate();
        let attacker = Identity::generate();
        let mut keys = SenderKeysGroup::new();
        keys.new_owner_epoch(ROOM);

        let victim_id = victim.public_id();
        let seq = 1u32;
        // The attacker seals under the shared key, claiming to be the victim.
        let forged = seal_media_frame(
            &keys,
            MediaKind::Video,
            ROOM,
            &victim_id,
            u64::from(seq),
            b"forged",
        )
        .unwrap();

        // The seal itself opens fine — this is exactly the gap being closed.
        assert!(open_media_frame(
            &keys,
            MediaKind::Video,
            ROOM,
            &victim_id,
            u64::from(seq),
            &forged
        )
        .is_some());

        // But the attacker can only sign with their own key.
        let sig_bytes = video_frame_signing_bytes(ROOM, &victim_id, seq, CODEC, &forged);
        let signature = attacker.sign(&sig_bytes);
        let victim_key =
            base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE, &victim_id).unwrap();
        assert!(
            !Identity::verify_with_public_key(&victim_key, &signature, &sig_bytes),
            "a frame signed by another member must not verify as the victim"
        );
    }
}
