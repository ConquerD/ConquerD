#![cfg_attr(windows, windows_subsystem = "windows")]

mod extract;
mod github;
mod gui;
mod manifest;
mod release_manifest;
mod shortcuts;
mod state;

use anyhow::{bail, Context};
use clap::Parser;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Global log file handle.
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// Write a line to the log file (and to stderr if a console is attached).
macro_rules! log {
    ($($arg:tt)*) => {{
        let msg = format!($($arg)*);
        // Try writing to stderr (works if a console is attached)
        let _ = eprintln!("{}", msg);
        // Always write to the log file
        if let Ok(mut guard) = $crate::LOG_FILE.lock() {
            if let Some(ref mut f) = *guard {
                let _ = writeln!(f, "{}", msg);
                let _ = f.flush();
            }
        }
    }};
}

/// Initialise file logging into `<base_dir>/installer.log`.
fn init_logging(base_dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(base_dir);
    let log_path = base_dir.join("installer.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let _ = writeln!(f, "\n--- {} ---", chrono_stamp());
        if let Ok(mut guard) = LOG_FILE.lock() {
            *guard = Some(f);
        }
    }
}

/// Simple timestamp without pulling in chrono.
fn chrono_stamp() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs();
    // Readable-enough UTC timestamp
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02} UTC")
}

/// On Windows, re-attach to the parent process console so that
/// `--silent` / `--uninstall` output appears on the calling terminal.
#[cfg(windows)]
fn attach_parent_console() {
    unsafe {
        // AttachConsole(ATTACH_PARENT_PROCESS)
        windows_sys::Win32::System::Console::AttachConsole(0xFFFFFFFF);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}

/// ConquerD Installer / Updater / Launcher
#[derive(Parser, Debug)]
#[command(name = "conquerd-installer", version, about)]
struct Cli {
    /// Path to a .7z archive to install from (skips download)
    #[arg(short, long)]
    archive: Option<PathBuf>,

    /// Base installation directory (default: %LOCALAPPDATA%\ConquerD)
    #[arg(short = 'd', long)]
    install_dir: Option<PathBuf>,

    /// Run in silent mode (no GUI)
    #[arg(short, long)]
    silent: bool,

    /// Skip shortcut creation
    #[arg(long)]
    no_shortcuts: bool,

    /// Perform uninstall (remove install dir and shortcuts)
    #[arg(long)]
    uninstall: bool,

    /// GitHub repo for downloading releases
    #[arg(long, default_value = "ConquerD/ConquerD")]
    repo: String,

    /// Check for updates, then launch the latest installed version (runner mode)
    #[arg(long)]
    launch: bool,

    /// Check for updates, install if available, then launch (called by the app)
    #[arg(long)]
    update_and_relaunch: bool,

    /// Kill running ConquerD.exe processes before updating
    #[arg(long)]
    kill: bool,

    /// Repair the current installation (verify and re-extract changed/missing files)
    #[arg(long)]
    repair: bool,
}

fn default_install_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ConquerD")
}

/// Windows client archives published by our build scripts:
/// `ConquerD-<version>-win64.7z` or `ConquerD-nightly-win64.7z`.
fn is_conquerd_client_archive(path: &std::path::Path) -> bool {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("7z"))
    {
        return false;
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    if stem.eq_ignore_ascii_case(github::nightly_archive_name().trim_end_matches(".7z")) {
        return true;
    }
    let prefix = "ConquerD-";
    let suffix = "-win64";
    if !stem.starts_with(prefix) || !stem.ends_with(suffix) {
        return false;
    }
    let version = &stem[prefix.len()..stem.len() - suffix.len()];
    version_token_is_semver(version)
}

