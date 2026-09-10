//! Group keying for end-to-end encrypted room traffic.
//!
//! This module defines the [`GroupKeySource`] trait — the read seam the codec
//! consumes — and [`SenderKeysGroup`], the **sender-keys** group keying that
//! backs it: a room's elected keyer (see `connection_manager::manager` —
//! deterministically the lexicographically smallest member `public_id`
//! currently present, not necessarily whoever created the room) generates a
//! per-epoch key and seals it to each member 1:1 (the `SfuGroupKey` path), and
//! rekeys on member removal for forward secrecy / post-compromise security.
//! Any member holding real key material can reseal it to a newcomer, so a
//! joiner reliably receives the current epoch on any join path — including
//! the built-in `default` room, which has no client-side creator at all (see
//! `backlog.md` "Crypto — group key reliability"). Until real key material has
//! been generated or received for a conversation, [`Self`] transparently falls
//! back to a deterministic per-room key (every member can derive it locally,
//! at the cost of zero confidentiality vs. the relay) so audio/chat/file keep
//! working during that gap. TreeKEM was considered as a future O(log N)
//! upgrade behind this same trait and declined (see `backlog.md` — invite-only
//! rooms don't hit the O(N) rekey cost that would justify it); this is the
//! long-term keying scheme, not an interim one.
//!
//! Two wire codecs share the epoch key:
//!
//! * **Voice** — each Opus frame is wrapped before the datagram path as
//!   `[epoch:u8][nonce:12][AES-256-GCM(opus)]`, `AAD = conv_id ‖ sender ‖
//!   sequence`, carried in the `ROOM_AUDIO_TAG` (0x04) `SfuAudio` envelope
//!   (`epoch` rides the frame, `sequence` rides the signed envelope as `seq`).
//! * **Room text chat** — the `SfuChat` `body` is sealed as
//!   `nonce ‖ AES-256-GCM(body)`, `AAD = conv_id ‖ sender ‖ message_id`, with
//!   `epoch` on the envelope.
//! * **Room file transfer** — each `SfuFileChunk` `data` field is sealed as
//!   `nonce ‖ AES-256-GCM(data)`, `AAD = conv_id ‖ sender ‖ transfer_id ‖
//!   chunk_index`, with `epoch` on the envelope. `SfuFileOffer` / `SfuFileComplete`
//!   metadata (size, sha256, rel_path) stays cleartext — only the chunk bytes
//!   need sealing to close the SFU content-opacity gap.
//!
//! The relay stays a dumb forwarder — it never sees the key or the plaintext.

use std::collections::{BTreeMap, HashMap};

use crate::crypto::{aesgcm_decrypt, aesgcm_encrypt, generate_nonce, hkdf_derive_key};

/// Length of the per-frame AES-GCM nonce (bytes).
pub const VOICE_NONCE_LEN: usize = 12;
/// Fixed frame prefix: `epoch (1)` + `nonce (12)`.
const VOICE_HEADER_LEN: usize = 1 + VOICE_NONCE_LEN;
/// Length of a group epoch key (AES-256).
pub const GROUP_KEY_LEN: usize = 32;
/// Retained epochs per conversation. Bounds memory while keeping a few just-
/// rotated-out keys around so frames in flight across a rekey still open.
const MAX_RETAINED_EPOCHS: usize = 4;

/// Source of per-conversation group keys for E2E room traffic.
///
/// The trait is deliberately tiny: a caller asks for the epoch to seal *new*
/// frames under, and can look up the key for any epoch it holds (needed to
/// *open* frames that were in flight across a rekey). [`SenderKeysGroup`]
/// supplies the real (and, per the backlog decision, permanent) implementation
/// behind this trait.
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

/// Per-conversation epoch state: the current epoch plus a bounded set of
/// recent epoch keys (newest wins for sealing; older ones open in-flight data).
#[derive(Debug, Default, Clone)]
struct GroupState {
    current: u8,
    /// Highest epoch seen on the wire for this conversation, including epochs
    /// we hold no key for.
    ///
    /// A peer that kept running while we restarted is ahead of us, and its
    /// frames are the only evidence of how far. Minting starts above this so a
    /// restarted keyer never offers an epoch the room has already passed - an
    /// offer every member would refuse, since `accept_group_key_epoch` cannot
    /// tell a restart from a rollback.
    seen_high: Option<u8>,
    keys: BTreeMap<u8, [u8; GROUP_KEY_LEN]>,
}

impl GroupState {
    /// Insert `key` at `epoch`, advance `current` to the newest epoch, and cap
    /// the retained key history.
    fn install(&mut self, epoch: u8, key: [u8; GROUP_KEY_LEN]) {
        self.keys.insert(epoch, key);
        // Treat the just-installed epoch as newest (owner rotates monotonically
        // and members receive increasing epochs within a session).
        self.current = epoch;
        self.note_seen(epoch);
        while self.keys.len() > MAX_RETAINED_EPOCHS {
            // Drop the lowest-numbered (oldest) retained epoch.
            if let Some(&oldest) = self.keys.keys().next() {
                self.keys.remove(&oldest);
            }
        }
    }

