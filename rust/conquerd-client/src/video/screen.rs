//! Screen, window, and game capture via Windows.Graphics.Capture (WGC).
//!
//! # Why WGC
//!
//! It is the only Windows API that covers all three sources this feature needs.
//! Desktop Duplication (DXGI) captures monitors but has no concept of a single
//! window, and it loses the frame stream when a fullscreen-exclusive game takes
//! the display. GDI `BitBlt` can address a window but cannot see
//! hardware-accelerated content, so a game window comes back black. WGC is what
//! the platform intends for this and what other conferencing apps use.
//!
//! # Shape of the pipeline
//!
//! WGC is push-oriented and GPU-resident: frames arrive in a pool as D3D11
//! textures. Everything above [`super::sender`] wants tightly-packed I420 in
//! system memory, so each frame is copied to a CPU-readable staging texture,
//! then converted and scaled down to the encoder's target size.
//!
//! ```text
//! WGC frame pool -> ID3D11Texture2D (BGRA, GPU)
//!                -> staging texture (BGRA, CPU-readable)
//!                -> BGRA -> I420 + box downscale  (super::scale)
//! ```
//!
//! The downscale is not optional: a 3840x2160 monitor is 33x the pixels of the
//! 640x360 the encoder is configured for, and handing that to H.264 unscaled
//! would blow the frame budget and the fragment cap in one step.

use windows::core::Interface;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::camera::CameraSource;
use super::frame::RawFrame;

/// How long to wait for the first frame before declaring the capture dead.
///
/// A monitor that is not changing produces no frames at all — WGC only
/// delivers on change — so this has to tolerate a genuinely idle desktop while
/// still failing fast when the source is invalid.
const FIRST_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Poll interval while waiting on the frame pool.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(4);

/// A live WGC capture, exposed through the same trait as a camera so
/// [`super::sender`] does not care which one it is driving.
pub struct ScreenCapture {
    _item: GraphicsCaptureItem,
    session: windows::Graphics::Capture::GraphicsCaptureSession,
    frame_pool: Direct3D11CaptureFramePool,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    /// Reused CPU-readable texture. Recreated only when the source resizes.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    /// Encoder-facing size. Frames are scaled into this.
    out_width: u32,
    out_height: u32,
    /// Last successfully converted frame, returned when the source is idle.
    ///
    /// WGC delivers nothing while the screen is static, but the encoder needs a
    /// steady cadence to keep its rate control sane and to give late joiners a
    /// keyframe. Repeating the last frame is what turns an event stream into
    /// the fixed-rate source everything above expects.
    last: Option<RawFrame>,
}

// SAFETY: the D3D11 and WinRT objects are exclusively owned by whichever thread
// constructed the capture, matching `CameraSource`'s contract (Send, not Sync).
unsafe impl Send for ScreenCapture {}

impl ScreenCapture {
    /// Open a capture for `target`, scaling output to fit `max_w` x `max_h`.
    pub fn open(target: &CaptureTarget, max_w: u32, max_h: u32) -> anyhow::Result<Self> {
        let item = create_capture_item(target)?;
        let size = item.Size()?;

        let (device, context) = create_d3d_device()?;
        let rt_device = direct3d_device_from(&device)?;

        // Two buffers: enough to keep the GPU from stalling on us without
        // building a backlog of stale frames, which for realtime video is
        // latency with no upside.
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &rt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )?;
        let session = frame_pool.CreateCaptureSession(&item)?;

        // Best-effort cosmetics: both are only available on newer Windows
        // builds, and an older build should still capture rather than refuse.
        let _ = session.SetIsCursorCaptureEnabled(true);
        let _ = session.SetIsBorderRequired(false);

        session.StartCapture()?;

        // Output is pinned to the encoder's configured size, not the source's
        // (which only sized the frame pool above). Content is letterboxed into
        // it, so a window resize mid-stream changes the bars rather than the
        // frame size the encoder was built for.
        let (out_width, out_height) = (max_w.max(2) & !1, max_h.max(2) & !1);

