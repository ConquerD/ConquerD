//! Capturing the audio a machine is *playing*, to send alongside video.
//!
//! Screen sharing a game or a video is close to useless silent, but the call
//! microphone is the wrong source for it: it would pick up the room, not the
//! application. What is wanted is the audio the OS is rendering — a loopback of
//! the output device.
//!
//! # Why this is not just "another input device"
//!
//! On every platform loopback is a distinct API rather than a device in the
//! normal capture list, which is why this does not reuse the CPAL path the mic
//! uses:
//!
//! * **Windows** — WASAPI, by two separate paths. Whole-machine audio opens the
//!   *render* endpoint with `AUDCLNT_STREAMFLAGS_LOOPBACK`; one application's
//!   audio has no endpoint to open at all and goes through
//!   `ActivateAudioInterfaceAsync` against a virtual device instead. Both are
//!   implemented here.
//! * **Linux** — PipeWire or a PulseAudio `.monitor` source. Unbuilt.
//! * **macOS** — no first-party loopback at all; needs a virtual device or
//!   ScreenCaptureKit's audio tap. Unbuilt, and the hardest of the three.
//!
//! # Timing
//!
//! Frames carry the offset the *device* reports, not a count of frames handed
//! out — see [`ContentFrame::offset_us`] and [`CaptureTimeline`]. This matters
//! far more for per-application capture than for whole-machine capture: one app
//! is silent most of the time, and a counting timeline would treat each of those
//! silences as though it had never happened.
//!
//! # Format contract
//!
//! Callers get 20 ms frames of mono `f32` at [`SAMPLE_RATE`], matching what the
//! Opus encoder wants — the same shape the mic path produces, so everything
//! downstream is shared. The device decides its own rate and channel count, so
//! [`downmix_to_mono`] and [`resample_linear`] bridge the gap; both are pure and
//! tested independently of any audio hardware.
//!
//! # Echo hazard
//!
//! Loopback captures *everything* the machine plays, which includes remote
//! peers' voices coming out of the speakers. Sending that back is a feedback
//! loop. Mitigation (excluding our own render stream, ducking, or requiring a
//! virtual cable) belongs with the send path, not here — this module's job is
//! to report what the device is playing, honestly.

use crate::call_controller::{SAMPLES_PER_FRAME, SAMPLE_RATE};

/// One 20 ms mono frame at [`SAMPLE_RATE`], with when the device captured it.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentFrame {
    /// [`SAMPLES_PER_FRAME`] mono samples.
    pub samples: Vec<f32>,
    /// Microseconds from this source's first captured sample to this frame's
    /// first sample, **as the capture device reports it**.
    ///
    /// Not a count of frames returned. The difference only shows up when the
    /// device stops producing — which is the normal state of a loopback
    /// endpoint, since an application that is not playing anything generates no
    /// packets at all. A counter would resume as though no time had passed, so
    /// every silence would push the audio permanently further behind the video
    /// it is supposed to be synchronised with. This offset skips the gap
    /// exactly, because the device timestamps it for us.
    pub offset_us: u64,
}

/// A source of content audio, in 20 ms mono `f32` frames at [`SAMPLE_RATE`].
pub trait ContentAudioSource: Send {
    /// Pull the next frame. Blocks until one is available or the device errors.
    ///
    /// Silence is a normal result, not an error: nothing playing is exactly
    /// what a loopback device reports most of the time.
    fn next_frame(&mut self) -> anyhow::Result<ContentFrame>;
}

/// Forward the trait through a box, so [`open_default`]'s
/// `Box<dyn ContentAudioSource>` satisfies the generic `S: ContentAudioSource`
/// bound that [`ContentAudioSender::start`](crate::content_sender::ContentAudioSender::start)
/// takes. Without this only callers willing to name a concrete backend could
/// use the sender — which is exactly what choosing one per platform rules out.
impl ContentAudioSource for Box<dyn ContentAudioSource> {
    fn next_frame(&mut self) -> anyhow::Result<ContentFrame> {
        (**self).next_frame()
    }
}

/// Placeholder for platforms without a loopback backend.
pub struct NullContentAudio;

impl ContentAudioSource for NullContentAudio {
    fn next_frame(&mut self) -> anyhow::Result<ContentFrame> {
        anyhow::bail!("content audio capture is not implemented on this platform")
    }
}

/// Which audio to capture alongside a video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentAudioSpec {
    /// Nothing. Sharing a camera does not imply sharing the machine's audio,
    /// and the microphone already carries the person on it.
    None,
    /// Everything the machine is playing.
    System,
    /// Only what one process is playing.
    ///
    /// Needs Windows 10 build 20348 or later; older systems have no process
    /// loopback API and fall back to [`System`](Self::System).
    Process(u32),
}

/// What the user asked for, before it is resolved against the video source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContentAudioMode {
    /// Follow the video source: an app shares its own audio, a monitor shares
    /// everything, a camera shares nothing.
    #[default]
    Auto,
    /// Always the whole machine, whatever is being shared.
    System,
    /// Never capture content audio.
    Off,
}

impl ContentAudioMode {
    /// Parse the persisted settings string. Unknown values mean `Auto`, so a
    /// settings file written by a newer build degrades rather than failing.
    pub fn from_setting(raw: &str) -> Self {
        match raw {
            "system" => Self::System,
            "off" => Self::Off,
            _ => Self::Auto,
        }
    }

    /// The value persisted in settings.
    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::System => "system",
            Self::Off => "off",
        }
    }
}

/// Resolve what to capture from the mode and the video source.
///
/// `source_pid` is the process behind the shared window, when the video source
/// is a window and its owner is known; `is_screen` distinguishes a monitor or
/// window share from a camera.
///
/// The auto rule is the one users expect without being told: sharing an
/// application shares that application, sharing a screen shares the machine,
/// and pointing a camera at yourself shares nothing extra — your microphone
/// already carries you, and capturing the speakers as well would feed remote
/// voices straight back out.
pub fn resolve_audio_spec(
    mode: ContentAudioMode,
    is_screen: bool,
    source_pid: Option<u32>,
) -> ContentAudioSpec {
    match mode {
        ContentAudioMode::Off => ContentAudioSpec::None,
        ContentAudioMode::System => ContentAudioSpec::System,
        ContentAudioMode::Auto => match (is_screen, source_pid) {
            // A window whose owner we know: capture just that app.
            (true, Some(pid)) => ContentAudioSpec::Process(pid),
            // A monitor, or a window we could not attribute.
            (true, None) => ContentAudioSpec::System,
            // A camera.
            (false, _) => ContentAudioSpec::None,
        },
    }
}

