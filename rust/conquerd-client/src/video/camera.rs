//! Camera capture.
//!
//! Everything above this module speaks [`RawFrame`] (tightly-packed I420), so
//! the backend's job is to open a device, negotiate a format, and convert
//! whatever the hardware actually delivers.
//!
//! # Backend choice
//!
//! Windows uses Media Foundation's `IMFSourceReader` — the same stack the
//! codec already initialises, so capture adds no new dependency and no new
//! build system. Webcams commonly offer NV12 or YUY2, both of which
//! [`super::nv12`] converts.
//!
//! Linux uses Video4Linux2 through the `v4l` crate. Both backends converge on
//! the same small set of pixel formats and reuse the same converters, because
//! the format a webcam offers is a property of the camera rather than of the
//! operating system — a YUY2-only device behaves the same either side.
//!
//! Platforms without a backend get [`NullCamera`], which reports no devices
//! rather than failing to compile, so their builds stay green and the camera
//! toggle simply reports off.

use super::frame::RawFrame;

/// A camera the user can select.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDevice {
    /// Stable identifier used to reopen this device.
    pub id: String,
    /// Human-readable name for the settings UI.
    pub name: String,
}

/// A capture backend.
///
/// `Send` but not `Sync`: capture handles have thread affinity, and the
/// intended use is one owned by a dedicated capture thread.
pub trait CameraSource: Send {
    /// Pull the next frame, converting to I420.
    ///
    /// Blocks until a frame is available or the device errors.
    fn next_frame(&mut self) -> anyhow::Result<RawFrame>;

    /// Negotiated capture size, which may differ from what was requested —
    /// cameras routinely substitute the nearest mode they support.
    fn dimensions(&self) -> (u32, u32);
}

/// Placeholder backend for platforms without a capture implementation yet.
pub struct NullCamera;

impl CameraSource for NullCamera {
    fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
        anyhow::bail!("camera capture is not implemented on this platform")
    }

    fn dimensions(&self) -> (u32, u32) {
        (0, 0)
    }
}

#[cfg(target_os = "windows")]
pub use windows_impl::{list_devices, MfCamera};

#[cfg(target_os = "linux")]
pub use linux_impl::{list_devices, V4l2Camera};

#[cfg(target_os = "macos")]
pub use macos_impl::{list_devices, AvfCamera};

// Keep the Objective-C boundary reachable when another host cross-lints it.
// The aliases avoid colliding with that host's native camera exports.
#[cfg(all(feature = "lint-macos", not(target_os = "macos")))]
#[doc(hidden)]
pub use macos_impl::{list_devices as lint_macos_list_devices, AvfCamera as LintMacosCamera};

/// Enumerate cameras. Always empty where capture is unimplemented.
#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
pub fn list_devices() -> Vec<CameraDevice> {
    Vec::new()
}

