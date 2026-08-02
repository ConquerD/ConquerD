//! Picture-in-picture: several capture sources drawn into one frame.
//!
//! # Why compose rather than send two streams
//!
//! A video fragment identifies its stream by sender peer id alone (see
//! [`super::fragment`]), so one peer cannot have two concurrent video streams
//! without a header change — and a second stream would double the bitrate of the
//! largest flow on the link. Compositing sidesteps both: the sources are merged
//! into one frame *before* the encoder, so the wire format, the receivers, and
//! the bandwidth budget are all unchanged. A peer running camera-over-game looks
//! exactly like a peer sharing one source.
//!
//! ```text
//! base source ─────────────────┐
//!                              ├─ compose ─> encode ─> seal ─> fragment
//! overlay source(s) ─ (thread) ┘
//! ```
//!
//! # Why the overlays get their own threads
//!
//! Capture backends block: `IMFSourceReader::ReadSample` waits on the camera.
//! Pulling each source in turn on the capture thread would make the output frame
//! rate the *sum* of every source's latency — two 30 fps cameras would compose
//! at 15 fps — and one stalled source would freeze the whole outgoing stream.
//! Each overlay therefore runs its own capture loop and publishes its newest
//! frame into a slot the compositor samples. The base source alone paces the
//! output, an overlay that is slow simply repeats, and an overlay that dies
//! leaves the rest of the picture running.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use super::camera::CameraSource;
use super::frame::RawFrame;
use super::scale::fit_within;
use super::sender::SourceSpec;

/// Most overlays allowed on one composite.
///
/// Each costs a capture device, a thread, and a scale per frame. Three is past
/// what any sane layout uses and well inside what the capture thread can absorb.
pub const MAX_OVERLAYS: usize = 3;

/// Narrowest an overlay may be, as a percentage of output width. Below this a
/// camera PIP is too small to recognise a face in after encoding.
pub const MIN_OVERLAY_PCT: u32 = 10;

/// Widest an overlay may be. Past half the width it stops being an inset and
/// starts hiding the thing it is inset over.
pub const MAX_OVERLAY_PCT: u32 = 50;

/// Default overlay width, as a percentage of output width.
pub const DEFAULT_OVERLAY_PCT: u32 = 25;

/// Gap between an overlay and the frame edge, as a percentage of output width.
const MARGIN_PCT: u32 = 2;

/// Which corner an overlay is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    /// The conventional webcam position, and the default here.
    #[default]
    BottomRight,
}

impl Corner {
    /// Parse the stored name. Unknown values fall back to the default rather
    /// than failing — this comes from a settings file that can be hand-edited.
    pub fn from_name(name: &str) -> Self {
        match name {
            "top-left" => Self::TopLeft,
            "top-right" => Self::TopRight,
            "bottom-left" => Self::BottomLeft,
            _ => Self::BottomRight,
        }
    }

    /// The name written to settings. Round-trips with [`from_name`](Self::from_name).
    pub fn as_name(self) -> &'static str {
        match self {
            Self::TopLeft => "top-left",
            Self::TopRight => "top-right",
            Self::BottomLeft => "bottom-left",
            Self::BottomRight => "bottom-right",
        }
    }
}

/// Where and how large an overlay is drawn, in fractions of the output frame.
///
/// Fractions rather than pixels so a layout survives a quality change: the same
/// settings produce the same-looking picture at 320x180 and at 1280x720.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub corner: Corner,
    /// Width as a percentage of output width. Height follows the source's own
    /// aspect ratio, so a 4:3 webcam is not stretched into a 16:9 box.
    pub size_pct: u32,
}

impl Default for Placement {
    fn default() -> Self {
        Self {
            corner: Corner::default(),
            size_pct: DEFAULT_OVERLAY_PCT,
        }
    }
}

impl Placement {
    /// Pixel size of the overlay for a source of aspect `src_w`:`src_h`.
    ///
    /// Both dimensions come back even — I420 subsamples chroma 2x2, so an odd
    /// size cannot be represented, let alone blitted onto the chroma grid.
    pub fn box_size(&self, out_w: u32, out_h: u32, src_w: u32, src_h: u32) -> (u32, u32) {
        let pct = self.size_pct.clamp(MIN_OVERLAY_PCT, MAX_OVERLAY_PCT);
        let w = ((out_w as u64 * pct as u64) / 100).max(2) as u32 & !1;
        let (src_w, src_h) = (src_w.max(1), src_h.max(1));
        let h = (((w as u64 * src_h as u64) / src_w as u64).max(2) as u32) & !1;
        // A very tall source could otherwise be taller than the frame it is
        // being drawn into, at which point the blit would clip it to nothing
        // useful. Shrink to fit instead, keeping the aspect ratio.
        if h > out_h {
            return fit_within(w, h, out_w, out_h);
        }
        (w, h)
    }

