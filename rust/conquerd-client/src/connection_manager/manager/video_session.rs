//! Inbound video: reassemble fragments, authenticate, unseal, emit.
//!
//! Both transports converge here. A room fragment arrives via the supernode
//! relay and a direct fragment over a 1:1 QUIC session, but the fragment format
//! is identical and so is everything after reassembly, so one path serves both
//! and there is no "which transport was this?" branch to get wrong.
//!
//! Order of operations on a completed frame is deliberate:
//!
//! 1. **Reassemble.** Bounded by [`crate::video::fragment`]'s own caps.
//! 2. **Verify the per-frame signature.** Before any decryption, because the
//!    room group key is *shared*: without this step any room member could seal
//!    a frame claiming to be another member and GCM would happily verify it.
//! 3. **Open the seal** (room only — direct video rides mTLS, like direct
//!    audio, and is not sealed).
//! 4. **Emit** the codec bytes for decoding.

use base64::Engine;
use tracing::{debug, warn};

use super::ConnectionManager;
use crate::connection_manager::events::ConnectionEvent;
use crate::identity::Identity;
use crate::video::fragment::SIGNATURE_LEN;

/// Recover the 32-byte Ed25519 public key a base64url `public_id` encodes.
fn public_key_from_public_id(public_id: &str) -> Option<Vec<u8>> {
    // Senders use `Identity::public_id`, which is URL_SAFE *with* padding.
    // Accept the un-padded form too: the relay layer strips padding from ids
    // in some paths, and a signature check is the wrong place to be strict
    // about an encoding detail that carries no security meaning.
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(public_id)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(public_id))
        .ok()?;
    (bytes.len() == 32).then_some(bytes)
}

impl ConnectionManager {
    /// Feed one inbound video fragment into reassembly.
    ///
    /// `session_peer_id` is the transport-verified identity of whoever sent the
    /// datagram. `is_room` selects whether the completed frame is expected to
    /// be E2E-sealed.
    pub(super) async fn accept_video_fragment(
        &mut self,
        session_peer_id: &str,
        fragment: &[u8],
        is_room: bool,
    ) {
        let now = std::time::Instant::now();
        let Some(frame) = self.video_reassembler.push(fragment, now) else {
            return; // Incomplete, duplicate, or refused by a cap.
        };

        let Some(public_key) = public_key_from_public_id(&frame.sender) else {
            debug!("[video] frame from unparseable sender id; dropping");
            return;
        };

        // Which conversation the sender bound into the signature. Room frames
        // bind the room id; direct frames bind the sorted peer pair.
        let conv_id = if is_room {
            self.current_room_id.clone()
        } else {
            crate::video::direct_conv_id(&self.identity.public_id(), &frame.sender)
        };
        if conv_id.is_empty() {
            return;
        }

        let signing_bytes = crate::video::video_frame_signing_bytes(
            &conv_id,
            &frame.sender,
            frame.frame_seq,
            frame.codec,
            frame.pts_us,
            &frame.sealed,
        );
        if !Identity::verify_with_public_key(&public_key, &frame.signature, &signing_bytes) {
            // Not merely a corrupt frame: a valid-looking frame that fails here
            // is an impersonation attempt, since only the named sender's key
            // can produce this signature.
            warn!(
                "[video] frame signature rejected for sender {}",
                &frame.sender[..8.min(frame.sender.len())]
            );
            return;
        }

        let encoded = if is_room {
            let Some(plain) = crate::group_key::open_media_frame(
                &self.group_keys,
                crate::group_key::MediaKind::Video,
                &conv_id,
                &frame.sender,
                u64::from(frame.frame_seq),
                &frame.sealed,
            ) else {
                debug!("[room.video.sfu] could not open sealed frame; dropping");
                return;
            };
            plain
        } else {
            // Direct video is not app-layer encrypted — same posture as direct
            // audio, where the QUIC mTLS session provides confidentiality and
            // no untrusted relay sees the bytes.
            frame.sealed
        };

        // Trust the transport-verified identity over the self-declared one for
        // the direct path; on the relay path the supernode does not
        // authenticate senders, so the signature above is what binds identity.
        let peer_id = if is_room {
            frame.sender
        } else {
            session_peer_id.to_owned()
        };

        self.emit_event(ConnectionEvent::VideoFrameReceived {
            peer_id,
            encoded,
            keyframe: frame.keyframe,
            codec: frame.codec,
            pts_us: frame.pts_us,
        });
    }
}