/// Id of the device an empty `video_input_device` setting opens.
///
/// The empty selection means "first available camera", which
/// [`MfCamera::open(None, ..)`](MfCamera::open) resolves to `list_devices()[0]`
/// — so this must stay the *same* choice, not merely a plausible one. Callers
/// use it to tell whether some other id names that same device; an empty string
/// means there is nothing to compare against.
pub fn default_device_id() -> String {
    list_devices()
        .first()
        .map(|d| d.id.clone())
        .unwrap_or_default()
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{CameraDevice, CameraSource};
    use crate::video::frame::RawFrame;
    use crate::video::nv12;

    use windows::core::PWSTR;
    use windows::Win32::Media::MediaFoundation::*;

    /// Enumerate video capture devices.
    pub fn list_devices() -> Vec<CameraDevice> {
        super::super::mediafoundation::ensure_started();
        let mut out = Vec::new();

        // SAFETY: the attribute store and returned array are released below.
        unsafe {
            let Ok(attrs) = create_attributes(1) else {
                return out;
            };
            if attrs
                .SetGUID(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
                )
                .is_err()
            {
                return out;
            }

            let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count: u32 = 0;
            if MFEnumDeviceSources(&attrs, &mut activates, &mut count).is_err()
                || activates.is_null()
            {
                return out;
            }

            let slice = std::slice::from_raw_parts(activates, count as usize);
            for activate in slice.iter().flatten() {
                let name = read_string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
                    .unwrap_or_else(|| "Camera".to_string());
                let id = read_string(
                    activate,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                )
                .unwrap_or_else(|| name.clone());
                out.push(CameraDevice { id, name });
            }
            windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
        }
        out
    }

    /// Read a string attribute, returning `None` when absent.
    unsafe fn read_string(activate: &IMFActivate, key: &windows::core::GUID) -> Option<String> {
        let mut ptr = PWSTR::null();
        let mut len = 0u32;
        activate.GetAllocatedString(key, &mut ptr, &mut len).ok()?;
        if ptr.is_null() {
            return None;
        }
        let s = ptr.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(ptr.0 as *const _));
        s
    }

    fn create_attributes(count: u32) -> windows::core::Result<IMFAttributes> {
        let mut attrs: Option<IMFAttributes> = None;
        // SAFETY: out-param is initialised by the call.
        unsafe { MFCreateAttributes(&mut attrs, count)? };
        attrs.ok_or_else(windows::core::Error::from_win32)
    }

    /// Media Foundation camera capture.
    pub struct MfCamera {
        reader: IMFSourceReader,
        width: u32,
        height: u32,
        stride: usize,
        format: CaptureFormat,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum CaptureFormat {
        Nv12,
        Yuy2,
    }

    // SAFETY: the reader is exclusively owned and never shared across threads.
    unsafe impl Send for MfCamera {}

    impl MfCamera {
        /// Open a camera by device id, or the first available when `None`.
        pub fn open(device_id: Option<&str>, width: u32, height: u32) -> anyhow::Result<Self> {
            super::super::mediafoundation::ensure_started();

            let devices = list_devices();
            if devices.is_empty() {
                anyhow::bail!("no video capture devices found");
            }
            let chosen = match device_id {
                Some(id) => devices
                    .iter()
                    .find(|d| d.id == id || d.name == id)
                    .ok_or_else(|| anyhow::anyhow!("camera '{id}' not found"))?,
                None => &devices[0],
            };

            // SAFETY: each COM call is checked; handles are released on drop.
            let reader = unsafe {
                let attrs = create_attributes(2)?;
                attrs.SetGUID(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
                )?;
                attrs.SetString(
                    &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
                    &windows::core::HSTRING::from(chosen.id.as_str()),
                )?;

                let source: IMFMediaSource = {
                    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
                    let mut count = 0u32;
                    MFEnumDeviceSources(&attrs, &mut activates, &mut count)?;
                    if activates.is_null() || count == 0 {
                        anyhow::bail!("camera '{}' could not be activated", chosen.name);
                    }
                    let slice = std::slice::from_raw_parts(activates, count as usize);
                    let activated = slice
                        .iter()
                        .flatten()
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("camera activation returned nothing"))?
                        .ActivateObject::<IMFMediaSource>();
                    windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
                    activated?
                };

                MFCreateSourceReaderFromMediaSource(&source, None)?
            };

            let (width, height, stride, format) = Self::negotiate(&reader, width, height)?;

            Ok(Self {
                reader,
                width,
                height,
                stride,
                format,
            })
        }

        /// Choose a capture format, preferring NV12 then YUY2.
        ///
        /// Requesting an exact size is advisory: cameras substitute the nearest
        /// mode they support, so the *negotiated* type is read back rather than
        /// assumed. Trusting the request is how you end up converting a
        /// 1280x720 buffer as though it were 640x360.
        fn negotiate(
            reader: &IMFSourceReader,
            want_w: u32,
            want_h: u32,
        ) -> anyhow::Result<(u32, u32, usize, CaptureFormat)> {
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

            // Each format is tried at the requested size first, then with no
            // size constraint at all. The second pass matters: a camera exposes
            // a fixed set of modes, and `SetCurrentMediaType` fails outright
            // for a size none of them match rather than picking the nearest.
            // Without the fallback, any caller asking for an unusual size — an
            // overlay inset, say — gets "offers neither NV12 nor YUY2", which
            // reads like a broken webcam rather than an unavailable mode.
            //
            // The negotiated size is read back below either way, so accepting
            // the device's own choice is safe: nothing downstream assumes the
            // request was honoured.
            for (subtype, format, constrain_size) in [
                (MFVideoFormat_NV12, CaptureFormat::Nv12, true),
                (MFVideoFormat_YUY2, CaptureFormat::Yuy2, true),
                (MFVideoFormat_NV12, CaptureFormat::Nv12, false),
                (MFVideoFormat_YUY2, CaptureFormat::Yuy2, false),
            ] {
                // SAFETY: building a media type and applying it to the reader.
                let applied = unsafe {
                    let Ok(mt) = MFCreateMediaType() else {
                        continue;
                    };
                    if mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).is_err()
                        || mt.SetGUID(&MF_MT_SUBTYPE, &subtype).is_err()
                    {
                        continue;
                    }
                    if constrain_size
                        && mt
                            .SetUINT64(&MF_MT_FRAME_SIZE, ((want_w as u64) << 32) | want_h as u64)
                            .is_err()
                    {
                        continue;
                    }
                    reader.SetCurrentMediaType(stream, None, &mt).is_ok()
                };
                if !applied {
                    continue;
                }

                // SAFETY: reading back what the device actually agreed to.
                unsafe {
                    let Ok(current) = reader.GetCurrentMediaType(stream) else {
                        continue;
                    };
                    let Ok(size) = current.GetUINT64(&MF_MT_FRAME_SIZE) else {
                        continue;
                    };
                    let w = (size >> 32) as u32;
                    let h = (size & 0xFFFF_FFFF) as u32;
                    if w == 0 || h == 0 {
                        continue;
                    }
                    let default_stride = match format {
                        CaptureFormat::Nv12 => w as usize,
                        CaptureFormat::Yuy2 => w as usize * 2,
                    };
                    let stride = current
                        .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                        .ok()
                        .map(|s| s as i32)
                        .filter(|s| *s > 0)
                        .map(|s| s as usize)
                        .unwrap_or(default_stride);
                    return Ok((w, h, stride, format));
                }
            }

            anyhow::bail!("camera offers neither NV12 nor YUY2")
        }
    }

    impl CameraSource for MfCamera {
        fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

            // SAFETY: synchronous read; the sample and its buffer are released
            // when they drop at the end of this scope.
            unsafe {
                let mut flags = 0u32;
                let mut timestamp = 0i64;
                let mut sample: Option<IMFSample> = None;
                self.reader
                    .ReadSample(
                        stream,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| anyhow::anyhow!("ReadSample: {e}"))?;

                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    anyhow::bail!("camera stream ended");
                }
                // A null sample with no error is normal: the reader signals
                // "nothing yet" rather than blocking indefinitely.
                let Some(sample) = sample else {
                    anyhow::bail!("no frame available yet");
                };

                let buffer = sample
                    .ConvertToContiguousBuffer()
                    .map_err(|e| anyhow::anyhow!("ConvertToContiguousBuffer: {e}"))?;

                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut max_len = 0u32;
                let mut cur_len = 0u32;
                buffer
                    .Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
                    .map_err(|e| anyhow::anyhow!("buffer Lock: {e}"))?;
                if ptr.is_null() || cur_len == 0 {
                    let _ = buffer.Unlock();
                    anyhow::bail!("camera returned an empty buffer");
                }
                let data = std::slice::from_raw_parts(ptr, cur_len as usize).to_vec();
                let _ = buffer.Unlock();

                let converted = match self.format {
                    CaptureFormat::Nv12 => nv12::nv12_to_i420(
                        &data,
                        self.stride,
                        self.stride * self.height as usize,
                        self.width,
                        self.height,
                    ),
                    CaptureFormat::Yuy2 => {
                        nv12::yuy2_to_i420(&data, self.stride, self.width, self.height)
                    }
                };

                let (y, u, v) = converted.ok_or_else(|| {
                    anyhow::anyhow!(
                        "conversion rejected a {} buffer of {} bytes ({}x{} stride {})",
                        match self.format {
                            CaptureFormat::Nv12 => "NV12",
                            CaptureFormat::Yuy2 => "YUY2",
                        },
                        data.len(),
                        self.width,
                        self.height,
                        self.stride
                    )
                })?;

                Ok(RawFrame {
                    width: self.width,
                    height: self.height,
                    y,
                    u,
                    v,
                })
            }
        }

        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    use super::{CameraDevice, CameraSource};
    use crate::video::frame::RawFrame;
    use crate::video::nv12;

    use v4l::buffer::Type;
    use v4l::io::traits::CaptureStream;
    use v4l::video::Capture;
    use v4l::{Device, FourCC};

    /// Pixel formats accepted from a device, most preferred first.
    ///
    /// All three are uncompressed and convert to I420 with plain byte work.
    /// MJPEG is deliberately absent even though many webcams offer it at
    /// higher resolutions: decoding it needs a JPEG decoder, which is a new
    /// dependency and a new attack surface on the capture path, and every
    /// device that offers MJPEG also offers YUYV at call resolutions.
    const PREFERRED: [&[u8; 4]; 3] = [
        b"YU12", // planar I420 — no conversion at all
        b"NV12", // semi-planar, converts with nv12_to_i420
        b"YUYV", // packed 4:2:2, the near-universal webcam fallback
    ];

    /// Pick the best mutually-supported pixel format, or `None` when the
    /// device offers nothing usable.
    ///
    /// Split out from [`V4l2Camera::open`] so it can be tested without a
    /// camera: this is the decision most likely to be wrong on hardware that
    /// is not to hand, and getting it wrong means every frame is converted by
    /// the wrong routine rather than failing outright.
    pub(super) fn choose_format(offered: &[[u8; 4]]) -> Option<[u8; 4]> {
        PREFERRED
            .iter()
            .find(|p| offered.contains(&***p))
            .map(|p| **p)
    }

    /// Enumerate V4L2 capture devices.
    pub fn list_devices() -> Vec<CameraDevice> {
        let mut out = Vec::new();
        for node in v4l::context::enum_devices() {
            let Some(path) = node.path().to_str() else {
                continue;
            };
            // A node existing does not make it a capture device: V4L2 also
            // exposes metadata and output nodes, and modern UVC cameras
            // publish several nodes per physical camera. Opening and asking
            // is the only reliable filter.
            let Ok(dev) = Device::with_path(node.path()) else {
                continue;
            };
            if Capture::enum_formats(&dev)
                .map(|f| f.is_empty())
                .unwrap_or(true)
            {
                continue;
            }
            let name = node.name().unwrap_or_else(|| path.to_owned());
            out.push(CameraDevice {
                id: path.to_owned(),
                name,
            });
        }
        out
    }

    /// A V4L2 capture device streaming memory-mapped buffers.
    pub struct V4l2Camera {
        // Declared before `device`: the stream borrows the device's fd
        // internally, so it must be torn down first.
        stream: v4l::io::mmap::Stream<'static>,
        _device: Box<Device>,
        fourcc: [u8; 4],
        width: u32,
        height: u32,
        /// Row stride the driver negotiated, which is not always `width`.
        stride: usize,
    }

    impl V4l2Camera {
        /// Open `device_id` (a `/dev/videoN` path) or the first camera found,
        /// asking for `width`x`height`.
        pub fn open(device_id: Option<&str>, width: u32, height: u32) -> anyhow::Result<Self> {
            let devices = list_devices();
            if devices.is_empty() {
                anyhow::bail!("no video capture devices found");
            }
            let chosen = match device_id {
                Some(id) => devices
                    .iter()
                    .find(|d| d.id == id || d.name == id)
                    .ok_or_else(|| anyhow::anyhow!("camera '{id}' not found"))?,
                None => &devices[0],
            };

            let device = Box::new(Device::with_path(&chosen.id)?);

            // Pick the best format the device actually offers rather than
            // asking for one and hoping: V4L2 substitutes silently, so a
            // request for NV12 on a YUYV-only camera comes back as YUYV and
            // the frames would be converted with the wrong routine.
            let offered: Vec<[u8; 4]> = Capture::enum_formats(&*device)?
                .into_iter()
                .map(|f| f.fourcc.repr)
                .collect();
            let wanted = choose_format(&offered).ok_or_else(|| {
                anyhow::anyhow!(
                    "camera '{}' offers no supported pixel format (has {:?})",
                    chosen.name,
                    offered
                        .iter()
                        .map(|f| String::from_utf8_lossy(f).to_string())
                        .collect::<Vec<_>>()
                )
            })?;

            let mut fmt = Capture::format(&*device)?;
            fmt.width = width;
            fmt.height = height;
            fmt.fourcc = FourCC::new(&wanted);
            // The driver returns what it actually set, which may differ in
            // both size and format from what was asked.
            let fmt = Capture::set_format(&*device, &fmt)?;

            let fourcc = fmt.fourcc.repr;
            if choose_format(&[fourcc]).is_none() {
                anyhow::bail!(
                    "camera '{}' substituted unsupported format {}",
                    chosen.name,
                    String::from_utf8_lossy(&fourcc)
                );
            }
            if fmt.width == 0 || fmt.height == 0 || fmt.width % 2 != 0 || fmt.height % 2 != 0 {
                // Odd dimensions have no valid 4:2:0 chroma plane.
                anyhow::bail!(
                    "camera negotiated an unusable size {}x{}",
                    fmt.width,
                    fmt.height
                );
            }

            // SAFETY: a self-referential struct, sound on three counts.
            //
            // 1. The `Device` lives behind a `Box`, so its address is stable
            //    even when `V4l2Camera` itself is moved.
            // 2. `stream` is declared *before* `_device` in the struct, and
            //    Rust drops fields in declaration order, so the stream is torn
            //    down while the device it borrows is still alive.
            // 3. Neither field is ever handed out, so no caller can separate
            //    them or move the device out.
            //
            // The alternative — reopening the device per frame, or an Rc — is
            // either a syscall on the hot path or a refcount for a pair that
            // is created and destroyed together anyway.
            let device_ref: &'static Device = unsafe { &*(&*device as *const Device) };
            // Four buffers: enough to absorb a scheduling hiccup without
            // adding a frame of latency the way a deep queue would.
            let stream = v4l::io::mmap::Stream::with_buffers(device_ref, Type::VideoCapture, 4)?;

            Ok(Self {
                stream,
                _device: device,
                fourcc,
                width: fmt.width,
                height: fmt.height,
                stride: fmt.stride as usize,
            })
        }
    }

    impl CameraSource for V4l2Camera {
        fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
            let (buf, meta) = self.stream.next()?;
            // A short buffer means a truncated frame; converting it would read
            // past the end or produce a torn picture.
            let payload = buf.get(..meta.bytesused as usize).unwrap_or(buf);

            let (w, h) = (self.width, self.height);
            let (y, u, v) = match &self.fourcc {
                b"YU12" => {
                    // Already I420. Still copied per plane, because the
                    // driver's rows may be padded to `stride`.
                    let (wu, hu) = (w as usize, h as usize);
                    let (cw, ch) = (wu / 2, hu / 2);
                    if payload.len() < self.stride * hu + 2 * (self.stride / 2) * ch {
                        anyhow::bail!("short I420 buffer: {} bytes", payload.len());
                    }
                    let mut y = Vec::with_capacity(wu * hu);
                    for r in 0..hu {
                        y.extend_from_slice(&payload[r * self.stride..r * self.stride + wu]);
                    }
                    let cstride = self.stride / 2;
                    let ubase = self.stride * hu;
                    let vbase = ubase + cstride * ch;
                    let mut uo = Vec::with_capacity(cw * ch);
                    let mut vo = Vec::with_capacity(cw * ch);
                    for r in 0..ch {
                        uo.extend_from_slice(
                            &payload[ubase + r * cstride..ubase + r * cstride + cw],
                        );
                        vo.extend_from_slice(
                            &payload[vbase + r * cstride..vbase + r * cstride + cw],
                        );
                    }
                    (y, uo, vo)
                }
                b"NV12" => nv12::nv12_to_i420(payload, self.stride, self.stride * h as usize, w, h)
                    .ok_or_else(|| anyhow::anyhow!("NV12 buffer too small for {w}x{h}"))?,
                b"YUYV" => nv12::yuy2_to_i420(payload, self.stride, w, h)
                    .ok_or_else(|| anyhow::anyhow!("YUY2 buffer too small for {w}x{h}"))?,
                other => anyhow::bail!("unsupported format {}", String::from_utf8_lossy(other)),
            };

            Ok(RawFrame {
                width: w,
                height: h,
                y,
                u,
                v,
            })
        }

        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }
    }
}

