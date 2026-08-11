//! VP8 encode and decode over a vendored libvpx.
//!
//! VP8 is what makes video work anywhere other than Windows. Windows has
//! Media Foundation H.264 and macOS has VideoToolbox, both backed by a codec
//! licence the OS holds; Linux has no equivalent, and shipping our own AVC
//! encoder would put MPEG-LA licensing on this project. VP8 is royalty-free,
//! so it is the codec every platform can carry — which also makes it the one
//! a Windows peer and a Linux peer can agree on.
//!
//! The build compiles libvpx's C without its assembly; see `build.rs` for why
//! and what it costs. Everything crosses into C through `src/shim.c` rather
//! than through libvpx's own structs — also explained there.
//!
//! ```
//! use conquerd_vpx::{Vp8Encoder, Vp8Decoder};
//! let (w, h) = (640usize, 360usize);
//! let y = vec![128u8; w * h];
//! let u = vec![128u8; (w / 2) * (h / 2)];
//! let v = vec![128u8; (w / 2) * (h / 2)];
//!
//! let mut enc = Vp8Encoder::new(w as u32, h as u32, 600_000, 30, 4)?;
//! let (packet, _keyframe) = enc.encode(&y, &u, &v)?;
//!
//! let mut dec = Vp8Decoder::new()?;
//! let frame = dec.decode(&packet)?;
//! assert_eq!((frame.width, frame.height), (640, 360));
//! # Ok::<(), anyhow::Error>(())
//! ```

use std::os::raw::{c_int, c_uchar};

#[allow(non_camel_case_types)]
mod ffi {
    use std::os::raw::{c_int, c_uchar};

    // Opaque: both types are defined in shim.c and never inspected here.
    #[repr(C)]
    pub struct cq_vp8_enc {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct cq_vp8_dec {
        _private: [u8; 0],
    }

    extern "C" {
        pub fn cq_vp8_enc_new(
            width: c_int,
            height: c_int,
            bitrate_bps: c_int,
            fps: c_int,
            keyframe_interval_secs: c_int,
        ) -> *mut cq_vp8_enc;
        pub fn cq_vp8_enc_free(e: *mut cq_vp8_enc);
        pub fn cq_vp8_enc_request_keyframe(e: *mut cq_vp8_enc);
        pub fn cq_vp8_enc_set_bitrate(e: *mut cq_vp8_enc, bitrate_bps: c_int) -> c_int;
        pub fn cq_vp8_enc_encode(
            e: *mut cq_vp8_enc,
            y: *const c_uchar,
            u: *const c_uchar,
            v: *const c_uchar,
            out: *mut c_uchar,
            out_cap: c_int,
            is_keyframe: *mut c_int,
        ) -> c_int;

        pub fn cq_vp8_dec_new() -> *mut cq_vp8_dec;
        pub fn cq_vp8_dec_free(d: *mut cq_vp8_dec);
        pub fn cq_vp8_dec_decode(
            d: *mut cq_vp8_dec,
            data: *const c_uchar,
            len: c_int,
            out: *mut c_uchar,
            out_cap: c_int,
            width: *mut c_int,
            height: *mut c_int,
        ) -> c_int;
    }
}

/// A decoded I420 frame, tightly packed.
///
/// Mirrors the client's `RawFrame` without depending on it — this crate sits
/// below the client and must not reach up into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// Luma, `width * height` bytes.
    pub y: Vec<u8>,
    /// Chroma-blue, `((width+1)/2) * ((height+1)/2)` bytes.
    pub u: Vec<u8>,
    /// Chroma-red, same size as [`Self::u`].
    pub v: Vec<u8>,
}

/// VP8 encoder.
///
/// Not `Sync`: libvpx codec contexts have no internal locking, and the
/// intended use is one encoder owned by the capture thread.
pub struct Vp8Encoder {
    inner: *mut ffi::cq_vp8_enc,
    /// Reused across frames so a steady encode does not allocate per frame.
    scratch: Vec<u8>,
    width: u32,
    height: u32,
}

// SAFETY: the context is owned exclusively by this struct and every call goes
// through `&mut self`, so it can move between threads but never be shared.
unsafe impl Send for Vp8Encoder {}

