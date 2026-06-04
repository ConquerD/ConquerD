//! Safe Opus decoder wrapper.

use crate::ffi;
use crate::OpusError;

/// Safe, owned wrapper around a libopus decoder state.
///
/// `Send` but not `Sync` — same threading contract as [`OpusEncoder`](crate::OpusEncoder).
pub struct OpusDecoder {
    ptr: *mut ffi::OpusDecoder,
}

// SAFETY: Exclusive ownership of the pointer; libopus state is not thread-local.
unsafe impl Send for OpusDecoder {}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        // SAFETY: pointer created by opus_decoder_create, freed at most once.
        unsafe { ffi::opus_decoder_destroy(self.ptr) };
    }
}

impl OpusDecoder {
    /// Create an Opus decoder.
    ///
    /// * `sample_rate` — 8000, 12000, 16000, 24000, or 48000 Hz.
    /// * `channels` — 1 (mono) or 2 (stereo).
    ///
    /// When the `dnn` feature is enabled, the embedded DNN weights are loaded
    /// immediately, activating OSCE neural speech enhancement for received
    /// SILK-layer voice packets.
    pub fn new(sample_rate: u32, channels: u8) -> Result<Self, OpusError> {
        let mut error = 0i32;
        let ptr =
            unsafe { ffi::opus_decoder_create(sample_rate as i32, channels as i32, &mut error) };
        if ptr.is_null() {
            return Err(OpusError(if error != 0 {
                error
            } else {
                ffi::OPUS_ALLOC_FAIL
            }));
        }
        let dec = Self { ptr };
        unsafe { crate::dnn::load_into_decoder(dec.ptr)? };
        Ok(dec)
    }

    /// Reset the decoder state.  Use when switching to a new incoming stream.
    pub fn reset_state(&mut self) -> Result<(), OpusError> {
        let r = unsafe { ffi::conquerd_dec_reset_state(self.ptr) };
        if r == ffi::OPUS_OK {
            Ok(())
        } else {
            Err(OpusError(r))
        }
    }

    /// Decode one Opus packet to 16-bit PCM.
    ///
    /// Pass `input = None` to perform **packet loss concealment** (PLC) for a
    /// missing frame; `output` must be sized for exactly the duration of the
    /// missing frame.
    ///
    /// With `input = Some(data)` and `decode_fec = true`, the decoder
    /// reconstructs audio for the *previous* (lost) packet using the FEC data
    /// embedded in `data`.  `output` must be sized for the duration of that
    /// prior frame.
    ///
    /// A safe output buffer size for 48 kHz mono with 20 ms frames is
    /// `960` samples (`1920` bytes).  The absolute maximum is
    /// `5760 * channels` samples.
    ///
    /// Returns the number of **decoded samples per channel** on success.
    pub fn decode(
        &mut self,
        input: Option<&[u8]>,
        output: &mut [i16],
        decode_fec: bool,
    ) -> Result<usize, OpusError> {
        let (data_ptr, data_len) = match input {
            Some(d) => (d.as_ptr(), d.len() as i32),
            None => (std::ptr::null(), 0),
        };
        let n = unsafe {
            ffi::opus_decode(
                self.ptr,
                data_ptr,
                data_len,
                output.as_mut_ptr(),
                output.len() as i32, // frame_size
                decode_fec as i32,
            )
        };
        if n < 0 {
            Err(OpusError(n))
        } else {
            Ok(n as usize)
        }
    }
}
