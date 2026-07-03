use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use snm_core::Defaults;
use tar::Archive;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseArtifact {
    pub version: String,
    pub platform: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sha256_url: String,
}

#[derive(Debug, Clone)]
pub struct DownloadedSupernode {
    pub binary_path: PathBuf,
    pub artifact: ReleaseArtifact,
}

/// Which cargo-based build front-end to use when building from source.
///
/// | Tool | Install | When to use |
/// |---|---|---|
/// | `Cargo` | built-in | native builds; cross-compile if your host already has a matching linker |
/// | `Zigbuild` | `cargo install cargo-zigbuild` + [zig](https://ziglang.org/download/) | cross-compile Windows→Linux with no external GCC required |
/// | `Cross` | `cargo install cross` + Docker | fully hermetic cross-compile via container |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LocalBuildTool {
    #[default]
    Cargo,
    Zigbuild,
    Cross,
}

impl LocalBuildTool {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cargo" => Ok(Self::Cargo),
            "zigbuild" | "cargo-zigbuild" => Ok(Self::Zigbuild),
            "cross" => Ok(Self::Cross),
            other => bail!(
                "unknown build tool {other:?}; expected one of: cargo, zigbuild, cross"
            ),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Zigbuild => "cargo",   // cargo zigbuild is a cargo subcommand
            Self::Cross => "cross",
        }
    }
}

/// Build `conquerd-supernode` from a local Cargo project and return the path to
/// the compiled binary.
///
/// * `source_dir` — root of the `conquerd-supernode` Cargo package (the directory
///   containing its `Cargo.toml`).
/// * `target_triple` — Rust target triple, e.g. `"x86_64-unknown-linux-musl"`.
///   Pass `None` to build for the host platform (only useful when the host and
///   remote share the same OS/arch).
/// * `tool` — which build front-end to invoke.
pub fn build_local_binary(
    source_dir: &std::path::Path,
    target_triple: Option<&str>,
    tool: LocalBuildTool,
) -> Result<PathBuf> {
    if !source_dir.join("Cargo.toml").exists() {
        bail!(
            "source directory {} does not contain a Cargo.toml",
            source_dir.display()
        );
    }

    // Assemble: `cargo [zigbuild] build --release [--target <triple>]`
    let mut cmd = std::process::Command::new(tool.as_str());
    cmd.current_dir(source_dir);
    // `cargo zigbuild` is itself the subcommand (replaces `build`).
    // `cargo cross` similarly wraps `build`.
    if tool == LocalBuildTool::Zigbuild {
        cmd.arg("zigbuild");
    } else {
        cmd.arg("build");
    }
    cmd.arg("--release");
    if let Some(triple) = target_triple {
        cmd.arg("--target").arg(triple);
    }

    // On Windows, cargo-zigbuild needs zig.exe on PATH.  It is often installed
    // via WinGet (or a similar per-user location) but not added to the system
    // PATH.  Search a handful of well-known locations and prepend the directory
    // to the child process's PATH if zig.exe is found there.
    #[cfg(windows)]
    if tool == LocalBuildTool::Zigbuild {
        let zig_already_on_path = std::env::var("PATH")
            .unwrap_or_default()
            .split(';')
            .any(|dir| std::path::Path::new(dir).join("zig.exe").exists());

        if !zig_already_on_path {
            if let Some(zig_dir) = find_zig_dir_windows() {
                let path_val = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{};{}", zig_dir.display(), path_val));
            }
        }
    }

    // If the source path (or its default target dir) contains spaces the zig
    // linker will misparse it.  Redirect output to a space-free temp location.
    let alt_target: Option<std::path::PathBuf> = if source_dir.to_string_lossy().contains(' ') {
        let dir = std::env::temp_dir().join("snm-build").join("target");
        std::fs::create_dir_all(&dir).ok();
        cmd.env("CARGO_TARGET_DIR", &dir);
        Some(dir)
    } else {
        None
    };

    // Capture stderr (where cargo writes errors) while letting stdout pass through.
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {} (is it installed?)", tool.as_str()))?;
    let stderr_bytes = child
        .stderr
        .take()
        .map(|mut r| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut r, &mut buf).ok();
            buf
        })
        .unwrap_or_default();
    let status = child.wait().context("wait for cargo")?;

    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes);
        // Extract the most useful lines: error[…] and ^^ lines, capped at 40 lines.
        let error_lines: Vec<&str> = stderr
            .lines()
            .filter(|l| {
                l.contains("error[") || l.contains("error:") || l.starts_with("  -->")
            })
            .take(40)
            .collect();
        let detail = if error_lines.is_empty() {
            stderr.trim().lines().take(30).collect::<Vec<_>>().join("\n")
        } else {
            error_lines.join("\n")
        };
        bail!(
            "cargo build failed (exit {}):\n{detail}",
            status.code().unwrap_or(-1)
        );
    }

    // Locate the binary.
    // When CARGO_TARGET_DIR is overridden (alt_target), that path IS the target
    // dir and cargo writes directly into it: <alt_target>/<triple>/release/bin
    // When using the source dir, cargo creates a `target` sub-dir:
    //   <source_dir>/target/<triple>/release/bin
    let binary_name = if cfg!(target_os = "windows") && target_triple.is_none() {
        "conquerd-supernode.exe"
    } else {
        "conquerd-supernode"
    };
    let target_dir: PathBuf = match &alt_target {
        Some(alt) => alt.clone(),
        None => source_dir.join("target"),
    };
    let binary_path = match target_triple {
        Some(triple) => target_dir.join(triple).join("release").join(binary_name),
        None => target_dir.join("release").join(binary_name),
    };

    if !binary_path.exists() {
        bail!(
            "build succeeded but binary not found at expected path: {}",
            binary_path.display()
        );
    }
    Ok(binary_path)
}

