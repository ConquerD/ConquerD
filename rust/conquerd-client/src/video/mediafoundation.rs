//! Hardware-accelerated H.264 via Media Foundation (Windows).
//!
//! This is the path that makes video feel like a modern client rather than a
//! CPU hog: [`MFTEnumEx`] is asked for **hardware** transforms first, so on a
//! machine with an NVIDIA, Intel or AMD GPU the encode runs on NVENC,
//! QuickSync or AMF respectively, at a few percent of the CPU a software
//! encoder would need. That CPU headroom matters here specifically because it
//! competes with the real-time audio pipeline in [`crate::call_controller`].
//!
//! # Licensing
//!
//! H.264 is patent-encumbered, but the licence rides with the operating
//! system: we call an encoder Windows already ships and already licenses.
//! Compiling a codec like openh264 from source would instead put MPEG-LA
//! exposure into a signed binary, which is why that route was rejected.
//!
//! # Pixel format
//!
//! Hardware MFTs want NV12; the rest of the pipeline is I420. [`super::nv12`]
//! converts between them, honouring the row strides Media Foundation reports —
//! see that module for why treating those buffers as tightly packed produces
//! sheared video.
//!
//! # Threading
//!
//! An [`MfEncoder`] or [`MfDecoder`] owns COM objects with apartment affinity
//! and must stay on the thread that created it. They are `Send` so they can be
//! moved onto a dedicated codec thread, but never `Sync`.

use std::sync::Once;

use tracing::{debug, warn};

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_core::VARIANT;

use super::codec::{VideoDecoder, VideoEncoder};
use super::frame::RawFrame;
use super::nv12;

/// Initialise COM + Media Foundation exactly once per process.
static MF_INIT: Once = Once::new();

/// Initialise COM + Media Foundation, safe to call from anywhere.
///
/// Exposed so the capture backend in [`super::camera`] shares one initialisation
/// with the codec rather than racing it.
pub fn ensure_started() {
    ensure_mf_started();
}

fn ensure_mf_started() {
    MF_INIT.call_once(|| {
        // SAFETY: called once; failures are non-fatal because the caller may
        // already have initialised COM on this thread with a compatible model.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
        }
    });
}

/// How a transform was obtained, for logging and for deciding whether the
/// quality/bitrate settings are worth pushing hard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceleration {
    /// A GPU transform (NVENC / QuickSync / AMF).
    Hardware,
    /// Microsoft's software transform.
    Software,
}

/// Errors from the Media Foundation codec path.
#[derive(Debug, Clone)]
pub enum MfError {
    /// No transform could be created at all.
    NoTransform(String),
    /// Configuring input/output media types failed.
    Configure(String),
    /// An encode or decode call failed.
    Process(String),
    /// Arguments were rejected before reaching Media Foundation.
    InvalidArgument(String),
}

impl std::fmt::Display for MfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTransform(m) => write!(f, "no H.264 transform available: {m}"),
            Self::Configure(m) => write!(f, "media type configuration failed: {m}"),
            Self::Process(m) => write!(f, "transform failed: {m}"),
            Self::InvalidArgument(m) => write!(f, "invalid argument: {m}"),
        }
    }
}

impl std::error::Error for MfError {}

/// Encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MfEncoderConfig {
    /// Frame width. Must be even.
    pub width: u32,
    /// Frame height. Must be even.
    pub height: u32,
    /// Target bitrate in bits per second.
    pub bitrate_bps: u32,
    /// Frame rate numerator (frames per second).
    pub fps: u32,
    /// Seconds between forced keyframes.
    ///
    /// A receiver that misses every keyframe request still recovers within this
    /// window, so keyframe-request loss degrades quality rather than
    /// deadlocking the stream.
    pub keyframe_interval_secs: u32,
}

impl Default for MfEncoderConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 360,
            bitrate_bps: 600_000,
            fps: 30,
            keyframe_interval_secs: 4,
        }
    }
}

/// Pack a width/height (or numerator/denominator) pair into the `u64` layout
/// Media Foundation uses for `MF_MT_FRAME_SIZE` and friends.
fn pack_ratio(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

/// Build the `VT_UI4` VARIANT `ICodecAPI` expects for bitrate.
///
/// `From<u32>` is what produces VT_UI4 specifically — the tag has to match or
/// the codec rejects the value — and it keeps the union handling inside
/// `windows-core` rather than requiring unsafe field writes here.
fn variant_u32(value: u32) -> VARIANT {
    VARIANT::from(value)
}

/// Find an H.264 transform, preferring hardware.
///
/// `MFT_ENUM_FLAG_SORTANDFILTER` puts the best match first, and asking for
/// hardware separately (rather than in one call with the software flag) is what
/// guarantees a GPU encoder is chosen when one exists — a combined enumeration
/// can return a software transform first on some driver stacks.
fn enumerate_transform(
    category: GUID,
    input: GUID,
    output: GUID,
    prefer_hardware: bool,
) -> Result<(IMFTransform, Acceleration), MfError> {
    let hardware = (
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
        Acceleration::Hardware,
    );
    let software = (
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
        Acceleration::Software,
    );
    let order = if prefer_hardware {
        [hardware, software]
    } else {
        [software, software]
    };

    for (flags, accel) in order {
        let in_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: input,
        };
        let out_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: output,
        };

        // SAFETY: the type-info structs outlive the call; the returned array is
        // freed by CoTaskMemFree below.
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        let hr = unsafe {
            MFTEnumEx(
                category,
                flags,
                Some(&in_info),
                Some(&out_info),
                &mut activates,
                &mut count,
            )
        };
        if hr.is_err() || count == 0 || activates.is_null() {
            continue;
        }

        // SAFETY: MFTEnumEx populated `count` entries.
        let slice = unsafe { std::slice::from_raw_parts(activates, count as usize) };
        let mut chosen = None;
        for activate in slice.iter().flatten() {
            // SAFETY: activate is a live IMFActivate from the enumeration.
            if let Ok(transform) = unsafe { activate.ActivateObject::<IMFTransform>() } {
                chosen = Some(transform);
                break;
            }
        }
        // SAFETY: the array was allocated by MFTEnumEx with CoTaskMemAlloc.
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _)) };

        if let Some(t) = chosen {
            return Ok((t, accel));
        }
    }
    Err(MfError::NoTransform(
        "MFTEnumEx returned no activatable H.264 transform".into(),
    ))
}

/// Hardware-accelerated H.264 encoder.
pub struct MfEncoder {
    transform: IMFTransform,
    /// Present only for asynchronous (hardware) transforms, which must be
    /// driven by `METransformNeedInput` / `METransformHaveOutput` events
    /// rather than by calling `ProcessInput` whenever we feel like it.
    ///
    /// Events arrive through an `IMFAsyncCallback` rather than by polling
    /// `GetEvent`: the contract specifies `BeginGetEvent`/`EndGetEvent`, and
    /// polling saw no events at all — which is what made every hardware
    /// transform fall back to software.
    events: Option<super::mf_async::EventPump>,
    /// Encoded frames the transform has handed back but the caller has not
    /// taken yet.
    ///
    /// An asynchronous MFT produces output on its own schedule, not one unit
    /// per `ProcessInput`. Leaving a surplus sitting inside the transform is
    /// what makes it stop posting `METransformNeedInput` — so output is always
    /// drained to exhaustion and parked here, and [`encode_inner`] hands back
    /// one per call to keep the frame-in/frame-out API intact.
    ///
    /// [`encode_inner`]: MfEncoder::encode_inner
    pending_output: std::collections::VecDeque<(Vec<u8>, bool)>,
    config: MfEncoderConfig,
    acceleration: Acceleration,
    frame_index: i64,
    force_keyframe: bool,
    nv12_scratch: Vec<u8>,
    /// H.264 SPS/PPS, in Annex-B form, prepended to every keyframe.
    ///
    /// Media Foundation publishes the sequence header as an *attribute on the
    /// output media type* (`MF_MT_MPEG_SEQUENCE_HEADER`) and does not guarantee
    /// it appears in the bitstream. A decoder fed only frame payloads therefore
    /// has no parameter sets and silently produces nothing — which is exactly
    /// how the receive path failed.
    ///
    /// Prepending to every keyframe (not just the first) is deliberate: this is
    /// a group call, and a peer who joins mid-stream, or recovers after loss,
    /// must be able to start decoding from the next keyframe alone.
    sequence_header: Vec<u8>,
}

