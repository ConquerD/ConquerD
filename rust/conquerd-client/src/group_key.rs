//! Group keying for end-to-end encrypted room traffic.
//!
//! This module defines the [`GroupKeySource`] trait — the seam that Part A's
//! sender-keys group keying (and the deferred TreeKEM upgrade) will implement —
//! plus the wire codec for **E2E-encrypted room audio** (backlog "Voice E2E").
//!
//! Each Opus frame is wrapped before it enters the datagram path as:
//! ```text
//! [epoch:u8][nonce:12][AES-256-GCM(opus)]
//! ```
//! authenticated with `AAD = conv_id ‖ sender ‖ sequence`, and carried inside
//! the existing `ROOM_AUDIO_TAG` (0x04) `SfuAudio` envelope (the sealed bytes
//! replace the plaintext Opus in the base64 `audio` field; `epoch` rides the
//! frame, `sequence` rides the signed envelope as `seq`). The relay stays a
//! dumb forwarder — it never sees the key or the plaintext.

use crate::crypto::{aesgcm_decrypt, aesgcm_encrypt, hkdf_derive_key};

/// Length of the per-frame AES-GCM nonce (bytes).
pub const VOICE_NONCE_LEN: usize = 12;
/// Fixed frame prefix: `epoch (1)` + `nonce (12)`.
const VOICE_HEADER_LEN: usize = 1 + VOICE_NONCE_LEN;

/// Source of per-conversation group keys for E2E room traffic.
///
/// The trait is deliberately tiny: a caller asks for the epoch to seal *new*
/// frames under, and can look up the key for any epoch it holds (needed to
/// *open* frames that were in flight across a rekey). Part A supplies the real
/// sender-keys implementation behind this trait; the deferred TreeKEM work
/// (backlog) swaps in later with no call-site changes.
///
/// `conv_id` is the conversation identifier — the SFU `room_id` for rooms, or
/// the sorted peer-pair id for direct calls.
pub trait GroupKeySource: Send + Sync {
    /// The epoch new frames for `conv_id` should be sealed under.
    fn current_epoch(&self, conv_id: &str) -> u8;

    /// The 32-byte symmetric key for `conv_id` at `epoch`, or `None` when this
    /// member holds no key for that epoch (a future epoch not yet received, or
    /// a past epoch it was rekeyed out of).
    fn epoch_key(&self, conv_id: &str, epoch: u8) -> Option<[u8; 32]>;
}

/// **Temporary** placeholder [`GroupKeySource`] used until Part A's real
/// sender-keys group keying lands. It derives a single epoch-0 key
/// deterministically from `conv_id` via HKDF.
///
/// # Security
///
/// This provides **no confidentiality against the relay/supernode**: the
/// supernode also knows `conv_id` (the room id) and can derive the identical
/// key. It exists only so the Voice E2E wire path — frame format, AAD binding,
/// send/receive wiring — is exercisable and testable end-to-end today. Part A
/// replaces this *impl* (not the trait) with the real per-room epoch key that
/// is generated as a secret, sealed to members, and never derivable from public
/// routing metadata.
///
/// Do **not** ship a release that relies on this type for actual privacy.
// TODO(part-a): replace with the sender-keys `GroupKeySource` impl.
#[derive(Debug, Default, Clone, Copy)]
pub struct TmpDeterministicGroupKey;

/// HKDF domain-separation label for the placeholder key. The `/v0` and `tmp`
/// make it obvious in captures that this is not the real group key.
const TMP_VOICE_KEY_INFO: &[u8] = b"conquerd-voice-e2e-tmp/v0";

impl GroupKeySource for TmpDeterministicGroupKey {
    fn current_epoch(&self, _conv_id: &str) -> u8 {
        0
    }

    fn epoch_key(&self, conv_id: &str, epoch: u8) -> Option<[u8; 32]> {
        if epoch != 0 {
            return None;
        }
        // ikm = conv_id, info = label ‖ conv_id (deterministic; not secret).
        let mut info = Vec::with_capacity(TMP_VOICE_KEY_INFO.len() + 1 + conv_id.len());
        info.extend_from_slice(TMP_VOICE_KEY_INFO);
        info.push(b'|');
        info.extend_from_slice(conv_id.as_bytes());
        hkdf_derive_key(conv_id.as_bytes(), &info).ok()
    }
}

/// Build the GCM associated data binding a frame to its conversation, sender,
/// and sequence. Length-prefixed so the three variable-length fields can't be
/// re-partitioned (e.g. moving bytes between `conv_id` and `sender`).
fn voice_aad(conv_id: &str, sender: &str, sequence: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(4 + conv_id.len() + 4 + sender.len() + 8);
    aad.extend_from_slice(&(conv_id.len() as u32).to_be_bytes());
    aad.extend_from_slice(conv_id.as_bytes());
    aad.extend_from_slice(&(sender.len() as u32).to_be_bytes());
    aad.extend_from_slice(sender.as_bytes());
    aad.extend_from_slice(&sequence.to_be_bytes());
    aad
}