        Ok(Self {
            _item: item,
            session,
            frame_pool,
            device,
            context,
            staging: None,
            out_width,
            out_height,
            last: None,
        })
    }

    /// Pull one frame from the pool and convert it, or `Ok(None)` if the pool
    /// is empty right now.
    fn try_convert_next(&mut self) -> anyhow::Result<Option<RawFrame>> {
        let Ok(frame) = self.frame_pool.TryGetNextFrame() else {
            return Ok(None);
        };

        let surface = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
        // SAFETY: the surface is live for the lifetime of `frame`.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface()? };

        // SAFETY: reading a description from a live texture.
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { texture.GetDesc(&mut desc) };

        let staging = self.ensure_staging(desc.Width, desc.Height)?;

        // SAFETY: same device; staging is a full-size copy destination.
        unsafe {
            self.context.CopyResource(&staging, &texture);
        }

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging was created with CPU read access and USAGE_STAGING.
        unsafe {
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }

        let converted = if mapped.pData.is_null() {
            None
        } else {
            // SAFETY: Map succeeded, so pData covers RowPitch * Height bytes.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    mapped.pData as *const u8,
                    mapped.RowPitch as usize * desc.Height as usize,
                )
            };
            // Letterboxed, so the frame size never changes even if the user
            // resizes the captured window or the display changes mode.
            super::scale::bgra_to_i420_letterboxed(
                bytes,
                mapped.RowPitch as usize,
                desc.Width,
                desc.Height,
                self.out_width,
                self.out_height,
            )
        };

        // SAFETY: balances the successful Map above.
        unsafe { self.context.Unmap(&staging, 0) };

        Ok(converted)
    }

    /// Get (creating or resizing as needed) the CPU-readable staging texture.
    ///
    /// Returns an owned handle rather than a borrow: `ID3D11Texture2D` is
    /// refcounted, so cloning is an `AddRef`, and holding a borrow of `self`
    /// here would block the `self.context` calls that immediately follow.
    fn ensure_staging(&mut self, width: u32, height: u32) -> anyhow::Result<ID3D11Texture2D> {
        let stale = match &self.staging {
            Some((_, w, h)) => *w != width || *h != height,
            None => true,
        };
        if stale {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut tex: Option<ID3D11Texture2D> = None;
            // SAFETY: descriptor is fully initialised; no initial data.
            unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
            let tex = tex.ok_or_else(|| anyhow::anyhow!("staging texture was not created"))?;
            self.staging = Some((tex, width, height));
        }
        self.staging
            .as_ref()
            .map(|(tex, _, _)| tex.clone())
            .ok_or_else(|| anyhow::anyhow!("staging texture unavailable"))
    }
}

impl CameraSource for ScreenCapture {
    fn next_frame(&mut self) -> anyhow::Result<RawFrame> {
        let deadline = std::time::Instant::now() + FIRST_FRAME_TIMEOUT;
        loop {
            if let Some(frame) = self.try_convert_next()? {
                self.last = Some(frame.clone());
                return Ok(frame);
            }
            // Nothing new. A static desktop is the normal case, so repeat the
            // previous frame rather than stalling the encoder's cadence.
            if let Some(prev) = &self.last {
                return Ok(prev.clone());
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("no frame from the capture source within the timeout");
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        (self.out_width, self.out_height)
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        // Order matters: stop delivery before the pool goes away, or WGC can
        // hand a frame to a pool that is being torn down.
        let _ = self.session.Close();
        let _ = self.frame_pool.Close();
    }
}

/// What to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureTarget {
    /// A display, identified by its `HMONITOR` as a raw pointer value.
    Monitor(isize),
    /// A top-level window, identified by its `HWND` as a raw pointer value.
    Window(isize),
}

// Geometry helpers (`fit_within`) live in [`super::scale`] so the letterbox
// path compiles on non-Windows CI. Re-export for any Windows-only caller that
// still expects the name on this module.
pub use super::scale::fit_within;

/// One thing the user can pick in the source list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSource {
    /// Stable-enough identifier to reopen this source: `"monitor:<handle>"` or
    /// `"window:<handle>"`.
    ///
    /// Handles are only valid for the current session — Windows recycles both
    /// HWND and HMONITOR values — so a saved id is re-validated on use rather
    /// than trusted. [`parse_target`] does that check.
    pub id: String,
    /// Human-readable label for the picker.
    pub name: String,
    /// Whether this is a display rather than a window.
    pub is_monitor: bool,
}

/// Parse an id produced by [`list_sources`] back into a target.
///
/// Returns `None` for anything malformed or no longer live, which is the
/// expected outcome for a source saved in settings during a previous session.
pub fn parse_target(id: &str) -> Option<CaptureTarget> {
    let (kind, handle) = id.split_once(':')?;
    let raw: isize = handle.parse().ok()?;
    if raw == 0 {
        return None;
    }
    match kind {
        "monitor" => {
            // SAFETY: a stale HMONITOR is reported invalid rather than faulting.
            let mut info = windows::Win32::Graphics::Gdi::MONITORINFO {
                cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFO>() as u32,
                ..Default::default()
            };
            let ok = unsafe {
                windows::Win32::Graphics::Gdi::GetMonitorInfoW(
                    windows::Win32::Graphics::Gdi::HMONITOR(raw as *mut std::ffi::c_void),
                    &mut info,
                )
            };
            ok.as_bool().then_some(CaptureTarget::Monitor(raw))
        }
        "window" => {
            let hwnd = windows::Win32::Foundation::HWND(raw as *mut std::ffi::c_void);
            // SAFETY: IsWindow is the documented liveness check for an HWND.
            let alive = unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd) };
            alive.as_bool().then_some(CaptureTarget::Window(raw))
        }
        _ => None,
    }
}