// SAFETY: the COM objects are exclusively owned and never shared. Sync is
// deliberately absent — MFTs have thread affinity.
unsafe impl Send for MfEncoder {}

impl MfEncoder {
    /// Create an encoder, preferring a GPU transform but falling back to the
    /// software one when the GPU path does not actually produce frames.
    ///
    /// The fallback is not paranoia. A hardware MFT can enumerate, unlock, and
    /// accept media types, and still never post `METransformNeedInput` — some
    /// drivers only start posting once the client has primed the queue with an
    /// `IMFAsyncCallback` via `BeginGetEvent`. Rather than ship a client that
    /// silently produces no video on those machines, construction probes the
    /// hardware path and quietly drops to software if it is not delivering.
    pub fn new(config: MfEncoderConfig) -> Result<Self, MfError> {
        // Validate up front so a caller error is reported as such, rather than
        // being reshaped into a "no transform available" by the fallback chain.
        validate_config(&config)?;

        let hardware_error = match Self::with_preference(config, true) {
            Ok(mut enc) => match enc.probe_with_reason() {
                Ok(()) => return Ok(enc),
                Err(why) => format!("hardware transform unusable: {why}"),
            },
            Err(e) => format!("no hardware transform: {e}"),
        };
        // Logged, not just carried: when the software fallback succeeds this
        // string is otherwise dropped, and "why am I on software?" becomes
        // unanswerable on the one machine where it matters.
        warn!("[video] falling back to software H.264 encoder — {hardware_error}");

        let mut sw = Self::with_preference(config, false).map_err(|e| {
            MfError::NoTransform(format!(
                "{hardware_error}; software fallback also failed: {e}"
            ))
        })?;
        sw.probe_with_reason().map_err(|why| {
            MfError::Process(format!(
                "{hardware_error}; software transform also failed: {why}"
            ))
        })?;
        Ok(sw)
    }

    /// Feed a few black frames to confirm the transform actually emits a
    /// bitstream, then reset it for real use.
    ///
    /// The error is kept rather than collapsed to a bool so a hardware fallback
    /// can say *why* it fell back — the difference between "no GPU encoder" and
    /// "GPU encoder never asked for input" is the whole diagnosis.
    fn probe_with_reason(&mut self) -> Result<(), String> {
        let probe = RawFrame::black(self.config.width, self.config.height);
        // Encoders pipeline, so a single frame proves nothing either way.
        for attempt in 0..16 {
            match self.encode_inner(&probe) {
                Ok((data, _)) if !data.is_empty() => {
                    self.reset_stream();
                    return Ok(());
                }
                Ok(_) => {}
                Err(e) => return Err(format!("frame {attempt}: {e}")),
            }
        }
        Err("16 probe frames produced no bitstream".to_string())
    }

    /// Flush pipelined state so the probe's frames do not appear in the stream.
    fn reset_stream(&mut self) {
        // SAFETY: advisory messages on a live transform; failures are benign.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
        }
        // Discard events queued before the flush. After a flush the transform
        // sends no further NeedInput until the next START_OF_STREAM, so a stale
        // one would authorise a ProcessInput it never asked for — answered with
        // MF_E_NOTACCEPTING.
        if let Some(pump) = &self.events {
            pump.clear();
        }
        // Likewise drop already-drained probe output — it is black frames on a
        // timeline that is about to restart at zero, so handing any of it to a
        // caller would put stale content at the head of the real stream.
        self.pending_output.clear();
        // SAFETY: re-arms the request cycle after the flush.
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
        self.frame_index = 0;
        self.force_keyframe = true;
    }

    fn with_preference(config: MfEncoderConfig, prefer_hardware: bool) -> Result<Self, MfError> {
        validate_config(&config)?;
        ensure_mf_started();

        let (transform, acceleration) = enumerate_transform(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFVideoFormat_NV12,
            MFVideoFormat_H264,
            prefer_hardware,
        )?;

        // Hardware MFTs are asynchronous and start out *locked*: any attempt to
        // set media types returns MF_E_TRANSFORM_ASYNC_LOCKED (0xC00D6D77)
        // until the caller declares it understands the async model. Only
        // hardware transforms are async, so reaching this branch is itself
        // confirmation that a GPU encoder was found.
        let is_async = unlock_if_async(&transform)?;

        // Output type must be set before input on an encoder MFT: the encoder
        // derives its input constraints from the chosen output format, and
        // setting input first is rejected with MF_E_TRANSFORM_TYPE_NOT_SET.
        let out_type = create_media_type(&[
            (MF_MT_MAJOR_TYPE, MediaTypeValue::Guid(MFMediaType_Video)),
            (MF_MT_SUBTYPE, MediaTypeValue::Guid(MFVideoFormat_H264)),
            (MF_MT_AVG_BITRATE, MediaTypeValue::U32(config.bitrate_bps)),
            (
                MF_MT_FRAME_SIZE,
                MediaTypeValue::U64(pack_ratio(config.width, config.height)),
            ),
            (
                MF_MT_FRAME_RATE,
                MediaTypeValue::U64(pack_ratio(config.fps, 1)),
            ),
            (
                MF_MT_PIXEL_ASPECT_RATIO,
                MediaTypeValue::U64(pack_ratio(1, 1)),
            ),
            (
                MF_MT_INTERLACE_MODE,
                MediaTypeValue::U32(MFVideoInterlace_Progressive.0 as u32),
            ),
            (
                MF_MT_MPEG2_PROFILE,
                MediaTypeValue::U32(eAVEncH264VProfile_Base.0 as u32),
            ),
        ])?;
        // SAFETY: transform is live; index 0 is the only output stream.
        unsafe { transform.SetOutputType(0, &out_type, 0) }
            .map_err(|e| MfError::Configure(format!("SetOutputType: {e}")))?;

        let in_type = create_media_type(&[
            (MF_MT_MAJOR_TYPE, MediaTypeValue::Guid(MFMediaType_Video)),
            (MF_MT_SUBTYPE, MediaTypeValue::Guid(MFVideoFormat_NV12)),
            (
                MF_MT_FRAME_SIZE,
                MediaTypeValue::U64(pack_ratio(config.width, config.height)),
            ),
            (
                MF_MT_FRAME_RATE,
                MediaTypeValue::U64(pack_ratio(config.fps, 1)),
            ),
            (
                MF_MT_PIXEL_ASPECT_RATIO,
                MediaTypeValue::U64(pack_ratio(1, 1)),
            ),
            (
                MF_MT_INTERLACE_MODE,
                MediaTypeValue::U32(MFVideoInterlace_Progressive.0 as u32),
            ),
        ])?;
        // SAFETY: as above, input stream 0.
        unsafe { transform.SetInputType(0, &in_type, 0) }
            .map_err(|e| MfError::Configure(format!("SetInputType: {e}")))?;

        // Arm the event pump *before* START_OF_STREAM: "The MFT must not send
        // any METransformNeedInput events until it receives the
        // MFT_MESSAGE_NOTIFY_START_OF_STREAM message" — meaning it may send one
        // the instant it does. An unarmed queue drops that first request, and
        // the encoder then waits forever for something that already happened.
        let events = if is_async {
            // SAFETY: an async MFT always implements IMFMediaEventGenerator.
            match transform.cast::<IMFMediaEventGenerator>() {
                Ok(gen) => match super::mf_async::EventPump::start(&gen) {
                    Ok(pump) => Some(pump),
                    Err(e) => {
                        warn!("[video] could not start MF event pump: {e}");
                        None
                    }
                },
                Err(e) => {
                    warn!("[video] async MFT has no event generator: {e}");
                    None
                }
            }
        } else {
            None
        };

        // SAFETY: streaming notifications on a configured transform.
        //
        // START_OF_STREAM is what releases an async transform to start
        // requesting input, so it must come after the pump is armed above.
        unsafe {
            let _ = transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
            let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }

        // Read the sequence header now, while the negotiated output type is
        // known. It is not available before SetOutputType and some transforms
        // drop the attribute once streaming begins.
        let sequence_header = read_sequence_header(&transform);
        if sequence_header.is_empty() {
            // Not fatal: a transform that emits SPS/PPS in-band needs no help.
            // Worth noting though, because a receiver that cannot decode with
            // an empty header here is a strong hint the assumption was wrong.
            debug!("[video] encoder published no MF_MT_MPEG_SEQUENCE_HEADER");
        } else {
            debug!(
                "[video] captured {}-byte H.264 sequence header",
                sequence_header.len()
            );
        }

        let scratch = vec![0u8; nv12::nv12_len(config.width, config.height)];
        Ok(Self {
            transform,
            events,
            pending_output: std::collections::VecDeque::new(),
            config,
            acceleration,
            frame_index: 0,
            force_keyframe: false,
            nv12_scratch: scratch,
            sequence_header,
        })
    }

    /// Whether a GPU transform was obtained.
    pub fn acceleration(&self) -> Acceleration {
        self.acceleration
    }

    /// The captured H.264 sequence header (SPS/PPS), for diagnostics.
    pub fn sequence_header(&self) -> &[u8] {
        &self.sequence_header
    }

    /// The configuration in force.
    pub fn config(&self) -> MfEncoderConfig {
        self.config
    }

    /// Duration of one frame in 100 ns units, Media Foundation's time base.
    fn frame_duration(&self) -> i64 {
        10_000_000i64 / self.config.fps.max(1) as i64
    }
}

