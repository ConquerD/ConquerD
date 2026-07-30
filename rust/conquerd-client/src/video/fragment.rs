//! Video frame fragmentation and reassembly over unreliable datagrams.
//!
//! Audio never needed this: one 20 ms Opus frame always fits inside a single
//! 1200-byte datagram, so `send_audio_datagram` can put a whole frame on the
//! wire untouched. A video frame does not — an inter frame runs a few KB and a
//! keyframe tens of KB, in every codec we support — and the relay silently
//! *drops* any datagram over `MAX_DATAGRAM_SIZE`. So the sender splits each
//! encoded frame into fragments and the receiver puts them back together.
//!
//! # Wire format
//!
//! This module owns everything *after* the channel tag; the transport prepends
//! [`VIDEO_TAG`](conquerd_features::channel_frame::VIDEO_TAG) or
//! [`ROOM_VIDEO_TAG`](conquerd_features::channel_frame::ROOM_VIDEO_TAG) exactly
//! as `encode_frame` does for the other channels.
//!
//! ```text
//! [ver:u8]                  0x02
//! [flags:u8]                bit0 keyframe · bit1 has_signature · bits2-7 reserved
//! [codec:u8]                VideoCodec wire byte
//! [sender_len:u8][sender]   base64url peer id (43 or 44 bytes in practice)
//! [frame_seq:u32 BE]
//! [frag_idx:u16 BE]
//! [frag_count:u16 BE]
//! [sig:64]                  Ed25519, present only on frag_idx == 0
//! [chunk bytes]
//! ```
//!
//! # Why the codec is on every frame
//!
//! Peers negotiate a codec from their advertised sets (see
//! [`conquerd_features::video_codec`]), but negotiation alone is not enough to
//! decode by. A room sender fans out to every member at once and there may be
//! no single codec all of them run, so "what did we agree on" is not a
//! well-defined question room-side — only "what is *this* frame" is. Carrying
//! the codec makes the receiver's decoder choice a fact rather than an
//! inference, and lets a member that cannot decode this codec drop the frame
//! and say so instead of feeding a decoder bytes it will choke on.
//!
//! The byte is bound into the frame signature (see
//! [`video_frame_signing_bytes`](super::video_frame_signing_bytes)), so a relay
//! flipping it fails verification rather than silently redirecting a frame to
//! the wrong decoder.
//!
//! # Why not the signed-JSON envelope room audio uses
//!
//! Room audio wraps every frame in a base64'd, Ed25519-signed `SfuAudio` JSON
//! message. That costs ~300 bytes of envelope plus a 1.33x base64 expansion,
//! leaving roughly 644 usable bytes per datagram, and forces the supernode to
//! run a full `serde_json` parse per datagram. At 30 fps with ~4 fragments per
//! frame that is over 120 parses and signature verifications per second per
//! sender. The binary header above is 56 bytes, leaving ~1142 usable bytes —
//! about 77% more goods per datagram — and the relay forwards it opaquely.
//!
//! # Authenticity
//!
//! Dropping the JSON envelope drops its per-message signature, and the group
//! key is *shared* (see [`crate::group_key`]), so GCM alone cannot tell one
//! room member from another: any member could seal a frame claiming to be a
//! different sender. Authenticity is preserved by signing **once per frame**
//! rather than once per datagram, with the signature riding fragment 0. The
//! receiver verifies after reassembly and before opening the GCM seal.
//!
//! This module is deliberately pure — no I/O, no clock of its own (callers
//! pass `now`) — because it is the piece most exposed to hostile input.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use conquerd_features::video_codec::VideoCodec;

/// Wire format version. Bump only for an incompatible header change.
///
/// `0x02` added the codec byte. There is no `0x01` compatibility path: video
/// has never shipped, so there is no fleet to interoperate with, and a parser
/// that accepted both would have to guess the codec of a v1 frame — exactly the
/// guess this version exists to remove.
pub const FRAGMENT_VERSION: u8 = 0x02;

/// Bytes of fixed header before the variable-length sender id.
const FIXED_PREFIX_LEN: usize = 4; // ver + flags + codec + sender_len
/// Bytes of fixed header after the sender id, excluding the signature.
const FIXED_SUFFIX_LEN: usize = 8; // frame_seq(4) + frag_idx(2) + frag_count(2)
/// Length of the Ed25519 signature carried on fragment 0.
pub const SIGNATURE_LEN: usize = 64;
/// Upper bound on the sender id we will parse. Real ids are 43-44 bytes of
/// base64url; the cap exists so a hostile length byte can't make us index far
/// past the header.
const MAX_SENDER_LEN: usize = 64;

const FLAG_KEYFRAME: u8 = 0b0000_0001;
const FLAG_HAS_SIGNATURE: u8 = 0b0000_0010;