// Also compiled under `lint-macos` so a non-macOS host can type-check and lint
// this module; see that feature's comment in Cargo.toml. Dead there, because
// the re-export above stays macOS-only.
#[cfg(any(target_os = "macos", feature = "lint-macos"))]
mod macos_impl {
    use super::{CameraDevice, CameraSource};
    use crate::video::frame::RawFrame;

    use std::os::raw::{c_char, c_int, c_uchar};

    /// Matches the `CQ_CAM_*` codes in `macos_camera.m`.
    const CQ_CAM_TIMEOUT: c_int = -2;
    const CQ_CAM_TOO_SMALL: c_int = -3;

    /// How long a single `next_frame` waits before reporting a stall.
    ///
    /// Long enough to cover a session still starting up (AVFoundation takes a
    /// beat to deliver the first frame, and the TCC prompt may be on screen),
    /// short enough that an unplugged camera surfaces rather than wedging the
    /// capture thread.
    const FRAME_TIMEOUT_MS: c_int = 2_000;

    /// The handle is an Objective-C object bridged out as an opaque pointer.
    /// It is deliberately *not* a C struct: under ARC, Objective-C pointers in
    /// a malloc'd struct make it non-trivial, and `free` would skip the
    /// destructors — see `macos_camera.m`.
    type Handle = std::ffi::c_void;

