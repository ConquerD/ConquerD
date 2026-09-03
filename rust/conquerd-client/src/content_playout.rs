//! Content-audio receive side: jitter buffer, decode, and the tick that
//! defines the audio timeline video is synchronised against.
//!
//! This is the master half of the audio-led design in [`crate::media_sync`].
//! Its steady 20 ms tick *is* the timeline: every frame it plays — real or
//! concealed — advances the per-peer anchor, and video is held or dropped
//! against that.
//!
//! # Why a jitter buffer at all
//!
//! Frames arrive over unreliable datagrams, so they come out of order, in
//! bursts, and with gaps. Playing them in arrival order would be audibly wrong,
//! and playing them the instant they arrive would inherit the network's jitter.
//! A shallow reorder window fixes both.
//!
//! Shallow on purpose: buffer depth is latency, and this stream is meant to be
//! *in sync with video*, so it cannot absorb much delay before the video it
//! belongs to has to wait too.
//!
//! # Concealment advances the timeline
//!
//! When a frame is missing the tick still fires and still advances the anchor
//! by one frame. That is what stops packet loss on the audio stream from
//! freezing video: the timeline keeps moving, so held frames keep being
//! released. Silence is substituted for the missing audio, which is honest —
//! Opus PLC could interpolate, but for arbitrary content (music, effects) an
//! invented 20 ms is as likely to be wrong as right.
//!
//! # No tick of its own
//!
//! This module is *driven*; it does not run a timer. The call controller ticks
//! it from the same 20 ms loop that plays voice, which is deliberate: two
//! independent 20 ms ticks would drift against each other, and the drift would
//! appear as A/V sync error that no amount of correct arithmetic here could
//! explain. One audio clock, one mixing point.

use std::collections::HashMap;

use crate::call_controller::SAMPLES_PER_FRAME;
use crate::content_sender::FRAME_DURATION_US;

/// Frames buffered per peer before the oldest is played out regardless.
///
/// Three frames is 60 ms of reordering tolerance — enough for ordinary network
/// jitter, and the ceiling on how far this stream can drag video behind it.
pub const JITTER_DEPTH: usize = 3;

/// Hard ceiling on frames held per peer, beyond which the oldest are shed.
///
/// [`JITTER_DEPTH`] is the *target* cushion, not a limit: the tick plays one
/// frame per 20 ms while the sender's capture device runs on its own clock, so
/// a sender running fast — or one delivering a burst after a stall — grows the
/// buffer faster than it drains. Left unbounded that is unbounded latency, and
/// because video is synchronised to this stream the picture is dragged back
/// with it. Shedding the oldest costs a skip; not shedding costs the sync.
///
/// Four times the target depth (240 ms) leaves room for ordinary bursts so the
/// bound only bites on genuine drift.
pub const MAX_BUFFERED_FRAMES: usize = JITTER_DEPTH * 4;

/// Gap in sequence numbers past which the buffer resynchronises rather than
/// waiting.
///
/// A sender that stopped and restarted, or a long outage, leaves a hole no
/// amount of waiting fills; continuing to expect the old sequence would stall
/// the stream indefinitely.
pub const RESYNC_GAP: u32 = 50;

/// Consecutive concealed frames after which the stream is declared over.
///
/// Concealment exists to carry the timeline across a *gap*, not to invent one.
/// Without a bound the tick concealed forever once a peer had ever sent audio,
/// which is not merely wasteful: every concealed frame re-anchors the A/V
/// timeline in [`crate::media_sync`], so a peer who stopped sharing left a
/// runaway clock climbing at real time on every receiver. When that peer next
/// shared *without* audio, their video restarted from zero on a fresh session
/// clock while the phantom timeline was minutes ahead — so every frame looked
/// hopelessly late and was dropped, and the tile sat on "Waiting for video…"
/// forever. Stopping here lets the anchor go stale
/// ([`crate::media_sync::ANCHOR_STALE_AFTER`]) so video free-runs as it should.
///
/// 20 frames is 400 ms: longer than any loss burst worth concealing, and it
/// matches the staleness window so the two halves of "the sender has stopped"
/// agree on how long that takes to conclude.
pub const MAX_CONCEALED_FRAMES: u32 = 20;

