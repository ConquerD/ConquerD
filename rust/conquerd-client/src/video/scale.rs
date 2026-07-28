//! BGRA to I420 conversion with downscaling.
//!
//! Screen capture hands back BGRA at the source's native resolution, which is
//! routinely far larger than the encoder is configured for — a 4K monitor is 33
//! times the pixels of a 640x360 preset. Converting and scaling in one pass
//! avoids materialising a full-size I420 intermediate, which at 4K would be
//! ~12 MB per frame, allocated and discarded thirty times a second.
//!
//! # Sampling
//!
//! Box filtering: each destination pixel averages the source rectangle that
//! maps onto it. Nearest-neighbour is cheaper but aliases badly on exactly the
//! content screen capture is for — text and thin UI lines shimmer and break up
//! as the window moves. Averaging also feeds the encoder a cleaner signal, so
//! it spends fewer bits on high-frequency noise that was a sampling artefact.

/// BT.601 studio-swing coefficients, matching what [`super::nv12`] assumes on
/// the camera path so both sources land in the same colour space.
#[inline]
fn rgb_to_y(r: f32, g: f32, b: f32) -> u8 {
    (0.257 * r + 0.504 * g + 0.098 * b + 16.0).clamp(0.0, 255.0) as u8
}

#[inline]
fn rgb_to_u(r: f32, g: f32, b: f32) -> u8 {
    (-0.148 * r - 0.291 * g + 0.439 * b + 128.0).clamp(0.0, 255.0) as u8
}

#[inline]
fn rgb_to_v(r: f32, g: f32, b: f32) -> u8 {
    (0.439 * r - 0.368 * g - 0.071 * b + 128.0).clamp(0.0, 255.0) as u8
}

/// Average the BGRA source rectangle `[x0,x1) x [y0,y1)` into linear RGB.
///
/// Returns `(r, g, b)`. The rectangle is clamped by the caller, so an empty
/// range cannot occur.
#[inline]
fn average_box(
    src: &[u8],
    stride: usize,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
) -> (f32, f32, f32) {
    let mut r = 0u32;
    let mut g = 0u32;
    let mut b = 0u32;
    let mut n = 0u32;
    for y in y0..y1 {
        let row = y * stride;
        for x in x0..x1 {
            let i = row + x * 4;
            // BGRA byte order; alpha is ignored — screen content is opaque and
            // the encoder has no alpha channel to carry it in anyway.
            b += src[i] as u32;
            g += src[i + 1] as u32;
            r += src[i + 2] as u32;
            n += 1;
        }
    }
    let n = n.max(1) as f32;
    (r as f32 / n, g as f32 / n, b as f32 / n)
}

/// Convert BGRA to tightly-packed I420, scaling to `dst_w` x `dst_h`.
///
/// `stride` is the source row pitch in bytes, which for a mapped D3D11 texture
/// is generally larger than `src_w * 4` — treating the buffer as tightly packed
/// is what produces the classic sheared image.
///
/// Returns `None` if the inputs are inconsistent rather than panicking: the
/// source is a mapped GPU texture whose dimensions come from the driver, so
/// validation belongs here rather than at every call site.
pub fn bgra_to_i420_scaled(
    src: &[u8],
    stride: usize,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Option<super::frame::RawFrame> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return None;
    }
    // I420 chroma is subsampled 2x2, so odd output dimensions are not
    // representable and the encoder rejects them.
    if dst_w % 2 != 0 || dst_h % 2 != 0 {
        return None;
    }
    if stride < src_w as usize * 4 {
        return None;
    }
    if src.len() < stride * src_h as usize {
        return None;
    }

    let (sw, sh) = (src_w as usize, src_h as usize);
    let (dw, dh) = (dst_w as usize, dst_h as usize);

    let mut y_plane = vec![0u8; dw * dh];
    let mut u_plane = vec![0u8; (dw / 2) * (dh / 2)];
    let mut v_plane = vec![0u8; (dw / 2) * (dh / 2)];

    // Luma: one box average per destination pixel.
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = (((dy + 1) * sh).div_ceil(dh)).min(sh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = (((dx + 1) * sw).div_ceil(dw)).min(sw).max(x0 + 1);
            let (r, g, b) = average_box(src, stride, x0, x1, y0, y1);
            y_plane[dy * dw + dx] = rgb_to_y(r, g, b);
        }
    }

    // Chroma: one box average per 2x2 destination block, so the source
    // rectangle is twice as wide and tall as the luma one. Averaging over the
    // whole block (rather than sampling one corner) is what keeps colour edges
    // from crawling when the captured window scrolls.
    let (cw, ch) = (dw / 2, dh / 2);
    for cy in 0..ch {
        let y0 = (cy * 2) * sh / dh;
        let y1 = ((((cy * 2) + 2) * sh).div_ceil(dh)).min(sh).max(y0 + 1);
        for cx in 0..cw {
            let x0 = (cx * 2) * sw / dw;
            let x1 = ((((cx * 2) + 2) * sw).div_ceil(dw)).min(sw).max(x0 + 1);
            let (r, g, b) = average_box(src, stride, x0, x1, y0, y1);
            u_plane[cy * cw + cx] = rgb_to_u(r, g, b);
            v_plane[cy * cw + cx] = rgb_to_v(r, g, b);
        }
    }

    Some(super::frame::RawFrame {
        width: dst_w,
        height: dst_h,
        y: y_plane,
        u: u_plane,
        v: v_plane,
    })
}