/// Maximum fragments one frame may claim.
///
/// Chunks are sized to fragment 0's budget (which also carries the signature),
/// so with a 1198-byte datagram and a 44-byte sender id each holds ~1078
/// bytes, capping a single frame at ~69 KB. That is ample for the CBR
/// bitrates this pipeline targets — a 640x360 keyframe at 600 kbps lands
/// around 10-20 KB — though a high-quality 720p keyframe could brush it. If
/// that ever happens the encoder should lower quality rather than this cap
/// rise, since the cap is what bounds reassembly memory.
///
/// It is also the allocation guard: the count is validated *before* any
/// per-fragment `Vec` is sized, so a hostile `frag_count = 65535` cannot make
/// us reserve 65535 slots.
pub const MAX_FRAGS_PER_FRAME: u16 = 64;

/// Maximum bytes of partially-reassembled frames held for one sender.
pub const MAX_PARTIAL_BYTES_PER_SENDER: usize = 512 * 1024;

/// Maximum senders tracked at once.
pub const MAX_SENDERS: usize = 32;

/// Maximum simultaneously in-flight partial frames per sender.
pub const MAX_IN_FLIGHT_PER_SENDER: usize = 8;

/// How long an incomplete frame is held before being abandoned.
///
/// A 30 fps frame interval is 33 ms, so 200 ms tolerates roughly six frames of
/// jitter while never carrying stale fragments into the next group of pictures.
pub const PARTIAL_TIMEOUT: Duration = Duration::from_millis(200);

/// How far behind the newest completed frame a fragment may be before it is
/// treated as dead weight. Late fragments for a superseded frame cannot be
/// rendered — the decoder has already moved on.
const MONOTONIC_LAG: u32 = 2;

/// Parsed fragment header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentHeader {
    /// Whether the frame this fragment belongs to is a keyframe.
    pub keyframe: bool,
    /// Codec the frame's bytes are in.
    pub codec: VideoCodec,
    /// Base64url peer id of the sender, as carried on the wire.
    pub sender: String,
    /// Sender-assigned frame counter.
    pub frame_seq: u32,
    /// Index of this fragment within the frame.
    pub frag_idx: u16,
    /// Total fragments in the frame.
    pub frag_count: u16,
    /// Frame signature; present only on `frag_idx == 0`.
    pub signature: Option<[u8; SIGNATURE_LEN]>,
}

/// A frame recovered from a complete set of fragments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReassembledFrame {
    /// Sender's base64url peer id.
    pub sender: String,
    /// Sender-assigned frame counter.
    pub frame_seq: u32,
    /// Whether this is a keyframe.
    pub keyframe: bool,
    /// Codec the sealed bytes decode as, once opened.
    pub codec: VideoCodec,
    /// Ed25519 signature over the frame, from fragment 0. The caller must
    /// verify this **before** opening `sealed`.
    pub signature: [u8; SIGNATURE_LEN],
    /// The sealed frame bytes: `[epoch:u8][nonce:12][aesgcm(encoded frame)]`,
    /// where the inner frame is in [`Self::codec`].
    pub sealed: Vec<u8>,
}

/// Serialise `sealed` into fragments, each at most `max_payload` bytes total.
///
/// `max_payload` is the datagram budget the transport can carry *after* its own
/// tag byte(s). Returns `None` if the frame cannot be expressed within
/// [`MAX_FRAGS_PER_FRAME`], if `max_payload` leaves no room for a chunk, or if
/// `sender` is unusably long — all sender-side programming errors rather than
/// runtime conditions.
pub fn fragment_frame(
    sender: &str,
    frame_seq: u32,
    keyframe: bool,
    codec: VideoCodec,
    signature: &[u8; SIGNATURE_LEN],
    sealed: &[u8],
    max_payload: usize,
) -> Option<Vec<Vec<u8>>> {
    if sender.is_empty() || sender.len() > MAX_SENDER_LEN {
        return None;
    }
    let base = FIXED_PREFIX_LEN + sender.len() + FIXED_SUFFIX_LEN;
    // Fragment 0 also carries the signature, so it has the least room. Sizing
    // every chunk to fragment 0's budget keeps the split uniform and means a
    // reordered arrival never needs a different size.
    let chunk_cap = max_payload.checked_sub(base + SIGNATURE_LEN)?;
    if chunk_cap == 0 {
        return None;
    }

    // An empty frame still produces one (empty) fragment so the receiver sees
    // the sequence rather than silently skipping it.
    let count = sealed.len().div_ceil(chunk_cap).max(1);
    if count > MAX_FRAGS_PER_FRAME as usize {
        return None;
    }

    let mut out = Vec::with_capacity(count);
    for idx in 0..count {
        // Indexed rather than `chunks()` so an empty frame still yields one
        // empty fragment (`0..0`) instead of no fragments at all.
        let start = idx * chunk_cap;
        let chunk = &sealed[start..(start + chunk_cap).min(sealed.len())];
        let is_first = idx == 0;
        let mut flags = 0u8;
        if keyframe {
            flags |= FLAG_KEYFRAME;
        }
        if is_first {
            flags |= FLAG_HAS_SIGNATURE;
        }

        let mut buf =
            Vec::with_capacity(base + if is_first { SIGNATURE_LEN } else { 0 } + chunk.len());
        buf.push(FRAGMENT_VERSION);
        buf.push(flags);
        buf.push(codec.as_wire());
        buf.push(sender.len() as u8);
        buf.extend_from_slice(sender.as_bytes());
        buf.extend_from_slice(&frame_seq.to_be_bytes());
        buf.extend_from_slice(&(idx as u16).to_be_bytes());
        buf.extend_from_slice(&(count as u16).to_be_bytes());
        if is_first {
            buf.extend_from_slice(signature);
        }
        buf.extend_from_slice(chunk);
        out.push(buf);
    }
    Some(out)
}