    fn note_seen(&mut self, epoch: u8) {
        self.seen_high = Some(match self.seen_high {
            Some(high) if high >= epoch => high,
            _ => epoch,
        });
    }

    /// Real key material, as opposed to an entry that only records what we have
    /// seen. Keeps an observation from masquerading as a key.
    fn has_keys(&self) -> bool {
        !self.keys.is_empty()
    }

    /// The epoch to mint next: above both what we hold and what we have seen.
    fn next_epoch(&self) -> u8 {
        let floor = match self.seen_high {
            Some(high) if high >= self.current => high,
            _ => self.current,
        };
        floor.wrapping_add(1)
    }
}

/// Sender-keys [`GroupKeySource`]: in-memory per-conversation epoch keys.
///
/// The room's elected keyer generates the key ([`Self::new_owner_epoch`]) and
/// rotates it on member removal ([`Self::rotate`]); every member installs keys
/// received over the sealed `SfuGroupKey` path ([`Self::install`]). Keys are
/// transport-only and held in memory: room chat is decrypted before it reaches
/// the chat store (which re-encrypts at rest under its own key) and audio is
/// ephemeral, so nothing here needs to survive a restart. A fresh epoch each
/// session is expected and cheap.
///
/// `epoch` is a `u8` (fixed by the voice frame format); it wraps after 256
/// rotations in a single session — far beyond realistic membership churn.
#[derive(Debug, Default)]
pub struct SenderKeysGroup {
    groups: HashMap<String, GroupState>,
}

impl SenderKeysGroup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Owner: start a fresh group for `conv_id` at epoch 0 with a random key,
    /// replacing any existing state. Returns `(epoch, key)` to seal to members.
    pub fn new_owner_epoch(&mut self, conv_id: &str) -> (u8, [u8; GROUP_KEY_LEN]) {
        let key = random_key();
        // Epoch 0 only when this conversation is genuinely new to us. If we
        // have seen the room at a higher epoch - a member kept running while we
        // restarted - start above it. Offering 0 to a member already past it is
        // refused as a rollback, and since the offer is refused silently the
        // keyer retries the same doomed epoch until it gives up.
        let epoch = self
            .groups
            .get(conv_id)
            .map(GroupState::next_epoch)
            .unwrap_or(0);
        let mut state = GroupState {
            seen_high: self.groups.get(conv_id).and_then(|s| s.seen_high),
            ..GroupState::default()
        };
        state.install(epoch, key);
        self.groups.insert(conv_id.to_owned(), state);
        (epoch, key)
    }

    /// Record an epoch seen on the wire that we hold no key for.
    ///
    /// Pure observation: it never creates key material, so the deterministic
    /// fallback and every `has_real_key` gate behave exactly as before. It only
    /// raises the floor the next mint starts above.
    pub fn note_observed_epoch(&mut self, conv_id: &str, epoch: u8) {
        self.groups
            .entry(conv_id.to_owned())
            .or_default()
            .note_seen(epoch);
    }

    /// True when the room has moved to an epoch we cannot open.
    ///
    /// The signal for an elected keyer to catch up: it restarted, minted from
    /// scratch, and is now behind members that never went away.
    pub fn is_behind(&self, conv_id: &str) -> bool {
        self.groups.get(conv_id).is_some_and(|s| match s.seen_high {
            Some(high) => !s.keys.contains_key(&high),
            None => false,
        })
    }

    /// Owner: bump to the next epoch with a fresh random key (rekey on member
    /// removal → forward secrecy + PCS). Returns `(new_epoch, key)` to reseal to
    /// the remaining members. Falls back to [`Self::new_owner_epoch`] if the
    /// group is somehow absent.
    pub fn rotate(&mut self, conv_id: &str) -> (u8, [u8; GROUP_KEY_LEN]) {
        let Some(state) = self.groups.get_mut(conv_id) else {
            return self.new_owner_epoch(conv_id);
        };
        let next = state.next_epoch();
        let key = random_key();
        state.install(next, key);
        (next, key)
    }

    /// Member (or owner echo): install a key received for `(conv_id, epoch)`.
    pub fn install(&mut self, conv_id: &str, epoch: u8, key: [u8; GROUP_KEY_LEN]) {
        self.groups
            .entry(conv_id.to_owned())
            .or_default()
            .install(epoch, key);
    }

    /// Always true: a room always has at least the deterministic epoch-0 key.
    pub fn has_key(&self, _conv_id: &str) -> bool {
        true
    }

    /// True when this peer holds real (distributed, non-deterministic) key
    /// material for `conv_id` — generated locally via [`Self::new_owner_epoch`]
    /// / [`Self::rotate`], or received over the sealed `SfuGroupKey` path via
    /// [`Self::install`]. Used to decide whether this peer already has
    /// something to hand on to newcomers (see `connection_manager::manager`'s
    /// elected-keyer logic) and whether the deterministic fallback below is
    /// still in play for `conv_id`.
    pub fn has_real_key(&self, conv_id: &str) -> bool {
        self.groups.get(conv_id).is_some_and(GroupState::has_keys)
    }

    /// Forget any distributed key material for `conv_id` (the deterministic
    /// fallback remains available).
    pub fn forget(&mut self, conv_id: &str) {
        self.groups.remove(conv_id);
    }
}

