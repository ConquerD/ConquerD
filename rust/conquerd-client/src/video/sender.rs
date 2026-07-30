//! Capture → encode → transport loop.
//!
//! Runs on a dedicated OS thread, not a tokio task. Both halves of the work
//! here are blocking and CPU-bound — `IMFSourceReader::ReadSample` waits on the
//! camera, and the encoder call waits on the codec — so parking a tokio worker
//! on them would stall unrelated futures, the audio pipeline included.
//!
//! The thread owns its camera and encoder outright, which is also what their
//! COM thread-affinity requires: both are `Send` but not `Sync`, so they are
//! moved in and never shared.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use tracing::{debug, info, warn};

use super::camera::CameraSource;
#[cfg(target_os = "windows")]
use super::camera::MfCamera;
use super::codec::VideoEncoder;
use super::composite::Placement;
use super::frame::RawFrame;

/// Quality preset, mapped from the `video_quality` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality {
    pub width: u32,
    pub height: u32,
    pub bitrate_bps: u32,
    pub fps: u32,
}

impl Quality {
    /// Resolve a preset name. Unknown names fall back to balanced rather than
    /// failing, since this comes from a settings string.
    pub fn from_name(name: &str) -> Self {
        match name {
            "low" => Self {
                width: 320,
                height: 180,
                bitrate_bps: 250_000,
                fps: 20,
            },
            "high" => Self {
                width: 1280,
                height: 720,
                bitrate_bps: 1_500_000,
                fps: 30,
            },
            // "balanced" and anything unrecognised.
            _ => Self {
                width: 640,
                height: 360,
                bitrate_bps: 600_000,
                fps: 30,
            },
        }
    }
}

/// Where video frames come from.
///
/// Exactly one is live at a time: picking a screen stops the camera and vice
/// versa. That is a deliberate constraint of the current wire format — a
/// fragment identifies its stream only by sender peer id, so one peer cannot
/// have two concurrent video streams without a header change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceSpec {
    /// A camera, by device id. `None` means the system default.
    Camera { device_id: Option<String> },
    /// A monitor or window, identified by a
    /// [`screen::CaptureSource::id`](super::screen::CaptureSource::id).
    Screen { target_id: String },
}

impl SourceSpec {
    /// Interpret a settings `video_input_device` string.
    ///
    /// One settings field doubles as the source selector: a `monitor:` or
    /// `window:` prefix means screen capture, anything else is a camera device
    /// id, and an empty string is the system default camera. One field rather
    /// than two because exactly one source is ever live.
    pub fn from_device_id(raw: &str) -> Self {
        if raw.starts_with("monitor:") || raw.starts_with("window:") {
            Self::Screen {
                target_id: raw.to_owned(),
            }
        } else {
            Self::Camera {
                device_id: (!raw.is_empty()).then(|| raw.to_owned()),
            }
        }
    }

    /// Open the backing capture device at (at most) `width` x `height`.
    ///
    /// Runs on the capture thread because both backends are blocking to open —
    /// a camera can take a second or more to spin up — and both have COM thread
    /// affinity, so they must be created where they will be used. That is also
    /// why this takes a size rather than a [`Quality`]: an overlay opens at its
    /// own inset size, not the stream's.
    ///
    /// The size is a *request*. Screen capture honours it exactly (letterboxing
    /// into it), but a camera substitutes its nearest supported mode, so callers
    /// must read [`CameraSource::dimensions`] back rather than assume.
    #[cfg(target_os = "windows")]
    pub(super) fn open(&self, width: u32, height: u32) -> anyhow::Result<Box<dyn CameraSource>> {
        match self {
            Self::Camera { device_id } => {
                let cam = MfCamera::open(device_id.as_deref(), width, height)?;
                Ok(Box::new(cam))
            }
            Self::Screen { target_id } => {
                let target = super::screen::parse_target(target_id).ok_or_else(|| {
                    // A stale id is the expected failure here: Windows recycles
                    // window and monitor handles, so one saved in settings last
                    // session may now name nothing, or something else.
                    anyhow::anyhow!("capture source '{target_id}' is no longer available")
                })?;
                let cap = super::screen::ScreenCapture::open(&target, width, height)?;
                Ok(Box::new(cap))
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    pub(super) fn open(&self, _width: u32, _height: u32) -> anyhow::Result<Box<dyn CameraSource>> {
        anyhow::bail!("video capture is not implemented on this platform")
    }

    /// Label for logs, without leaking a full window title.
    fn kind(&self) -> &'static str {
        match self {
            Self::Camera { .. } => "camera",
            Self::Screen { .. } => "screen",
        }
    }
}

/// Everything that goes into the one outgoing video stream.
///
/// A peer can only *send* one stream — a fragment names its stream by sender
/// alone — so "camera and game at once" has to mean one frame containing both.
/// This is that description: a base source that fills the frame, plus insets
/// drawn on top of it. With no overlays it is exactly the single-source capture
/// that came before, and costs exactly as much.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CaptureLayout {
    /// The source that fills the frame.
    pub base: SourceSpec,
    /// Insets drawn over the base, in stacking order.
    pub overlays: Vec<(SourceSpec, Placement)>,
}

impl Default for SourceSpec {
    /// The default camera — what an empty `video_input_device` means.
    fn default() -> Self {
        Self::Camera { device_id: None }
    }
}

impl CaptureLayout {
    /// Build a layout from the two settings that describe it.
    ///
    /// `device_id` is `video_input_device` (the base) and `overlays_json` is
    /// `video_overlays_json`. Both are user-editable, so this is total: anything
    /// unusable is dropped and the capture still starts.
    ///
    /// Resolves what the empty "default camera" selection actually opens before
    /// deduplicating, since that is the case the raw ids cannot catch — see
    /// [`from_settings_with_default`](Self::from_settings_with_default). The
    /// enumeration is skipped unless it can change the answer.
    pub fn from_settings(device_id: &str, overlays_json: &str) -> Self {
        let overlays = super::composite::parse_overlays(overlays_json);
        let default_id = if device_id.trim().is_empty() && !overlays.is_empty() {
            super::camera::default_device_id()
        } else {
            String::new()
        };
        Self::build(device_id, overlays, &default_id)
    }