/// Enumerate capturable monitors and top-level windows.
pub fn list_sources() -> Vec<CaptureSource> {
    let mut out = list_monitors();
    out.extend(list_windows());
    out
}

fn list_monitors() -> Vec<CaptureSource> {
    // Collected through a callback, so the accumulator travels as lparam.
    unsafe extern "system" fn cb(
        monitor: windows::Win32::Graphics::Gdi::HMONITOR,
        _dc: windows::Win32::Graphics::Gdi::HDC,
        _rect: *mut windows::Win32::Foundation::RECT,
        data: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        let out = &mut *(data.0 as *mut Vec<CaptureSource>);
        let mut info = windows::Win32::Graphics::Gdi::MONITORINFOEXW {
            monitorInfo: windows::Win32::Graphics::Gdi::MONITORINFO {
                cbSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::MONITORINFOEXW>() as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let ok = windows::Win32::Graphics::Gdi::GetMonitorInfoW(
            monitor,
            std::ptr::from_mut(&mut info).cast(),
        );
        if ok.as_bool() {
            let rc = info.monitorInfo.rcMonitor;
            // `MONITORINFOF_PRIMARY`. Spelled out because windows-rs 0.58 does
            // not surface the constant in the Gdi module.
            const MONITORINFOF_PRIMARY: u32 = 0x0000_0001;
            let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
            let idx = out.iter().filter(|s| s.is_monitor).count() + 1;
            let label = if primary {
                format!(
                    "Screen {idx} (primary) — {}x{}",
                    rc.right - rc.left,
                    rc.bottom - rc.top
                )
            } else {
                format!(
                    "Screen {idx} — {}x{}",
                    rc.right - rc.left,
                    rc.bottom - rc.top
                )
            };
            out.push(CaptureSource {
                id: format!("monitor:{}", monitor.0 as isize),
                name: label,
                is_monitor: true,
            });
        }
        windows::Win32::Foundation::TRUE
    }

    let mut out: Vec<CaptureSource> = Vec::new();
    // SAFETY: the callback only touches `out`, which outlives the call.
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::EnumDisplayMonitors(
            None,
            None,
            Some(cb),
            windows::Win32::Foundation::LPARAM(std::ptr::from_mut(&mut out) as isize),
        );
    }
    out
}

fn list_windows() -> Vec<CaptureSource> {
    use windows::Win32::UI::WindowsAndMessaging as wm;

    unsafe extern "system" fn cb(
        hwnd: windows::Win32::Foundation::HWND,
        data: windows::Win32::Foundation::LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        use windows::Win32::UI::WindowsAndMessaging as wm;
        let out = &mut *(data.0 as *mut Vec<CaptureSource>);

        // Only windows a user would recognise: visible, titled, not a tool
        // window, and not the invisible shell/host windows that would
        // otherwise flood the list.
        if !wm::IsWindowVisible(hwnd).as_bool() {
            return windows::Win32::Foundation::TRUE;
        }
        let len = wm::GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return windows::Win32::Foundation::TRUE;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let written = wm::GetWindowTextW(hwnd, &mut buf);
        if written <= 0 {
            return windows::Win32::Foundation::TRUE;
        }
        let title = String::from_utf16_lossy(&buf[..written as usize]);

        let ex = wm::GetWindowLongPtrW(hwnd, wm::GWL_EXSTYLE) as u32;
        if ex & wm::WS_EX_TOOLWINDOW.0 != 0 {
            return windows::Win32::Foundation::TRUE;
        }

        // Cloaked windows are the UWP suspended-app case: present in the
        // window list, invisible to the user, and captured as a black frame.
        let mut cloaked: u32 = 0;
        let hr = windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWA_CLOAKED,
            std::ptr::from_mut(&mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        );
        if hr.is_ok() && cloaked != 0 {
            return windows::Win32::Foundation::TRUE;
        }

        out.push(CaptureSource {
            id: format!("window:{}", hwnd.0 as isize),
            name: title,
            is_monitor: false,
        });
        windows::Win32::Foundation::TRUE
    }

    let mut out: Vec<CaptureSource> = Vec::new();
    // SAFETY: the callback only touches `out`, which outlives the call.
    unsafe {
        let _ = wm::EnumWindows(
            Some(cb),
            windows::Win32::Foundation::LPARAM(std::ptr::from_mut(&mut out) as isize),
        );
    }
    out
}

fn create_capture_item(target: &CaptureTarget) -> anyhow::Result<GraphicsCaptureItem> {
    // The WinRT activation factory exposes the Win32 interop that turns an
    // HWND/HMONITOR into a GraphicsCaptureItem; there is no pure-WinRT path
    // from a Win32 handle.
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;

    // SAFETY: the handles come from enumeration below and are validated by the
    // interop call, which fails rather than faulting on a stale handle.
    let item = unsafe {
        match target {
            CaptureTarget::Monitor(h) => interop.CreateForMonitor(
                windows::Win32::Graphics::Gdi::HMONITOR(*h as *mut std::ffi::c_void),
            )?,
            CaptureTarget::Window(h) => interop.CreateForWindow(
                windows::Win32::Foundation::HWND(*h as *mut std::ffi::c_void),
            )?,
        }
    };
    Ok(item)
}

fn create_d3d_device() -> anyhow::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: standard device creation; out-params are checked below.
    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE::default(),
            // BGRA support is required for WGC's B8G8R8A8 frame format.
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device = device.ok_or_else(|| anyhow::anyhow!("D3D11 device was not created"))?;
    let context = context.ok_or_else(|| anyhow::anyhow!("D3D11 context was not created"))?;
    Ok((device, context))
}