impl GroupKeySource for SenderKeysGroup {
    fn current_epoch(&self, conv_id: &str) -> u8 {
        // The highest epoch this peer actually holds real key material for, or
        // 0 (the deterministic fallback epoch) when no real key has been
        // generated/received yet for this conversation — e.g. the brief window
        // right after joining before the elected keyer's `SfuGroupKey` arrives.
        self.groups
            .get(conv_id)
            .filter(|s| s.has_keys())
            .map(|s| s.current)
            .unwrap_or(0)
    }

    fn epoch_key(&self, conv_id: &str, epoch: u8) -> Option<[u8; GROUP_KEY_LEN]> {
        if let Some(state) = self.groups.get(conv_id).filter(|s| s.has_keys()) {
            // We hold real key material for this conversation — use it, even
            // at epoch 0 (a real owner-generated epoch-0 key, not the
            // deterministic one). A requested epoch we don't hold (evicted or
            // not yet received) fails closed rather than silently downgrading
            // to the deterministic key.
            return state.keys.get(&epoch).copied();
        }
        if epoch == 0 {
            // No real key has been generated/received yet for `conv_id`. Fall
            // back to the deterministic per-room key so audio/chat/file keep
            // working in the gap before real keying lands; this is superseded
            // the instant a real key is installed via `install`/
            // `new_owner_epoch` (no confidentiality vs. the relay until then —
            // it also knows `conv_id`).
            return Some(deterministic_room_key(conv_id));
        }
        None
    }
}

/// Generate a random 32-byte group key.
fn random_key() -> [u8; GROUP_KEY_LEN] {
    let mut key = [0u8; GROUP_KEY_LEN];
    key.copy_from_slice(&generate_nonce(GROUP_KEY_LEN));
    key
}

/// HKDF domain-separation label for the deterministic per-room key.
const ROOM_KEY_INFO: &[u8] = b"conquerd-room-key/v1";

/// Deterministic 32-byte key shared by every member of `conv_id` (the room id).
/// Derived so all peers agree without any distribution step.
fn deterministic_room_key(conv_id: &str) -> [u8; GROUP_KEY_LEN] {
    let mut info = Vec::with_capacity(ROOM_KEY_INFO.len() + 1 + conv_id.len());
    info.extend_from_slice(ROOM_KEY_INFO);
    info.push(b'|');
    info.extend_from_slice(conv_id.as_bytes());
    hkdf_derive_key(conv_id.as_bytes(), &info).unwrap_or([0u8; GROUP_KEY_LEN])
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

/// Which media stream a sealed frame belongs to.
///
/// Audio and video each run their own `sequence` counter starting at 0, so
/// without a domain separator a video frame at `seq = 5` and an audio frame at
/// `seq = 5` would share identical AAD under the same group key — letting an
/// attacker substitute one for the other and still pass the GCM tag check.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum MediaKind {
    /// Opus voice frames (`room.audio.sfu`, `core.audio.opus`).
    Voice,
    /// Video frames (`room.video.sfu`, `core.video.v1`).
    Video,
    /// Content / system audio that accompanies video (`room.audio.content.sfu`,
    /// `core.audio.content.v1`).
    ///
    /// Separate from [`Voice`](Self::Voice) even though both carry Opus: the
    /// two are different streams with different sequence spaces, so without a
    /// distinct domain a captured content frame could be replayed as a voice
    /// frame at the same sequence and still pass the GCM tag check.
    ContentAudio,
}

/// AAD for a media frame, domain-separated by [`MediaKind`].
///
/// **`MediaKind::Voice` deliberately appends nothing**, so its bytes stay
/// identical to what [`voice_aad`] has always produced. That is a wire-format
/// compatibility requirement, not a stylistic choice: every deployed client
/// computes the audio AAD the old way, and any change here makes their frames
/// fail to open — the whole network goes silent through an upgrade.
///
/// Putting the kind byte at the *front* would read more cleanly. Do not do it;
/// it changes the audio bytes. New kinds must only ever *append*.
fn media_aad(kind: MediaKind, conv_id: &str, sender: &str, sequence: u64) -> Vec<u8> {
    let mut aad = voice_aad(conv_id, sender, sequence);
    match kind {
        MediaKind::Voice => {}
        MediaKind::Video => aad.push(0x02),
        MediaKind::ContentAudio => aad.push(0x03),
    }
    aad
}

