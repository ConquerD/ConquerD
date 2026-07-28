//! I420 <-> NV12 conversion.
//!
//! Hardware video encoders almost universally want **NV12**, while the rest of
//! this pipeline speaks **I420** (what cameras hand us, what Qt's
//! `Format_YUV420P` maps to). Both are planar YUV 4:2:0 and differ only in how
//! chroma is laid out:
//!
//! ```text
//! I420:  [Y w*h][U (w/2)*(h/2)][V (w/2)*(h/2)]      three planes
//! NV12:  [Y w*h][UV interleaved (w/2)*(h/2)*2]      two planes, U,V,U,V…
//! ```
//!
//! So conversion is an interleave, not a colour-space change — no arithmetic,
//! no loss, and cheap enough to sit in the per-frame path.
//!
//! # Strides
//!
//! Media Foundation hands back buffers whose rows are padded to a stride that
//! is usually larger than the visible width, and a hardware encoder may want
//! its input padded too. Every function here therefore takes explicit strides
//! and copies **row by row**. Treating a strided buffer as tightly packed is
//! the classic cause of a picture that looks diagonally sheared, and it is a
//! bug that renders "almost correctly" — which makes it easy to ship.

/// Bytes needed for a tightly-packed NV12 frame.
pub fn nv12_len(width: u32, height: u32) -> usize {
    let (w, h) = (width as usize, height as usize);
    w * h + (w / 2) * (h / 2) * 2
}

/// Convert tightly-packed I420 planes into NV12.
///
/// `dst` is written as `[Y][UV]` with row padding of `dst_stride` for both
/// planes (NV12's UV plane uses the same stride as Y, since two half-width
/// chroma samples occupy the same bytes as one full-width luma row).
///
/// Returns `false` if any buffer is too small, rather than panicking — this
/// sits on the capture hot path.
pub fn i420_to_nv12(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
) -> bool {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w / 2, h / 2);
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
        return false;
    }
    if y.len() < w * h || u.len() < cw * ch || v.len() < cw * ch {
        return false;
    }
    if dst_stride < w {
        return false;
    }
    // Y rows then UV rows, both at dst_stride.
    if dst.len() < dst_stride * h + dst_stride * ch {
        return false;
    }

    for row in 0..h {
        dst[row * dst_stride..row * dst_stride + w].copy_from_slice(&y[row * w..row * w + w]);
    }

    let uv_base = dst_stride * h;
    for row in 0..ch {
        let out = &mut dst[uv_base + row * dst_stride..uv_base + row * dst_stride + cw * 2];
        let u_row = &u[row * cw..row * cw + cw];
        let v_row = &v[row * cw..row * cw + cw];
        for col in 0..cw {
            out[col * 2] = u_row[col];
            out[col * 2 + 1] = v_row[col];
        }
    }
    true
}

/// Convert an NV12 buffer into tightly-packed I420 planes.
///
/// `src_stride` is the row stride of both the Y and UV planes, and `uv_offset`
/// is where the UV plane begins — Media Foundation reports these separately and
/// the UV plane does **not** always start at `stride * height`.
pub fn nv12_to_i420(
    src: &[u8],
    src_stride: usize,
    uv_offset: usize,
    width: u32,
    height: u32,
) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w / 2, h / 2);
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || src_stride < w {
        return None;
    }
    if src.len() < uv_offset + src_stride * ch.saturating_sub(1) + cw * 2 {
        return None;
    }
    if uv_offset + src_stride * (ch.saturating_sub(1)) + cw * 2 > src.len() {
        return None;
    }

    let mut y = Vec::with_capacity(w * h);
    for row in 0..h {
        let start = row * src_stride;
        if start + w > src.len() {
            return None;
        }
        y.extend_from_slice(&src[start..start + w]);
    }

    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for row in 0..ch {
        let start = uv_offset + row * src_stride;
        if start + cw * 2 > src.len() {
            return None;
        }
        let uv = &src[start..start + cw * 2];
        for col in 0..cw {
            u.push(uv[col * 2]);
            v.push(uv[col * 2 + 1]);
        }
    }

    Some((y, u, v))
}

