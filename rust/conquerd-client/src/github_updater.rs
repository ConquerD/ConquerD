//! GitHub release updater — check for new ConquerD versions.
//!
//! Checks the GitHub Releases API for newer versions and spawns the
//! `conquerd-installer` binary to apply updates.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

pub const GITHUB_API: &str = "https://api.github.com";
pub const DEFAULT_REPO: &str = "vbawol/ConquerD";
/// Minimum interval between auto-checks.
pub const CHECK_INTERVAL_SECS: u64 = 3600;

fn default_automatic_checks_enabled() -> bool {
    true
}

#[derive(Deserialize)]
struct UpdateSettings {
    #[serde(default = "default_automatic_checks_enabled")]
    update_check_enabled: bool,
}

/// Read the persisted automatic-update preference without requiring the Qt UI.
/// Missing, older, or malformed settings retain the default-enabled behavior.
pub fn automatic_checks_enabled(settings_path: &Path) -> bool {
    std::fs::read_to_string(settings_path)
        .ok()
        .and_then(|json| serde_json::from_str::<UpdateSettings>(&json).ok())
        .map(|settings| settings.update_check_enabled)
        .unwrap_or_else(default_automatic_checks_enabled)
}

/// Resolve the updater installed beside the running client.
pub fn installed_installer_path() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|path| sibling_installer_path(&path))
}

fn sibling_installer_path(client_path: &Path) -> Option<PathBuf> {
    client_path.parent().map(|directory| {
        directory.join(format!(
            "conquerd-installer{}",
            std::env::consts::EXE_SUFFIX
        ))
    })
}

fn installer_arguments(repo: &str) -> Vec<OsString> {
    [
        "--update-and-relaunch",
        "--silent",
        "--kill",
        "--repo",
        repo,
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    pub tag_name: String,
    pub name: Option<String>,
    pub body: Option<String>,
    pub html_url: String,
}

impl ReleaseInfo {
    /// Version string stripped of a leading `v`.
    pub fn version(&self) -> &str {
        self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)
    }
}

/// Events emitted by the updater.
#[derive(Debug, Clone)]
pub enum UpdateEvent {
    /// A newer release was found.
    UpdateAvailable(ReleaseInfo),
    /// Currently on the latest release.
    AlreadyLatest,
    /// Check failed.
    CheckError(String),
    /// Installer launched successfully.
    InstallerStarted,
    /// Installer launch failed.
    InstallerError(String),
}

// ---------------------------------------------------------------------------
// Version comparison
// ---------------------------------------------------------------------------

/// Returns `true` if `candidate` is newer than `current` (semver ordering).
pub fn is_newer(current: &str, candidate: &str) -> bool {
    parse_semver(candidate)
        .zip(parse_semver(current))
        .map(|(c, cur)| c > cur)
        .unwrap_or(false)
}

fn parse_semver(v: &str) -> Option<(u64, u64, u64)> {
    let stripped = v.strip_prefix('v').unwrap_or(v);
    let parts: Vec<&str> = stripped.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let major = parts[0].parse::<u64>().ok()?;
    let minor = parts[1].parse::<u64>().ok()?;
    let patch = parts[2].split('-').next()?.parse::<u64>().ok()?;
    Some((major, minor, patch))
}

// ---------------------------------------------------------------------------
// Updater
// ---------------------------------------------------------------------------

/// Background update checker.
///
/// Use [`Updater::split`] to get channels, then spawn the future.
pub struct Updater {
    current_version: String,
    repo: String,
    installer_path: Option<PathBuf>,
    automatic_checks_enabled: bool,

    event_tx: mpsc::Sender<UpdateEvent>,
    cmd_rx: mpsc::Receiver<UpdaterCommand>,
}

#[derive(Debug)]
pub enum UpdaterCommand {
    /// Trigger an immediate check.
    Check,
    /// Enable or disable startup and hourly automatic checks.
    SetAutomaticChecks(bool),
    /// Apply the given release by launching the installer.
    ApplyUpdate(ReleaseInfo),
    Shutdown,
}

impl Updater {
    pub fn split(
        current_version: impl Into<String>,
        repo: impl Into<String>,
        installer_path: Option<PathBuf>,
        automatic_checks_enabled: bool,
    ) -> (
        mpsc::Sender<UpdaterCommand>,
        mpsc::Receiver<UpdateEvent>,
        impl std::future::Future<Output = ()>,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<UpdateEvent>(16);
        let (cmd_tx, cmd_rx) = mpsc::channel::<UpdaterCommand>(8);
        let u = Self {
            current_version: current_version.into(),
            repo: repo.into(),
            installer_path,
            automatic_checks_enabled,
            event_tx,
            cmd_rx,
        };
        (cmd_tx, event_rx, u.run())
    }

