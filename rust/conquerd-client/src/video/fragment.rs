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
//! [ver:u8]                  0x03
//! [flags:u8]                bit0 keyframe · bit1 has_signature · bits2-7 reserved
//! [codec:u8]                VideoCodec wire byte
//! [pts:u64 BE]              microseconds since session start
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
//! sender. The binary header above is ~64 bytes before the signature (12-byte
//! fixed prefix including `pts_us`, ~44-byte sender, 8-byte seq/frag fields),
//! leaving ~1134 usable bytes — about 76% more goods per datagram — and the
//! relay forwards it opaquely.
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
/// `0x02` added the codec byte, `0x03` the presentation timestamp. There is no
/// compatibility path to either: video has never shipped, so there is no fleet
/// to interoperate with, and a parser accepting an older version would have to
/// invent the field it lacks — exactly the guess each bump exists to remove.
pub const FRAGMENT_VERSION: u8 = 0x03;

/// Bytes of fixed header before the variable-length sender id.
const FIXED_PREFIX_LEN: usize = 12; // ver + flags + codec + pts(8) + sender_len
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
/// so with a 1198-byte datagram and a 44-byte sender id each holds ~1070 bytes.
/// At 128 that caps a single frame at ~134 KB.
///
/// **Why not 64.** It was, and 64 is ~67 KB — which 1080p exceeds on every
/// keyframe. Measured against the Media Foundation encoder at the auto bitrate,
/// 1080p keyframes run 72–80 KB (75 fragments) where 720p runs ~35 KB (33).
/// A frame over the cap is not truncated or degraded: [`fragment_frame`]
/// returns `None` and the sender drops it whole. Dropping *keyframes*
/// specifically means receivers never get the one frame a decoder can start
/// from, so the stream produces no picture at all while inter frames keep
/// arriving — indistinguishable, from the outside, from a broken decoder.
///
/// Raising it is a compatibility change even though the header is untouched:
/// [`parse_fragment`] rejects a `frag_count` above this bound, so a receiver
/// still on 64 drops every 1080p keyframe a sender on 128 emits. Both ends must
/// ship together.
///
/// It is also the allocation guard: the count is validated *before* any
/// per-fragment `Vec` is sized, so a hostile `frag_count = 65535` cannot make
/// us reserve 65535 slots. The real memory bound is
/// [`MAX_PARTIAL_BYTES_PER_SENDER`], which is unchanged and still sheds the
/// oldest partial long before 128 × 8 in-flight fragments could accumulate.
pub const MAX_FRAGS_PER_FRAME: u16 = 128;

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

/// A backward jump this large is a sender that restarted, not a late fragment.
///
/// Sequence numbers start at zero in a fresh process, so a peer who restarts
/// their client re-uses numbers this receiver has long since passed. Judged
/// purely by [`MONOTONIC_LAG`] every one of those frames is "superseded", and
/// because a sender is never forgotten while it has completed-frame history,
/// the receiver drops that peer's video *permanently* — for the minutes or
/// hours it takes the counter to climb back past where it was. Leaving the room
/// does not clear it either; only restarting the receiving client does.
///
/// Nothing legitimate lands here. Fragments are reordered by tens of
/// milliseconds at worst and abandoned entirely after [`PARTIAL_TIMEOUT`], so
/// 256 frames — more than eight seconds at 30 fps — cannot be jitter.
const RESTART_BACKWARD_JUMP: u32 = 256;

/// Whether `hdr` is the start of a new stream from a sender we have history
/// for, rather than a fragment of a frame already superseded.
///
/// Two conditions, and the keyframe one is doing real work in both directions:
///
/// * **Correctness.** A decoder cannot resume from an inter frame — its
///   reference frames belong to the process that exited. Resyncing on anything
///   else would accept bytes only to fail decoding them. A restarting encoder
///   always emits a keyframe first, and every fragment of it carries the flag,
///   so the one we happen to receive first is enough.
/// * **Blast radius.** Reassembly runs *before* the per-frame signature is
///   checked — it has to, since the signature covers the reassembled frame — so
///   until then a sender id is merely claimed. Requiring the flag means a
///   stray fragment from an old session cannot reset a healthy stream's state
///   in passing. It is not an authentication check and is not load-bearing as
///   one: a room member could always forge this much. It keeps an accident from
///   costing a stream, and the cost of being wrong is bounded — a reset loses
///   at most the frames currently in flight, which the next keyframe replaces.
fn is_stream_restart(last_completed: u32, hdr: &FragmentHeader) -> bool {
    hdr.keyframe && last_completed.wrapping_sub(hdr.frame_seq) > RESTART_BACKWARD_JUMP
}

