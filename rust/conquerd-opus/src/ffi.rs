//! Raw FFI declarations for libopus and the conquerd_opus_shim.
//!
//! The opus_encoder_ctl / opus_decoder_ctl functions are variadic in C.
//! Rather than using Rust variadic FFI (which has subtle platform ABI
//! differences), all CTL calls go through named shim functions compiled
//! from `shim.c` with fixed signatures.

#![allow(non_camel_case_types)]

use std::os::raw::{c_int, c_void};

// ── Opaque codec state types ────────────────────────────────────────────────

/// Opaque encoder state allocated by `opus_encoder_create`.
#[repr(C)]
pub struct OpusEncoder {
    _priv: [u8; 0],
}

/// Opaque decoder state allocated by `opus_decoder_create`.
#[repr(C)]
pub struct OpusDecoder {
    _priv: [u8; 0],
}

// ── Application constants (opus_defines.h) ──────────────────────────────────

pub const OPUS_APPLICATION_VOIP: c_int = 2048;
pub const OPUS_APPLICATION_AUDIO: c_int = 2049;
pub const OPUS_APPLICATION_RESTRICTED_LOWDELAY: c_int = 2051;

// ── Error codes (opus_defines.h) ─────────────────────────────────────────────

pub const OPUS_OK: c_int = 0;
pub const OPUS_BAD_ARG: c_int = -1;
pub const OPUS_BUFFER_TOO_SMALL: c_int = -2;
pub const OPUS_INTERNAL_ERROR: c_int = -3;
pub const OPUS_INVALID_PACKET: c_int = -4;
/// Returned when a CTL is not supported by this build (e.g. DNN blob CTL on
/// a library built with weights already compiled in).
pub const OPUS_UNIMPLEMENTED: c_int = -5;
pub const OPUS_INVALID_STATE: c_int = -6;
pub const OPUS_ALLOC_FAIL: c_int = -7;

// ── Core encoder / decoder functions ────────────────────────────────────────

extern "C" {
    /// Allocate and initialize an encoder.
    /// `error` receives `OPUS_OK` on success or an error code.
    /// Returns null on allocation failure (check `error`).
    pub fn opus_encoder_create(
        fs: i32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusEncoder;

    /// Free an encoder created with `opus_encoder_create`.
    pub fn opus_encoder_destroy(st: *mut OpusEncoder);

    /// Encode one frame of 16-bit PCM audio.
    ///
    /// `frame_size` is the number of samples per channel in `pcm`.
    /// Returns the number of bytes written to `data` (≥ 0) or a negative
    /// error code.  A result of 1–2 bytes indicates a DTX comfort-noise
    /// frame; transmit it to keep FEC state coherent.
    pub fn opus_encode(
        st: *mut OpusEncoder,
        pcm: *const i16,
        frame_size: c_int,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;

    /// Allocate and initialize a decoder.
    pub fn opus_decoder_create(fs: i32, channels: c_int, error: *mut c_int) -> *mut OpusDecoder;

    /// Free a decoder created with `opus_decoder_create`.
    pub fn opus_decoder_destroy(st: *mut OpusDecoder);

    /// Decode one Opus packet to 16-bit PCM.
    ///
    /// Pass `data = null` with `len = 0` to trigger packet loss concealment
    /// (PLC).  `frame_size` must equal the number of samples per channel in
    /// `pcm`; for PLC it must match the duration of the missing frame.
    /// Returns the number of decoded samples per channel or a negative error code.
    pub fn opus_decode(
        st: *mut OpusDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut i16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;

    /// Convert an Opus error code to a human-readable C string.
    pub fn opus_strerror(error: c_int) -> *const u8;
}

// ── Shim functions (fixed-signature CTL wrappers from shim.c) ───────────────

extern "C" {
    // ── Encoder shims ──

    pub fn conquerd_enc_set_bitrate(enc: *mut OpusEncoder, value: i32) -> c_int;
    pub fn conquerd_enc_set_vbr(enc: *mut OpusEncoder, value: i32) -> c_int;
    pub fn conquerd_enc_set_inband_fec(enc: *mut OpusEncoder, value: i32) -> c_int;
    pub fn conquerd_enc_set_packet_loss_perc(enc: *mut OpusEncoder, value: i32) -> c_int;
    pub fn conquerd_enc_set_dtx(enc: *mut OpusEncoder, value: i32) -> c_int;
    pub fn conquerd_enc_set_complexity(enc: *mut OpusEncoder, value: i32) -> c_int;
    /// Enable DRED with the given depth in 10 ms frames (0 = disable).
    pub fn conquerd_enc_set_dred_duration(enc: *mut OpusEncoder, frames_10ms: i32) -> c_int;
    /// Load external DNN weights blob.  Returns `OPUS_UNIMPLEMENTED` when
    /// libopus was compiled with the weights already built in as C arrays
    /// (which is the normal conquerd-opus build path).  In that case the
    /// call is a no-op and the compiled-in weights remain in effect.
    /// `data` must remain valid for the lifetime of the encoder when used.
    pub fn conquerd_enc_set_dnn_blob(enc: *mut OpusEncoder, data: *const c_void, len: i32)
        -> c_int;
    pub fn conquerd_enc_reset_state(enc: *mut OpusEncoder) -> c_int;

    // ── Decoder shims ──

    /// Apply Q8 dB gain to decoded output (0 = no adjustment).
    pub fn conquerd_dec_set_gain(dec: *mut OpusDecoder, value: i32) -> c_int;
    pub fn conquerd_dec_set_dnn_blob(dec: *mut OpusDecoder, data: *const c_void, len: i32)
        -> c_int;
    /// Enable (1) or disable (0) OSCE blind bandwidth extension.
    pub fn conquerd_dec_set_osce_bwe(dec: *mut OpusDecoder, value: i32) -> c_int;
    pub fn conquerd_dec_reset_state(dec: *mut OpusDecoder) -> c_int;
}
