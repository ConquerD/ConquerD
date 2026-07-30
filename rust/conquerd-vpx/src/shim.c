/*
 * Narrow C surface over libvpx's VP8 encoder and decoder.
 *
 * Rust talks to this file, not to libvpx directly. libvpx's public API is
 * built on large structs -- vpx_codec_ctx_t, vpx_codec_enc_cfg_t (fifty-odd
 * fields), vpx_image_t -- whose layouts depend on the same vpx_config.h the
 * build generates. Redeclaring those in Rust means hand-maintaining a byte
 * exact mirror of a struct that changes with configuration, and getting it
 * subtly wrong yields memory corruption rather than a compile error.
 *
 * So the FFI boundary is drawn here instead: opaque pointers and plain
 * integers, with signatures this project owns. The Rust side cannot get a
 * layout wrong because it never names a libvpx type.
 */

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include "vpx/vpx_codec.h"
#include "vpx/vpx_decoder.h"
#include "vpx/vpx_encoder.h"
#include "vpx/vp8cx.h"
#include "vpx/vp8dx.h"

struct cq_vp8_enc {
  vpx_codec_ctx_t ctx;
  vpx_image_t img;
  int width;
  int height;
  /* Frame counter, in the timebase given at construction. libvpx wants a
   * monotonically increasing pts; the caller does not supply one. */
  int64_t pts;
  /* Set by cq_vp8_enc_request_keyframe, consumed by the next encode. */
  int force_keyframe;
};

struct cq_vp8_dec {
  vpx_codec_ctx_t ctx;
};

/* ---- Encoder ---------------------------------------------------------- */

struct cq_vp8_enc *cq_vp8_enc_new(int width, int height, int bitrate_bps,
                                  int fps, int keyframe_interval_secs) {
  if (width <= 0 || height <= 0 || fps <= 0) return NULL;

  struct cq_vp8_enc *e = (struct cq_vp8_enc *)calloc(1, sizeof(*e));
  if (!e) return NULL;

  vpx_codec_enc_cfg_t cfg;
  if (vpx_codec_enc_config_default(vpx_codec_vp8_cx(), &cfg, 0)) {
    free(e);
    return NULL;
  }

  cfg.g_w = (unsigned int)width;
  cfg.g_h = (unsigned int)height;
  cfg.g_timebase.num = 1;
  cfg.g_timebase.den = fps;
  cfg.rc_target_bitrate = (unsigned int)(bitrate_bps / 1000);
  cfg.rc_end_usage = VPX_CBR;
  /* Real-time: no lookahead, and a small buffer so the rate controller reacts
   * within a call's latency budget rather than averaging over seconds. */
  cfg.g_lag_in_frames = 0;
  cfg.g_pass = VPX_RC_ONE_PASS;
  cfg.rc_buf_sz = 1000;
  cfg.rc_buf_initial_sz = 500;
  cfg.rc_buf_optimal_sz = 600;
  /* Let the encoder drop frames rather than blow the bitrate; a dropped frame
   * costs less than a late one on a live link. */
  cfg.rc_dropframe_thresh = 30;
  cfg.kf_mode = VPX_KF_AUTO;
  cfg.kf_max_dist = (unsigned int)(keyframe_interval_secs > 0
                                       ? keyframe_interval_secs * fps
                                       : 4 * fps);
  cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
  cfg.g_threads = 1;

  if (vpx_codec_enc_init(&e->ctx, vpx_codec_vp8_cx(), &cfg, 0)) {
    free(e);
    return NULL;
  }

  /* cpu_used trades quality for speed; 8 is well into the realtime end, which
   * is the right side of that trade for a pure-C build with no SIMD. */
  vpx_codec_control(&e->ctx, VP8E_SET_CPUUSED, 8);

  if (!vpx_img_alloc(&e->img, VPX_IMG_FMT_I420, (unsigned int)width,
                     (unsigned int)height, 1)) {
    vpx_codec_destroy(&e->ctx);
    free(e);
    return NULL;
  }

  e->width = width;
  e->height = height;
  e->pts = 0;
  e->force_keyframe = 0;
  return e;
}

void cq_vp8_enc_free(struct cq_vp8_enc *e) {
  if (!e) return;
  vpx_img_free(&e->img);
  vpx_codec_destroy(&e->ctx);
  free(e);
}

void cq_vp8_enc_request_keyframe(struct cq_vp8_enc *e) {
  if (e) e->force_keyframe = 1;
}

int cq_vp8_enc_set_bitrate(struct cq_vp8_enc *e, int bitrate_bps) {
  if (!e || bitrate_bps <= 0) return -1;
  /* Read-modify-write the live config: vpx_codec_enc_config_set on a running
   * encoder retargets the rate controller without resetting reference frames,
   * which is what keeps a rate change from costing a keyframe. */
  vpx_codec_enc_cfg_t cfg;
  if (vpx_codec_enc_config_default(vpx_codec_vp8_cx(), &cfg, 0)) return -1;
  cfg.g_w = (unsigned int)e->width;
  cfg.g_h = (unsigned int)e->height;
  cfg.rc_target_bitrate = (unsigned int)(bitrate_bps / 1000);
  cfg.rc_end_usage = VPX_CBR;
  cfg.g_lag_in_frames = 0;
  cfg.g_pass = VPX_RC_ONE_PASS;
  cfg.rc_buf_sz = 1000;
  cfg.rc_buf_initial_sz = 500;
  cfg.rc_buf_optimal_sz = 600;
  cfg.rc_dropframe_thresh = 30;
  cfg.g_error_resilient = VPX_ERROR_RESILIENT_DEFAULT;
  cfg.g_threads = 1;
  return vpx_codec_enc_config_set(&e->ctx, &cfg) ? -1 : 0;
}

