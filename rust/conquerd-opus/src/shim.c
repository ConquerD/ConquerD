/* conquerd_opus_shim.c
 *
 * Wraps all variadic opus_encoder_ctl() / opus_decoder_ctl() calls with
 * fixed-signature C functions so the Rust layer doesn't need to deal with
 * variadic FFI, which has subtle ABI differences across platforms.
 *
 * This file is compiled by the `cc` build crate and linked alongside
 * the libopus static library.
 */

#include "opus.h"

/* ── Encoder CTLs ─────────────────────────────────────────────────────────── */

int conquerd_enc_set_bitrate(OpusEncoder *enc, opus_int32 value) {
    return opus_encoder_ctl(enc, OPUS_SET_BITRATE_REQUEST, value);
}

int conquerd_enc_set_vbr(OpusEncoder *enc, opus_int32 value) {
    return opus_encoder_ctl(enc, OPUS_SET_VBR_REQUEST, value);
}

int conquerd_enc_set_inband_fec(OpusEncoder *enc, opus_int32 value) {
    return opus_encoder_ctl(enc, OPUS_SET_INBAND_FEC_REQUEST, value);
}

int conquerd_enc_set_packet_loss_perc(OpusEncoder *enc, opus_int32 value) {
    return opus_encoder_ctl(enc, OPUS_SET_PACKET_LOSS_PERC_REQUEST, value);
}

int conquerd_enc_set_dtx(OpusEncoder *enc, opus_int32 value) {
    return opus_encoder_ctl(enc, OPUS_SET_DTX_REQUEST, value);
}

int conquerd_enc_set_complexity(OpusEncoder *enc, opus_int32 value) {
    return opus_encoder_ctl(enc, OPUS_SET_COMPLEXITY_REQUEST, value);
}

/* frames_10ms: number of 10 ms redundancy frames (e.g. 10 → 100 ms depth).
 * Set to 0 to disable DRED.  Requires DNN weights to be loaded first. */
int conquerd_enc_set_dred_duration(OpusEncoder *enc, opus_int32 frames_10ms) {
    return opus_encoder_ctl(enc, OPUS_SET_DRED_DURATION_REQUEST, frames_10ms);
}

/* Load external DNN model weights.  data points to the weights blob;
 * len is its byte length.  The pointer must remain valid for the lifetime
 * of the encoder (we extract C arrays from the Xiph tarball into opus/dnn/,
 * lives for the lifetime of the binary). */
int conquerd_enc_set_dnn_blob(OpusEncoder *enc, const void *data, opus_int32 len) {
    return opus_encoder_ctl(enc, OPUS_SET_DNN_BLOB_REQUEST, data, len);
}

int conquerd_enc_reset_state(OpusEncoder *enc) {
    return opus_encoder_ctl(enc, OPUS_RESET_STATE);
}

/* ── Decoder CTLs ─────────────────────────────────────────────────────────── */

/* Q8 dB gain applied to decoded output.  0 = no adjustment (default). */
int conquerd_dec_set_gain(OpusDecoder *dec, opus_int32 value) {
    return opus_decoder_ctl(dec, OPUS_SET_GAIN_REQUEST, value);
}

int conquerd_dec_set_dnn_blob(OpusDecoder *dec, const void *data, opus_int32 len) {
    return opus_decoder_ctl(dec, OPUS_SET_DNN_BLOB_REQUEST, data, len);
}

/* Enable (1) or disable (0) OSCE blind bandwidth extension for
 * wideband signals when decoding to 48 kHz.  Default: 0. */
int conquerd_dec_set_osce_bwe(OpusDecoder *dec, opus_int32 value) {
    return opus_decoder_ctl(dec, OPUS_SET_OSCE_BWE_REQUEST, value);
}

int conquerd_dec_reset_state(OpusDecoder *dec) {
    return opus_decoder_ctl(dec, OPUS_RESET_STATE);
}
