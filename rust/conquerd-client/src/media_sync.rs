//! Audio-led A/V synchronisation: deciding when a decoded video frame is shown.
//!
//! Content audio is the master and video is the slave. That asymmetry is not
//! arbitrary — audio glitches are far more noticeable than a video frame shown
//! a few milliseconds early or late, and stretching or resampling audio to
//! chase video is both expensive and audible. So audio plays at its own steady
//! cadence and video is held or dropped to meet it.
//!
//! # How the audio timeline is known
//!
//! Not by asking the audio path where it is. Instead, every time a content
//! frame is *played*, the receiver records a [`PlayoutAnchor`]: the frame's
//! timestamp and the instant it went out. Between anchors the timeline is
//! extrapolated with the local monotonic clock, which is accurate over the
//! 20 ms gaps involved.
//!
//! Extrapolating rather than interpolating matters for loss: when concealment
//! substitutes a frame, the caller advances the anchor by one frame duration
//! anyway, so video keeps moving instead of freezing until real audio returns.
//!
//! # Per sender, never across senders
//!
//! Timestamps are on the *sender's* session clock, so two peers' streams are
//! not comparable. Every anchor and every queue here is keyed by peer, and
//! nothing in this module ever compares one peer's timestamp to another's.
//!
//! # Degrading to free-run
//!
//! A peer sending video but no content audio — a camera-only call, or a peer
//! whose build predates this — has no anchor, and video must render as soon as
//! it decodes rather than waiting for a timeline that will never arrive. That
//! is [`SyncDecision::Show`] from [`VideoPlayout::decide`] whenever the anchor
//! is absent or stale.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How far *behind* the audio timeline a frame may be and still be shown.
///
/// Past this it is dropped: catching up by showing stale frames back-to-back
/// looks worse than skipping to the current one, and the frames after it are
/// already waiting.
pub const LATE_TOLERANCE_US: u64 = 50_000;

/// How far *ahead* of the audio timeline a frame may be and still be shown.
///
/// Small but non-zero: releasing a frame a little early is imperceptible,
/// whereas holding every frame until its exact instant would add a systematic
/// delay equal to the tick period.
pub const EARLY_TOLERANCE_US: u64 = 10_000;

/// Frames held per peer before the oldest is discarded.
///
/// Deliberately shallow. A deep queue is latency, and video that is behind is
/// better cut than buffered — the whole point of holding a frame is to wait a
/// few milliseconds, not to build a reservoir.
pub const MAX_QUEUED_FRAMES: usize = 6;

/// How stale an anchor may be before the audio timeline is treated as gone.
///
/// Beyond this the sender has stopped sending content audio (muted, stopped
/// sharing, or disconnected), and video must free-run rather than stall
/// against a timeline frozen in the past.
pub const ANCHOR_STALE_AFTER: Duration = Duration::from_millis(400);

/// The last content-audio frame played for one peer, and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayoutAnchor {
    /// Timestamp of the frame that was played, on the sender's clock.
    pub pts_us: u64,
    /// Local instant it was played.
    pub at: Instant,
}

impl PlayoutAnchor {
    /// Where the sender's audio timeline has reached, as of `now`.
    ///
    /// Saturates rather than wrapping if `now` precedes the anchor, which a
    /// caller holding a stale instant could otherwise turn into an enormous
    /// timestamp and a wrongly-dropped frame.
    pub fn extrapolate(&self, now: Instant) -> u64 {
        self.pts_us
            .saturating_add(now.saturating_duration_since(self.at).as_micros() as u64)
    }

    /// Whether this anchor is too old to steer by.
    pub fn is_stale(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.at) >= ANCHOR_STALE_AFTER
    }
}

/// What to do with a decoded video frame right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncDecision {
    /// Display it.
    Show,
    /// Keep it; it is ahead of the audio timeline.
    Hold,
    /// Discard it; the audio timeline has already passed it.
    Drop,
}