/// Open the loopback source described by `spec`.
///
/// A per-process capture that cannot be opened falls back to whole-system
/// audio rather than failing: process loopback needs Windows 10 build 20348,
/// and on an older machine hearing the whole desktop is much closer to what
/// was asked for than hearing nothing.
pub fn open_for(spec: ContentAudioSpec) -> anyhow::Result<Box<dyn ContentAudioSource>> {
    match spec {
        ContentAudioSpec::None => anyhow::bail!("this video source shares no audio"),
        #[cfg(target_os = "windows")]
        ContentAudioSpec::Process(pid) => match windows_impl::WasapiLoopback::open_process(pid) {
            Ok(c) => Ok(Box::new(c)),
            Err(e) => {
                tracing::warn!(
                    "[content-audio] per-app capture unavailable ({e}); \
                     falling back to system audio"
                );
                Ok(Box::new(windows_impl::WasapiLoopback::open_system()?))
            }
        },
        #[cfg(target_os = "windows")]
        ContentAudioSpec::System => Ok(Box::new(windows_impl::WasapiLoopback::open_system()?)),
        #[cfg(not(target_os = "windows"))]
        ContentAudioSpec::Process(_) | ContentAudioSpec::System => anyhow::bail!(
            "sharing system audio is not implemented on this platform yet \
             (Linux needs a PipeWire/PulseAudio monitor source; macOS needs a \
             virtual device or ScreenCaptureKit)"
        ),
    }
}