    async fn check_github(&self) -> Result<Option<ReleaseInfo>, String> {
        let url = format!("{}/repos/{}/releases/latest", GITHUB_API, self.repo);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("conquerd-client/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("HTTP client build: {e}"))?;

        let resp = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| format!("HTTP request: {e}"))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None); // no releases yet
        }
        if !resp.status().is_success() {
            return Err(format!("GitHub API returned {}", resp.status()));
        }

        let release: ReleaseInfo = resp.json().await.map_err(|e| format!("JSON parse: {e}"))?;
        Ok(Some(release))
    }

    async fn check_and_publish(&self) {
        let event = match self.check_github().await {
            Ok(Some(release)) if is_newer(&self.current_version, release.version()) => {
                UpdateEvent::UpdateAvailable(release)
            }
            Ok(_) => UpdateEvent::AlreadyLatest,
            Err(error) => UpdateEvent::CheckError(error),
        };
        let _ = self.event_tx.send(event).await;
    }

    async fn run(mut self) {
        info!("Updater started (current: {})", self.current_version);
        let check_interval = Duration::from_secs(CHECK_INTERVAL_SECS);
        let first_check = if self.automatic_checks_enabled {
            tokio::time::Instant::now()
        } else {
            tokio::time::Instant::now() + check_interval
        };
        let mut interval = tokio::time::interval_at(first_check, check_interval);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if self.automatic_checks_enabled {
                        self.check_and_publish().await;
                    }
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        UpdaterCommand::Shutdown => break,
                        UpdaterCommand::Check => {
                            self.check_and_publish().await;
                        }
                        UpdaterCommand::SetAutomaticChecks(enabled) => {
                            let newly_enabled = enabled && !self.automatic_checks_enabled;
                            self.automatic_checks_enabled = enabled;
                            if newly_enabled {
                                self.check_and_publish().await;
                                interval.reset_after(check_interval);
                            }
                        }
                        UpdaterCommand::ApplyUpdate(_rel) => {
                            if let Some(path) = &self.installer_path {
                                match std::process::Command::new(path)
                                    .args(installer_arguments(&self.repo))
                                    .spawn()
                                {
                                    Ok(_) => {
                                        let _ = self.event_tx.send(UpdateEvent::InstallerStarted).await;
                                    }
                                    Err(e) => {
                                        let _ = self.event_tx.send(UpdateEvent::InstallerError(e.to_string())).await;
                                    }
                                }
                            } else {
                                let _ = self.event_tx.send(UpdateEvent::InstallerError(
                                    "No installer path configured".to_string()
                                )).await;
                            }
                        }
                    }
                }
                else => break,
            }
        }
        info!("Updater stopped");
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert!(is_newer("1.0.0", "1.0.1"));
        assert!(is_newer("1.0.9", "1.1.0"));
        assert!(is_newer("1.9.9", "2.0.0"));
        assert!(!is_newer("1.0.1", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn version_strips_v_prefix() {
        assert!(is_newer("v1.0.0", "v1.0.1"));
        assert!(is_newer("1.0.0", "v1.0.1"));
    }

    #[test]
    fn installer_is_resolved_beside_client_for_this_platform() {
        let client =
            Path::new("install").join(format!("DoubleSlash{}", std::env::consts::EXE_SUFFIX));
        let expected = Path::new("install").join(format!(
            "conquerd-installer{}",
            std::env::consts::EXE_SUFFIX
        ));

        assert_eq!(sibling_installer_path(&client), Some(expected));
    }

    #[test]
    fn update_handoff_reuses_checked_repository() {
        assert_eq!(
            installer_arguments("owner/project"),
            [
                "--update-and-relaunch",
                "--silent",
                "--kill",
                "--repo",
                "owner/project",
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn automatic_checks_default_on_for_missing_or_older_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");
        assert!(automatic_checks_enabled(&settings));

        std::fs::write(&settings, r#"{"theme":"dark"}"#).unwrap();
        assert!(automatic_checks_enabled(&settings));

        std::fs::write(&settings, "not valid JSON").unwrap();
        assert!(automatic_checks_enabled(&settings));
    }

    #[test]
    fn automatic_checks_follow_the_persisted_setting() {
        let dir = tempfile::tempdir().unwrap();
        let settings = dir.path().join("settings.json");

        std::fs::write(&settings, r#"{"update_check_enabled":false}"#).unwrap();
        assert!(!automatic_checks_enabled(&settings));

        std::fs::write(&settings, r#"{"update_check_enabled":true}"#).unwrap();
        assert!(automatic_checks_enabled(&settings));
    }

    #[tokio::test]
    async fn disabled_automatic_checks_make_no_startup_request() {
        let (commands, mut events, updater) =
            Updater::split("1.0.0", "invalid/repository", None, false);
        let task = tokio::spawn(updater);
        tokio::task::yield_now().await;

        assert!(matches!(
            events.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        commands.send(UpdaterCommand::Shutdown).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("disabled updater must remain responsive")
            .unwrap();
    }
}