/// Parse one fragment into its header and chunk bytes.
///
/// Returns `None` for anything malformed. Every length is validated before it
/// is used to index or allocate.
pub fn parse_fragment(buf: &[u8]) -> Option<(FragmentHeader, &[u8])> {
    if buf.len() < FIXED_PREFIX_LEN {
        return None;
    }
    if buf[0] != FRAGMENT_VERSION {
        return None;
    }
    let flags = buf[1];
    // An unknown codec is rejected here rather than carried up as "unknown":
    // nothing downstream can decode it, and letting it through would put an
    // undecodable frame through reassembly and signature verification first.
    let codec = VideoCodec::from_wire(buf[2])?;
    let sender_len = buf[3] as usize;
    if sender_len == 0 || sender_len > MAX_SENDER_LEN {
        return None;
    }

    let sender_end = FIXED_PREFIX_LEN + sender_len;
    let suffix_end = sender_end + FIXED_SUFFIX_LEN;
    if buf.len() < suffix_end {
        return None;
    }
    let sender = std::str::from_utf8(&buf[FIXED_PREFIX_LEN..sender_end]).ok()?;

    let frame_seq = u32::from_be_bytes(buf[sender_end..sender_end + 4].try_into().ok()?);
    let frag_idx = u16::from_be_bytes(buf[sender_end + 4..sender_end + 6].try_into().ok()?);
    let frag_count = u16::from_be_bytes(buf[sender_end + 6..suffix_end].try_into().ok()?);

    // Validate the count *before* it is ever used to size an allocation.
    if frag_count == 0 || frag_count > MAX_FRAGS_PER_FRAME {
        return None;
    }
    if frag_idx >= frag_count {
        return None;
    }

    // The signature rides fragment 0 and only fragment 0. Enforcing both
    // directions stops a peer omitting it (unauthenticated frame) or attaching
    // it to every fragment (wasted bytes and verification work).
    let has_sig = flags & FLAG_HAS_SIGNATURE != 0;
    if has_sig != (frag_idx == 0) {
        return None;
    }

    let (signature, chunk_start) = if has_sig {
        let sig_end = suffix_end + SIGNATURE_LEN;
        if buf.len() < sig_end {
            return None;
        }
        let mut sig = [0u8; SIGNATURE_LEN];
        sig.copy_from_slice(&buf[suffix_end..sig_end]);
        (Some(sig), sig_end)
    } else {
        (None, suffix_end)
    };

    Some((
        FragmentHeader {
            keyframe: flags & FLAG_KEYFRAME != 0,
            codec,
            sender: sender.to_string(),
            frame_seq,
            frag_idx,
            frag_count,
            signature,
        },
        &buf[chunk_start..],
    ))
}

/// True when `a` is newer than `b` under serial-number arithmetic, so the
/// comparison stays correct across the `u32` wrap. At 30 fps the counter wraps
/// after roughly four and a half years of continuous streaming, but a session
/// that resumes near the boundary must not stall for 2^31 frames.
fn seq_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

struct PartialFrame {
    keyframe: bool,
    codec: VideoCodec,
    signature: Option<[u8; SIGNATURE_LEN]>,
    chunks: Vec<Option<Vec<u8>>>,
    received: u16,
    bytes: usize,
    first_seen: Instant,
}

#[derive(Default)]
struct SenderState {
    partials: HashMap<u32, PartialFrame>,
    bytes: usize,
    /// Highest frame sequence completed so far, for monotonic eviction.
    last_completed: Option<u32>,
}

impl SenderState {
    fn drop_partial(&mut self, seq: u32) {
        if let Some(p) = self.partials.remove(&seq) {
            self.bytes = self.bytes.saturating_sub(p.bytes);
        }
    }

