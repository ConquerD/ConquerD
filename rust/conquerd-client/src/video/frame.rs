//! Raw video frames in I420 (planar YUV 4:2:0).
//!
//! I420 is the pivot format for the whole pipeline: cameras deliver it (or
//! something trivially convertible), libvpx consumes and produces it, and Qt's
//! `QVideoFrameFormat::Format_YUV420P` maps to it one-to-one so the GPU does
//! the YUV-to-RGB conversion for free. Nothing in this pipeline should ever
//! convert to RGB on the CPU.
//!
//! Planes are stored tightly packed — `y` is `width * height`, `u` and `v` are
//! each `(width/2) * (height/2)`. Capture backends and decoders both produce
//! *strided* buffers whose rows may be padded, so they must copy row by row
//! into this layout rather than a single `memcpy`. That padding difference is
//! the classic source of diagonally-sheared video.

/// A decoded frame in tightly-packed I420.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawFrame {
    /// Frame width in pixels. Always even.
    pub width: u32,
    /// Frame height in pixels. Always even.
    pub height: u32,
    /// Luma plane, `width * height` bytes.
    pub y: Vec<u8>,
    /// Blue-difference chroma, `(width/2) * (height/2)` bytes.
    pub u: Vec<u8>,
    /// Red-difference chroma, `(width/2) * (height/2)` bytes.
    pub v: Vec<u8>,
}

impl RawFrame {
    /// Allocate a black frame. Dimensions are rounded down to even values,
    /// since 4:2:0 chroma is subsampled by two in each axis.
    pub fn black(width: u32, height: u32) -> Self {
        let (width, height) = (width & !1, height & !1);
        let (cw, ch) = (width / 2, height / 2);
        Self {
            width,
            height,
            y: vec![16u8; (width * height) as usize],
            // 128 is the neutral chroma value: 16/128/128 is video-range black.
            u: vec![128u8; (cw * ch) as usize],
            v: vec![128u8; (cw * ch) as usize],
        }
    }

    /// Bytes in a tightly-packed I420 frame of this size.
    pub fn packed_len(width: u32, height: u32) -> usize {
        let (width, height) = (width & !1, height & !1);
        (width * height) as usize + 2 * ((width / 2) * (height / 2)) as usize
    }

    /// Whether the plane lengths agree with the declared dimensions.
    pub fn is_consistent(&self) -> bool {
        let (cw, ch) = (self.width / 2, self.height / 2);
        self.width % 2 == 0
            && self.height % 2 == 0
            && self.y.len() == (self.width * self.height) as usize
            && self.u.len() == (cw * ch) as usize
            && self.v.len() == (cw * ch) as usize
    }

    /// A deterministic test pattern: a moving diagonal gradient.
    ///
    /// Used to prove the transport end to end before a real codec exists. Two
    /// frames with the same `tick` are byte-identical, so a receiver can
    /// checksum what it reassembled against what the sender produced.
    pub fn test_pattern(width: u32, height: u32, tick: u32) -> Self {
        let mut f = Self::black(width, height);
        let (w, h) = (f.width, f.height);
        for row in 0..h {
            for col in 0..w {
                f.y[(row * w + col) as usize] = ((row + col + tick) % 256) as u8;
            }
        }
        let (cw, ch) = (w / 2, h / 2);
        for row in 0..ch {
            for col in 0..cw {
                let i = (row * cw + col) as usize;
                f.u[i] = ((col * 2 + tick) % 256) as u8;
                f.v[i] = ((row * 2 + tick) % 256) as u8;
            }
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_frame_is_consistent() {
        let f = RawFrame::black(640, 360);
        assert!(f.is_consistent());
        assert_eq!(f.y.len(), 640 * 360);
        assert_eq!(f.u.len(), 320 * 180);
        assert_eq!(f.v.len(), 320 * 180);
    }

    #[test]
    fn odd_dimensions_are_rounded_down() {
        let f = RawFrame::black(641, 361);
        assert_eq!((f.width, f.height), (640, 360));
        assert!(f.is_consistent());
    }

    #[test]
    fn packed_len_matches_allocation() {
        let f = RawFrame::black(160, 120);
        assert_eq!(
            RawFrame::packed_len(160, 120),
            f.y.len() + f.u.len() + f.v.len()
        );
        // The stub codec's working size, quoted in the Phase 3 plan.
        assert_eq!(RawFrame::packed_len(160, 120), 28_800);
    }

    #[test]
    fn test_pattern_is_deterministic_and_moves() {
        let a = RawFrame::test_pattern(64, 48, 3);
        let b = RawFrame::test_pattern(64, 48, 3);
        assert_eq!(a, b, "same tick must produce identical bytes");
        assert!(a.is_consistent());
        assert_ne!(a, RawFrame::test_pattern(64, 48, 4));
    }
}