/// Open whole-system loopback.
pub fn open_default() -> anyhow::Result<Box<dyn ContentAudioSource>> {
    open_for(ContentAudioSpec::System)
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{
        downmix_to_mono, CaptureTimeline, ContentAudioSource, ContentFrame, FrameAccumulator,
        Resampler,
    };

    use std::collections::VecDeque;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use windows::core::{implement, Interface, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    /// How long to wait for the endpoint to fill before polling again.
    ///
    /// Half a frame: long enough not to spin, short enough that a 20 ms frame
    /// is never more than one poll late.
    const POLL_MS: u64 = 10;

    /// `WAVEFORMATEX` tags. The `windows` crate exposes these only as `u32`
    /// constants in unrelated modules, while `wFormatTag` is a `u16`.
    const WAVE_FORMAT_PCM: u16 = 0x0001;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 0x0003;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

    /// How long to wait for the asynchronous activation to complete.
    ///
    /// Generous: this is a one-off at share start, and a hang here would be a
    /// frozen UI. The timeout exists so a driver that never calls back degrades
    /// to system audio instead of wedging the thread forever.
    const ACTIVATE_TIMEOUT: Duration = Duration::from_secs(3);

    /// `VT_BLOB`. Not exported by the `windows` crate's public surface.
    const VT_BLOB_TAG: u16 = 65;

    /// A `PROPVARIANT` holding a blob.
    ///
    /// `ActivateAudioInterfaceAsync` takes its parameters as a `VT_BLOB`
    /// PROPVARIANT, but `windows_core::PROPVARIANT` wraps a private
    /// representation with no way to construct that variant. This is the same
    /// C layout, passed by pointer, which is all the ABI requires — the size
    /// assertion below is what keeps that claim honest if the crate's
    /// definition ever changes.
    #[repr(C)]
    struct PropVariantBlob {
        vt: u16,
        reserved1: u16,
        reserved2: u16,
        reserved3: u16,
        cb_size: u32,
        _pad: u32,
        p_blob_data: *mut u8,
    }

    const _: () = assert!(
        std::mem::size_of::<PropVariantBlob>() == std::mem::size_of::<windows::core::PROPVARIANT>(),
        "PROPVARIANT layout changed; the blob shim no longer matches the ABI"
    );

    /// Signals that an asynchronous activation has finished.
    ///
    /// Deliberately carries no result: COM interfaces are not `Send`, so
    /// smuggling the activated `IAudioClient` out through shared state would not
    /// compile. The operation object is held by the caller anyway, so the
    /// handler only has to say "done" and let the caller read the result on its
    /// own thread.
    #[implement(IActivateAudioInterfaceCompletionHandler)]
    struct ActivationHandler {
        done: Arc<(Mutex<bool>, Condvar)>,
    }

    #[allow(non_snake_case)]
    impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationHandler_Impl {
        fn ActivateCompleted(
            &self,
            _op: Option<&IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            let (lock, cv) = &*self.done;
            if let Ok(mut done) = lock.lock() {
                *done = true;
            }
            cv.notify_all();
            Ok(())
        }
    }

    /// Activate an `IAudioClient` bound to one process's audio rather than to a
    /// device endpoint.
    ///
    /// There is no `IMMDevice` for "one application", so this goes through the
    /// virtual `VAD\Process_Loopback` device and tells it which process tree to
    /// include. Requires Windows 10 build 20348; older systems fail the
    /// activation, which is what routes callers back to system audio.
    fn activate_process_client(pid: u32) -> anyhow::Result<IAudioClient> {
        // SAFETY: the activation parameters and the PROPVARIANT shim are locals
        // that outlive the call, which consumes them synchronously; the
        // completion handler is refcounted by COM.
        unsafe {
            // Already-initialised is not an error: Qt or Media Foundation may
            // have set the apartment up first.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let mut params = AUDIOCLIENT_ACTIVATION_PARAMS {
                ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
                ..Default::default()
            };
            // The tree, not just the one process: browsers and games routinely
            // render audio from a child process, and excluding those would
            // capture silence from a window that is plainly making noise.
            params.Anonymous.ProcessLoopbackParams = AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            };

            let blob = PropVariantBlob {
                vt: VT_BLOB_TAG,
                reserved1: 0,
                reserved2: 0,
                reserved3: 0,
                cb_size: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
                _pad: 0,
                p_blob_data: std::ptr::addr_of_mut!(params) as *mut u8,
            };

            let done = Arc::new((Mutex::new(false), Condvar::new()));
            let handler: IActivateAudioInterfaceCompletionHandler = ActivationHandler {
                done: Arc::clone(&done),
            }
            .into();

            let operation = ActivateAudioInterfaceAsync(
                VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
                &IAudioClient::IID,
                Some(std::ptr::addr_of!(blob) as *const windows::core::PROPVARIANT),
                &handler,
            )?;

            {
                let (lock, cv) = &*done;
                let mut ready = lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("activation lock poisoned"))?;
                while !*ready {
                    let (guard, timeout) = cv
                        .wait_timeout(ready, ACTIVATE_TIMEOUT)
                        .map_err(|_| anyhow::anyhow!("activation lock poisoned"))?;
                    ready = guard;
                    if timeout.timed_out() && !*ready {
                        anyhow::bail!("process loopback activation timed out");
                    }
                }
            }

            // Two failures to distinguish: the call to fetch the result, and
            // the result itself. A build without process loopback reports the
            // second, and reporting it as success would hand back a null
            // interface to dereference later.
            let mut hr = windows::core::HRESULT(0);
            let mut unknown: Option<windows::core::IUnknown> = None;
            operation.GetActivateResult(&mut hr, &mut unknown)?;
            hr.ok()?;

            let client: IAudioClient = unknown
                .ok_or_else(|| anyhow::anyhow!("activation returned no interface"))?
                .cast()?;
            Ok(client)
        }
    }

    /// How the endpoint hands us samples.
    ///
    /// The shared-mode *endpoint* mix format is always 32-bit float, but the
    /// process-loopback virtual device has no mix format to ask for and takes
    /// whichever of these it is offered — so both have to be readable.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum SampleKind {
        F32,
        I16,
    }

    /// WASAPI loopback capture of the default render endpoint.
    ///
    /// Loopback is a *render* endpoint opened for capture, not a capture
    /// endpoint — that flag is the entire difference between hearing the
    /// machine and hearing the microphone.
    pub struct WasapiLoopback {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        channels: usize,
        rate: u32,
        kind: SampleKind,
        /// Held across device reads, not rebuilt per read: see [`Resampler`].
        resampler: Resampler,
        accumulator: FrameAccumulator,
        timeline: CaptureTimeline,
        ready: VecDeque<ContentFrame>,
        /// Set for the event-driven process-loopback path; `None` when polling.
        event: Option<HANDLE>,
    }

    // SAFETY: the COM interfaces are owned exclusively by this struct and every
    // method takes `&mut self`, so they move between threads but are never
    // shared. The capture thread owns one for its lifetime.
    unsafe impl Send for WasapiLoopback {}

    impl WasapiLoopback {
        /// Capture only what process `pid` is playing.
        ///
        /// Uses WASAPI's process loopback, which is a different activation path
        /// from the endpoint one below: there is no `IMMDevice` for "one app",
        /// so the client is activated against a virtual device that is told
        /// which process tree to include.
        ///
        /// Requires Windows 10 build 20348 or later. Callers are expected to
        /// fall back to [`open_system`](Self::open_system) on error rather than
        /// surfacing a failure — see `open_for`.
        pub fn open_process(pid: u32) -> anyhow::Result<Self> {
            // The virtual device does not answer `GetMixFormat` — there is no
            // endpoint to have one — so the format is ours to choose rather
            // than to discover. Float first because it is what the audio engine
            // mixes in and so costs no conversion; 16-bit is the format the
            // documented sample uses, kept as a fallback because a refusal here
            // would otherwise drop the user all the way back to capturing the
            // whole machine.
            let mut last: Option<anyhow::Error> = None;
            for kind in [SampleKind::F32, SampleKind::I16] {
                match Self::open_process_with(pid, kind) {
                    Ok(s) => return Ok(s),
                    Err(e) => {
                        tracing::debug!("[content-audio] process loopback as {kind:?} failed: {e}");
                        last = Some(e);
                    }
                }
            }
            Err(last.unwrap_or_else(|| anyhow::anyhow!("process loopback could not be opened")))
        }

        fn open_process_with(pid: u32, kind: SampleKind) -> anyhow::Result<Self> {
            // Fixed rather than negotiated: this is a virtual device with no
            // hardware preference, so asking for the rate the pipeline already
            // wants avoids a resample that would otherwise be pure loss.
            const CHANNELS: u16 = 2;
            const RATE: u32 = super::SAMPLE_RATE;

            let client = activate_process_client(pid)?;

            // SAFETY: `client` is a live COM object from the activation above;
            // every call is checked and the event handle outlives the client.
            unsafe {
                let bits: u16 = match kind {
                    SampleKind::F32 => 32,
                    SampleKind::I16 => 16,
                };
                let block_align = CHANNELS * bits / 8;
                let format = WAVEFORMATEX {
                    wFormatTag: match kind {
                        SampleKind::F32 => WAVE_FORMAT_IEEE_FLOAT,
                        SampleKind::I16 => WAVE_FORMAT_PCM,
                    },
                    nChannels: CHANNELS,
                    nSamplesPerSec: RATE,
                    nAvgBytesPerSec: RATE * block_align as u32,
                    nBlockAlign: block_align,
                    wBitsPerSample: bits,
                    cbSize: 0,
                };

                // Process loopback is event-driven: unlike an endpoint, the
                // virtual device is not initialised in polling mode, so the
                // event handle is required rather than an optimisation.
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    2_000_000, // 200 ms, in 100-ns units
                    0,         // shared mode: periodicity must be zero
                    &format,
                    None,
                )?;

                // Auto-reset, initially unsignalled: each wait consumes exactly
                // one period's notification.
                let event = CreateEventW(None, false, false, PCWSTR::null())?;
                if let Err(e) = client.SetEventHandle(event) {
                    let _ = CloseHandle(event);
                    return Err(e.into());
                }

                let capture: IAudioCaptureClient = client.GetService()?;
                if let Err(e) = client.Start() {
                    let _ = CloseHandle(event);
                    return Err(e.into());
                }

                Ok(Self {
                    client,
                    capture,
                    channels: CHANNELS as usize,
                    rate: RATE,
                    kind,
                    resampler: Resampler::new(RATE),
                    accumulator: FrameAccumulator::new(),
                    timeline: CaptureTimeline::new(),
                    ready: VecDeque::new(),
                    event: Some(event),
                })
            }
        }

        /// Open the default playback device for whole-system loopback capture.
        pub fn open_system() -> anyhow::Result<Self> {
            // SAFETY: every COM call is checked; the apartment is initialised
            // per-thread and interfaces are released by their Drop impls.
            unsafe {
                // Already-initialised is not an error: the process may have a
                // COM apartment from Qt or Media Foundation already.
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

                let enumerator: IMMDeviceEnumerator =
                    CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
                // eRender, not eCapture: loopback taps what is being played.
                let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
                let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

                // The endpoint dictates its own mix format; converting is our
                // problem, not something the device will do for us.
                let format = client.GetMixFormat()?;
                if format.is_null() {
                    anyhow::bail!("audio endpoint reported no mix format");
                }
                let channels = (*format).nChannels as usize;
                let rate = (*format).nSamplesPerSec;
                let bits = (*format).wBitsPerSample;
                let tag = (*format).wFormatTag;

                // The shared-mode mix format is 32-bit float on every Windows
                // version this targets. Bail rather than silently reading
                // integer samples as floats, which is full-scale noise rather
                // than a subtly wrong result.
                //
                // WAVE_FORMAT_EXTENSIBLE's true type lives in a SubFormat
                // GUID this does not parse — the 32-bit width check plus the
                // shared-mode invariant is what stands in for that, which is
                // why an unexpected width is refused rather than assumed.
                let is_float = tag == WAVE_FORMAT_IEEE_FLOAT || tag == WAVE_FORMAT_EXTENSIBLE;
                if !is_float || bits != 32 {
                    CoTaskMemFree(Some(format as *const _));
                    anyhow::bail!(
                        "unsupported endpoint mix format: {bits}-bit, tag {tag} \
                         (expected 32-bit float)"
                    );
                }

                // 200 ms of buffer: enough that a scheduling hiccup does not
                // lose audio, short enough not to add noticeable latency.
                let result = client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    2_000_000, // 100-ns units
                    0,
                    format,
                    None,
                );
                CoTaskMemFree(Some(format as *const _));
                result?;

                let capture: IAudioCaptureClient = client.GetService()?;
                client.Start()?;

                Ok(Self {
                    client,
                    capture,
                    channels,
                    rate,
                    kind: SampleKind::F32,
                    resampler: Resampler::new(rate),
                    accumulator: FrameAccumulator::new(),
                    timeline: CaptureTimeline::new(),
                    ready: VecDeque::new(),
                    event: None,
                })
            }
        }

        /// Drain whatever the endpoint currently holds into whole frames.
        fn pump(&mut self) -> anyhow::Result<()> {
            // SAFETY: each GetBuffer is matched by exactly one ReleaseBuffer,
            // including on the silent path.
            unsafe {
                loop {
                    let available = self.capture.GetNextPacketSize()?;
                    if available == 0 {
                        return Ok(());
                    }

                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames: u32 = 0;
                    let mut flags: u32 = 0;
                    // The device's own capture timestamp, in 100-ns units. This
                    // is what makes a silence cost exactly its own length
                    // instead of vanishing — see `CaptureTimeline`.
                    let mut qpc_100ns: u64 = 0;
                    self.capture.GetBuffer(
                        &mut data,
                        &mut frames,
                        &mut flags,
                        None,
                        Some(&mut qpc_100ns),
                    )?;

                    let sample_count = frames as usize * self.channels;
                    // AUDCLNT_BUFFERFLAGS_SILENT means the buffer contents are
                    // undefined, not zero — reading it would be noise. Nothing
                    // is playing, so synthesise the silence it represents.
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null();
                    let interleaved: Vec<f32> = if silent {
                        vec![0.0; sample_count]
                    } else {
                        match self.kind {
                            SampleKind::F32 => {
                                std::slice::from_raw_parts(data as *const f32, sample_count)
                                    .to_vec()
                            }
                            // i16 full scale is 32768 in the negative direction
                            // and 32767 in the positive; dividing by 32768
                            // keeps the result inside [-1.0, 1.0] rather than
                            // letting the most negative sample exceed it.
                            SampleKind::I16 => {
                                std::slice::from_raw_parts(data as *const i16, sample_count)
                                    .iter()
                                    .map(|s| *s as f32 / 32768.0)
                                    .collect()
                            }
                        }
                    };
                    self.capture.ReleaseBuffer(frames)?;

                    // Before pushing: the accumulator's pending count is what
                    // the timeline compares the device's reading against, and
                    // pushing first would destroy it.
                    if qpc_100ns != 0
                        && self
                            .timeline
                            .on_packet(qpc_100ns / 10, self.accumulator.pending_len())
                    {
                        self.accumulator.reset();
                    }

                    let mono = downmix_to_mono(&interleaved, self.channels);
                    let resampled = self.resampler.process(&mono);
                    for samples in self.accumulator.push(&resampled) {
                        let offset_us = self.timeline.next_offset_us();
                        self.timeline.on_frame_emitted();
                        self.ready.push_back(ContentFrame { samples, offset_us });
                    }
                }
            }
        }
    }

    impl ContentAudioSource for WasapiLoopback {
        fn next_frame(&mut self) -> anyhow::Result<ContentFrame> {
            loop {
                if let Some(frame) = self.ready.pop_front() {
                    return Ok(frame);
                }
                self.pump()?;
                if self.ready.is_empty() {
                    // Nothing yet. A loopback device delivers nothing at all
                    // when its source is idle, so this is the steady state
                    // rather than an error — and the reason frame offsets come
                    // from the device rather than from a counter.
                    self.wait_for_data();
                }
            }
        }
    }

    impl WasapiLoopback {
        /// Block briefly for more audio.
        ///
        /// The timeout matters on the event path too: a process that stops
        /// playing stops the notifications with it, and an untimed wait would
        /// then block until the app made a sound again — with no way for the
        /// capture thread to notice it had been asked to stop.
        fn wait_for_data(&self) {
            match self.event {
                // SAFETY: the handle is owned by this struct and closed only in
                // `Drop`, after the client has been stopped.
                Some(h) => unsafe {
                    let _ = WaitForSingleObject(h, POLL_MS as u32 * 2);
                },
                None => std::thread::sleep(Duration::from_millis(POLL_MS)),
            }
        }
    }

    impl Drop for WasapiLoopback {
        fn drop(&mut self) {
            // SAFETY: stopping an already-stopped client is harmless. The event
            // is closed only after the client is stopped, so WASAPI cannot
            // signal a handle that has been released.
            unsafe {
                let _ = self.client.Stop();
                if let Some(h) = self.event.take() {
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}

/// Average interleaved `channels`-channel samples down to mono.
///
/// Averaging rather than taking the left channel: a game that pans an effect
/// hard right would otherwise vanish, and the sum of two correlated channels
/// clips where the average does not.
///
/// Returns an empty vec for a zero channel count rather than dividing by it.
pub fn downmix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// Linear resampler that carries its phase across calls.
///
/// The state is the whole point. A device read is an arbitrary slice of a
/// continuous signal, so a resampler that restarts at phase 0 for every read —
/// and interpolates its final sample against a duplicate of the last one,
/// having no successor to reach for — inserts a step discontinuity at every
/// buffer boundary. At WASAPI's ~10 ms polling that is a click a hundred times
/// a second for the entire share, which is heard as continuous distortion
/// rather than as clicks. Carrying the fractional read position and the
/// previous chunk's last sample makes the joins seamless.
///
/// Linear rather than windowed-sinc for the same reason the voice path uses it
/// (see the audio-quality item in `backlog.md`): the artefacts sit far above
/// the band that matters at these rates, and a polyphase filter is a drop-in
/// replacement later if it ever becomes audible. Content audio is more
/// demanding than speech here, so this is the more likely of the two to want
/// upgrading.
#[derive(Debug, Clone)]
pub struct Resampler {
    in_rate: u32,
    /// Fractional read position in input samples, relative to the start of the
    /// next chunk. May be negative, meaning "between `prev` and this chunk".
    pos: f64,
    /// Last sample of the previous chunk, so interpolation spans the join.
    prev: Option<f32>,
}

impl Resampler {
    /// A resampler converting `in_rate` to [`SAMPLE_RATE`].
    pub fn new(in_rate: u32) -> Self {
        Self {
            in_rate,
            pos: 0.0,
            prev: None,
        }
    }

    /// Resample one chunk, continuing from where the last call left off.
    ///
    /// Returns `input` unchanged when the rates already match, which is the
    /// common case — most render endpoints run at 48 kHz.
    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.in_rate == SAMPLE_RATE || self.in_rate == 0 || input.is_empty() {
            return input.to_vec();
        }
        let ratio = self.in_rate as f64 / SAMPLE_RATE as f64;
        let n = input.len();
        let last = (n - 1) as f64;
        let prev = self.prev;
        let tap = |i: isize| -> f32 {
            if i < 0 {
                // Only reachable once `prev` exists; on the very first chunk
                // `pos` starts at 0 and never looks back.
                prev.unwrap_or(input[0])
            } else {
                input[(i as usize).min(n - 1)]
            }
        };

        let mut out = Vec::with_capacity((n as f64 / ratio).ceil() as usize + 1);
        let mut pos = self.pos;
        while pos <= last {
            let idx = pos.floor();
            let frac = (pos - idx) as f32;
            let a = tap(idx as isize);
            let b = tap(idx as isize + 1);
            out.push(a + (b - a) * frac);
            pos += ratio;
        }
        // Rebase onto the next chunk. The remainder is kept exactly, which is
        // what stops per-chunk rounding from accumulating into rate drift.
        self.pos = pos - n as f64;
        self.prev = Some(input[n - 1]);
        out
    }
}

/// Resample mono `input` from `in_rate` to [`SAMPLE_RATE`] in one shot.
///
/// Convenience wrapper over a fresh [`Resampler`]; correct only for a signal
/// that begins and ends here. Anything reading a device in chunks must hold a
/// [`Resampler`] instead — see its documentation for what a reset phase does to
/// the audio.
pub fn resample_linear(input: &[f32], in_rate: u32) -> Vec<f32> {
    Resampler::new(in_rate).process(input)
}

/// Convert `f32` samples in [-1.0, 1.0] to the `i16` the Opus encoder takes.
///
/// Clamps rather than wrapping. A loopback endpoint can legitimately hand back
/// samples slightly outside unity — a mixer summing several apps, or a plugin
/// with makeup gain — and `as i16` on an out-of-range float wraps to the
/// opposite rail, turning a loud passage into a burst of full-scale noise.
/// Clipping is the honest failure here.
pub fn f32_to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect()
}

/// Accumulates arbitrary-length device reads into exact 20 ms frames.
///
/// Loopback devices hand back whatever happens to be in the endpoint buffer,
/// which is rarely a whole frame and never reliably so. Opus needs exactly
/// [`SAMPLES_PER_FRAME`], so the remainder has to be carried rather than padded
/// — padding with silence would inject a click on every read boundary.
#[derive(Default)]
pub struct FrameAccumulator {
    pending: Vec<f32>,
}

impl FrameAccumulator {
    /// Create an empty accumulator.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add samples, returning every whole frame that is now available.
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let mut out = Vec::new();
        while self.pending.len() >= SAMPLES_PER_FRAME {
            out.push(self.pending.drain(..SAMPLES_PER_FRAME).collect());
        }
        out
    }

    /// Samples held back, waiting for the rest of a frame.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Discard the partial frame.
    ///
    /// Used when the device reports a gap: the samples held back were captured
    /// before it and the ones arriving come after, so joining them would splice
    /// two different moments into one frame.
    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

/// Microseconds of audio in `samples` mono samples at [`SAMPLE_RATE`].
fn samples_to_us(samples: usize) -> u64 {
    (samples as u64) * 1_000_000 / (SAMPLE_RATE as u64)
}

/// How far the device timestamp may diverge from the accumulated sample count
/// before it is treated as a gap rather than as noise.
///
/// Above the jitter of a buffered read and above the rounding the resampler
/// introduces, so ordinary operation never trips it; far below the length of a
/// silence a user would produce by pausing whatever they are sharing. Note that
/// the *threshold* being 100 ms does not mean 100 ms of error survives — a
/// correction sets the timeline to exactly what the device reported.
const GAP_US: u64 = 100_000;

/// Turns device capture timestamps into per-frame offsets.
///
/// # Why the device's clock and not ours
///
/// The tempting implementation numbers the frames as they come out and
/// multiplies by 20 ms. That is right only while the device produces audio
/// continuously, and a loopback device does not: it produces nothing at all
/// when nothing is playing. Counting therefore treats a thirty-second silence
/// as though it never happened, and every silence in a session adds to a
/// permanent lag behind the video the audio is meant to line up with.
///
/// Taking the reading from the device instead makes a gap cost exactly its own
/// length, which is the entire behaviour this type exists to provide.
#[derive(Debug, Default)]
pub struct CaptureTimeline {
    /// Device timestamp of the first sample this source ever saw, so offsets
    /// start from zero regardless of what the device counts from.
    origin_us: Option<u64>,
    /// Offset carried by the next whole frame to be emitted.
    next_us: u64,
}

impl CaptureTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    /// Offset for the next frame the accumulator produces.
    pub fn next_offset_us(&self) -> u64 {
        self.next_us
    }

    /// Account for a packet the device timestamped `device_us`, with
    /// `pending_samples` already held back by the accumulator.
    ///
    /// Returns `true` when the device jumped — the caller must then drop the
    /// partial frame, since it belongs to the moment before the gap.
    pub fn on_packet(&mut self, device_us: u64, pending_samples: usize) -> bool {
        let origin = *self.origin_us.get_or_insert(device_us);
        // A device that reports a timestamp before its own first one is
        // nonsense; saturating keeps the timeline monotonic rather than
        // wrapping into an enormous offset.
        let device_off = device_us.saturating_sub(origin);
        let expected = self.next_us + samples_to_us(pending_samples);

        // Only a jump *forward* is a gap. Backwards means the device
        // re-reported time we have already emitted frames for, and honouring it
        // would send timestamps that go backwards — worse than ignoring it.
        if device_off > expected.saturating_add(GAP_US) {
            self.next_us = device_off;
            return true;
        }
        false
    }

    /// Account for one whole frame having been emitted.
    pub fn on_frame_emitted(&mut self) {
        self.next_us = self
            .next_us
            .saturating_add(samples_to_us(SAMPLES_PER_FRAME));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady state: the device's readings track the samples it has delivered,
    /// so frames come out exactly one frame apart and nothing is discarded.
    #[test]
    fn an_uninterrupted_stream_advances_one_frame_at_a_time() {
        let mut t = CaptureTimeline::new();
        // A device that starts its clock at some arbitrary point, to prove
        // offsets are relative to the first packet rather than absolute.
        let origin = 900_000_000;
        let mut offsets = Vec::new();
        for packet in 0..5u64 {
            let device_us = origin + packet * 20_000;
            assert!(
                !t.on_packet(device_us, 0),
                "packet {packet} wrongly reported a gap"
            );
            offsets.push(t.next_offset_us());
            t.on_frame_emitted();
        }
        assert_eq!(offsets, vec![0, 20_000, 40_000, 60_000, 80_000]);
    }

    /// The whole point: a device that stops producing and resumes later must
    /// cost exactly the silence it sat out.
    #[test]
    fn a_gap_costs_exactly_its_own_length() {
        let mut t = CaptureTimeline::new();
        assert!(!t.on_packet(0, 0));
        assert_eq!(t.next_offset_us(), 0);
        t.on_frame_emitted();

        // Thirty seconds of the shared application making no sound at all.
        assert!(t.on_packet(30_000_000, 0), "the gap was not detected");
        assert_eq!(
            t.next_offset_us(),
            30_000_000,
            "audio would resume 30s behind the video"
        );
    }

    /// A burst read is not a gap. Several frames arriving at once is the
    /// normal shape of a buffered device, and treating it as a discontinuity
    /// would discard audio on every read.
    #[test]
    fn a_buffered_burst_is_not_a_gap() {
        let mut t = CaptureTimeline::new();
        assert!(!t.on_packet(0, 0));
        // 60 ms delivered in one packet: three frames come out, and the next
        // packet is timestamped 60 ms later.
        for _ in 0..3 {
            t.on_frame_emitted();
        }
        assert!(
            !t.on_packet(60_000, 0),
            "a normal burst read looked like a gap"
        );
        assert_eq!(t.next_offset_us(), 60_000);
    }

    /// Samples held back for the rest of a frame count toward where the device
    /// should be. Ignoring them would make every partially-filled frame look
    /// like a small gap.
    #[test]
    fn samples_pending_in_the_accumulator_count_as_elapsed() {
        let mut t = CaptureTimeline::new();
        assert!(!t.on_packet(0, 0));
        // Half a frame (10 ms at 48 kHz) is waiting; the device's next packet
        // is timestamped accordingly.
        let half = SAMPLES_PER_FRAME / 2;
        assert!(!t.on_packet(10_000, half));
        assert_eq!(t.next_offset_us(), 0, "the partial frame was discarded");
    }

    /// Only a jump forward is a gap. A device that re-reports a time we have
    /// already emitted frames for must not drag the timeline backwards, since
    /// a receiver cannot make sense of timestamps that go back.
    #[test]
    fn the_timeline_never_moves_backwards() {
        let mut t = CaptureTimeline::new();
        assert!(!t.on_packet(5_000_000, 0));
        t.on_frame_emitted();
        t.on_frame_emitted();
        let before = t.next_offset_us();
        // Earlier than the origin entirely.
        assert!(!t.on_packet(1_000_000, 0));
        assert_eq!(t.next_offset_us(), before);
    }

    /// A device that reports no timestamp at all (some drivers report zero) is
    /// handled by the caller, not here — but the threshold must still be big
    /// enough that ordinary jitter never trips it.
    #[test]
    fn jitter_below_the_threshold_is_absorbed() {
        let mut t = CaptureTimeline::new();
        assert!(!t.on_packet(0, 0));
        t.on_frame_emitted();
        // 20 ms expected, 90 ms reported: late, but inside the deadband.
        assert!(!t.on_packet(90_000, 0));
        assert_eq!(t.next_offset_us(), 20_000);
        // 20 ms expected, 150 ms reported: outside it.
        assert!(t.on_packet(150_000, 0));
        assert_eq!(t.next_offset_us(), 150_000);
    }

    /// Opens real process loopback against this test process.
    ///
    /// Ignored by default because it needs a machine, not a build: it touches
    /// COM, a virtual audio device, and a Windows build number. Run it by hand
    /// (`cargo test -- --ignored process_loopback`) after changing anything in
    /// the activation path — the `PROPVARIANT` shim in particular is a layout
    /// claim the compiler can only half check, and a wrong one fails here
    /// rather than in review.
    ///
    /// Asserts only that the device opens. The test process plays no audio, so
    /// pulling a frame would block forever; whether the *right* application is
    /// captured is a question only a person with a noisy app can answer.
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires a Windows build with process loopback (10.0.20348+)"]
    fn process_loopback_activates_against_a_real_pid() {
        let pid = std::process::id();
        match super::windows_impl::WasapiLoopback::open_process(pid) {
            Ok(capture) => drop(capture),
            Err(e) => panic!("process loopback failed to open for pid {pid}: {e}"),
        }
    }

    #[test]
    fn the_accumulator_can_drop_a_partial_frame() {
        let mut acc = FrameAccumulator::new();
        assert!(acc.push(&vec![0.5; 100]).is_empty());
        assert_eq!(acc.pending_len(), 100);
        acc.reset();
        assert_eq!(acc.pending_len(), 0);
    }

    #[test]
    fn mono_passes_through_untouched() {
        let input = vec![0.1, -0.2, 0.3];
        assert_eq!(downmix_to_mono(&input, 1), input);
    }

    #[test]
    fn stereo_averages_both_channels() {
        // L/R pairs: (1,-1) -> 0, (0.5,0.5) -> 0.5
        let input = vec![1.0, -1.0, 0.5, 0.5];
        assert_eq!(downmix_to_mono(&input, 2), vec![0.0, 0.5]);
    }

    /// Hard-panned content must survive the downmix. Taking the left channel
    /// instead would silence it entirely — a plausible-looking shortcut that
    /// loses half the game's audio.
    #[test]
    fn hard_panned_audio_is_not_lost() {
        let right_only = vec![0.0, 1.0, 0.0, 1.0];
        let mono = downmix_to_mono(&right_only, 2);
        assert!(mono.iter().all(|s| *s > 0.0), "panned audio vanished");
    }

    #[test]
    fn surround_downmixes_without_clipping() {
        // Six correlated channels at full scale must not sum past 1.0.
        let input = vec![1.0f32; 12];
        let mono = downmix_to_mono(&input, 6);
        assert_eq!(mono.len(), 2);
        assert!(mono.iter().all(|s| *s <= 1.0), "downmix clipped");
    }

    #[test]
    fn a_partial_trailing_frame_is_dropped_not_padded() {
        // 5 samples of stereo is two whole frames plus a stray sample; the
        // stray must not become a half-silent frame.
        let input = vec![1.0, 1.0, 2.0, 2.0, 3.0];
        assert_eq!(downmix_to_mono(&input, 2), vec![1.0, 2.0]);
    }

    #[test]
    fn zero_channels_does_not_divide_by_zero() {
        assert!(downmix_to_mono(&[1.0, 2.0], 0).is_empty());
    }

    #[test]
    fn matching_rate_is_a_passthrough() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_linear(&input, SAMPLE_RATE), input);
    }

    #[test]
    fn downsampling_shortens_proportionally() {
        // 96k -> 48k halves the sample count.
        let input = vec![0.0f32; 960];
        assert_eq!(resample_linear(&input, 96_000).len(), 480);
    }

    #[test]
    fn upsampling_lengthens_proportionally() {
        // 24k -> 48k doubles it. One sample of slack: whole output samples are
        // emitted only where both interpolation taps exist, and the remainder
        // is carried into the next chunk rather than invented here.
        let input = vec![0.0f32; 240];
        let out = resample_linear(&input, 48_000 / 2);
        assert!((out.len() as i64 - 480).abs() <= 1, "got {}", out.len());
    }

    /// The defect the resampler's state exists to remove. A continuous signal
    /// fed in device-sized chunks must come out continuous: restarting the
    /// interpolation phase at every read inserted a step at every chunk join —
    /// at WASAPI's ~10 ms polling, a hundred a second for the whole share,
    /// which is heard as constant distortion rather than as clicks.
    #[test]
    fn chunk_boundaries_do_not_reset_the_phase() {
        let mut r = Resampler::new(44_100);
        let mut out = Vec::new();
        let mut n = 0.0f32;
        for _ in 0..8 {
            let chunk: Vec<f32> = (0..441)
                .map(|_| {
                    n += 1.0;
                    n
                })
                .collect();
            out.extend(r.process(&chunk));
        }
        // A ramp through any correct linear interpolator stays monotonic; a
        // phase reset shows up as a jump back toward the start of the chunk.
        for pair in out.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "phase reset at a chunk boundary: {pair:?}"
            );
        }
    }

    /// Carrying the exact fractional remainder is what keeps the *rate* right
    /// across chunks. Rounding each chunk independently would drift, and since
    /// video is synchronised to this stream the drift would read as A/V error.
    #[test]
    fn the_sustained_rate_is_exact_across_chunks() {
        let mut r = Resampler::new(44_100);
        let mut produced = 0usize;
        for _ in 0..100 {
            produced += r.process(&vec![0.0f32; 441]).len();
        }
        // 100 * 10 ms of 44.1 kHz input is 1 s, so 48 000 output samples.
        assert!(
            (produced as i64 - 48_000).abs() <= 2,
            "rate drifted: {produced}"
        );
    }

    /// A ramp must stay monotonic through resampling — a sign that
    /// interpolation is reading neighbouring samples rather than scrambling
    /// indices.
    #[test]
    fn a_ramp_stays_monotonic() {
        let input: Vec<f32> = (0..960).map(|i| i as f32).collect();
        let out = resample_linear(&input, 96_000);
        for pair in out.windows(2) {
            assert!(pair[1] >= pair[0], "ramp inverted: {pair:?}");
        }
    }

    #[test]
    fn resampling_never_indexes_past_the_input() {
        // Deliberately awkward rates and a tiny buffer: the last output sample
        // interpolates against index+1, which must clamp.
        for rate in [8_000, 11_025, 44_100, 96_000, 192_000] {
            let input = vec![1.0f32, 2.0];
            let _ = resample_linear(&input, rate);
        }
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(resample_linear(&[], 44_100).is_empty());
        assert!(resample_linear(&[1.0], 0).len() == 1);
    }

    #[test]
    fn sharing_an_app_captures_that_app() {
        assert_eq!(
            resolve_audio_spec(ContentAudioMode::Auto, true, Some(4242)),
            ContentAudioSpec::Process(4242)
        );
    }

    #[test]
    fn sharing_a_monitor_captures_the_machine() {
        assert_eq!(
            resolve_audio_spec(ContentAudioMode::Auto, true, None),
            ContentAudioSpec::System
        );
    }

    /// A camera must not pull in the speakers: the microphone already carries
    /// the person, and capturing output as well feeds remote voices back out.
    #[test]
    fn a_camera_shares_no_content_audio() {
        assert_eq!(
            resolve_audio_spec(ContentAudioMode::Auto, false, None),
            ContentAudioSpec::None
        );
        assert_eq!(
            resolve_audio_spec(ContentAudioMode::Auto, false, Some(7)),
            ContentAudioSpec::None
        );
    }

    #[test]
    fn explicit_modes_ignore_the_video_source() {
        for (screen, pid) in [(true, Some(9u32)), (true, None), (false, None)] {
            assert_eq!(
                resolve_audio_spec(ContentAudioMode::System, screen, pid),
                ContentAudioSpec::System
            );
            assert_eq!(
                resolve_audio_spec(ContentAudioMode::Off, screen, pid),
                ContentAudioSpec::None
            );
        }
    }

    #[test]
    fn the_mode_setting_round_trips() {
        for m in [
            ContentAudioMode::Auto,
            ContentAudioMode::System,
            ContentAudioMode::Off,
        ] {
            assert_eq!(ContentAudioMode::from_setting(m.as_setting()), m);
        }
    }

    /// A settings file from a newer build must degrade rather than fail.
    #[test]
    fn an_unknown_mode_falls_back_to_auto() {
        assert_eq!(
            ContentAudioMode::from_setting("per-window-hdr"),
            ContentAudioMode::Auto
        );
        assert_eq!(ContentAudioMode::from_setting(""), ContentAudioMode::Auto);
    }

    #[test]
    fn f32_converts_to_i16_at_full_scale() {
        let out = f32_to_i16(&[0.0, 1.0, -1.0, 0.5]);
        assert_eq!(out[0], 0);
        assert_eq!(out[1], i16::MAX);
        assert!(out[2] <= i16::MIN + 1);
        assert!((out[3] - 16_383).abs() <= 1);
    }

    /// The failure this clamp prevents: `as i16` on an out-of-range float
    /// wraps to the opposite rail, so a loud passage becomes full-scale noise
    /// of the wrong sign rather than a clipped version of itself.
    #[test]
    fn out_of_range_samples_clip_rather_than_wrap() {
        let out = f32_to_i16(&[2.5, -2.5, 100.0, -100.0]);
        assert_eq!(out[0], i16::MAX);
        assert!(out[1] <= i16::MIN + 1);
        assert_eq!(out[2], i16::MAX);
        assert!(out[3] <= i16::MIN + 1);
    }

    #[test]
    fn accumulator_emits_only_whole_frames() {
        let mut acc = FrameAccumulator::new();
        assert!(acc.push(&vec![0.0; SAMPLES_PER_FRAME - 1]).is_empty());
        assert_eq!(acc.pending_len(), SAMPLES_PER_FRAME - 1);

        let frames = acc.push(&[0.0]);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), SAMPLES_PER_FRAME);
        assert_eq!(acc.pending_len(), 0);
    }

    #[test]
    fn accumulator_splits_a_large_read_into_several_frames() {
        let mut acc = FrameAccumulator::new();
        let frames = acc.push(&vec![0.0; SAMPLES_PER_FRAME * 3 + 7]);
        assert_eq!(frames.len(), 3);
        assert_eq!(acc.pending_len(), 7);
    }

    /// The property that matters: no sample is lost or duplicated across reads
    /// that do not align to frame boundaries. A padding implementation would
    /// insert silence here and click on every boundary.
    #[test]
    fn samples_survive_unaligned_reads_in_order() {
        let mut acc = FrameAccumulator::new();
        let total = SAMPLES_PER_FRAME * 2;
        let source: Vec<f32> = (0..total).map(|i| i as f32).collect();

        let mut got = Vec::new();
        for chunk in source.chunks(137) {
            for frame in acc.push(chunk) {
                got.extend(frame);
            }
        }
        assert_eq!(got, source, "samples were reordered, lost, or padded");
    }
}