    extern "C" {
        fn cq_mac_cam_list(buf: *mut c_char, cap: c_int) -> c_int;
        fn cq_mac_cam_open(device_id: *const c_char, width: c_int, height: c_int) -> *mut Handle;
        fn cq_mac_cam_free(handle: *mut Handle);
        fn cq_mac_cam_next_frame(
            handle: *mut Handle,
            out: *mut c_uchar,
            cap: c_int,
            width: *mut c_int,
            height: *mut c_int,
            timeout_ms: c_int,
        ) -> c_int;
    }

    /// Enumerate AVFoundation capture devices.
    pub fn list_devices() -> Vec<CameraDevice> {
        // The shim writes NUL-separated id/name pairs and reports the needed
        // size as a negative when the buffer is short, so one retry with the
        // exact size always suffices.
        let mut cap: usize = 4096;
        for _ in 0..2 {
            let mut buf = vec![0u8; cap];
            // SAFETY: `buf` is writable for `cap` bytes; the shim never writes
            // past the cap it is given.
            let rc = unsafe { cq_mac_cam_list(buf.as_mut_ptr() as *mut c_char, cap as c_int) };
            if rc < 0 {
                cap = rc.unsigned_abs() as usize;
                continue;
            }
            let mut out = Vec::with_capacity(rc as usize);
            let mut parts = buf.split(|b| *b == 0);
            for _ in 0..rc {
                let (Some(id), Some(name)) = (parts.next(), parts.next()) else {
                    break;
                };
                out.push(CameraDevice {
                    id: String::from_utf8_lossy(id).into_owned(),
                    name: String::from_utf8_lossy(name).into_owned(),
                });
            }
            return out;
        }
        Vec::new()
    }