impl VideoEncoder for MfEncoder {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)> {
        self.encode_inner(frame)
    }

    fn request_keyframe(&mut self) {
        self.force_keyframe = true;
    }

    /// Retarget the encoder's mean bitrate through `ICodecAPI`.
    ///
    /// `ICodecAPI` is the only way to change rate on a *running* MFT. Setting
    /// `MF_MT_AVG_BITRATE` on a new media type would work too, but renegotiating
    /// the output type resets the encoder — every adaptation would cost a
    /// keyframe, which is a bandwidth spike precisely when the link is already
    /// congested. That would make ABR actively harmful.
    ///
    /// Not every transform implements `ICodecAPI` (Microsoft's software encoder
    /// notably may not), so an unsupported encoder is reported as an error the
    /// caller can log once and stop retrying, not treated as fatal.
    fn set_bitrate(&mut self, bps: u32) -> anyhow::Result<()> {
        if bps == 0 {
            anyhow::bail!("bitrate must be non-zero");
        }
        let codec_api = self
            .transform
            .cast::<ICodecAPI>()
            .map_err(|e| anyhow::anyhow!("encoder does not expose ICodecAPI: {e}"))?;

        // SAFETY: `value` is a VT_UI4 VARIANT built here and dropped here; the
        // codec copies what it needs during SetValue.
        unsafe {
            let value = variant_u32(bps);
            codec_api
                .SetValue(&CODECAPI_AVEncCommonMeanBitRate, &value)
                .map_err(|e| anyhow::anyhow!("set mean bitrate {bps}: {e}"))?;
        }
        self.config.bitrate_bps = bps;
        debug!("[video] encoder bitrate -> {bps} bps");
        Ok(())
    }
}

impl MfEncoder {
    fn encode_inner(&mut self, frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)> {
        if frame.width != self.config.width || frame.height != self.config.height {
            anyhow::bail!(
                "frame {}x{} does not match encoder {}x{}",
                frame.width,
                frame.height,
                self.config.width,
                self.config.height
            );
        }
        let stride = self.config.width as usize;
        if !nv12::i420_to_nv12(
            &frame.y,
            &frame.u,
            &frame.v,
            frame.width,
            frame.height,
            &mut self.nv12_scratch,
            stride,
        ) {
            anyhow::bail!("I420 -> NV12 conversion rejected the frame");
        }

        let duration = self.frame_duration();
        let timestamp = self.frame_index * duration;
        self.frame_index += 1;

        let sample = make_sample(&self.nv12_scratch, timestamp, duration)
            .map_err(|e| anyhow::anyhow!("build input sample: {e}"))?;
        if self.force_keyframe {
            // SAFETY: sample is live and owned here.
            unsafe {
                let _ = sample.SetUINT32(&MFSampleExtension_CleanPoint, 1);
            }
            self.force_keyframe = false;
        }

        // Async (hardware) transforms must be asked before being fed: "Any call
        // to ProcessInput that does not correspond to an METransformNeedInput
        // event must return MF_E_NOTACCEPTING."
        self.await_need_input()?;

        // SAFETY: stream 0; the transform consumes the sample.
        unsafe { self.transform.ProcessInput(0, &sample, 0) }
            .map_err(|e| anyhow::anyhow!("ProcessInput: {e}"))?;

        // Drain every sample the transform is ready to give us into the park
        // queue. Leaving surplus output inside an async MFT is what makes it
        // stop posting METransformNeedInput — so we pull to exhaustion and
        // hand back one sample per call below.
        self.pull_available_output()?;

        // Frame-in / frame-out API: one encode call yields at most one sample.
        // Hardware pipelines often emit nothing for the first several frames
        // (empty Ok) and later emit a burst that lands in pending_output.
        match self.pending_output.pop_front() {
            Some((data, keyframe)) => Ok((self.with_sequence_header(data, keyframe), keyframe)),
            None => Ok((Vec::new(), false)),
        }
    }