fn version_token_is_semver(token: &str) -> bool {
    let core = token.split('-').next().unwrap_or(token);
    let parts: Vec<&str> = core.split('.').collect();
    if !(2..=4).contains(&parts.len()) {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Sort key for auto-detected archives: prefer newer semver releases over nightly.
fn archive_pick_rank(path: &std::path::Path) -> (u8, u64, u64, u64) {
    if let Some(v) = detect_version_from_archive(path) {
        let mut parts = v.split('.').filter_map(|p| p.parse::<u64>().ok());
        let major = parts.next().unwrap_or(0);
        let minor = parts.next().unwrap_or(0);
        let patch = parts.next().unwrap_or(0);
        return (1, major, minor, patch);
    }
    (0, 0, 0, 0)
}

/// Look for a ConquerD client .7z next to the running executable.
fn detect_archive() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| is_conquerd_client_archive(p))
        .collect();

    candidates.sort_by_key(|path| std::cmp::Reverse(archive_pick_rank(path)));
    candidates.into_iter().next()
}

/// Validate a .7z archive against a .sha256 sidecar file if one exists.
fn validate_sha256(archive: &std::path::Path) -> anyhow::Result<bool> {
    let sha_path = archive.with_extension("7z.sha256");
    if !sha_path.is_file() {
        return Ok(false);
    }

    let sha_content = std::fs::read_to_string(&sha_path)?;
    let expected = sha_content
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    if expected.len() != 64 {
        anyhow::bail!(
            "Invalid SHA-256 checksum in {}: '{}'",
            sha_path.display(),
            expected
        );
    }

    let data = std::fs::read(archive)?;
    let actual = format!("{:x}", Sha256::digest(&data));

    if actual != expected {
        anyhow::bail!("SHA-256 mismatch!\n  Expected: {expected}\n  Actual:   {actual}");
    }

    Ok(true)
}