/// Seal one media frame for `conv_id` into `[epoch:u8][nonce:12][aesgcm(pt)]`.
///
/// Returns `None` when `keys` holds no current key for `conv_id` (the caller
/// then decides whether to drop the frame or fall back to a cleartext path).
/// The 96-bit nonce is fresh-random per frame, so nonce uniqueness under a
/// given key does not depend on `sequence`.
///
/// For video the caller seals the **whole encoded frame once** and fragments
/// the resulting ciphertext; fragments are not sealed individually (a partial
/// frame cannot be decoded anyway, so per-fragment sealing would cost 28 bytes
/// and one GCM operation per fragment for no benefit).
pub fn seal_media_frame(
    keys: &dyn GroupKeySource,
    kind: MediaKind,
    conv_id: &str,
    sender: &str,
    sequence: u64,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let epoch = keys.current_epoch(conv_id);
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = media_aad(kind, conv_id, sender, sequence);
    let (nonce, ct) = aesgcm_encrypt(&key, plaintext, &aad).ok()?;
    debug_assert_eq!(nonce.len(), VOICE_NONCE_LEN);
    let mut frame = Vec::with_capacity(VOICE_HEADER_LEN + ct.len());
    frame.push(epoch);
    frame.extend_from_slice(&nonce);
    frame.extend_from_slice(&ct);
    Some(frame)
}

/// Open a frame produced by [`seal_media_frame`], recovering the plaintext.
///
/// Returns `None` on any failure — short frame, unknown epoch, or a failed GCM
/// tag (wrong key, tampered ciphertext, or an AAD mismatch: a `conv_id`,
/// `sender`, `sequence`, or [`MediaKind`] that differs from what the sender
/// bound in).
pub fn open_media_frame(
    keys: &dyn GroupKeySource,
    kind: MediaKind,
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
    let aad = media_aad(kind, conv_id, sender, sequence);
    aesgcm_decrypt(&key, nonce, ct, &aad).ok()
}

/// Seal one Opus frame. Thin wrapper over [`seal_media_frame`] with
/// [`MediaKind::Voice`]; the produced bytes are unchanged from before video
/// existed.
pub fn seal_voice_frame(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    sequence: u64,
    opus: &[u8],
) -> Option<Vec<u8>> {
    seal_media_frame(keys, MediaKind::Voice, conv_id, sender, sequence, opus)
}

/// Open a frame produced by [`seal_voice_frame`], recovering the Opus bytes.
pub fn open_voice_frame(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    sequence: u64,
    frame: &[u8],
) -> Option<Vec<u8>> {
    open_media_frame(keys, MediaKind::Voice, conv_id, sender, sequence, frame)
}

// ---------------------------------------------------------------------------
// Room text-chat body codec
// ---------------------------------------------------------------------------

/// AAD binding a chat body to its room, sender, and message id. Length-prefixed
/// like [`voice_aad`] so the variable-length fields can't be re-partitioned.
fn chat_aad(conv_id: &str, sender: &str, message_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(4 + conv_id.len() + 4 + sender.len() + 4 + message_id.len());
    for field in [conv_id, sender, message_id] {
        aad.extend_from_slice(&(field.len() as u32).to_be_bytes());
        aad.extend_from_slice(field.as_bytes());
    }
    aad
}

/// Seal a room chat `body` under the current epoch key as `nonce ‖ aesgcm(body)`.
///
/// Returns `(epoch, sealed)` so the caller can put `epoch` on the envelope, or
/// `None` when no current key exists for `conv_id`. `AAD = conv_id ‖ sender ‖
/// message_id`.
pub fn seal_chat_body(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    message_id: &str,
    body: &[u8],
) -> Option<(u8, Vec<u8>)> {
    let epoch = keys.current_epoch(conv_id);
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = chat_aad(conv_id, sender, message_id);
    let (nonce, ct) = aesgcm_encrypt(&key, body, &aad).ok()?;
    let mut sealed = Vec::with_capacity(nonce.len() + ct.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ct);
    Some((epoch, sealed))
}

/// Open a chat body sealed by [`seal_chat_body`] under `(conv_id, epoch)`.
pub fn open_chat_body(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    message_id: &str,
    epoch: u8,
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < VOICE_NONCE_LEN {
        return None;
    }
    let nonce = &sealed[..VOICE_NONCE_LEN];
    let ct = &sealed[VOICE_NONCE_LEN..];
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = chat_aad(conv_id, sender, message_id);
    aesgcm_decrypt(&key, nonce, ct, &aad).ok()
}

// ---------------------------------------------------------------------------
// Room file-chunk codec
// ---------------------------------------------------------------------------