    /// Run the transform's event loop until it asks for input.
    ///
    /// A no-op on synchronous transforms, which accept input whenever asked.
    ///
    /// The loop is the whole point. `METransformNeedInput` and
    /// `METransformHaveOutput` are not independent streams: once the transform
    /// has posted output, it stops asking for input until that output is
    /// collected. Waiting for `NeedInput` alone therefore deadlocks against
    /// pending output — the two sides wait on each other and the encode times
    /// out with a queue full of uncollected `HaveOutput` events.
    fn await_need_input(&mut self) -> anyhow::Result<()> {
        if self.events.is_none() {
            return Ok(());
        }
        // A compressed frame is always smaller than its raw source, so the raw
        // size is a safe upper bound for the output buffer.
        let min_buffer = self.config.width * self.config.height;
        let deadline = std::time::Instant::now() + NEED_INPUT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            // `u32` is Copy, so the pump borrow ends here and the arms below are
            // free to take `&mut self`.
            let next = self.events.as_ref().and_then(|p| p.wait_next(remaining));
            match next {
                Some(crate::video::mf_async::ME_TRANSFORM_NEED_INPUT) => return Ok(()),
                Some(crate::video::mf_async::ME_TRANSFORM_HAVE_OUTPUT) => {
                    if let Some(item) = drain_output(&self.transform, min_buffer)? {
                        self.push_pending(item);
                    }
                }
                // Drain-complete, format changes and the like: nothing to do
                // here, but keep waiting for the request.
                Some(_) => {}
                None => {
                    // Bounded rather than blocking: on the capture thread an
                    // indefinite wait would freeze the call, and dropping one
                    // frame is always the better failure.
                    anyhow::bail!("transform did not request input within the timeout");
                }
            }
        }
    }

    /// Pull every currently-available encoded sample into [`Self::pending_output`].
    ///
    /// Synchronous MFTs signal "no more" via `MF_E_TRANSFORM_NEED_MORE_INPUT`.
    /// Asynchronous ones post `METransformHaveOutput` per sample and return
    /// `E_UNEXPECTED` if `ProcessOutput` is called without one, so we only call
    /// while the event pump still has a matching event.
    fn pull_available_output(&mut self) -> anyhow::Result<()> {
        // A compressed frame is always smaller than its raw source, so the raw
        // size is a safe upper bound for the output buffer.
        let min_buffer = self.config.width * self.config.height;
        let is_async = self.events.is_some();

        if is_async {
            // Each take() consumes one METransformHaveOutput. Loop until the
            // pump has no more, so a burst of samples does not stall the MFT.
            while self
                .events
                .as_ref()
                .is_some_and(|p| p.take(crate::video::mf_async::ME_TRANSFORM_HAVE_OUTPUT))
            {
                // An event without a sample is unexpected but non-fatal — keep
                // draining the remaining events rather than bailing.
                if let Some(item) = drain_output(&self.transform, min_buffer)? {
                    self.push_pending(item);
                }
            }
        } else {
            // Sync path: ProcessOutput until the transform asks for more input.
            while let Some(item) = drain_output(&self.transform, min_buffer)? {
                self.push_pending(item);
            }
        }
        Ok(())
    }

    /// Park one drained sample, shedding the oldest if the queue is full.
    ///
    /// The cap is a safety net: under a healthy capture cadence the queue is
    /// 0–2 deep. If the caller stalls, dropping the oldest keeps memory and
    /// the MFT's input-accept path from growing without bound.
    fn push_pending(&mut self, item: (Vec<u8>, bool)) {
        const MAX_PENDING: usize = 8;
        while self.pending_output.len() >= MAX_PENDING {
            let _ = self.pending_output.pop_front();
        }
        self.pending_output.push_back(item);
    }

    /// Ensure a keyframe carries SPS/PPS so it can be decoded standalone.
    ///
    /// Only keyframes are touched: inter frames reference a decoder state that
    /// already has the parameter sets, and prefixing them would waste bandwidth
    /// on every frame.
    fn with_sequence_header(&self, data: Vec<u8>, keyframe: bool) -> Vec<u8> {
        if !keyframe || self.sequence_header.is_empty() || starts_with_sps(&data) {
            return data;
        }
        let mut out = Vec::with_capacity(self.sequence_header.len() + data.len());
        out.extend_from_slice(&self.sequence_header);
        out.extend_from_slice(&data);
        out
    }
}

/// Hardware-accelerated H.264 decoder.
pub struct MfDecoder {
    transform: IMFTransform,
    /// Present only for asynchronous (hardware) transforms. See the field of
    /// the same name on [`MfEncoder`] for why polling is not an option.
    events: Option<super::mf_async::EventPump>,
    /// Decoded frames collected while waiting for the transform to ask for
    /// input, not yet handed to the caller. See [`MfEncoder::pending_output`]
    /// for why servicing output during that wait is mandatory.
    pending_frames: std::collections::VecDeque<RawFrame>,
    acceleration: Acceleration,
    width: u32,
    height: u32,
}

/// Ceiling on decoded frames held for the caller.
///
/// Small on purpose: a receiver this far behind is better off skipping to live
/// than replaying stale frames, and raw I420 is ~460 KB each at 640×360.
const MAX_PENDING_FRAMES: usize = 4;

