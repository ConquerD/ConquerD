//! Turning CameraX buffers into the I420 frames the encoder expects.
//!
//! CameraX hands out `YUV_420_888`, which is a family rather than a layout:
//! each plane carries its own row stride, and the chroma planes carry a *pixel*
//! stride that is 1 when they are planar and 2 when the device is really
//! producing NV12/NV21 with the two chroma planes interleaved in one buffer.
//! Nothing downstream copes with that, so it is normalised here into tightly
//! packed I420.

use conquerd_client::video::frame::RawFrame;

/// One plane as CameraX describes it.
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub row_stride: usize,
    pub pixel_stride: usize,
}

/// Copy a plane into a tightly packed buffer of `width * height` bytes.
///
/// Handles both the row padding every device applies and the interleaved
/// chroma layout many of them use.
fn pack_plane(plane: &Plane<'_>, width: usize, height: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height);

    for row in 0..height {
        let start = row * plane.row_stride;

        if plane.pixel_stride == 1 {
            // Planar: the row is contiguous, so take it wholesale. A short
            // final row is possible when the buffer omits trailing padding.
            let end = (start + width).min(plane.data.len());
            if start >= plane.data.len() {
                break;
            }
            out.extend_from_slice(&plane.data[start..end]);
            // Pad a truncated row rather than shifting every later row.
            out.resize((row + 1) * width, 0);
        } else {
            // Interleaved: every `pixel_stride`th byte belongs to this plane.
            for column in 0..width {
                let index = start + column * plane.pixel_stride;
                out.push(plane.data.get(index).copied().unwrap_or(0));
            }
        }
    }

    out.resize(width * height, 0);
    out
}

/// Build a tightly packed I420 frame, applying `rotation_degrees`.
///
/// Phone sensors are mounted landscape, so a portrait preview is almost always
/// 90 degrees out. Rotating here rather than at the far end means every peer,
/// on any platform, receives an upright picture without needing to know it came
/// from a phone.
pub fn build_frame(
    y: &Plane<'_>,
    u: &Plane<'_>,
    v: &Plane<'_>,
    width: usize,
    height: usize,
    rotation_degrees: i32,
) -> RawFrame {
    // 4:2:0 chroma is subsampled by two in each axis, so odd dimensions have
    // no valid representation.
    let width = width & !1;
    let height = height & !1;
    let (cw, ch) = (width / 2, height / 2);

    let y_plane = pack_plane(y, width, height);
    let u_plane = pack_plane(u, cw, ch);
    let v_plane = pack_plane(v, cw, ch);

    match rotation_degrees.rem_euclid(360) {
        90 => rotate_i420(&y_plane, &u_plane, &v_plane, width, height, Rotation::Cw90),
        180 => rotate_i420(&y_plane, &u_plane, &v_plane, width, height, Rotation::Cw180),
        270 => rotate_i420(&y_plane, &u_plane, &v_plane, width, height, Rotation::Cw270),
        _ => RawFrame {
            width: width as u32,
            height: height as u32,
            y: y_plane,
            u: u_plane,
            v: v_plane,
        },
    }
}

#[derive(Clone, Copy)]
enum Rotation {
    Cw90,
    Cw180,
    Cw270,
}

/// Rotate a single packed plane clockwise.
fn rotate_plane(src: &[u8], width: usize, height: usize, rotation: Rotation) -> Vec<u8> {
    let mut out = vec![0u8; width * height];

    for row in 0..height {
        for column in 0..width {
            let Some(&pixel) = src.get(row * width + column) else {
                continue;
            };
            // 90 and 270 transpose, so the destination is indexed by height.
            let index = match rotation {
                Rotation::Cw90 => column * height + (height - 1 - row),
                Rotation::Cw180 => (height - 1 - row) * width + (width - 1 - column),
                Rotation::Cw270 => (width - 1 - column) * height + row,
            };
            if let Some(slot) = out.get_mut(index) {
                *slot = pixel;
            }
        }
    }

    out
}