/// Seal one Opus frame for `conv_id` into `[epoch:u8][nonce:12][aesgcm(opus)]`.
///
/// Returns `None` when `keys` holds no current key for `conv_id` (the caller
/// then decides whether to drop the frame or fall back to a cleartext path).
/// The 96-bit nonce is fresh-random per frame, so nonce uniqueness under a
/// given key does not depend on `sequence`.
pub fn seal_voice_frame(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    sequence: u64,
    opus: &[u8],
) -> Option<Vec<u8>> {
    let epoch = keys.current_epoch(conv_id);
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = voice_aad(conv_id, sender, sequence);
    let (nonce, ct) = aesgcm_encrypt(&key, opus, &aad).ok()?;
    debug_assert_eq!(nonce.len(), VOICE_NONCE_LEN);
    let mut frame = Vec::with_capacity(VOICE_HEADER_LEN + ct.len());
    frame.push(epoch);
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&ct);
    Some(frame)
}

/// Open a frame produced by [`seal_voice_frame`], recovering the Opus bytes.
///
/// Returns `None` on any failure — short frame, unknown epoch, or a failed GCM
/// tag (wrong key, tampered ciphertext, or an AAD mismatch: a `conv_id`,
/// `sender`, or `sequence` that differs from what the sender bound in).
pub fn open_voice_frame(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    sequence: u64,
    frame: &[u8],
) -> Option<Vec<u8>> {
    if frame.len() < VOICE_HEADER_LEN {
        return None;
    }
    let epoch = frame[0];
    let nonce = &frame[1..VOICE_HEADER_LEN];
    let ct = &frame[VOICE_HEADER_LEN..];
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = voice_aad(conv_id, sender, sequence);
    aesgcm_decrypt(&key, nonce, ct, &aad).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONV: &str = "room-abc";
    const SENDER: &str = "alice-public-id";

    #[test]
    fn roundtrip_recovers_opus() {
        let keys = TmpDeterministicGroupKey;
        let opus = b"\x01\x02\x03 fake opus frame \xff\xfe";
        let frame = seal_voice_frame(&keys, CONV, SENDER, 7, opus).unwrap();
        // Frame carries epoch(1) + nonce(12) + ct(>=opus+16 tag).
        assert_eq!(frame[0], 0); // epoch 0 for the tmp source
        assert!(frame.len() >= VOICE_HEADER_LEN + opus.len() + 16);
        let out = open_voice_frame(&keys, CONV, SENDER, 7, &frame).unwrap();
        assert_eq!(out, opus);
    }

    #[test]
    fn wrong_sequence_fails_to_open() {
        let keys = TmpDeterministicGroupKey;
        let frame = seal_voice_frame(&keys, CONV, SENDER, 7, b"data").unwrap();
        // Same key, but the AAD sequence differs → GCM tag check fails.
        assert!(open_voice_frame(&keys, CONV, SENDER, 8, &frame).is_none());
    }

    #[test]
    fn wrong_sender_or_conv_fails_to_open() {
        let keys = TmpDeterministicGroupKey;
        let frame = seal_voice_frame(&keys, CONV, SENDER, 1, b"data").unwrap();
        assert!(open_voice_frame(&keys, CONV, "mallory", 1, &frame).is_none());
        // A different conv id derives a different key *and* a different AAD.
        assert!(open_voice_frame(&keys, "other-room", SENDER, 1, &frame).is_none());
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let keys = TmpDeterministicGroupKey;
        let mut frame = seal_voice_frame(&keys, CONV, SENDER, 1, b"payload").unwrap();
        let last = frame.len() - 1;
        frame[last] ^= 0x01; // flip a tag bit
        assert!(open_voice_frame(&keys, CONV, SENDER, 1, &frame).is_none());
    }

    #[test]
    fn nonce_is_fresh_per_frame() {
        let keys = TmpDeterministicGroupKey;
        let a = seal_voice_frame(&keys, CONV, SENDER, 1, b"data").unwrap();
        let b = seal_voice_frame(&keys, CONV, SENDER, 1, b"data").unwrap();
        // Same inputs, but the random nonce differs, so ciphertext differs.
        assert_ne!(a[1..VOICE_HEADER_LEN], b[1..VOICE_HEADER_LEN]);
        assert_ne!(a, b);
    }

    #[test]
    fn short_frame_is_rejected() {
        let keys = TmpDeterministicGroupKey;
        assert!(open_voice_frame(&keys, CONV, SENDER, 1, &[0u8; 5]).is_none());
        assert!(open_voice_frame(&keys, CONV, SENDER, 1, &[]).is_none());
    }

    #[test]
    fn tmp_source_has_no_key_for_nonzero_epoch() {
        let keys = TmpDeterministicGroupKey;
        assert!(keys.epoch_key(CONV, 1).is_none());
        assert!(keys.epoch_key(CONV, 0).is_some());
    }
}
