//! URI scheme registration — registers the `d://` protocol handler
//! (and keeps legacy `conquerd://`).
//!
//! On Windows: writes to HKCU\Software\Classes\d and ...\conquerd
//! (no elevation needed). On other platforms: no-op.

#[cfg(not(target_os = "windows"))]
use tracing::debug;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register the `d://` URI scheme handler for the current user
/// (and keep `conquerd://` working).
///
/// On Windows, points to `conquerd-installer.exe` in
/// `%LOCALAPPDATA%\DoubleSlash` (or the pre-rebrand `ConquerD` folder)
/// if it exists, otherwise points to the current executable.
///
/// Returns `Ok(true)` if registered, `Ok(false)` on non-Windows platforms.
pub fn register() -> std::io::Result<bool> {
    #[cfg(target_os = "windows")]
    {
        windows::register()
    }
    #[cfg(not(target_os = "windows"))]
    {
        debug!("[uri_scheme] registration is only supported on Windows");
        Ok(false)
    }
}

/// Unregister the `d://` and legacy `conquerd://` URI scheme handlers.
///
/// Returns `Ok(true)` if the key was removed, `Ok(false)` on non-Windows.
pub fn unregister() -> std::io::Result<bool> {
    #[cfg(target_os = "windows")]
    {
        windows::unregister()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(false)
    }
}

/// Returns `true` if the scheme is already registered for this install.
pub fn is_registered() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::is_registered()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

// ---------------------------------------------------------------------------
// Windows implementation
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod windows {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use tracing::{info, warn};
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    fn exe_path() -> PathBuf {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            for folder in ["DoubleSlash", "ConquerD"] {
                let installer = PathBuf::from(&local)
                    .join(folder)
                    .join("conquerd-installer.exe");
                if installer.exists() {
                    return installer;
                }
            }
        }
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("DoubleSlash.exe"))
    }

    const SCHEMES: &[&str] = &["d", "conquerd"];

    fn root_for(scheme: &str) -> String {
        format!(r"Software\Classes\{scheme}")
    }

    pub fn is_registered() -> bool {
        // winreg is not a dependency — use raw registry APIs via std::process
        // to avoid pulling in the crate. Check existence by trying to read the
        // command value.
        SCHEMES.iter().any(|scheme| {
            let key_path = format!(r"{}\shell\open\command", root_for(scheme));
            matches!(
                std::process::Command::new("reg")
                    .args(["query", &format!(r"HKCU\{key_path}"), "/ve"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output(),
                Ok(out) if out.status.success()
            )
        })
    }

    fn register_scheme(scheme: &str, cmd: &str) -> std::io::Result<bool> {
        let root = root_for(scheme);
        let entries: Vec<(String, &str, &str)> = vec![
            (root.clone(), "", "URL:DoubleSlash Protocol"),
            (root.clone(), "URL Protocol", ""),
            (format!(r"{root}\shell"), "", ""),
            (format!(r"{root}\shell\open"), "", ""),
            (format!(r"{root}\shell\open\command"), "", cmd),
        ];

        for (key, name, value) in entries {
            let hkcu_key = format!(r"HKCU\{key}");
            let status = std::process::Command::new("reg")
                .args(["add", &hkcu_key, "/f", "/v", name, "/d", value])
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if !status.success() {
                warn!("[uri_scheme] reg add failed for {hkcu_key}");
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn register() -> std::io::Result<bool> {
        let exe = exe_path();
        let cmd = format!(r#""{}" "%1""#, exe.display());
        let mut any = false;
        for scheme in SCHEMES {
            if register_scheme(scheme, &cmd)? {
                info!("[uri_scheme] {scheme}:// registered → {}", exe.display());
                any = true;
            }
        }
        Ok(any)
    }

    pub fn unregister() -> std::io::Result<bool> {
        let mut any = false;
        for scheme in SCHEMES {
            let hkcu_key = format!(r"HKCU\{}", root_for(scheme));
            let status = std::process::Command::new("reg")
                .args(["delete", &hkcu_key, "/f"])
                .creation_flags(CREATE_NO_WINDOW)
                .status()?;
            if status.success() {
                info!("[uri_scheme] {scheme}:// unregistered");
                any = true;
            }
        }
        Ok(any)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_noop_on_non_windows() {
        // On non-Windows platforms this should return Ok(false) without error.
        #[cfg(not(target_os = "windows"))]
        {
            assert_eq!(register().unwrap(), false);
            assert_eq!(unregister().unwrap(), false);
            assert!(!is_registered());
        }
        // On Windows just call is_registered — don't actually write to the registry.
        #[cfg(target_os = "windows")]
        {
            // Smoke test: should not panic
            let _ = is_registered();
        }
    }
}
