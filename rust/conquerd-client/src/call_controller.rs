//! Call controller — audio call lifecycle state machine.
//!
//! Coordinates:
//! - Audio engine (CPAL + Opus via conquerd-opus)
//! - QUIC peer audio sessions
//! - Metrics & voice activity
//!
//! All public methods are synchronous (send over channels); the async driver
//! lives in `run()`. The Qt/QML UI layer signals will be wired in Phase 3
//! via cxx-qt callbacks stored in the `CallController`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::connection_manager::ConnectionCommand;
use conquerd_opus::{Application as OpusApp, OpusDecoder, OpusEncoder};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Stream;
use ringbuf::{
    traits::{Consumer, Observer, Producer, Split},
    HeapRb,
};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Audio constants
// ---------------------------------------------------------------------------

pub const SAMPLE_RATE: u32 = 48_000;
/// 20 ms Opus frames.
pub const SAMPLES_PER_FRAME: usize = (SAMPLE_RATE as usize) * 20 / 1000; // 960
/// 5 ms fade window.
pub const FADE_SAMPLES: usize = (SAMPLE_RATE as usize) * 5 / 1000; // 240

// ---------------------------------------------------------------------------
// Audio pipeline
// ---------------------------------------------------------------------------

/// Live CPAL streams + Opus codec. Dropped when a call ends.
///
/// `cpal::Stream` is `Send` but `!Sync`; we assert `Send` here because the
/// pipeline is always owned and accessed from a single tokio task.
struct AudioPipeline {
    /// Keep streams alive — dropped here when the call ends.
    _capture_stream: Stream,
    _playback_stream: Stream,
    /// Push decoded PCM frames into the CPAL playback ring buffer.
    playback_prod: ringbuf::HeapProd<i16>,
    /// Mute flag shared with the capture callback.
    muted: Arc<AtomicBool>,
    /// Noise gate strength (0=off, 1=mild, 2=moderate, 3=aggressive, 4=max).
    /// Shared with the capture callback so it can be updated mid-call.
    noise_strength: Arc<AtomicU32>,
    /// Input gain 0–200 (100 = unity). Shared with the capture callback.
    input_gain: Arc<AtomicU32>,
    /// Output gain 0–200 (100 = unity). Applied in push_inbound.
    output_gain: Arc<AtomicU32>,
    /// Per-peer Opus decoders (lazily created on first inbound frame).
    decoders: HashMap<String, OpusDecoder>,
}

// CPAL Stream is Send but !Sync; OpusDecoder wraps a raw pointer and is
// also !Sync. The pipeline is exclusively owned and accessed from the single
// CallController tokio task, so both Send and Sync are safe to assert here.
unsafe impl Send for AudioPipeline {}
unsafe impl Sync for AudioPipeline {}