// SAFETY: see `MfEncoder`.
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    /// Create a decoder, preferring hardware.
    ///
    /// Software is a normal outcome here, not a failure. Windows exposes GPU
    /// H.264 *decode* mainly through Microsoft's own decoder MFT driven by a
    /// D3D device manager, rather than as a vendor MFT that `MFTEnumEx` returns
    /// under `MFT_ENUM_FLAG_HARDWARE` — so on most machines this enumeration
    /// legitimately finds nothing. Decode is also far cheaper than encode, so
    /// the software path is comfortable at call resolutions.
    pub fn new() -> Result<Self, MfError> {
        match Self::with_preference(true) {
            Ok(dec) => Ok(dec),
            Err(hardware_error) => {
                debug!("[video] no hardware H.264 decoder ({hardware_error}); using software");
                Self::with_preference(false)
            }
        }
    }

    /// Create a decoder from the hardware or software transform list.
    ///
    /// A hardware decoder is asynchronous, so it only accepts input after
    /// posting `METransformNeedInput` — delivered through the
    /// [`EventPump`](super::mf_async::EventPump) armed below. Falling back to
    /// software on any failure keeps a machine with a broken or busy hardware
    /// decoder playing video rather than showing nothing.
    fn with_preference(prefer_hardware: bool) -> Result<Self, MfError> {
        ensure_mf_started();
        let (transform, acceleration) = enumerate_transform(
            MFT_CATEGORY_VIDEO_DECODER,
            MFVideoFormat_H264,
            MFVideoFormat_NV12,
            prefer_hardware,
        )?;
        // Decoders are async on hardware too, and equally refuse media types
        // until unlocked.
        let is_async = unlock_if_async(&transform)?;

        // Before any media type, because the attribute is read when the
        // transform configures its picture buffers. See `request_low_latency`:
        // without it this decoder emits nothing for its first ~30 submissions.
        request_low_latency(&transform);

        // Frame size and rate are nominally optional on an H.264 decoder input
        // type, but omitting them leaves Microsoft's decoder incompletely
        // configured: it accepts `SetInputType`, reports `ACCEPT_DATA`, and
        // then rejects every `ProcessInput` with `MF_E_NOTACCEPTING`. The
        // values are only a hint — the decoder re-derives the real geometry
        // from the stream's sequence header and signals a format change.
        let in_type = create_media_type(&[
            (MF_MT_MAJOR_TYPE, MediaTypeValue::Guid(MFMediaType_Video)),
            (MF_MT_SUBTYPE, MediaTypeValue::Guid(MFVideoFormat_H264)),
            (
                MF_MT_INTERLACE_MODE,
                MediaTypeValue::U32(MFVideoInterlace_Progressive.0 as u32),
            ),
            (MF_MT_FRAME_SIZE, MediaTypeValue::U64(pack_ratio(640, 360))),
            (MF_MT_FRAME_RATE, MediaTypeValue::U64(pack_ratio(30, 1))),
            (
                MF_MT_PIXEL_ASPECT_RATIO,
                MediaTypeValue::U64(pack_ratio(1, 1)),
            ),
        ])?;
        // Decoders take input first — the mirror of the encoder, because here
        // the *input* format is what determines the available outputs.
        // SAFETY: stream 0 on a live transform.
        unsafe { transform.SetInputType(0, &in_type, 0) }
            .map_err(|e| MfError::Configure(format!("decoder SetInputType: {e}")))?;

        // An output type is **mandatory** before the transform will accept data.
        // Per the Basic MFT Processing Model: "Before an MFT can process data,
        // the client must set a media type for each of the streams." Until then
        // `ProcessInput` fails with `MF_E_NOTACCEPTING` — while
        // `GetInputStatus` still reports `ACCEPT_DATA`, a pair of signals that
        // makes a missing media type look like anything but.
        //
        // The failure is checked, not swallowed. The original code set this
        // with `let _ =`, so a failure here was invisible and surfaced much
        // later as an unexplainable rejection on the first frame.
        select_decoder_output_type(&transform)?;

        // Arm the pump before START_OF_STREAM — the transform may post its
        // first METransformNeedInput the instant it receives that message, and
        // an unarmed queue would drop it. See `MfEncoder::with_preference`.
        let events = if is_async {
            // SAFETY: an async MFT always implements IMFMediaEventGenerator.
            let generator = transform
                .cast::<IMFMediaEventGenerator>()
                .map_err(|e| MfError::Configure(format!("decoder event generator: {e}")))?;
            Some(
                super::mf_async::EventPump::start(&generator)
                    .map_err(|e| MfError::Configure(format!("decoder event pump: {e}")))?,
            )
        } else {
            None
        };

        // SAFETY: streaming notification on a now fully-configured transform.
        //
        // `MFT_MESSAGE_NOTIFY_START_OF_STREAM` is sent only for asynchronous
        // transforms — it is defined for those alone.
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| MfError::Configure(format!("decoder BEGIN_STREAMING: {e}")))?;
            if is_async {
                let _ = transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
            }
        }

        Ok(Self {
            transform,
            events,
            pending_frames: std::collections::VecDeque::new(),
            acceleration,
            width: 0,
            height: 0,
        })
    }

    /// Whether a GPU transform was obtained.
    pub fn acceleration(&self) -> Acceleration {
        self.acceleration
    }

    /// Run the transform's event loop until it asks for input.
    ///
    /// The decoder's mirror of [`MfEncoder::await_need_input`], and deadlocks
    /// the same way without it: a decoder holding an undelivered frame stops
    /// requesting input until that frame is collected.
    fn await_need_input(&mut self) -> anyhow::Result<()> {
        if self.events.is_none() {
            return Ok(());
        }
        let deadline = std::time::Instant::now() + NEED_INPUT_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let next = self.events.as_ref().and_then(|p| p.wait_next(remaining));
            match next {
                Some(crate::video::mf_async::ME_TRANSFORM_NEED_INPUT) => return Ok(()),
                Some(crate::video::mf_async::ME_TRANSFORM_HAVE_OUTPUT) => {
                    if let Some(frame) = self.take_output_frame()? {
                        self.pending_frames.push_back(frame);
                        while self.pending_frames.len() > MAX_PENDING_FRAMES {
                            self.pending_frames.pop_front();
                        }
                    }
                }
                Some(_) => {}
                // Not fatal for a decoder: `ProcessInput` is still worth
                // attempting, and its `MF_E_NOTACCEPTING` path recovers by
                // draining. Returning Ok keeps a slow decoder from turning
                // every frame into an error.
                None => return Ok(()),
            }
        }
    }

    /// `ProcessOutput` once and convert the result to I420.
    fn take_output_frame(&mut self) -> anyhow::Result<Option<RawFrame>> {
        let min_buffer = self
            .width
            .checked_mul(self.height)
            .map(|n| n * 3 / 2)
            .unwrap_or(0)
            .max(1920 * 1080 * 3 / 2);
        let Some((data, _)) = drain_output(&self.transform, min_buffer)? else {
            return Ok(None);
        };
        let Some((width, height, stride)) = self.refresh_output_geometry() else {
            return Ok(None);
        };
        let uv_offset = stride * height as usize;
        Ok(
            nv12::nv12_to_i420(&data, stride, uv_offset, width, height).map(|(y, u, v)| RawFrame {
                width,
                height,
                y,
                u,
                v,
            }),
        )
    }

    /// Whether `ProcessOutput` may be called right now.
    ///
    /// Always true for a synchronous transform, which answers "nothing yet"
    /// with `MF_E_TRANSFORM_NEED_MORE_INPUT`. An asynchronous one instead
    /// returns `E_UNEXPECTED` for a `ProcessOutput` it never signalled, so the
    /// `METransformHaveOutput` event has to be consumed first.
    fn output_ready(&self, wait: std::time::Duration) -> bool {
        match &self.events {
            Some(pump) => pump.wait_for(crate::video::mf_async::ME_TRANSFORM_HAVE_OUTPUT, wait),
            None => true,
        }
    }

    /// Collect one pending output frame, if the transform has one waiting.
    ///
    /// Used to clear the backlog that makes `ProcessInput` return
    /// `MF_E_NOTACCEPTING`.
    fn drain_pending(&mut self) -> anyhow::Result<Option<RawFrame>> {
        // The backlog is by definition already there, so no waiting: an async
        // transform that has not signalled output simply has none to give.
        if !self.output_ready(std::time::Duration::ZERO) {
            return Ok(None);
        }
        self.take_output_frame()
    }

    /// Read the negotiated frame size and row stride from the current output
    /// type, which the decoder only fills in once it has parsed a keyframe.
    fn refresh_output_geometry(&mut self) -> Option<(u32, u32, usize)> {
        // SAFETY: querying the current output type on a live transform.
        unsafe {
            let mt = self.transform.GetOutputCurrentType(0).ok()?;
            let size = mt.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
            let width = (size >> 32) as u32;
            let height = (size & 0xFFFF_FFFF) as u32;
            if width == 0 || height == 0 {
                return None;
            }
            // Default stride is the width; a negative or absent value means
            // "tightly packed", which is the common case for NV12 here.
            let stride = mt
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .ok()
                .map(|s| s as i32)
                .filter(|s| *s > 0)
                .map(|s| s as usize)
                .unwrap_or(width as usize);
            self.width = width;
            self.height = height;
            Some((width, height, stride))
        }
    }
}

impl VideoDecoder for MfDecoder {
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<Option<RawFrame>> {
        if encoded.is_empty() {
            anyhow::bail!("empty frame");
        }

        let sample = make_sample(encoded, 0, 0)
            .map_err(|e| anyhow::anyhow!("build decoder input sample: {e}"))?;

        // Services HaveOutput while waiting, parking decoded frames. Without
        // that a decoder holding a frame never asks for input again.
        self.await_need_input()?;

        // SAFETY: stream 0; the transform consumes the sample.
        //
        // `MF_E_NOTACCEPTING` means the transform is holding output it wants
        // collected before it will take more input. Recovering here rather than
        // returning is what keeps a single missed drain from wedging the stream
        // permanently — every later submission would otherwise hit the same
        // rejection and look like a broken decoder.
        if let Err(e) = unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            if e.code() != MF_E_NOTACCEPTING {
                anyhow::bail!("decoder ProcessInput: {e}");
            }
            let pending = self.drain_pending()?;
            // SAFETY: retry after draining; stream 0 as above.
            unsafe { self.transform.ProcessInput(0, &sample, 0) }
                .map_err(|e| anyhow::anyhow!("decoder ProcessInput after drain: {e}"))?;
            if let Some(frame) = pending {
                // The drained frame is older than the one just submitted, so it
                // is the correct one to return now.
                return Ok(Some(frame));
            }
        }

        // Anything the transform is ready to hand over now, including output
        // completed by the frame just submitted.
        if self.output_ready(NEED_INPUT_TIMEOUT) {
            if let Some(frame) = self.take_output_frame()? {
                self.pending_frames.push_back(frame);
            }
        }

        // A decoder pipelines: the frame just submitted is not the one it hands
        // back, and on the first calls after construction — or before a
        // keyframe has been seen — it has nothing at all. That is `Ok(None)`,
        // not an error: reported as a failure it drives the receiver to request
        // keyframes and eventually rebuild this decoder, which restarts the
        // very warm-up being waited on. See `request_low_latency`, which is
        // what keeps the wait to a couple of frames.
        Ok(self.pending_frames.pop_front())
    }
}

/// Reject encoder settings Media Foundation would refuse or mis-handle.
fn validate_config(config: &MfEncoderConfig) -> Result<(), MfError> {
    if config.width == 0
        || config.height == 0
        || !config.width.is_multiple_of(2)
        || !config.height.is_multiple_of(2)
    {
        return Err(MfError::InvalidArgument(format!(
            "dimensions must be even and non-zero, got {}x{}",
            config.width, config.height
        )));
    }
    if config.fps == 0 || config.bitrate_bps == 0 {
        return Err(MfError::InvalidArgument(
            "fps and bitrate must be non-zero".into(),
        ));
    }
    Ok(())
}

