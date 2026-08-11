//! Video codec seam, the codec registry, plus a compression-free stub.
//!
//! [`VideoEncoder`] and [`VideoDecoder`] are the seam; [`make_encoder`] and
//! [`make_decoder`] pick an implementation for a
//! [`VideoCodec`](conquerd_features::video_codec::VideoCodec), and
//! [`available_codecs`] reports what this build can run so the client
//! advertises that set and nothing more.
//!
//! Peers negotiate over these sets rather than assuming a single codec, because
//! what a build can run is a platform fact: Media Foundation's H.264 exists on
//! Windows and nowhere else. See [`conquerd_features::video_codec`] for the
//! negotiation rules and why the capability id names no codec.
//!
//! The stub exists so the transport can be proven end to end without a real
//! codec. It deliberately does **no** compression: it packs raw I420 and
//! declares every frame a keyframe. At the 160x120 working size that is 28.8 KB
//! per frame — about 27 fragments — which stresses reassembly *harder* than a
//! real codec will, with zero new dependencies. If a frame arrives corrupt
//! while the stub is in use, the bug is in the transport, not the codec. It is
//! never advertised to a peer.

use conquerd_features::video_codec::VideoCodec;

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

/// Forward the trait through a box, so [`make_encoder`]'s
/// `Box<dyn VideoEncoder>` satisfies the generic `E: VideoEncoder` bound that
/// [`VideoSender::start`](super::sender::VideoSender::start) takes. Without
/// this the registry could only be used by callers willing to name a concrete
/// encoder type — which is exactly what picking a codec at runtime rules out.
impl VideoEncoder for Box<dyn VideoEncoder> {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<(Vec<u8>, bool)> {
        (**self).encode(frame)
    }

    fn request_keyframe(&mut self) {
        (**self).request_keyframe()
    }

    fn set_bitrate(&mut self, bps: u32) -> anyhow::Result<()> {
        (**self).set_bitrate(bps)
    }
}

/// Decodes what a [`VideoEncoder`] produced.
pub trait VideoDecoder: Send {
    /// Decode one encoded frame.
    ///
    /// `Ok(None)` means the frame was **accepted but produced no picture yet**
    /// — a pipelined decoder holds the first submissions while it fills, and a
    /// decoder that has not seen a keyframe yet has nothing it could draw. That
    /// is a normal state, not a failure, and the distinction is load-bearing:
    /// the receiver escalates on `Err` (keyframe request, then rebuilding the
    /// decoder) and would tear a healthy decoder down mid-warm-up if the two
    /// were collapsed into one. See [`super::receiver`].
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<Option<RawFrame>>;
}

/// See the [`VideoEncoder`] box forward above.
impl VideoDecoder for Box<dyn VideoDecoder> {
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<Option<RawFrame>> {
        (**self).decode(encoded)
    }
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
    /// Never `Ok(None)`: the stub carries a whole picture in every frame, so
    /// there is no pipeline to fill and nothing to wait for.
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<Option<RawFrame>> {
        if encoded.len() < STUB_HEADER_LEN {
            anyhow::bail!("stub frame too short: {} bytes", encoded.len());
        }
        let width = u16::from_be_bytes([encoded[0], encoded[1]]) as u32;
        let height = u16::from_be_bytes([encoded[2], encoded[3]]) as u32;
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
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
        Ok(Some(RawFrame {
            width,
            height,
            y: encoded[STUB_HEADER_LEN..y_end].to_vec(),
            u: encoded[y_end..u_end].to_vec(),
            v: encoded[u_end..].to_vec(),
        }))
    }
}

// ── VP8, via vendored libvpx ────────────────────────────────────────────────

/// VP8 encoder adapter.
///
/// VP8 is the codec that makes video work off Windows: Media Foundation H.264
/// and VideoToolbox H.264 rely on a licence the OS holds, and Linux has no
/// equivalent, so VP8 — royalty-free, and built from vendored source on every
/// platform — is the one codec a Windows peer and a Linux peer can agree on.
pub struct Vp8EncoderAdapter(conquerd_vpx::Vp8Encoder);

impl VideoEncoder for Vp8EncoderAdapter {
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
        let (ew, eh) = self.0.dimensions();
        if (frame.width, frame.height) != (ew, eh) {
            // libvpx encodes at the size it was built for; a mismatched frame
            // would be read with the wrong stride rather than rescaled.
            anyhow::bail!(
                "frame is {}x{} but the encoder was built for {ew}x{eh}",
                frame.width,
                frame.height
            );
        }
        self.0.encode(&frame.y, &frame.u, &frame.v)
    }

    fn request_keyframe(&mut self) {
        self.0.request_keyframe();
    }

    fn set_bitrate(&mut self, bps: u32) -> anyhow::Result<()> {
        self.0.set_bitrate(bps)
    }
}