impl Vp8Encoder {
    /// Create an encoder for `width`x`height` at `fps`, targeting
    /// `bitrate_bps` with at most `keyframe_interval_secs` between keyframes.
    pub fn new(
        width: u32,
        height: u32,
        bitrate_bps: u32,
        fps: u32,
        keyframe_interval_secs: u32,
    ) -> anyhow::Result<Self> {
        // VP8 needs even dimensions for its 4:2:0 chroma planes.
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            anyhow::bail!("VP8 needs non-zero even dimensions, got {width}x{height}");
        }
        // SAFETY: dimensions validated above; the shim returns null on failure.
        let inner = unsafe {
            ffi::cq_vp8_enc_new(
                width as c_int,
                height as c_int,
                bitrate_bps as c_int,
                fps as c_int,
                keyframe_interval_secs as c_int,
            )
        };
        if inner.is_null() {
            anyhow::bail!("libvpx rejected a {width}x{height} VP8 encoder at {bitrate_bps} bps");
        }
        Ok(Self {
            inner,
            // A keyframe is the large case; sizing for an uncompressed frame
            // means the buffer never grows in practice.
            scratch: vec![0u8; (width as usize * height as usize * 3 / 2).max(64 * 1024)],
            width,
            height,
        })
    }

    /// Encoded dimensions, as configured.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Ask for the next encoded frame to be a keyframe.
    pub fn request_keyframe(&mut self) {
        // SAFETY: `inner` is non-null for the lifetime of self.
        unsafe { ffi::cq_vp8_enc_request_keyframe(self.inner) }
    }

    /// Retarget the average bitrate without rebuilding the encoder.
    pub fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()> {
        // SAFETY: `inner` is non-null for the lifetime of self.
        let rc = unsafe { ffi::cq_vp8_enc_set_bitrate(self.inner, bitrate_bps as c_int) };
        if rc != 0 {
            anyhow::bail!("libvpx rejected a bitrate change to {bitrate_bps} bps");
        }
        Ok(())
    }

    /// Encode one tightly-packed I420 frame.
    ///
    /// Returns the encoded packet and whether it is a keyframe. An empty
    /// packet is not an error: under rate control libvpx may legitimately drop
    /// a frame, and the caller should simply not send anything for it.
    pub fn encode(&mut self, y: &[u8], u: &[u8], v: &[u8]) -> anyhow::Result<(Vec<u8>, bool)> {
        let (w, h) = (self.width as usize, self.height as usize);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        // Checked here rather than in C: the shim indexes these planes by row
        // and a short buffer would read out of bounds.
        if y.len() < w * h || u.len() < cw * ch || v.len() < cw * ch {
            anyhow::bail!(
                "frame planes too small for {w}x{h}: y={} u={} v={}",
                y.len(),
                u.len(),
                v.len()
            );
        }

        let mut is_keyframe: c_int = 0;
        loop {
            // SAFETY: planes are length-checked above; `scratch` is a valid
            // writable buffer of `scratch.len()`; `is_keyframe` is a live int.
            let rc = unsafe {
                ffi::cq_vp8_enc_encode(
                    self.inner,
                    y.as_ptr() as *const c_uchar,
                    u.as_ptr() as *const c_uchar,
                    v.as_ptr() as *const c_uchar,
                    self.scratch.as_mut_ptr() as *mut c_uchar,
                    self.scratch.len() as c_int,
                    &mut is_keyframe,
                )
            };
            match rc {
                -2 => {
                    // Output did not fit. Only possible for a pathological
                    // keyframe; grow once and retry rather than failing.
                    let bigger = self.scratch.len() * 2;
                    if bigger > 64 * 1024 * 1024 {
                        anyhow::bail!("VP8 frame exceeded the 64 MB output ceiling");
                    }
                    self.scratch.resize(bigger, 0);
                }
                -1 => anyhow::bail!("libvpx VP8 encode failed"),
                n => return Ok((self.scratch[..n as usize].to_vec(), is_keyframe != 0)),
            }
        }
    }
}

impl Drop for Vp8Encoder {
    fn drop(&mut self) {
        // SAFETY: `inner` was produced by `cq_vp8_enc_new` and is freed once.
        unsafe { ffi::cq_vp8_enc_free(self.inner) }
    }
}

/// VP8 decoder. One per sender — inter frames reference that sender's history.
pub struct Vp8Decoder {
    inner: *mut ffi::cq_vp8_dec,
    scratch: Vec<u8>,
}

// SAFETY: see `Vp8Encoder`.
unsafe impl Send for Vp8Decoder {}

