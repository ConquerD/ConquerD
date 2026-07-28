//! Event pump for asynchronous (hardware) Media Foundation transforms.
//!
//! Hardware MFTs do not answer `ProcessInput`/`ProcessOutput` on demand. They
//! post `METransformNeedInput` and `METransformHaveOutput` out of band, and the
//! documented way to receive those is `BeginGetEvent` with an
//! `IMFAsyncCallback` — not polling `GetEvent`. Polling is what an earlier
//! attempt did, and it saw no events at all, so the hardware path fell back to
//! software every time.
//!
//! The callback runs on a Media Foundation worker thread. It does the minimum
//! there — record the event kind, re-arm — and hands everything else to the
//! codec thread through a mutex-guarded queue.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

use windows::core::{implement, Result as WinResult};
// `to_interface` (used to re-arm from inside the callback) lives on this trait.
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFMediaEventGenerator,
};
use windows_core::IUnknownImpl;

/// `METransformNeedInput`.
pub const ME_TRANSFORM_NEED_INPUT: u32 = 601;
/// `METransformHaveOutput`.
pub const ME_TRANSFORM_HAVE_OUTPUT: u32 = 602;
/// `METransformDrainComplete`.
pub const ME_TRANSFORM_DRAIN_COMPLETE: u32 = 603;

/// Shared state between the MF worker thread and the codec thread.
#[derive(Default)]
struct Shared {
    /// Event kinds in arrival order.
    ///
    /// Order matters: a `NeedInput` consumed out of order would let us feed a
    /// transform that had not asked, which the contract answers with
    /// `MF_E_NOTACCEPTING`.
    queue: Mutex<VecDeque<u32>>,
    /// Signals arrival so a waiter does not have to spin.
    cv: Condvar,
}

/// COM callback object handed to `BeginGetEvent`.
///
/// Holds the generator so it can re-arm itself: an MF event queue delivers one
/// event per `BeginGetEvent`, so failing to re-arm inside `Invoke` stops the
/// stream dead after exactly one event.
#[implement(IMFAsyncCallback)]
struct EventCallback {
    shared: Arc<Shared>,
    generator: IMFMediaEventGenerator,
}

#[allow(non_snake_case)]
impl IMFAsyncCallback_Impl for EventCallback_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> WinResult<()> {
        // Returning E_NOTIMPL is explicitly allowed and asks MF to use its
        // default work queue, which is what we want.
        Err(windows::core::Error::from(
            windows::Win32::Foundation::E_NOTIMPL,
        ))
    }

    fn Invoke(&self, result: Option<&IMFAsyncResult>) -> WinResult<()> {
        let Some(result) = result else {
            return Ok(());
        };
        // SAFETY: `result` is the completion handed back by MF for the
        // BeginGetEvent we issued; EndGetEvent consumes it exactly once.
        let event = match unsafe { self.generator.EndGetEvent(result) } {
            Ok(e) => e,
            // The queue was shut down (transform released). Do not re-arm.
            Err(_) => return Ok(()),
        };

        // SAFETY: live event object.
        if let Ok(kind) = unsafe { event.GetType() } {
            self.shared.push(kind);
        }

        // Re-arm for the next event. Without this the pump stops after one.
        // SAFETY: `self` is a live COM object for as long as MF holds a ref.
        let me: IMFAsyncCallback = self.to_interface();
        unsafe {
            let _ = self.generator.BeginGetEvent(&me, None);
        }
        Ok(())
    }
}

/// Receives transform events for one asynchronous MFT.
pub struct EventPump {
    shared: Arc<Shared>,
    // Kept alive for as long as the pump exists; dropping it would release the
    // callback while MF may still hold a reference.
    _callback: IMFAsyncCallback,
}