/// Launch the ConquerD exe from the given versioned directory.
///
/// Re-verifies the executable's SHA-256 against the install manifest
/// immediately before spawning to close the extract→exec TOCTOU window.
fn launch_app(version_dir: &std::path::Path) -> anyhow::Result<()> {
    let exe = state::find_exe(version_dir).ok_or_else(|| {
        anyhow::anyhow!("No ConquerD executable found in {}", version_dir.display())
    })?;
    let working_dir = exe.parent().unwrap_or(version_dir);

    // Re-hash the binary immediately before exec and compare against the
    // manifest written at install time.  This prevents a race where a
    // process swaps the extracted file between installation and launch.
    let manifest_path = version_dir.join("manifest.json");
    if manifest_path.is_file() {
        if let Ok(Some(m)) = manifest::read_manifest(&manifest_path) {
            let rel = exe
                .strip_prefix(version_dir)
                .unwrap_or(&exe)
                .to_string_lossy()
                .replace('\\', "/");
            if let Some(expected_hash) = m.files.get(&rel) {
                let actual_hash = extract::hash_file(&exe)?;
                if actual_hash != *expected_hash {
                    anyhow::bail!(
                        "Pre-launch integrity check failed for {}: \
                         expected {}, got {}",
                        exe.display(),
                        expected_hash,
                        actual_hash
                    );
                }
            }
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        std::process::Command::new(&exe)
            .current_dir(working_dir)
            .creation_flags(DETACHED_PROCESS)
            .spawn()?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new(&exe)
            .current_dir(working_dir)
            .spawn()?;
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let base_dir = cli.install_dir.clone().unwrap_or_else(default_install_dir);

    init_logging(&base_dir);

    // For CLI-only modes, re-attach to the parent console so output is visible.
    if cli.silent || cli.uninstall {
        attach_parent_console();
    }

    // ── Uninstall ───────────────────────────────────────────────────────
    if cli.uninstall {
        return run_uninstall(&base_dir);
    }

    // ── Launch mode: check for updates, then run latest installed version ─
    if cli.launch {
        return run_launch(&base_dir, &cli.repo);
    }

    // ── Update-and-relaunch mode (called by the running app) ────────────
    if cli.update_and_relaunch {
        if cli.kill {
            state::kill_running_instances();
        }
        return run_update_and_relaunch(&base_dir, &cli.repo, cli.no_shortcuts, cli.silent);
    }

    // ── Repair mode ─────────────────────────────────────────────────────
    if cli.repair {
        if cli.silent {
            attach_parent_console();
            return run_repair_silent(&base_dir);
        }
        let install_state =
            state::read_state(&base_dir).unwrap_or_else(|_| state::InstallState::empty());
        return gui::run_gui(gui::GuiConfig {
            archive: cli.archive,
            base_dir,
            no_shortcuts: cli.no_shortcuts,
            repo: cli.repo,
            kill: cli.kill,
            install_state,
            repair: true,
        });
    }

    // ── Archive provided or found alongside: install it ─────────────────
    let archive = cli.archive.clone().or_else(detect_archive);

    if cli.silent {
        if cli.kill {
            state::kill_running_instances();
        }
        return run_silent(&archive, &base_dir, &cli);
    }

    // ── GUI mode ────────────────────────────────────────────────────────
    // Read current install state to decide the starting flow
    let install_state =
        state::read_state(&base_dir).unwrap_or_else(|_| state::InstallState::empty());

    gui::run_gui(gui::GuiConfig {
        archive,
        base_dir,
        no_shortcuts: cli.no_shortcuts,
        repo: cli.repo,
        kill: cli.kill,
        install_state,
        repair: false,
    })
}

fn launchable_current_dir(st: &state::InstallState) -> Option<&std::path::Path> {
    st.current_path()
        .filter(|dir| state::find_exe(dir).is_some())
}

/// --launch: check for updates, then run the latest installed version.
fn run_launch(base_dir: &std::path::Path, repo: &str) -> anyhow::Result<()> {
    let st = state::read_state(base_dir)?;
    if launchable_current_dir(&st).is_some() {
        if let Err(e) = run_update_and_relaunch(base_dir, repo, false, true) {
            log!("Update check/launch failed: {e:#}");
            let fallback_state = state::read_state(base_dir).unwrap_or(st);
            if let Some(dir) = launchable_current_dir(&fallback_state) {
                log!("Launching installed version after update failure.");
                launch_app(dir)?;
                return Ok(());
            }
            return Err(e);
        }
        return Ok(());
    }
    // No valid install — fall through to GUI installer
    log!("No installed version found. Starting installer…");
    let install_state = st;
    gui::run_gui(gui::GuiConfig {
        archive: None,
        base_dir: base_dir.to_path_buf(),
        no_shortcuts: false,
        repo: "ConquerD/ConquerD".to_string(),
        kill: false,
        install_state,
        repair: false,
    })
}

/// --update-and-relaunch: check GitHub, install if newer, then launch.
fn run_update_and_relaunch(
    base_dir: &std::path::Path,
    repo: &str,
    no_shortcuts: bool,
    silent: bool,
) -> anyhow::Result<()> {
    let mut st = state::read_state(base_dir)?;

    let nightly = github::resolve_nightly_channel(base_dir);
    let source = if nightly {
        format!("nightly channel ({})", github::nightly_archive_name())
    } else {
        format!("{repo} latest release")
    };
    log!("Checking for updates from {source}…");
    let release = match github::fetch_release(repo, nightly) {
        Ok(r) => r,
        Err(e) => {
            log!("Update check failed: {e:#}");
            // Launch current version anyway
            if let Some(dir) = st.current_path() {
                launch_app(dir)?;
            }
            return Ok(());
        }
    };

    let needs_update = match github::needs_release_update(&release, &st, nightly) {
        Ok(v) => v,
        Err(e) => {
            log!("Update check failed: {e:#}");
            if let Some(dir) = st.current_path() {
                launch_app(dir)?;
            }
            return Ok(());
        }
    };

    if !needs_update {
        log!("Already up to date (v{}).", st.current_version);
        if let Some(dir) = st.current_path() {
            launch_app(dir)?;
        }
        return Ok(());
    }

    log!("Updating to v{}…", release.version);

    if silent {
        // Download, extract, update state, launch — all non-interactive
        let temp = std::env::temp_dir().join(&release.archive_name);
        github::download_file(&release.archive_url, &temp, |_, _| {})?;

        let archive_sha256 = if !release.sha256_url.is_empty() {
            let expected = github::fetch_sha256(&release.sha256_url)?;
            github::verify_download(&temp, &expected)?;
            expected
        } else {
            String::new()
        };

        // Cross-check against the release manifest when available.
        if !release.manifest_url.is_empty() {
            let raw_json = github::fetch_release_manifest(&release.manifest_url)
                .with_context(|| format!("Failed to fetch {}", release.manifest_url))?;
            let archive_hash = extract::hash_file(&temp)?;
            release_manifest::verify_archive_hash(
                &raw_json,
                nightly,
                &release.version,
                github::current_platform_id(),
                &archive_hash,
            )?;
            log!(
                "Release manifest verified for {}.",
                if nightly {
                    format!("nightly ({})", github::current_platform_id())
                } else {
                    format!("v{}", release.version)
                }
            );
        } else if nightly {
            bail!("Nightly install requires releases_manifest.json");
        }

        let ver_dir = state::version_dir(base_dir, &release.version);
        std::fs::create_dir_all(&ver_dir)?;
        extract::extract_7z(&temp, &ver_dir)?;

        // Remove old versions
        let old: Vec<_> = st.old_versions().into_iter().cloned().collect();
        for old_ver in &old {
            let _ = std::fs::remove_dir_all(&old_ver.path);
            st.remove_version(&old_ver.version);
        }

        st.add_version(&release.version, &ver_dir);
        st.set_channel(nightly);
        st.archive_sha256 = archive_sha256;
        state::write_state(base_dir, &st)?;
        state::self_copy(base_dir, nightly)?;

        if !no_shortcuts {
            let installer_in_base = state::installer_path(base_dir, nightly);
            shortcuts::create_shortcuts_for_launcher(&installer_in_base)?;
        }

        log!("Updated to v{}. Launching…", release.version);
        launch_app(&ver_dir)?;
        return Ok(());
    }

    // Non-silent: use GUI for the update flow
    gui::run_gui(gui::GuiConfig {
        archive: None,
        base_dir: base_dir.to_path_buf(),
        no_shortcuts,
        repo: repo.to_string(),
        kill: false,
        install_state: st,
        repair: false,
    })
}

/// Silent install from a local archive.
fn run_silent(
    archive: &Option<PathBuf>,
    base_dir: &std::path::Path,
    cli: &Cli,
) -> anyhow::Result<()> {
    let archive = archive.as_ref().ok_or_else(|| {
        anyhow::anyhow!("No .7z archive found. Use --archive or place it next to the installer.")
    })?;

    // Try to detect version from archive filename (ConquerD-1.2.3-win64.7z)
    let version = detect_version_from_archive(archive)
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let ver_dir = state::version_dir(base_dir, &version);
    log!("Installing v{version} to {}", ver_dir.display());

    std::fs::create_dir_all(&ver_dir)?;
    let extracted = extract::extract_7z(archive, &ver_dir)?;
    log!("Extracted {} files", extracted.len());

    let manifest_path = ver_dir.join("manifest.json");
    manifest::write_manifest(&ver_dir, &extracted, &manifest_path)?;

    // Update install state
    let mut st = state::read_state(base_dir)?;

    // Remove old versions
    let old: Vec<_> = st.old_versions().into_iter().cloned().collect();
    for old_ver in &old {
        let _ = std::fs::remove_dir_all(&old_ver.path);
        st.remove_version(&old_ver.version);
    }

    let nightly = github::resolve_nightly_channel(base_dir);
    st.add_version(&version, &ver_dir);
    st.set_channel(nightly);
    state::write_state(base_dir, &st)?;
    state::self_copy(base_dir, nightly)?;

    if !cli.no_shortcuts {
        let installer_in_base = state::installer_path(base_dir, nightly);
        shortcuts::create_shortcuts_for_launcher(&installer_in_base)?;
        log!("Shortcuts created");
    }

    log!("Installation complete (v{version})");
    Ok(())
}

/// --repair --silent: verify and repair the current installation from CLI.
fn run_repair_silent(base_dir: &std::path::Path) -> anyhow::Result<()> {
    let st = state::read_state(base_dir)?;
    if st.current_version.is_empty() {
        anyhow::bail!("No installed version found. Run the installer normally first.");
    }

    let ver_dir = state::version_dir(base_dir, &st.current_version);
    let manifest_path = ver_dir.join("manifest.json");

    let m = manifest::read_manifest(&manifest_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "No manifest.json in {}. Cannot repair — try a full reinstall.",
            ver_dir.display()
        )
    })?;

    log!(
        "Verifying {} files for v{}\u{2026}",
        m.files.len(),
        st.current_version
    );

    let report = extract::verify_install(&ver_dir, &m.files, |checked, total| {
        if checked % 100 == 0 || checked == total {
            log!("  checked {checked}/{total}");
        }
    })?;

    if report.is_ok() {
        log!("All {} files intact — nothing to repair.", report.checked);
        return Ok(());
    }

    log!(
        "Found {} changed, {} missing file(s).",
        report.changed.len(),
        report.missing.len()
    );

    // Look for a local archive
    let archive = detect_archive().ok_or_else(|| {
        anyhow::anyhow!(
            "No .7z archive found next to the installer. \
             Place the matching archive alongside the installer or use --archive."
        )
    })?;

    log!("Extracting {} for repair\u{2026}", archive.display());
    let staging_dir = std::env::temp_dir().join(format!("conquerd_repair_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging_dir);
    std::fs::create_dir_all(&staging_dir)?;

    extract::extract_7z(&archive, &staging_dir)?;

    let damaged_set: std::collections::HashSet<&str> = report
        .changed
        .iter()
        .chain(report.missing.iter())
        .map(|s| s.as_str())
        .collect();

    let mut repaired = 0usize;
    for rel_path in &damaged_set {
        let src = staging_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let dest = ver_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));

        if src.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dest)?;
            repaired += 1;
        } else {
            log!("  warning: {} not found in archive — skipping", rel_path);
        }
    }

    let _ = std::fs::remove_dir_all(&staging_dir);
    log!("Repair complete — {repaired} file(s) restored.");
    Ok(())
}