/// Annex-B 4-byte start code. H.264 NAL units in a byte stream are separated by
/// this (or a 3-byte variant), and Media Foundation's H.264 MFT emits that form.
const ANNEX_B_START: [u8; 4] = [0x00, 0x00, 0x00, 0x01];

/// Read `MF_MT_MPEG_SEQUENCE_HEADER` (SPS/PPS) from a transform's current
/// output type. Empty when the transform does not publish one.
fn read_sequence_header(transform: &IMFTransform) -> Vec<u8> {
    // SAFETY: queries the negotiated output type; every failure path is
    // treated as "no header available" rather than an error, because a
    // transform emitting parameter sets in-band legitimately has none.
    unsafe {
        let Ok(mt) = transform.GetOutputCurrentType(0) else {
            return Vec::new();
        };
        // Two-call idiom: ask for the size, then fill a buffer of that size.
        let Ok(len) = mt.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) else {
            return Vec::new();
        };
        if len == 0 {
            return Vec::new();
        }
        let mut buf = vec![0u8; len as usize];
        let mut written: u32 = 0;
        if mt
            .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buf, Some(&mut written))
            .is_err()
        {
            return Vec::new();
        }
        // Trust the reported length over the buffer we sized, in case the
        // transform wrote less than it advertised.
        buf.truncate((written as usize).min(buf.len()));
        buf
    }
}

/// Whether `data` already begins with an Annex-B start code followed by an SPS
/// NAL (`nal_unit_type == 7`).
///
/// Used to avoid prepending a duplicate header when the encoder already emitted
/// one in-band. A duplicate is usually harmless, but some decoders treat an
/// unexpected second parameter set as a stream error.
fn starts_with_sps(data: &[u8]) -> bool {
    // 3-byte start codes are legal too, so check both framings.
    let after_start = if data.starts_with(&ANNEX_B_START) {
        4
    } else if data.starts_with(&ANNEX_B_START[1..]) {
        3
    } else {
        return false;
    };
    data.get(after_start).is_some_and(|nal| nal & 0x1F == 7)
}

/// Declare async support on a hardware MFT so it will accept media types.
///
/// Returns whether the transform is asynchronous. A hardware MFT rejects
/// `SetInputType` with `MF_E_TRANSFORM_ASYNC_LOCKED` until `MF_TRANSFORM_ASYNC_UNLOCK`
/// is set, and setting it is a promise that the caller drives the event queue —
/// which [`MfEncoder::encode`] does.
fn unlock_if_async(transform: &IMFTransform) -> Result<bool, MfError> {
    // SAFETY: transform is live; GetAttributes may legitimately fail on some
    // software MFTs, which simply means "not async".
    unsafe {
        let Ok(attrs) = transform.GetAttributes() else {
            return Ok(false);
        };
        let is_async = attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0;
        if is_async {
            attrs
                .SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
                .map_err(|e| MfError::Configure(format!("MF_TRANSFORM_ASYNC_UNLOCK: {e}")))?;
        }
        Ok(is_async)
    }
}

/// Ask a transform to hand each picture over as soon as it is decoded.
///
/// Microsoft's H.264 decoder defaults to a *playback* pipeline: it fills a
/// reorder buffer before releasing the first picture, which measures at around
/// **thirty submissions** — a full second at 30 fps — of `ProcessOutput`
/// returning nothing. For a media file that is invisible. For a call it is
/// fatal in two ways: a second of added latency, and, worse, a receiver whose
/// watchdogs conclude the decoder is broken and rebuild it long before it would
/// have produced anything. A rebuilt decoder starts the wait over, so the
/// stream never produces a single frame — the tile sits on "Waiting for video…"
/// while frames arrive at full rate.
///
/// `MF_LOW_LATENCY` is the documented switch: it tells the transform the stream
/// is real time, so it emits each picture immediately and skips the reordering
/// that a call's bitstream (no B-frames) does not need anyway.
///
/// Best-effort on purpose. Not every transform implements either control, and
/// one that ignores both is merely slow to start rather than broken — which is
/// precisely the case [`super::receiver`] now tolerates instead of treating as
/// a decode failure.
fn request_low_latency(transform: &IMFTransform) {
    // SAFETY: reading/setting attributes on a live transform. Both controls are
    // optional; a failure costs an optimisation, never correctness.
    unsafe {
        match transform.GetAttributes() {
            Ok(attrs) => {
                if let Err(e) = attrs.SetUINT32(&MF_LOW_LATENCY, 1) {
                    debug!("[video] transform refused MF_LOW_LATENCY: {e}");
                }
            }
            Err(e) => debug!("[video] transform exposes no attribute store: {e}"),
        }
        // Some hardware decoders expose the same idea only through ICodecAPI.
        // VT_BOOL is what this property is defined as — the tag has to match or
        // the codec rejects the value, exactly as for the bitrate VARIANT.
        if let Ok(codec_api) = transform.cast::<ICodecAPI>() {
            let value = VARIANT::from(true);
            let _ = codec_api.SetValue(&CODECAPI_AVLowLatencyMode, &value);
        }
    }
}

/// How long to wait for a hardware transform to ask for input before giving up
/// on this frame.
///
/// Deliberately bounded. A blocking `GetEvent` deadlocks the calling thread
/// outright if the transform never posts the event — which is exactly what a
/// misconfigured hardware MFT does — and on the capture thread that would
/// freeze the whole call rather than merely drop a frame. Dropping a frame is
/// always the better failure here.
const NEED_INPUT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// Build an `IMFSample` wrapping a copy of `data`.
fn make_sample(data: &[u8], timestamp: i64, duration: i64) -> windows::core::Result<IMFSample> {
    // SAFETY: buffer is created with the requested length and released by COM
    // refcounting when the sample drops.
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
        let mut dst: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer.Lock(&mut dst, Some(&mut max_len), Some(&mut cur_len))?;
        if !dst.is_null() && max_len as usize >= data.len() {
            std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        buffer.Unlock()?;
        buffer.SetCurrentLength(data.len() as u32)?;

        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(timestamp)?;
        sample.SetSampleDuration(duration)?;
        Ok(sample)
    }
}

/// Pull one encoded sample out of a transform, if it has one ready.
///
/// Returns `(bitstream, is_keyframe)`.
fn drain_output(
    transform: &IMFTransform,
    min_buffer: u32,
) -> anyhow::Result<Option<(Vec<u8>, bool)>> {
    // SAFETY: stream 0; the output sample is allocated by us when the MFT does
    // not provide its own.
    unsafe {
        let info = transform
            .GetOutputStreamInfo(0)
            .map_err(|e| anyhow::anyhow!("GetOutputStreamInfo: {e}"))?;

        let provides_samples = info.dwFlags
            & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32
                | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32)
            != 0;

        let mut buffers = [MFT_OUTPUT_DATA_BUFFER::default()];
        if !provides_samples {
            // `cbSize` is frequently reported as 0 by encoders that expect the
            // caller to pick a size. Allocating that literally yields a 1-byte
            // buffer and every ProcessOutput fails with a buffer-too-small
            // error that surfaces as "no video" rather than as an error.
            let buffer = MFCreateMemoryBuffer(info.cbSize.max(min_buffer).max(4096))?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            buffers[0].pSample = std::mem::ManuallyDrop::new(Some(sample));
        }

        let mut status = 0u32;
        let hr = transform.ProcessOutput(0, &mut buffers, &mut status);
        if let Err(e) = hr {
            // "Not ready yet" — normal while the pipeline fills.
            if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                return Ok(None);
            }
            // An async transform answers E_UNEXPECTED when asked for output it
            // did not signal. That happens legitimately after a flush: the
            // METransformHaveOutput events already in flight are delivered
            // *after* the queue is cleared, so the next collection acts on an
            // event describing a sample the flush discarded. Treating it as
            // "nothing to give" is what the contract implies; treating it as
            // fatal killed the stream intermittently, which is far worse than
            // skipping one collection.
            if e.code() == windows::Win32::Foundation::E_UNEXPECTED {
                debug!("[video] ProcessOutput on a stale output event; ignoring");
                return Ok(None);
            }
            // The transform wants a new output media type. This must be
            // *acted on*: until the type is renegotiated every subsequent
            // ProcessOutput fails the same way, so silently ignoring it
            // produces a stream that never emits a single frame.
            if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                renegotiate_output_type(transform)?;
                return Ok(None);
            }
            return Err(anyhow::anyhow!("ProcessOutput: {e}"));
        }

        let sample_opt = std::mem::ManuallyDrop::take(&mut buffers[0].pSample);
        let Some(sample) = sample_opt else {
            return Ok(None);
        };

        let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;

        let buffer = sample.ConvertToContiguousBuffer()?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len = 0u32;
        let mut cur_len = 0u32;
        buffer.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))?;
        let data = if ptr.is_null() || cur_len == 0 {
            Vec::new()
        } else {
            std::slice::from_raw_parts(ptr, cur_len as usize).to_vec()
        };
        buffer.Unlock()?;

        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some((data, keyframe)))
    }
}