/*
 * Encode one I420 frame.
 *
 * Plane pointers are tightly packed (stride == width for Y, width/2 for U/V),
 * matching the caller's RawFrame. Output is written into `out` and the byte
 * count returned; `*is_keyframe` reports whether the encoder produced one.
 *
 * Returns the encoded length, 0 when the encoder produced nothing for this
 * frame (a legitimate outcome when it drops under rate control), or -1 on
 * error. A return of -2 means the output buffer was too small.
 */
int cq_vp8_enc_encode(struct cq_vp8_enc *e, const uint8_t *y, const uint8_t *u,
                      const uint8_t *v, uint8_t *out, int out_cap,
                      int *is_keyframe) {
  if (!e || !y || !u || !v || !out || !is_keyframe) return -1;
  *is_keyframe = 0;

  const int w = e->width;
  const int h = e->height;
  const int cw = (w + 1) / 2;
  const int ch = (h + 1) / 2;

  /* Copy row by row: the caller's planes are tightly packed, libvpx's are
   * stride-aligned, and the two only coincide by accident. */
  for (int r = 0; r < h; ++r) {
    memcpy(e->img.planes[VPX_PLANE_Y] + (size_t)r * e->img.stride[VPX_PLANE_Y],
           y + (size_t)r * w, (size_t)w);
  }
  for (int r = 0; r < ch; ++r) {
    memcpy(e->img.planes[VPX_PLANE_U] + (size_t)r * e->img.stride[VPX_PLANE_U],
           u + (size_t)r * cw, (size_t)cw);
    memcpy(e->img.planes[VPX_PLANE_V] + (size_t)r * e->img.stride[VPX_PLANE_V],
           v + (size_t)r * cw, (size_t)cw);
  }

  const vpx_enc_frame_flags_t flags = e->force_keyframe ? VPX_EFLAG_FORCE_KF : 0;
  e->force_keyframe = 0;

  if (vpx_codec_encode(&e->ctx, &e->img, e->pts, 1, flags, VPX_DL_REALTIME)) {
    return -1;
  }
  e->pts++;

  int written = 0;
  vpx_codec_iter_t iter = NULL;
  const vpx_codec_cx_pkt_t *pkt;
  while ((pkt = vpx_codec_get_cx_data(&e->ctx, &iter)) != NULL) {
    if (pkt->kind != VPX_CODEC_CX_FRAME_PKT) continue;
    if (written + (int)pkt->data.frame.sz > out_cap) return -2;
    memcpy(out + written, pkt->data.frame.buf, pkt->data.frame.sz);
    written += (int)pkt->data.frame.sz;
    if (pkt->data.frame.flags & VPX_FRAME_IS_KEY) *is_keyframe = 1;
  }
  return written;
}

/* ---- Decoder ---------------------------------------------------------- */

struct cq_vp8_dec *cq_vp8_dec_new(void) {
  struct cq_vp8_dec *d = (struct cq_vp8_dec *)calloc(1, sizeof(*d));
  if (!d) return NULL;
  if (vpx_codec_dec_init(&d->ctx, vpx_codec_vp8_dx(), NULL, 0)) {
    free(d);
    return NULL;
  }
  return d;
}

void cq_vp8_dec_free(struct cq_vp8_dec *d) {
  if (!d) return;
  vpx_codec_destroy(&d->ctx);
  free(d);
}

/*
 * Decode one frame into tightly-packed I420.
 *
 * `*width`/`*height` receive the decoded dimensions, which the caller cannot
 * know in advance -- a VP8 stream carries its own size and may change it at a
 * keyframe. Returns bytes written, -1 on decode error, or -2 when `out_cap` is
 * too small for the frame (the caller can re-ask with the reported size).
 */
int cq_vp8_dec_decode(struct cq_vp8_dec *d, const uint8_t *data, int len,
                      uint8_t *out, int out_cap, int *width, int *height) {
  if (!d || !data || len <= 0 || !width || !height) return -1;

  if (vpx_codec_decode(&d->ctx, data, (unsigned int)len, NULL, 0)) return -1;

  vpx_codec_iter_t iter = NULL;
  vpx_image_t *img = vpx_codec_get_frame(&d->ctx, &iter);
  if (!img) return -1;

  const int w = (int)img->d_w;
  const int h = (int)img->d_h;
  const int cw = (w + 1) / 2;
  const int ch = (h + 1) / 2;
  *width = w;
  *height = h;

  const int needed = w * h + 2 * cw * ch;
  if (!out || needed > out_cap) return -2;

  int off = 0;
  for (int r = 0; r < h; ++r) {
    memcpy(out + off, img->planes[VPX_PLANE_Y] + (size_t)r * img->stride[VPX_PLANE_Y],
           (size_t)w);
    off += w;
  }
  for (int r = 0; r < ch; ++r) {
    memcpy(out + off, img->planes[VPX_PLANE_U] + (size_t)r * img->stride[VPX_PLANE_U],
           (size_t)cw);
    off += cw;
  }
  for (int r = 0; r < ch; ++r) {
    memcpy(out + off, img->planes[VPX_PLANE_V] + (size_t)r * img->stride[VPX_PLANE_V],
           (size_t)cw);
    off += cw;
  }
  return off;
}