/// Look up a CPAL device by name, falling back to the host default when the
/// name is empty or no device matches.  `kind` is "input" or "output" — used
/// only for the diagnostic log line.
fn resolve_cpal_device(
    host: &cpal::Host,
    name: Option<&str>,
    kind: &str,
) -> anyhow::Result<cpal::Device> {
    let trimmed = name.map(str::trim).filter(|s| !s.is_empty());
    if let Some(target) = trimmed {
        let iter_res = if kind == "input" {
            host.input_devices().map(|it| it.collect::<Vec<_>>())
        } else {
            host.output_devices().map(|it| it.collect::<Vec<_>>())
        };
        if let Ok(devs) = iter_res {
            for d in devs {
                if let Ok(n) = d.name() {
                    if n == target {
                        info!("Audio {kind} device: matched '{}' by name", target);
                        return Ok(d);
                    }
                }
            }
            warn!(
                "Audio {kind} device '{}' not found — falling back to system default",
                target
            );
        }
    }
    let dev = if kind == "input" {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    dev.ok_or_else(|| anyhow::anyhow!("No default audio {kind} device"))
}

impl AudioPipeline {
    /// Open CPAL I/O devices and start audio capture + playback.
    ///
    /// `input_device_name` / `output_device_name` are the user's selected
    /// device names from the settings page.  An empty / `None` value falls
    /// back to the host's default device.
    ///
    /// Returns `(pipeline, encoded_rx, speaking_rx)` where `encoded_rx` yields
    /// outbound Opus frames and `speaking_rx` yields speaking-state booleans
    /// derived from an inline RMS energy VAD on each 20 ms capture frame.
    #[allow(clippy::type_complexity)]
    fn start(
        start_muted: bool,
        input_device_name: Option<&str>,
        output_device_name: Option<&str>,
        initial_input_vol: u32,
        initial_output_vol: u32,
        initial_noise_idx: u32,
    ) -> anyhow::Result<(
        Self,
        mpsc::Receiver<Vec<u8>>,
        tokio::sync::mpsc::UnboundedReceiver<bool>,
        tokio::sync::mpsc::UnboundedReceiver<f32>,
    )> {
        let host = cpal::default_host();

        let input_dev = resolve_cpal_device(&host, input_device_name, "input")?;
        let output_dev = resolve_cpal_device(&host, output_device_name, "output")?;

        // Query device default configs. On Windows (WASAPI shared mode) and
        // most CoreAudio devices, build_*_stream will refuse any config that
        // doesn't match the device default — so we must adopt the device's
        // native sample-rate and channel-count and resample / mix in software.
        let input_default = input_dev
            .default_input_config()
            .map_err(|e| anyhow::anyhow!("Query default input config: {e}"))?;
        let output_default = output_dev
            .default_output_config()
            .map_err(|e| anyhow::anyhow!("Query default output config: {e}"))?;
        let in_sr = input_default.sample_rate().0;
        let in_ch = input_default.channels() as usize;
        let out_sr = output_default.sample_rate().0;
        let out_ch = output_default.channels() as usize;
        let in_sample_fmt = input_default.sample_format();
        let out_sample_fmt = output_default.sample_format();
        info!(
            "Audio devices: input={}Hz x{} ({:?}), output={}Hz x{} ({:?})",
            in_sr, in_ch, in_sample_fmt, out_sr, out_ch, out_sample_fmt
        );
        let input_cfg = cpal::StreamConfig {
            channels: input_default.channels(),
            sample_rate: input_default.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };
        let output_cfg = cpal::StreamConfig {
            channels: output_default.channels(),
            sample_rate: output_default.sample_rate(),
            buffer_size: cpal::BufferSize::Default,
        };

        // -- Capture side -----------------------------------------------
        let muted = Arc::new(AtomicBool::new(start_muted));
        let muted_cap = Arc::clone(&muted);

        let noise_strength_arc = Arc::new(AtomicU32::new(initial_noise_idx.min(4)));
        let noise_strength_cap = Arc::clone(&noise_strength_arc);
        let input_gain_arc = Arc::new(AtomicU32::new(initial_input_vol.min(200)));
        let input_gain_cap = Arc::clone(&input_gain_arc);
        let output_gain_arc = Arc::new(AtomicU32::new(initial_output_vol.min(200)));
        let mut noise_floor_rms: f32 = 100.0;

        let mut encoder = OpusEncoder::new(48_000, 1, OpusApp::Voip)
            .map_err(|e| anyhow::anyhow!("Opus encoder init: {e}"))?;
        encoder
            .set_bitrate(48_000)
            .map_err(|e| anyhow::anyhow!("Set bitrate: {e}"))?;
        encoder
            .set_inband_fec(true)
            .map_err(|e| anyhow::anyhow!("Set inband FEC: {e}"))?;
        encoder
            .set_packet_loss_perc(10)
            .map_err(|e| anyhow::anyhow!("Set packet loss perc: {e}"))?;
        encoder
            .set_dtx(true)
            .map_err(|e| anyhow::anyhow!("Set DTX: {e}"))?;
        // Enable DRED: 10 × 10 ms frames = 100 ms redundancy depth.
        // Non-fatal: OPUS_UNIMPLEMENTED is returned if the weights were not
        // loaded (dnn feature disabled) or this libopus build lacks DRED.
        if let Err(e) = encoder.set_dred_duration_ms(100) {
            warn!("DRED duration not set (DRED may be unavailable): {e}");
        }

        let (encoded_tx, encoded_rx) = mpsc::channel::<Vec<u8>>(128);
        let (speaking_tx, speaking_rx) = tokio::sync::mpsc::unbounded_channel::<bool>();
        let (level_tx, level_rx) = tokio::sync::mpsc::unbounded_channel::<f32>();
        let mut capture_accum: Vec<i16> = Vec::with_capacity(SAMPLES_PER_FRAME * 2);

        // Inline VAD parameters (RMS on i16 PCM)
        const VAD_THRESHOLD: f32 = 500.0; // ~-36 dBFS relative to 32767
        const VAD_ATTACK_FRAMES: u32 = 2; // 40 ms onset
        const VAD_RELEASE_FRAMES: u32 = 15; // 300 ms hold-off
        let mut vad_speaking = false;
        let mut vad_above_count = 0u32;
        let mut vad_below_count = 0u32;

        // Resampler state for in_sr → 48 kHz mono (linear interpolation).
        let mut resamp_prev: f32 = 0.0;
        let mut resamp_phase: f64 = 0.0; // fractional source-sample position [0,1)
        let resamp_ratio: f64 = in_sr as f64 / SAMPLE_RATE as f64;

        let capture_stream = match in_sample_fmt {
            cpal::SampleFormat::F32 => input_dev
                .build_input_stream(
                    &input_cfg,
                    move |data: &[f32], _info: &cpal::InputCallbackInfo| {
                        let ns = noise_strength_cap.load(Ordering::Relaxed);
                        let ig = input_gain_cap.load(Ordering::Relaxed) as f32 / 100.0;
                        capture_callback_f32(
                            data,
                            in_ch,
                            &muted_cap,
                            &mut capture_accum,
                            &mut resamp_prev,
                            &mut resamp_phase,
                            resamp_ratio,
                            &mut vad_speaking,
                            &mut vad_above_count,
                            &mut vad_below_count,
                            VAD_THRESHOLD,
                            VAD_ATTACK_FRAMES,
                            VAD_RELEASE_FRAMES,
                            &speaking_tx,
                            &level_tx,
                            &mut encoder,
                            &encoded_tx,
                            &mut noise_floor_rms,
                            ns,
                            ig,
                        );
                    },
                    |err| warn!("Capture stream error: {err}"),
                    None,
                )
                .map_err(|e| anyhow::anyhow!("Build capture stream (f32): {e}"))?,
            cpal::SampleFormat::I16 => input_dev
                .build_input_stream(
                    &input_cfg,
                    move |data: &[i16], _info: &cpal::InputCallbackInfo| {
                        let ns = noise_strength_cap.load(Ordering::Relaxed);
                        let ig = input_gain_cap.load(Ordering::Relaxed) as f32 / 100.0;
                        capture_callback_i16(
                            data,
                            in_ch,
                            &muted_cap,
                            &mut capture_accum,
                            &mut resamp_prev,
                            &mut resamp_phase,
                            resamp_ratio,
                            &mut vad_speaking,
                            &mut vad_above_count,
                            &mut vad_below_count,
                            VAD_THRESHOLD,
                            VAD_ATTACK_FRAMES,
                            VAD_RELEASE_FRAMES,
                            &speaking_tx,
                            &level_tx,
                            &mut encoder,
                            &encoded_tx,
                            &mut noise_floor_rms,
                            ns,
                            ig,
                        );
                    },
                    |err| warn!("Capture stream error: {err}"),
                    None,
                )
                .map_err(|e| anyhow::anyhow!("Build capture stream (i16): {e}"))?,
            cpal::SampleFormat::U16 => input_dev
                .build_input_stream(
                    &input_cfg,
                    move |data: &[u16], _info: &cpal::InputCallbackInfo| {
                        let ns = noise_strength_cap.load(Ordering::Relaxed);
                        let ig = input_gain_cap.load(Ordering::Relaxed) as f32 / 100.0;
                        capture_callback_u16(
                            data,
                            in_ch,
                            &muted_cap,
                            &mut capture_accum,
                            &mut resamp_prev,
                            &mut resamp_phase,
                            resamp_ratio,
                            &mut vad_speaking,
                            &mut vad_above_count,
                            &mut vad_below_count,
                            VAD_THRESHOLD,
                            VAD_ATTACK_FRAMES,
                            VAD_RELEASE_FRAMES,
                            &speaking_tx,
                            &level_tx,
                            &mut encoder,
                            &encoded_tx,
                            &mut noise_floor_rms,
                            ns,
                            ig,
                        );
                    },
                    |err| warn!("Capture stream error: {err}"),
                    None,
                )
                .map_err(|e| anyhow::anyhow!("Build capture stream (u16): {e}"))?,
            other => {
                return Err(anyhow::anyhow!(
                    "Unsupported input sample format: {other:?}"
                ))
            }
        };
        capture_stream
            .play()
            .map_err(|e| anyhow::anyhow!("Start capture: {e}"))?;

        // -- Playback side -----------------------------------------------
        const RING_FRAMES: usize = 50; // ~1 s of audio at 48 kHz / 960 spp
        let ring: HeapRb<i16> = HeapRb::new(RING_FRAMES * SAMPLES_PER_FRAME);
        let (playback_prod, mut playback_cons) = ring.split();

        // Resampler state for 48 kHz mono → out_sr (linear interpolation).
        let mut pb_prev: f32 = 0.0;
        let mut pb_next: f32 = 0.0;
        let mut pb_phase: f64 = 1.0; // start by pulling a fresh source sample
        let pb_ratio: f64 = SAMPLE_RATE as f64 / out_sr as f64;

        let build_output_with = |sample_fmt: cpal::SampleFormat| -> anyhow::Result<Stream> {
            match sample_fmt {
                cpal::SampleFormat::F32 => output_dev
                    .build_output_stream(
                        &output_cfg,
                        move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                            playback_callback_f32(
                                data,
                                out_ch,
                                &mut playback_cons,
                                &mut pb_prev,
                                &mut pb_next,
                                &mut pb_phase,
                                pb_ratio,
                            );
                        },
                        |err| warn!("Playback stream error: {err}"),
                        None,
                    )
                    .map_err(|e| anyhow::anyhow!("Build playback stream (f32): {e}")),
                cpal::SampleFormat::I16 => output_dev
                    .build_output_stream(
                        &output_cfg,
                        move |data: &mut [i16], _info: &cpal::OutputCallbackInfo| {
                            playback_callback_i16(
                                data,
                                out_ch,
                                &mut playback_cons,
                                &mut pb_prev,
                                &mut pb_next,
                                &mut pb_phase,
                                pb_ratio,
                            );
                        },
                        |err| warn!("Playback stream error: {err}"),
                        None,
                    )
                    .map_err(|e| anyhow::anyhow!("Build playback stream (i16): {e}")),
                cpal::SampleFormat::U16 => output_dev
                    .build_output_stream(
                        &output_cfg,
                        move |data: &mut [u16], _info: &cpal::OutputCallbackInfo| {
                            playback_callback_u16(
                                data,
                                out_ch,
                                &mut playback_cons,
                                &mut pb_prev,
                                &mut pb_next,
                                &mut pb_phase,
                                pb_ratio,
                            );
                        },
                        |err| warn!("Playback stream error: {err}"),
                        None,
                    )
                    .map_err(|e| anyhow::anyhow!("Build playback stream (u16): {e}")),
                other => Err(anyhow::anyhow!(
                    "Unsupported output sample format: {other:?}"
                )),
            }
        };
        let playback_stream = build_output_with(out_sample_fmt)?;
        playback_stream
            .play()
            .map_err(|e| anyhow::anyhow!("Start playback: {e}"))?;

        Ok((
            AudioPipeline {
                _capture_stream: capture_stream,
                _playback_stream: playback_stream,
                playback_prod,
                muted,
                noise_strength: noise_strength_arc,
                input_gain: input_gain_arc,
                output_gain: output_gain_arc,
                decoders: HashMap::new(),
            },
            encoded_rx,
            speaking_rx,
            level_rx,
        ))
    }

    /// Decode an inbound Opus frame from `peer_id`, push PCM to playback, and
    /// return the normalised RMS level (0.0–1.0) of the decoded frame.
    ///
    /// Pass `opus_data = None` to trigger Opus PLC (packet loss concealment)
    /// for a missing frame without corrupting the decoder state.
    fn push_inbound(&mut self, peer_id: &str, opus_data: Option<&[u8]>) -> f32 {
        let decoder = self
            .decoders
            .entry(peer_id.to_owned())
            .or_insert_with(|| OpusDecoder::new(48_000, 1).expect("Opus decoder init"));
        let mut pcm = [0i16; SAMPLES_PER_FRAME];
        match decoder.decode(opus_data, &mut pcm, false) {
            Ok(n) => {
                // Compute RMS from decoded PCM (i16 → normalised float),
                // then apply the same dB-scale used for the local mic capture
                // so remote levels have comparable visual weight on the ring.
                let sum_sq: f64 = pcm[..n].iter().map(|&s| (s as f64 / 32768.0).powi(2)).sum();
                let rms = (sum_sq / n as f64).sqrt() as f32;
                let level_norm: f32 = if rms < 1e-6 {
                    0.0
                } else {
                    let db = 20.0_f32 * rms.log10();
                    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
                };
                // Apply output gain and push whole frame atomically.
                // Drop the entire frame if the ring is too full to avoid
                // corrupting partial frames with individual-sample drops.
                let gain = self.output_gain.load(Ordering::Relaxed) as f32 / 100.0;
                if self.playback_prod.vacant_len() >= n {
                    for &s in &pcm[..n] {
                        let gained = (s as f32 * gain).clamp(-32768.0, 32767.0) as i16;
                        let _ = self.playback_prod.try_push(gained);
                    }
                } else {
                    debug!("Playback ring full — dropping frame from {peer_id}");
                }
                level_norm
            }
            Err(e) => {
                warn!("Opus decode error from {peer_id}: {e}");
                0.0
            }
        }
    }

    fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    fn set_noise_strength(&self, strength_idx: u32) {
        self.noise_strength
            .store(strength_idx.min(4), Ordering::Relaxed);
    }

    fn set_input_gain(&self, pct: u32) {
        self.input_gain.store(pct.min(200), Ordering::Relaxed);
    }

    fn set_output_gain(&self, pct: u32) {
        self.output_gain.store(pct.min(200), Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// State types
// ---------------------------------------------------------------------------

/// Overall call lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallState {
    Idle,
    /// Audio input is active for a mic test but no call is in progress.
    MicTest,
    Connecting,
    InCall,
    Disconnecting,
}

impl CallState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::MicTest => "mic_test",
            Self::Connecting => "connecting",
            Self::InCall => "in_call",
            Self::Disconnecting => "disconnecting",
        }
    }
}