/// The rule itself, against an already-resolved audio position.
///
/// The single place the tolerances are applied, so `decide` and the queue sweep
/// cannot drift apart — two copies of a threshold comparison is exactly the
/// kind of duplication that ends with frames shown by one path and dropped by
/// the other.
///
/// `None` means no usable timeline, which shows the frame: see the module docs
/// on free-running.
fn decide_against(audio_now_us: Option<u64>, pts_us: u64) -> SyncDecision {
    let Some(audio_now) = audio_now_us else {
        return SyncDecision::Show;
    };
    if pts_us + LATE_TOLERANCE_US < audio_now {
        SyncDecision::Drop
    } else if pts_us <= audio_now + EARLY_TOLERANCE_US {
        SyncDecision::Show
    } else {
        SyncDecision::Hold
    }
}

/// Per-peer video hold queues and audio anchors.
///
/// Generic over the frame payload so this module stays free of pixel formats
/// and stays testable with trivial stand-ins: the sync rule cares only about
/// timestamps, and nothing here should need to know what a picture is.
pub struct VideoPlayout<T> {
    anchors: HashMap<String, PlayoutAnchor>,
    queues: HashMap<String, Vec<QueuedFrame<T>>>,
}

// Hand-written rather than derived: a derived impl would demand `T: Debug`,
// and the payload is a decoded picture that no log should ever try to print.
impl<T> std::fmt::Debug for VideoPlayout<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoPlayout")
            .field("peers_anchored", &self.anchors.len())
            .field("peers_queued", &self.queues.len())
            .finish()
    }
}

// Derived `Default` would demand `T: Default`, which the payload need not be.
impl<T> Default for VideoPlayout<T> {
    fn default() -> Self {
        Self {
            anchors: HashMap::new(),
            queues: HashMap::new(),
        }
    }
}

/// A decoded frame waiting for its moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedFrame<T> {
    pub pts_us: u64,
    /// The decoded frame, however the caller represents it.
    pub payload: T,
}

impl<T: Clone> VideoPlayout<T> {
    /// Create an empty playout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a content-audio frame was just played for `peer`.
    ///
    /// Call this on concealed frames too, advancing `pts_us` by one frame
    /// duration: without it, loss on the audio stream would stall video.
    pub fn note_audio_played(&mut self, peer: &str, pts_us: u64, at: Instant) {
        self.anchors
            .insert(peer.to_owned(), PlayoutAnchor { pts_us, at });
    }

    /// The peer's audio timeline as of `now`, or `None` when it cannot be
    /// steered by (absent or stale).
    pub fn audio_now_us(&self, peer: &str, now: Instant) -> Option<u64> {
        self.anchors
            .get(peer)
            .filter(|a| !a.is_stale(now))
            .map(|a| a.extrapolate(now))
    }

    /// Decide what to do with a frame of `pts_us` from `peer`.
    ///
    /// With no usable audio timeline this is always [`SyncDecision::Show`] —
    /// see the module docs on free-running.
    pub fn decide(&self, peer: &str, pts_us: u64, now: Instant) -> SyncDecision {
        decide_against(self.audio_now_us(peer, now), pts_us)
    }

    /// Queue a decoded frame, returning every frame that should be displayed
    /// now, oldest first.
    ///
    /// Frames the timeline has passed are discarded rather than returned, so a
    /// caller can render everything it receives without re-checking.
    pub fn push(&mut self, peer: &str, frame: QueuedFrame<T>, now: Instant) -> Vec<QueuedFrame<T>> {
        let queue = self.queues.entry(peer.to_owned()).or_default();
        queue.push(frame);
        // Ordered by timestamp: the network can reorder, and showing frames out
        // of order is worse than the reordering itself.
        queue.sort_by_key(|f| f.pts_us);

        // Shed the oldest first if the peer is running ahead of us. Dropping
        // the newest instead would mean never catching up.
        while queue.len() > MAX_QUEUED_FRAMES {
            queue.remove(0);
        }

        // Resolved once for the whole sweep rather than per frame: the answer
        // cannot change within a single call, and re-deriving it would invite
        // the two paths to disagree.
        let audio_now = self.audio_now_us(peer, now);
        let queue = self.queues.entry(peer.to_owned()).or_default();

        let mut out = Vec::new();
        queue.retain(|f| match decide_against(audio_now, f.pts_us) {
            SyncDecision::Show => {
                out.push(f.clone());
                false
            }
            SyncDecision::Drop => false,
            SyncDecision::Hold => true,
        });
        out
    }