/// Convert BGRA to I420 at exactly `dst_w` x `dst_h`, scaling the source to
/// fit and centring it on black bars.
///
/// A **fixed** output size is the point. The encoder is configured once with
/// one resolution and rejects any frame that does not match, but a captured
/// window can be resized by the user at any moment and a monitor can change
/// mode. Letterboxing absorbs both: the content is scaled to fit and the
/// leftover is filled, so the frame handed to the encoder is always the size it
/// expects. The alternative — reconfiguring the encoder on every resize — costs
/// a keyframe and a rate-control reset each time.
pub fn bgra_to_i420_letterboxed(
    src: &[u8],
    stride: usize,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Option<super::frame::RawFrame> {
    if dst_w == 0 || dst_h == 0 || dst_w % 2 != 0 || dst_h % 2 != 0 {
        return None;
    }
    let (cw, ch) = super::screen::fit_within(src_w, src_h, dst_w, dst_h);
    let content = bgra_to_i420_scaled(src, stride, src_w, src_h, cw, ch)?;
    if cw == dst_w && ch == dst_h {
        return Some(content);
    }

    // Offsets must be even so the chroma planes stay aligned to the 2x2 grid.
    let off_x = (((dst_w - cw) / 2) as usize) & !1;
    let off_y = (((dst_h - ch) / 2) as usize) & !1;

    let (dw, dh) = (dst_w as usize, dst_h as usize);
    let (cwu, chu) = (cw as usize, ch as usize);

    // Black in BT.601 studio swing: luma floor 16, chroma neutral 128.
    let mut y = vec![16u8; dw * dh];
    let mut u = vec![128u8; (dw / 2) * (dh / 2)];
    let mut v = vec![128u8; (dw / 2) * (dh / 2)];

    for row in 0..chu {
        let dst = (off_y + row) * dw + off_x;
        y[dst..dst + cwu].copy_from_slice(&content.y[row * cwu..row * cwu + cwu]);
    }
    let (cdw, ccw) = (dw / 2, cwu / 2);
    for row in 0..chu / 2 {
        let dst = (off_y / 2 + row) * cdw + off_x / 2;
        u[dst..dst + ccw].copy_from_slice(&content.u[row * ccw..row * ccw + ccw]);
        v[dst..dst + ccw].copy_from_slice(&content.v[row * ccw..row * ccw + ccw]);
    }

    Some(super::frame::RawFrame {
        width: dst_w,
        height: dst_h,
        y,
        u,
        v,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a BGRA buffer of one solid colour, with padding after each row so
    /// the stride genuinely differs from the packed width.
    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8, pad: usize) -> (Vec<u8>, usize) {
        let stride = w as usize * 4 + pad;
        let mut buf = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * stride + x * 4;
                buf[i] = b;
                buf[i + 1] = g;
                buf[i + 2] = r;
                buf[i + 3] = 255;
            }
        }
        (buf, stride)
    }

    #[test]
    fn produces_correctly_sized_planes() {
        let (buf, stride) = solid(64, 48, 10, 20, 30, 0);
        let f = bgra_to_i420_scaled(&buf, stride, 64, 48, 32, 24).unwrap();
        assert_eq!((f.width, f.height), (32, 24));
        assert_eq!(f.y.len(), 32 * 24);
        assert_eq!(f.u.len(), 16 * 12);
        assert_eq!(f.v.len(), 16 * 12);
        assert!(f.is_consistent());
    }

    #[test]
    fn solid_white_maps_to_full_luma() {
        let (buf, stride) = solid(32, 32, 255, 255, 255, 0);
        let f = bgra_to_i420_scaled(&buf, stride, 32, 32, 16, 16).unwrap();
        // BT.601 studio swing puts white at 235, not 255.
        assert!(
            f.y.iter().all(|&y| y >= 233),
            "white should be near the 235 studio-swing ceiling"
        );
        // Neutral colour means both chroma planes sit at the 128 midpoint.
        assert!(f.u.iter().all(|&u| (u as i32 - 128).abs() <= 1));
        assert!(f.v.iter().all(|&v| (v as i32 - 128).abs() <= 1));
    }

    #[test]
    fn solid_black_maps_to_the_luma_floor() {
        let (buf, stride) = solid(16, 16, 0, 0, 0, 0);
        let f = bgra_to_i420_scaled(&buf, stride, 16, 16, 8, 8).unwrap();
        assert!(f.y.iter().all(|&y| y <= 17), "black floor is 16");
    }

    /// The bug this function exists to avoid: a mapped GPU texture's row pitch
    /// is almost never `width * 4`, and ignoring it shears the image.
    #[test]
    fn row_padding_does_not_shear_the_image() {
        let padded = solid(16, 16, 200, 50, 50, 64);
        let packed = solid(16, 16, 200, 50, 50, 0);
        let a = bgra_to_i420_scaled(&padded.0, padded.1, 16, 16, 8, 8).unwrap();
        let b = bgra_to_i420_scaled(&packed.0, packed.1, 16, 16, 8, 8).unwrap();
        assert_eq!(a, b, "stride handling changed the result");
    }

    #[test]
    fn scaling_averages_rather_than_dropping_pixels() {
        // Half black, half white vertically. Downscaling 2:1 must yield the
        // average, not whichever row nearest-neighbour happened to land on.
        let w = 8u32;
        let h = 8u32;
        let stride = w as usize * 4;
        let mut buf = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            let v = if y < 4 { 0u8 } else { 255u8 };
            for x in 0..w as usize {
                let i = y * stride + x * 4;
                buf[i] = v;
                buf[i + 1] = v;
                buf[i + 2] = v;
                buf[i + 3] = 255;
            }
        }
        let f = bgra_to_i420_scaled(&buf, stride, w, h, 4, 4).unwrap();
        // Top two rows sample only black, bottom two only white; nothing
        // should land in between for this exact 2:1 split.
        assert!(f.y[0] <= 17, "top should be black");
        assert!(f.y[f.y.len() - 1] >= 233, "bottom should be white");
    }

    #[test]
    fn rejects_inconsistent_inputs() {
        let (buf, stride) = solid(16, 16, 1, 2, 3, 0);
        // Stride smaller than a packed row.
        assert!(bgra_to_i420_scaled(&buf, 8, 16, 16, 8, 8).is_none());
        // Buffer too short for the claimed height.
        assert!(bgra_to_i420_scaled(&buf[..16], stride, 16, 16, 8, 8).is_none());
        // Odd output dimensions are not representable in I420.
        assert!(bgra_to_i420_scaled(&buf, stride, 16, 16, 7, 8).is_none());
        assert!(bgra_to_i420_scaled(&buf, stride, 16, 16, 8, 7).is_none());
        // Zero dimensions.
        assert!(bgra_to_i420_scaled(&buf, stride, 0, 16, 8, 8).is_none());
        assert!(bgra_to_i420_scaled(&buf, stride, 16, 16, 0, 8).is_none());
    }

    /// A hostile or buggy driver reporting a huge size must not make us read
    /// past the mapped buffer.
    #[test]
    fn refuses_to_read_past_the_buffer() {
        let (buf, stride) = solid(8, 8, 0, 0, 0, 0);
        assert!(bgra_to_i420_scaled(&buf, stride, 8, 4096, 8, 8).is_none());
    }

    // ── Letterboxing ────────────────────────────────────────────────────────

    /// The property the encoder depends on: whatever the source is, the output
    /// is exactly the requested size. A window the user resizes mid-call must
    /// not change the frame size, or every later frame is rejected.
    #[test]
    fn letterbox_always_produces_the_requested_size() {
        for (sw, sh) in [
            (1920u32, 1080u32),
            (1024, 768),
            (400, 900),
            (37, 61),
            (2560, 1440),
        ] {
            let (buf, stride) = solid(sw, sh, 120, 60, 30, 16);
            let f = bgra_to_i420_letterboxed(&buf, stride, sw, sh, 640, 360)
                .unwrap_or_else(|| panic!("{sw}x{sh} produced nothing"));
            assert_eq!(
                (f.width, f.height),
                (640, 360),
                "{sw}x{sh} must letterbox to exactly 640x360"
            );
            assert!(f.is_consistent(), "{sw}x{sh} produced inconsistent planes");
        }
    }

    #[test]
    fn letterbox_fills_the_bars_with_black_not_garbage() {
        // 1:1 source into 16:9 output leaves pillarbox bars on both sides.
        let (buf, stride) = solid(400, 400, 255, 255, 255, 0);
        let f = bgra_to_i420_letterboxed(&buf, stride, 400, 400, 640, 360).unwrap();
        // Far-left column is bar; centre is content.
        assert!(f.y[0] <= 17, "left bar should be the luma floor");
        let mid = (180 * 640 + 320) as usize;
        assert!(f.y[mid] >= 200, "centre should carry the white content");
    }

    #[test]
    fn letterbox_centres_the_content() {
        let (buf, stride) = solid(400, 400, 255, 255, 255, 0);
        let f = bgra_to_i420_letterboxed(&buf, stride, 400, 400, 640, 360).unwrap();
        let row = 180usize;
        let left_bar = (0..640).find(|&x| f.y[row * 640 + x] > 100).unwrap();
        let right_bar = (0..640).rev().find(|&x| f.y[row * 640 + x] > 100).unwrap();
        let left_gap = left_bar;
        let right_gap = 639 - right_bar;
        assert!(
            (left_gap as i32 - right_gap as i32).abs() <= 2,
            "content should be centred, gaps were {left_gap} and {right_gap}"
        );
    }

    /// A source that already matches the target must not pay for a copy.
    #[test]
    fn letterbox_passes_an_exact_fit_straight_through() {
        let (buf, stride) = solid(1280, 720, 10, 200, 10, 0);
        let f = bgra_to_i420_letterboxed(&buf, stride, 1280, 720, 640, 360).unwrap();
        assert_eq!((f.width, f.height), (640, 360));
        // No bars at all: every pixel is content.
        assert!(f.y.iter().all(|&y| y > 17), "16:9 into 16:9 needs no bars");
    }

    #[test]
    fn letterbox_rejects_odd_output_dimensions() {
        let (buf, stride) = solid(64, 64, 1, 1, 1, 0);
        assert!(bgra_to_i420_letterboxed(&buf, stride, 64, 64, 641, 360).is_none());
        assert!(bgra_to_i420_letterboxed(&buf, stride, 64, 64, 640, 361).is_none());
    }

    #[test]
    fn upscaling_is_representable_even_if_unusual() {
        // `fit_within` never asks for this, but the function must not corrupt
        // memory if some caller does.
        let (buf, stride) = solid(4, 4, 100, 100, 100, 0);
        let f = bgra_to_i420_scaled(&buf, stride, 4, 4, 8, 8).unwrap();
        assert_eq!((f.width, f.height), (8, 8));
        assert!(f.is_consistent());
    }
}