/// Per-peer audio connection state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerAudioState {
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl PeerAudioState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}

// ---------------------------------------------------------------------------
// Commands / Events
// ---------------------------------------------------------------------------

/// Commands sent from the application layer into the call controller.
#[derive(Debug)]
pub enum CallCommand {
    /// Start audio capture and enter Connecting state.
    StartAudio { voice_activation: bool },
    /// Stop audio and close all peer connections.
    StopAudio,
    /// Connect or ensure a QUIC audio session for a peer.
    InitiatePeer {
        peer_id: String,
        host: Option<String>,
        port: Option<u16>,
    },
    /// Close a specific peer connection.
    RemovePeer { peer_id: String },
    /// Mute / unmute local microphone.
    SetMuted(bool),
    /// Enable / disable noise suppression.
    SetNoiseSuppression(bool),
    /// Set noise gate strength (0=off, 1=mild, 2=moderate, 3=aggressive, 4=max).
    SetNoiseStrength(u32),
    /// Set input microphone gain (0–200, 100=unity).
    SetInputGain(u32),
    /// Set output speaker gain (0–200, 100=unity).
    SetOutputGain(u32),
    /// Inbound direct-peer audio frame (Opus bytes from a 1:1 QUIC session).
    DirectAudioInbound { peer_id: String, opus_data: Vec<u8> },
    /// Switch PTT ↔ voice-activation.
    SetVoiceActivation(bool),
    /// Inbound room audio frame (Opus bytes from a room member).
    RoomAudioInbound { peer_id: String, opus_data: Vec<u8> },
    /// Update the jitter buffer depth (in Opus frames, 1–20). Takes effect
    /// immediately on the next playout cycle.
    SetJitterDepth(usize),
    /// Enter SFU room audio mode: outbound frames are sent via the supernode
    /// rather than directly to individual QUIC peers.
    SetRoomMode {
        supernode_id: String,
        room_id: String,
    },
    /// Leave SFU room audio mode; revert to direct peer audio.
    ClearRoomMode,
    /// Update the preferred capture / playback device names. Empty string
    /// means "use system default". Takes effect on the next audio start.
    SetAudioDevices {
        input: Option<String>,
        output: Option<String>,
    },
    /// Start a microphone test (capture + level events, no peer sending).
    StartMicTest,
    /// Stop the microphone test.
    StopMicTest,
    /// Play a short speaker test tone.
    TestSpeaker,
    /// Shutdown the controller task.
    Shutdown,
}

/// Events emitted by the call controller to the application layer.
#[derive(Debug, Clone)]
pub enum CallEvent {
    StateChanged(CallState),
    PeerAudioStateChanged {
        peer_id: String,
        state: PeerAudioState,
    },
    CallError(String),
    CaptureError(String),
    /// Per-peer metrics snapshot (JSON value for flexibility).
    MetricsUpdated(serde_json::Value),
    LocalSpeakingChanged(bool),
    LocalLevelChanged(f32),
    RemoteSpeakingChanged {
        peer_id: String,
        speaking: bool,
    },
    RemoteLevelChanged {
        peer_id: String,
        level: f32,
    },
}

// ---------------------------------------------------------------------------
// Per-peer audio session (stub — full impl requires conquerd-audio)
// ---------------------------------------------------------------------------

struct PeerAudioSession {
    peer_id: String,
    state: PeerAudioState,
    /// Jitter buffer depth (frames).
    jitter_depth: usize,
}

impl PeerAudioSession {
    fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            state: PeerAudioState::Connecting,
            jitter_depth: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// CallController
// ---------------------------------------------------------------------------

/// Owns the call lifecycle and audio peer sessions.
///
/// Create with [`CallController::split`], spawn the returned future, and
/// communicate via the channels.
pub struct CallController {
    state: CallState,
    muted: bool,
    voice_activation: bool,
    peers: HashMap<String, PeerAudioSession>,

    event_tx: mpsc::Sender<CallEvent>,
    cmd_rx: mpsc::Receiver<CallCommand>,

    /// Connection manager command channel for sending audio datagrams.
    cm_cmd_tx: Option<mpsc::Sender<ConnectionCommand>>,

    /// True when audio should be sent to the SFU supernode rather than
    /// directly to individual QUIC peers.
    room_mode: bool,

    /// Live audio pipeline (None when idle).
    audio: Option<AudioPipeline>,
    /// Outbound encoded Opus frames from the capture callback (None when idle).
    encoded_rx: Option<mpsc::Receiver<Vec<u8>>>,
    /// Speaking-state events from the inline VAD in the capture callback.
    speaking_rx: Option<tokio::sync::mpsc::UnboundedReceiver<bool>>,
    /// Normalized audio level events (0.0–1.0) emitted each Opus frame.
    level_rx: Option<tokio::sync::mpsc::UnboundedReceiver<f32>>,
    /// Cached local speaking state (used to suppress redundant events and for
    /// immediate false-emission on mute).
    local_speaking: bool,
    /// User-selected capture device name (empty = system default).
    input_device: Option<String>,
    /// User-selected playback device name (empty = system default).
    output_device: Option<String>,