fn run_uninstall(base_dir: &std::path::Path) -> anyhow::Result<()> {
    state::kill_running_instances();
    shortcuts::remove_shortcuts()?;

    if base_dir.exists() {
        std::fs::remove_dir_all(base_dir)?;
        log!("Removed {}", base_dir.display());
    }
    log!("Uninstall complete");
    Ok(())
}

/// Try to extract a version string from an archive filename like `ConquerD-1.2.3-win64.7z`.
fn detect_version_from_archive(archive: &std::path::Path) -> Option<String> {
    let name = archive.file_stem()?.to_string_lossy().to_string();
    // Look for a pattern like X.Y.Z in the name
    let re_like: Vec<&str> = name.split('-').collect();
    for part in &re_like {
        let pieces: Vec<&str> = part.split('.').collect();
        if pieces.len() >= 2 && pieces.iter().all(|p| p.chars().all(|c| c.is_ascii_digit())) {
            return Some(part.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_conquerd_client_archive_accepts_release_and_nightly_names() {
        assert!(is_conquerd_client_archive(std::path::Path::new(
            "ConquerD-1.0.0-win64.7z"
        )));
        assert!(is_conquerd_client_archive(std::path::Path::new(
            "conquerd-nightly-win64.7z"
        )));
    }

    #[test]
    fn is_conquerd_client_archive_rejects_unrelated_seven_zip_files() {
        for name in [
            "backup.7z",
            "7z2301-x64.7z",
            "ConquerD-backup.7z",
            "ConquerD-1.0.0.7z",
            "conquerd-supernode-1.0.0-win64.zip",
        ] {
            assert!(
                !is_conquerd_client_archive(std::path::Path::new(name)),
                "unexpected match for {name}"
            );
        }
    }

    #[test]
    fn launchable_current_dir_requires_current_executable() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let ver_dir = tmp.path().join("conquerd_1.0.0");
        std::fs::create_dir_all(&ver_dir).expect("version dir");

        let mut st = state::InstallState::empty();
        st.add_version("1.0.0", &ver_dir);

        assert!(launchable_current_dir(&st).is_none());

        std::fs::write(ver_dir.join("ConquerD.exe"), b"test").expect("exe marker");

        assert_eq!(launchable_current_dir(&st), Some(ver_dir.as_path()));
    }
}