    /// AVFoundation camera capture.
    pub struct AvfCamera {
        inner: *mut Handle,
        scratch: Vec<u8>,
        width: u32,
        height: u32,
    }

    // SAFETY: the handle is owned exclusively and every call takes `&mut self`.
    // The Objective-C side does its own locking around the frame slot.
    unsafe impl Send for AvfCamera {}

    impl AvfCamera {
        /// Open `device_id` (an AVFoundation unique id) or the first camera.
        pub fn open(device_id: Option<&str>, width: u32, height: u32) -> anyhow::Result<Self> {
            let devices = list_devices();
            if devices.is_empty() {
                // Also the symptom of a denied camera permission: macOS hides
                // devices from an app without TCC consent rather than failing
                // the open, so say so instead of only "no camera".
                anyhow::bail!(
                    "no video capture devices found — if a camera is attached, \
                     check Privacy & Security > Camera"
                );
            }
            let chosen = match device_id {
                Some(id) => devices
                    .iter()
                    .find(|d| d.id == id || d.name == id)
                    .ok_or_else(|| anyhow::anyhow!("camera '{id}' not found"))?,
                None => &devices[0],
            };

            let c_id = std::ffi::CString::new(chosen.id.as_str())?;
            // SAFETY: `c_id` is a valid NUL-terminated string that outlives the
            // call; the shim returns null on failure.
            let inner = unsafe { cq_mac_cam_open(c_id.as_ptr(), width as c_int, height as c_int) };
            if inner.is_null() {
                anyhow::bail!("could not start capture on camera '{}'", chosen.name);
            }

            let mut cam = Self {
                inner,
                // Sized for the request; grows if the session delivers larger.
                scratch: vec![0u8; RawFrame::packed_len(width.max(2), height.max(2))],
                width: 0,
                height: 0,
            };

            // Pull one frame to learn the size the session actually produces —
            // AVFoundation presets are coarse and the delegate reports what
            // arrives, so this is the only way to know.
            let first = cam.next_frame()?;
            cam.width = first.width;
            cam.height = first.height;
            Ok(cam)
        }
    }

