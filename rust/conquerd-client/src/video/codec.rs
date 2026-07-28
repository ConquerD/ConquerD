//! Video codec seam, plus a compression-free stub implementation.
//!
//! The stub exists so the transport can be proven end to end before libvpx is
//! vendored. It deliberately does **no** compression: it packs raw I420 and
//! declares every frame a keyframe. At the 160x120 working size that is 28.8 KB
//! per frame — about 27 fragments — which stresses reassembly *harder* than
//! real VP8 will, with zero new dependencies. If a frame arrives corrupt while
//! the stub is in use, the bug is in the transport, not the codec.
//!
//! Phase 4 replaces [`StubEncoder`] / [`StubDecoder`] with libvpx behind the
//! same two traits. Nothing above this module should need to change.

use super::frame::RawFrame;

/// Working resolution for the stub codec.
///
/// Small on purpose: without compression a larger frame would exceed
/// [`MAX_FRAGS_PER_FRAME`](super::fragment::MAX_FRAGS_PER_FRAME).
pub const STUB_WIDTH: u32 = 160;
/// See [`STUB_WIDTH`].
pub const STUB_HEIGHT: u32 = 120;

/// Bytes of stub header before the planes: `[width:u16 BE][height:u16 BE]`.
const STUB_HEADER_LEN: usize = 4;

/// Encodes raw frames into a compressed (or, for the stub, packed) byte string.
pub trait VideoEncoder: Send {
    /// Encode one frame. Returns the encoded bytes and whether it is a
    /// keyframe — the caller needs the flag for the fragment header so a
    /// receiver can tell whether it can start decoding here.
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)>;

    /// Ask for the next frame to be a keyframe, in response to a receiver's
    /// keyframe request. Implementations must treat this as a request for the
    /// *next* encode, never encode one synchronously here.
    fn request_keyframe(&mut self);

    /// Retarget the encoder's average bitrate, for adaptive control.
    ///
    /// Must adjust the *running* encoder rather than rebuild it: a rebuild
    /// resets reference frames, so every rate change would cost a keyframe —
    /// a bandwidth spike at exactly the moment the link is already struggling.
    ///
    /// Defaults to accepting and ignoring the request, which is right for
    /// codecs with no rate control (the stub packs raw I420 at a fixed size).
    fn set_bitrate(&mut self, _bps: u32) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Decodes what a [`VideoEncoder`] produced.
pub trait VideoDecoder: Send {
    /// Decode one encoded frame.
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<RawFrame>;
}

/// Compression-free encoder: packs I420 behind a 4-byte dimension header.
#[derive(Debug, Default)]
pub struct StubEncoder;

impl VideoEncoder for StubEncoder {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)> {
        if !frame.is_consistent() {
            anyhow::bail!(
                "inconsistent frame: {}x{} with planes {}/{}/{}",
                frame.width,
                frame.height,
                frame.y.len(),
                frame.u.len(),
                frame.v.len()
            );
        }
        let mut out =
            Vec::with_capacity(STUB_HEADER_LEN + frame.y.len() + frame.u.len() + frame.v.len());
        out.extend_from_slice(&(frame.width as u16).to_be_bytes());
        out.extend_from_slice(&(frame.height as u16).to_be_bytes());
        out.extend_from_slice(&frame.y);
        out.extend_from_slice(&frame.u);
        out.extend_from_slice(&frame.v);
        // Every stub frame stands alone, so every frame is a keyframe.
        Ok((out, true))
    }

    fn request_keyframe(&mut self) {
        // No-op: every stub frame is already a keyframe.
    }
}

/// Counterpart to [`StubEncoder`].
#[derive(Debug, Default)]
pub struct StubDecoder;

impl VideoDecoder for StubDecoder {
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<RawFrame> {
        if encoded.len() < STUB_HEADER_LEN {
            anyhow::bail!("stub frame too short: {} bytes", encoded.len());
        }
        let width = u16::from_be_bytes([encoded[0], encoded[1]]) as u32;
        let height = u16::from_be_bytes([encoded[2], encoded[3]]) as u32;
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            anyhow::bail!("stub frame has bad dimensions {width}x{height}");
        }

        let y_len = (width * height) as usize;
        let c_len = ((width / 2) * (height / 2)) as usize;
        let want = STUB_HEADER_LEN + y_len + 2 * c_len;
        // Checked before slicing: a hostile header must not make us index past
        // the buffer or allocate against a size the payload doesn't back.
        if encoded.len() != want {
            anyhow::bail!(
                "stub frame length {} does not match {width}x{height} (want {want})",
                encoded.len()
            );
        }

        let y_end = STUB_HEADER_LEN + y_len;
        let u_end = y_end + c_len;
        Ok(RawFrame {
            width,
            height,
            y: encoded[STUB_HEADER_LEN..y_end].to_vec(),
            u: encoded[y_end..u_end].to_vec(),
            v: encoded[u_end..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_round_trips_a_frame() {
        let frame = RawFrame::test_pattern(STUB_WIDTH, STUB_HEIGHT, 9);
        let (encoded, keyframe) = StubEncoder.encode(&frame).unwrap();
        assert!(keyframe);
        assert_eq!(
            encoded.len(),
            STUB_HEADER_LEN + RawFrame::packed_len(STUB_WIDTH, STUB_HEIGHT)
        );
        assert_eq!(StubDecoder.decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn stub_frame_fits_the_fragment_budget() {
        // The stub must stay encodable within MAX_FRAGS_PER_FRAME, or Phase 3
        // would be testing the fragmenter's refusal path instead of its
        // reassembly path.
        let frame = RawFrame::test_pattern(STUB_WIDTH, STUB_HEIGHT, 0);
        let (encoded, _) = StubEncoder.encode(&frame).unwrap();
        let parts = super::super::fragment::fragment_frame(
            "sender-id-placeholder-000000000000000000000",
            1,
            true,
            &[0u8; super::super::fragment::SIGNATURE_LEN],
            &encoded,
            1198,
        );
        let parts = parts.expect("stub frame must fit MAX_FRAGS_PER_FRAME");
        assert!(
            parts.len() > 20,
            "expected a genuinely multi-fragment frame"
        );
    }

    #[test]
    fn decode_rejects_truncated_input() {
        assert!(StubDecoder.decode(&[]).is_err());
        assert!(StubDecoder.decode(&[0, 160]).is_err());
    }

    #[test]
    fn decode_rejects_length_mismatch() {
        // Header claims 160x120 but the payload is far too short — must be
        // refused rather than panicking on a slice.
        let mut bad = Vec::new();
        bad.extend_from_slice(&160u16.to_be_bytes());
        bad.extend_from_slice(&120u16.to_be_bytes());
        bad.extend_from_slice(&[0u8; 100]);
        assert!(StubDecoder.decode(&bad).is_err());
    }

    #[test]
    fn decode_rejects_bad_dimensions() {
        for (w, h) in [(0u16, 120u16), (160, 0), (161, 120), (160, 121)] {
            let mut bad = Vec::new();
            bad.extend_from_slice(&w.to_be_bytes());
            bad.extend_from_slice(&h.to_be_bytes());
            bad.extend_from_slice(&[0u8; 64]);
            assert!(
                StubDecoder.decode(&bad).is_err(),
                "{w}x{h} must be rejected"
            );
        }
    }

    #[test]
    fn encode_rejects_inconsistent_frame() {
        let mut frame = RawFrame::black(64, 48);
        frame.u.truncate(3);
        assert!(StubEncoder.encode(&frame).is_err());
    }
}