impl Vp8Decoder {
    /// Create a decoder.
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: returns null on failure, checked below.
        let inner = unsafe { ffi::cq_vp8_dec_new() };
        if inner.is_null() {
            anyhow::bail!("could not initialise a libvpx VP8 decoder");
        }
        Ok(Self {
            inner,
            // 640x360 I420; grows on demand for larger streams.
            scratch: vec![0u8; 640 * 360 * 3 / 2],
        })
    }

    /// Decode one VP8 packet into I420.
    pub fn decode(&mut self, data: &[u8]) -> anyhow::Result<DecodedFrame> {
        if data.is_empty() {
            anyhow::bail!("empty VP8 packet");
        }
        let mut width: c_int = 0;
        let mut height: c_int = 0;

        for attempt in 0..2 {
            // SAFETY: `data` is a valid slice; `scratch` is writable for its
            // length; both out-params are live ints.
            let rc = unsafe {
                ffi::cq_vp8_dec_decode(
                    self.inner,
                    data.as_ptr() as *const c_uchar,
                    data.len() as c_int,
                    self.scratch.as_mut_ptr() as *mut c_uchar,
                    self.scratch.len() as c_int,
                    &mut width,
                    &mut height,
                )
            };
            match rc {
                -2 if attempt == 0 => {
                    // The shim reported the real dimensions before refusing, so
                    // the retry is sized exactly rather than guessed.
                    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
                    let needed = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
                    if needed == 0 || needed > 64 * 1024 * 1024 {
                        anyhow::bail!("VP8 stream declared an unusable size {width}x{height}");
                    }
                    self.scratch.resize(needed, 0);
                }
                -2 => anyhow::bail!("VP8 output buffer still too small after resize"),
                -1 => anyhow::bail!("libvpx VP8 decode failed"),
                _ => {
                    let (w, h) = (width as u32, height as u32);
                    let (cw, chh) = (w.div_ceil(2) as usize, h.div_ceil(2) as usize);
                    let y_len = w as usize * h as usize;
                    let c_len = cw * chh;
                    return Ok(DecodedFrame {
                        width: w,
                        height: h,
                        y: self.scratch[..y_len].to_vec(),
                        u: self.scratch[y_len..y_len + c_len].to_vec(),
                        v: self.scratch[y_len + c_len..y_len + 2 * c_len].to_vec(),
                    });
                }
            }
        }
        unreachable!("the loop returns or bails on every path")
    }
}

