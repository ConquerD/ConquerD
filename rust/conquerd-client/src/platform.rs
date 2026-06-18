//! Platform integration helpers — uri_scheme, taskbar badge, ringtone, UPnP.

use tracing::{info, warn};

// Ringtone playback state (one global OutputStream keeps the audio device open)
use std::sync::Mutex;

struct RingtoneState {
    _stream: rodio::OutputStream,
    sink: rodio::Sink,
}

// SAFETY: rodio's OutputStream contains a raw pointer to a CPAL thread;
// we only access it from the calling thread and hold the mutex.
unsafe impl Send for RingtoneState {}

static RINGTONE: Mutex<Option<RingtoneState>> = Mutex::new(None);

/// Embedded ringtone OGG bytes — compile-time bundle from `assets/ringtone.ogg`.
///
/// If the asset is absent, `RINGTONE_BYTES` will be a zero-length slice and
/// `play_ringtone` will silently skip playback.
///
/// To avoid breaking the build on machines without the asset, we use a
/// build.rs `DEP_` trick; here we simply try to load at runtime.
fn load_ringtone_bytes() -> Option<Vec<u8>> {
    // Try exe-adjacent `assets/ringtone.ogg`, then cwd fallback.
    let candidates: Vec<std::path::PathBuf> = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("assets").join("ringtone.ogg"))),
        Some(std::path::PathBuf::from("assets/ringtone.ogg")),
    ]
    .into_iter()
    .flatten()
    .collect();
    for path in &candidates {
        if let Ok(data) = std::fs::read(path) {
            return Some(data);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// URI scheme
// ---------------------------------------------------------------------------

/// Registered custom URI scheme (`conquerd://`).
pub const URI_SCHEME: &str = "conquerd";

// ---------------------------------------------------------------------------
// Single-instance guard
// ---------------------------------------------------------------------------

/// Return `true` when this process should exit immediately because another
/// `ConquerD.exe` instance is already running AND this invocation was started
/// only to dispatch a `conquerd://` URL from the OS shell.
///
/// Background: at startup we register `conquerd://` with the Windows shell so
/// invite links open the app.  Chromium's "external protocol handler" inside
/// our embedded `WebEngineView` can also hand `conquerd://` URLs to the
/// shell if a fetch fails for any reason — which would normally spawn a
/// second `ConquerD.exe` at the unlock prompt and look like a stray popup.
///
/// To prevent that, we acquire a per-user named mutex.  If another instance
/// already holds it and our argv contains a `conquerd://` URL, we exit
/// silently — the running instance keeps handling everything in-process via
/// its own QtWebEngine scheme handler.
///
/// Returns `false` on non-Windows platforms.
pub fn should_exit_as_duplicate_instance() -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::sync::OnceLock;
        // Hold the mutex handle for the lifetime of the process so the OS
        // tracks ownership correctly.  Stored in a static so it is not
        // dropped at the end of this function.
        static MUTEX_HANDLE: OnceLock<usize> = OnceLock::new();

        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
        use windows::Win32::System::Threading::CreateMutexW;

        // Build a wide-char mutex name.  "Local\\" prefix scopes it to the
        // current logon session, matching our per-user identity model.
        let name: Vec<u16> = "Local\\ConquerD-SingleInstance\0".encode_utf16().collect();

        unsafe {
            match CreateMutexW(None, false, PCWSTR(name.as_ptr())) {
                Ok(handle) => {
                    let already = GetLastError() == ERROR_ALREADY_EXISTS;
                    // Leak the handle into the OnceLock so it stays alive.
                    let _ = MUTEX_HANDLE.set(handle.0 as usize);
                    if already {
                        // Another instance is alive.  Only suppress this
                        // process if it was launched to dispatch a URL —
                        // otherwise the user intentionally double-launched
                        // and should still get a normal window.
                        let argv_has_uri =
                            std::env::args().any(|a| a.starts_with(&format!("{URI_SCHEME}://")));
                        if argv_has_uri {
                            info!(
                                "[single-instance] another ConquerD is running; \
                                 exiting silently to avoid duplicate URL-handler popup"
                            );
                            return true;
                        }
                    }
                    false
                }
                Err(e) => {
                    warn!("[single-instance] CreateMutexW failed: {e}");
                    false
                }
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Parse a `conquerd://` URI into its components.
///
/// Expected format: `conquerd://action/payload`
pub fn parse_uri(uri: &str) -> Option<(String, String)> {
    let stripped = uri.strip_prefix(&format!("{URI_SCHEME}://"))?;
    let (action, payload) = stripped.split_once('/').unwrap_or((stripped, ""));
    Some((action.to_string(), payload.to_string()))
}

/// Register the `conquerd://` URI scheme handler with the OS.
///
/// - **Windows**: writes to `HKCU\Software\Classes\conquerd\...`
/// - **macOS**: handled via `Info.plist` `CFBundleURLTypes` at build time
/// - **Linux**: installs a `.desktop` file (see `packaging/install_uri_scheme.sh`)
///
/// Returns `true` if successful. Failures are non-fatal (logged only).
pub fn register_uri_scheme() -> bool {
    #[cfg(target_os = "windows")]
    {
        register_uri_scheme_windows()
    }
    #[cfg(not(target_os = "windows"))]
    {
        // macOS: done via Info.plist; Linux: done at install time
        info!("URI scheme registration skipped on this platform");
        false
    }
}

#[cfg(target_os = "windows")]
fn register_uri_scheme_windows() -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // Use reg.exe to add to HKCU\Software\Classes
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let base = format!("HKCU\\Software\\Classes\\{URI_SCHEME}");
    let ok = Command::new("reg")
        .args(["add", &base, "/ve", "/d", "URL:ConquerD Protocol", "/f"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        let cmd_key = format!("{base}\\shell\\open\\command");
        Command::new("reg")
            .args([
                "add",
                &cmd_key,
                "/ve",
                "/d",
                &format!("\"{exe}\" \"%1\""),
                "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        Command::new("reg")
            .args(["add", &base, "/v", "URL Protocol", "/d", "", "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .map(|s| s.success())
            .unwrap_or(ok)
    } else {
        false
    }
}

// ---------------------------------------------------------------------------
// Taskbar badge
// ---------------------------------------------------------------------------

/// Set the taskbar unread-message badge count.
///
/// - **Windows**: Win32 `ITaskbarList3::SetOverlayIcon` via windows-rs
/// - **macOS**: `NSApp.dockTile.badgeLabel` (not yet implemented)
/// - **Linux**: `UnityLauncherEntry.count` via D-Bus (not yet implemented)
pub fn set_taskbar_badge(count: u32) {
    #[cfg(target_os = "windows")]
    {
        set_taskbar_badge_windows(count);
    }
    #[cfg(target_os = "macos")]
    {
        // Simple implementation via osascript (no extra deps)
        let script = format!(
            r#"tell application "System Events" to set the dock tile of process "ConquerD" to {{label:"{}"}}"#,
            count
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        // Basic D-Bus signal for Unity/GNOME taskbar count via dbus-send
        let _ = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=com.canonical.Unity",
                "--type=method_call",
                "/com/canonical/unity/launcherentry/1",
                "com.canonical.Unity.LauncherEntry.Update",
                "string:application://conquerd.desktop",
                &format!("variant:{{'count': <int64:{}>}}", count as i64),
            ])
            .spawn();
    }
}

/// Clear the taskbar badge.
pub fn clear_taskbar_badge() {
    set_taskbar_badge(0);
}

#[cfg(target_os = "windows")]
fn set_taskbar_badge_windows(count: u32) {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::HICON;

    unsafe {
        // COM must be initialized on the calling thread.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let taskbar: ITaskbarList3 = match CoCreateInstance(&TaskbarList, None, CLSCTX_ALL) {
            Ok(t) => t,
            Err(e) => {
                warn!("Taskbar badge: CoCreateInstance failed: {e}");
                return;
            }
        };

        // Null HWND: targets the foreground window of the calling process
        let hwnd = HWND(0isize as _);
        if count == 0 {
            let _ = taskbar.SetOverlayIcon(hwnd, HICON(0isize as _), PCWSTR::null());
        } else {
            let hicon = create_badge_icon(count);
            let tip: Vec<u16> = "Unread messages\0".encode_utf16().collect();
            let _ = taskbar.SetOverlayIcon(hwnd, hicon, PCWSTR(tip.as_ptr()));
            if hicon.0 as usize != 0 {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyIcon(hicon);
            }
        }
    }
}

#[cfg(target_os = "windows")]
unsafe fn create_badge_icon(count: u32) -> windows::Win32::UI::WindowsAndMessaging::HICON {
    use windows::Win32::Foundation::{COLORREF, HWND, RECT};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject,
        FillRect, GetDC, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TextOutW, TRANSPARENT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

    const SIZE: i32 = 16;
    let hdc_screen = GetDC(HWND(0isize as _));
    let hdc = CreateCompatibleDC(hdc_screen);
    let hbm = CreateCompatibleBitmap(hdc_screen, SIZE, SIZE);
    let _ = SelectObject(hdc, hbm);
    let _ = ReleaseDC(HWND(0isize as _), hdc_screen);

    // Red background (BGR)
    let brush = CreateSolidBrush(COLORREF(0x0000CC));
    let rect = RECT {
        left: 0,
        top: 0,
        right: SIZE,
        bottom: SIZE,
    };
    let _ = FillRect(hdc, &rect, brush);
    let _ = DeleteObject(brush);

    // White count text
    let _ = SetBkMode(hdc, TRANSPARENT);
    let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));
    let label: Vec<u16> = if count < 100 {
        format!("{count}\0").encode_utf16().collect()
    } else {
        "99+\0".encode_utf16().collect()
    };
    let _ = TextOutW(hdc, 2, 2, &label);

    let mask = CreateCompatibleBitmap(hdc_screen, SIZE, SIZE);
    let info = ICONINFO {
        fIcon: windows::Win32::Foundation::BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: hbm,
    };
    let hicon = CreateIconIndirect(&info).unwrap_or(HICON(0isize as _));
    let _ = DeleteObject(mask);
    let _ = DeleteObject(hbm);
    let _ = DeleteDC(hdc);
    hicon
}

// ---------------------------------------------------------------------------
// Ringtone
// ---------------------------------------------------------------------------

/// Play the incoming-call ringtone.
///
/// Returns immediately; playback runs on a background thread managed by rodio.
/// Looks for `assets/ringtone.ogg` next to the executable (or in cwd).
pub fn play_ringtone() {
    let Some(bytes) = load_ringtone_bytes() else {
        info!("Ringtone: asset not found, skipping playback");
        return;
    };
    let Ok((stream, handle)) = rodio::OutputStream::try_default() else {
        warn!("Ringtone: could not open audio output");
        return;
    };
    let Ok(sink) = rodio::Sink::try_new(&handle) else {
        warn!("Ringtone: could not create sink");
        return;
    };
    let cursor = std::io::Cursor::new(bytes);
    match rodio::Decoder::new(cursor) {
        Ok(decoder) => {
            sink.append(decoder);
            sink.set_volume(0.8);
        }
        Err(e) => {
            warn!("Ringtone: decode error: {e}");
            return;
        }
    }
    let state = RingtoneState {
        _stream: stream,
        sink,
    };
    if let Ok(mut guard) = RINGTONE.lock() {
        *guard = Some(state);
    }
    info!("Ringtone: playing");
}

/// Stop the currently playing ringtone.
pub fn stop_ringtone() {
    if let Ok(mut guard) = RINGTONE.lock() {
        if let Some(state) = guard.take() {
            state.sink.stop();
        }
    }
    info!("Ringtone: stopped");
}

// ---------------------------------------------------------------------------
// Push-to-talk
// ---------------------------------------------------------------------------

/// Start a PTT polling thread. Returns a stop flag — set it to `true` to halt.
///
/// The `muted_tx` channel receives `true` (muted) when the key is NOT held
/// and `false` (unmuted) when the key IS held. Polls at ~60 Hz.
pub fn start_ptt_polling(
    key_name: String,
    muted_tx: std::sync::mpsc::SyncSender<bool>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    match std::thread::Builder::new()
        .name("conquerd-ptt".into())
        .spawn(move || {
            #[cfg(target_os = "windows")]
            let vk = key_name_to_vk_windows(&key_name);
            #[cfg(not(target_os = "windows"))]
            let _ = key_name;

            let mut prev_muted = true;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                #[cfg(target_os = "windows")]
                let pressed = if vk != 0 {
                    unsafe {
                        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
                        (GetAsyncKeyState(vk) as u16 & 0x8000) != 0
                    }
                } else {
                    false
                };
                #[cfg(not(target_os = "windows"))]
                let pressed = false;

                let muted = !pressed;
                if muted != prev_muted {
                    prev_muted = muted;
                    let _ = muted_tx.try_send(muted);
                }
                std::thread::sleep(std::time::Duration::from_millis(16));
            }
        }) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!("failed to spawn PTT thread: {e}");
            std::thread::Builder::new()
                .spawn(|| {})
                .unwrap_or_else(|_| std::process::abort())
        }
    }
}

#[cfg(target_os = "windows")]
fn key_name_to_vk_windows(name: &str) -> i32 {
    match name.to_lowercase().trim() {
        "space" => 0x20,
        "f1" => 0x70,
        "f2" => 0x71,
        "f3" => 0x72,
        "f4" => 0x73,
        "f5" => 0x74,
        "f6" => 0x75,
        "f7" => 0x76,
        "f8" => 0x77,
        "f9" => 0x78,
        "f10" => 0x79,
        "f11" => 0x7A,
        "f12" => 0x7B,
        "ctrl" | "control" | "lctrl" => 0x11,
        "alt" | "lalt" => 0x12,
        "shift" | "lshift" => 0x10,
        "capslock" => 0x14,
        "tab" => 0x09,
        "enter" | "return" => 0x0D,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "left" => 0x25,
        "right" => 0x27,
        "up" => 0x26,
        "down" => 0x28,
        "esc" | "escape" => 0x1B,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        // Mouse buttons (GetAsyncKeyState works with VK_*BUTTON codes)
        "mouse1" | "lmb" => 0x01, // VK_LBUTTON
        "mouse2" | "rmb" => 0x02, // VK_RBUTTON
        "mouse3" | "mmb" => 0x04, // VK_MBUTTON
        "mouse4" | "xb1" => 0x05, // VK_XBUTTON1 (Back)
        "mouse5" | "xb2" => 0x06, // VK_XBUTTON2 (Forward)
        s if s.len() == 1 => s
            .chars()
            .next()
            .and_then(|ch| ch.to_uppercase().next())
            .map(|c| c as i32)
            .unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Desktop notifications
// ---------------------------------------------------------------------------

/// Show a transient desktop notification (non-blocking, best-effort).
pub fn show_notification(title: &str, body: &str) {
    #[cfg(target_os = "windows")]
    show_notification_windows(title, body);
    #[cfg(target_os = "linux")]
    show_notification_linux(title, body);
    #[cfg(target_os = "macos")]
    show_notification_macos(title, body);
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let _ = (title, body);
    }
}

#[cfg(target_os = "windows")]
fn show_notification_windows(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    // Use Windows Forms NotifyIcon balloon tip via PowerShell — always works
    // on Windows 7+ without app registration. Spawned async so never blocks.
    let title = title.replace('"', "");
    let body = body.replace('"', "");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms;\
         $n=New-Object System.Windows.Forms.NotifyIcon;\
         $n.Icon=[System.Drawing.SystemIcons]::Information;\
         $n.Visible=$true;\
         $n.ShowBalloonTip(4000,\"{title}\",\"{body}\",[System.Windows.Forms.ToolTipIcon]::None);\
         Start-Sleep -Seconds 5;$n.Visible=$false;$n.Dispose()"
    );
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(target_os = "linux")]
fn show_notification_linux(title: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .args(["-a", "ConquerD", "-t", "4000", title, body])
        .spawn();
}

#[cfg(target_os = "macos")]
fn show_notification_macos(title: &str, body: &str) {
    let script = format!(
        "display notification {} with title {} subtitle \"ConquerD\"",
        serde_json::to_string(body).unwrap_or_default(),
        serde_json::to_string(title).unwrap_or_default()
    );
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn();
}

// ---------------------------------------------------------------------------
// UPnP port mapping
// ---------------------------------------------------------------------------

/// Request a UPnP port mapping for the local QUIC listener.
///
/// Returns `Some(external_port)` on success or `None` if UPnP is unavailable.
pub async fn request_upnp_mapping(local_port: u16) -> Option<u16> {
    use futures_util::{pin_mut, StreamExt};
    use rupnp::ssdp::{SearchTarget, URN};

    info!("UPnP: searching for IGD gateway...");

    let search_target = SearchTarget::RootDevice;
    let devices = match rupnp::discover(&search_target, std::time::Duration::from_secs(3)).await {
        Ok(d) => d,
        Err(e) => {
            info!("UPnP: SSDP search failed: {}", e);
            return None;
        }
    };
    pin_mut!(devices);

    let wan_ip_urn = URN::service("schemas-upnp-org", "WANIPConnection", 1);
    let wan_ppp_urn = URN::service("schemas-upnp-org", "WANPPPConnection", 1);

    while let Some(device) = devices.next().await {
        match device {
            Ok(dev) => {
                if let Some(external) =
                    try_add_port_mapping(&dev, &wan_ip_urn, &wan_ppp_urn, local_port).await
                {
                    info!("UPnP: successfully mapped {} -> {}", external, local_port);
                    return Some(external);
                }
            }
            Err(e) => warn!("UPnP: device discovery error: {}", e),
        }
    }

    info!("UPnP: no suitable IGD found");
    None
}

async fn try_add_port_mapping(
    device: &rupnp::Device,
    wan_ip_urn: &rupnp::ssdp::URN,
    wan_ppp_urn: &rupnp::ssdp::URN,
    local_port: u16,
) -> Option<u16> {
    let service = device
        .find_service(wan_ip_urn)
        .or_else(|| device.find_service(wan_ppp_urn))?;

    let description = "ConquerD QUIC";
    let internal_client = local_ip().unwrap_or_default();

    match add_port_mapping(
        service,
        device.url(),
        local_port,
        local_port,
        &internal_client,
        description,
    )
    .await
    {
        Ok(_) => Some(local_port),
        Err(e) => {
            warn!("UPnP: AddPortMapping failed on first try: {}", e);
            for offset in 1..10 {
                let Some(try_port) = local_port.checked_add(offset) else {
                    continue;
                };
                if add_port_mapping(
                    service,
                    device.url(),
                    local_port,
                    try_port,
                    &internal_client,
                    description,
                )
                .await
                .is_ok()
                {
                    return Some(try_port);
                }
            }
            None
        }
    }
}

async fn add_port_mapping(
    service: &rupnp::Service,
    device_url: &rupnp::http::Uri,
    local_port: u16,
    external_port: u16,
    internal_client: &str,
    description: &str,
) -> Result<std::collections::HashMap<String, String>, rupnp::Error> {
    let payload = format!(
        "<NewRemoteHost></NewRemoteHost>\
         <NewExternalPort>{external_port}</NewExternalPort>\
         <NewProtocol>UDP</NewProtocol>\
         <NewInternalPort>{local_port}</NewInternalPort>\
         <NewInternalClient>{internal_client}</NewInternalClient>\
         <NewEnabled>1</NewEnabled>\
         <NewPortMappingDescription>{description}</NewPortMappingDescription>\
         <NewLeaseDuration>0</NewLeaseDuration>"
    );
    service.action(device_url, "AddPortMapping", &payload).await
}

fn local_ip() -> Option<String> {
    // Simple best-effort local IP (for UPnP internal client)
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}

/// Release a UPnP port mapping previously obtained with [`request_upnp_mapping`].
pub async fn release_upnp_mapping(external_port: u16) {
    info!("UPnP: releasing mapping for port {} (stub)", external_port);
}

// ---------------------------------------------------------------------------
// Desktop shortcuts
// ---------------------------------------------------------------------------

/// Create Start Menu and Desktop shortcuts for the ConquerD executable.
///
/// **Windows only** — no-op on other platforms.  Uses PowerShell
/// `WScript.Shell` COM to write `.lnk` files into the user's Desktop and
/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs`; no elevation is
/// required.
pub fn create_desktop_shortcuts() {
    #[cfg(target_os = "windows")]
    create_shortcuts_windows();
    #[cfg(not(target_os = "windows"))]
    info!("Desktop shortcuts creation skipped — not supported on this platform");
}

/// Remove the Start Menu and Desktop shortcuts created by
/// [`create_desktop_shortcuts`].
pub fn remove_desktop_shortcuts() {
    #[cfg(target_os = "windows")]
    remove_shortcuts_windows();
    #[cfg(not(target_os = "windows"))]
    info!("Desktop shortcuts removal skipped — not supported on this platform");
}

/// Return `true` if at least one ConquerD shortcut (Desktop or Start Menu)
/// already exists.  **Windows only** — always returns `false` on other
/// platforms.
pub fn has_desktop_shortcuts() -> bool {
    #[cfg(target_os = "windows")]
    {
        has_shortcuts_windows()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
fn has_shortcuts_windows() -> bool {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let start_menu =
        std::path::Path::new(&appdata).join(r"Microsoft\Windows\Start Menu\Programs\ConquerD.lnk");

    let userprofile = std::env::var("USERPROFILE").unwrap_or_default();
    let desktop = std::path::Path::new(&userprofile).join(r"Desktop\ConquerD.lnk");

    start_menu.exists() || desktop.exists()
}

#[cfg(target_os = "windows")]
fn create_shortcuts_windows() {
    use std::os::windows::process::CommandExt;
    let exe = match std::env::current_exe() {
        Ok(p) => p.to_string_lossy().replace('/', "\\"),
        Err(e) => {
            warn!("Shortcuts: could not determine exe path: {e}");
            return;
        }
    };
    // PowerShell WScript.Shell — targets user-local paths, no elevation needed.
    let script = format!(
        r#"$ws = New-Object -ComObject WScript.Shell; \
$paths = @( \
    [Environment]::GetFolderPath('Desktop'), \
    [Environment]::GetFolderPath('Programs') \
); \
foreach ($dir in $paths) {{ \
    if (-not (Test-Path $dir)) {{ continue }}; \
    $lnk = $ws.CreateShortcut("$dir\ConquerD.lnk"); \
    $lnk.TargetPath = "{exe}"; \
    $lnk.Description = "ConquerD — Privacy-first peer connectivity"; \
    $lnk.Save(); \
}}"#
    );
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .creation_flags(0x08000000u32)
        .spawn();
    info!("Shortcuts: Start Menu and Desktop shortcuts created");
}

#[cfg(target_os = "windows")]
fn remove_shortcuts_windows() {
    use std::os::windows::process::CommandExt;
    let script = r#"$paths = @( \
    [Environment]::GetFolderPath('Desktop'), \
    [Environment]::GetFolderPath('Programs') \
); \
foreach ($dir in $paths) { \
    $lnk = "$dir\ConquerD.lnk"; \
    if (Test-Path $lnk) { Remove-Item $lnk -Force } \
}"#;
    let _ = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            script,
        ])
        .creation_flags(0x08000000u32)
        .spawn();
    info!("Shortcuts: Start Menu and Desktop shortcuts removed");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uri_invite() {
        let (action, payload) = parse_uri("conquerd://invite/abc123def456").unwrap();
        assert_eq!(action, "invite");
        assert_eq!(payload, "abc123def456");
    }

    #[test]
    fn parse_uri_no_payload() {
        let (action, payload) = parse_uri("conquerd://open").unwrap();
        assert_eq!(action, "open");
        assert_eq!(payload, "");
    }

    #[test]
    fn parse_uri_invalid_scheme() {
        assert!(parse_uri("https://example.com").is_none());
    }
}