    /// [`from_settings`](Self::from_settings) with the default camera supplied
    /// rather than enumerated, so the rule can be tested without a device.
    ///
    /// Ids are deduplicated, and an overlay naming the base is discarded. That
    /// is not tidiness — a camera cannot be opened twice, so the duplicate would
    /// fail to open, log, and leave a hole in the picture for no reason.
    ///
    /// `default_camera_id` is what an empty `device_id` resolves to. Comparing
    /// raw ids alone misses exactly the layout users end up with: base left on
    /// "Default camera" and the same webcam added by id as an overlay. The
    /// duplicate then reached Media Foundation, which cannot open one device
    /// twice and reports it as a *format* failure ("offers neither NV12 nor
    /// YUY2") — an error that reads like a broken webcam rather than a layout
    /// that was never openable.
    pub fn from_settings_with_default(
        device_id: &str,
        overlays_json: &str,
        default_camera_id: &str,
    ) -> Self {
        Self::build(
            device_id,
            super::composite::parse_overlays(overlays_json),
            default_camera_id,
        )
    }

    fn build(
        device_id: &str,
        configs: Vec<super::composite::OverlayConfig>,
        default_camera_id: &str,
    ) -> Self {
        let base_id = device_id.trim();
        let base = SourceSpec::from_device_id(base_id);

        let mut seen = vec![base_id.to_owned()];
        // Both spellings of the base are claimed: the stored one and the device
        // it resolves to. Either can be what an overlay names.
        if base_id.is_empty() {
            let resolved = default_camera_id.trim();
            if !resolved.is_empty() {
                seen.push(resolved.to_owned());
            }
        }

        let overlays = configs
            .into_iter()
            .filter_map(|cfg| {
                let id = cfg.id.trim().to_owned();
                if seen.contains(&id) {
                    debug!("[video] skipping overlay '{id}': already in the layout");
                    return None;
                }
                seen.push(id.clone());
                Some((SourceSpec::from_device_id(&id), cfg.placement()))
            })
            .collect();

        Self { base, overlays }
    }

    /// A single-source layout, for callers that have no overlays to add.
    pub fn single(base: SourceSpec) -> Self {
        Self {
            base,
            overlays: Vec::new(),
        }
    }

    /// Whether more than one source is involved.
    pub fn is_composite(&self) -> bool {
        !self.overlays.is_empty()
    }

    /// Open every source and return one capture that produces frames of exactly
    /// `quality.width` x `quality.height`.
    ///
    /// The size guarantee is the point of doing this here: the encoder is
    /// configured once and rejects every frame that does not match, and a camera
    /// is free to hand back its nearest supported mode instead of the one that
    /// was asked for.
    fn open(&self, quality: Quality) -> anyhow::Result<Box<dyn CameraSource>> {
        let base = self.base.open(quality.width, quality.height)?;
        if !self.is_composite() {
            return Ok(super::composite::NormalizedSource::wrap(
                base,
                quality.width,
                quality.height,
            ));
        }
        Ok(Box::new(super::composite::CompositeSource::new(
            base,
            self.overlays.clone(),
            quality.width,
            quality.height,
            quality.fps,
        )))
    }