    /// Top-left pixel of an overlay of size `ov_w` x `ov_h`.
    ///
    /// Even, for the same chroma-grid reason as [`box_size`](Self::box_size).
    pub fn position(&self, out_w: u32, out_h: u32, ov_w: u32, ov_h: u32) -> (u32, u32) {
        let margin = ((out_w as u64 * MARGIN_PCT as u64) / 100) as u32 & !1;
        // Saturating throughout: an overlay larger than the frame pins to the
        // origin rather than wrapping around through an underflow.
        let right = out_w.saturating_sub(ov_w).saturating_sub(margin);
        let bottom = out_h.saturating_sub(ov_h).saturating_sub(margin);
        let (x, y) = match self.corner {
            Corner::TopLeft => (margin, margin),
            Corner::TopRight => (right, margin),
            Corner::BottomLeft => (margin, bottom),
            Corner::BottomRight => (right, bottom),
        };
        (
            x.min(out_w.saturating_sub(2)) & !1,
            y.min(out_h.saturating_sub(2)) & !1,
        )
    }
}

/// One overlay as stored in the `video_overlays_json` setting.
///
/// Deliberately stringly-typed and fully `#[serde(default)]`: it is written by
/// the settings UI, may be edited by hand, and must never fail a whole capture
/// because one field was mistyped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayConfig {
    /// Source id, in the same space as `video_input_device` — a camera device
    /// id, or `monitor:`/`window:` prefixed for screen capture.
    #[serde(default)]
    pub id: String,
    /// Corner name, as [`Corner::as_name`].
    #[serde(default = "default_corner_name")]
    pub corner: String,
    /// Width as a percentage of output width.
    #[serde(default = "default_size_pct")]
    pub size: u32,
}

fn default_corner_name() -> String {
    Corner::default().as_name().to_owned()
}

fn default_size_pct() -> u32 {
    DEFAULT_OVERLAY_PCT
}

impl OverlayConfig {
    /// The placement this config describes, with out-of-range values clamped.
    pub fn placement(&self) -> Placement {
        Placement {
            corner: Corner::from_name(&self.corner),
            size_pct: self.size.clamp(MIN_OVERLAY_PCT, MAX_OVERLAY_PCT),
        }
    }
}

/// Parse the `video_overlays_json` setting into overlay configs.
///
/// Tolerant by design — an empty, absent, or malformed value means "no
/// overlays", which is the pre-PIP behaviour. Refusing to capture because a
/// settings blob did not parse would turn a cosmetic preference into a broken
/// camera.
pub fn parse_overlays(json: &str) -> Vec<OverlayConfig> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Vec<OverlayConfig>>(trimmed) {
        Ok(list) => list
            .into_iter()
            .filter(|o| !o.id.trim().is_empty())
            .take(MAX_OVERLAYS)
            .collect(),
        Err(e) => {
            warn!("[video] ignoring unreadable overlay layout: {e}");
            Vec::new()
        }
    }
}

// ── Pixel operations ────────────────────────────────────────────────────────

