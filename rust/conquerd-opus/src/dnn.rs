//! DNN model weight integration for DRED and OSCE neural features.
//!
//! The model weights ship as C source arrays in the `opus_data-<sha256>.tar.gz`
//! tarball distributed by the Xiph.Org Foundation.  When the `dnn` feature is
//! enabled, the fetch script extracts those C files into `opus/dnn/` and cmake
//! compiles them directly into the libopus static library.
//!
//! Because the weights are compiled into the library itself:
//! - `OPUS_SET_DNN_BLOB_REQUEST` returns `OPUS_UNIMPLEMENTED` (the compiled-in
//!   weights are used instead), which we treat as a successful no-op.
//! - DRED and OSCE activate automatically once `OPUS_SET_DRED_DURATION_REQUEST`
//!   is called on the encoder.
//! - No runtime I/O or separate binary blob is needed.

use crate::ffi;
use crate::OpusError;

/// No-op: weights are compiled into libopus from the C data source arrays.
///
/// `OPUS_SET_DNN_BLOB_REQUEST` (code 4052) will return `OPUS_UNIMPLEMENTED`
/// when the library has built-in weights, which is silently ignored here.
/// DRED activates as soon as `set_dred_duration_ms` is called on the encoder.
pub(crate) unsafe fn load_into_encoder(_enc: *mut ffi::OpusEncoder) -> Result<(), OpusError> {
    Ok(())
}

/// No-op: weights are compiled into libopus from the C data source arrays.
///
/// OSCE activates automatically for SILK-layer voice packets once the model
/// C arrays are compiled in.
pub(crate) unsafe fn load_into_decoder(_dec: *mut ffi::OpusDecoder) -> Result<(), OpusError> {
    Ok(())
}
