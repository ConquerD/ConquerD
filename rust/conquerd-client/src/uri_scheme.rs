//! URI scheme registration — registers the `conquerd://` protocol handler.
//!
//! On Windows: writes to HKCU\Software\Classes\conquerd (no elevation needed).
//! On other platforms: no-op.

#[cfg(not(target_os = "windows"))]
use tracing::debug;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Register the `conquerd://` URI scheme handler for the current user.
///
/// On Windows, points to `conquerd-installer.exe` in `%LOCALAPPDATA%\ConquerD`
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

/// Unregister the `conquerd://` URI scheme handler.
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
            let installer = PathBuf::from(&local)
                .join("ConquerD")
                .join("conquerd-installer.exe");
            if installer.exists() {
                return installer;
            }
        }
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("conquerd.exe"))
    }

    const ROOT: &str = r"Software\Classes\conquerd";

    pub fn is_registered() -> bool {
        // winreg is not a dependency — use raw registry APIs via std::process
        // to avoid pulling in the crate. Check existence by trying to read the
        // command value.
        let key_path = format!(r"{ROOT}\shell\open\command");
        matches!(
            std::process::Command::new("reg")
                .args(["query", &format!(r"HKCU\{key_path}"), "/ve"])
                .creation_flags(CREATE_NO_WINDOW)
                .output(),
            Ok(out) if out.status.success()
        )
    }

    pub fn register() -> std::io::Result<bool> {
        let exe = exe_path();
        let cmd = format!(r#""{}" "%1""#, exe.display());

        let entries: &[(&str, &str, &str)] = &[
            (ROOT, "", "URL:ConquerD Protocol"),
            (ROOT, "URL Protocol", ""),
            (&format!(r"{ROOT}\shell"), "", ""),
            (&format!(r"{ROOT}\shell\open"), "", ""),
            (&format!(r"{ROOT}\shell\open\command"), "", &cmd),
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

        info!("[uri_scheme] conquerd:// registered → {}", exe.display());
        Ok(true)
    }

    pub fn unregister() -> std::io::Result<bool> {
        let hkcu_key = format!(r"HKCU\{ROOT}");
        let status = std::process::Command::new("reg")
            .args(["delete", &hkcu_key, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if status.success() {
            info!("[uri_scheme] conquerd:// unregistered");
            Ok(true)
        } else {
            Ok(false)
        }
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