/// VP8 decoder adapter. See [`Vp8EncoderAdapter`].
pub struct Vp8DecoderAdapter(conquerd_vpx::Vp8Decoder);

impl VideoDecoder for Vp8DecoderAdapter {
    /// libvpx decodes each packet on the call that submits it, so like the stub
    /// this never reports "not yet" — a packet either yields a picture or fails.
    fn decode(&mut self, encoded: &[u8]) -> anyhow::Result<Option<RawFrame>> {
        let f = self.0.decode(encoded)?;
        Ok(Some(RawFrame {
            width: f.width,
            height: f.height,
            y: f.y,
            u: f.u,
            v: f.v,
        }))
    }
}

// ── Codec registry ──────────────────────────────────────────────────────────

/// Codec-neutral encoder settings.
///
/// Mirrors the fields every encoder needs, so callers configure a codec without
/// naming one. Codec-specific tuning stays inside the implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderParams {
    /// Encoded frame width in pixels.
    pub width: u32,
    /// Encoded frame height in pixels.
    pub height: u32,
    /// Target average bitrate. Encoders that support rate control retarget a
    /// running encoder via [`VideoEncoder::set_bitrate`] rather than rebuilding.
    pub bitrate_bps: u32,
    /// Target frame rate.
    pub fps: u32,
    /// Maximum seconds between keyframes.
    pub keyframe_interval_secs: u32,
}

/// Codecs this build can encode **and** decode.
///
/// Both directions, deliberately: negotiation produces one codec used for the
/// whole session, so advertising a codec we can only decode would let a peer
/// pick something we cannot send.
///
/// This is a *build* capability, not a live probe. Constructing a Media
/// Foundation encoder allocates COM objects and can fail on a machine with no
/// usable MFT, and doing that at startup to answer "what do we advertise" would
/// cost every launch. A runtime failure still soft-fails the camera toggle —
/// what this function exists to prevent is the categorically worse case of
/// advertising a codec this binary has no implementation of at all.
// `vec_init_then_push` fires on non-Windows, where the `cfg` block below is
// empty and the first push therefore follows `Vec::new()` directly. Collapsing
// it into a `vec![]` literal would mean one literal per platform, which is what
// this shape exists to avoid as macOS and its VideoToolbox entry arrive.
#[allow(clippy::vec_init_then_push)]
pub fn available_codecs() -> Vec<VideoCodec> {
    let mut out = Vec::new();
    #[cfg(target_os = "windows")]
    {
        // Media Foundation H.264, using the codec licence held by the OS.
        // First because it is the hardware path where both peers have it.
        out.push(VideoCodec::H264);
    }
    // VP8 is built from vendored libvpx on every platform, so it is always
    // available. That is deliberate rather than incidental: it is what gives a
    // Windows peer and a Linux peer a codec in common.
    out.push(VideoCodec::Vp8);
    out
}

/// Build an encoder for `codec`, or `Err` if this build cannot encode it.
pub fn make_encoder(
    codec: VideoCodec,
    params: EncoderParams,
) -> anyhow::Result<Box<dyn VideoEncoder>> {
    match codec {
        #[cfg(target_os = "windows")]
        VideoCodec::H264 => {
            let enc =
                super::mediafoundation::MfEncoder::new(super::mediafoundation::MfEncoderConfig {
                    width: params.width,
                    height: params.height,
                    bitrate_bps: params.bitrate_bps,
                    fps: params.fps,
                    keyframe_interval_secs: params.keyframe_interval_secs,
                })?;
            Ok(Box::new(enc))
        }
        #[cfg(not(target_os = "windows"))]
        VideoCodec::H264 => anyhow::bail!(
            "H.264 encode needs Media Foundation, which is Windows-only; \
             this build has no H.264 encoder"
        ),
        VideoCodec::Vp8 => Ok(Box::new(Vp8EncoderAdapter(conquerd_vpx::Vp8Encoder::new(
            params.width,
            params.height,
            params.bitrate_bps,
            params.fps,
            params.keyframe_interval_secs,
        )?))),
        VideoCodec::Stub => Ok(Box::new(StubEncoder)),
    }
}