fn rotate_i420(
    y: &[u8],
    u: &[u8],
    v: &[u8],
    width: usize,
    height: usize,
    rotation: Rotation,
) -> RawFrame {
    let (cw, ch) = (width / 2, height / 2);
    let rotated_y = rotate_plane(y, width, height, rotation);
    let rotated_u = rotate_plane(u, cw, ch, rotation);
    let rotated_v = rotate_plane(v, cw, ch, rotation);

    // A quarter turn swaps the axes; a half turn does not.
    let (out_w, out_h) = match rotation {
        Rotation::Cw180 => (width, height),
        Rotation::Cw90 | Rotation::Cw270 => (height, width),
    };

    RawFrame {
        width: out_w as u32,
        height: out_h as u32,
        y: rotated_y,
        u: rotated_u,
        v: rotated_v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(data: &[u8], row_stride: usize, pixel_stride: usize) -> Plane<'_> {
        Plane {
            data,
            row_stride,
            pixel_stride,
        }
    }

    #[test]
    fn packs_a_padded_planar_row() {
        // 2x2 luma with two bytes of row padding, which is what a device with
        // a 4-byte row alignment produces.
        let data = [1, 2, 0xFF, 0xFF, 3, 4, 0xFF, 0xFF];
        let packed = pack_plane(&plane(&data, 4, 1), 2, 2);
        assert_eq!(packed, vec![1, 2, 3, 4], "padding must not survive");
    }

    #[test]
    fn packs_interleaved_chroma() {
        // pixel_stride 2 is the NV12/NV21 case: this plane's bytes are every
        // other byte, with the other chroma plane in between.
        let data = [1, 9, 2, 9, 3, 9, 4, 9];
        let packed = pack_plane(&plane(&data, 4, 2), 2, 2);
        assert_eq!(packed, vec![1, 2, 3, 4], "the other plane must be skipped");
    }

    #[test]
    fn a_short_buffer_does_not_panic() {
        // Truncated buffers must degrade to black rather than read past the
        // end - this data arrives from another process's camera HAL.
        let data = [1, 2];
        let packed = pack_plane(&plane(&data, 4, 1), 2, 2);
        assert_eq!(packed.len(), 4);
    }

    #[test]
    fn quarter_turns_swap_the_axes() {
        let y = vec![0u8; 8 * 4];
        let u = vec![0u8; 4 * 2];
        let v = vec![0u8; 4 * 2];
        let rotated = rotate_i420(&y, &u, &v, 8, 4, Rotation::Cw90);
        assert_eq!((rotated.width, rotated.height), (4, 8));
        assert!(rotated.is_consistent(), "planes must match the new size");
    }

    #[test]
    fn a_half_turn_keeps_the_axes() {
        let y = vec![0u8; 8 * 4];
        let u = vec![0u8; 4 * 2];
        let v = vec![0u8; 4 * 2];
        let rotated = rotate_i420(&y, &u, &v, 8, 4, Rotation::Cw180);
        assert_eq!((rotated.width, rotated.height), (8, 4));
        assert!(rotated.is_consistent());
    }

    #[test]
    fn rotating_ninety_degrees_moves_the_corner() {
        // Top-left of a 2x2 must land top-right after a clockwise quarter turn.
        let y = vec![1, 2, 3, 4];
        let u = vec![0u8; 1];
        let v = vec![0u8; 1];
        let rotated = rotate_i420(&y, &u, &v, 2, 2, Rotation::Cw90);
        assert_eq!(rotated.y, vec![3, 1, 4, 2]);
    }

    #[test]
    fn odd_dimensions_are_rounded_down() {
        let data = vec![0u8; 64];
        let frame = build_frame(
            &plane(&data, 9, 1),
            &plane(&data, 5, 1),
            &plane(&data, 5, 1),
            9,
            5,
            0,
        );
        assert_eq!((frame.width, frame.height), (8, 4));
        assert!(frame.is_consistent());
    }
}
