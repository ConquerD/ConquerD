//! Safe Opus encoder wrapper.

use crate::ffi;
use crate::Application;
use crate::OpusError;

/// Safe, owned wrapper around a libopus encoder state.
///
/// The encoder is `Send` (can be moved to another thread) but not `Sync`
/// (must not be used concurrently from multiple threads without external
/// synchronization).  In practice the audio capture callback holds it
/// exclusively as `&mut OpusEncoder`.
pub struct OpusEncoder {
    ptr: *mut ffi::OpusEncoder,
}

// SAFETY: The encoder pointer is exclusively owned; Send is sound because
// libopus encoder state is not thread-local.
unsafe impl Send for OpusEncoder {}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: `ptr` was created by `opus_encoder_create` and has not been
        // freed before (Drop is called at most once).
        unsafe { ffi::opus_encoder_destroy(self.ptr) };
    }
}

impl OpusEncoder {
    /// Create an Opus encoder.
    ///
    /// * `sample_rate` — 8000, 12000, 16000, 24000, or 48000 Hz.
    /// * `channels` — 1 (mono) or 2 (stereo).
    /// * `application` — coding mode; use [`Application::Voip`] for VoIP.
    ///
    /// When the `dnn` feature is enabled, the embedded DNN weights are loaded
    /// immediately so DRED can be activated later with
    /// [`set_dred_duration_ms`](Self::set_dred_duration_ms).
    pub fn new(
        sample_rate: u32,
        channels: u8,
        application: Application,
    ) -> Result<Self, OpusError> {
        let mut error = 0i32;
        let ptr = unsafe {
            ffi::opus_encoder_create(
                sample_rate as i32,
                channels as i32,
                application.as_ffi(),
                &mut error,
            )
        };
        if ptr.is_null() {
            return Err(OpusError(if error != 0 {
                error
            } else {
                ffi::OPUS_ALLOC_FAIL
            }));
        }
        let enc = Self { ptr };
        // Load DNN weights (no-op when feature is disabled or when the library
        // was already built with compiled-in weights).
        unsafe { crate::dnn::load_into_encoder(enc.ptr)? };
        Ok(enc)
    }

    // ── Encoder configuration ─────────────────────────────────────────────

    /// Set the target bitrate in bits per second.
    ///
    /// Range: 500–512 000 bps, or -1 (`OPUS_BITRATE_MAX`) to use as much as
    /// the output buffer allows.  Default is determined by channel count and
    /// sample rate.
    pub fn set_bitrate(&mut self, bps: i32) -> Result<(), OpusError> {
        let r = unsafe { ffi::conquerd_enc_set_bitrate(self.ptr, bps) };
        opus_ok(r)
    }

    /// Enable (`true`) or disable (`false`) variable bitrate.  Default: enabled.
    pub fn set_vbr(&mut self, enable: bool) -> Result<(), OpusError> {
        opus_ok(unsafe { ffi::conquerd_enc_set_vbr(self.ptr, enable as i32) })
    }

    /// Enable in-band forward error correction (FEC) for the SILK layer.
    ///
    /// When enabled and packet loss percentage is non-zero, the encoder
    /// embeds redundant data allowing the decoder to partially reconstruct
    /// lost packets.
    pub fn set_inband_fec(&mut self, enable: bool) -> Result<(), OpusError> {
        opus_ok(unsafe { ffi::conquerd_enc_set_inband_fec(self.ptr, enable as i32) })
    }

    /// Set the expected packet loss percentage (0–100).
    ///
    /// Higher values trade quality for loss robustness.  Works in combination
    /// with [`set_inband_fec`](Self::set_inband_fec).
    pub fn set_packet_loss_perc(&mut self, pct: u8) -> Result<(), OpusError> {
        opus_ok(unsafe { ffi::conquerd_enc_set_packet_loss_perc(self.ptr, pct as i32) })
    }

    /// Enable discontinuous transmission (DTX).
    ///
    /// When enabled, the encoder produces tiny comfort-noise packets during
    /// silence rather than full-size frames, greatly reducing bandwidth during
    /// quiet periods.
    pub fn set_dtx(&mut self, enable: bool) -> Result<(), OpusError> {
        opus_ok(unsafe { ffi::conquerd_enc_set_dtx(self.ptr, enable as i32) })
    }

    /// Set the computational complexity (0 = lowest, 10 = highest).
    ///
    /// Higher complexity produces better quality at a given bitrate.
    /// Default is 9.
    pub fn set_complexity(&mut self, complexity: i32) -> Result<(), OpusError> {
        opus_ok(unsafe { ffi::conquerd_enc_set_complexity(self.ptr, complexity) })
    }

    /// Enable Deep Redundancy Encoding (DRED) with the given maximum depth.
    ///
    /// `duration_ms` is the maximum amount of redundant historical audio
    /// appended to each packet, in milliseconds.  Must be a multiple of 10.
    /// Set to 0 to disable DRED.
    ///
    /// The DNN weights must have been loaded first (happens automatically in
    /// [`new`](Self::new) when the `dnn` feature is enabled).
    ///
    /// **Bandwidth overhead**: approximately 2 kbps per 10 ms of depth.
    /// 100 ms depth ≈ +20 kbps overhead; recommended only at ≥ 48 kbps.
    pub fn set_dred_duration_ms(&mut self, duration_ms: u32) -> Result<(), OpusError> {
        // The CTL unit is 10 ms frames, not milliseconds.
        let frames_10ms = (duration_ms / 10) as i32;
        opus_ok(unsafe { ffi::conquerd_enc_set_dred_duration(self.ptr, frames_10ms) })
    }

    /// Reset the encoder state (clears codec memory / algorithm state).
    ///
    /// Use when switching to a new audio stream on the same encoder instance.
    pub fn reset_state(&mut self) -> Result<(), OpusError> {
        opus_ok(unsafe { ffi::conquerd_enc_reset_state(self.ptr) })
    }

    // ── Encoding ──────────────────────────────────────────────────────────

    /// Encode one Opus frame of 16-bit PCM audio.
    ///
    /// `pcm` must contain exactly one frame of audio — 120, 240, 480, 960,
    /// 1920, or 2880 samples per channel at 48 kHz.  For mono (channels = 1)
    /// the slice length equals the frame size.
    ///
    /// `output` must be at least 4000 bytes.  Actual packet size is usually
    /// much smaller.
    ///
    /// Returns the number of bytes written on success.  A 1–2 byte result
    /// is a DTX silence frame; still transmit it so the remote FEC decoder
    /// remains coherent.
    pub fn encode(&mut self, pcm: &[i16], output: &mut [u8]) -> Result<usize, OpusError> {
        let n = unsafe {
            ffi::opus_encode(
                self.ptr,
                pcm.as_ptr(),
                pcm.len() as i32, // frame_size == slice len for mono
                output.as_mut_ptr(),
                output.len() as i32,
            )
        };
        if n < 0 {
            Err(OpusError(n))
        } else {
            Ok(n as usize)
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

#[inline]
fn opus_ok(ret: std::os::raw::c_int) -> Result<(), OpusError> {
    if ret == ffi::OPUS_OK {
        Ok(())
    } else {
        Err(OpusError(ret))
    }
}