/// Set the decoder's output type by walking its own offered list.
///
/// The list is enumerated rather than fabricated: index 0 is not necessarily
/// NV12, and a hand-built type would have to guess a frame size the decoder has
/// not derived yet. Each candidate is tried in order and the first accepted one
/// wins, preferring NV12 since that is what the render path converts from.
fn select_decoder_output_type(transform: &IMFTransform) -> Result<(), MfError> {
    let mut last_err = String::from("no output types offered");

    // SAFETY: enumeration terminates on MF_E_NO_MORE_TYPES; every call is on a
    // live transform with its input type already set.
    unsafe {
        // Two passes: take NV12 if offered, otherwise whatever is accepted.
        for want_nv12 in [true, false] {
            for index in 0..32u32 {
                let mt = match transform.GetOutputAvailableType(0, index) {
                    Ok(mt) => mt,
                    // Exhausted, or the transform has no list at all.
                    Err(_) => break,
                };
                if want_nv12 {
                    let is_nv12 = mt
                        .GetGUID(&MF_MT_SUBTYPE)
                        .map(|g| g == MFVideoFormat_NV12)
                        .unwrap_or(false);
                    if !is_nv12 {
                        continue;
                    }
                }
                match transform.SetOutputType(0, &mt, 0) {
                    Ok(()) => {
                        debug!("[video] decoder output type set (index {index}, nv12={want_nv12})");
                        return Ok(());
                    }
                    Err(e) => last_err = format!("index {index}: {e}"),
                }
            }
        }
    }

    Err(MfError::Configure(format!(
        "decoder would not accept any offered output type ({last_err})"
    )))
}

/// Accept the transform's own preferred output type after a stream change.
///
/// Media Foundation signals `MF_E_TRANSFORM_STREAM_CHANGE` when it wants the
/// output type reset; the contract is to enumerate available types and set one
/// before pulling output again.
fn renegotiate_output_type(transform: &IMFTransform) -> anyhow::Result<()> {
    // SAFETY: enumerating output types on a live transform; index 0 is the
    // transform's own first preference.
    unsafe {
        let mt = transform
            .GetOutputAvailableType(0, 0)
            .map_err(|e| anyhow::anyhow!("GetOutputAvailableType: {e}"))?;
        transform
            .SetOutputType(0, &mt, 0)
            .map_err(|e| anyhow::anyhow!("SetOutputType after stream change: {e}"))?;
    }
    Ok(())
}

/// A value to stamp onto an `IMFMediaType`.
enum MediaTypeValue {
    Guid(GUID),
    U32(u32),
    U64(u64),
}

fn create_media_type(attrs: &[(GUID, MediaTypeValue)]) -> Result<IMFMediaType, MfError> {
    // SAFETY: each setter targets the freshly created media type.
    unsafe {
        let mt = MFCreateMediaType()
            .map_err(|e| MfError::Configure(format!("MFCreateMediaType: {e}")))?;
        for (key, value) in attrs {
            let r = match value {
                MediaTypeValue::Guid(g) => mt.SetGUID(key, g),
                MediaTypeValue::U32(v) => mt.SetUINT32(key, *v),
                MediaTypeValue::U64(v) => mt.SetUINT64(key, *v),
            };
            r.map_err(|e| MfError::Configure(format!("set attribute {key:?}: {e}")))?;
        }
        Ok(mt)
    }
}