fn direct3d_device_from(device: &ID3D11Device) -> anyhow::Result<IDirect3DDevice> {
    let dxgi: IDXGIDevice = device.cast()?;
    // SAFETY: `dxgi` is a live DXGI device from the D3D11 device above.
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi)? };
    Ok(inspectable.cast()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercises the real Win32 enumeration. Ignored by default because CI has
    /// no desktop session; run on a workstation:
    /// `cargo test --bins -- --ignored screen`
    #[test]
    #[ignore = "requires an interactive Windows desktop session"]
    fn enumerates_real_monitors_and_windows() {
        let sources = list_sources();
        let monitors: Vec<_> = sources.iter().filter(|s| s.is_monitor).collect();
        let windows: Vec<_> = sources.iter().filter(|s| !s.is_monitor).collect();
        println!("monitors: {}", monitors.len());
        for m in &monitors {
            println!("  {} [{}]", m.name, m.id);
        }
        println!("windows: {}", windows.len());
        for w in windows.iter().take(10) {
            println!("  {} [{}]", w.name, w.id);
        }

        assert!(
            !monitors.is_empty(),
            "an interactive session must have at least one display"
        );
        // Every id must round-trip back to a live target, or the picker would
        // hand the capture layer something it cannot open.
        for s in &sources {
            assert!(
                parse_target(&s.id).is_some(),
                "enumerated source {} did not parse back",
                s.id
            );
        }
    }

    /// End-to-end against the real compositor: open the primary display, pull
    /// frames, and check they are plausible pictures rather than blank buffers.
    #[test]
    #[ignore = "requires an interactive Windows desktop session"]
    fn captures_real_frames_from_the_primary_display() {
        let monitor = list_sources()
            .into_iter()
            .find(|s| s.is_monitor)
            .expect("an interactive session has a display");
        let target = parse_target(&monitor.id).expect("monitor id must parse");

        let mut cap = ScreenCapture::open(&target, 640, 360).expect("open capture");
        assert_eq!(cap.dimensions(), (640, 360), "output size must be pinned");

        let mut distinct_frames = 0usize;
        let mut prev: Option<Vec<u8>> = None;
        for _ in 0..15 {
            let f = cap.next_frame().expect("capture a frame");
            assert_eq!((f.width, f.height), (640, 360));
            assert!(f.is_consistent(), "plane sizes must match the dimensions");
            if prev.as_ref() != Some(&f.y) {
                distinct_frames += 1;
            }
            prev = Some(f.y.clone());
            std::thread::sleep(std::time::Duration::from_millis(30));
        }

        let y = prev.expect("at least one frame");
        let min = *y.iter().min().unwrap();
        let max = *y.iter().max().unwrap();
        println!(
            "captured {}x{}, luma range {min}..{max}, {distinct_frames} distinct frames",
            640, 360
        );
        // A desktop has contrast. An all-identical luma plane means we captured
        // a blank surface — the classic symptom of the GDI path or a failed
        // texture copy, and it would otherwise look like "capture works".
        assert!(
            max - min > 16,
            "captured frame has no contrast (luma {min}..{max}) — likely a blank surface"
        );
    }

    #[test]
    fn parse_target_rejects_malformed_ids() {
        for bad in [
            "",
            "monitor",
            "monitor:",
            "monitor:abc",
            "bogus:1",
            "window:0",
            "1234",
        ] {
            assert!(parse_target(bad).is_none(), "{bad:?} should not parse");
        }
    }
}