    /// Timestamp of the last audio frame received from each remote room peer.
    /// Used to derive speaking state: peer is "speaking" while frames arrive
    /// and transitions to silent after PEER_SILENCE_TIMEOUT with no frames.
    room_peer_last_audio: HashMap<String, std::time::Instant>,

    /// Timestamp of the last `RemoteLevelChanged` event emitted per peer.
    /// Throttles level updates to ≤10 Hz (100 ms) to keep the model reset
    /// rate manageable.
    room_peer_last_level: HashMap<String, std::time::Instant>,

    /// Per-peer incoming Opus frame queues for jitter buffering.
    /// Frames are pushed on arrival and popped on the 20 ms playout tick.
    peer_jitter_queues: HashMap<String, VecDeque<Vec<u8>>>,
    /// Tracks whether a peer has accumulated enough frames to begin playout
    /// (i.e. has passed the initial buffering phase).
    peer_playout_started: HashMap<String, bool>,
    /// Jitter buffer depth in Opus frames (1 frame = 20 ms). Configurable
    /// via `CallCommand::SetJitterDepth`.
    jitter_depth: usize,
    /// Input gain 0–200 (100=unity). Sent to AudioPipeline on start.
    input_vol: u32,
    /// Output gain 0–200 (100=unity). Sent to AudioPipeline on start.
    output_vol: u32,
    /// Noise gate strength index 0–4. Sent to AudioPipeline on start.
    noise_strength_idx: u32,
}

impl CallController {
    /// Create a controller and split it into channels + a runnable future.
    ///
    /// Returns `(cmd_tx, event_rx, task_future)`. Spawn the future with
    /// `tokio::spawn`.
    pub fn split(
        cm_cmd_tx: Option<mpsc::Sender<ConnectionCommand>>,
    ) -> (
        mpsc::Sender<CallCommand>,
        mpsc::Receiver<CallEvent>,
        impl std::future::Future<Output = ()>,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<CallEvent>(256);
        let (cmd_tx, cmd_rx) = mpsc::channel::<CallCommand>(64);
        let ctrl = Self {
            state: CallState::Idle,
            muted: true,
            voice_activation: false,
            peers: HashMap::new(),
            event_tx,
            cmd_rx,
            cm_cmd_tx,
            room_mode: false,
            audio: None,
            encoded_rx: None,
            speaking_rx: None,
            level_rx: None,
            local_speaking: false,
            input_device: None,
            output_device: None,
            room_peer_last_audio: HashMap::new(),
            room_peer_last_level: HashMap::new(),
            peer_jitter_queues: HashMap::new(),
            peer_playout_started: HashMap::new(),
            jitter_depth: 3,
            input_vol: 100,
            output_vol: 100,
            noise_strength_idx: 2,
        };
        (cmd_tx, event_rx, ctrl.run())
    }

    // -- Internal helpers ---------------------------------------------------

    fn set_state(&mut self, new_state: CallState) {
        if self.state != new_state {
            info!("Call state: {:?} → {:?}", self.state, new_state);
            self.state = new_state.clone();
            let _ = self.event_tx.try_send(CallEvent::StateChanged(new_state));
        }
    }

    fn emit_peer_state(&self, peer_id: &str, state: PeerAudioState) {
        let _ = self.event_tx.try_send(CallEvent::PeerAudioStateChanged {
            peer_id: peer_id.to_string(),
            state,
        });
    }

    // -- Command handlers ---------------------------------------------------

    fn handle_start_audio(&mut self, voice_activation: bool) {
        if self.state != CallState::Idle {
            return;
        }
        self.voice_activation = voice_activation;
        self.muted = !voice_activation; // PTT starts muted; VAD starts open

        // Transition first so UI updates immediately.
        self.set_state(CallState::Connecting);

        match AudioPipeline::start(
            self.muted,
            self.input_device.as_deref(),
            self.output_device.as_deref(),
            self.input_vol,
            self.output_vol,
            self.noise_strength_idx,
        ) {
            Ok((pipeline, encoded_rx, speaking_rx, level_rx)) => {
                self.audio = Some(pipeline);
                self.encoded_rx = Some(encoded_rx);
                self.speaking_rx = Some(speaking_rx);
                self.level_rx = Some(level_rx);
                let mode = if voice_activation { "VAD" } else { "PTT" };
                info!("Audio pipeline started (mode={mode})");
            }
            Err(e) => {
                error!("Failed to start audio pipeline: {e}");
                // Emit error advisory; call continues (relay-only / remote playback).
                let _ = self
                    .event_tx
                    .try_send(CallEvent::CaptureError(e.to_string()));
            }
        }
    }

    fn handle_stop_audio(&mut self) {
        self.set_state(CallState::Disconnecting);
        let peer_ids: Vec<String> = self.peers.keys().cloned().collect();
        for pid in peer_ids {
            self.emit_peer_state(&pid, PeerAudioState::Closed);
        }
        self.peers.clear();
        // Drop the pipeline — CPAL streams stop when their handle is dropped.
        self.audio = None;
        self.encoded_rx = None;
        self.speaking_rx = None;
        self.level_rx = None;
        if self.local_speaking {
            self.local_speaking = false;
            let _ = self
                .event_tx
                .try_send(CallEvent::LocalSpeakingChanged(false));
        }
        let _ = self.event_tx.try_send(CallEvent::LocalLevelChanged(0.0));
        self.set_state(CallState::Idle);
        info!("Call ended, audio pipeline stopped");
    }

    fn handle_initiate_peer(&mut self, peer_id: String, host: Option<String>, port: Option<u16>) {
        if !matches!(self.state, CallState::Connecting | CallState::InCall) {
            return;
        }
        self.peers
            .entry(peer_id.clone())
            .or_insert_with(|| PeerAudioSession::new(&peer_id));
        self.emit_peer_state(&peer_id, PeerAudioState::Connecting);
        if let (Some(h), Some(p)) = (&host, port) {
            info!(
                "Initiating QUIC audio to {}:{} for peer {}",
                h,
                p,
                &peer_id[..12.min(peer_id.len())]
            );
            // Establish QUIC peer connection via the connection manager.
            if let Some(ref tx) = self.cm_cmd_tx {
                let _ = tx.try_send(ConnectionCommand::ConnectDirect {
                    peer_id: peer_id.clone(),
                    host: h.clone(),
                    port: p,
                });
            }
        } else {
            debug!(
                "Ensuring session bookkeeping for peer {}",
                &peer_id[..12.min(peer_id.len())]
            );
        }
    }

    fn handle_remove_peer(&mut self, peer_id: &str) {
        // Clear any buffered audio for this peer.
        self.peer_jitter_queues.remove(peer_id);
        self.peer_playout_started.remove(peer_id);
        if self.peers.remove(peer_id).is_some() {
            self.emit_peer_state(peer_id, PeerAudioState::Closed);
            // Tell the connection manager to drop this peer's QUIC session.
            if let Some(ref tx) = self.cm_cmd_tx {
                // Reuse SendAudioFrame channel tag: send a zero-length frame as
                // a "close" hint. A dedicated DisconnectPeer command is cleaner
                // but requires a CM-side handler; for now we just stop forwarding
                // audio and let QUIC idle-timeout clean up.
                let _ = tx.try_send(ConnectionCommand::SendAudioFrame {
                    peer_id: peer_id.to_owned(),
                    opus_data: Vec::new(), // zero-length = close hint
                });
            }
        }
    }

    fn handle_room_audio(&mut self, peer_id: String, opus_data: Vec<u8>) {
        use std::time::{Duration, Instant};
        const PEER_SILENCE_TIMEOUT: Duration = Duration::from_millis(600);

        let now = Instant::now();
        let was_silent = self
            .room_peer_last_audio
            .get(&peer_id)
            .map(|t| now.duration_since(*t) > PEER_SILENCE_TIMEOUT)
            .unwrap_or(true);
        self.room_peer_last_audio.insert(peer_id.clone(), now);
        if was_silent {
            let _ = self.event_tx.try_send(CallEvent::RemoteSpeakingChanged {
                peer_id: peer_id.clone(),
                speaking: true,
            });
        }

        // Enqueue for jitter-buffered playout; decode happens on the 20 ms
        // playout tick so irregular network arrivals don't cause clicks/pops.
        let queue = self.peer_jitter_queues.entry(peer_id).or_default();
        queue.push_back(opus_data);
        // Cap queue at 8× depth to bound memory under extreme bursts.
        let max_depth = (self.jitter_depth * 8).max(16);
        while queue.len() > max_depth {
            queue.pop_front();
        }
    }