impl EventPump {
    /// Start pumping events from `generator`.
    ///
    /// Must be called **before** `MFT_MESSAGE_NOTIFY_START_OF_STREAM`: the
    /// transform may post its first `METransformNeedInput` immediately on
    /// receiving that message, and an unarmed queue would drop it.
    pub fn start(generator: &IMFMediaEventGenerator) -> WinResult<Self> {
        let shared = Arc::new(Shared::default());
        let callback: IMFAsyncCallback = EventCallback {
            shared: Arc::clone(&shared),
            generator: generator.clone(),
        }
        .into();

        // SAFETY: arms the queue; the callback outlives this call via the
        // reference MF holds plus the one stored below.
        unsafe { generator.BeginGetEvent(&callback, None)? };

        Ok(Self {
            shared,
            _callback: callback,
        })
    }

    /// Wait up to `timeout` for an event of `kind`, consuming it.
    ///
    /// Other event kinds seen while waiting are left queued rather than
    /// discarded — a `HaveOutput` observed while waiting for `NeedInput` still
    /// has to be acted on, or the transform stalls holding a frame.
    ///
    /// Only safe when the caller can act on the *other* kind afterwards. To
    /// drive a transform, use [`wait_next`](Self::wait_next).
    pub fn wait_for(&self, kind: u32, timeout: std::time::Duration) -> bool {
        self.shared.wait_for(kind, timeout)
    }

    /// Pop the next event in arrival order, waiting up to `timeout` for one.
    ///
    /// This, not [`wait_for`](Self::wait_for), is how an MFT is meant to be
    /// driven. The two event kinds are not independent: a transform that has
    /// posted `METransformHaveOutput` will not post another
    /// `METransformNeedInput` until that output is collected, so waiting for
    /// one specific kind deadlocks against the other. The caller must see every
    /// event and act on it.
    pub fn wait_next(&self, timeout: std::time::Duration) -> Option<u32> {
        self.shared.wait_next(timeout)
    }

    /// Consume one queued event of `kind` if present, without waiting.
    pub fn take(&self, kind: u32) -> bool {
        self.shared.take(kind)
    }

    /// Drop every queued event, e.g. after a flush.
    ///
    /// Required by the contract: after `MFT_MESSAGE_COMMAND_FLUSH` the
    /// transform sends no further `NeedInput` until the next
    /// `NOTIFY_START_OF_STREAM`, so stale pre-flush events would authorise a
    /// `ProcessInput` the transform has not asked for.
    pub fn clear(&self) {
        self.shared.clear();
    }
}

/// The queue half, split out from [`EventPump`] so its semantics can be tested
/// without a live Media Foundation transform to post events.
impl Shared {
    fn push(&self, kind: u32) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(kind);
        self.cv.notify_all();
    }

    fn wait_for(&self, kind: u32, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(pos) = q.iter().position(|&k| k == kind) {
                q.remove(pos);
                return true;
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (guard, _) = self
                .cv
                .wait_timeout(q, remaining)
                .unwrap_or_else(|e| e.into_inner());
            q = guard;
        }
    }

    fn wait_next(&self, timeout: std::time::Duration) -> Option<u32> {
        let deadline = std::time::Instant::now() + timeout;
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(kind) = q.pop_front() {
                return Some(kind);
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let (guard, _) = self
                .cv
                .wait_timeout(q, remaining)
                .unwrap_or_else(|e| e.into_inner());
            q = guard;
        }
    }

    fn take(&self, kind: u32) -> bool {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = q.iter().position(|&k| k == kind) {
            q.remove(pos);
            return true;
        }
        false
    }

    fn clear(&self) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

// SAFETY: all shared state is behind a mutex; the COM pointer is only used to
// keep the callback alive and is not dereferenced across threads by us.
unsafe impl Send for EventPump {}
unsafe impl Sync for EventPump {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const NONE: Duration = Duration::ZERO;

    /// The bug this module exists to fix, in miniature.
    ///
    /// A transform that has posted output stops asking for input until that
    /// output is collected. Waiting for `NeedInput` alone therefore hangs
    /// against a queue full of `HaveOutput` — which is exactly how the hardware
    /// encoder presented: it stalled on the fourth frame with three
    /// uncollected `HaveOutput` events pending.
    #[test]
    fn waiting_for_one_kind_does_not_see_the_other() {
        let shared = Shared::default();
        for _ in 0..3 {
            shared.push(ME_TRANSFORM_HAVE_OUTPUT);
        }
        assert!(
            !shared.wait_for(ME_TRANSFORM_NEED_INPUT, NONE),
            "NeedInput must not be reported when only HaveOutput is queued"
        );
        // And the output is still there to be collected — not consumed by the
        // failed wait, or the frames would be lost outright.
        assert!(shared.take(ME_TRANSFORM_HAVE_OUTPUT));
    }

