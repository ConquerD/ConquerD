//! `conquerd-opus`: first-party libopus wrapper for ConquerD.
//!
//! Builds libopus from the official [xiph/opus](https://github.com/xiph/opus)
//! git submodule, exposes a safe Rust API, and supports the DNN model weights
//! for **DRED** (Deep Redundancy Encoding) and **OSCE** (Opus Speech Coding
//! Enhancement).  The weights ship as C source arrays in a separately
//! distributed tarball; when extracted into `opus/dnn/`, cmake compiles them
//! directly into libopus — no runtime I/O or binary blob embedding needed.
//!
//! # Features
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `dnn`   | ✓       | Require the DNN model C source arrays to be present in `opus/dnn/` at build time.  Run `scripts/fetch_opus_weights.{ps1,sh}` to extract them from the Xiph tarball before building. |
//!
//! To build without neural features (e.g. for offline CI environments):
//! ```toml
//! conquerd-opus = { path = "../conquerd-opus", default-features = false }
//! ```
//!
//! # Submodule
//!
//! The libopus source must be available at `rust/conquerd-opus/opus/`:
//! ```text
//! git submodule update --init rust/conquerd-opus/opus
//! ```
//!
//! # Quick start
//!
//! ```no_run
//! use conquerd_opus::{Application, OpusDecoder, OpusEncoder};
//!
//! // Encoder
//! let mut enc = OpusEncoder::new(48_000, 1, Application::Voip).unwrap();
//! enc.set_bitrate(48_000).unwrap();
//! enc.set_inband_fec(true).unwrap();
//! enc.set_packet_loss_perc(10).unwrap();
//! enc.set_dtx(true).unwrap();
//! enc.set_dred_duration_ms(100).unwrap();  // 100 ms DRED depth (10 frames × 10 ms)
//!
//! let mut opus_buf = vec![0u8; 4000];
//! let pcm_frame = vec![0i16; 960]; // 20 ms @ 48 kHz mono
//! let len = enc.encode(&pcm_frame, &mut opus_buf).unwrap();
//!
//! // Decoder
//! let mut dec = OpusDecoder::new(48_000, 1).unwrap();
//! let mut pcm_out = vec![0i16; 960];
//! let _samples = dec.decode(Some(&opus_buf[..len]), &mut pcm_out, false).unwrap();
//! ```

mod dnn;
pub mod ffi;

mod decoder;
mod encoder;

pub use decoder::OpusDecoder;
pub use encoder::OpusEncoder;

// ── Public types ─────────────────────────────────────────────────────────────

/// Opus coding application / mode.
///
/// The choice affects which coding strategies the encoder favors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Application {
    /// Optimised for speech intelligibility in VoIP applications.
    ///
    /// Applies voice-activity detection, SILK layer tuning, and in-band FEC.
    /// Recommended for ConquerD voice calls.
    Voip,

    /// Optimised for musical or broadcast audio where input fidelity matters
    /// more than speech clarity.
    Audio,

    /// Minimum algorithmic delay; disables the SILK layer and lookahead.
    ///
    /// Latency drops to ~2.5 ms but speech quality at low bitrates is reduced.
    LowDelay,
}

impl Application {
    pub(crate) fn as_ffi(self) -> std::os::raw::c_int {
        match self {
            Self::Voip => ffi::OPUS_APPLICATION_VOIP,
            Self::Audio => ffi::OPUS_APPLICATION_AUDIO,
            Self::LowDelay => ffi::OPUS_APPLICATION_RESTRICTED_LOWDELAY,
        }
    }
}

/// Opus error code.
///
/// Wraps the raw negative integer returned by libopus.  Use `.to_string()`
/// for a human-readable description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusError(pub i32);

impl std::fmt::Display for OpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: opus_strerror always returns a non-null, static C string.
        let msg = unsafe {
            let ptr = ffi::opus_strerror(self.0);
            if ptr.is_null() {
                return write!(f, "opus error {}", self.0);
            }
            std::ffi::CStr::from_ptr(ptr as *const std::os::raw::c_char)
                .to_str()
                .unwrap_or("unknown")
        };
        write!(f, "opus error {}: {}", self.0, msg)
    }
}

impl std::error::Error for OpusError {}