/// Parsed fragment header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentHeader {
    /// Whether the frame this fragment belongs to is a keyframe.
    pub keyframe: bool,
    /// Codec the frame's bytes are in.
    pub codec: VideoCodec,
    /// Capture time, microseconds since the sender's session start.
    pub pts_us: u64,
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
    /// Capture time, microseconds since the sender's session start. Comparable
    /// only against other media from the *same* sender.
    pub pts_us: u64,
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
// The parameters are the wire header fields, in wire order. Grouping them into
// a struct would not remove the need to supply every one; it would just move
// the same five values behind a name that each call site constructs inline,
// and put the serializer's arguments out of step with the format it writes.
#[allow(clippy::too_many_arguments)]
pub fn fragment_frame(
    sender: &str,
    frame_seq: u32,
    keyframe: bool,
    codec: VideoCodec,
    pts_us: u64,
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
        buf.extend_from_slice(&pts_us.to_be_bytes());
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
    let pts_us = u64::from_be_bytes(buf[3..11].try_into().ok()?);
    let sender_len = buf[11] as usize;
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
            pts_us,
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
    pts_us: u64,
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

    /// Forget every sender: partials and completed-frame history alike.
    ///
    /// Called when the session a stream belonged to ends (leaving a room,
    /// joining another). The history is what suppresses late fragments, and
    /// keeping it across sessions means judging a new stream against a counter
    /// from an old one — [`RESTART_BACKWARD_JUMP`] recovers from that, but not
    /// carrying the stale state in the first place is better.
    pub fn clear(&mut self) {
        self.senders.clear();
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

        // Ignore fragments for frames the decoder has already moved past —
        // unless the sender has plainly started over, in which case "behind" is
        // the wrong reading of the number and dropping it would wedge this peer
        // for good. See RESTART_BACKWARD_JUMP.
        if let Some(last) = state.last_completed {
            let cutoff = last.wrapping_sub(MONOTONIC_LAG);
            if seq_newer(cutoff, hdr.frame_seq) || hdr.frame_seq == last {
                if is_stream_restart(last, &hdr) {
                    // Everything buffered belongs to the previous stream, and
                    // its numbering no longer means anything here. Silent by
                    // design — this module keeps no logging of its own; the
                    // receiver's stall watchdog is what makes the outage that
                    // this prevents visible in a log.
                    state.partials.clear();
                    state.bytes = 0;
                    state.last_completed = None;
                } else {
                    return None;
                }
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
                pts_us: hdr.pts_us,
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

        // Likewise the timestamp: one frame was captured at one instant, so a
        // fragment claiming otherwise is not part of it. Splicing it in would
        // corrupt the bytes the signature covers.
        if entry.pts_us != hdr.pts_us {
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
        let pts_us = entry.pts_us;
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
            pts_us,
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
    const PTS: u64 = 1_234_567;
    /// Mirrors the real budget: 1200-byte datagram minus the relay's index and
    /// channel tag bytes.
    const MAX_PAYLOAD: usize = 1198;

    fn frags(sealed: &[u8], seq: u32, keyframe: bool) -> Vec<Vec<u8>> {
        fragment_frame(SENDER, seq, keyframe, CODEC, PTS, &SIG, sealed, MAX_PAYLOAD).unwrap()
    }

    /// Largest 1080p keyframe measured from the Media Foundation encoder at the
    /// auto bitrate (3.12 Mbps at 30 fps), plus the GCM seal's 13-byte header
    /// and 16-byte tag. The cap has to clear this or 1080p produces no picture
    /// at all — see [`MAX_FRAGS_PER_FRAME`].
    const MEASURED_1080P_KEYFRAME: usize = 79_627 + 29;

    /// The regression this guards: at 64 fragments every 1080p keyframe was
    /// dropped by the sender, so receivers never got a frame their decoder
    /// could start from and the tile sat on "Waiting for video…" while inter
    /// frames arrived normally.
    #[test]
    fn a_1080p_keyframe_fits_the_fragment_budget() {
        let sealed = vec![0xA5u8; MEASURED_1080P_KEYFRAME];
        let parts = fragment_frame(SENDER, 1, true, CODEC, PTS, &SIG, &sealed, MAX_PAYLOAD)
            .expect("a measured 1080p keyframe must fit");
        assert!(
            parts.len() <= MAX_FRAGS_PER_FRAME as usize,
            "{} fragments exceeds the cap of {MAX_FRAGS_PER_FRAME}",
            parts.len()
        );
        // And it must survive the round trip, not merely fragment: the receiver
        // enforces the same bound when parsing.
        let mut r = Reassembler::new();
        let now = Instant::now();
        let mut out = None;
        for part in &parts {
            out = out.or(r.push(part, now));
        }
        let frame = out.expect("a 1080p keyframe must reassemble");
        assert_eq!(frame.sealed.len(), MEASURED_1080P_KEYFRAME);
        assert!(frame.keyframe);
    }

    /// Headroom is deliberate but not unlimited: the cap is still what bounds
    /// reassembly memory, so a frame past it must be refused rather than
    /// silently truncated.
    #[test]
    fn a_frame_past_the_cap_is_still_refused() {
        let chunk =
            MAX_PAYLOAD - (FIXED_PREFIX_LEN + SENDER.len() + FIXED_SUFFIX_LEN) - SIGNATURE_LEN;
        let too_big = vec![0u8; chunk * (MAX_FRAGS_PER_FRAME as usize + 1)];
        assert!(
            fragment_frame(SENDER, 1, true, CODEC, PTS, &SIG, &too_big, MAX_PAYLOAD).is_none(),
            "a frame needing more than the cap must not be fragmented"
        );
    }

    /// The bug this guards: sequence numbers restart at zero in a fresh
    /// process, so a peer who restarts their client re-uses numbers this
    /// receiver has passed. Judged only by MONOTONIC_LAG every frame reads as
    /// superseded, and since a sender with completed-frame history is never
    /// evicted, their video is dropped for as long as it takes the counter to
    /// climb back — which leaving and rejoining the room does not fix, because
    /// the reassembler outlives the room session.
    #[test]
    fn a_sender_that_restarts_from_zero_is_accepted_again() {
        let mut r = Reassembler::new();
        let now = Instant::now();

        // A long-running stream.
        for seq in [5_000u32, 5_001, 5_002] {
            let f = frags(b"frame from the old process", seq, true);
            assert!(r.push(&f[0], now).is_some(), "seq {seq} should complete");
        }

        // Same peer, new process, counter back to zero.
        let f = frags(b"frame from the new process", 0, true);
        let out = r
            .push(&f[0], now)
            .expect("a restarted sender must not be ignored");
        assert_eq!(out.frame_seq, 0);

        // And the stream continues from the new numbering.
        let f = frags(b"and the next one", 1, false);
        assert!(r.push(&f[0], now).is_some());
    }

    /// Only a keyframe resyncs. An inter frame cannot start a decoder — its
    /// references died with the old process — so accepting one would take the
    /// bytes and fail to decode them, and it would let a stray fragment from a
    /// finished session reset a healthy stream in passing.
    #[test]
    fn a_restart_is_only_taken_from_a_keyframe() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let f = frags(b"established stream", 5_000, true);
        assert!(r.push(&f[0], now).is_some());

        // Inter frame at the restarted numbering: ignored.
        let f = frags(b"inter frame after restart", 1, false);
        assert!(
            r.push(&f[0], now).is_none(),
            "an inter frame must not resync the stream"
        );

        // The keyframe that follows does resync it.
        let f = frags(b"keyframe after restart", 2, true);
        assert!(r.push(&f[0], now).is_some());
    }

    /// Every fragment of a keyframe carries the flag, so a restart is picked up
    /// from whichever one arrives first — not only fragment 0.
    #[test]
    fn a_restart_is_detected_from_any_fragment_of_the_keyframe() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let f = frags(b"established stream", 9_000, true);
        assert!(r.push(&f[0], now).is_some());

        // A multi-fragment keyframe from the restarted sender, delivered from
        // its *second* fragment onward — fragment 0 arrives last.
        let restarted = frags(&vec![4u8; 4_000], 3, true);
        assert!(restarted.len() > 2, "need a multi-fragment frame");
        let mut completed = None;
        for part in restarted.iter().skip(1) {
            completed = completed.or(r.push(part, now));
        }
        assert!(completed.is_none(), "frame 0 is still missing");
        let out = r
            .push(&restarted[0], now)
            .expect("the restarted keyframe must complete");
        assert_eq!(out.frame_seq, 3);
    }

    /// The other half of the rule: ordinary lateness must still be suppressed,
    /// or a duplicated or reordered fragment would reset the stream and hand
    /// the decoder a frame it has already shown.
    #[test]
    fn an_ordinary_late_frame_is_still_dropped() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let f = frags(b"current", 1_000, true);
        assert!(r.push(&f[0], now).is_some());

        // Anything past the MONOTONIC_LAG tolerance but short of the restart
        // jump — including the boundary itself, which must read as late.
        // Keyframes, so it is the *distance* rule being tested here and not
        // the keyframe requirement standing in for it.
        for late in [997u32, 990, 1_000 - RESTART_BACKWARD_JUMP] {
            let f = frags(b"late", late, true);
            assert!(
                r.push(&f[0], now).is_none(),
                "seq {late} is late, not a restart"
            );
        }
    }

    /// Restart detection must not fire on the wrap itself: at the u32 boundary
    /// the next sequence number is genuinely newer, not a restart.
    #[test]
    fn wrapping_past_the_boundary_is_not_read_as_a_restart() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let f = frags(b"before the wrap", u32::MAX, true);
        assert!(r.push(&f[0], now).is_some());
        let f = frags(b"after the wrap", 0, false);
        let out = r.push(&f[0], now).expect("0 follows u32::MAX");
        assert_eq!(out.frame_seq, 0);
    }

    #[test]
    fn clear_forgets_every_sender() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let f = frags(&vec![9u8; 4_000], 1, true);
        // Push all but the last fragment so there is buffered state to lose.
        for part in &f[..f.len() - 1] {
            assert!(r.push(part, now).is_none());
        }
        assert!(r.buffered_bytes(SENDER) > 0);
        assert_eq!(r.tracked_senders(), 1);

        r.clear();
        assert_eq!(r.tracked_senders(), 0);
        assert_eq!(r.buffered_bytes(SENDER), 0);
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
    fn header_fields_sit_where_v3_says() {
        let f = frags(b"x", 1, true);
        assert_eq!(f[0][0], FRAGMENT_VERSION);
        assert_eq!(f[0][2], CODEC.as_wire());
        // pts occupies 3..11, big-endian.
        assert_eq!(
            u64::from_be_bytes(f[0][3..11].try_into().unwrap()),
            PTS,
            "pts must be big-endian at offset 3"
        );
        assert_eq!(f[0][11], SENDER.len() as u8);
    }

    #[test]
    fn pts_survives_a_multi_fragment_round_trip() {
        let sealed: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
        // A value with bytes set across the whole u64, so a truncated or
        // wrongly-ordered field would not compare equal by luck.
        let pts = 0x0123_4567_89AB_CDEFu64;
        let f = fragment_frame(SENDER, 9, false, CODEC, pts, &SIG, &sealed, MAX_PAYLOAD).unwrap();
        assert!(f.len() > 1);

        let mut r = Reassembler::new();
        let mut out = None;
        for part in &f {
            if let Some(frame) = r.push(part, Instant::now()) {
                out = Some(frame);
            }
        }
        assert_eq!(out.expect("reassembled").pts_us, pts);
    }

    /// One frame was captured at one instant, so fragments disagreeing about
    /// when cannot belong together — splicing them would corrupt the bytes the
    /// signature covers.
    #[test]
    fn fragments_disagreeing_on_pts_do_not_combine() {
        let sealed: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
        let a = fragment_frame(SENDER, 4, false, CODEC, 1_000, &SIG, &sealed, MAX_PAYLOAD).unwrap();
        let b = fragment_frame(SENDER, 4, false, CODEC, 2_000, &SIG, &sealed, MAX_PAYLOAD).unwrap();
        assert!(a.len() > 2, "need a multi-fragment frame");

        let mut r = Reassembler::new();
        let now = Instant::now();
        assert!(r.push(&a[0], now).is_none());
        for part in b.iter().skip(1) {
            assert!(
                r.push(part, now).is_none(),
                "mismatched pts must not splice"
            );
        }
        let mut out = None;
        for part in a.iter().skip(1) {
            if let Some(frame) = r.push(part, now) {
                out = Some(frame);
            }
        }
        let done = out.expect("matching fragments complete the frame");
        assert_eq!(done.pts_us, 1_000);
        assert_eq!(done.sealed, sealed);
    }

    #[test]
    fn codec_survives_a_multi_fragment_round_trip() {
        for codec in [VideoCodec::H264, VideoCodec::Vp8, VideoCodec::Stub] {
            let sealed: Vec<u8> = (0..5_000u32).map(|i| (i % 251) as u8).collect();
            let f =
                fragment_frame(SENDER, 7, false, codec, PTS, &SIG, &sealed, MAX_PAYLOAD).unwrap();
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
            PTS,
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
            PTS,
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
            let f = fragment_frame(&who, 1, false, CODEC, PTS, &SIG, &sealed, MAX_PAYLOAD).unwrap();
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
        assert!(fragment_frame(SENDER, 1, true, CODEC, PTS, &SIG, &huge, MAX_PAYLOAD).is_none());
    }

    #[test]
    fn refuses_payload_budget_that_leaves_no_room() {
        assert!(fragment_frame(SENDER, 1, true, CODEC, PTS, &SIG, b"x", 10).is_none());
    }

    #[test]
    fn two_senders_are_tracked_independently() {
        let mut r = Reassembler::new();
        let now = Instant::now();
        let sealed: Vec<u8> = (0..6_000u32).map(|i| (i % 249) as u8).collect();

        let a = fragment_frame("alice", 1, true, CODEC, PTS, &SIG, &sealed, MAX_PAYLOAD).unwrap();
        let b = fragment_frame("bob", 1, true, CODEC, PTS, &SIG, &sealed, MAX_PAYLOAD).unwrap();

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