    /// Remove the partial that has been waiting longest.
    fn evict_oldest(&mut self) {
        if let Some(&seq) = self
            .partials
            .iter()
            .min_by_key(|(_, p)| p.first_seen)
            .map(|(seq, _)| seq)
        {
            self.drop_partial(seq);
        }
    }
}

/// Reassembles fragments into whole frames, bounded in both time and memory.
///
/// Three independent eviction rules run together, because each alone leaves a
/// hole: time alone lets a fast sender pile up partials inside the window,
/// count alone lets a slow trickle live forever, and monotonic alone does
/// nothing for a sender that never completes a frame.
#[derive(Default)]
pub struct Reassembler {
    senders: HashMap<String, SenderState>,
}

impl Reassembler {
    /// Create an empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of senders currently tracked. Exposed for metrics and tests.
    pub fn tracked_senders(&self) -> usize {
        self.senders.len()
    }

    /// Bytes currently held in partial frames for `sender`.
    pub fn buffered_bytes(&self, sender: &str) -> usize {
        self.senders.get(sender).map_or(0, |s| s.bytes)
    }

    /// Drop every partial older than [`PARTIAL_TIMEOUT`], and forget senders
    /// that are left with nothing buffered.
    pub fn evict_expired(&mut self, now: Instant) {
        self.senders.retain(|_, s| {
            let expired: Vec<u32> = s
                .partials
                .iter()
                .filter(|(_, p)| now.duration_since(p.first_seen) >= PARTIAL_TIMEOUT)
                .map(|(seq, _)| *seq)
                .collect();
            for seq in expired {
                s.drop_partial(seq);
            }
            // A sender that has completed frames is kept so `last_completed`
            // still suppresses late fragments; one with no history at all is
            // dropped so the sender table doesn't grow without bound.
            !s.partials.is_empty() || s.last_completed.is_some()
        });
    }