/// One received, not-yet-played content frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingFrame {
    pub seq: u32,
    pub pts_us: u64,
    /// Encoded Opus bytes.
    pub opus: Vec<u8>,
}

/// What the tick should do for one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickAction {
    /// Decode and play this frame.
    Play(PendingFrame),
    /// The expected frame is missing: play silence and advance anyway.
    Conceal { pts_us: u64 },
    /// Nothing buffered and no timeline yet — the peer is not sending.
    Idle,
}

/// Reorder buffer for one peer's content-audio stream.
#[derive(Debug)]
pub struct JitterBuffer {
    pending: Vec<PendingFrame>,
    /// Sequence the next tick expects. `None` until the first frame plays.
    next_seq: Option<u32>,
    /// Timestamp the next tick will report, advanced every tick including
    /// concealed ones.
    next_pts_us: u64,
    /// Concealed frames since the last real one, bounded by
    /// [`MAX_CONCEALED_FRAMES`].
    concealed_streak: u32,
    /// Filling the cushion: play nothing until [`JITTER_DEPTH`] frames are
    /// held. See [`Self::tick`] for why starting without one is not merely
    /// less smooth but self-sustaining.
    buffering: bool,
    /// Ticks spent waiting for the cushion, so a slow or ending stream is not
    /// held forever waiting for frames that are not coming.
    fill_ticks: usize,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            next_seq: None,
            next_pts_us: 0,
            concealed_streak: 0,
            // A fresh buffer has no cushion, so it starts by building one.
            buffering: true,
            fill_ticks: 0,
        }
    }
}

impl JitterBuffer {
    /// Create an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a frame. Duplicates and frames already played are discarded.
    pub fn push(&mut self, frame: PendingFrame) {
        // Checked unconditionally: a retransmit or a duplicated datagram is a
        // duplicate whether or not playout has started yet, and the
        // before-first-tick case is easy to miss because there is no sequence
        // to compare against.
        if self.pending.iter().any(|f| f.seq == frame.seq) {
            return;
        }
        if let Some(next) = self.next_seq {
            // Already played. Bounded by RESYNC_GAP so a wrapped or restarted
            // sequence is treated as new rather than as ancient.
            if frame.seq < next && next.wrapping_sub(frame.seq) < RESYNC_GAP {
                return;
            }
        }
        self.pending.push(frame);
        self.pending.sort_by_key(|f| f.seq);
        // A sender whose capture device runs faster than this receiver's 20 ms
        // playout tick delivers more than one frame per tick, and every frame
        // held is latency the video it belongs to must also wait out. Shedding
        // the oldest bounds both; the release rule below resynchronises on
        // whatever is left.
        while self.pending.len() > MAX_BUFFERED_FRAMES {
            self.pending.remove(0);
        }
    }

    /// Decide what to play now, advancing the timeline by one frame.
    ///
    /// Always advances while the stream is running, which is the property the
    /// video side depends on: a tick that returned nothing on loss would let
    /// the anchor go stale and video would jump. It stops advancing once the
    /// stream has plainly ended — see [`MAX_CONCEALED_FRAMES`].
    pub fn tick(&mut self) -> TickAction {
        // Build a cushion before playing anything, and rebuild it after an
        // underrun. Playing the instant a frame lands leaves no tolerance at
        // all, and the resulting failure is not a graceful one: the next frame
        // to arrive a millisecond after its tick is concealed, `push` then
        // discards it as already-played, and because the sender's cadence and
        // this tick keep the same phase every following frame meets the same
        // fate — the stream locks into concealing until MAX_CONCEALED_FRAMES
        // resets it, so roughly one frame in twenty reaches the speakers.
        if self.buffering {
            let stalled = self.fill_ticks >= JITTER_DEPTH && !self.pending.is_empty();
            if self.pending.len() < JITTER_DEPTH && !stalled {
                self.fill_ticks = self.fill_ticks.saturating_add(1);
                return match self.next_seq {
                    // A timeline already exists, so keep it moving across the
                    // refill or held video would stall. `next_seq` is *not*
                    // advanced: the frames still in flight are what the refill
                    // is waiting for, and advancing past them is precisely what
                    // would make `push` throw them away.
                    Some(_) => self.conceal(false),
                    None => TickAction::Idle,
                };
            }
            self.buffering = false;
            self.fill_ticks = 0;
            // Leaving the cushion always resumes from whatever is buffered,
            // even if the head is not the expected sequence. The refill already
            // spent those ticks concealing; stepping through the hole a frame
            // at a time would pay for the same gap twice and leave playout
            // permanently that much further behind.
            if let Some(first) = self.pending.first().cloned() {
                self.start_from(&first);
                return TickAction::Play(first);
            }
            return TickAction::Idle;
        }

        let Some(expected) = self.next_seq else {
            // No timeline and not buffering can only mean nothing has arrived.
            return TickAction::Idle;
        };

        // A long gap means the sender restarted or was away; resynchronise on
        // whatever is buffered rather than waiting for a frame that is gone.
        if let Some(first) = self.pending.first().cloned() {
            if first.seq.wrapping_sub(expected) >= RESYNC_GAP
                || self.pending.len() > JITTER_DEPTH
                || first.seq == expected
            {
                self.start_from(&first);
                return TickAction::Play(first);
            }
            // Later frames are here but the expected one is not: it was lost
            // rather than delayed, so conceal in its place and move on.
            return self.conceal(true);
        }

        // Underrun — nothing buffered at all. Rebuild the cushion rather than
        // limping on with none, and hold `next_seq` so frames still in flight
        // are accepted when they land.
        self.buffering = true;
        self.conceal(false)
    }