pub fn resolve_release_artifact(defaults: &Defaults, platform: &str) -> Result<ReleaseArtifact> {
    let version = defaults.version.trim();
    if version.is_empty() {
        bail!("defaults.version must not be empty");
    }
    if version == "local" {
        bail!("version = \"local\" does not resolve to a GitHub release artifact");
    }

    let platform = normalize_release_platform(platform)?;
    let tag = release_tag(version);
    let asset_name = asset_name(version, platform)?;
    let repo = release_repo(defaults);
    let asset_url = format!("https://github.com/{repo}/releases/download/{tag}/{asset_name}");
    Ok(ReleaseArtifact {
        version: version.into(),
        platform: platform.into(),
        sha256_url: format!("{asset_url}.sha256"),
        asset_url,
        asset_name,
    })
}

pub async fn download_supernode_artifact(
    defaults: &Defaults,
    platform: &str,
) -> Result<DownloadedSupernode> {
    let artifact = resolve_release_artifact(defaults, platform)?;
    let archive = download_bytes(&artifact.asset_url)
        .await
        .with_context(|| format!("download {}", artifact.asset_url))?;
    let sha_sidecar = download_text(&artifact.sha256_url)
        .await
        .with_context(|| format!("download {}", artifact.sha256_url))?;
    verify_sha256(&archive, &sha_sidecar).context("verify release sha256")?;

    let binary = extract_supernode_binary(&archive, &artifact)?;
    let path = cache_path(&artifact, "conquerd-supernode")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create cache dir {}", parent.display()))?;
    }
    fs::write(&path, binary).with_context(|| format!("write {}", path.display()))?;

    Ok(DownloadedSupernode {
        binary_path: path,
        artifact,
    })
}

fn release_repo(defaults: &Defaults) -> String {
    std::env::var("SNM_SUPERNODE_RELEASE_REPO")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| defaults.release_repo.clone())
}

fn release_tag(version: &str) -> String {
    match version {
        "nightly" | "latest-nightly" | "latest" => "nightly".into(),
        v if v.starts_with('v') => v.into(),
        v => format!("v{v}"),
    }
}

fn asset_name(version: &str, platform: &str) -> Result<String> {
    let release_version = match version {
        "nightly" | "latest-nightly" | "latest" => "nightly",
        v => v,
    };
    let ext = match platform {
        "linux-x86_64" | "linux-aarch64" => "tar.gz",
        "win64" => "zip",
        other => bail!("no supernode GitHub release asset is defined for platform {other}"),
    };
    Ok(format!(
        "conquerd-supernode-{release_version}-{platform}.{ext}"
    ))
}

fn normalize_release_platform(platform: &str) -> Result<&str> {
    match platform {
        "linux-x86_64" | "linux-aarch64" | "win64" => Ok(platform),
        "linux-amd64" => Ok("linux-x86_64"),
        "linux-arm64" => Ok("linux-aarch64"),
        other => bail!("unsupported release platform {other}"),
    }
}

async fn download_bytes(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::get(url).await?;
    if !response.status().is_success() {
        bail!("GET {url} returned {}", response.status());
    }
    Ok(response.bytes().await?.to_vec())
}

async fn download_text(url: &str) -> Result<String> {
    let response = reqwest::get(url).await?;
    if !response.status().is_success() {
        bail!("GET {url} returned {}", response.status());
    }
    Ok(response.text().await?)
}

fn verify_sha256(bytes: &[u8], sidecar: &str) -> Result<()> {
    let expected = sidecar
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("empty sha256 sidecar"))?
        .to_ascii_lowercase();
    if expected.len() != 64 || !expected.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("invalid sha256 sidecar");
    }

    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        bail!("sha256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn extract_supernode_binary(archive_bytes: &[u8], artifact: &ReleaseArtifact) -> Result<Vec<u8>> {
    match artifact.asset_name.as_str() {
        name if name.ends_with(".tar.gz") => extract_from_tar_gz(archive_bytes),
        name if name.ends_with(".zip") => {
            bail!("Windows zip supernode installs are not supported yet")
        }
        _ => bail!("unsupported artifact type: {}", artifact.asset_name),
    }
}