    /// Advance jitter buffers by one 20 ms Opus frame for every active room
    /// peer.  Called from the 20 ms `playout_tick` in `run()`.
    ///
    /// - If a peer's queue hasn't yet reached `jitter_depth`, we skip it
    ///   (initial buffering phase — introduces target_depth × 20 ms latency).
    /// - Once playout has started, an empty queue triggers Opus PLC so the
    ///   decoder state stays coherent during brief packet-loss gaps.
    /// - If the peer goes fully silent (last packet > PEER_SILENCE_TIMEOUT ago)
    ///   and the queue is empty, we clean up its playout state.
    fn tick_playout(&mut self) {
        use std::time::{Duration, Instant};
        const PEER_SILENCE_TIMEOUT: Duration = Duration::from_millis(600);
        const LEVEL_UPDATE_INTERVAL: Duration = Duration::from_millis(100);

        if self.audio.is_none() {
            return;
        }
        let now = Instant::now();

        // Two-pass to avoid simultaneous mutable borrow of peer_jitter_queues
        // while also needing to remove entries from it.
        let peer_ids: Vec<String> = self.peer_jitter_queues.keys().cloned().collect();
        let mut to_remove: Vec<String> = Vec::new();
        // (peer_id, Some(frame) | None-for-PLC)
        let mut to_decode: Vec<(String, Option<Vec<u8>>)> = Vec::new();

        for peer_id in peer_ids {
            let recently_active = self
                .room_peer_last_audio
                .get(&peer_id)
                .map(|t| now.duration_since(*t) < PEER_SILENCE_TIMEOUT)
                .unwrap_or(false);

            let queue_len = self
                .peer_jitter_queues
                .get(&peer_id)
                .map(|q| q.len())
                .unwrap_or(0);

            let started = *self.peer_playout_started.get(&peer_id).unwrap_or(&false);

            if !started {
                // Accumulate until we have enough frames to smooth over jitter.
                if queue_len >= self.jitter_depth {
                    self.peer_playout_started.insert(peer_id.clone(), true);
                    // Fall through to pop below.
                } else {
                    continue; // Still buffering.
                }
            }

            if queue_len == 0 && !recently_active {
                // Peer has gone silent and we have nothing left to play.
                to_remove.push(peer_id);
                continue;
            }

            // Pop the next queued frame (None → PLC for this slot).
            let frame = self
                .peer_jitter_queues
                .get_mut(&peer_id)
                .and_then(|q| q.pop_front());
            to_decode.push((peer_id, frame));
        }

        for peer_id in to_remove {
            self.peer_jitter_queues.remove(&peer_id);
            self.peer_playout_started.remove(&peer_id);
        }

        for (peer_id, frame) in to_decode {
            let level = if let Some(ref mut pipeline) = self.audio {
                pipeline.push_inbound(&peer_id, frame.as_deref())
            } else {
                return;
            };

            // Throttle level events to ≤10 Hz per peer.
            let should_emit = self
                .room_peer_last_level
                .get(&peer_id)
                .map(|t| now.duration_since(*t) >= LEVEL_UPDATE_INTERVAL)
                .unwrap_or(true);
            if should_emit && level > 0.0 {
                self.room_peer_last_level.insert(peer_id.clone(), now);
                let _ = self
                    .event_tx
                    .try_send(CallEvent::RemoteLevelChanged { peer_id, level });
            }
        }
    }

    fn handle_start_mic_test(&mut self) {
        if self.state != CallState::Idle {
            return;
        }
        self.set_state(CallState::MicTest);
        match AudioPipeline::start(
            false,
            self.input_device.as_deref(),
            self.output_device.as_deref(),
            self.input_vol,
            self.output_vol,
            self.noise_strength_idx,
        ) {
            Ok((pipeline, encoded_rx, speaking_rx, level_rx)) => {
                self.audio = Some(pipeline);
                self.encoded_rx = Some(encoded_rx);
                self.speaking_rx = Some(speaking_rx);
                self.level_rx = Some(level_rx);
                info!("Mic test started");
            }
            Err(e) => {
                error!("Failed to start audio for mic test: {e}");
                let _ = self
                    .event_tx
                    .try_send(CallEvent::CaptureError(e.to_string()));
                self.set_state(CallState::Idle);
            }
        }
    }

    fn handle_stop_mic_test(&mut self) {
        if self.state != CallState::MicTest {
            return;
        }
        self.audio = None;
        self.encoded_rx = None;
        self.speaking_rx = None;
        self.level_rx = None;
        if self.local_speaking {
            self.local_speaking = false;
            let _ = self
                .event_tx
                .try_send(CallEvent::LocalSpeakingChanged(false));
        }
        let _ = self.event_tx.try_send(CallEvent::LocalLevelChanged(0.0));
        self.set_state(CallState::Idle);
        info!("Mic test stopped");
    }

    // -- Main event loop ----------------------------------------------------