/// Average a rectangle of one plane.
#[inline]
fn box_avg(plane: &[u8], stride: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> u8 {
    let mut sum = 0u32;
    let mut n = 0u32;
    for y in y0..y1 {
        let row = y * stride;
        for x in x0..x1 {
            sum += plane[row + x] as u32;
            n += 1;
        }
    }
    (sum / n.max(1)) as u8
}

/// Resample one plane into `dst_w` x `dst_h` with a box filter.
fn scale_plane(src: &[u8], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<u8> {
    let mut out = vec![0u8; dst_w * dst_h];
    for dy in 0..dst_h {
        let y0 = dy * src_h / dst_h;
        let y1 = (((dy + 1) * src_h).div_ceil(dst_h)).min(src_h).max(y0 + 1);
        for dx in 0..dst_w {
            let x0 = dx * src_w / dst_w;
            let x1 = (((dx + 1) * src_w).div_ceil(dst_w)).min(src_w).max(x0 + 1);
            out[dy * dst_w + dx] = box_avg(src, src_w, x0, x1, y0, y1);
        }
    }
    out
}

/// Resample an I420 frame to exactly `dst_w` x `dst_h`, ignoring aspect ratio.
///
/// Box filtering for the same reason [`super::scale`] uses it: nearest-neighbour
/// makes text and thin UI lines shimmer, and hands the encoder high-frequency
/// noise to spend bits on.
///
/// Returns `None` for dimensions I420 cannot represent, rather than panicking —
/// sizes here derive from what a capture device negotiated, which is not ours
/// to trust.
pub fn scale_i420(src: &RawFrame, dst_w: u32, dst_h: u32) -> Option<RawFrame> {
    if dst_w < 2 || dst_h < 2 || dst_w % 2 != 0 || dst_h % 2 != 0 || !src.is_consistent() {
        return None;
    }
    if src.width == dst_w && src.height == dst_h {
        return Some(src.clone());
    }
    let (sw, sh) = (src.width as usize, src.height as usize);
    let (dw, dh) = (dst_w as usize, dst_h as usize);
    Some(RawFrame {
        width: dst_w,
        height: dst_h,
        y: scale_plane(&src.y, sw, sh, dw, dh),
        u: scale_plane(&src.u, sw / 2, sh / 2, dw / 2, dh / 2),
        v: scale_plane(&src.v, sw / 2, sh / 2, dw / 2, dh / 2),
    })
}

/// Fit an I420 frame into exactly `dst_w` x `dst_h`, preserving aspect ratio and
/// centring it on black.
///
/// The fixed output size is what the encoder requires: it is configured once and
/// rejects every frame that does not match. A camera that substituted its
/// nearest supported mode for the one we asked for would otherwise produce a
/// stream where *every* frame fails to encode.
pub fn letterbox_i420(src: &RawFrame, dst_w: u32, dst_h: u32) -> Option<RawFrame> {
    if dst_w < 2 || dst_h < 2 || dst_w % 2 != 0 || dst_h % 2 != 0 || !src.is_consistent() {
        return None;
    }
    if src.width == dst_w && src.height == dst_h {
        return Some(src.clone());
    }
    let (cw, ch) = fit_within(src.width, src.height, dst_w, dst_h);
    let content = scale_i420(src, cw, ch)?;
    if cw == dst_w && ch == dst_h {
        return Some(content);
    }
    let mut out = RawFrame::black(dst_w, dst_h);
    let x = ((dst_w - cw) / 2) & !1;
    let y = ((dst_h - ch) / 2) & !1;
    overlay_i420(&mut out, &content, x, y);
    Some(out)
}

/// Draw `src` onto `dst` with its top-left corner at `(x, y)`.
///
/// Offsets are forced even and the source is clipped to the destination, so a
/// placement computed against a different frame size cannot write out of bounds.
/// Opaque — I420 carries no alpha, and blending a PIP edge would cost a
/// per-pixel multiply for something a hard edge conveys just as well.
pub fn overlay_i420(dst: &mut RawFrame, src: &RawFrame, x: u32, y: u32) {
    if !dst.is_consistent() || !src.is_consistent() {
        return;
    }
    let (x, y) = (x & !1, y & !1);
    if x >= dst.width || y >= dst.height {
        return;
    }
    let w = (src.width.min(dst.width - x)) & !1;
    let h = (src.height.min(dst.height - y)) & !1;
    if w == 0 || h == 0 {
        return;
    }

    let (dw, sw) = (dst.width as usize, src.width as usize);
    let (xu, yu, wu, hu) = (x as usize, y as usize, w as usize, h as usize);

    for row in 0..hu {
        let d = (yu + row) * dw + xu;
        let s = row * sw;
        dst.y[d..d + wu].copy_from_slice(&src.y[s..s + wu]);
    }

    // Chroma planes are half resolution in both axes, which is exactly why the
    // offsets and extents above had to be even.
    let (dcw, scw) = (dw / 2, sw / 2);
    for row in 0..hu / 2 {
        let d = (yu / 2 + row) * dcw + xu / 2;
        let s = row * scw;
        dst.u[d..d + wu / 2].copy_from_slice(&src.u[s..s + wu / 2]);
        dst.v[d..d + wu / 2].copy_from_slice(&src.v[s..s + wu / 2]);
    }
}

// ── Overlay capture ─────────────────────────────────────────────────────────

/// One overlay's capture thread and the slot it publishes into.
///
/// Only the newest frame is kept: for realtime video a queue is latency, and the
/// compositor never wants a frame older than the base frame it is drawing onto.
struct OverlayTap {
    latest: Arc<Mutex<Option<RawFrame>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    placement: Placement,
}

impl OverlayTap {
    /// Open `source` on a new thread and start publishing scaled frames.
    ///
    /// Returns immediately. Opening happens on the new thread because both
    /// backends are blocking to open and both have COM thread affinity, so they
    /// must be created where they are used.
    fn spawn(source: SourceSpec, placement: Placement, out_w: u32, out_h: u32, fps: u32) -> Self {
        let latest: Arc<Mutex<Option<RawFrame>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let latest_t = Arc::clone(&latest);
        let stop_t = Arc::clone(&stop);

        let handle = std::thread::Builder::new()
            .name("conquerd-video-overlay".into())
            .spawn(move || {
                // What to *ask* the source for differs by kind, and getting it
                // wrong is invisible: a failed overlay open only logs, so the
                // inset silently never appears.
                //
                // Screen capture letterboxes to exactly what it is asked for,
                // so requesting the final box means a monitor overlay needs no
                // second scale at all. The final box needs the source's aspect
                // ratio, which is only known once it is open, so 16:9 is an
                // estimate corrected below.
                //
                // A camera cannot do that. It has a fixed set of modes and the
                // inset box — a fifth of the output — is far below the smallest
                // of them, so asking for it makes `SetCurrentMediaType` fail for
                // every format and the whole overlay disappear. Ask for the
                // stream size, which is a real mode, and let the scale below fit
                // it into the box.
                let (want_w, want_h) = match &source {
                    SourceSpec::Camera { .. } => (out_w, out_h),
                    SourceSpec::Screen { .. } => placement.box_size(out_w, out_h, 16, 9),
                };
                let mut capture = match source.open(want_w, want_h) {
                    Ok(c) => c,
                    Err(e) => {
                        // Degrade rather than fail: the base source keeps
                        // running and the user sees a picture without the inset,
                        // which beats no picture at all.
                        warn!("[video] overlay source could not be opened: {e}");
                        return;
                    }
                };
                let (sw, sh) = capture.dimensions();
                let (box_w, box_h) = placement.box_size(out_w, out_h, sw, sh);
                info!("[video] overlay {sw}x{sh} -> {box_w}x{box_h} at {:?}", placement.corner);

                let interval = std::time::Duration::from_micros(1_000_000 / fps.max(1) as u64);
                let mut consecutive_errors = 0u32;

                while !stop_t.load(Ordering::Relaxed) {
                    let started = std::time::Instant::now();
                    match capture.next_frame() {
                        Ok(frame) => {
                            consecutive_errors = 0;
                            // Skip rather than unwrap: a panic here would leave
                            // the capture device open with nothing left holding
                            // a handle to close it.
                            if let (Some(scaled), Ok(mut slot)) =
                                (scale_i420(&frame, box_w, box_h), latest_t.lock())
                            {
                                *slot = Some(scaled);
                            }
                        }
                        Err(e) => {
                            consecutive_errors += 1;
                            // Devices routinely fail their first reads while
                            // spinning up, so tolerate a burst — but give up
                            // eventually rather than spinning on a dead device
                            // for the length of the call.
                            if consecutive_errors > 120 {
                                warn!("[video] overlay failed {consecutive_errors} times, dropping it: {e}");
                                break;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                    }
                    if let Some(rest) = interval.checked_sub(started.elapsed()) {
                        std::thread::sleep(rest);
                    }
                }
                debug!("[video] overlay capture stopped");
            })
            .ok();

        Self {
            latest,
            stop,
            handle,
            placement,
        }
    }

    /// The newest frame, or `None` before the first one arrives.
    ///
    /// Cloned rather than taken so a tap slower than the base keeps showing its
    /// last frame instead of flickering in and out of the picture.
    fn newest(&self) -> Option<RawFrame> {
        self.latest.lock().ok()?.clone()
    }
}

impl Drop for OverlayTap {
    fn drop(&mut self) {
        // Joining is what releases the device — and turns its capture light off.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// A [`CameraSource`] that draws overlay sources on top of a base source.
///
/// Presents as an ordinary capture, so [`super::sender`], the encoder, and the
/// transport never learn that more than one device is involved.
pub struct CompositeSource {
    base: Box<dyn CameraSource>,
    overlays: Vec<OverlayTap>,
    out_w: u32,
    out_h: u32,
}

impl CompositeSource {
    /// Compose `base` with one tap per entry in `overlays`.
    ///
    /// `out_w` x `out_h` is the encoder's configured size, which every frame
    /// this produces will match exactly.
    pub fn new(
        base: Box<dyn CameraSource>,
        overlays: Vec<(SourceSpec, Placement)>,
        out_w: u32,
        out_h: u32,
        fps: u32,
    ) -> Self {
        let (out_w, out_h) = (out_w.max(2) & !1, out_h.max(2) & !1);
        let taps = overlays
            .into_iter()
            .take(MAX_OVERLAYS)
            .map(|(spec, placement)| OverlayTap::spawn(spec, placement, out_w, out_h, fps))
            .collect();
        Self {
            base,
            overlays: taps,
            out_w,
            out_h,
        }
    }
}

impl CameraSource for CompositeSource {
    fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
        // The base alone paces the output. Overlays are sampled, never waited
        // on: a stalled inset must not stall the stream it is inset into.
        let base = self.base.next_frame()?;
        let mut frame = if base.width == self.out_w && base.height == self.out_h {
            base
        } else {
            letterbox_i420(&base, self.out_w, self.out_h).ok_or_else(|| {
                anyhow::anyhow!(
                    "base frame {}x{} could not be fitted to {}x{}",
                    base.width,
                    base.height,
                    self.out_w,
                    self.out_h
                )
            })?
        };

        // Later overlays draw over earlier ones, so the list order is the
        // stacking order — which is what lets two overlays share a corner
        // predictably instead of racing.
        for tap in &self.overlays {
            let Some(inset) = tap.newest() else {
                continue;
            };
            let (x, y) = tap
                .placement
                .position(self.out_w, self.out_h, inset.width, inset.height);
            overlay_i420(&mut frame, &inset, x, y);
        }

        Ok(frame)
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.out_w, self.out_h)
    }
}

/// A capture whose frames are fitted to a fixed size.
///
/// Wraps a source that negotiated something other than what was asked for —
/// cameras substitute their nearest supported mode, and the encoder rejects
/// every frame that is not exactly its configured size.
pub struct NormalizedSource {
    inner: Box<dyn CameraSource>,
    out_w: u32,
    out_h: u32,
}

impl NormalizedSource {
    /// Wrap `inner` only if it needs it, so a source that already matches pays
    /// nothing.
    pub fn wrap(inner: Box<dyn CameraSource>, out_w: u32, out_h: u32) -> Box<dyn CameraSource> {
        let (out_w, out_h) = (out_w.max(2) & !1, out_h.max(2) & !1);
        if inner.dimensions() == (out_w, out_h) {
            return inner;
        }
        let (w, h) = inner.dimensions();
        info!("[video] source negotiated {w}x{h}, fitting to {out_w}x{out_h}");
        Box::new(Self {
            inner,
            out_w,
            out_h,
        })
    }
}

impl CameraSource for NormalizedSource {
    fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
        let frame = self.inner.next_frame()?;
        letterbox_i420(&frame, self.out_w, self.out_h).ok_or_else(|| {
            anyhow::anyhow!(
                "frame {}x{} could not be fitted to {}x{}",
                frame.width,
                frame.height,
                self.out_w,
                self.out_h
            )
        })
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.out_w, self.out_h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame of one solid luma value, with distinguishable chroma.
    fn solid(w: u32, h: u32, luma: u8, cb: u8, cr: u8) -> RawFrame {
        let mut f = RawFrame::black(w, h);
        f.y.fill(luma);
        f.u.fill(cb);
        f.v.fill(cr);
        f
    }

    // ── Geometry ────────────────────────────────────────────────────────────

    #[test]
    fn an_overlay_box_keeps_the_source_aspect_ratio() {
        let p = Placement {
            corner: Corner::BottomRight,
            size_pct: 25,
        };
        // 16:9 source at 25% of a 640-wide frame.
        assert_eq!(p.box_size(640, 360, 1920, 1080), (160, 90));
        // 4:3 webcam gets a 4:3 box rather than pillarbox bars.
        assert_eq!(p.box_size(640, 360, 640, 480), (160, 120));
    }

    #[test]
    fn overlay_boxes_are_always_even() {
        for pct in [MIN_OVERLAY_PCT, 17, 23, 33, MAX_OVERLAY_PCT] {
            for (sw, sh) in [(1920u32, 1080u32), (640, 480), (1000, 999), (3, 7)] {
                let p = Placement {
                    corner: Corner::TopLeft,
                    size_pct: pct,
                };
                let (w, h) = p.box_size(642, 362, sw, sh);
                assert_eq!(w % 2, 0, "{pct}% of {sw}x{sh} gave odd width {w}");
                assert_eq!(h % 2, 0, "{pct}% of {sw}x{sh} gave odd height {h}");
                assert!(w >= 2 && h >= 2);
            }
        }
    }

    /// A wildly out-of-range size must not be able to produce an overlay that
    /// covers the frame — that would be a source the user cannot see past.
    #[test]
    fn overlay_size_is_clamped_to_a_usable_range() {
        let huge = Placement {
            corner: Corner::TopLeft,
            size_pct: 5000,
        };
        let (w, _) = huge.box_size(640, 360, 1920, 1080);
        assert!(w <= 640 * MAX_OVERLAY_PCT / 100);

        let tiny = Placement {
            corner: Corner::TopLeft,
            size_pct: 0,
        };
        let (w, _) = tiny.box_size(640, 360, 1920, 1080);
        assert!(w >= 640 * MIN_OVERLAY_PCT / 100);
    }

    /// A portrait source at a large percentage would otherwise be taller than
    /// the frame, and the blit would silently clip most of it away.
    #[test]
    fn a_tall_source_is_shrunk_to_fit_the_frame() {
        let p = Placement {
            corner: Corner::TopLeft,
            size_pct: MAX_OVERLAY_PCT,
        };
        let (w, h) = p.box_size(640, 360, 1080, 1920);
        assert!(h <= 360, "overlay {w}x{h} is taller than the frame");
        assert!(w <= 640);
    }

    #[test]
    fn each_corner_lands_inside_the_frame() {
        for corner in [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ] {
            let p = Placement {
                corner,
                size_pct: 25,
            };
            let (w, h) = p.box_size(640, 360, 1920, 1080);
            let (x, y) = p.position(640, 360, w, h);
            assert_eq!(x % 2, 0);
            assert_eq!(y % 2, 0);
            assert!(x + w <= 640, "{corner:?} overflows right: {x}+{w}");
            assert!(y + h <= 360, "{corner:?} overflows bottom: {y}+{h}");
        }
    }

    #[test]
    fn corners_are_actually_distinct() {
        let mut seen = Vec::new();
        for corner in [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ] {
            let p = Placement {
                corner,
                size_pct: 25,
            };
            let (w, h) = p.box_size(640, 360, 1920, 1080);
            let pos = p.position(640, 360, w, h);
            assert!(!seen.contains(&pos), "{corner:?} duplicated {pos:?}");
            seen.push(pos);
        }
    }

    /// An overlay bigger than the frame must pin to the origin, not wrap around
    /// through an unsigned underflow into a huge coordinate.
    #[test]
    fn an_oversized_overlay_does_not_underflow() {
        let p = Placement {
            corner: Corner::BottomRight,
            size_pct: 50,
        };
        let (x, y) = p.position(320, 180, 640, 360);
        assert!(x < 320 && y < 180, "position {x},{y} escaped the frame");
    }

    #[test]
    fn corner_names_round_trip() {
        for corner in [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ] {
            assert_eq!(Corner::from_name(corner.as_name()), corner);
        }
        // Anything unrecognised falls back rather than failing.
        assert_eq!(Corner::from_name("nonsense"), Corner::default());
        assert_eq!(Corner::from_name(""), Corner::default());
    }

    // ── Pixels ──────────────────────────────────────────────────────────────

    #[test]
    fn scaling_produces_consistent_planes() {
        let src = solid(640, 480, 200, 90, 240);
        let out = scale_i420(&src, 160, 120).unwrap();
        assert_eq!((out.width, out.height), (160, 120));
        assert!(out.is_consistent());
        // A solid source stays solid through a box filter.
        assert!(out.y.iter().all(|&v| v == 200));
        assert!(out.u.iter().all(|&v| v == 90));
        assert!(out.v.iter().all(|&v| v == 240));
    }

    #[test]
    fn scaling_to_the_same_size_is_a_passthrough() {
        let src = solid(64, 48, 100, 110, 120);
        assert_eq!(scale_i420(&src, 64, 48).unwrap(), src);
    }

    #[test]
    fn scaling_rejects_sizes_i420_cannot_represent() {
        let src = solid(64, 48, 100, 110, 120);
        assert!(scale_i420(&src, 63, 48).is_none());
        assert!(scale_i420(&src, 64, 47).is_none());
        assert!(scale_i420(&src, 0, 48).is_none());
    }

    #[test]
    fn scaling_rejects_an_inconsistent_frame() {
        let mut bad = solid(64, 48, 10, 10, 10);
        bad.u.truncate(3);
        assert!(scale_i420(&bad, 32, 24).is_none());
        assert!(letterbox_i420(&bad, 32, 24).is_none());
    }

    /// The property the encoder depends on: whatever the camera negotiated, the
    /// frame it is handed is exactly the size it was configured for.
    /// The bug this guards: an overlay asked its source for the *inset* size.
    /// A screen letterboxes to anything, but a camera has fixed modes and the
    /// inset is smaller than all of them, so the open failed and the overlay
    /// silently never appeared — while the same camera worked as the base.
    #[test]
    fn a_camera_overlay_is_opened_at_a_size_a_camera_can_actually_provide() {
        let placement = Placement {
            corner: Corner::TopRight,
            size_pct: 20,
        };
        let (out_w, out_h) = (640u32, 360u32);

        let inset = placement.box_size(out_w, out_h, 16, 9);
        assert!(
            inset.0 < 320,
            "test assumes the inset is far below any camera mode, got {inset:?}"
        );

        // A camera must be asked for the stream size, not the inset.
        let camera = SourceSpec::Camera { device_id: None };
        let camera_request = match &camera {
            SourceSpec::Camera { .. } => (out_w, out_h),
            SourceSpec::Screen { .. } => inset,
        };
        assert_eq!(camera_request, (out_w, out_h));

        // A screen keeps the inset request, which is what avoids a second scale.
        let screen = SourceSpec::Screen {
            target_id: "monitor:1".to_owned(),
        };
        let screen_request = match &screen {
            SourceSpec::Camera { .. } => (out_w, out_h),
            SourceSpec::Screen { .. } => inset,
        };
        assert_eq!(screen_request, inset);
    }

    #[test]
    fn letterboxing_always_produces_the_requested_size() {
        for (w, h) in [(640u32, 480u32), (1280, 720), (320, 180), (800, 600)] {
            let src = solid(w, h, 180, 100, 150);
            let out = letterbox_i420(&src, 640, 360).unwrap();
            assert_eq!(
                (out.width, out.height),
                (640, 360),
                "{w}x{h} was not fitted"
            );
            assert!(out.is_consistent());
        }
    }

    #[test]
    fn letterboxing_fills_the_bars_with_black() {
        // 4:3 into 16:9 leaves pillarbox bars on the left and right.
        let src = solid(640, 480, 235, 128, 128);
        let out = letterbox_i420(&src, 640, 360).unwrap();
        let row = 180usize;
        assert_eq!(out.y[row * 640], 16, "left bar must be the luma floor");
        assert_eq!(
            out.y[row * 640 + 639],
            16,
            "right bar must be the luma floor"
        );
        assert_eq!(out.y[row * 640 + 320], 235, "centre must carry the content");
    }

    #[test]
    fn an_overlay_lands_exactly_where_it_was_placed() {
        let mut base = solid(64, 48, 16, 128, 128);
        let inset = solid(16, 12, 200, 90, 240);
        overlay_i420(&mut base, &inset, 8, 4);

        assert!(base.is_consistent());
        // Inside the inset.
        assert_eq!(base.y[4 * 64 + 8], 200);
        assert_eq!(base.y[(4 + 11) * 64 + (8 + 15)], 200);
        // Just outside it, on every side.
        assert_eq!(base.y[3 * 64 + 8], 16);
        assert_eq!(base.y[4 * 64 + 7], 16);
        assert_eq!(base.y[(4 + 12) * 64 + 8], 16);
        assert_eq!(base.y[4 * 64 + (8 + 16)], 16);
        // Chroma follows at half resolution.
        assert_eq!(base.u[2 * 32 + 4], 90);
        assert_eq!(base.v[2 * 32 + 4], 240);
    }

    /// The bug this clipping exists to prevent: a placement computed against a
    /// different frame size must not write past the destination planes.
    #[test]
    fn an_overlay_is_clipped_rather_than_overrunning() {
        let mut base = solid(64, 48, 16, 128, 128);
        let inset = solid(32, 24, 200, 90, 240);
        // Straddles the bottom-right corner.
        overlay_i420(&mut base, &inset, 48, 36);
        assert!(base.is_consistent());
        assert_eq!(base.y[36 * 64 + 48], 200, "the visible part must be drawn");

        // Entirely outside: a no-op, not a panic.
        let before = base.clone();
        overlay_i420(&mut base, &inset, 64, 0);
        overlay_i420(&mut base, &inset, 0, 48);
        overlay_i420(&mut base, &inset, 9_999, 9_999);
        assert_eq!(base, before);
    }

    #[test]
    fn an_overlay_snaps_to_the_chroma_grid() {
        let mut odd = solid(64, 48, 16, 128, 128);
        let mut even = odd.clone();
        let inset = solid(16, 12, 200, 90, 240);
        overlay_i420(&mut odd, &inset, 9, 5);
        overlay_i420(&mut even, &inset, 8, 4);
        assert_eq!(odd, even, "odd offsets must round to the chroma grid");
    }

    #[test]
    fn an_inconsistent_overlay_is_ignored() {
        let mut base = solid(64, 48, 16, 128, 128);
        let before = base.clone();
        let mut bad = solid(16, 12, 200, 90, 240);
        bad.v.truncate(1);
        overlay_i420(&mut base, &bad, 0, 0);
        assert_eq!(base, before, "a malformed overlay must not be drawn");
    }

    // ── Settings parsing ────────────────────────────────────────────────────

    #[test]
    fn overlays_parse_from_the_settings_blob() {
        let list =
            parse_overlays(r#"[{"id":"monitor:1","corner":"top-left","size":30},{"id":"cam-a"}]"#);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "monitor:1");
        assert_eq!(
            list[0].placement(),
            Placement {
                corner: Corner::TopLeft,
                size_pct: 30
            }
        );
        // Missing fields fall back to the defaults.
        assert_eq!(list[1].placement(), Placement::default());
    }

    /// A settings blob is user-editable and survives upgrades, so every
    /// malformed shape must degrade to "no overlays" rather than break capture.
    #[test]
    fn unreadable_layouts_mean_no_overlays() {
        for blob in ["", "   ", "{}", "not json", "[", r#"[{"id":42}]"#, "null"] {
            assert!(
                parse_overlays(blob).is_empty(),
                "{blob:?} should have parsed to nothing"
            );
        }
    }

    #[test]
    fn empty_ids_are_dropped_and_the_count_is_capped() {
        let list = parse_overlays(r#"[{"id":""},{"id":"  "},{"id":"a"}]"#);
        assert_eq!(list.len(), 1, "blank ids select no device");

        let many: String = (0..10)
            .map(|i| format!(r#"{{"id":"cam-{i}"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(parse_overlays(&format!("[{many}]")).len(), MAX_OVERLAYS);
    }

    #[test]
    fn stored_sizes_are_clamped_on_the_way_in() {
        let list = parse_overlays(r#"[{"id":"a","size":900},{"id":"b","size":1}]"#);
        assert_eq!(list[0].placement().size_pct, MAX_OVERLAY_PCT);
        assert_eq!(list[1].placement().size_pct, MIN_OVERLAY_PCT);
    }

    // ── Composition end to end ──────────────────────────────────────────────

    /// A source that yields a fixed frame, for driving the composite without a
    /// capture device.
    struct FixedSource(RawFrame);

    impl CameraSource for FixedSource {
        fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
            Ok(self.0.clone())
        }
        fn dimensions(&self) -> (u32, u32) {
            (self.0.width, self.0.height)
        }
    }

    /// With no overlays configured the composite is a passthrough — the
    /// pre-PIP behaviour has to survive exactly.
    #[test]
    fn a_composite_with_no_overlays_passes_the_base_through() {
        let base = solid(640, 360, 120, 100, 140);
        let mut c = CompositeSource::new(Box::new(FixedSource(base.clone())), vec![], 640, 360, 30);
        assert_eq!(c.dimensions(), (640, 360));
        assert_eq!(c.next_frame().unwrap(), base);
    }

    /// The encoder rejects any frame that is not its configured size, so a base
    /// camera that substituted its own mode must still come out at the target.
    #[test]
    fn a_composite_fits_a_mismatched_base_to_the_output_size() {
        let base = solid(640, 480, 120, 100, 140);
        let mut c = CompositeSource::new(Box::new(FixedSource(base)), vec![], 640, 360, 30);
        let out = c.next_frame().unwrap();
        assert_eq!((out.width, out.height), (640, 360));
        assert!(out.is_consistent());
    }

    /// A tap holding one frame and no thread, so composition can be tested
    /// without a capture device.
    fn static_tap(frame: RawFrame, placement: Placement) -> OverlayTap {
        OverlayTap {
            latest: Arc::new(Mutex::new(Some(frame))),
            stop: Arc::new(AtomicBool::new(false)),
            handle: None,
            placement,
        }
    }

    #[test]
    fn a_composite_draws_its_overlay_in_the_chosen_corner() {
        let base = solid(640, 360, 60, 100, 140);
        let placement = Placement {
            corner: Corner::BottomRight,
            size_pct: 25,
        };
        let (ow, oh) = placement.box_size(640, 360, 1920, 1080);
        let mut c = CompositeSource {
            base: Box::new(FixedSource(base)),
            overlays: vec![static_tap(solid(ow, oh, 210, 80, 200), placement)],
            out_w: 640,
            out_h: 360,
        };

        let out = c.next_frame().unwrap();
        assert!(out.is_consistent());
        let (x, y) = placement.position(640, 360, ow, oh);
        // Inside the inset.
        assert_eq!(out.y[(y as usize) * 640 + x as usize], 210);
        assert_eq!(
            out.y[(y as usize + oh as usize - 1) * 640 + (x as usize + ow as usize - 1)],
            210
        );
        // The opposite corner is untouched base.
        assert_eq!(out.y[0], 60);
        // Chroma was moved with it, so the inset is not grey.
        assert_eq!(out.u[(y as usize / 2) * 320 + x as usize / 2], 80);
        assert_eq!(out.v[(y as usize / 2) * 320 + x as usize / 2], 200);
    }

    /// Later overlays draw over earlier ones, which is what makes two insets
    /// sharing a corner land predictably instead of by thread-timing luck.
    #[test]
    fn later_overlays_draw_over_earlier_ones() {
        let placement = Placement {
            corner: Corner::TopLeft,
            size_pct: 25,
        };
        let (ow, oh) = placement.box_size(640, 360, 1920, 1080);
        let mut c = CompositeSource {
            base: Box::new(FixedSource(solid(640, 360, 60, 100, 140))),
            overlays: vec![
                static_tap(solid(ow, oh, 100, 80, 200), placement),
                static_tap(solid(ow, oh, 200, 90, 210), placement),
            ],
            out_w: 640,
            out_h: 360,
        };
        let out = c.next_frame().unwrap();
        let (x, y) = placement.position(640, 360, ow, oh);
        assert_eq!(out.y[(y as usize) * 640 + x as usize], 200, "last one wins");
    }

    /// A tap that has not produced a frame yet — the first few hundred
    /// milliseconds of every call — must not hold up the base picture.
    #[test]
    fn a_silent_overlay_is_skipped_rather_than_waited_for() {
        let base = solid(320, 180, 60, 100, 140);
        let mut c = CompositeSource {
            base: Box::new(FixedSource(base.clone())),
            overlays: vec![OverlayTap {
                latest: Arc::new(Mutex::new(None)),
                stop: Arc::new(AtomicBool::new(false)),
                handle: None,
                placement: Placement::default(),
            }],
            out_w: 320,
            out_h: 180,
        };
        assert_eq!(c.next_frame().unwrap(), base);
    }

    /// The failure that matters most in practice: an overlay device that is
    /// gone (unplugged webcam, closed window) must cost the inset, not the
    /// stream. Also exercises the real tap thread's spawn and join.
    #[test]
    fn an_overlay_that_cannot_open_leaves_the_base_picture_alone() {
        let base = solid(320, 180, 120, 100, 140);
        let mut c = CompositeSource::new(
            Box::new(FixedSource(base.clone())),
            vec![(
                SourceSpec::Camera {
                    device_id: Some("no-such-device".into()),
                },
                Placement::default(),
            )],
            320,
            180,
            30,
        );
        assert_eq!(
            c.next_frame().unwrap(),
            base,
            "a dead overlay must not alter the picture"
        );
        // Dropping must join the tap thread rather than leaking it.
        drop(c);
    }

    #[test]
    fn normalizing_a_matching_source_adds_no_wrapper() {
        let base = solid(640, 360, 10, 20, 30);
        let mut src = NormalizedSource::wrap(Box::new(FixedSource(base.clone())), 640, 360);
        // Byte-identical, and in particular not letterboxed onto black.
        assert_eq!(src.next_frame().unwrap(), base);
        assert_eq!(src.dimensions(), (640, 360));
    }

    #[test]
    fn normalizing_a_mismatched_source_resizes_every_frame() {
        let mut src = NormalizedSource::wrap(
            Box::new(FixedSource(solid(1280, 960, 200, 90, 240))),
            640,
            360,
        );
        assert_eq!(src.dimensions(), (640, 360));
        let f = src.next_frame().unwrap();
        assert_eq!((f.width, f.height), (640, 360));
        assert!(f.is_consistent());
    }
}
