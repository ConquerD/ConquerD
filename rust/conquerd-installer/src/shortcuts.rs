use anyhow::Result;
use std::path::Path;

/// Create shortcuts that point to the installer exe as the launcher.
/// The shortcut target is `<installer_exe> --launch` so it always
/// launches the latest installed version without needing shortcut updates.
#[cfg(windows)]
pub fn create_shortcuts_for_launcher(installer_exe: &Path) -> Result<()> {
    if !installer_exe.exists() {
        anyhow::bail!("Installer not found at {}", installer_exe.display());
    }
    let exe_str = installer_exe.to_string_lossy();
    let working_dir = installer_exe
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // Start Menu shortcut
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let start_menu =
        std::path::PathBuf::from(&appdata).join("Microsoft\\Windows\\Start Menu\\Programs");
    if start_menu.exists() {
        let lnk = start_menu.join("ConquerD.lnk");
        create_lnk_shortcut(&lnk, &exe_str, "--launch", &working_dir)?;
    }

    // Desktop shortcut
    if let Some(desktop) = dirs::desktop_dir() {
        let lnk = desktop.join("ConquerD.lnk");
        create_lnk_shortcut(&lnk, &exe_str, "--launch", &working_dir)?;
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn create_shortcuts_for_launcher(installer_exe: &Path) -> Result<()> {
    if let Some(data_home) = dirs::data_dir() {
        let apps_dir = data_home.join("applications");
        std::fs::create_dir_all(&apps_dir)?;
        let desktop_entry = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=ConquerD\n\
             Comment=Private Voice & Chat\n\
             Exec={} --launch\n\
             Terminal=false\n\
             Categories=Network;Chat;\n",
            installer_exe.display()
        );
        std::fs::write(apps_dir.join("conquerd.desktop"), desktop_entry)?;
    }
    Ok(())
}

#[cfg(windows)]
fn create_lnk_shortcut(
    lnk_path: &Path,
    target: &str,
    arguments: &str,
    working_dir: &str,
) -> Result<()> {
    use std::process::Command;

    let script = format!(
        r#"$ws = New-Object -ComObject WScript.Shell; $s = $ws.CreateShortcut('{}'); $s.TargetPath = '{}'; $s.Arguments = '{}'; $s.WorkingDirectory = '{}'; $s.Description = 'ConquerD - Private Voice & Chat'; $s.Save()"#,
        lnk_path.to_string_lossy().replace('\'', "''"),
        target.replace('\'', "''"),
        arguments.replace('\'', "''"),
        working_dir.replace('\'', "''"),
    );

    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "Warning: shortcut creation failed for {}: {}",
            lnk_path.display(),
            stderr.trim()
        );
    }
    Ok(())
}

#[cfg(windows)]
pub fn remove_shortcuts() -> Result<()> {
    // Desktop shortcut
    if let Some(desktop) = dirs::desktop_dir() {
        let lnk = desktop.join("ConquerD.lnk");
        if lnk.exists() {
            std::fs::remove_file(&lnk)?;
        }
    }

    // Start Menu shortcut
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    let start_menu = std::path::PathBuf::from(appdata)
        .join("Microsoft\\Windows\\Start Menu\\Programs\\ConquerD.lnk");
    if start_menu.exists() {
        std::fs::remove_file(&start_menu)?;
    }

    Ok(())
}

#[cfg(not(windows))]
pub fn remove_shortcuts() -> Result<()> {
    if let Some(data_home) = dirs::data_dir() {
        let desktop_entry = data_home.join("applications/conquerd.desktop");
        if desktop_entry.exists() {
            std::fs::remove_file(&desktop_entry)?;
        }
    }
    Ok(())
}
