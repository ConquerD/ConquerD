use crate::release_manifest;
use crate::state::{self, InstallState, CHANNEL_NIGHTLY};
use anyhow::{bail, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const GITHUB_API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("ConquerD-Installer/", env!("CARGO_PKG_VERSION"));
const NIGHTLY_TAG: &str = "nightly";
const NIGHTLY_DOWNLOAD_BASE: &str =
    "https://github.com/ConquerD/ConquerD/releases/download/nightly";

#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    #[allow(dead_code)]
    pub tag: String,
    pub version: String,
    pub archive_url: String,
    pub archive_name: String,
    pub sha256_url: String,
    /// URL for the signed releases_manifest.json asset, if present.
    pub manifest_url: String,
}

/// Human-facing details for the update-available UI.
#[derive(Debug, Clone, Default)]
pub struct UpdateDetails {
    /// e.g. `"1.0.0"` or `"1.2.3"` (semver without leading `v`).
    pub version: String,
    /// `"nightly"` when on the nightly channel, else empty.
    pub channel: String,
    /// e.g. `"2026-07-10"` or `"2026-07-10 14:32 UTC"`.
    pub datetime: String,
    /// Archive SHA-256 hex (lowercase).
    pub hash: String,
}

impl UpdateDetails {
    /// One-line title: `v1.0.0 nightly` or `v1.2.3`.
    pub fn version_line(&self) -> String {
        let ver = self.version.trim_start_matches(['v', 'V']);
        if self.channel.is_empty() {
            format!("v{ver}")
        } else {
            format!("v{ver} {}", self.channel)
        }
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// True when the running executable is the nightly installer build.
pub fn is_nightly_installer() -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()))
        .map(|name| name.contains("nightly"))
        .unwrap_or(false)
}

/// Resolve whether this install session should use the nightly channel.
pub fn resolve_nightly_channel(base_dir: &Path) -> bool {
    if is_nightly_installer() {
        return true;
    }
    state::read_state(base_dir)
        .map(|st| st.channel == CHANNEL_NIGHTLY)
        .unwrap_or(false)
}

/// Platform id used in releases_manifest.json entries.
pub fn current_platform_id() -> &'static str {
    #[cfg(windows)]
    {
        "win64"
    }
    #[cfg(target_os = "macos")]
    {
        "macos-arm64"
    }
    #[cfg(target_os = "linux")]
    {
        "linux-x86_64"
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        "unknown"
    }
}

/// Windows client nightly `.7z` published on the `nightly` GitHub release.
pub const WINDOWS_CLIENT_NIGHTLY_7Z: &str = "ConquerD-nightly-win64.7z";

/// Platform-specific nightly archive published on the `nightly` GitHub release.
pub fn nightly_archive_name() -> &'static str {
    #[cfg(windows)]
    {
        WINDOWS_CLIENT_NIGHTLY_7Z
    }
    #[cfg(target_os = "macos")]
    {
        "ConquerD-nightly-macos-arm64.dmg"
    }
    #[cfg(target_os = "linux")]
    {
        "ConquerD-nightly-x86_64.AppImage"
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        "ConquerD-nightly.7z"
    }
}

/// Fetch the rolling nightly release via direct asset URLs (no GitHub API).
pub fn fetch_nightly_release() -> anyhow::Result<ReleaseInfo> {
    let archive_name = nightly_archive_name();
    let archive_url = format!("{NIGHTLY_DOWNLOAD_BASE}/{archive_name}");
    let sha256_url = format!("{archive_url}.sha256");

    Ok(ReleaseInfo {
        tag: NIGHTLY_TAG.to_string(),
        version: NIGHTLY_TAG.to_string(),
        archive_url,
        archive_name: archive_name.to_string(),
        sha256_url,
        manifest_url: format!("{NIGHTLY_DOWNLOAD_BASE}/releases_manifest.json"),
    })
}

/// Fetch and parse the nightly manifest, returning the archive hash for this platform.
pub fn nightly_remote_hash(release: &ReleaseInfo) -> anyhow::Result<String> {
    if release.manifest_url.is_empty() {
        bail!("Nightly release is missing releases_manifest.json URL");
    }

    let raw_json = fetch_release_manifest(&release.manifest_url)?;
    let mf = release_manifest::ReleaseManifest::parse_unsigned(&raw_json)?;
    mf.build_hash_for_platform(current_platform_id())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No nightly manifest entry for platform {}",
                current_platform_id()
            )
        })
}