/// AAD binding a file chunk to its room, sender, transfer, and chunk index.
/// Length-prefixed like [`voice_aad`] / [`chat_aad`] so the variable-length
/// fields can't be re-partitioned.
fn file_chunk_aad(conv_id: &str, sender: &str, transfer_id: &str, chunk_index: u64) -> Vec<u8> {
    let mut aad =
        Vec::with_capacity(4 + conv_id.len() + 4 + sender.len() + 4 + transfer_id.len() + 8);
    for field in [conv_id, sender, transfer_id] {
        aad.extend_from_slice(&(field.len() as u32).to_be_bytes());
        aad.extend_from_slice(field.as_bytes());
    }
    aad.extend_from_slice(&chunk_index.to_be_bytes());
    aad
}

/// Seal a `room.file.v1` chunk's raw bytes under the current epoch key as
/// `nonce ‖ aesgcm(data)`.
///
/// Returns `(epoch, sealed)` so the caller can put `epoch` on the envelope, or
/// `None` when no current key exists for `conv_id`. `AAD = conv_id ‖ sender ‖
/// transfer_id ‖ chunk_index`.
pub fn seal_file_chunk(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    transfer_id: &str,
    chunk_index: u64,
    data: &[u8],
) -> Option<(u8, Vec<u8>)> {
    let epoch = keys.current_epoch(conv_id);
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = file_chunk_aad(conv_id, sender, transfer_id, chunk_index);
    let (nonce, ct) = aesgcm_encrypt(&key, data, &aad).ok()?;
    let mut sealed = Vec::with_capacity(nonce.len() + ct.len());
    sealed.extend_from_slice(&nonce);
    sealed.extend_from_slice(&ct);
    Some((epoch, sealed))
}