    /// Frames currently held for `peer`.
    pub fn queued_len(&self, peer: &str) -> usize {
        self.queues.get(peer).map_or(0, |q| q.len())
    }

    /// Forget a peer entirely — they left, or turned their camera off.
    pub fn forget(&mut self, peer: &str) {
        self.anchors.remove(peer);
        self.queues.remove(peer);
    }

    /// Forget everyone, on leaving a room or ending a call.
    pub fn clear(&mut self) {
        self.anchors.clear();
        self.queues.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pts_us: u64) -> QueuedFrame<Vec<u8>> {
        QueuedFrame {
            pts_us,
            payload: vec![pts_us as u8],
        }
    }

    /// The concrete instantiation the tests exercise.
    fn playout() -> VideoPlayout<Vec<u8>> {
        VideoPlayout::new()
    }

    #[test]
    fn extrapolation_advances_with_the_local_clock() {
        let at = Instant::now();
        let anchor = PlayoutAnchor {
            pts_us: 1_000_000,
            at,
        };
        assert_eq!(anchor.extrapolate(at), 1_000_000);
        assert_eq!(
            anchor.extrapolate(at + Duration::from_millis(20)),
            1_020_000
        );
    }

    #[test]
    fn extrapolation_saturates_before_the_anchor() {
        let at = Instant::now() + Duration::from_secs(10);
        let anchor = PlayoutAnchor { pts_us: 500, at };
        // An instant before the anchor must not wrap into a huge timestamp,
        // which would drop every frame as hopelessly late.
        assert_eq!(anchor.extrapolate(at - Duration::from_secs(5)), 500);
    }

    /// A peer sending video but no content audio must render immediately.
    /// Waiting for a timeline that will never arrive would freeze the tile.
    #[test]
    fn video_free_runs_without_an_audio_anchor() {
        let p = playout();
        assert_eq!(
            p.decide("alice", 12_345, Instant::now()),
            SyncDecision::Show
        );
    }

