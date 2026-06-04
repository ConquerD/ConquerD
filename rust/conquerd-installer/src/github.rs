use anyhow::{bail, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const GITHUB_API: &str = "https://api.github.com";
const USER_AGENT: &str = concat!("ConquerD-Installer/", env!("CARGO_PKG_VERSION"));

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

/// Fetch the latest release from a GitHub repo (e.g. "vbawol/ConquerD").
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
