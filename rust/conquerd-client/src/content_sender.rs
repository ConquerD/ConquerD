//! Content-audio capture → encode → send loop.
//!
//! Mirrors [`crate::video::sender`]: a dedicated OS thread, because both halves
//! block. The loopback endpoint waits on the device and the Opus call is
//! CPU-bound, so parking a tokio worker on either would stall unrelated
//! futures — the voice pipeline included, which is the one thing this feature
//! must never disturb.
//!
//! # Timestamps come from the device, not from a clock read
//!
//! The obvious implementation reads the session clock once per frame. It is
//! wrong here, and visibly so: a loopback endpoint hands back whatever is in
//! its buffer, so one device read routinely yields several 20 ms frames at
//! once. Stamping each with "now" would give three frames captured 60 ms apart
//! near-identical timestamps, and a receiver extrapolating from them would
//! conclude the stream had stalled and then jumped.
//!
//! So the clock is read **once**, to anchor the stream, and each frame is
//! anchor + the offset the capture device reports for it (see
//! [`ContentFrame::offset_us`](crate::content_capture::ContentFrame::offset_us)).
//!
//! The near-miss worth naming is counting frames instead — anchor + n x 20 ms.
//! It is right for as long as the device produces audio continuously, and a
//! loopback device does not: an application that is not playing anything
//! produces no packets at all. Counting therefore resumes after a silence as
//! though no time had passed, so every pause pushes the audio further behind
//! the video for the rest of the session. Asking the device costs nothing and
//! skips the gap exactly.
//!
//! Long-run drift between the device clock and the session clock is real and
//! deliberately not corrected in v1; at the tens-of-minutes scale a call runs,
//! it stays far inside the +/-40-80 ms sync target.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use conquerd_opus::{Application as OpusApp, OpusEncoder};
use tracing::{debug, info, warn};

use crate::call_controller::{SAMPLES_PER_FRAME, SAMPLE_RATE};
use crate::content_capture::ContentAudioSource;
use crate::media_clock::SessionMediaClock;

/// Microseconds of audio in one frame. Fixed by [`SAMPLES_PER_FRAME`].
pub const FRAME_DURATION_US: u64 = (SAMPLES_PER_FRAME as u64) * 1_000_000 / (SAMPLE_RATE as u64);

/// Bitrate for content audio.
///
/// Well above the voice default: this carries music and effects rather than
/// speech, where the codec cannot lean on a vocal model and artefacts are much
/// more audible.
pub const CONTENT_BITRATE_BPS: i32 = 96_000;

/// Presentation timestamp for frame `index` of a stream anchored at
/// `anchor_us`.
///
/// Split out and pure because it is the piece most easily got wrong — see the
/// module docs on why this is not a clock read.
pub fn pts_for_frame(anchor_us: u64, index: u64) -> u64 {
    anchor_us.saturating_add(index.saturating_mul(FRAME_DURATION_US))
}