impl Drop for Vp8Decoder {
    fn drop(&mut self) {
        // SAFETY: `inner` was produced by `cq_vp8_dec_new` and is freed once.
        unsafe { ffi::cq_vp8_dec_free(self.inner) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 320;
    const H: u32 = 240;

    /// A deterministic pattern with real spatial detail, so a codec that
    /// silently produced a flat frame would not pass.
    fn test_frame(seed: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (w, h) = (W as usize, H as usize);
        let (cw, ch) = (w / 2, h / 2);
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            for col in 0..w {
                y[row * w + col] = ((row * 3 + col * 5) as u8).wrapping_add(seed);
            }
        }
        let u = vec![100u8.wrapping_add(seed); cw * ch];
        let v = vec![160u8.wrapping_add(seed); cw * ch];
        (y, u, v)
    }

    #[test]
    fn encoder_rejects_odd_or_zero_dimensions() {
        assert!(Vp8Encoder::new(0, 240, 600_000, 30, 4).is_err());
        assert!(Vp8Encoder::new(321, 240, 600_000, 30, 4).is_err());
        assert!(Vp8Encoder::new(320, 0, 600_000, 30, 4).is_err());
    }

    #[test]
    fn encoder_rejects_short_planes() {
        let mut enc = Vp8Encoder::new(W, H, 600_000, 30, 4).unwrap();
        let (y, u, v) = test_frame(0);
        assert!(enc.encode(&y[..y.len() - 1], &u, &v).is_err());
        assert!(enc.encode(&y, &u[..u.len() - 1], &v).is_err());
    }

    /// The property that matters: real bytes in, a decodable frame out, at the
    /// size we asked for. Without this the whole cross-platform story rests on
    /// a codec nobody has run.
    #[test]
    fn encode_decode_round_trip() {
        let mut enc = Vp8Encoder::new(W, H, 600_000, 30, 4).unwrap();
        let mut dec = Vp8Decoder::new().unwrap();
        let (y, u, v) = test_frame(7);

        let (packet, keyframe) = enc.encode(&y, &u, &v).unwrap();
        assert!(!packet.is_empty(), "first frame must produce output");
        assert!(keyframe, "the first encoded frame is always a keyframe");

        let out = dec.decode(&packet).unwrap();
        assert_eq!((out.width, out.height), (W, H));
        assert_eq!(out.y.len(), (W * H) as usize);
        assert_eq!(out.u.len(), ((W / 2) * (H / 2)) as usize);
        assert_eq!(out.v.len(), out.u.len());
    }

    /// Lossy, so exact equality is the wrong test — but a codec that returned
    /// garbage would not land anywhere near the input.
    #[test]
    fn decoded_luma_resembles_the_input() {
        let mut enc = Vp8Encoder::new(W, H, 2_000_000, 30, 4).unwrap();
        let mut dec = Vp8Decoder::new().unwrap();
        let (y, u, v) = test_frame(0);

        let (packet, _) = enc.encode(&y, &u, &v).unwrap();
        let out = dec.decode(&packet).unwrap();

        let total: u64 = out
            .y
            .iter()
            .zip(y.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs() as u64)
            .sum();
        let mean = total / out.y.len() as u64;
        assert!(
            mean < 24,
            "mean luma error {mean} is too high to be a decode"
        );
    }

    /// Compression has to actually happen, or the fragmenter's frame-size
    /// assumptions (and the relay quota) are wrong.
    #[test]
    fn output_is_much_smaller_than_the_raw_frame() {
        let mut enc = Vp8Encoder::new(W, H, 600_000, 30, 4).unwrap();
        let (y, u, v) = test_frame(3);
        let (packet, _) = enc.encode(&y, &u, &v).unwrap();
        let raw = (W * H) as usize * 3 / 2;
        assert!(
            packet.len() < raw / 2,
            "encoded {} bytes vs {raw} raw — that is not compression",
            packet.len()
        );
    }

    /// Inter frames must reference the sender's history, so a decoder fed the
    /// stream in order tracks it; this also covers the multi-frame path where
    /// the encoder may drop or delay output.
    #[test]
    fn a_sequence_of_frames_decodes_in_order() {
        let mut enc = Vp8Encoder::new(W, H, 600_000, 30, 4).unwrap();
        let mut dec = Vp8Decoder::new().unwrap();
        let mut decoded = 0;
        for seed in 0..10u8 {
            let (y, u, v) = test_frame(seed);
            let (packet, _) = enc.encode(&y, &u, &v).unwrap();
            if packet.is_empty() {
                continue; // legitimately dropped under rate control
            }
            let out = dec.decode(&packet).unwrap();
            assert_eq!((out.width, out.height), (W, H));
            decoded += 1;
        }
        assert!(decoded >= 5, "only {decoded} of 10 frames decoded");
    }

    #[test]
    fn requested_keyframe_is_produced() {
        let mut enc = Vp8Encoder::new(W, H, 600_000, 30, 4).unwrap();
        let (y, u, v) = test_frame(1);
        // Frame 1 is a keyframe on its own; get past it first.
        let _ = enc.encode(&y, &u, &v).unwrap();
        let (y2, u2, v2) = test_frame(2);
        let _ = enc.encode(&y2, &u2, &v2).unwrap();

        enc.request_keyframe();
        let (y3, u3, v3) = test_frame(3);
        let (packet, keyframe) = enc.encode(&y3, &u3, &v3).unwrap();
        assert!(!packet.is_empty());
        assert!(
            keyframe,
            "an explicitly requested keyframe must be produced"
        );
    }

    #[test]
    fn bitrate_can_be_retargeted_on_a_running_encoder() {
        let mut enc = Vp8Encoder::new(W, H, 600_000, 30, 4).unwrap();
        let (y, u, v) = test_frame(4);
        let _ = enc.encode(&y, &u, &v).unwrap();
        enc.set_bitrate(200_000)
            .expect("retargeting a live encoder must not fail");
        let (packet, _) = enc.encode(&y, &u, &v).unwrap();
        let _ = packet;
    }

    #[test]
    fn decoder_rejects_garbage() {
        let mut dec = Vp8Decoder::new().unwrap();
        assert!(dec.decode(&[]).is_err());
        assert!(dec.decode(&[0xde, 0xad, 0xbe, 0xef, 0x00, 0x11]).is_err());
    }
}