    /// `wait_next` is the fix: every event is visible, so the caller can
    /// service output and then get its input request.
    #[test]
    fn wait_next_surfaces_every_event_in_arrival_order() {
        let shared = Shared::default();
        shared.push(ME_TRANSFORM_HAVE_OUTPUT);
        shared.push(ME_TRANSFORM_HAVE_OUTPUT);
        shared.push(ME_TRANSFORM_NEED_INPUT);

        assert_eq!(shared.wait_next(NONE), Some(ME_TRANSFORM_HAVE_OUTPUT));
        assert_eq!(shared.wait_next(NONE), Some(ME_TRANSFORM_HAVE_OUTPUT));
        assert_eq!(shared.wait_next(NONE), Some(ME_TRANSFORM_NEED_INPUT));
        assert_eq!(shared.wait_next(NONE), None, "queue should be drained");
    }

    /// Ordering is not incidental. Consuming a `NeedInput` that arrived before
    /// output was collected would authorise a `ProcessInput` the transform is
    /// not ready for, answered with `MF_E_NOTACCEPTING`.
    #[test]
    fn wait_next_does_not_reorder_a_later_need_input_ahead_of_output() {
        let shared = Shared::default();
        shared.push(ME_TRANSFORM_HAVE_OUTPUT);
        shared.push(ME_TRANSFORM_NEED_INPUT);
        assert_eq!(shared.wait_next(NONE), Some(ME_TRANSFORM_HAVE_OUTPUT));
    }

    /// A zero timeout must still return an already-queued event: the encode
    /// path calls with whatever remains of its budget, which can be zero.
    #[test]
    fn zero_timeout_still_returns_queued_events() {
        let shared = Shared::default();
        shared.push(ME_TRANSFORM_NEED_INPUT);
        assert_eq!(shared.wait_next(NONE), Some(ME_TRANSFORM_NEED_INPUT));
        assert_eq!(shared.wait_next(NONE), None);
    }

    /// After a flush the transform sends no further `NeedInput` until the next
    /// `START_OF_STREAM`, so a surviving stale one would be acted on wrongly.
    #[test]
    fn clear_discards_pre_flush_events() {
        let shared = Shared::default();
        shared.push(ME_TRANSFORM_NEED_INPUT);
        shared.push(ME_TRANSFORM_HAVE_OUTPUT);
        shared.clear();
        assert_eq!(shared.wait_next(NONE), None);
    }

    /// `take` is order-insensitive by design — it asks "is there one of these
    /// at all", used when draining output the transform already signalled.
    #[test]
    fn take_finds_its_kind_behind_other_events() {
        let shared = Shared::default();
        shared.push(ME_TRANSFORM_NEED_INPUT);
        shared.push(ME_TRANSFORM_HAVE_OUTPUT);
        assert!(shared.take(ME_TRANSFORM_HAVE_OUTPUT));
        assert!(!shared.take(ME_TRANSFORM_HAVE_OUTPUT));
        assert_eq!(shared.wait_next(NONE), Some(ME_TRANSFORM_NEED_INPUT));
    }

    /// A waiter blocked on an empty queue must be woken by the MF worker
    /// thread's push, not left to time out.
    #[test]
    fn a_blocked_waiter_is_woken_by_a_later_event() {
        let shared = Arc::new(Shared::default());
        let writer = Arc::clone(&shared);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            writer.push(ME_TRANSFORM_NEED_INPUT);
        });
        // Generous ceiling: this asserts the wake happened, not how fast.
        assert_eq!(
            shared.wait_next(Duration::from_secs(5)),
            Some(ME_TRANSFORM_NEED_INPUT)
        );
        handle.join().expect("writer thread");
    }
}