/// Open a chunk sealed by [`seal_file_chunk`] under `(conv_id, epoch)`.
pub fn open_file_chunk(
    keys: &dyn GroupKeySource,
    conv_id: &str,
    sender: &str,
    transfer_id: &str,
    chunk_index: u64,
    epoch: u8,
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < VOICE_NONCE_LEN {
        return None;
    }
    let nonce = &sealed[..VOICE_NONCE_LEN];
    let ct = &sealed[VOICE_NONCE_LEN..];
    let key = keys.epoch_key(conv_id, epoch)?;
    let aad = file_chunk_aad(conv_id, sender, transfer_id, chunk_index);
    aesgcm_decrypt(&key, nonce, ct, &aad).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONV: &str = "room-abc";
    const SENDER: &str = "alice-public-id";

    /// An owner group with a single epoch-0 key, for codec tests.
    fn owner_group() -> SenderKeysGroup {
        let mut g = SenderKeysGroup::new();
        g.new_owner_epoch(CONV);
        g
    }

    #[test]
    fn roundtrip_recovers_opus() {
        let keys = owner_group();
        let opus = b"\x01\x02\x03 fake opus frame \xff\xfe";
        let frame = seal_voice_frame(&keys, CONV, SENDER, 7, opus).unwrap();
        assert_eq!(frame[0], 0); // epoch 0
        assert!(frame.len() >= VOICE_HEADER_LEN + opus.len() + 16);
        let out = open_voice_frame(&keys, CONV, SENDER, 7, &frame).unwrap();
        assert_eq!(out, opus);
    }

    #[test]
    fn wrong_sequence_fails_to_open() {
        let keys = owner_group();
        let frame = seal_voice_frame(&keys, CONV, SENDER, 7, b"data").unwrap();
        assert!(open_voice_frame(&keys, CONV, SENDER, 8, &frame).is_none());
    }

    /// GOLDEN: the voice AAD byte layout is frozen. Every deployed client
    /// computes these exact bytes; changing them makes existing peers fail to
    /// open our frames (and us theirs), so the network goes silent through an
    /// upgrade. If this test fails, the change is wrong — not the test.
    #[test]
    fn voice_aad_bytes_are_frozen() {
        let aad = voice_aad("room-1", "alice", 7);
        assert_eq!(
            aad,
            vec![
                0, 0, 0, 6, // conv_id length (u32 BE)
                b'r', b'o', b'o', b'm', b'-', b'1', //
                0, 0, 0, 5, // sender length (u32 BE)
                b'a', b'l', b'i', b'c', b'e', //
                0, 0, 0, 0, 0, 0, 0, 7, // sequence (u64 BE)
            ]
        );
    }

    /// GOLDEN: `MediaKind::Voice` must append nothing, so the wrapper stays
    /// byte-identical to the pre-video `voice_aad`.
    #[test]
    fn voice_media_aad_is_identical_to_legacy_voice_aad() {
        assert_eq!(
            media_aad(MediaKind::Voice, CONV, SENDER, 42),
            voice_aad(CONV, SENDER, 42)
        );
    }

    #[test]
    fn video_aad_differs_from_voice_at_same_seq() {
        let voice = media_aad(MediaKind::Voice, CONV, SENDER, 5);
        let video = media_aad(MediaKind::Video, CONV, SENDER, 5);
        assert_ne!(voice, video);
        // Video appends its separator, so voice remains a strict prefix.
        assert_eq!(&video[..voice.len()], &voice[..]);
        assert_eq!(video.len(), voice.len() + 1);
    }

    /// The substitution attack the domain separator exists to stop: a video
    /// frame at sequence N must not open as audio at sequence N, or vice
    /// versa, even though both are sealed under the same group key.
    #[test]
    fn video_frame_cannot_be_substituted_for_voice_frame() {
        let keys = owner_group();
        let payload = b"frame bytes";

        let video = seal_media_frame(&keys, MediaKind::Video, CONV, SENDER, 5, payload).unwrap();
        assert!(open_voice_frame(&keys, CONV, SENDER, 5, &video).is_none());

        let voice = seal_voice_frame(&keys, CONV, SENDER, 5, payload).unwrap();
        assert!(open_media_frame(&keys, MediaKind::Video, CONV, SENDER, 5, &voice).is_none());
    }

    #[test]
    fn video_frame_roundtrips() {
        let keys = owner_group();
        let encoded = b"\x9d\x01\x2a fake vp8 keyframe \x00\xff";
        let sealed = seal_media_frame(&keys, MediaKind::Video, CONV, SENDER, 3, encoded).unwrap();
        assert_eq!(sealed[0], 0); // epoch 0
        let out = open_media_frame(&keys, MediaKind::Video, CONV, SENDER, 3, &sealed).unwrap();
        assert_eq!(out, encoded);
    }

    #[test]
    fn wrong_sender_or_conv_fails_to_open() {
        let keys = owner_group();
        let frame = seal_voice_frame(&keys, CONV, SENDER, 1, b"data").unwrap();
        assert!(open_voice_frame(&keys, CONV, "mallory", 1, &frame).is_none());
        // A different conv id derives a different room key → tag check fails.
        assert!(open_voice_frame(&keys, "other-room", SENDER, 1, &frame).is_none());
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let keys = owner_group();
        let mut frame = seal_voice_frame(&keys, CONV, SENDER, 1, b"payload").unwrap();
        let last = frame.len() - 1;
        frame[last] ^= 0x01;
        assert!(open_voice_frame(&keys, CONV, SENDER, 1, &frame).is_none());
    }

    #[test]
    fn nonce_is_fresh_per_frame() {
        let keys = owner_group();
        let a = seal_voice_frame(&keys, CONV, SENDER, 1, b"data").unwrap();
        let b = seal_voice_frame(&keys, CONV, SENDER, 1, b"data").unwrap();
        assert_ne!(a[1..VOICE_HEADER_LEN], b[1..VOICE_HEADER_LEN]);
        assert_ne!(a, b);
    }

    #[test]
    fn short_frame_is_rejected() {
        let keys = owner_group();
        assert!(open_voice_frame(&keys, CONV, SENDER, 1, &[0u8; 5]).is_none());
        assert!(open_voice_frame(&keys, CONV, SENDER, 1, &[]).is_none());
    }

    #[test]
    fn every_room_has_a_deterministic_key() {
        // A never-distributed group still seals: every room has the deterministic
        // epoch-0 key, so audio/chat work even in rooms a peer only joined.
        let keys = SenderKeysGroup::new();
        assert!(keys.has_key(CONV));
        assert!(seal_voice_frame(&keys, CONV, SENDER, 1, b"x").is_some());
        assert!(seal_chat_body(&keys, CONV, SENDER, "m1", b"x").is_some());
    }

    #[test]
    fn chat_body_roundtrip_and_binding() {
        let keys = owner_group();
        let (epoch, sealed) = seal_chat_body(&keys, CONV, SENDER, "m1", b"hello room").unwrap();
        assert_eq!(epoch, 0);
        let out = open_chat_body(&keys, CONV, SENDER, "m1", epoch, &sealed).unwrap();
        assert_eq!(out, b"hello room");
        // Wrong message_id / sender / epoch all fail the tag check.
        assert!(open_chat_body(&keys, CONV, SENDER, "m2", epoch, &sealed).is_none());
        assert!(open_chat_body(&keys, CONV, "eve", "m1", epoch, &sealed).is_none());
        assert!(open_chat_body(&keys, CONV, SENDER, "m1", 1, &sealed).is_none());
    }

    #[test]
    fn two_independent_members_interop_via_deterministic_key() {
        // Two members that never exchanged a key still agree, because both derive
        // the same deterministic per-room key — this is what makes audio work in
        // any room. (Consequence: anyone who knows the room id can derive it —
        // the documented "no confidentiality vs. the relay" caveat.)
        let a = SenderKeysGroup::new();
        let b = SenderKeysGroup::new();
        let frame = seal_voice_frame(&a, CONV, SENDER, 3, b"opus").unwrap();
        assert_eq!(
            open_voice_frame(&b, CONV, SENDER, 3, &frame),
            Some(b"opus".to_vec())
        );
        // A different room derives a different key, so cross-room frames fail.
        assert!(open_voice_frame(&b, "other-room", SENDER, 3, &frame).is_none());
    }

    #[test]
    fn file_chunk_roundtrip_and_binding() {
        let keys = owner_group();
        let (epoch, sealed) =
            seal_file_chunk(&keys, CONV, SENDER, "xfer-1", 3, b"chunk bytes").unwrap();
        assert_eq!(epoch, 0);
        let out = open_file_chunk(&keys, CONV, SENDER, "xfer-1", 3, epoch, &sealed).unwrap();
        assert_eq!(out, b"chunk bytes");
        // Wrong transfer_id / chunk_index / sender / epoch all fail the tag check.
        assert!(open_file_chunk(&keys, CONV, SENDER, "xfer-2", 3, epoch, &sealed).is_none());
        assert!(open_file_chunk(&keys, CONV, SENDER, "xfer-1", 4, epoch, &sealed).is_none());
        assert!(open_file_chunk(&keys, CONV, "eve", "xfer-1", 3, epoch, &sealed).is_none());
        assert!(open_file_chunk(&keys, CONV, SENDER, "xfer-1", 3, 1, &sealed).is_none());
    }

    #[test]
    fn file_chunk_tampered_ciphertext_fails_to_open() {
        let keys = owner_group();
        let (epoch, mut sealed) =
            seal_file_chunk(&keys, CONV, SENDER, "xfer-1", 0, b"payload").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0x01;
        assert!(open_file_chunk(&keys, CONV, SENDER, "xfer-1", 0, epoch, &sealed).is_none());
    }

    #[test]
    fn retained_epochs_are_bounded() {
        let mut owner = SenderKeysGroup::new();
        owner.new_owner_epoch(CONV);
        for _ in 0..10 {
            owner.rotate(CONV);
        }
        let state = owner.groups.get(CONV).unwrap();
        assert!(state.keys.len() <= MAX_RETAINED_EPOCHS);
        // The current epoch key is always retained.
        assert!(state.keys.contains_key(&state.current));
    }

    #[test]
    fn real_key_is_actually_used_once_distributed() {
        // Regression test for the reliability fix: once a member holds real
        // key material, encryption must use it — not silently stay pinned to
        // the deterministic fallback (the previously-shipped, "wired but
        // dormant" behavior this closes out).
        let keys = owner_group();
        assert!(keys.has_real_key(CONV));
        let real_key = keys.epoch_key(CONV, 0).unwrap();
        assert_ne!(real_key, deterministic_room_key(CONV));
    }

    #[test]
    fn rotation_is_actually_consumed_for_new_frames() {
        // Before the fix, `current_epoch` was hardcoded to 0 forever, so a
        // rotated (higher-epoch) key was generated and distributed but never
        // actually selected for new outgoing frames. Confirm it now is.
        let mut owner = SenderKeysGroup::new();
        owner.new_owner_epoch(CONV);
        let (new_epoch, new_key) = owner.rotate(CONV);
        assert_eq!(owner.current_epoch(CONV), new_epoch);
        assert_eq!(owner.epoch_key(CONV, new_epoch), Some(new_key));

        let frame = seal_voice_frame(&owner, CONV, SENDER, 1, b"post-rotate").unwrap();
        assert_eq!(frame[0], new_epoch);
    }

    #[test]
    fn unknown_higher_epoch_fails_closed_instead_of_falling_back() {
        // A member with real key state for a conversation, asked to open a
        // frame at an epoch it never received, must fail rather than silently
        // downgrading to the deterministic key.
        let keys = owner_group();
        assert!(keys.epoch_key(CONV, 5).is_none());
    }

    #[test]
    fn no_real_key_yet_falls_back_to_deterministic_only_at_epoch_zero() {
        let keys = SenderKeysGroup::new();
        assert!(!keys.has_real_key(CONV));
        assert_eq!(keys.current_epoch(CONV), 0);
        assert_eq!(keys.epoch_key(CONV, 0), Some(deterministic_room_key(CONV)));
        assert_eq!(keys.epoch_key(CONV, 1), None);
    }

    // ── Restarted keyer / epoch monotonicity ────────────────────────────────

    /// The deadlock this exists for.
    ///
    /// Keys are in-memory by design, so a keyer that restarts has no epoch
    /// state and used to mint 0. Every member still running holds a higher
    /// epoch and refuses 0 as a rollback, silently, so the keyer retried a
    /// doomed epoch until it gave up and the room stayed wedged - nobody else
    /// would mint, because the keyer is the elected one.
    #[test]
    fn a_restarted_keyer_mints_above_the_epoch_it_has_seen() {
        let mut restarted = SenderKeysGroup::new();
        // All a restarted keyer knows is the epoch on frames it cannot open.
        restarted.note_observed_epoch(CONV, 4);

        let (epoch, _) = restarted.new_owner_epoch(CONV);

        assert_eq!(epoch, 5, "must mint above the room, not below it");
    }

    /// And the offer has to be one a member will actually take: the whole point
    /// is that `accept_group_key_epoch` sees an ordinary forward rotation.
    #[test]
    fn a_member_accepts_what_a_restarted_keyer_offers() {
        let member_epoch = 4u8;

        let mut restarted = SenderKeysGroup::new();
        restarted.note_observed_epoch(CONV, member_epoch);
        let (offered, _) = restarted.new_owner_epoch(CONV);

        assert!(
            crate::connection_manager::manager::accept_group_key_epoch(true, member_epoch, offered),
            "member at epoch {member_epoch} refused the restarted keyer's epoch {offered}",
        );
        // The old behaviour, kept here as the thing that must never come back.
        assert!(
            !crate::connection_manager::manager::accept_group_key_epoch(true, member_epoch, 0),
            "epoch 0 must still read as a rollback",
        );
    }

    /// A fresh conversation is still epoch 0 - the catch-up must not inflate
    /// epochs for rooms that never went anywhere.
    #[test]
    fn an_unseen_conversation_still_starts_at_zero() {
        let mut group = SenderKeysGroup::new();
        assert_eq!(group.new_owner_epoch(CONV).0, 0);
    }

    /// Observing an epoch must not look like holding a key. `has_real_key`
    /// gates outbound sends and the deterministic fallback, so an observation
    /// leaking into either would either send under a key nobody has or disable
    /// the fallback that covers the gap before real keying lands.
    #[test]
    fn an_observation_is_not_key_material() {
        let mut group = SenderKeysGroup::new();
        group.note_observed_epoch(CONV, 3);

        assert!(!group.has_real_key(CONV), "an observation is not a key");
        assert!(
            !crate::connection_manager::manager::may_send_room_e2e_content(
                group.has_real_key(CONV)
            ),
            "outbound must stay closed on an observation alone",
        );
        assert_eq!(group.current_epoch(CONV), 0);
        assert_eq!(
            group.epoch_key(CONV, 0),
            Some(deterministic_room_key(CONV)),
            "the deterministic fallback must survive an observation",
        );
    }

    /// `is_behind` is the trigger for catching up, so it must fire only when
    /// the room really has moved past what we can open.
    #[test]
    fn is_behind_only_when_an_epoch_is_out_of_reach() {
        let mut group = SenderKeysGroup::new();
        assert!(
            !group.is_behind(CONV),
            "nothing seen, nothing to catch up to"
        );

        group.new_owner_epoch(CONV);
        assert!(!group.is_behind(CONV), "our own epoch is not ahead of us");

        group.note_observed_epoch(CONV, 7);
        assert!(group.is_behind(CONV), "epoch 7 is unopenable");

        group.install(CONV, 7, [9u8; GROUP_KEY_LEN]);
        assert!(!group.is_behind(CONV), "installing 7 caught us up");
    }

    /// Rotation has to clear the backlog too: a keyer that learned of epoch 6
    /// and then rekeys on a membership change must land above 6, not at 1.
    #[test]
    fn rotate_also_clears_a_backlog() {
        let mut group = SenderKeysGroup::new();
        group.new_owner_epoch(CONV);
        group.note_observed_epoch(CONV, 6);

        let (epoch, key) = group.rotate(CONV);

        assert_eq!(epoch, 7);
        assert_eq!(group.epoch_key(CONV, 7), Some(key));
        assert!(!group.is_behind(CONV));
    }

    /// Catching up must leave the room readable: frames sealed under the new
    /// epoch open, which is the outcome all of the above is for.
    #[test]
    fn a_caught_up_keyer_and_member_can_talk_again() {
        let mut keyer = SenderKeysGroup::new();
        keyer.note_observed_epoch(CONV, 4);
        let (epoch, key) = keyer.new_owner_epoch(CONV);

        // The member installs it as an ordinary forward rotation.
        let mut member = SenderKeysGroup::new();
        member.install(CONV, 4, [1u8; GROUP_KEY_LEN]);
        assert!(crate::connection_manager::manager::accept_group_key_epoch(
            true,
            member.current_epoch(CONV),
            epoch
        ));
        member.install(CONV, epoch, key);

        let sealed = seal_chat_body(&keyer, CONV, SENDER, "mid-1", b"back again").unwrap();
        let opened = open_chat_body(&member, CONV, SENDER, "mid-1", sealed.0, &sealed.1);
        assert_eq!(opened.as_deref(), Some(&b"back again"[..]));
    }
}