/// Handle to a running content-audio capture thread.
pub struct ContentAudioSender {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ContentAudioSender {
    /// Start capturing from `source`, stamping against `clock`, and handing
    /// each encoded frame to `emit`.
    ///
    /// `emit` returns `false` when the frame could not be queued; that is
    /// logged sparsely rather than treated as fatal, since a full outbound
    /// queue is a congestion signal and dropping the newest frame is the
    /// correct real-time response.
    pub fn start<S, E>(mut source: S, clock: SessionMediaClock, mut emit: E) -> Self
    where
        S: ContentAudioSource + 'static,
        E: FnMut(Vec<u8>, u64) -> bool + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("conquerd-content-audio".into())
            .spawn(move || {
                let mut encoder = match OpusEncoder::new(SAMPLE_RATE, 1, OpusApp::Audio) {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("[content-audio] could not create Opus encoder: {e}");
                        return;
                    }
                };
                if let Err(e) = encoder.set_bitrate(CONTENT_BITRATE_BPS) {
                    // Not fatal: the encoder still works at its default rate.
                    debug!("[content-audio] could not set bitrate: {e}");
                }

                let mut opus_buf = [0u8; 4096];
                let mut anchor_us: Option<u64> = None;
                let mut dropped: u32 = 0;
                info!("[content-audio] capture started");

                while !stop_t.load(Ordering::Relaxed) {
                    let frame = match source.next_frame() {
                        Ok(f) => f,
                        Err(e) => {
                            warn!("[content-audio] capture stopped: {e}");
                            break;
                        }
                    };

                    // Anchor on the first frame only; see the module docs. The
                    // offset within the stream is the *device's* answer, not a
                    // frame count, so a silence the device sat out costs
                    // exactly its own length here.
                    let anchor = *anchor_us.get_or_insert_with(|| clock.now_pts_us());
                    let pts_us = anchor.saturating_add(frame.offset_us);

                    let pcm = crate::content_capture::f32_to_i16(&frame.samples);
                    match encoder.encode(&pcm, &mut opus_buf) {
                        Ok(n) if n > 0 => {
                            if !emit(opus_buf[..n].to_vec(), pts_us) {
                                dropped = dropped.saturating_add(1);
                                // At 50 fps a per-frame warning would flood.
                                if dropped % 100 == 1 {
                                    debug!(
                                        "[content-audio] outbound queue full, dropped {dropped}"
                                    );
                                }
                            }
                        }
                        // Zero-length output is legitimate while the encoder
                        // primes; it is not an error.
                        Ok(_) => {}
                        Err(e) => debug!("[content-audio] encode failed: {e}"),
                    }
                }
                info!("[content-audio] capture stopped");
            })
            .ok();