/// Fetch nightly manifest details used by the update prompt UI.
pub fn nightly_update_details(release: &ReleaseInfo) -> anyhow::Result<UpdateDetails> {
    if release.manifest_url.is_empty() {
        bail!("Nightly release is missing releases_manifest.json URL");
    }

    let raw_json = fetch_release_manifest(&release.manifest_url)?;
    let mf = release_manifest::ReleaseManifest::parse_unsigned(&raw_json)?;
    let entry = mf
        .entry_for_platform(current_platform_id())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No nightly manifest entry for platform {}",
                current_platform_id()
            )
        })?;

    Ok(UpdateDetails {
        // Installer package version is the product version; channel is separate.
        version: env!("CARGO_PKG_VERSION").to_string(),
        channel: "nightly".to_string(),
        datetime: format_release_datetime(entry.published_at, &entry.version, &entry.build_id),
        hash: entry.build_hash.to_lowercase(),
    })
}

/// Best-effort human datetime from manifest fields.
fn format_release_datetime(published_at: f64, version: &str, build_id: &str) -> String {
    if published_at > 0.0 {
        let secs = published_at.floor() as i64;
        if let Some(s) = format_unix_utc(secs) {
            return s;
        }
    }
    // Prefer YYYYMMDD embedded in `nightly-20260710`.
    if let Some(date) = version.strip_prefix("nightly-") {
        if date.len() >= 8 && date.as_bytes().iter().take(8).all(u8::is_ascii_digit) {
            let y = &date[0..4];
            let m = &date[4..6];
            let d = &date[6..8];
            return format!("{y}-{m}-{d}");
        }
    }
    // build_id may end with a run number; leave blank rather than inventing.
    let _ = build_id;
    String::new()
}