    /// Label for logs, without leaking a full window title.
    fn kind(&self) -> String {
        if self.is_composite() {
            format!("{} + {} overlay(s)", self.base.kind(), self.overlays.len())
        } else {
            self.base.kind().to_owned()
        }
    }
}

/// What the capture thread does with each encoded frame.
///
/// A trait rather than a channel type so the loop can be exercised in tests
/// without a live connection manager.
pub trait FrameSink: Send {
    /// Hand off one encoded frame. `keyframe` rides the fragment header.
    fn send(&mut self, encoded: Vec<u8>, keyframe: bool);
}

/// Floor for adaptive bitrate control.
///
/// Far above the audio floor (16 kbps) because video degrades differently:
/// below roughly this rate H.264 stops being a picture and becomes a slideshow
/// of blocks, at which point the stream costs bandwidth without conveying
/// anything. Better to hold here and let frames drop.
pub const MIN_VIDEO_BITRATE_BPS: u32 = 120_000;

/// Loss above which the encoder backs off, in percent.
const ABR_BACKOFF_LOSS_PCT: f32 = 10.0;
/// Loss below which the encoder may climb again.
const ABR_RECOVER_LOSS_PCT: f32 = 4.0;

/// Handle to a running capture thread.
///
/// Dropping this stops the thread and releases the camera — which is what turns
/// the hardware capture light off, so it matters that it is not leaked.
pub struct VideoSender {
    stop: Arc<AtomicBool>,
    keyframe_requested: Arc<AtomicBool>,
    frames_sent: Arc<AtomicU32>,
    /// Target bitrate published to the capture thread.
    ///
    /// An atomic rather than a channel because the capture thread reads it once
    /// per frame and only ever needs the newest value — a queue of superseded
    /// bitrates would be worse than useless on a link that is already behind.
    target_bitrate_bps: Arc<AtomicU32>,
    /// The user's quality preset, which adaptation may drop below but never
    /// exceed. Raising past what the user asked for would silently spend their
    /// bandwidth on quality they declined.
    ceiling_bps: u32,
    /// Currently published rate, so a no-op adaptation writes nothing.
    current_bps: u32,
    /// Smoothed loss. Raw per-tick loss is far too noisy to steer on: a single
    /// spike would halve the bitrate and a single clean tick would undo it.
    loss_ema: f32,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl VideoSender {
    /// Start capturing `layout` and feeding `sink`.
    ///
    /// Returns immediately; the sources are opened on the new thread because
    /// opening is itself blocking and can take a second or more.
    /// `preview_peer_id` is our own peer id, so the captured frame can be shown
    /// locally. `None` disables self-preview.
    pub fn start<S, E>(
        layout: CaptureLayout,
        quality: Quality,
        mut encoder: E,
        mut sink: S,
        preview_peer_id: Option<String>,
    ) -> Self
    where
        S: FrameSink + 'static,
        E: VideoEncoder + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe_requested = Arc::new(AtomicBool::new(false));
        let frames_sent = Arc::new(AtomicU32::new(0));
        let target_bitrate_bps = Arc::new(AtomicU32::new(quality.bitrate_bps));

        let stop_t = Arc::clone(&stop);
        let kf_t = Arc::clone(&keyframe_requested);
        let count_t = Arc::clone(&frames_sent);
        let bitrate_t = Arc::clone(&target_bitrate_bps);
        let preview_id = preview_peer_id.filter(|s| !s.is_empty());

        let handle = std::thread::Builder::new()
            .name("conquerd-video-capture".into())
            .spawn(move || {
                let kind = layout.kind();
                let mut camera = match layout.open(quality) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("[video] could not open {kind} source: {e}");
                        return;
                    }
                };
                let (cw, ch) = camera.dimensions();
                info!("[video] {kind} capture started at {cw}x{ch}");

                // Pace to the target frame rate. Without this the loop runs as
                // fast as the camera delivers, which wastes CPU and overshoots
                // the encoder's rate-control assumptions.
                let frame_interval =
                    std::time::Duration::from_micros(1_000_000 / quality.fps.max(1) as u64);
                let mut consecutive_errors = 0u32;
                let mut applied_bitrate = quality.bitrate_bps;

                while !stop_t.load(Ordering::Relaxed) {
                    let started = std::time::Instant::now();

                    // Apply adaptation on the capture thread, which is the only
                    // thread allowed to touch the encoder — MFTs have thread
                    // affinity, so the ABR caller cannot do this itself.
                    let want_bitrate = bitrate_t.load(Ordering::Relaxed);
                    if want_bitrate != applied_bitrate {
                        if let Err(e) = encoder.set_bitrate(want_bitrate) {
                            debug!("[video] bitrate {want_bitrate} rejected: {e}");
                        }
                        // Recorded either way. Re-attempting a value the encoder
                        // already refused would retry 30 times a second and log
                        // just as often, for a request that cannot start working.
                        applied_bitrate = want_bitrate;
                    }

                    let frame = match camera.next_frame() {
                        Ok(f) => {
                            consecutive_errors = 0;
                            f
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            // The first reads routinely fail while the device
                            // spins up, so tolerate a burst before giving up.
                            if consecutive_errors > 120 {
                                warn!("[video] camera failed {consecutive_errors} times, stopping: {e}");
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                    };

                    // Local self-preview, from the raw frame before encoding.
                    //
                    // Our own frames are sent to the network and never come
                    // back, so without this the local tile has no source at all
                    // and renders empty however the UI is wired. Taking the
                    // pre-encode frame is also strictly better than looping our
                    // own stream back through the codec: no added latency, no
                    // second decoder, and it shows what the camera actually
                    // sees rather than what compression made of it.
                    if let Some(id) = preview_id.as_deref() {
                        // Skip when no tile is bound — the common case, since
                        // self-preview only exists while expanded or popped out.
                        if super::sink::has_sink(id) {
                            super::sink::push_frame(id, &frame);
                        }
                    }

                    if kf_t.swap(false, Ordering::Relaxed) {
                        encoder.request_keyframe();
                    }

                    match encoder.encode(&frame) {
                        Ok((data, keyframe)) if !data.is_empty() => {
                            count_t.fetch_add(1, Ordering::Relaxed);
                            sink.send(data, keyframe);
                        }
                        // Empty output is normal while a pipelined encoder
                        // fills; it is not an error and must not be logged per
                        // frame at 30 fps.
                        Ok(_) => {}
                        Err(e) => debug!("[video] encode failed: {e}"),
                    }

                    if let Some(rest) = frame_interval.checked_sub(started.elapsed()) {
                        std::thread::sleep(rest);
                    }
                }
                info!("[video] {kind} capture stopped");
            })
            .ok();

        Self {
            stop,
            keyframe_requested,
            frames_sent,
            target_bitrate_bps,
            ceiling_bps: quality.bitrate_bps,
            current_bps: quality.bitrate_bps,
            loss_ema: 0.0,
            handle,
        }
    }