    /// Play silence for one frame, advancing the timeline.
    ///
    /// `advance_seq` distinguishes a frame known to be lost (advance past it)
    /// from one still expected to arrive (hold, so `push` does not discard it).
    fn conceal(&mut self, advance_seq: bool) -> TickAction {
        // Nothing has arrived for long enough that this is a stopped stream,
        // not a gap in a running one. Forget the timeline rather than keep
        // inventing it; a later frame starts a fresh one.
        if self.concealed_streak >= MAX_CONCEALED_FRAMES {
            self.reset();
            return TickAction::Idle;
        }
        if advance_seq {
            self.next_seq = self.next_seq.map(|s| s.wrapping_add(1));
        }
        let pts_us = self.next_pts_us;
        self.next_pts_us = pts_us.saturating_add(FRAME_DURATION_US);
        self.concealed_streak += 1;
        TickAction::Conceal { pts_us }
    }

    /// Adopt `frame` as the frame being played now, and continue from it.
    fn start_from(&mut self, frame: &PendingFrame) {
        self.pending.retain(|f| f.seq != frame.seq);
        self.next_seq = Some(frame.seq.wrapping_add(1));
        self.next_pts_us = frame.pts_us.saturating_add(FRAME_DURATION_US);
        // A real frame ends any run of concealment, however long.
        self.concealed_streak = 0;
    }

    /// Return to the not-started state, discarding the timeline.
    fn reset(&mut self) {
        *self = Self::new();
    }

    /// Whether this buffer holds no frames and no timeline — i.e. the peer is
    /// not sending and nothing is pending on their behalf.
    pub fn is_idle(&self) -> bool {
        self.next_seq.is_none() && self.pending.is_empty()
    }

    /// Frames currently buffered.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

/// Per-peer content-audio receive state.
#[derive(Default)]
pub struct ContentPlayout {
    buffers: HashMap<String, JitterBuffer>,
}

impl ContentPlayout {
    /// Create an empty playout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a verified, unsealed frame from `peer`.
    pub fn accept(&mut self, peer: &str, frame: PendingFrame) {
        self.buffers.entry(peer.to_owned()).or_default().push(frame);
    }

    /// Advance every peer by one frame, returning what each should play.
    ///
    /// Peers with nothing to play are omitted rather than reported as idle, so
    /// a caller can iterate the result directly. They are also dropped: a peer
    /// who has stopped sending is indistinguishable from one who never started,
    /// and keeping an inert buffer for everyone who ever shared audio in a
    /// long-lived room is an unbounded map with nothing in it.
    pub fn tick(&mut self) -> Vec<(String, TickAction)> {
        let mut out = Vec::new();
        self.buffers.retain(|peer, buf| match buf.tick() {
            TickAction::Idle => !buf.is_idle(),
            action => {
                out.push((peer.clone(), action));
                true
            }
        });
        out
    }

