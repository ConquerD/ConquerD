//! GitHub release updater — check for new ConquerD versions.
//!
//! Checks the GitHub Releases API for newer versions and spawns the
//! `conquerd-installer` binary to apply updates.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::info;

pub const GITHUB_API: &str = "https://api.github.com";
pub const DEFAULT_REPO: &str = "vbawol/ConquerD";
/// Minimum interval between auto-checks.
pub const CHECK_INTERVAL_SECS: u64 = 3600;

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

    event_tx: mpsc::Sender<UpdateEvent>,
    cmd_rx: mpsc::Receiver<UpdaterCommand>,
}

#[derive(Debug)]
pub enum UpdaterCommand {
    /// Trigger an immediate check.
    Check,
    /// Apply the given release by launching the installer.
    ApplyUpdate(ReleaseInfo),
    Shutdown,
}

impl Updater {
    pub fn split(
        current_version: impl Into<String>,
        repo: impl Into<String>,
        installer_path: Option<PathBuf>,
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

    async fn run(mut self) {
        info!("Updater started (current: {})", self.current_version);
        let mut interval = tokio::time::interval(Duration::from_secs(CHECK_INTERVAL_SECS));

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    // Auto-check on interval
                }
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        UpdaterCommand::Shutdown => break,
                        UpdaterCommand::Check => {
                            match self.check_github().await {
                                Ok(Some(rel)) => {
                                    if is_newer(&self.current_version, rel.version()) {
                                        let _ = self.event_tx.send(UpdateEvent::UpdateAvailable(rel)).await;
                                    } else {
                                        let _ = self.event_tx.send(UpdateEvent::AlreadyLatest).await;
                                    }
                                }
                                Ok(None) => {
                                    let _ = self.event_tx.send(UpdateEvent::AlreadyLatest).await;
                                }
                                Err(e) => {
                                    let _ = self.event_tx.send(UpdateEvent::CheckError(e)).await;
                                }
                            }
                        }
                        UpdaterCommand::ApplyUpdate(rel) => {
                            if let Some(path) = &self.installer_path {
                                match std::process::Command::new(path)
                                    .arg("--install")
                                    .arg(rel.tag_name)
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
}