    /// Start a capture whose frames are only ever shown locally — the settings
    /// preview of the selected source.
    ///
    /// Nothing is encoded or transmitted: the preview draws the same raw
    /// pre-encode frame self-preview already uses, so no encoder is built and
    /// no session needs to exist. The capture loop itself is shared with
    /// [`start`](Self::start) rather than copied, because the fiddly parts —
    /// opening the source, tolerating the burst of read errors every device
    /// emits while spinning up, pacing to the preset, and releasing the device
    /// on stop — are exactly the parts a second copy would get wrong. Getting
    /// the last one wrong leaves the hardware capture light on.
    pub fn start_preview(layout: CaptureLayout, quality: Quality, preview_peer_id: String) -> Self {
        Self::start(
            layout,
            quality,
            DiscardEncoder,
            DiscardSink,
            Some(preview_peer_id),
        )
    }

    /// Feed a transport loss measurement into adaptive bitrate control.
    ///
    /// Called on the same connection-stats tick that drives audio ABR, so both
    /// media adapt from one measurement rather than each estimating separately.
    pub fn apply_network_quality(&mut self, loss_pct: f32) {
        self.loss_ema = update_video_loss_ema(self.loss_ema, loss_pct);
        let next = next_video_bitrate(self.current_bps, self.ceiling_bps, self.loss_ema);
        if next == self.current_bps {
            return;
        }
        self.current_bps = next;
        self.target_bitrate_bps.store(next, Ordering::Relaxed);
        debug!(
            "[video] ABR -> {next} bps (loss EMA {:.1}%, ceiling {})",
            self.loss_ema, self.ceiling_bps
        );
    }

    /// The rate adaptation has currently settled on, for diagnostics.
    pub fn current_bitrate_bps(&self) -> u32 {
        self.current_bps
    }

