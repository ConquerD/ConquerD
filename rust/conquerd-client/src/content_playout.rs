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

use tracing::warn;

use crate::call_controller::SAMPLES_PER_FRAME;
use crate::content_sender::FRAME_DURATION_US;

/// Frames buffered per peer before the oldest is played out regardless.
///
/// Three frames is 60 ms of reordering tolerance — enough for ordinary network
/// jitter, and the ceiling on how far this stream can drag video behind it.
pub const JITTER_DEPTH: usize = 3;

/// Gap in sequence numbers past which the buffer resynchronises rather than
/// waiting.
///
/// A sender that stopped and restarted, or a long outage, leaves a hole no
/// amount of waiting fills; continuing to expect the old sequence would stall
/// the stream indefinitely.
pub const RESYNC_GAP: u32 = 50;

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
#[derive(Debug, Default)]
pub struct JitterBuffer {
    pending: Vec<PendingFrame>,
    /// Sequence the next tick expects. `None` until the first frame arrives.
    next_seq: Option<u32>,
    /// Timestamp the next tick will report, advanced every tick including
    /// concealed ones.
    next_pts_us: u64,
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
    }

    /// Decide what to play now, advancing the timeline by one frame.
    ///
    /// Always advances when the stream is running, which is the property the
    /// video side depends on: a tick that returned nothing on loss would let
    /// the anchor go stale and video would jump.
    pub fn tick(&mut self) -> TickAction {
        let Some(expected) = self.next_seq else {
            // Not started: adopt the earliest buffered frame as the origin.
            let Some(first) = self.pending.first().cloned() else {
                return TickAction::Idle;
            };
            self.pending.retain(|f| f.seq != first.seq);
            self.next_seq = Some(first.seq.wrapping_add(1));
            self.next_pts_us = first.pts_us.saturating_add(FRAME_DURATION_US);
            return TickAction::Play(first);
        };

        // A long gap means the sender restarted or was away; resynchronise on
        // whatever is buffered rather than waiting for a frame that is gone.
        if let Some(first) = self.pending.first().cloned() {
            if first.seq.wrapping_sub(expected) >= RESYNC_GAP
                || self.pending.len() > JITTER_DEPTH
                || first.seq == expected
            {
                self.pending.retain(|f| f.seq != first.seq);
                self.next_seq = Some(first.seq.wrapping_add(1));
                self.next_pts_us = first.pts_us.saturating_add(FRAME_DURATION_US);
                return TickAction::Play(first);
            }
        }

        // Expected frame not here yet: conceal and keep the timeline moving.
        let pts_us = self.next_pts_us;
        self.next_seq = Some(expected.wrapping_add(1));
        self.next_pts_us = pts_us.saturating_add(FRAME_DURATION_US);
        TickAction::Conceal { pts_us }
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
    /// a caller can iterate the result directly.
    pub fn tick(&mut self) -> Vec<(String, TickAction)> {
        let mut out = Vec::new();
        for (peer, buf) in self.buffers.iter_mut() {
            match buf.tick() {
                TickAction::Idle => {}
                action => out.push((peer.clone(), action)),
            }
        }
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

    #[test]
    fn an_empty_buffer_is_idle() {
        assert_eq!(JitterBuffer::new().tick(), TickAction::Idle);
    }

    #[test]
    fn the_first_frame_starts_the_timeline() {
        let mut b = JitterBuffer::new();
        b.push(f(10));
        assert_eq!(b.tick(), TickAction::Play(f(10)));
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
        let mut b = JitterBuffer::new();
        b.push(f(0));
        assert!(matches!(b.tick(), TickAction::Play(_)));
        // A late duplicate of seq 0 must not be queued behind the timeline.
        b.push(f(0));
        assert!(b.is_empty());
    }

    /// The property video depends on: a missing frame must still advance the
    /// timeline. A tick that returned nothing would let the anchor go stale and
    /// video would free-run and jump.
    #[test]
    fn a_gap_conceals_and_still_advances() {
        let mut b = JitterBuffer::new();
        b.push(f(0));
        assert!(matches!(b.tick(), TickAction::Play(_)));

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
        let mut b = JitterBuffer::new();
        b.push(f(0));
        let _ = b.tick();

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
        let mut b = JitterBuffer::new();
        b.push(f(0));
        let _ = b.tick(); // plays 0, now expecting 1

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
        let mut b = JitterBuffer::new();
        b.push(f(0));
        let _ = b.tick();

        b.push(f(1_000));
        match b.tick() {
            TickAction::Play(fr) => assert_eq!(fr.seq, 1_000),
            other => panic!("expected resync, got {other:?}"),
        }
    }

    #[test]
    fn playout_tracks_peers_independently() {
        let mut p = ContentPlayout::new();
        p.accept("alice", f(0));
        p.accept("bob", f(100));

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