    impl CameraSource for AvfCamera {
        fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
            let mut w: c_int = 0;
            let mut h: c_int = 0;
            for _ in 0..2 {
                // SAFETY: `scratch` is writable for its length; both out-params
                // are live ints; `inner` is non-null for the lifetime of self.
                let rc = unsafe {
                    cq_mac_cam_next_frame(
                        self.inner,
                        self.scratch.as_mut_ptr() as *mut c_uchar,
                        self.scratch.len() as c_int,
                        &mut w,
                        &mut h,
                        FRAME_TIMEOUT_MS,
                    )
                };
                match rc {
                    CQ_CAM_TOO_SMALL => {
                        // The session is producing a larger frame than the
                        // requested preset; resize once and retry.
                        let (nw, nh) = (w.max(2) as u32, h.max(2) as u32);
                        self.scratch = vec![0u8; RawFrame::packed_len(nw, nh)];
                    }
                    CQ_CAM_TIMEOUT => {
                        anyhow::bail!("camera delivered no frame within {FRAME_TIMEOUT_MS}ms")
                    }
                    n if n < 0 => anyhow::bail!("camera capture stopped"),
                    n => {
                        let (uw, uh) = (w as u32, h as u32);
                        let (cw, ch) = ((uw as usize).div_ceil(2), (uh as usize).div_ceil(2));
                        let y_len = uw as usize * uh as usize;
                        let c_len = cw * ch;
                        if n as usize != y_len + 2 * c_len {
                            anyhow::bail!("camera returned {n} bytes, expected I420 for {uw}x{uh}");
                        }
                        return Ok(RawFrame {
                            width: uw,
                            height: uh,
                            y: self.scratch[..y_len].to_vec(),
                            u: self.scratch[y_len..y_len + c_len].to_vec(),
                            v: self.scratch[y_len + c_len..y_len + 2 * c_len].to_vec(),
                        });
                    }
                }
            }
            anyhow::bail!("camera frame did not fit after a resize")
        }