    /// Forget a peer — they left or stopped sharing.
    pub fn forget(&mut self, peer: &str) {
        self.buffers.remove(peer);
    }

    /// Forget everyone.
    pub fn clear(&mut self) {
        self.buffers.clear();
    }

    /// Peers currently tracked.
    pub fn tracked_peers(&self) -> usize {
        self.buffers.len()
    }
}

/// Silence for one frame, substituted when concealing.
pub fn silent_frame() -> Vec<i16> {
    vec![0i16; SAMPLES_PER_FRAME]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(seq: u32) -> PendingFrame {
        PendingFrame {
            seq,
            pts_us: seq as u64 * FRAME_DURATION_US,
            opus: vec![seq as u8],
        }
    }

    /// A buffer that has played `seq` and is expecting `seq + 1`.
    ///
    /// Priming is not incidental to these tests: the cushion is the fix for
    /// [`a_late_stream_is_not_locked_into_concealment`], so a test that starts
    /// by playing on the first pushed frame would be asserting the defect.
    fn started_at(seq: u32) -> JitterBuffer {
        let mut b = JitterBuffer::new();
        b.push(f(seq));
        for _ in 0..=JITTER_DEPTH {
            match b.tick() {
                TickAction::Play(fr) => {
                    assert_eq!(fr.seq, seq);
                    return b;
                }
                TickAction::Idle => {}
                other => panic!("unexpected {other:?} while priming"),
            }
        }
        panic!("the cushion never released");
    }

    #[test]
    fn an_empty_buffer_is_idle() {
        assert_eq!(JitterBuffer::new().tick(), TickAction::Idle);
    }

    /// The regression this module's cushion exists for. Every frame arrives one
    /// tick after the tick that wanted it — the ordinary case, since the
    /// sender's capture clock and this playout tick share no phase.
    ///
    /// Without a cushion the first frame played immediately, the second was
    /// concealed a millisecond before it landed, `push` then dropped it as
    /// already-played, and the phase never corrected: roughly one frame in
    /// twenty reached the speakers and the rest was silence. That is what
    /// "garbled" sounded like.
    #[test]
    fn a_late_stream_is_not_locked_into_concealment() {
        let mut b = JitterBuffer::new();
        let mut played = 0usize;
        let total = 200u32;

        for seq in 0..total {
            // Tick first, so the frame for this tick is always one tick late.
            if let TickAction::Play(_) = b.tick() {
                played += 1;
            }
            b.push(f(seq));
        }
        // Drain the cushion.
        for _ in 0..(JITTER_DEPTH + 2) {
            if let TickAction::Play(_) = b.tick() {
                played += 1;
            }
        }

        assert!(
            played as u32 >= total - JITTER_DEPTH as u32 - 2,
            "a uniformly late stream must still play: {played} of {total}"
        );
    }

    /// The cushion is built before anything plays. One frame in hand is no
    /// tolerance at all.
    #[test]
    fn playout_waits_for_the_cushion_before_starting() {
        let mut b = JitterBuffer::new();
        b.push(f(10));
        assert_eq!(b.tick(), TickAction::Idle, "one frame is not a cushion");
        b.push(f(11));
        assert_eq!(b.tick(), TickAction::Idle);
        b.push(f(12));
        assert_eq!(b.tick(), TickAction::Play(f(10)));
    }

    /// A sender that never fills the cushion — a slow or ending stream — must
    /// not be held silent waiting for frames that are not coming.
    #[test]
    fn a_short_stream_starts_anyway_rather_than_waiting_forever() {
        let mut b = JitterBuffer::new();
        b.push(f(0));
        let mut ticks = 0;
        loop {
            ticks += 1;
            match b.tick() {
                TickAction::Play(fr) => {
                    assert_eq!(fr.seq, 0);
                    break;
                }
                TickAction::Idle => assert!(ticks <= JITTER_DEPTH + 1, "waited too long"),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// An underrun rebuilds the cushion instead of limping on without one —
    /// and, critically, holds `next_seq` so the frames still in flight are not
    /// discarded as already-played the moment they land.
    #[test]
    fn an_underrun_rebuffers_without_discarding_frames_in_flight() {
        let mut b = started_at(0);
        // Nothing arrives for a while.
        for _ in 0..3 {
            assert!(matches!(b.tick(), TickAction::Conceal { .. }));
        }
        // The frames that were merely delayed now land.
        for seq in 1..=3 {
            b.push(f(seq));
        }
        assert_eq!(b.len(), 3, "delayed frames must not be treated as played");

        let played: Vec<u32> = (0..3)
            .filter_map(|_| match b.tick() {
                TickAction::Play(fr) => Some(fr.seq),
                _ => None,
            })
            .collect();
        assert_eq!(played, vec![1, 2, 3]);
    }

    /// Buffer depth is latency, and video is synchronised to this stream — so a
    /// sender whose clock outruns the tick must not be allowed to drag the
    /// picture back indefinitely.
    #[test]
    fn a_fast_sender_cannot_grow_the_buffer_without_bound() {
        let mut b = JitterBuffer::new();
        let mut seq = 0;
        for _ in 0..250 {
            // Two frames arrive per tick: the buffer can only drain one.
            b.push(f(seq));
            b.push(f(seq + 1));
            seq += 2;
            let _ = b.tick();
            assert!(b.len() <= MAX_BUFFERED_FRAMES, "buffer grew to {}", b.len());
        }
    }

    #[test]
    fn frames_play_in_sequence_order_not_arrival_order() {
        let mut b = JitterBuffer::new();
        b.push(f(0));
        // Arrive reversed.
        b.push(f(3));
        b.push(f(2));
        b.push(f(1));

        let played: Vec<u32> = (0..4)
            .filter_map(|_| match b.tick() {
                TickAction::Play(fr) => Some(fr.seq),
                _ => None,
            })
            .collect();
        assert_eq!(played, vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_duplicate_is_discarded() {
        let mut b = JitterBuffer::new();
        b.push(f(0));
        b.push(f(0));
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn a_frame_already_played_is_not_replayed() {
        let mut b = started_at(0);
        // A late duplicate of seq 0 must not be queued behind the timeline.
        b.push(f(0));
        assert!(b.is_empty());
    }

    /// The property video depends on: a missing frame must still advance the
    /// timeline. A tick that returned nothing would let the anchor go stale and
    /// video would free-run and jump.
    #[test]
    fn a_gap_conceals_and_still_advances() {
        let mut b = started_at(0);

        // seq 1 never arrives.
        let a = b.tick();
        match a {
            TickAction::Conceal { pts_us } => {
                assert_eq!(
                    pts_us, FRAME_DURATION_US,
                    "concealed pts must continue the timeline"
                );
            }
            other => panic!("expected concealment, got {other:?}"),
        }
    }

    #[test]
    fn concealed_timestamps_stay_evenly_spaced() {
        let mut b = started_at(0);

        let mut stamps = Vec::new();
        for _ in 0..5 {
            if let TickAction::Conceal { pts_us } = b.tick() {
                stamps.push(pts_us);
            }
        }
        assert_eq!(stamps.len(), 5);
        for pair in stamps.windows(2) {
            assert_eq!(pair[1] - pair[0], FRAME_DURATION_US);
        }
    }

    /// A late frame must not be held forever behind a gap: past the buffer
    /// depth the stream plays on rather than waiting.
    #[test]
    fn the_buffer_does_not_stall_past_its_depth() {
        let mut b = started_at(0); // played 0, now expecting 1

        // 1 is lost; 2..=5 arrive.
        for seq in 2..=5 {
            b.push(f(seq));
        }
        // Past JITTER_DEPTH the buffer must release rather than conceal
        // indefinitely.
        let action = b.tick();
        assert!(
            matches!(action, TickAction::Play(_)),
            "buffer stalled behind a lost frame: {action:?}"
        );
    }

    /// A sender that restarted leaves a hole no waiting fills.
    #[test]
    fn a_large_sequence_jump_resynchronises() {
        let mut b = started_at(0);

        b.push(f(1_000));
        match b.tick() {
            TickAction::Play(fr) => assert_eq!(fr.seq, 1_000),
            other => panic!("expected resync, got {other:?}"),
        }
    }

    /// The defect this bound exists to stop. Concealment used to run forever
    /// once a peer had sent anything, and every concealed frame re-anchored the
    /// A/V timeline — so a peer who stopped sharing left a clock climbing at
    /// real time on every receiver, and their *next* share (video with no
    /// audio, so nothing to correct it) had every frame dropped as late.
    #[test]
    fn a_stopped_stream_stops_advancing_the_timeline() {
        let mut b = started_at(0);

        let mut concealed = 0;
        for _ in 0..(MAX_CONCEALED_FRAMES + 5) {
            match b.tick() {
                TickAction::Conceal { .. } => concealed += 1,
                TickAction::Idle => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(concealed, MAX_CONCEALED_FRAMES);
        assert!(
            matches!(b.tick(), TickAction::Idle),
            "a stopped stream must stay idle, not resume concealing"
        );
    }

    /// Stopping must not poison the peer: sharing again starts a clean
    /// timeline from whatever they send next, at whatever timestamp.
    #[test]
    fn a_restarted_stream_starts_a_fresh_timeline() {
        let mut b = JitterBuffer::new();
        // A first session that ran a while, then stopped.
        b.push(PendingFrame {
            seq: 900,
            pts_us: 60_000_000,
            opus: vec![1],
        });
        for _ in 0..(MAX_CONCEALED_FRAMES as usize + JITTER_DEPTH + 2) {
            let _ = b.tick();
        }

        // A new session: sequence and timestamps both restart near zero.
        b.push(f(0));
        for _ in 0..JITTER_DEPTH {
            let _ = b.tick();
        }
        match b.tick() {
            TickAction::Play(fr) => {
                assert_eq!(fr.seq, 0);
                assert_eq!(fr.pts_us, 0, "the new session's own timeline, not the old");
            }
            other => panic!("expected the new session to play, got {other:?}"),
        }
    }

    /// Concealment must still cover an ordinary gap — the bound is an end-of-
    /// stream test, not a reason to abandon a stream that is merely lossy.
    #[test]
    fn a_short_gap_still_conceals_and_recovers() {
        let mut b = started_at(0);

        for _ in 0..3 {
            assert!(matches!(b.tick(), TickAction::Conceal { .. }));
        }
        // The stream resumes; it carries on and the streak is forgotten.
        for seq in 4..4 + JITTER_DEPTH as u32 {
            b.push(f(seq));
        }
        assert!(matches!(b.tick(), TickAction::Play(_)));
        while let TickAction::Play(_) = b.tick() {}
        for _ in 0..(MAX_CONCEALED_FRAMES - 1) {
            assert!(
                matches!(b.tick(), TickAction::Conceal { .. }),
                "a real frame must reset the concealment budget"
            );
        }
    }

    /// A peer who stops is dropped rather than kept as an inert buffer: in a
    /// long-lived room the map would otherwise grow for everyone who ever
    /// shared audio.
    #[test]
    fn a_stopped_peer_is_forgotten_by_the_playout() {
        let mut p = ContentPlayout::new();
        p.accept("alice", f(0));
        assert_eq!(p.tracked_peers(), 1);

        for _ in 0..(MAX_CONCEALED_FRAMES as usize + JITTER_DEPTH + 3) {
            let _ = p.tick();
        }
        assert_eq!(
            p.tracked_peers(),
            0,
            "a peer who stopped sending should not be tracked forever"
        );
    }

    #[test]
    fn playout_tracks_peers_independently() {
        let mut p = ContentPlayout::new();
        for seq in 0..JITTER_DEPTH as u32 {
            p.accept("alice", f(seq));
            p.accept("bob", f(100 + seq));
        }

        let actions = p.tick();
        assert_eq!(actions.len(), 2);
        assert_eq!(p.tracked_peers(), 2);

        p.forget("alice");
        assert_eq!(p.tracked_peers(), 1);
        p.clear();
        assert_eq!(p.tracked_peers(), 0);
    }

    #[test]
    fn a_silent_peer_is_omitted_rather_than_reported() {
        let mut p = ContentPlayout::new();
        p.accept("alice", f(0));
        p.forget("alice");
        assert!(p.tick().is_empty());
    }

    /// The join the whole design rests on: the tick must set an anchor on the
    #[test]
    fn a_silent_frame_is_one_frame_of_zeroes() {
        let s = silent_frame();
        assert_eq!(s.len(), SAMPLES_PER_FRAME);
        assert!(s.iter().all(|v| *v == 0));
    }
}
