//! The sender-side clock that synchronised media is stamped against.
//!
//! A/V sync needs one thing above all: audio and video that were captured at
//! the same instant must carry the same number. That is only true if both are
//! stamped from a single clock, so this type exists to be that clock — created
//! once when a media session starts, shared by every capture path, destroyed
//! when the session ends.
//!
//! # What it is not
//!
//! **Not wall-clock, and not shared between peers.** Timestamps are relative to
//! this sender's session start, so two peers never need agreeing clocks and
//! there is no NTP dependency. A receiver compares timestamps *within* one
//! sender's stream and never across senders — see the receiver policy in
//! `backlog.md`.
//!
//! **Not the voice path's clock.** The mic keeps its existing untimestamped
//! path; only content audio and video are stamped. That split is deliberate and
//! is explained in the same place.
//!
//! # Stamping discipline
//!
//! Stamp at **capture**, before encode. Stamping after the encoder folds encode
//! latency into the timestamp, and since audio and video encoders have very
//! different latencies, that shows up as a fixed skew no receiver can correct —
//! the exact defect this clock exists to prevent.

use std::sync::Arc;
use std::time::Instant;

/// Microseconds since a session's start.
///
/// `u64` microseconds rather than a coarser tick: microseconds are unambiguous
/// across the 20 ms audio and ~33 ms video cadences, and `u64` cannot wrap in
/// any session a human will hold (it covers ~584,000 years).
pub type PtsUs = u64;

/// Monotonic media clock for one session.
///
/// Cheap to clone — clones share the same origin, which is the point: the
/// capture threads each hold one and must agree.
#[derive(Clone, Debug)]
pub struct SessionMediaClock {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Session origin. `Instant` is monotonic, so this is immune to the system
    /// clock being stepped mid-call by NTP or a user — which a wall-clock
    /// timestamp would not be.
    t0: Instant,
}

impl SessionMediaClock {
    /// Start a clock whose origin is now.
    pub fn start() -> Self {
        Self::starting_at(Instant::now())
    }

    /// Start a clock with an explicit origin.
    ///
    /// Exists for tests, which need a known origin to assert against, and for
    /// the case where a session's true start is already known when the clock is
    /// constructed.
    pub fn starting_at(t0: Instant) -> Self {
        Self {
            inner: Arc::new(Inner { t0 }),
        }
    }

    /// Microseconds since session start, for stamping a frame captured now.
    pub fn now_pts_us(&self) -> PtsUs {
        self.pts_at(Instant::now())
    }

    /// Microseconds between session start and `at`.
    ///
    /// Preferred over [`now_pts_us`](Self::now_pts_us) when the capture backend
    /// reports *when* a frame was taken: using that instant instead of the
    /// moment it reached us keeps queueing delay out of the timestamp.
    ///
    /// An `at` before the origin saturates to 0 rather than wrapping. That can
    /// only happen if a caller holds an instant captured before the clock
    /// existed, and a zero is a harmless "start of session" rather than the
    /// enormous number a wrapping subtraction would produce.
    pub fn pts_at(&self, at: Instant) -> PtsUs {
        at.saturating_duration_since(self.inner.t0).as_micros() as u64
    }

    /// The origin, for callers that need to convert their own instants.
    pub fn origin(&self) -> Instant {
        self.inner.t0
    }

    /// Whether two handles are the same clock.
    ///
    /// Audio and video stamped from *different* clocks would each look
    /// internally consistent while being mutually meaningless, and the symptom
    /// would be a fixed A/V offset rather than an error. Callers that thread a
    /// clock through several components can assert on this.
    pub fn is_same_clock(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn starts_at_zero() {
        let t0 = Instant::now();
        let clock = SessionMediaClock::starting_at(t0);
        assert_eq!(clock.pts_at(t0), 0);
    }

    #[test]
    fn advances_in_microseconds() {
        let t0 = Instant::now();
        let clock = SessionMediaClock::starting_at(t0);
        assert_eq!(clock.pts_at(t0 + Duration::from_millis(20)), 20_000);
        assert_eq!(clock.pts_at(t0 + Duration::from_secs(1)), 1_000_000);
        // A video frame interval at 30fps.
        assert_eq!(clock.pts_at(t0 + Duration::from_micros(33_333)), 33_333);
    }

    /// The whole reason the type exists: two capture paths holding clones must
    /// produce comparable numbers for the same instant.
    #[test]
    fn clones_share_one_timeline() {
        let t0 = Instant::now();
        let audio_clock = SessionMediaClock::starting_at(t0);
        let video_clock = audio_clock.clone();

        let captured = t0 + Duration::from_millis(500);
        assert_eq!(audio_clock.pts_at(captured), video_clock.pts_at(captured));
        assert!(audio_clock.is_same_clock(&video_clock));
    }

    /// Two *separately started* clocks are not interchangeable. Stamping audio
    /// from one and video from another yields a constant offset that presents
    /// as a sync bug, not an error, so the distinction is worth being able to
    /// assert on.
    #[test]
    fn independently_started_clocks_are_distinguishable() {
        let a = SessionMediaClock::start();
        let b = SessionMediaClock::start();
        assert!(!a.is_same_clock(&b));
    }

    #[test]
    fn an_instant_before_the_origin_saturates_to_zero() {
        let t0 = Instant::now() + Duration::from_secs(10);
        let clock = SessionMediaClock::starting_at(t0);
        // Well before the origin; must not wrap into a huge value.
        assert_eq!(clock.pts_at(t0 - Duration::from_secs(5)), 0);
    }

    #[test]
    fn now_is_monotonic_and_close_to_the_origin() {
        let clock = SessionMediaClock::start();
        let a = clock.now_pts_us();
        let b = clock.now_pts_us();
        assert!(b >= a, "clock went backwards: {a} then {b}");
        // A freshly started clock should read near zero, not garbage.
        assert!(b < 5_000_000, "unexpectedly large pts {b} on a new clock");
    }

    /// Sanity check on the unit: a 20 ms Opus frame and a 30fps video frame
    /// must be distinguishable, which is why microseconds were chosen over
    /// milliseconds-as-integers.
    #[test]
    fn resolution_separates_audio_and_video_cadences() {
        let t0 = Instant::now();
        let clock = SessionMediaClock::starting_at(t0);
        let audio_frame = clock.pts_at(t0 + Duration::from_millis(20));
        let video_frame = clock.pts_at(t0 + Duration::from_micros(33_333));
        assert_ne!(audio_frame, video_frame);
        assert!(video_frame - audio_frame > 13_000);
    }

    #[test]
    fn origin_round_trips() {
        let t0 = Instant::now();
        let clock = SessionMediaClock::starting_at(t0);
        assert_eq!(clock.origin(), t0);
        assert_eq!(clock.pts_at(clock.origin()), 0);
    }
}