impl ConnectionManager {
    /// Accept one inbound content-audio frame (room or direct).
    ///
    /// Same order of operations as video, and for the same reason on the room
    /// path: the room group key is *shared*, so the per-frame signature is what
    /// binds identity and must be checked **before** anything is decrypted.
    ///
    /// 1. Parse (bounded, hostile-input-safe — see [`crate::content_audio`]).
    /// 2. Verify the signature, which covers the timestamp.
    /// 3. Open the seal (room only — direct rides mTLS, raw Opus on the wire).
    /// 4. Emit for decode and playout.
    ///
    /// `session_peer_id` is the transport-verified identity on the direct path
    /// (or the relaying supernode id on the room path, unused after parse).
    /// `is_room` selects seal expectations and which conversation id the
    /// signature binds.
    pub(super) async fn accept_content_audio_frame(
        &mut self,
        session_peer_id: &str,
        buf: &[u8],
        is_room: bool,
    ) {
        let label = if is_room {
            "room.audio.content.sfu"
        } else {
            "core.audio.content.v1"
        };

        let Some(frame) = crate::content_audio::parse_frame(buf) else {
            debug!("[{label}] malformed frame; dropping");
            return;
        };

        let Some(public_key) = public_key_from_public_id(&frame.sender) else {
            debug!("[{label}] unparseable sender id; dropping");
            return;
        };

        let conv_id = if is_room {
            self.current_room_id.clone()
        } else {
            crate::video::direct_conv_id(&self.identity.public_id(), &frame.sender)
        };
        if conv_id.is_empty() {
            return;
        }

        let signing_bytes = crate::content_audio::content_audio_signing_bytes(
            &conv_id,
            &frame.sender,
            frame.seq,
            frame.pts_us,
            &frame.payload,
        );
        if !Identity::verify_with_public_key(&public_key, &frame.signature, &signing_bytes) {
            // As with video, a well-formed frame failing here is an
            // impersonation attempt rather than corruption: only the named
            // sender's key can produce this signature.
            warn!(
                "[{label}] signature rejected for sender {}",
                &frame.sender[..8.min(frame.sender.len())]
            );
            return;
        }

        let opus = if is_room {
            let Some(plain) = crate::group_key::open_media_frame(
                &self.group_keys,
                crate::group_key::MediaKind::ContentAudio,
                &conv_id,
                &frame.sender,
                u64::from(frame.seq),
                &frame.payload,
            ) else {
                debug!("[{label}] could not open sealed frame; dropping");
                return;
            };
            plain
        } else {
            // Direct content audio is not app-layer encrypted — same posture as
            // direct video/voice: the QUIC mTLS session provides confidentiality.
            frame.payload
        };

        // Trust the transport-verified identity over the self-declared one for
        // the direct path; on the relay path the signature above binds identity.
        let peer_id = if is_room {
            frame.sender
        } else {
            session_peer_id.to_owned()
        };

        self.emit_event(ConnectionEvent::ContentAudioReceived {
            peer_id,
            opus,
            pts_us: frame.pts_us,
            seq: frame.seq,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_round_trips_through_public_id() {
        let key = [9u8; 32];
        let id = crate::crypto::derive_public_id(&key);
        assert_eq!(public_key_from_public_id(&id).as_deref(), Some(&key[..]));
    }

    #[test]
    fn unpadded_public_id_is_accepted() {
        let key = [3u8; 32];
        let padded = crate::crypto::derive_public_id(&key);
        let unpadded = padded.trim_end_matches('=');
        assert_eq!(
            public_key_from_public_id(unpadded).as_deref(),
            Some(&key[..])
        );
    }

    #[test]
    fn rejects_ids_that_are_not_32_byte_keys() {
        assert!(public_key_from_public_id("").is_none());
        assert!(public_key_from_public_id("not base64!!").is_none());
        // Valid base64 but the wrong length for an Ed25519 key.
        let short = base64::engine::general_purpose::URL_SAFE.encode([1u8; 16]);
        assert!(public_key_from_public_id(&short).is_none());
    }

    #[test]
    fn signature_len_matches_ed25519() {
        assert_eq!(SIGNATURE_LEN, 64);
    }
}