    #[test]
    fn a_frame_matching_the_audio_timeline_is_shown() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 1_000_000, now);
        assert_eq!(p.decide("alice", 1_000_000, now), SyncDecision::Show);
    }

    #[test]
    fn a_frame_ahead_of_the_timeline_is_held() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 1_000_000, now);
        // 100 ms ahead: well past the early tolerance.
        assert_eq!(p.decide("alice", 1_100_000, now), SyncDecision::Hold);
    }

    #[test]
    fn a_frame_far_behind_the_timeline_is_dropped() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 1_000_000, now);
        // 200 ms late: showing it would be visibly behind the audio.
        assert_eq!(p.decide("alice", 800_000, now), SyncDecision::Drop);
    }

    /// The tolerances must not leave a gap where a frame is neither shown,
    /// held, nor dropped — every timestamp has to land somewhere.
    #[test]
    fn every_timestamp_gets_a_decision() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 1_000_000, now);
        for offset in (0..400_000).step_by(1_000) {
            let pts = 1_000_000u64.saturating_sub(200_000).saturating_add(offset);
            let d = p.decide("alice", pts, now);
            assert!(matches!(
                d,
                SyncDecision::Show | SyncDecision::Hold | SyncDecision::Drop
            ));
        }
    }

    /// A sender that stops content audio must not freeze the video tile: past
    /// the staleness window the timeline is abandoned and video free-runs.
    #[test]
    fn a_stale_anchor_releases_video_to_free_run() {
        let mut p = playout();
        let t0 = Instant::now();
        p.note_audio_played("alice", 1_000_000, t0);

        let later = t0 + ANCHOR_STALE_AFTER;
        assert!(p.audio_now_us("alice", later).is_none());
        // A frame far ahead would otherwise be held forever.
        assert_eq!(p.decide("alice", 9_000_000, later), SyncDecision::Show);
    }

    #[test]
    fn anchors_are_per_peer_and_never_shared() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 5_000_000, now);
        // Bob has no anchor of his own; alice's must not steer his video.
        assert!(p.audio_now_us("bob", now).is_none());
        assert_eq!(p.decide("bob", 1, now), SyncDecision::Show);
    }

    #[test]
    fn queued_frames_are_released_as_the_timeline_reaches_them() {
        let mut p = playout();
        let t0 = Instant::now();
        p.note_audio_played("alice", 1_000_000, t0);

        // 60 ms ahead — held.
        let shown = p.push("alice", frame(1_060_000), t0);
        assert!(shown.is_empty());
        assert_eq!(p.queued_len("alice"), 1);

        // The audio timeline advances to meet it.
        p.note_audio_played("alice", 1_060_000, t0 + Duration::from_millis(60));
        let shown = p.push("alice", frame(1_080_000), t0 + Duration::from_millis(60));
        assert_eq!(shown.len(), 1, "the held frame should now be released");
        assert_eq!(shown[0].pts_us, 1_060_000);
    }

    #[test]
    fn reordered_frames_are_released_in_timestamp_order() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 2_000_000, now);

        // Arrive out of order; both are due.
        p.push("alice", frame(1_990_000), now);
        let shown = p.push("alice", frame(1_980_000), now);
        let stamps: Vec<u64> = shown.iter().map(|f| f.pts_us).collect();
        let mut sorted = stamps.clone();
        sorted.sort_unstable();
        assert_eq!(stamps, sorted, "frames released out of order: {stamps:?}");
    }

    /// The queue is a few milliseconds of slack, not a reservoir. Past the cap
    /// the *oldest* goes, because dropping the newest would mean never
    /// catching up.
    #[test]
    fn the_queue_is_bounded_and_sheds_the_oldest() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 0, now);

        for i in 0..(MAX_QUEUED_FRAMES + 4) {
            // Far ahead, so everything is held.
            p.push("alice", frame(10_000_000 + i as u64 * 20_000), now);
        }
        assert_eq!(p.queued_len("alice"), MAX_QUEUED_FRAMES);
    }

    /// Concealed audio must advance the anchor, or loss on the audio stream
    /// would stall video behind a timeline that stopped moving.
    #[test]
    fn advancing_the_anchor_through_loss_keeps_video_moving() {
        let mut p = playout();
        let t0 = Instant::now();
        p.note_audio_played("alice", 1_000_000, t0);

        let held = p.push("alice", frame(1_100_000), t0);
        assert!(held.is_empty());

        // Five concealed frames: the caller advances by a frame each time.
        let mut pts = 1_000_000;
        for i in 1..=5u64 {
            pts += 20_000;
            p.note_audio_played("alice", pts, t0 + Duration::from_millis(20 * i));
        }
        let shown = p.push("alice", frame(1_120_000), t0 + Duration::from_millis(100));
        assert!(
            !shown.is_empty(),
            "video stalled behind a timeline that loss should have advanced"
        );
    }

    #[test]
    fn forgetting_a_peer_clears_both_anchor_and_queue() {
        let mut p = playout();
        let now = Instant::now();
        p.note_audio_played("alice", 1_000_000, now);
        p.push("alice", frame(9_000_000), now);
        assert_eq!(p.queued_len("alice"), 1);

        p.forget("alice");
        assert_eq!(p.queued_len("alice"), 0);
        assert!(p.audio_now_us("alice", now).is_none());
    }

    #[test]
    fn clear_forgets_everyone() {
        let mut p = playout();
        let now = Instant::now();
        for peer in ["alice", "bob"] {
            p.note_audio_played(peer, 1_000, now);
            p.push(peer, frame(9_000_000), now);
        }
        p.clear();
        assert_eq!(p.queued_len("alice"), 0);
        assert_eq!(p.queued_len("bob"), 0);
    }
}