fn extract_from_tar_gz(archive_bytes: &[u8]) -> Result<Vec<u8>> {
    let gz = GzDecoder::new(Cursor::new(archive_bytes));
    let mut archive = Archive::new(gz);
    for entry in archive.entries().context("read tar entries")? {
        let mut entry = entry.context("read tar entry")?;
        let path = entry.path().context("read tar path")?;
        if path.file_name().and_then(|name| name.to_str()) == Some("conquerd-supernode") {
            let mut binary = Vec::new();
            entry
                .read_to_end(&mut binary)
                .context("extract conquerd-supernode")?;
            return Ok(binary);
        }
    }
    bail!("archive did not contain conquerd-supernode");
}

fn cache_path(artifact: &ReleaseArtifact, filename: &str) -> Result<PathBuf> {
    let repo = artifact
        .asset_url
        .strip_prefix("https://github.com/")
        .and_then(|rest| rest.split_once("/releases/download/").map(|(repo, _)| repo))
        .unwrap_or("unknown/repo")
        .replace(['/', '\\'], "_");
    Ok(std::env::temp_dir()
        .join("supernode-manager")
        .join(repo)
        .join(&artifact.version)
        .join(&artifact.platform)
        .join(filename))
}

/// On Windows, search well-known locations for a directory containing `zig.exe`.
///
/// Checks (in order):
/// 1. `%LOCALAPPDATA%\Microsoft\WinGet\Packages\zig.zig*\zig-*\`  (winget install zig.zig)
/// 2. `%LOCALAPPDATA%\zig\`
/// 3. `C:\zig\`
/// 4. `%USERPROFILE%\.zig\`
#[cfg(windows)]
fn find_zig_dir_windows() -> Option<PathBuf> {
    // 1. WinGet packages directory
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let winget_base = PathBuf::from(&local_app_data)
            .join("Microsoft")
            .join("WinGet")
            .join("Packages");
        if let Ok(entries) = std::fs::read_dir(&winget_base) {
            for pkg in entries.flatten() {
                let pkg_name = pkg.file_name();
                if pkg_name.to_string_lossy().starts_with("zig.zig") {
                    // Each zig version lives in a versioned sub-directory
                    if let Ok(subs) = std::fs::read_dir(pkg.path()) {
                        for sub in subs.flatten() {
                            if sub.path().join("zig.exe").exists() {
                                return Some(sub.path());
                            }
                        }
                    }
                    // Also check the package dir itself
                    if pkg.path().join("zig.exe").exists() {
                        return Some(pkg.path());
                    }
                }
            }
        }

        // 2. %LOCALAPPDATA%\zig
        let local_zig = PathBuf::from(&local_app_data).join("zig");
        if local_zig.join("zig.exe").exists() {
            return Some(local_zig);
        }
    }

    // 3. C:\zig
    let c_zig = PathBuf::from(r"C:\zig");
    if c_zig.join("zig.exe").exists() {
        return Some(c_zig);
    }

    // 4. %USERPROFILE%\.zig
    if let Ok(home) = std::env::var("USERPROFILE") {
        let home_zig = PathBuf::from(home).join(".zig");
        if home_zig.join("zig.exe").exists() {
            return Some(home_zig);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use snm_core::Defaults;

    use super::*;

    #[test]
    fn nightly_linux_x86_64_uses_fixed_github_asset() {
        let defaults = Defaults::default();
        let artifact = resolve_release_artifact(&defaults, "linux-x86_64").unwrap();
        assert_eq!(
            artifact.asset_name,
            "conquerd-supernode-nightly-linux-x86_64.tar.gz"
        );
        assert_eq!(artifact.asset_url, "https://github.com/ConquerD/ConquerD/releases/download/nightly/conquerd-supernode-nightly-linux-x86_64.tar.gz");
        assert_eq!(artifact.sha256_url, "https://github.com/ConquerD/ConquerD/releases/download/nightly/conquerd-supernode-nightly-linux-x86_64.tar.gz.sha256");
    }

    #[test]
    fn tagged_release_uses_v_prefixed_tag_and_versioned_asset() {
        let defaults = Defaults {
            version: "1.2.3".into(),
            ..Defaults::default()
        };
        let artifact = resolve_release_artifact(&defaults, "linux-aarch64").unwrap();
        assert!(artifact.asset_url.contains("/releases/download/v1.2.3/"));
        assert_eq!(
            artifact.asset_name,
            "conquerd-supernode-1.2.3-linux-aarch64.tar.gz"
        );
    }

    #[test]
    fn verifies_sha256_sidecar_with_filename() {
        let bytes = b"hello";
        let sum = format!("{:x}", Sha256::digest(bytes));
        verify_sha256(bytes, &format!("{sum}  archive.tar.gz\n")).unwrap();
    }
}