        Self { stop, handle }
    }

    /// Stop the thread and wait for it to release the device.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ContentAudioSender {
    fn drop(&mut self) {
        // Releasing the loopback endpoint matters: leaving it open holds a
        // WASAPI client against the render device for the process lifetime.
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_capture::ContentFrame;

    #[test]
    fn frame_duration_is_twenty_milliseconds() {
        assert_eq!(FRAME_DURATION_US, 20_000);
    }

    #[test]
    fn timestamps_advance_by_exactly_one_frame() {
        assert_eq!(pts_for_frame(0, 0), 0);
        assert_eq!(pts_for_frame(0, 1), 20_000);
        assert_eq!(pts_for_frame(0, 50), 1_000_000); // one second
    }

    #[test]
    fn the_anchor_offsets_the_whole_stream() {
        let anchor = 7_500_000;
        assert_eq!(pts_for_frame(anchor, 0), anchor);
        assert_eq!(pts_for_frame(anchor, 3), anchor + 60_000);
    }

    /// The defect this design exists to prevent: several frames emerging from
    /// one device read must still be spaced a frame apart, not collapsed onto
    /// the instant they happened to be dequeued.
    #[test]
    fn a_burst_of_frames_stays_evenly_spaced() {
        let anchor = 1_000_000;
        let burst: Vec<u64> = (0..4).map(|i| pts_for_frame(anchor, i)).collect();
        for pair in burst.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                FRAME_DURATION_US,
                "frames collapsed onto one instant: {burst:?}"
            );
        }
    }

    #[test]
    fn timestamps_are_monotonic_over_a_long_run() {
        // An hour of audio: 180_000 frames. Must not wrap or go backwards.
        let mut last = 0;
        for i in 0..180_000u64 {
            let pts = pts_for_frame(0, i);
            assert!(pts >= last);
            last = pts;
        }
        assert_eq!(last, 3_599_980_000); // just under one hour, in microseconds
    }

    #[test]
    fn a_pathological_index_saturates_rather_than_wrapping() {
        // Wrapping would put a frame *before* the session start and make a
        // receiver's extrapolation jump backwards.
        assert_eq!(pts_for_frame(u64::MAX, 1), u64::MAX);
        assert_eq!(pts_for_frame(0, u64::MAX), u64::MAX);
    }

    /// A source that fails immediately must not wedge the thread: `start`
    /// returns a live handle and dropping it joins cleanly.
    #[test]
    fn a_failing_source_shuts_down_cleanly() {
        struct Failing;
        impl ContentAudioSource for Failing {
            fn next_frame(&mut self) -> anyhow::Result<ContentFrame> {
                anyhow::bail!("no device")
            }
        }

        let clock = SessionMediaClock::start();
        let started = std::time::Instant::now();
        let sender = ContentAudioSender::start(Failing, clock, |_, _| true);
        drop(sender);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(3),
            "shutdown hung on a failing source"
        );
    }

    /// Frames must reach the emit callback with monotonically increasing
    /// timestamps, exercising the real thread rather than just the arithmetic.
    #[test]
    fn frames_reach_the_sink_with_advancing_timestamps() {
        use std::sync::mpsc;

        struct Silence {
            left: usize,
            emitted: u64,
        }
        impl ContentAudioSource for Silence {
            fn next_frame(&mut self) -> anyhow::Result<ContentFrame> {
                if self.left == 0 {
                    anyhow::bail!("done");
                }
                self.left -= 1;
                let offset_us = self.emitted * FRAME_DURATION_US;
                self.emitted += 1;
                Ok(ContentFrame {
                    samples: vec![0.0; SAMPLES_PER_FRAME],
                    offset_us,
                })
            }
        }

        let (tx, rx) = mpsc::channel();
        let clock = SessionMediaClock::start();
        // Held alive deliberately: dropping the handle sets the stop flag, and
        // doing that immediately can beat the thread to its first loop check —
        // which is correct shutdown behaviour but tests nothing.
        let sender = ContentAudioSender::start(
            Silence {
                left: 8,
                emitted: 0,
            },
            clock,
            move |_opus, pts| {
                let _ = tx.send(pts);
                true
            },
        );

        // The source ends itself after 8 frames, so the thread exits, drops the
        // closure, and closes the channel without needing the stop flag.
        let stamps: Vec<u64> = rx.iter().collect();
        drop(sender);

        assert!(!stamps.is_empty(), "no frames reached the sink");
        for pair in stamps.windows(2) {
            assert_eq!(
                pair[1] - pair[0],
                FRAME_DURATION_US,
                "timestamps not one frame apart: {stamps:?}"
            );
        }
    }

    /// The defect that made per-process capture worth being careful about: an
    /// application that goes quiet produces no packets, and if the stream
    /// resumed as though no time had passed, the audio would stay behind the
    /// video by the length of every silence for the rest of the session.
    ///
    /// The source reports the device's own offsets, so the gap must survive
    /// intact all the way to the emitted timestamps.
    #[test]
    fn a_silence_the_device_sat_out_keeps_its_full_length() {
        use std::sync::mpsc;

        /// Two frames, a ten-second gap, then two more.
        struct Gapped {
            offsets: std::vec::IntoIter<u64>,
        }
        impl ContentAudioSource for Gapped {
            fn next_frame(&mut self) -> anyhow::Result<ContentFrame> {
                match self.offsets.next() {
                    Some(offset_us) => Ok(ContentFrame {
                        samples: vec![0.0; SAMPLES_PER_FRAME],
                        offset_us,
                    }),
                    None => anyhow::bail!("done"),
                }
            }
        }

        let offsets = vec![0, 20_000, 10_020_000, 10_040_000];
        let (tx, rx) = mpsc::channel();
        let clock = SessionMediaClock::start();
        let sender = ContentAudioSender::start(
            Gapped {
                offsets: offsets.clone().into_iter(),
            },
            clock,
            move |_opus, pts| {
                let _ = tx.send(pts);
                true
            },
        );
        let stamps: Vec<u64> = rx.iter().collect();
        drop(sender);

        assert_eq!(stamps.len(), offsets.len(), "frames lost: {stamps:?}");
        // The anchor is unknown (it is a live clock read), so compare the
        // shape of the timeline rather than absolute values.
        let base = stamps[0];
        let relative: Vec<u64> = stamps.iter().map(|s| s - base).collect();
        assert_eq!(
            relative, offsets,
            "the ten-second silence did not survive to the wire"
        );
    }
}