    /// Feed one fragment in. Returns the frame once its last piece arrives.
    ///
    /// Malformed input, fragments for superseded frames, and anything that
    /// would breach a cap are dropped silently — this is an unreliable
    /// datagram path, and a caller cannot act on most of these anyway.
    pub fn push(&mut self, buf: &[u8], now: Instant) -> Option<ReassembledFrame> {
        let (hdr, chunk) = parse_fragment(buf)?;
        self.evict_expired(now);

        if !self.senders.contains_key(&hdr.sender) && self.senders.len() >= MAX_SENDERS {
            return None;
        }
        let state = self.senders.entry(hdr.sender.clone()).or_default();

        // Ignore fragments for frames the decoder has already moved past.
        if let Some(last) = state.last_completed {
            let cutoff = last.wrapping_sub(MONOTONIC_LAG);
            if seq_newer(cutoff, hdr.frame_seq) || hdr.frame_seq == last {
                return None;
            }
        }

        // Make room before inserting a brand-new partial.
        if !state.partials.contains_key(&hdr.frame_seq)
            && state.partials.len() >= MAX_IN_FLIGHT_PER_SENDER
        {
            state.evict_oldest();
        }

        let entry = state
            .partials
            .entry(hdr.frame_seq)
            .or_insert_with(|| PartialFrame {
                keyframe: hdr.keyframe,
                codec: hdr.codec,
                signature: None,
                // Safe: `parse_fragment` already bounded frag_count by
                // MAX_FRAGS_PER_FRAME, so this allocates at most 64 slots.
                chunks: vec![None; hdr.frag_count as usize],
                received: 0,
                bytes: 0,
                first_seen: now,
            });

        // A fragment claiming a different total than its siblings is either a
        // spoof or a desync; trust the first arrival and drop the odd one out.
        if entry.chunks.len() != hdr.frag_count as usize {
            return None;
        }

        // Likewise for the codec: one frame is in one codec, so a fragment
        // disagreeing with its siblings cannot be part of this frame. Splicing
        // it in would corrupt the frame the signature is computed over.
        if entry.codec != hdr.codec {
            return None;
        }

        let idx = hdr.frag_idx as usize;
        // Duplicate fragment — keep the first copy so counters stay honest.
        if entry.chunks[idx].is_some() {
            return None;
        }

        if state.bytes + chunk.len() > MAX_PARTIAL_BYTES_PER_SENDER {
            // Shed the oldest work rather than the fragment we just received;
            // the newest frame is the one with a chance of being rendered.
            while state.bytes + chunk.len() > MAX_PARTIAL_BYTES_PER_SENDER
                && state.partials.len() > 1
            {
                state.evict_oldest();
            }
            if state.bytes + chunk.len() > MAX_PARTIAL_BYTES_PER_SENDER {
                state.drop_partial(hdr.frame_seq);
                return None;
            }
        }

        // Re-borrow: `evict_oldest` above needed `&mut state`, and it may have
        // removed this very partial if it was the only one buffered.
        let entry = state.partials.get_mut(&hdr.frame_seq)?;
        if let Some(sig) = hdr.signature {
            entry.signature = Some(sig);
        }
        entry.bytes += chunk.len();
        entry.chunks[idx] = Some(chunk.to_vec());
        entry.received += 1;
        state.bytes += chunk.len();

        if entry.received < hdr.frag_count {
            return None;
        }

        // Complete. A frame whose fragment 0 never arrived has no signature
        // and cannot be authenticated, so it is discarded rather than passed
        // up unverified.
        let signature = entry.signature?;
        let keyframe = entry.keyframe;
        let codec = entry.codec;
        let mut sealed = Vec::with_capacity(entry.bytes);
        for slot in entry.chunks.iter() {
            sealed.extend_from_slice(slot.as_deref()?);
        }

        state.drop_partial(hdr.frame_seq);
        let newer = state
            .last_completed
            .is_none_or(|l| seq_newer(hdr.frame_seq, l));
        if newer {
            state.last_completed = Some(hdr.frame_seq);
            let cutoff = hdr.frame_seq.wrapping_sub(MONOTONIC_LAG);
            let stale: Vec<u32> = state
                .partials
                .keys()
                .copied()
                .filter(|&seq| seq_newer(cutoff, seq))
                .collect();
            for seq in stale {
                state.drop_partial(seq);
            }
        }

        Some(ReassembledFrame {
            sender: hdr.sender,
            frame_seq: hdr.frame_seq,
            keyframe,
            codec,
            signature,
            sealed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SENDER: &str = "alice-public-id-base64url-xxxxxxxxxxxxxxxxxxx";
    const SIG: [u8; SIGNATURE_LEN] = [7u8; SIGNATURE_LEN];
    const CODEC: VideoCodec = VideoCodec::H264;
    /// Mirrors the real budget: 1200-byte datagram minus the relay's index and
    /// channel tag bytes.
    const MAX_PAYLOAD: usize = 1198;

    fn frags(sealed: &[u8], seq: u32, keyframe: bool) -> Vec<Vec<u8>> {
        fragment_frame(SENDER, seq, keyframe, CODEC, &SIG, sealed, MAX_PAYLOAD).unwrap()
    }

    #[test]
    fn single_fragment_round_trips() {
        let sealed = b"a small sealed frame";
        let f = frags(sealed, 1, true);
        assert_eq!(f.len(), 1);

        let mut r = Reassembler::new();
        let out = r.push(&f[0], Instant::now()).unwrap();
        assert_eq!(out.sender, SENDER);
        assert_eq!(out.frame_seq, 1);
        assert!(out.keyframe);
        assert_eq!(out.signature, SIG);
        assert_eq!(out.sealed, sealed);
        assert_eq!(out.codec, CODEC);
    }

    // ── Codec on the wire ───────────────────────────────────────────────────

    /// GOLDEN: the header layout. A silent shift here desynchronises every
    /// field after it, and the symptom would be undecodable video rather than
    /// a parse error.
    #[test]
    fn codec_byte_sits_at_offset_two() {
        let f = frags(b"x", 1, true);
        assert_eq!(f[0][0], FRAGMENT_VERSION);
        assert_eq!(f[0][2], CODEC.as_wire());
        assert_eq!(f[0][3], SENDER.len() as u8);
    }

    #[test]
    fn codec_survives_a_multi_fragment_round_trip() {
        for codec in [VideoCodec::H264, VideoCodec::Vp8, VideoCodec::Stub] {
            let sealed: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
            let f = fragment_frame(SENDER, 7, false, codec, &SIG, &sealed, MAX_PAYLOAD).unwrap();
            assert!(f.len() > 1);

            let mut r = Reassembler::new();
            let mut out = None;
            for part in &f {
                if let Some(frame) = r.push(part, Instant::now()) {
                    out = Some(frame);
                }
            }
            assert_eq!(out.expect("reassembled").codec, codec);
        }
    }

    /// A codec byte this build does not know must be refused outright. Parsing
    /// it as "some codec" would carry an undecodable frame through reassembly
    /// and signature verification before anything noticed.
    #[test]
    fn an_unknown_codec_byte_is_rejected() {
        let mut f = frags(b"payload", 1, true).remove(0);
        f[2] = 0x7E; // not a VideoCodec
        assert!(parse_fragment(&f).is_none());

        let mut r = Reassembler::new();
        assert!(r.push(&f, Instant::now()).is_none());
    }

    /// Fragments of one frame that disagree about the codec cannot all belong
    /// to that frame. Splicing them would corrupt the bytes the signature is
    /// computed over.
    #[test]
    fn fragments_disagreeing_on_codec_do_not_combine() {
        let sealed: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
        let a = fragment_frame(
            SENDER,
            3,
            false,
            VideoCodec::H264,
            &SIG,
            &sealed,
            MAX_PAYLOAD,
        )
        .unwrap();
        let b = fragment_frame(
            SENDER,
            3,
            false,
            VideoCodec::Vp8,
            &SIG,
            &sealed,
            MAX_PAYLOAD,
        )
        .unwrap();
        assert!(a.len() > 2, "need a multi-fragment frame for this test");

        let mut r = Reassembler::new();
        let now = Instant::now();
        // First fragment establishes H.264 for frame 3.
        assert!(r.push(&a[0], now).is_none());
        // The VP8-tagged siblings must not be accepted into it...
        for part in b.iter().skip(1) {
            assert!(r.push(part, now).is_none());
        }
        // ...so the frame is still incomplete, and only the matching
        // fragments finish it.
        let mut out = None;
        for part in a.iter().skip(1) {
            if let Some(frame) = r.push(part, now) {
                out = Some(frame);
            }
        }
        let done = out.expect("matching fragments complete the frame");
        assert_eq!(done.codec, VideoCodec::H264);
        assert_eq!(done.sealed, sealed);
    }

    /// The header grew by one byte, so the usable chunk budget shrank by one.
    /// Stated as a test because getting this wrong produces datagrams one byte
    /// over the relay ceiling, which the relay drops silently.
    #[test]
    fn fragments_never_exceed_the_payload_budget() {
        let sealed: Vec<u8> = (0..20_000u32).map(|i| (i % 251) as u8).collect();
        for part in frags(&sealed, 1, true) {
            assert!(
                part.len() <= MAX_PAYLOAD,
                "fragment of {} bytes exceeds the {MAX_PAYLOAD}-byte budget",
                part.len()
            );
        }
    }

    #[test]
    fn multi_fragment_round_trips() {
        let sealed: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let f = frags(&sealed, 2, false);
        assert!(f.len() > 1, "10 KB must span several fragments");

        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut done = None;
        for part in &f {
            if let Some(frame) = r.push(part, now) {
                done = Some(frame);
            }
        }
        let out = done.expect("last fragment completes the frame");
        assert_eq!(out.sealed, sealed);
        assert!(!out.keyframe);
        assert_eq!(r.buffered_bytes(SENDER), 0, "completed frame must be freed");
    }

    #[test]
    fn fragments_fit_the_datagram_budget() {
        let sealed = vec![0xABu8; 40_000];
        for part in frags(&sealed, 3, true) {
            assert!(
                part.len() <= MAX_PAYLOAD,
                "fragment of {} bytes exceeds the {MAX_PAYLOAD}-byte budget",
                part.len()
            );
        }
    }

    #[test]
    fn shuffled_arrival_reassembles() {
        let sealed: Vec<u8> = (0..8_000u32).map(|i| (i % 253) as u8).collect();
        let mut f = frags(&sealed, 4, false);
        f.reverse();

        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut done = None;
        for part in &f {
            if let Some(frame) = r.push(part, now) {
                done = Some(frame);
            }
        }
        assert_eq!(done.unwrap().sealed, sealed);
    }

    #[test]
    fn duplicate_fragment_is_ignored() {
        let sealed: Vec<u8> = vec![1u8; 5_000];
        let f = frags(&sealed, 5, false);
        let mut r = Reassembler::new();
        let now = Instant::now();

        assert!(r.push(&f[0], now).is_none());
        let before = r.buffered_bytes(SENDER);
        assert!(r.push(&f[0], now).is_none(), "duplicate must not complete");
        assert_eq!(
            r.buffered_bytes(SENDER),
            before,
            "duplicate must not double-count"
        );

        let mut done = None;
        for part in &f[1..] {
            if let Some(frame) = r.push(part, now) {
                done = Some(frame);
            }
        }
        assert_eq!(done.unwrap().sealed, sealed);
    }

    #[test]
    fn missing_fragment_times_out() {
        let sealed: Vec<u8> = vec![2u8; 5_000];
        let f = frags(&sealed, 6, false);
        let mut r = Reassembler::new();
        let start = Instant::now();

        for part in &f[..f.len() - 1] {
            assert!(r.push(part, start).is_none());
        }
        assert!(r.buffered_bytes(SENDER) > 0);

        r.evict_expired(start + PARTIAL_TIMEOUT);
        assert_eq!(r.buffered_bytes(SENDER), 0, "stale partial must be evicted");
    }

    #[test]
    fn frame_without_fragment_zero_is_discarded() {
        // Fragment 0 carries the signature; without it the frame cannot be
        // authenticated, so completing the rest must not yield a frame.
        let sealed: Vec<u8> = vec![3u8; 3_000];
        let f = frags(&sealed, 7, false);
        assert!(f.len() >= 2);

        let mut r = Reassembler::new();
        let now = Instant::now();
        for part in &f[1..] {
            assert!(r.push(part, now).is_none());
        }
    }

    // ── Hostile input ────────────────────────────────────────────────────────

    #[test]
    fn rejects_bad_version() {
        let mut f = frags(b"x", 1, true).remove(0);
        f[0] = 0xFF;
        assert!(parse_fragment(&f).is_none());
    }

    #[test]
    fn rejects_zero_frag_count() {
        let mut f = frags(b"x", 1, true).remove(0);
        let end = FIXED_PREFIX_LEN + SENDER.len() + FIXED_SUFFIX_LEN;
        f[end - 2..end].copy_from_slice(&0u16.to_be_bytes());
        assert!(parse_fragment(&f).is_none());
    }

    #[test]
    fn rejects_frag_idx_beyond_count() {
        let mut f = frags(b"x", 1, true).remove(0);
        let end = FIXED_PREFIX_LEN + SENDER.len() + FIXED_SUFFIX_LEN;
        // count = 1, so idx = 1 is out of range. Clear the signature flag too,
        // since a non-zero index must not claim one.
        f[1] &= !FLAG_HAS_SIGNATURE;
        f[end - 4..end - 2].copy_from_slice(&1u16.to_be_bytes());
        assert!(parse_fragment(&f).is_none());
    }

    #[test]
    fn rejects_absurd_frag_count_without_allocating() {
        // The guard that matters: u16::MAX slots must never be reserved.
        let mut f = frags(b"x", 1, true).remove(0);
        let end = FIXED_PREFIX_LEN + SENDER.len() + FIXED_SUFFIX_LEN;
        f[end - 2..end].copy_from_slice(&u16::MAX.to_be_bytes());
        assert!(parse_fragment(&f).is_none());

        let mut over = f.clone();
        over[end - 2..end].copy_from_slice(&(MAX_FRAGS_PER_FRAME + 1).to_be_bytes());
        assert!(parse_fragment(&over).is_none());
    }

    #[test]
    fn rejects_signature_flag_mismatch() {
        // Fragment 0 without the flag: unauthenticated.
        let mut f = frags(b"x", 1, true).remove(0);
        f[1] &= !FLAG_HAS_SIGNATURE;
        assert!(parse_fragment(&f).is_none());

        // A non-zero fragment claiming a signature.
        let sealed: Vec<u8> = vec![4u8; 5_000];
        let parts = frags(&sealed, 2, false);
        let mut later = parts[1].clone();
        later[1] |= FLAG_HAS_SIGNATURE;
        assert!(parse_fragment(&later).is_none());
    }

    #[test]
    fn rejects_truncated_and_empty_input() {
        assert!(parse_fragment(&[]).is_none());
        assert!(parse_fragment(&[FRAGMENT_VERSION]).is_none());
        let f = frags(b"payload", 1, true).remove(0);
        for cut in 1..f
            .len()
            .min(FIXED_PREFIX_LEN + SENDER.len() + FIXED_SUFFIX_LEN + SIGNATURE_LEN)
        {
            assert!(
                parse_fragment(&f[..cut]).is_none(),
                "truncation at {cut} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_bad_sender_length() {
        let mut f = frags(b"x", 1, true).remove(0);
        f[2] = 0; // zero-length sender
        assert!(parse_fragment(&f).is_none());
        f[2] = (MAX_SENDER_LEN + 1) as u8;
        assert!(parse_fragment(&f).is_none());
    }

    #[test]
    fn rejects_non_utf8_sender() {
        let mut f = frags(b"x", 1, true).remove(0);
        f[FIXED_PREFIX_LEN] = 0xFF;
        assert!(parse_fragment(&f).is_none());
    }

    // ── Caps and eviction ────────────────────────────────────────────────────

    #[test]
    fn sender_table_is_capped() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let sealed = vec![9u8; 4_000]; // multi-fragment, so partials persist

        for i in 0..MAX_SENDERS + 5 {
            let who = format!("sender-{i:03}");
            let f = fragment_frame(&who, 1, false, CODEC, &SIG, &sealed, MAX_PAYLOAD).unwrap();
            let _ = r.push(&f[0], now);
        }
        assert_eq!(r.tracked_senders(), MAX_SENDERS);
    }

    #[test]
    fn in_flight_partials_are_capped_per_sender() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let sealed = vec![8u8; 4_000];

        // Start more partial frames than the cap; never complete any.
        for seq in 0..(MAX_IN_FLIGHT_PER_SENDER as u32 + 4) {
            let f = frags(&sealed, seq, false);
            assert!(r.push(&f[0], now).is_none());
        }
        let s = &r.senders[SENDER];
        assert!(
            s.partials.len() <= MAX_IN_FLIGHT_PER_SENDER,
            "partials {} exceeded cap {MAX_IN_FLIGHT_PER_SENDER}",
            s.partials.len()
        );
    }

    #[test]
    fn per_sender_memory_is_capped() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        // Near the per-frame ceiling (64 fragments) without exceeding it, so
        // a handful of concurrent partials would blow the per-sender cap.
        let sealed = vec![6u8; 60_000];

        for seq in 0..12u32 {
            let f = frags(&sealed, seq, false);
            for part in &f[..f.len() - 1] {
                let _ = r.push(part, now);
            }
        }
        assert!(
            r.buffered_bytes(SENDER) <= MAX_PARTIAL_BYTES_PER_SENDER,
            "buffered {} exceeded cap {MAX_PARTIAL_BYTES_PER_SENDER}",
            r.buffered_bytes(SENDER)
        );
    }

    #[test]
    fn completing_a_frame_evicts_superseded_partials() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let big = vec![5u8; 5_000];

        // Leave frames 0 and 1 incomplete.
        for seq in 0..2u32 {
            let f = frags(&big, seq, false);
            assert!(r.push(&f[0], now).is_none());
        }
        assert!(r.buffered_bytes(SENDER) > 0);

        // Complete frame 10, which is far ahead of both.
        let f = frags(b"done", 10, true);
        assert!(r.push(&f[0], now).is_some());

        assert_eq!(
            r.buffered_bytes(SENDER),
            0,
            "partials older than seq-2 must be dropped once a newer frame lands"
        );
    }

    #[test]
    fn late_fragments_for_superseded_frames_are_dropped() {
        let mut r = Reassembler::new();
        let now = Instant::now();

        let f = frags(b"current", 20, true);
        assert!(r.push(&f[0], now).is_some());

        // A straggler from well before the completed frame.
        let old = frags(&vec![1u8; 5_000], 5, false);
        assert!(r.push(&old[0], now).is_none());
        assert_eq!(r.buffered_bytes(SENDER), 0, "must not buffer a dead frame");

        // A frame within the lag window is still accepted (reordering).
        let recent = frags(&vec![1u8; 5_000], 19, false);
        assert!(r.push(&recent[0], now).is_none());
        assert!(r.buffered_bytes(SENDER) > 0, "seq-1 is still in the window");
    }

    #[test]
    fn sequence_wraparound_is_handled() {
        assert!(seq_newer(0, u32::MAX));
        assert!(!seq_newer(u32::MAX, 0));
        assert!(seq_newer(1, u32::MAX - 1));

        let mut r = Reassembler::new();
        let now = Instant::now();

        let f = frags(b"pre-wrap", u32::MAX - 1, true);
        assert!(r.push(&f[0], now).is_some());
        // Wrapping past the boundary must still count as newer, not stall.
        let f = frags(b"post-wrap", 1, true);
        assert!(
            r.push(&f[0], now).is_some(),
            "a wrapped sequence must not be mistaken for an ancient frame"
        );
    }

    #[test]
    fn empty_frame_produces_one_fragment() {
        let f = frags(b"", 1, false);
        assert_eq!(f.len(), 1);
        let mut r = Reassembler::new();
        let out = r.push(&f[0], Instant::now()).unwrap();
        assert!(out.sealed.is_empty());
    }

    #[test]
    fn oversized_frame_is_refused() {
        // Beyond MAX_FRAGS_PER_FRAME fragments the sender must refuse rather
        // than emit a frame the receiver is guaranteed to reject.
        let huge = vec![0u8; MAX_FRAGS_PER_FRAME as usize * MAX_PAYLOAD];
        assert!(fragment_frame(SENDER, 1, true, CODEC, &SIG, &huge, MAX_PAYLOAD).is_none());
    }

    #[test]
    fn refuses_payload_budget_that_leaves_no_room() {
        assert!(fragment_frame(SENDER, 1, true, CODEC, &SIG, b"x", 10).is_none());
    }

    #[test]
    fn two_senders_are_tracked_independently() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let sealed: Vec<u8> = (0..6_000u32).map(|i| (i % 249) as u8).collect();

        let a = fragment_frame("alice", 1, true, CODEC, &SIG, &sealed, MAX_PAYLOAD).unwrap();
        let b = fragment_frame("bob", 1, true, CODEC, &SIG, &sealed, MAX_PAYLOAD).unwrap();

        // Interleave the two streams.
        let mut done = 0;
        for (pa, pb) in a.iter().zip(b.iter()) {
            if r.push(pa, now).is_some() {
                done += 1;
            }
            if r.push(pb, now).is_some() {
                done += 1;
            }
        }
        assert_eq!(done, 2, "both senders must complete independently");
    }
}