fn format_unix_utc(secs: i64) -> Option<String> {
    // Manual UTC formatting avoids pulling in chrono for one display string.
    // Algorithm: civil_from_days (Howard Hinnant).
    if secs < 0 {
        return None;
    }
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    Some(format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC"))
}

/// Build UI details for a remote release (nightly or stable).
pub fn update_details_for_release(
    release: &ReleaseInfo,
    nightly: bool,
) -> anyhow::Result<UpdateDetails> {
    if nightly {
        if let Ok(d) = nightly_update_details(release) {
            return Ok(d);
        }
        // Fall back to .sha256 when the manifest is unavailable.
        let hash = if !release.sha256_url.is_empty() {
            fetch_sha256(&release.sha256_url).unwrap_or_default()
        } else {
            String::new()
        };
        return Ok(UpdateDetails {
            version: env!("CARGO_PKG_VERSION").to_string(),
            channel: "nightly".to_string(),
            datetime: String::new(),
            hash,
        });
    }

    Ok(UpdateDetails {
        version: release.version.clone(),
        channel: String::new(),
        datetime: String::new(),
        hash: if !release.sha256_url.is_empty() {
            fetch_sha256(&release.sha256_url).unwrap_or_default()
        } else {
            String::new()
        },
    })
}

/// Fetch either the rolling nightly build or the latest stable GitHub release.
pub fn fetch_release(repo: &str, nightly: bool) -> anyhow::Result<ReleaseInfo> {
    if nightly {
        fetch_nightly_release()
    } else {
        fetch_latest_release(repo)
    }
}

/// Decide whether a remote release should be installed over the current one.
pub fn needs_release_update(
    release: &ReleaseInfo,
    st: &InstallState,
    nightly: bool,
) -> anyhow::Result<bool> {
    if st.current_version.is_empty() {
        return Ok(true);
    }

    if nightly {
        if !release.manifest_url.is_empty() {
            if let Ok(remote_hash) = nightly_remote_hash(release) {
                return Ok(nightly_update_available(&remote_hash, st));
            }
        }
        if !release.sha256_url.is_empty() {
            let remote_sha = fetch_sha256(&release.sha256_url)?;
            return Ok(nightly_update_available(&remote_sha, st));
        }
        return Ok(true);
    }

    Ok(state::is_newer(&release.version, &st.current_version))
}

/// Returns true when a nightly build should replace the installed copy.
pub fn nightly_update_available(remote_sha: &str, st: &InstallState) -> bool {
    if st.archive_sha256.is_empty() {
        return true;
    }
    remote_sha != st.archive_sha256
}

/// Fetch the latest release from a GitHub repo (e.g. "ConquerD/ConquerD").
pub fn fetch_latest_release(repo: &str) -> anyhow::Result<ReleaseInfo> {
    let url = format!("{GITHUB_API}/repos/{repo}/releases/latest");

    let resp = ureq::get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", USER_AGENT)
        .call()
        .context("Failed to query GitHub releases")?;

    let release: GhRelease = resp.into_json().context("Failed to parse release JSON")?;

    let tag = release.tag_name;
    let version = tag.trim_start_matches(['v', 'V']).to_string();

    let mut archive_url = String::new();
    let mut archive_name = String::new();
    let mut sha256_url = String::new();
    let mut manifest_url = String::new();

    for asset in &release.assets {
        if asset.name.ends_with(".7z") {
            archive_url = asset.browser_download_url.clone();
            archive_name = asset.name.clone();
        } else if asset.name.ends_with(".sha256") {
            sha256_url = asset.browser_download_url.clone();
        } else if asset.name == "releases_manifest.json" {
            manifest_url = asset.browser_download_url.clone();
        }
    }

    if archive_url.is_empty() {
        bail!("No .7z asset found in release {tag}");
    }

    Ok(ReleaseInfo {
        tag,
        version,
        archive_url,
        archive_name,
        sha256_url,
        manifest_url,
    })
}

/// Download and return the raw text of the signed release manifest.
pub fn fetch_release_manifest(url: &str) -> anyhow::Result<String> {
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .context("Failed to download releases_manifest.json")?;
    resp.into_string().context("Failed to read manifest body")
}

/// Fetch the SHA-256 checksum string from a .sha256 asset URL.
pub fn fetch_sha256(url: &str) -> anyhow::Result<String> {
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .context("Failed to download .sha256 file")?;

    let content = resp.into_string().context("Failed to read .sha256 body")?;
    // Format: "<hash>  <filename>" or just "<hash>"
    let hash = content
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_lowercase();

    if hash.len() != 64 {
        bail!("Invalid SHA-256 hash in checksum file: '{hash}'");
    }

    Ok(hash)
}

/// Download a file to `dest`, calling `progress(bytes_downloaded, total_bytes)`
/// periodically. Returns the path written.
pub fn download_file(
    url: &str,
    dest: &Path,
    progress: impl Fn(u64, u64),
) -> anyhow::Result<PathBuf> {
    let resp = ureq::get(url)
        .set("User-Agent", USER_AGENT)
        .call()
        .context("Download request failed")?;

    let total: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut reader = resp.into_reader();
    let mut file =
        std::fs::File::create(dest).with_context(|| format!("Cannot create {}", dest.display()))?;

    let mut downloaded: u64 = 0;
    let mut buf = [0u8; 64 * 1024];

    loop {
        let n = reader
            .read(&mut buf)
            .context("Read error during download")?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .context("Write error during download")?;
        downloaded += n as u64;
        progress(downloaded, total);
    }

    file.flush()?;
    Ok(dest.to_path_buf())
}

/// Verify a downloaded file against an expected SHA-256 hex digest.
pub fn verify_download(path: &Path, expected: &str) -> anyhow::Result<()> {
    let data = std::fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(&data));

    if actual != expected {
        bail!(
            "SHA-256 mismatch!\n  Expected: {expected}\n  Actual:   {actual}\nThe download may be corrupted."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_nightly_release_uses_direct_github_download_urls() {
        let release = fetch_nightly_release().expect("nightly release info");
        let archive_name = nightly_archive_name();

        assert_eq!(release.tag, "nightly");
        assert_eq!(release.version, "nightly");
        assert_eq!(release.archive_name, archive_name);
        assert_eq!(
            release.archive_url,
            format!("{NIGHTLY_DOWNLOAD_BASE}/{archive_name}")
        );
        assert_eq!(
            release.sha256_url,
            format!("{NIGHTLY_DOWNLOAD_BASE}/{archive_name}.sha256")
        );
        assert_eq!(
            release.manifest_url,
            format!("{NIGHTLY_DOWNLOAD_BASE}/releases_manifest.json")
        );
    }

    #[test]
    fn update_details_version_line_nightly() {
        let d = UpdateDetails {
            version: "1.0.0".into(),
            channel: "nightly".into(),
            datetime: "2026-07-10".into(),
            hash: "abc".into(),
        };
        assert_eq!(d.version_line(), "v1.0.0 nightly");
    }

    #[test]
    fn update_details_version_line_stable() {
        let d = UpdateDetails {
            version: "1.2.3".into(),
            channel: String::new(),
            datetime: String::new(),
            hash: String::new(),
        };
        assert_eq!(d.version_line(), "v1.2.3");
    }

    #[test]
    fn format_release_datetime_from_nightly_version() {
        let s = format_release_datetime(0.0, "nightly-20260710", "");
        assert_eq!(s, "2026-07-10");
    }

    #[test]
    fn format_release_datetime_from_unix() {
        // 2024-01-01 00:00:00 UTC
        let s = format_release_datetime(1_704_067_200.0, "1.0.0", "");
        assert!(s.starts_with("2024-01-01"), "got {s}");
        assert!(s.ends_with("UTC"), "got {s}");
    }

    #[test]
    fn nightly_update_available_when_hashes_differ() {
        let mut st = InstallState::empty();
        st.archive_sha256 = "abc".to_string();
        assert!(nightly_update_available("def", &st));
    }

    #[test]
    fn nightly_update_available_when_hashes_match() {
        let mut st = InstallState::empty();
        st.archive_sha256 = "abc".to_string();
        assert!(!nightly_update_available("abc", &st));
    }

    #[test]
    fn nightly_update_available_when_local_hash_missing() {
        let st = InstallState::empty();
        assert!(nightly_update_available("abc", &st));
    }
}