    async fn run(mut self) {
        info!("CallController started");
        let mut metrics_tick = tokio::time::interval(Duration::from_secs(2));
        let mut speaking_tick = tokio::time::interval(Duration::from_millis(600));
        // 20 ms playout tick — one Opus frame per peer per tick.
        let mut playout_tick = tokio::time::interval(Duration::from_millis(20));
        // Discard the first (immediate) tick so silence-detection only fires
        // after real intervals.
        speaking_tick.reset();
        playout_tick.reset();

        loop {
            tokio::select! {
                _ = playout_tick.tick() => {
                    self.tick_playout();
                }
                _ = speaking_tick.tick() => {
                    // Expire speaking state for room peers that have gone silent.
                    if !self.room_peer_last_audio.is_empty() {
                        use std::time::{Duration, Instant};
                        const PEER_SILENCE_TIMEOUT: Duration = Duration::from_millis(600);
                        let now = Instant::now();
                        let silent: Vec<String> = self
                            .room_peer_last_audio
                            .iter()
                            .filter(|(_, t)| now.duration_since(**t) > PEER_SILENCE_TIMEOUT)
                            .map(|(id, _)| id.clone())
                            .collect();
                        for peer_id in silent {
                            self.room_peer_last_audio.remove(&peer_id);
                            let _ = self.event_tx.try_send(CallEvent::RemoteSpeakingChanged {
                                peer_id,
                                speaking: false,
                            });
                        }
                    }
                }
                _ = metrics_tick.tick() => {
                    // Emit per-peer audio metrics snapshot.
                    if self.state != CallState::Idle && !self.peers.is_empty() {
                        let peers: Vec<serde_json::Value> = self.peers.values().map(|s| {
                            serde_json::json!({
                                "peer_id": s.peer_id,
                                "state": s.state.as_str(),
                                "jitter_depth": s.jitter_depth,
                            })
                        }).collect();
                        let payload = serde_json::json!({ "peers": peers });
                        let _ = self.event_tx.try_send(CallEvent::MetricsUpdated(payload));
                    }
                }
                // Poll outbound Opus frames when the audio pipeline is active.
                frame = async {
                    match &mut self.encoded_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(bytes) = frame {
                        self.broadcast_audio_frame(bytes).await;
                    }
                }
                // Poll VAD speaking-state events from the capture callback.
                speaking = async {
                    match &mut self.speaking_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(s) = speaking {
                        if s != self.local_speaking {
                            self.local_speaking = s;
                            let _ = self.event_tx
                                .try_send(CallEvent::LocalSpeakingChanged(s));
                        }
                    }
                }
                // Poll audio level events from the capture callback.
                level = async {
                    match &mut self.level_rx {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(lvl) = level {
                        let _ = self.event_tx.try_send(CallEvent::LocalLevelChanged(lvl));
                    }
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        CallCommand::Shutdown => {
                            info!("CallController shutting down");
                            self.handle_stop_audio();
                            break;
                        }
                        CallCommand::StartAudio { voice_activation } => {
                            self.handle_start_audio(voice_activation);
                        }
                        CallCommand::StopAudio => {
                            self.handle_stop_audio();
                        }
                        CallCommand::InitiatePeer { peer_id, host, port } => {
                            self.handle_initiate_peer(peer_id, host, port);
                        }
                        CallCommand::RemovePeer { peer_id } => {
                            self.handle_remove_peer(&peer_id);
                        }
                        CallCommand::SetMuted(m) => {
                            self.muted = m;
                            if let Some(p) = &self.audio {
                                p.set_muted(m);
                            }
                            // When muted, clear speaking immediately without
                            // waiting for the VAD release countdown.
                            if m && self.local_speaking {
                                self.local_speaking = false;
                                let _ = self.event_tx
                                    .try_send(CallEvent::LocalSpeakingChanged(false));
                            }
                            debug!("Mute set to {}", m);
                        }
                        CallCommand::SetNoiseSuppression(enabled) => {
                            if !enabled {
                                self.noise_strength_idx = 0;
                            } else if self.noise_strength_idx == 0 {
                                self.noise_strength_idx = 2; // default to moderate
                            }
                            if let Some(p) = &self.audio {
                                p.set_noise_strength(self.noise_strength_idx);
                            }
                        }
                        CallCommand::SetNoiseStrength(idx) => {
                            self.noise_strength_idx = idx.min(4);
                            if let Some(p) = &self.audio {
                                p.set_noise_strength(self.noise_strength_idx);
                            }
                        }
                        CallCommand::SetInputGain(pct) => {
                            self.input_vol = pct.min(200);
                            if let Some(p) = &self.audio {
                                p.set_input_gain(self.input_vol);
                            }
                        }
                        CallCommand::SetOutputGain(pct) => {
                            self.output_vol = pct.min(200);
                            if let Some(p) = &self.audio {
                                p.set_output_gain(self.output_vol);
                            }
                        }
                        CallCommand::DirectAudioInbound { peer_id, opus_data } => {
                            // Treat direct 1:1 audio through the same jitter-buffered
                            // path as room audio so it benefits from playout smoothing.
                            self.handle_room_audio(peer_id, opus_data);
                        }
                        CallCommand::SetVoiceActivation(enabled) => {
                            self.voice_activation = enabled;
                            if self.state != CallState::Idle && enabled {
                                self.muted = false;
                                if let Some(p) = &self.audio {
                                    p.set_muted(false);
                                }
                            }
                            debug!("Voice-activation mode: {}", enabled);
                        }
                        CallCommand::RoomAudioInbound { peer_id, opus_data } => {
                            self.handle_room_audio(peer_id, opus_data);
                        }
                        CallCommand::SetJitterDepth(depth) => {
                            self.jitter_depth = depth.clamp(1, 20);
                            debug!("Jitter buffer depth set to {} frames ({} ms)", self.jitter_depth, self.jitter_depth * 20);
                        }
                        CallCommand::SetRoomMode { supernode_id, room_id } => {
                            self.room_mode = true;
                            debug!("Call controller: entered room audio mode (supernode={}, room={})", supernode_id, room_id);
                        }
                        CallCommand::ClearRoomMode => {
                            self.room_mode = false;
                            debug!("Call controller: cleared room audio mode");
                        }
                        CallCommand::SetAudioDevices { input, output } => {
                            self.input_device = input.and_then(|s| {
                                let t = s.trim().to_string();
                                if t.is_empty() { None } else { Some(t) }
                            });
                            self.output_device = output.and_then(|s| {
                                let t = s.trim().to_string();
                                if t.is_empty() { None } else { Some(t) }
                            });
                            debug!(
                                "Audio devices updated: input={:?}, output={:?}",
                                self.input_device, self.output_device
                            );
                        }
                        CallCommand::StartMicTest => {
                            self.handle_start_mic_test();
                        }
                        CallCommand::StopMicTest => {
                            self.handle_stop_mic_test();
                        }
                        CallCommand::TestSpeaker => {
                            tokio::task::spawn_blocking(play_speaker_test_beep);
                        }
                    }
                }
                else => break,
            }
        }
        info!("CallController stopped");
    }