/// Build a decoder for `codec`, or `Err` if this build cannot decode it.
///
/// Called per (sender, codec) rather than once per session: in a room, two
/// members may legitimately send in different codecs, and each needs its own
/// decoder instance anyway because inter frames reference that sender's history.
pub fn make_decoder(codec: VideoCodec) -> anyhow::Result<Box<dyn VideoDecoder>> {
    match codec {
        #[cfg(target_os = "windows")]
        VideoCodec::H264 => Ok(Box::new(super::mediafoundation::MfDecoder::new()?)),
        #[cfg(not(target_os = "windows"))]
        VideoCodec::H264 => anyhow::bail!(
            "H.264 decode needs Media Foundation, which is Windows-only; \
             this build has no H.264 decoder"
        ),
        VideoCodec::Vp8 => Ok(Box::new(
            Vp8DecoderAdapter(conquerd_vpx::Vp8Decoder::new()?),
        )),
        VideoCodec::Stub => Ok(Box::new(StubDecoder)),
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
        assert_eq!(StubDecoder.decode(&encoded).unwrap(), Some(frame));
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
            VideoCodec::Stub,
            0,
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

    fn test_params() -> EncoderParams {
        EncoderParams {
            width: STUB_WIDTH,
            height: STUB_HEIGHT,
            bitrate_bps: 600_000,
            fps: 30,
            keyframe_interval_secs: 4,
        }
    }

    /// The stub must never be advertised, so it must never be in the set the
    /// client hands to capability registration.
    #[test]
    fn available_codecs_never_includes_the_stub() {
        assert!(!available_codecs().contains(&VideoCodec::Stub));
    }

    /// Advertising a codec we cannot construct is the exact dishonesty this
    /// registry exists to prevent, so every advertised codec must build both
    /// halves on this platform.
    #[test]
    fn every_available_codec_can_build_both_halves() {
        for codec in available_codecs() {
            assert!(
                make_encoder(codec, test_params()).is_ok(),
                "{codec:?} is advertised but has no encoder"
            );
            assert!(
                make_decoder(codec).is_ok(),
                "{codec:?} is advertised but has no decoder"
            );
        }
    }

    #[test]
    fn the_stub_is_always_constructible_for_transport_tests() {
        assert!(make_encoder(VideoCodec::Stub, test_params()).is_ok());
        assert!(make_decoder(VideoCodec::Stub).is_ok());
    }

    /// VP8 is what a Linux peer will negotiate, so it has to be present in
    /// every build regardless of platform — not only where an OS codec is
    /// missing.
    #[test]
    fn vp8_is_available_on_every_platform() {
        assert!(
            available_codecs().contains(&VideoCodec::Vp8),
            "VP8 must be built everywhere or cross-platform calls have no mutual codec"
        );
    }

    /// The adapter has to survive a real frame, not just construct: a mismatch
    /// between `RawFrame`'s packing and what libvpx expects would corrupt the
    /// picture rather than error.
    #[test]
    fn vp8_adapter_round_trips_a_real_frame() {
        let params = EncoderParams {
            width: 320,
            height: 240,
            ..test_params()
        };
        let mut enc = make_encoder(VideoCodec::Vp8, params).expect("vp8 encoder");
        let mut dec = make_decoder(VideoCodec::Vp8).expect("vp8 decoder");

        let original = RawFrame::test_pattern(320, 240, 5);
        let (packet, keyframe) = enc.encode(&original).expect("encode");
        assert!(!packet.is_empty() && keyframe);

        let out = dec.decode(&packet).expect("decode").expect("a picture");
        assert_eq!((out.width, out.height), (320, 240));
        assert!(out.is_consistent());
    }

    /// libvpx encodes at the size it was constructed for, so a mismatched
    /// frame must be refused rather than read with the wrong stride.
    #[test]
    fn vp8_adapter_refuses_a_frame_of_the_wrong_size() {
        let params = EncoderParams {
            width: 320,
            height: 240,
            ..test_params()
        };
        let mut enc = make_encoder(VideoCodec::Vp8, params).unwrap();
        assert!(enc.encode(&RawFrame::black(160, 120)).is_err());
    }

    /// On Windows the advertised set must contain H.264, since that is what
    /// the Media Foundation path actually encodes.
    #[cfg(target_os = "windows")]
    #[test]
    fn windows_advertises_h264() {
        assert!(available_codecs().contains(&VideoCodec::H264));
    }
}