// Referenced so the import stays used on all build configurations.
const _: Option<PCWSTR> = None;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_ratio_matches_media_foundation_layout() {
        // MF packs the pair as (high << 32) | low.
        assert_eq!(pack_ratio(640, 360), (640u64 << 32) | 360);
        assert_eq!(pack_ratio(30, 1), (30u64 << 32) | 1);
    }

    // ── SPS detection / sequence-header prefixing ───────────────────────────

    #[test]
    fn detects_sps_after_a_four_byte_start_code() {
        // 0x67 = start code + nal_ref_idc(3) + nal_unit_type 7 (SPS).
        assert!(starts_with_sps(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x42]));
    }

    #[test]
    fn detects_sps_after_a_three_byte_start_code() {
        // Both framings are legal Annex-B; only checking the 4-byte form would
        // miss encoders that emit the short one and cause a duplicate header.
        assert!(starts_with_sps(&[0x00, 0x00, 0x01, 0x67, 0x42]));
    }

    #[test]
    fn does_not_mistake_other_nal_types_for_sps() {
        // 0x65 = IDR slice (type 5), 0x68 = PPS (type 8), 0x41 = non-IDR (1).
        // An IDR without parameter sets is precisely the case that needs the
        // header prepended, so treating it as SPS would defeat the fix.
        assert!(!starts_with_sps(&[0x00, 0x00, 0x00, 0x01, 0x65]));
        assert!(!starts_with_sps(&[0x00, 0x00, 0x00, 0x01, 0x68]));
        assert!(!starts_with_sps(&[0x00, 0x00, 0x00, 0x01, 0x41]));
    }

    #[test]
    fn requires_a_start_code() {
        // A bare 0x67 with no start code is not a byte-stream NAL.
        assert!(!starts_with_sps(&[0x67, 0x42, 0x00]));
        assert!(!starts_with_sps(&[]));
        assert!(!starts_with_sps(&[0x00, 0x00]));
        // Truncated right after the start code — must not index past the end.
        assert!(!starts_with_sps(&[0x00, 0x00, 0x00, 0x01]));
    }

    #[test]
    fn nal_type_is_read_from_the_low_five_bits() {
        // nal_ref_idc occupies bits 5-6, so several byte values are all SPS.
        for b in [0x07u8, 0x27, 0x47, 0x67] {
            assert!(
                starts_with_sps(&[0x00, 0x00, 0x00, 0x01, b]),
                "{b:#04x} has nal_unit_type 7 and must count as SPS"
            );
        }
        // The forbidden_zero_bit being set does not change the type bits.
        assert!(starts_with_sps(&[0x00, 0x00, 0x00, 0x01, 0x87]));
    }

    #[test]
    fn encoder_rejects_bad_configuration_before_touching_com() {
        for cfg in [
            MfEncoderConfig {
                width: 0,
                ..Default::default()
            },
            MfEncoderConfig {
                width: 641,
                ..Default::default()
            },
            MfEncoderConfig {
                height: 361,
                ..Default::default()
            },
            MfEncoderConfig {
                fps: 0,
                ..Default::default()
            },
            MfEncoderConfig {
                bitrate_bps: 0,
                ..Default::default()
            },
        ] {
            assert!(
                matches!(MfEncoder::new(cfg), Err(MfError::InvalidArgument(_))),
                "expected {cfg:?} to be rejected"
            );
        }
    }

    /// Exercises the real Media Foundation stack. Ignored by default because CI
    /// containers have no media stack; run explicitly on a workstation:
    /// `cargo test --bins -- --ignored mediafoundation`
    #[test]
    #[ignore = "requires a Windows machine with a Media Foundation H.264 encoder"]
    fn hardware_encoder_round_trips_a_frame() {
        let cfg = MfEncoderConfig {
            width: 320,
            height: 240,
            bitrate_bps: 400_000,
            fps: 30,
            keyframe_interval_secs: 4,
        };
        // Report the hardware attempt on its own before the fallback chain
        // swallows it. `MfEncoder::new` deliberately degrades to software, so
        // without this a machine that *has* NVENC and a machine whose NVENC is
        // broken look identical from the outside.
        match MfEncoder::with_preference(cfg, true) {
            Ok(mut hw) => match hw.probe_with_reason() {
                Ok(()) => println!("hardware probe: OK ({:?})", hw.acceleration()),
                Err(why) => println!("hardware probe FAILED: {why}"),
            },
            Err(e) => println!("hardware transform unavailable: {e}"),
        }

        let mut enc = MfEncoder::new(cfg).expect("create encoder");
        println!("acceleration: {:?}", enc.acceleration());

        let frame = RawFrame::test_pattern(320, 240, 1);
        // Hardware encoders pipeline, so feed several frames before expecting
        // output rather than asserting on the first.
        let mut produced = 0usize;
        for _ in 0..30 {
            let (data, _key) = enc.encode(&frame).expect("encode");
            if !data.is_empty() {
                produced += 1;
            }
        }
        assert!(produced > 0, "encoder produced no bitstream in 30 frames");
    }

    /// ABR retargets a *live* encoder. The property that matters is not that
    /// the rate change succeeds — `ICodecAPI` is genuinely optional — but that
    /// a refused change cannot wedge the stream. An encoder that stops
    /// producing after an adaptation attempt would black out the call at
    /// exactly the moment the network was already struggling.
    #[test]
    #[ignore = "requires a Windows machine with a Media Foundation H.264 encoder"]
    fn retargeting_bitrate_does_not_wedge_a_running_encoder() {
        let cfg = MfEncoderConfig {
            width: 320,
            height: 240,
            bitrate_bps: 400_000,
            fps: 30,
            keyframe_interval_secs: 4,
        };
        let mut enc = MfEncoder::new(cfg).expect("create encoder");
        let frame = RawFrame::test_pattern(320, 240, 3);
        for _ in 0..10 {
            let _ = enc.encode(&frame).expect("warmup encode");
        }

        match enc.set_bitrate(200_000) {
            Ok(()) => println!("bitrate retarget OK ({:?})", enc.acceleration()),
            Err(e) => println!(
                "bitrate retarget unsupported ({:?}): {e}",
                enc.acceleration()
            ),
        }

        let mut produced = 0usize;
        for _ in 0..30 {
            let (data, _) = enc.encode(&frame).expect("encode after retarget");
            if !data.is_empty() {
                produced += 1;
            }
        }
        assert!(
            produced > 0,
            "encoder produced nothing after a bitrate change"
        );
    }

    /// Full pipeline against the real OS codec: I420 -> NV12 -> H.264 ->
    /// NV12 -> I420.
    ///
    /// Full pipeline against the real OS codec: I420 -> NV12 -> H.264 -> NV12
    /// -> I420.
    ///
    /// Two things this test's shape encodes, both learned the hard way:
    ///
    /// * **Encode and decode are separate phases.** Interleaving them hides an
    ///   undrained output sample: once `ProcessOutput` has not been pumped, the
    ///   decoder legitimately answers `MF_E_NOTACCEPTING` ("I have output to
    ///   produce") to every later `ProcessInput`, which reads as a permanent
    ///   configuration failure rather than a transient one.
    /// * **Both halves pipeline deeply.** The encoder emitted nothing for its
    ///   first 12 frames and the decoder returned nothing until submission ~30,
    ///   so asserting on the first frame of either would fail on a healthy
    ///   codec.
    ///
    /// Ignored by default because CI has no media stack; run on a workstation:
    /// `cargo test --bins -- --ignored mediafoundation`
    #[test]
    #[ignore = "requires a Windows machine with Media Foundation H.264"]
    fn encode_decode_round_trips_through_media_foundation() {
        let cfg = MfEncoderConfig {
            width: 320,
            height: 240,
            bitrate_bps: 800_000,
            fps: 30,
            keyframe_interval_secs: 1,
        };
        let mut enc = MfEncoder::new(cfg).expect("create encoder");
        // As with the encoder, report the hardware attempt before the fallback
        // hides it — software decode is expected here, but "expected" should be
        // observable rather than assumed.
        match MfDecoder::with_preference(true) {
            Ok(hw) => println!("hardware decoder: OK ({:?})", hw.acceleration()),
            Err(e) => println!("hardware decoder unavailable: {e}"),
        }

        let mut dec = MfDecoder::new().expect("create decoder");
        println!("encoder acceleration: {:?}", enc.acceleration());
        println!("decoder acceleration: {:?}", dec.acceleration());
        println!(
            "sequence header: {} bytes {:02x?}",
            enc.sequence_header().len(),
            &enc.sequence_header()[..enc.sequence_header().len().min(24)]
        );

        // Encode first, decode second. Interleaving them made it impossible to
        // tell an encoder pipeline delay from a decoder rejection.
        let source = RawFrame::test_pattern(320, 240, 4);
        let mut encoded_frames: Vec<(Vec<u8>, bool)> = Vec::new();
        for _ in 0..60 {
            let (data, key) = enc.encode(&source).expect("encode");
            if !data.is_empty() {
                encoded_frames.push((data, key));
            }
        }
        println!("encoder produced {} frames", encoded_frames.len());
        assert!(!encoded_frames.is_empty(), "encoder produced nothing");
        println!(
            "first: {} bytes key={} sps={} prefix={:02x?}",
            encoded_frames[0].0.len(),
            encoded_frames[0].1,
            starts_with_sps(&encoded_frames[0].0),
            &encoded_frames[0].0[..encoded_frames[0].0.len().min(16)]
        );

        let mut decoded = None;
        let mut decoded_at = usize::MAX;
        for (n, (data, _key)) in encoded_frames.iter().enumerate() {
            match dec.decode(data) {
                Ok(Some(frame)) => {
                    println!("decoded on submission {n}");
                    decoded = Some(frame);
                    decoded_at = n;
                    break;
                }
                // Accepted, nothing out yet — the pipeline filling. Print the
                // first few so a warm-up is distinguishable from a rejection.
                Ok(None) => {
                    if n < 4 {
                        println!("submission {n}: no picture yet");
                    }
                }
                Err(e) => {
                    if n < 4 {
                        println!("submission {n} failed: {e}");
                    }
                }
            }
        }

        let out = decoded.expect("decoder never produced a frame");
        // Low latency is the difference between a couple of submissions and
        // about thirty — see `request_low_latency`. Asserting the bound here is
        // what keeps a regression in that setup from reading as merely "slow".
        assert!(
            decoded_at < 10,
            "first picture took {decoded_at} submissions — low-latency mode is not in effect"
        );
        assert_eq!((out.width, out.height), (320, 240));
        assert!(out.is_consistent(), "decoded frame planes are inconsistent");
        // H.264 is lossy, so compare structure rather than exact bytes: a
        // correctly wired pipeline reproduces the gradient's rough shape, while
        // a stride or plane-order bug produces noise or a flat field.
        let first_row_spread = out.y[..320].iter().copied().max().unwrap() as i32
            - out.y[..320].iter().copied().min().unwrap() as i32;
        assert!(
            first_row_spread > 40,
            "decoded luma row is too flat ({first_row_spread}) — the gradient did not survive"
        );
    }
}