        fn dimensions(&self) -> (u32, u32) {
            (self.width, self.height)
        }
    }

    impl Drop for AvfCamera {
        fn drop(&mut self) {
            // SAFETY: `inner` came from `cq_mac_cam_open` and is freed once.
            unsafe { cq_mac_cam_free(self.inner) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_camera_reports_no_frames() {
        let mut cam = NullCamera;
        assert!(cam.next_frame().is_err());
        assert_eq!(cam.dimensions(), (0, 0));
    }

    /// Enumeration must be safe to call even with no camera attached — the
    /// settings UI calls it unconditionally.
    #[test]
    fn listing_devices_never_panics() {
        let devices = list_devices();
        for d in &devices {
            assert!(!d.id.is_empty(), "device id must be usable for reopening");
        }
    }

    /// Format preference on Linux. Tested without a camera because the
    /// consequence of getting it wrong is not a failure but a picture
    /// converted by the wrong routine.
    #[cfg(target_os = "linux")]
    mod v4l2_format_choice {
        use crate::video::camera::linux_impl::choose_format;

        #[test]
        fn planar_i420_wins_when_offered() {
            // YU12 needs no conversion at all, so it beats both others.
            assert_eq!(
                choose_format(&[*b"YUYV", *b"NV12", *b"YU12"]),
                Some(*b"YU12")
            );
        }

        #[test]
        fn nv12_beats_yuyv() {
            // NV12 is a plane copy; YUY2 needs vertical chroma averaging.
            assert_eq!(choose_format(&[*b"YUYV", *b"NV12"]), Some(*b"NV12"));
        }

        #[test]
        fn yuyv_is_the_fallback_every_webcam_has() {
            assert_eq!(choose_format(&[*b"YUYV"]), Some(*b"YUYV"));
        }

        /// MJPEG-only devices must be refused rather than accepted and then
        /// fed to a converter that would read compressed bytes as luma.
        #[test]
        fn compressed_only_devices_are_refused() {
            assert_eq!(choose_format(&[*b"MJPG"]), None);
            assert_eq!(choose_format(&[*b"H264", *b"MJPG"]), None);
            assert_eq!(choose_format(&[]), None);
        }

        /// A device offering both compressed and raw formats must land on the
        /// raw one rather than the first entry it advertises.
        #[test]
        fn a_raw_format_is_chosen_past_compressed_ones() {
            assert_eq!(choose_format(&[*b"MJPG", *b"YUYV"]), Some(*b"YUYV"));
        }
    }

    /// Opens the real default camera. Ignored: CI has no webcam, and on a
    /// workstation it turns on the capture light.
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "requires a physical camera"]
    fn captures_a_frame_from_the_default_camera() {
        let devices = list_devices();
        println!("cameras: {devices:?}");
        assert!(!devices.is_empty(), "no camera attached");

        let mut cam = MfCamera::open(None, 640, 360).expect("open camera");
        let (w, h) = cam.dimensions();
        println!("negotiated {w}x{h}");

        // The first reads often return "nothing yet" while the device starts.
        let mut frame = None;
        for _ in 0..60 {
            if let Ok(f) = cam.next_frame() {
                frame = Some(f);
                break;
            }
        }
        let frame = frame.expect("camera produced no frame");
        assert!(frame.is_consistent());
        assert_eq!((frame.width, frame.height), (w, h));
    }

    /// The Linux sibling of the test above, and the only thing that exercises
    /// [`V4l2Camera`] against a real driver rather than against
    /// [`choose_format`] in isolation. Ignored for the same reasons: CI has no
    /// webcam, and on a workstation it turns the capture light on.
    ///
    /// Under WSL the camera arrives over USB/IP, which is worth knowing when
    /// this fails with "no video capture devices found" on a machine that
    /// plainly has a webcam: the device has to be attached to the running
    /// distribution (`usbipd attach --wsl --busid N-M`) and the user has to be
    /// in the `video` group before `/dev/video0` can be opened.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a physical camera"]
    fn captures_a_frame_from_the_default_camera() {
        use crate::video::camera::linux_impl::choose_format;
        use v4l::video::Capture;

        let devices = list_devices();
        println!("cameras: {devices:?}");
        assert!(!devices.is_empty(), "no camera attached");

        // What the driver actually offers is printed rather than asserted:
        // the reason to run this on hardware is to learn which branch of
        // `choose_format` real cameras take, and that answer is a property of
        // the device. The only claim made here is that one of them is usable —
        // an MJPEG-only camera is a legitimate failure this should report
        // loudly rather than a bug in the chooser.
        let probe = v4l::Device::with_path(&devices[0].id).expect("open device node");
        let offered: Vec<[u8; 4]> = Capture::enum_formats(&probe)
            .expect("enumerate formats")
            .into_iter()
            .map(|f| f.fourcc.repr)
            .collect();
        let name = |f: &[u8; 4]| String::from_utf8_lossy(f).to_string();
        println!("offers: {:?}", offered.iter().map(name).collect::<Vec<_>>());
        let picked = choose_format(&offered);
        println!("chose:  {:?}", picked.as_ref().map(name));
        assert!(picked.is_some(), "a real camera offers no supported format");
        // Released before `V4l2Camera::open` reopens the node: the probe holds
        // its own fd, and some drivers refuse a second streaming open.
        drop(probe);

        let mut cam = V4l2Camera::open(None, 640, 360).expect("open camera");
        let (w, h) = cam.dimensions();
        println!("negotiated {w}x{h}");

        // The first reads often return "nothing yet" while the device starts.
        let mut frame = None;
        for _ in 0..60 {
            if let Ok(f) = cam.next_frame() {
                frame = Some(f);
                break;
            }
        }
        let frame = frame.expect("camera produced no frame");
        assert!(frame.is_consistent());
        assert_eq!((frame.width, frame.height), (w, h));

        // A frame of one flat value is what an unstarted sensor or a buffer
        // that was never filled produces, and it passes every check above —
        // the plane lengths are right either way. Light through a lens always
        // varies somewhere, so this is what separates "captured a picture"
        // from "captured the right number of bytes".
        let first = frame.y[0];
        assert!(
            frame.y.iter().any(|&p| p != first),
            "luma plane is a single flat value — capture produced no picture"
        );
    }
}
