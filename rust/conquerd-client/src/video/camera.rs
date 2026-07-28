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
//! Other platforms currently get [`NullCamera`], which reports no devices
//! rather than failing to compile. That keeps mac and Linux CI green while
//! video is Windows-first; an AVFoundation or V4L2 backend slots in behind
//! [`CameraSource`] without touching callers.

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

/// Enumerate cameras. Always empty where capture is unimplemented.
#[cfg(not(target_os = "windows"))]
pub fn list_devices() -> Vec<CameraDevice> {
    Vec::new()
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::{CameraDevice, CameraSource};
    use crate::video::frame::RawFrame;
    use crate::video::nv12;

    use windows::core::{Interface, PWSTR};
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

            for (subtype, format) in [
                (MFVideoFormat_NV12, CaptureFormat::Nv12),
                (MFVideoFormat_YUY2, CaptureFormat::Yuy2),
            ] {
                // SAFETY: building a media type and applying it to the reader.
                let applied = unsafe {
                    let Ok(mt) = MFCreateMediaType() else {
                        continue;
                    };
                    if mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).is_err()
                        || mt.SetGUID(&MF_MT_SUBTYPE, &subtype).is_err()
                        || mt
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
}