/// Convert packed YUY2 (a.k.a. YUYV, 4:2:2) into tightly-packed I420.
///
/// Many webcams offer YUY2 when they do not offer NV12, so this is the common
/// fallback on the capture path. Layout is `Y0 U0 Y1 V0` per two pixels, with
/// chroma subsampled horizontally only.
///
/// Going to 4:2:0 therefore needs **vertical** chroma subsampling as well: each
/// output chroma row averages two input rows. Taking only the even row instead
/// is a tempting shortcut that produces visible chroma shimmer on motion.
pub fn yuy2_to_i420(
    src: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
) -> Option<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w / 2, h / 2);
    if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 || src_stride < w * 2 {
        return None;
    }
    if src.len() < src_stride * (h - 1) + w * 2 {
        return None;
    }

    let mut y = Vec::with_capacity(w * h);
    for row in 0..h {
        let base = row * src_stride;
        for col in 0..w {
            y.push(src[base + col * 2]);
        }
    }

    let mut u = Vec::with_capacity(cw * ch);
    let mut v = Vec::with_capacity(cw * ch);
    for crow in 0..ch {
        let r0 = crow * 2 * src_stride;
        let r1 = (crow * 2 + 1) * src_stride;
        for ccol in 0..cw {
            // Each YUY2 macropixel is 4 bytes and covers 2 horizontal pixels.
            let i = ccol * 4;
            let u0 = src[r0 + i + 1] as u16;
            let v0 = src[r0 + i + 3] as u16;
            let u1 = src[r1 + i + 1] as u16;
            let v1 = src[r1 + i + 3] as u16;
            u.push(((u0 + u1) / 2) as u8);
            v.push(((v0 + v1) / 2) as u8);
        }
    }

    Some((y, u, v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::frame::RawFrame;

    #[test]
    fn nv12_len_matches_layout() {
        assert_eq!(nv12_len(640, 360), 640 * 360 + 320 * 180 * 2);
        assert_eq!(nv12_len(2, 2), 4 + 2);
    }

    #[test]
    fn round_trip_preserves_every_sample() {
        let f = RawFrame::test_pattern(64, 48, 5);
        let stride = f.width as usize;
        let mut nv12 = vec![0u8; nv12_len(f.width, f.height)];
        assert!(i420_to_nv12(
            &f.y, &f.u, &f.v, f.width, f.height, &mut nv12, stride
        ));

        let uv_offset = stride * f.height as usize;
        let (y, u, v) = nv12_to_i420(&nv12, stride, uv_offset, f.width, f.height).unwrap();
        assert_eq!(y, f.y);
        assert_eq!(u, f.u);
        assert_eq!(v, f.v);
    }

    #[test]
    fn round_trip_survives_padded_strides() {
        // The case that produces sheared video when handled as packed.
        let f = RawFrame::test_pattern(64, 48, 9);
        let stride = 96; // deliberately > width
        let mut nv12 = vec![0xCDu8; stride * 48 + stride * 24];
        assert!(i420_to_nv12(
            &f.y, &f.u, &f.v, f.width, f.height, &mut nv12, stride
        ));

        let (y, u, v) = nv12_to_i420(&nv12, stride, stride * 48, f.width, f.height).unwrap();
        assert_eq!(y, f.y, "luma must survive a padded stride");
        assert_eq!(u, f.u);
        assert_eq!(v, f.v);
    }

    #[test]
    fn interleave_order_is_u_then_v() {
        // 2x2 frame: one chroma sample. NV12 stores U before V; getting this
        // backwards swaps red and blue, which looks plausible enough to miss.
        let y = vec![10, 11, 12, 13];
        let u = vec![200];
        let v = vec![100];
        let mut nv12 = vec![0u8; nv12_len(2, 2)];
        assert!(i420_to_nv12(&y, &u, &v, 2, 2, &mut nv12, 2));
        assert_eq!(&nv12[0..4], &[10, 11, 12, 13]);
        assert_eq!(nv12[4], 200, "U must come first");
        assert_eq!(nv12[5], 100, "V must come second");
    }

    #[test]
    fn rejects_undersized_destination() {
        let f = RawFrame::black(16, 16);
        let mut small = vec![0u8; 10];
        assert!(!i420_to_nv12(
            &f.y, &f.u, &f.v, f.width, f.height, &mut small, 16
        ));
    }

    #[test]
    fn rejects_stride_narrower_than_width() {
        let f = RawFrame::black(16, 16);
        let mut dst = vec![0u8; nv12_len(16, 16)];
        assert!(!i420_to_nv12(
            &f.y, &f.u, &f.v, f.width, f.height, &mut dst, 8
        ));
        assert!(nv12_to_i420(&dst, 8, 256, 16, 16).is_none());
    }

    #[test]
    fn rejects_odd_dimensions() {
        let y = vec![0u8; 15 * 15];
        let c = vec![0u8; 7 * 7];
        let mut dst = vec![0u8; 1024];
        assert!(!i420_to_nv12(&y, &c, &c, 15, 15, &mut dst, 15));
        assert!(nv12_to_i420(&dst, 15, 225, 15, 15).is_none());
    }

    #[test]
    fn rejects_truncated_source() {
        let short = vec![0u8; 8];
        assert!(nv12_to_i420(&short, 64, 4096, 64, 48).is_none());
    }

    // ── YUY2 ────────────────────────────────────────────────────────────────

    /// Build a YUY2 buffer from per-pixel luma and per-macropixel chroma.
    fn make_yuy2(w: usize, h: usize, luma: impl Fn(usize, usize) -> u8, u: u8, v: u8) -> Vec<u8> {
        let mut out = vec![0u8; w * 2 * h];
        for row in 0..h {
            for pair in 0..w / 2 {
                let i = row * w * 2 + pair * 4;
                out[i] = luma(row, pair * 2);
                out[i + 1] = u;
                out[i + 2] = luma(row, pair * 2 + 1);
                out[i + 3] = v;
            }
        }
        out
    }

    #[test]
    fn yuy2_extracts_luma_for_every_pixel() {
        let (w, h) = (8usize, 4usize);
        let src = make_yuy2(w, h, |r, c| (r * 16 + c) as u8, 90, 200);
        let (y, u, v) = yuy2_to_i420(&src, w * 2, w as u32, h as u32).unwrap();

        assert_eq!(y.len(), w * h);
        for row in 0..h {
            for col in 0..w {
                assert_eq!(y[row * w + col], (row * 16 + col) as u8);
            }
        }
        assert_eq!(u.len(), (w / 2) * (h / 2));
        assert!(u.iter().all(|&x| x == 90));
        assert!(v.iter().all(|&x| x == 200));
    }

    #[test]
    fn yuy2_averages_chroma_across_row_pairs() {
        // Rows alternate chroma; 4:2:0 output must average them vertically
        // rather than sampling only the even row.
        let (w, h) = (4usize, 2usize);
        let mut src = vec![0u8; w * 2 * h];
        for pair in 0..w / 2 {
            let i = pair * 4;
            src[i + 1] = 100; // row 0 U
            src[i + 3] = 20; // row 0 V
            let j = w * 2 + pair * 4;
            src[j + 1] = 200; // row 1 U
            src[j + 3] = 60; // row 1 V
        }
        let (_, u, v) = yuy2_to_i420(&src, w * 2, w as u32, h as u32).unwrap();
        assert!(u.iter().all(|&x| x == 150), "U must be the mean of 100/200");
        assert!(v.iter().all(|&x| x == 40), "V must be the mean of 20/60");
    }

    #[test]
    fn yuy2_handles_padded_stride() {
        let (w, h) = (8usize, 4usize);
        let stride = w * 2 + 16;
        let mut src = vec![0xEEu8; stride * h];
        for row in 0..h {
            for pair in 0..w / 2 {
                let i = row * stride + pair * 4;
                src[i] = (row * 16 + pair * 2) as u8;
                src[i + 1] = 128;
                src[i + 2] = (row * 16 + pair * 2 + 1) as u8;
                src[i + 3] = 128;
            }
        }
        let (y, _, _) = yuy2_to_i420(&src, stride, w as u32, h as u32).unwrap();
        assert_eq!(y[0], 0);
        assert_eq!(y[w], 16, "second row must start after the padding");
    }

    #[test]
    fn yuy2_rejects_bad_geometry() {
        let src = vec![0u8; 256];
        assert!(yuy2_to_i420(&src, 16, 0, 4).is_none());
        assert!(yuy2_to_i420(&src, 16, 7, 4).is_none());
        assert!(yuy2_to_i420(&src, 4, 8, 4).is_none(), "stride < width*2");
        assert!(yuy2_to_i420(&[0u8; 4], 16, 8, 4).is_none(), "truncated");
    }
}