    /// Broadcast an Opus frame to all connected peers via QUIC datagrams, or
    /// to the SFU supernode when in room mode.
    /// In MicTest state, loops the frame back through the local decoder so
    /// the user hears their own voice through the playback device.
    async fn broadcast_audio_frame(&mut self, frame: Vec<u8>) {
        if self.state == CallState::MicTest {
            if let Some(pipeline) = &mut self.audio {
                pipeline.push_inbound("__mic_test__", Some(&frame));
            }
            return;
        }
        if let Some(ref tx) = self.cm_cmd_tx {
            if self.room_mode {
                // SFU room mode: route audio through the supernode's WebSocket
                // broadcast so every room member receives it without requiring
                // direct QUIC connectivity between peers.
                let _ = tx.try_send(ConnectionCommand::SendRoomAudio { opus_data: frame });
            } else {
                // Direct call mode: send to each QUIC-connected peer.
                for peer_id in self.peers.keys() {
                    let _ = tx.try_send(ConnectionCommand::SendAudioFrame {
                        peer_id: peer_id.clone(),
                        opus_data: frame.clone(),
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Capture / playback callback helpers
// ---------------------------------------------------------------------------
//
// CPAL stream callbacks must accept whatever sample-format the device's
// default config exposes. To support F32 / I16 / U16 inputs and outputs
// without duplicating the entire VAD + Opus + resampler logic, the per-format
// callbacks normalise to f32 mono, run a shared linear resampler down/up to
// 48 kHz, and delegate to a shared inner core.

/// Adaptive noise gate applied on a per-20ms-frame basis.
///
/// Tracks a slow-moving noise floor via exponential moving average and applies
/// a smooth gain ramp when signal energy drops below a strength-dependent
/// multiple of that floor.  Higher `strength_idx` = more aggressive gating.
///
/// * 0 — off (no processing)
/// * 1 — mild (4× floor)
/// * 2 — moderate (8× floor)
/// * 3 — aggressive (16× floor)
/// * 4 — max (32× floor)
fn apply_noise_gate(frame: &mut [i16], noise_floor: &mut f32, strength_idx: u32) {
    if strength_idx == 0 || frame.is_empty() {
        return;
    }
    let rms = {
        let sq_sum: f64 = frame.iter().map(|&s| (s as f64).powi(2)).sum();
        (sq_sum / frame.len() as f64).sqrt() as f32
    };
    // Update noise floor: fast attack (floor rises quickly) / slow release.
    if rms < *noise_floor {
        *noise_floor = *noise_floor * (1.0 - 0.05) + rms * 0.05;
    } else {
        *noise_floor = *noise_floor * (1.0 - 0.001) + rms * 0.001;
    }
    // Clamp floor so it doesn’t collapse to zero in total silence.
    *noise_floor = noise_floor.max(1.0);
    let multiplier: f32 = match strength_idx {
        1 => 4.0,
        2 => 8.0,
        3 => 16.0,
        _ => 32.0,
    };
    let threshold = *noise_floor * multiplier;
    if rms < threshold {
        // Smooth gain ramp: approaches 0.05 at the floor, 1.0 at threshold.
        let gate_gain = (rms / threshold).sqrt().clamp(0.05, 1.0);
        for s in frame.iter_mut() {
            *s = (*s as f32 * gate_gain) as i16;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_capture_mono_f32(
    mono_in: &[f32],
    capture_accum: &mut Vec<i16>,
    resamp_prev: &mut f32,
    resamp_phase: &mut f64,
    resamp_ratio: f64,
    vad_speaking: &mut bool,
    vad_above_count: &mut u32,
    vad_below_count: &mut u32,
    vad_threshold: f32,
    vad_attack_frames: u32,
    vad_release_frames: u32,
    speaking_tx: &tokio::sync::mpsc::UnboundedSender<bool>,
    level_tx: &tokio::sync::mpsc::UnboundedSender<f32>,
    encoder: &mut OpusEncoder,
    encoded_tx: &mpsc::Sender<Vec<u8>>,
    noise_floor: &mut f32,
    noise_strength_idx: u32,
    input_gain: f32,
) {
    // Linear-interpolation resampler: input rate → 48 kHz.
    for &src in mono_in {
        while *resamp_phase < 1.0 {
            let interp = *resamp_prev * (1.0 - *resamp_phase as f32) + src * (*resamp_phase as f32);
            // Apply input gain and clamp to [-1, 1] before quantising.
            capture_accum.push(((interp * input_gain).clamp(-1.0, 1.0) * 32_767.0) as i16);
            *resamp_phase += resamp_ratio;
        }
        *resamp_phase -= 1.0;
        *resamp_prev = src;
    }

    // Drain complete 20 ms frames at 48 kHz.
    let mut opus_buf = [0u8; 4096];
    while capture_accum.len() >= SAMPLES_PER_FRAME {
        let mut frame: Vec<i16> = capture_accum.drain(..SAMPLES_PER_FRAME).collect();

        // Noise gate before VAD/encode so the gate doesn't trip on background noise.
        apply_noise_gate(&mut frame, noise_floor, noise_strength_idx);

        // RMS energy → VAD + level meter.
        let rms_sq: f64 =
            frame.iter().map(|&s| (s as f64) * (s as f64)).sum::<f64>() / frame.len() as f64;
        let rms = rms_sq.sqrt() as f32;
        let level_norm: f32 = if rms < 1.0 {
            0.0
        } else {
            let db = 20.0 * (rms / 32_767.0_f32).log10();
            ((db + 60.0) / 60.0).clamp(0.0, 1.0)
        };
        let _ = level_tx.send(level_norm);
        if rms >= vad_threshold {
            *vad_above_count = vad_above_count.saturating_add(1);
            *vad_below_count = 0;
            if !*vad_speaking && *vad_above_count >= vad_attack_frames {
                *vad_speaking = true;
                let _ = speaking_tx.send(true);
            }
        } else {
            *vad_below_count = vad_below_count.saturating_add(1);
            *vad_above_count = 0;
            if *vad_speaking && *vad_below_count >= vad_release_frames {
                *vad_speaking = false;
                let _ = speaking_tx.send(false);
            }
        }

        match encoder.encode(&frame, &mut opus_buf) {
            Ok(n) if n > 0 => {
                let _ = encoded_tx.try_send(opus_buf[..n].to_vec());
            }
            Ok(_) => {}
            Err(e) => warn!("Opus encode error: {e}"),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_callback_f32(
    data: &[f32],
    in_ch: usize,
    muted: &Arc<AtomicBool>,
    capture_accum: &mut Vec<i16>,
    resamp_prev: &mut f32,
    resamp_phase: &mut f64,
    resamp_ratio: f64,
    vad_speaking: &mut bool,
    vad_above_count: &mut u32,
    vad_below_count: &mut u32,
    vad_threshold: f32,
    vad_attack_frames: u32,
    vad_release_frames: u32,
    speaking_tx: &tokio::sync::mpsc::UnboundedSender<bool>,
    level_tx: &tokio::sync::mpsc::UnboundedSender<f32>,
    encoder: &mut OpusEncoder,
    encoded_tx: &mpsc::Sender<Vec<u8>>,
    noise_floor: &mut f32,
    noise_strength_idx: u32,
    input_gain: f32,
) {
    if muted.load(Ordering::Relaxed) {
        capture_accum.clear();
        *vad_above_count = 0;
        return;
    }
    // Downmix to mono by averaging interleaved channels.
    let frames = data.len() / in_ch.max(1);
    let mut mono = Vec::with_capacity(frames);
    if in_ch <= 1 {
        mono.extend_from_slice(data);
    } else {
        let inv = 1.0 / in_ch as f32;
        for f in 0..frames {
            let base = f * in_ch;
            let sum: f32 = data[base..base + in_ch].iter().copied().sum();
            mono.push(sum * inv);
        }
    }
    process_capture_mono_f32(
        &mono,
        capture_accum,
        resamp_prev,
        resamp_phase,
        resamp_ratio,
        vad_speaking,
        vad_above_count,
        vad_below_count,
        vad_threshold,
        vad_attack_frames,
        vad_release_frames,
        speaking_tx,
        level_tx,
        encoder,
        encoded_tx,
        noise_floor,
        noise_strength_idx,
        input_gain,
    );
}

#[allow(clippy::too_many_arguments)]
fn capture_callback_i16(
    data: &[i16],
    in_ch: usize,
    muted: &Arc<AtomicBool>,
    capture_accum: &mut Vec<i16>,
    resamp_prev: &mut f32,
    resamp_phase: &mut f64,
    resamp_ratio: f64,
    vad_speaking: &mut bool,
    vad_above_count: &mut u32,
    vad_below_count: &mut u32,
    vad_threshold: f32,
    vad_attack_frames: u32,
    vad_release_frames: u32,
    speaking_tx: &tokio::sync::mpsc::UnboundedSender<bool>,
    level_tx: &tokio::sync::mpsc::UnboundedSender<f32>,
    encoder: &mut OpusEncoder,
    encoded_tx: &mpsc::Sender<Vec<u8>>,
    noise_floor: &mut f32,
    noise_strength_idx: u32,
    input_gain: f32,
) {
    let buf: Vec<f32> = data.iter().map(|&s| s as f32 / 32_768.0).collect();
    capture_callback_f32(
        &buf,
        in_ch,
        muted,
        capture_accum,
        resamp_prev,
        resamp_phase,
        resamp_ratio,
        vad_speaking,
        vad_above_count,
        vad_below_count,
        vad_threshold,
        vad_attack_frames,
        vad_release_frames,
        speaking_tx,
        level_tx,
        encoder,
        encoded_tx,
        noise_floor,
        noise_strength_idx,
        input_gain,
    );
}

#[allow(clippy::too_many_arguments)]
fn capture_callback_u16(
    data: &[u16],
    in_ch: usize,
    muted: &Arc<AtomicBool>,
    capture_accum: &mut Vec<i16>,
    resamp_prev: &mut f32,
    resamp_phase: &mut f64,
    resamp_ratio: f64,
    vad_speaking: &mut bool,
    vad_above_count: &mut u32,
    vad_below_count: &mut u32,
    vad_threshold: f32,
    vad_attack_frames: u32,
    vad_release_frames: u32,
    speaking_tx: &tokio::sync::mpsc::UnboundedSender<bool>,
    level_tx: &tokio::sync::mpsc::UnboundedSender<f32>,
    encoder: &mut OpusEncoder,
    encoded_tx: &mpsc::Sender<Vec<u8>>,
    noise_floor: &mut f32,
    noise_strength_idx: u32,
    input_gain: f32,
) {
    let buf: Vec<f32> = data
        .iter()
        .map(|&s| (s as f32 - 32_768.0) / 32_768.0)
        .collect();
    capture_callback_f32(
        &buf,
        in_ch,
        muted,
        capture_accum,
        resamp_prev,
        resamp_phase,
        resamp_ratio,
        vad_speaking,
        vad_above_count,
        vad_below_count,
        vad_threshold,
        vad_attack_frames,
        vad_release_frames,
        speaking_tx,
        level_tx,
        encoder,
        encoded_tx,
        noise_floor,
        noise_strength_idx,
        input_gain,
    );
}

/// Pull one 48 kHz mono i16 sample from the ring (0 on underrun).
fn pb_pull_source(playback_cons: &mut ringbuf::HeapCons<i16>) -> f32 {
    let s = playback_cons.try_pop().unwrap_or(0);
    s as f32 / 32_767.0
}

/// Generate `out_ch` interleaved f32 samples at `out_sr` from a 48 kHz mono
/// source ring using linear interpolation.
fn playback_callback_f32(
    data: &mut [f32],
    out_ch: usize,
    playback_cons: &mut ringbuf::HeapCons<i16>,
    pb_prev: &mut f32,
    pb_next: &mut f32,
    pb_phase: &mut f64,
    pb_ratio: f64,
) {
    let och = out_ch.max(1);
    let frames = data.len() / och;
    for f in 0..frames {
        while *pb_phase >= 1.0 {
            *pb_prev = *pb_next;
            *pb_next = pb_pull_source(playback_cons);
            *pb_phase -= 1.0;
        }
        let interp = *pb_prev * (1.0 - *pb_phase as f32) + *pb_next * (*pb_phase as f32);
        let base = f * och;
        for c in 0..och {
            data[base + c] = interp;
        }
        *pb_phase += pb_ratio;
    }
}

fn playback_callback_i16(
    data: &mut [i16],
    out_ch: usize,
    playback_cons: &mut ringbuf::HeapCons<i16>,
    pb_prev: &mut f32,
    pb_next: &mut f32,
    pb_phase: &mut f64,
    pb_ratio: f64,
) {
    let och = out_ch.max(1);
    let frames = data.len() / och;
    for f in 0..frames {
        while *pb_phase >= 1.0 {
            *pb_prev = *pb_next;
            *pb_next = pb_pull_source(playback_cons);
            *pb_phase -= 1.0;
        }
        let interp = *pb_prev * (1.0 - *pb_phase as f32) + *pb_next * (*pb_phase as f32);
        let s = (interp.clamp(-1.0, 1.0) * 32_767.0) as i16;
        let base = f * och;
        for c in 0..och {
            data[base + c] = s;
        }
        *pb_phase += pb_ratio;
    }
}

fn playback_callback_u16(
    data: &mut [u16],
    out_ch: usize,
    playback_cons: &mut ringbuf::HeapCons<i16>,
    pb_prev: &mut f32,
    pb_next: &mut f32,
    pb_phase: &mut f64,
    pb_ratio: f64,
) {
    let och = out_ch.max(1);
    let frames = data.len() / och;
    for f in 0..frames {
        while *pb_phase >= 1.0 {
            *pb_prev = *pb_next;
            *pb_next = pb_pull_source(playback_cons);
            *pb_phase -= 1.0;
        }
        let interp = *pb_prev * (1.0 - *pb_phase as f32) + *pb_next * (*pb_phase as f32);
        let s = ((interp.clamp(-1.0, 1.0) * 32_767.0) as i32 + 32_768) as u16;
        let base = f * och;
        for c in 0..och {
            data[base + c] = s;
        }
        *pb_phase += pb_ratio;
    }
}

// ---------------------------------------------------------------------------
// Speaker test
// ---------------------------------------------------------------------------

/// Play a 440 Hz sine-wave beep for ~1.2 s on the default output device,
/// honouring the device's native sample-rate / channel-count / sample-format.
/// Runs on a blocking thread (via `tokio::task::spawn_blocking`).
fn play_speaker_test_beep() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let Some(output_dev) = host.default_output_device() else {
        warn!("Speaker test: no default output device");
        return;
    };
    let default_cfg = match output_dev.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            warn!("Speaker test: could not query default output config: {e}");
            return;
        }
    };
    let sample_rate = default_cfg.sample_rate().0 as f32;
    let channels = default_cfg.channels() as usize;
    let sample_fmt = default_cfg.sample_format();
    let cfg = cpal::StreamConfig {
        channels: default_cfg.channels(),
        sample_rate: default_cfg.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };
    const BEEP_HZ: f32 = 440.0;
    const AMPLITUDE: f32 = 0.4;
    const DURATION_SECS: f32 = 1.2;
    let total_samples = (sample_rate * DURATION_SECS) as usize;

    fn next_sine(idx: usize, sr: f32) -> f32 {
        let t = idx as f32 / sr;
        (2.0 * std::f32::consts::PI * BEEP_HZ * t).sin() * AMPLITUDE
    }

    let err_cb = |e| warn!("Speaker test stream error: {e}");
    let stream_result: Result<cpal::Stream, cpal::BuildStreamError> = match sample_fmt {
        cpal::SampleFormat::F32 => {
            let mut sample_count = 0usize;
            output_dev.build_output_stream(
                &cfg,
                move |data: &mut [f32], _| {
                    let frames = data.len() / channels.max(1);
                    for f in 0..frames {
                        let v = if sample_count < total_samples {
                            let s = next_sine(sample_count, sample_rate);
                            sample_count += 1;
                            s
                        } else {
                            0.0
                        };
                        let base = f * channels.max(1);
                        for c in 0..channels.max(1) {
                            data[base + c] = v;
                        }
                    }
                },
                err_cb,
                None,
            )
        }
        cpal::SampleFormat::I16 => {
            let mut sample_count = 0usize;
            output_dev.build_output_stream(
                &cfg,
                move |data: &mut [i16], _| {
                    let frames = data.len() / channels.max(1);
                    for f in 0..frames {
                        let v = if sample_count < total_samples {
                            let s = next_sine(sample_count, sample_rate);
                            sample_count += 1;
                            (s.clamp(-1.0, 1.0) * 32_767.0) as i16
                        } else {
                            0
                        };
                        let base = f * channels.max(1);
                        for c in 0..channels.max(1) {
                            data[base + c] = v;
                        }
                    }
                },
                err_cb,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut sample_count = 0usize;
            output_dev.build_output_stream(
                &cfg,
                move |data: &mut [u16], _| {
                    let frames = data.len() / channels.max(1);
                    for f in 0..frames {
                        let v = if sample_count < total_samples {
                            let s = next_sine(sample_count, sample_rate);
                            sample_count += 1;
                            ((s.clamp(-1.0, 1.0) * 32_767.0) as i32 + 32_768) as u16
                        } else {
                            32_768
                        };
                        let base = f * channels.max(1);
                        for c in 0..channels.max(1) {
                            data[base + c] = v;
                        }
                    }
                },
                err_cb,
                None,
            )
        }
        other => {
            warn!("Speaker test: unsupported sample format {other:?}");
            return;
        }
    };
    let stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            warn!("Speaker test: could not build output stream: {e}");
            return;
        }
    };
    if let Err(e) = stream.play() {
        warn!("Speaker test: could not start stream: {e}");
        return;
    }
    std::thread::sleep(std::time::Duration::from_millis(1400));
    // stream dropped here, CPAL stops playback
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Drain the event channel until a `StateChanged` event arrives.
    /// Skips advisory events (e.g. `CaptureError` when no audio device is available in tests).
    async fn next_state(rx: &mut mpsc::Receiver<CallEvent>) -> CallState {
        loop {
            match rx.recv().await.expect("event channel closed") {
                CallEvent::StateChanged(s) => return s,
                _ => {} // skip advisory events
            }
        }
    }

    #[tokio::test]
    async fn state_transitions() {
        let (cmd_tx, mut event_rx, fut) = CallController::split(None);
        let handle = tokio::spawn(fut);

        // Start audio
        cmd_tx
            .send(CallCommand::StartAudio {
                voice_activation: false,
            })
            .await
            .unwrap();
        assert_eq!(next_state(&mut event_rx).await, CallState::Connecting);

        // Stop audio
        cmd_tx.send(CallCommand::StopAudio).await.unwrap();
        assert_eq!(next_state(&mut event_rx).await, CallState::Disconnecting);
        assert_eq!(next_state(&mut event_rx).await, CallState::Idle);

        cmd_tx.send(CallCommand::Shutdown).await.unwrap();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn ptt_starts_muted() {
        let (cmd_tx, mut event_rx, fut) = CallController::split(None);
        let handle = tokio::spawn(fut);

        cmd_tx
            .send(CallCommand::StartAudio {
                voice_activation: false,
            })
            .await
            .unwrap();
        // Drain until Connecting (skips CaptureError if no audio device)
        assert_eq!(next_state(&mut event_rx).await, CallState::Connecting);

        cmd_tx.send(CallCommand::Shutdown).await.unwrap();
        handle.await.unwrap();
    }
}