    /// Ask the encoder to emit a keyframe on its next frame, in response to a
    /// receiver's request.
    pub fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::Relaxed);
    }

    /// Frames successfully encoded and handed to the sink so far. Used to tell
    /// "camera is running" from "camera opened but produced nothing".
    pub fn frames_sent(&self) -> u32 {
        self.frames_sent.load(Ordering::Relaxed)
    }

    /// Signal the thread to stop and wait for the camera to be released.
    pub fn stop(mut self) {
        self.signal_and_join();
    }

    fn signal_and_join(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for VideoSender {
    fn drop(&mut self) {
        // Joining on drop is deliberate: letting the thread outlive this handle
        // would keep the camera (and its indicator light) on with nothing left
        // holding a reference to turn it off.
        self.signal_and_join();
    }
}

/// Smooth a raw loss reading into the value ABR steers on.
///
/// Asymmetric on purpose. Loss is weighted in quickly (0.6 of the new reading)
/// because congestion that is not reacted to compounds — the queue keeps
/// growing. A clean tick decays the estimate only halfway, so one lucky sample
/// cannot undo a genuine congestion signal and set off an oscillation between
/// full rate and backed-off rate.
fn update_video_loss_ema(current_ema: f32, loss_pct: f32) -> f32 {
    let loss = loss_pct.clamp(0.0, 100.0);
    if loss > current_ema {
        current_ema * 0.4 + loss * 0.6
    } else {
        (current_ema * 0.5 + loss * 0.5).max(0.0)
    }
}

/// Pure ABR decision: given the live rate, the user's ceiling and smoothed
/// loss, return the next bitrate.
///
/// AIMD, mirroring [`next_bitrate`](crate::call_controller) for audio: back off
/// multiplicatively (−25%) under sustained loss, hold in the uncertain band,
/// and climb gently (+10%) only when the link looks genuinely clear. The
/// asymmetry is the point — backing off late costs a frozen picture, while
/// climbing too eagerly re-creates the congestion just escaped.
///
/// Video backs off harder than audio (−25% vs −20%) because it is the larger
/// flow: it is what is actually causing the congestion, and shedding it is what
/// protects the audio sharing the link. Audio staying intelligible while video
/// degrades is the right trade in a call.
fn next_video_bitrate(current: u32, ceiling: u32, loss_pct: f32) -> u32 {
    let target = if loss_pct > ABR_BACKOFF_LOSS_PCT {
        (current as f32 * 0.75) as u32
    } else if loss_pct > ABR_RECOVER_LOSS_PCT {
        current
    } else {
        (current as f32 * 1.10) as u32 + 10_000
    };
    // The ceiling can legitimately sit below the floor on the "low" preset, so
    // the floor must not be allowed to raise the rate past what the user chose.
    let floor = MIN_VIDEO_BITRATE_BPS.min(ceiling);
    target.clamp(floor, ceiling.max(floor))
}

/// Encoder for a preview-only capture.
///
/// Returns no bytes, which the capture loop already handles as "nothing ready
/// this tick" (the normal state of a pipelined encoder that is still filling),
/// so no frame ever reaches the sink. Skipping the encode outright is the point:
/// a preview costs one camera and nothing else, and can therefore run without a
/// hardware encoder — which is also what keeps it available on machines where
/// [`MfEncoder`](super::mediafoundation::MfEncoder) creation fails.
struct DiscardEncoder;

impl VideoEncoder for DiscardEncoder {
    fn encode(&mut self, _frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)> {
        Ok((Vec::new(), false))
    }

    fn request_keyframe(&mut self) {}
}

/// Sink for a preview-only capture. Unreachable in practice — [`DiscardEncoder`]
/// never produces a frame to hand over — but the loop is generic over a sink, so
/// it needs one.
struct DiscardSink;

impl FrameSink for DiscardSink {
    fn send(&mut self, _encoded: Vec<u8>, _keyframe: bool) {}
}

/// A [`FrameSink`] that forwards to a channel, for wiring into the connection
/// manager without this module depending on it.
pub struct ChannelSink<T> {
    tx: tokio::sync::mpsc::Sender<T>,
    make: Box<dyn Fn(Vec<u8>, bool) -> T + Send>,
    dropped: u32,
}

impl<T: Send> ChannelSink<T> {
    /// Wrap `tx`, using `make` to build the message for each frame.
    pub fn new(
        tx: tokio::sync::mpsc::Sender<T>,
        make: impl Fn(Vec<u8>, bool) -> T + Send + 'static,
    ) -> Self {
        Self {
            tx,
            make: Box::new(make),
            dropped: 0,
        }
    }
}

impl<T: Send> FrameSink for ChannelSink<T> {
    fn send(&mut self, encoded: Vec<u8>, keyframe: bool) {
        // try_send, never blocking: stalling the capture thread on a full
        // channel would back pressure into the camera and stutter the frame
        // clock. Dropping the newest frame is the correct real-time choice.
        if self.tx.try_send((self.make)(encoded, keyframe)).is_err() {
            self.dropped = self.dropped.saturating_add(1);
            // Log sparsely — at 30 fps a per-frame warning would flood.
            if self.dropped % 60 == 1 {
                debug!(
                    "[video] outbound queue full, dropped {} frames",
                    self.dropped
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_presets_are_even_dimensioned() {
        // 4:2:0 chroma requires even dimensions; an odd preset would be
        // rejected by the encoder at runtime instead of here.
        for name in ["low", "balanced", "high", "nonsense"] {
            let q = Quality::from_name(name);
            assert_eq!(q.width % 2, 0, "{name} width must be even");
            assert_eq!(q.height % 2, 0, "{name} height must be even");
            assert!(q.fps > 0 && q.bitrate_bps > 0, "{name} must be non-zero");
        }
    }

    #[test]
    fn unknown_quality_falls_back_to_balanced() {
        assert_eq!(Quality::from_name("wat"), Quality::from_name("balanced"));
        assert_eq!(Quality::from_name("").width, 640);
    }

    #[test]
    fn presets_are_ordered_by_cost() {
        let low = Quality::from_name("low");
        let bal = Quality::from_name("balanced");
        let high = Quality::from_name("high");
        assert!(low.width < bal.width && bal.width < high.width);
        assert!(low.bitrate_bps < bal.bitrate_bps && bal.bitrate_bps < high.bitrate_bps);
    }

    /// A sink that records what it was handed.
    struct RecordingSink(Arc<std::sync::Mutex<Vec<(usize, bool)>>>);

    impl FrameSink for RecordingSink {
        fn send(&mut self, encoded: Vec<u8>, keyframe: bool) {
            self.0.lock().unwrap().push((encoded.len(), keyframe));
        }
    }

    /// An encoder that emits a fixed payload, so the loop can be tested with no
    /// codec or camera involved.
    struct FakeEncoder {
        keyframe_next: bool,
    }

    impl VideoEncoder for FakeEncoder {
        fn encode(&mut self, _frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)> {
            let kf = self.keyframe_next;
            self.keyframe_next = false;
            Ok((vec![1, 2, 3], kf))
        }

        fn request_keyframe(&mut self) {
            self.keyframe_next = true;
        }
    }

    #[test]
    fn a_device_id_selects_the_matching_source_kind() {
        assert_eq!(
            SourceSpec::from_device_id("monitor:0"),
            SourceSpec::Screen {
                target_id: "monitor:0".into()
            }
        );
        assert_eq!(
            SourceSpec::from_device_id("window:1234"),
            SourceSpec::Screen {
                target_id: "window:1234".into()
            }
        );
        assert_eq!(
            SourceSpec::from_device_id(r"\\?\usb#vid_046d"),
            SourceSpec::Camera {
                device_id: Some(r"\\?\usb#vid_046d".into())
            }
        );
    }

    /// The settings UI stores "" for "first available camera", so an empty id
    /// must reach the backend as the default rather than as a device named "".
    #[test]
    fn an_empty_device_id_means_the_default_camera() {
        assert_eq!(
            SourceSpec::from_device_id(""),
            SourceSpec::Camera { device_id: None }
        );
    }

    // ── Layouts ─────────────────────────────────────────────────────────────

    #[test]
    fn a_layout_with_no_overlay_blob_is_single_source() {
        for blob in ["", "[]", "garbage"] {
            let layout = CaptureLayout::from_settings("monitor:1", blob);
            assert!(!layout.is_composite(), "{blob:?} should stay single-source");
            assert_eq!(
                layout.base,
                SourceSpec::Screen {
                    target_id: "monitor:1".into()
                }
            );
        }
    }

    #[test]
    fn a_layout_pairs_each_overlay_with_its_placement() {
        let layout = CaptureLayout::from_settings(
            "monitor:1",
            r#"[{"id":"cam-a","corner":"top-left","size":30}]"#,
        );
        assert!(layout.is_composite());
        assert_eq!(
            layout.overlays,
            vec![(
                SourceSpec::Camera {
                    device_id: Some("cam-a".into())
                },
                Placement {
                    corner: crate::video::composite::Corner::TopLeft,
                    size_pct: 30
                }
            )]
        );
    }

    /// A device cannot be opened twice, so an overlay naming the base — or
    /// naming another overlay — would fail to open and leave a hole in the
    /// picture. Dropping it up front is the difference between a clean layout
    /// and a warning every session.
    #[test]
    fn a_layout_never_opens_the_same_source_twice() {
        let layout = CaptureLayout::from_settings(
            "cam-a",
            r#"[{"id":"cam-a"},{"id":"cam-b"},{"id":"cam-b"}]"#,
        );
        assert_eq!(layout.overlays.len(), 1);
        assert_eq!(
            layout.overlays[0].0,
            SourceSpec::Camera {
                device_id: Some("cam-b".into())
            }
        );
    }

    /// The default camera is the empty id, so an overlay with a blank id would
    /// silently become "the default camera" — which is very often the base.
    #[test]
    fn a_blank_overlay_id_does_not_become_the_default_camera() {
        let layout = CaptureLayout::from_settings("", r#"[{"id":""},{"id":"   "}]"#);
        assert!(!layout.is_composite());
    }

    /// The collision raw ids cannot see: the base is left on "Default camera"
    /// (stored as `""`) and the same webcam is added by id as an overlay. This
    /// reached the capture thread as a second open of one device, which Media
    /// Foundation reports as a format failure — so the overlay vanished from the
    /// stream while the settings page still listed it.
    #[test]
    fn an_overlay_naming_the_default_camera_is_not_opened_twice() {
        let layout = CaptureLayout::from_settings_with_default(
            "",
            r#"[{"id":"usb#vid_046d"},{"id":"window:42"}]"#,
            "usb#vid_046d",
        );
        assert_eq!(
            layout.overlays.len(),
            1,
            "the overlay naming the resolved base must be dropped"
        );
        assert_eq!(
            layout.overlays[0].0,
            SourceSpec::Screen {
                target_id: "window:42".into()
            }
        );
    }

    /// The same device named explicitly on both sides is already caught, and
    /// must stay caught now that the empty case is resolved separately.
    #[test]
    fn an_overlay_naming_an_explicit_base_is_still_dropped() {
        let layout = CaptureLayout::from_settings_with_default(
            "usb#vid_046d",
            r#"[{"id":"usb#vid_046d"}]"#,
            // A different default, which must not be consulted at all when the
            // base is explicit — resolving it there would drop a legitimate
            // overlay of the first camera.
            "usb#other",
        );
        assert!(!layout.is_composite());

        let kept = CaptureLayout::from_settings_with_default(
            "usb#vid_046d",
            r#"[{"id":"usb#other"}]"#,
            "usb#other",
        );
        assert!(
            kept.is_composite(),
            "the first camera is a valid overlay when the base is some other device"
        );
    }

    /// With no camera attached there is nothing for the empty base to resolve
    /// to, and an empty id must not then match every overlay.
    #[test]
    fn an_unresolvable_default_camera_drops_nothing() {
        let layout =
            CaptureLayout::from_settings_with_default("", r#"[{"id":"window:42"}]"#, "   ");
        assert!(layout.is_composite());
    }

    #[test]
    fn layout_labels_say_how_many_sources_are_involved() {
        assert_eq!(CaptureLayout::from_settings("", "").kind(), "camera");
        assert_eq!(
            CaptureLayout::from_settings("monitor:1", r#"[{"id":"cam-a"}]"#).kind(),
            "screen + 1 overlay(s)"
        );
    }

    /// The preview path leans on this: an encoder that yields nothing must look
    /// like a pipelined encoder still filling, not like an error, so the loop
    /// keeps capturing (and keeps feeding the preview surface) indefinitely.
    #[test]
    fn the_preview_encoder_produces_nothing_to_send() {
        let (data, keyframe) = DiscardEncoder.encode(&RawFrame::black(64, 48)).unwrap();
        assert!(
            data.is_empty(),
            "a preview must never queue a frame to send"
        );
        assert!(!keyframe);
    }

    #[test]
    fn channel_sink_drops_rather_than_blocking_when_full() {
        // Capacity 1, then push 5 frames. A blocking sink would deadlock the
        // capture thread; this must drop instead.
        let (tx, _rx) = tokio::sync::mpsc::channel::<(Vec<u8>, bool)>(1);
        let mut sink = ChannelSink::new(tx, |d, k| (d, k));
        for _ in 0..5 {
            sink.send(vec![0u8; 8], false);
        }
        assert!(sink.dropped >= 3, "expected drops, saw {}", sink.dropped);
    }

    // ── Adaptive bitrate ────────────────────────────────────────────────────

    const BALANCED: u32 = 600_000;

    #[test]
    fn sustained_loss_backs_the_bitrate_off() {
        let mut bps = BALANCED;
        for _ in 0..6 {
            bps = next_video_bitrate(bps, BALANCED, 30.0);
        }
        assert!(
            bps < BALANCED / 2,
            "heavy loss should more than halve the rate, got {bps}"
        );
    }

    #[test]
    fn a_clear_link_recovers_to_the_ceiling_but_never_past_it() {
        let mut bps = MIN_VIDEO_BITRATE_BPS;
        for _ in 0..100 {
            bps = next_video_bitrate(bps, BALANCED, 0.0);
        }
        assert_eq!(
            bps, BALANCED,
            "recovery must converge on the user's chosen quality, not exceed it"
        );
    }

    #[test]
    fn the_mid_band_holds_rather_than_oscillating() {
        let bps = next_video_bitrate(400_000, BALANCED, 7.0);
        assert_eq!(bps, 400_000, "the uncertain band must not move the rate");
    }

    /// Backing off must stop somewhere watchable rather than shrinking toward
    /// zero — a stream too small to render is bandwidth spent for nothing.
    #[test]
    fn backoff_stops_at_the_floor() {
        let mut bps = BALANCED;
        for _ in 0..200 {
            bps = next_video_bitrate(bps, BALANCED, 100.0);
        }
        assert_eq!(bps, MIN_VIDEO_BITRATE_BPS);
    }

    /// No shipping preset sits below the floor today (the lowest is 250 kbps),
    /// but the clamp must not be able to raise a rate *above* a ceiling — that
    /// would spend bandwidth the user explicitly declined. Tested directly
    /// rather than through a preset so it stays honest if presets change.
    #[test]
    fn a_ceiling_below_the_floor_is_still_respected() {
        let tiny = MIN_VIDEO_BITRATE_BPS / 2;
        for loss in [0.0, 7.0, 50.0] {
            assert_eq!(
                next_video_bitrate(tiny, tiny, loss),
                tiny,
                "at {loss}% loss a sub-floor ceiling must still cap the rate"
            );
        }
    }

    /// Every shipping preset must be adaptable — a preset pinned at the floor
    /// could never back off, which would defeat ABR on that quality setting.
    #[test]
    fn every_preset_has_room_to_adapt_downward() {
        for name in ["low", "balanced", "high"] {
            let ceiling = Quality::from_name(name).bitrate_bps;
            assert!(
                ceiling > MIN_VIDEO_BITRATE_BPS,
                "{name} preset ({ceiling}) must sit above the ABR floor"
            );
            assert!(
                next_video_bitrate(ceiling, ceiling, 50.0) < ceiling,
                "{name} preset must be able to back off"
            );
        }
    }

    #[test]
    fn loss_ema_reacts_faster_to_congestion_than_to_recovery() {
        // One bad reading from clean should move most of the way up...
        let spiked = update_video_loss_ema(0.0, 20.0);
        // ...while one clean reading from bad should only move halfway down.
        let relaxed = update_video_loss_ema(20.0, 0.0);
        assert!(
            spiked > 20.0 - relaxed,
            "congestion must be weighted in faster than it is forgotten \
             (rise {spiked:.1} vs fall to {relaxed:.1})"
        );
    }

    #[test]
    fn a_single_clean_tick_does_not_erase_a_congestion_signal() {
        let mut ema = 0.0;
        for _ in 0..4 {
            ema = update_video_loss_ema(ema, 25.0);
        }
        let after_one_clean = update_video_loss_ema(ema, 0.0);
        assert!(
            after_one_clean > ABR_RECOVER_LOSS_PCT,
            "one clean sample must not immediately re-authorise climbing \
             (EMA fell to {after_one_clean:.1})"
        );
    }

    #[test]
    fn loss_ema_ignores_out_of_range_readings() {
        // Transport stats are external input; a negative or >100 value must not
        // poison the estimator into never adapting again.
        assert!((0.0..=100.0).contains(&update_video_loss_ema(0.0, -5.0)));
        assert!((0.0..=100.0).contains(&update_video_loss_ema(0.0, 1e9)));
    }

    #[test]
    fn recording_sink_receives_what_the_encoder_produced() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut sink = RecordingSink(Arc::clone(&log));
        let mut enc = FakeEncoder {
            keyframe_next: true,
        };
        let frame = RawFrame::black(64, 48);

        let (data, kf) = enc.encode(&frame).unwrap();
        sink.send(data, kf);
        assert_eq!(log.lock().unwrap().as_slice(), &[(3, true)]);
    }
}
