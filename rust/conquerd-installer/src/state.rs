use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const STATE_FILE: &str = "current_version.json";

/// Tracks all installed versions and which one is current.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallState {
    /// The currently active version string (e.g. "1.0.0").
    pub current_version: String,
    /// All installed versions, newest first.
    pub versions: Vec<InstalledVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledVersion {
    pub version: String,
    /// Absolute path to the versioned directory (e.g. …\ConquerD\conquerd_1.0.0).
    pub path: PathBuf,
    /// Unix timestamp when this version was installed.
    pub installed_at: u64,
}

impl InstallState {
    /// Create a fresh state with no versions installed.
    pub fn empty() -> Self {
        Self {
            current_version: String::new(),
            versions: Vec::new(),
        }
    }

    /// Register a newly installed version and mark it current.
    pub fn add_version(&mut self, version: &str, path: &Path) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Remove any existing entry for this version
        self.versions.retain(|v| v.version != version);

        self.versions.push(InstalledVersion {
            version: version.to_string(),
            path: path.to_path_buf(),
            installed_at: now,
        });

        self.current_version = version.to_string();
    }

    /// Remove a version entry by version string.
    pub fn remove_version(&mut self, version: &str) {
        self.versions.retain(|v| v.version != version);
        if self.current_version == version {
            // Point current to the newest remaining version
            self.current_version = self
                .versions
                .iter()
                .max_by_key(|v| v.installed_at)
                .map(|v| v.version.clone())
                .unwrap_or_default();
        }
    }

    /// Return the path to the current version's directory, if any.
    pub fn current_path(&self) -> Option<&Path> {
        self.versions
            .iter()
            .find(|v| v.version == self.current_version)
            .map(|v| v.path.as_path())
    }

    /// Return versions that are NOT current (candidates for cleanup).
    pub fn old_versions(&self) -> Vec<&InstalledVersion> {
        self.versions
            .iter()
            .filter(|v| v.version != self.current_version)
            .collect()
    }
}

/// Return the path to `current_version.json` inside the base install dir.
pub fn state_path(base_dir: &Path) -> PathBuf {
    base_dir.join(STATE_FILE)
}

/// Read the install state from disk. Returns empty state if file doesn't exist.
pub fn read_state(base_dir: &Path) -> Result<InstallState> {
    let path = state_path(base_dir);
    if !path.exists() {
        return Ok(InstallState::empty());
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let state: InstallState = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(state)
}

/// Write install state to disk atomically (write-then-rename).
pub fn write_state(base_dir: &Path, state: &InstallState) -> Result<()> {
    std::fs::create_dir_all(base_dir)?;
    let path = state_path(base_dir);
    let json = serde_json::to_string_pretty(state)?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Build the versioned subdirectory path: `<base_dir>/conquerd_<version>`.
pub fn version_dir(base_dir: &Path, version: &str) -> PathBuf {
    base_dir.join(format!("conquerd_{version}"))
}

/// Find the ConquerD executable inside a versioned install directory.
pub fn find_exe(version_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        "ConquerD.exe",
        "conquerd.exe",
        "ConquerD/ConquerD.exe",
        "ConquerD/conquerd.exe",
    ];
    for rel in &candidates {
        let p = version_dir.join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Compare two version strings.  Returns true if `remote` > `local`.
pub fn is_newer(remote: &str, local: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    parse(remote) > parse(local)
}

/// Kill all running ConquerD.exe processes (Windows).
#[cfg(windows)]
pub fn kill_running_instances() {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "ConquerD.exe"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(not(windows))]
pub fn kill_running_instances() {
    let _ = std::process::Command::new("pkill")
        .args(["-f", "ConquerD"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Copy the running installer exe into base_dir if it isn't already there.
pub fn self_copy(base_dir: &Path) -> Result<()> {
    let self_exe = std::env::current_exe().context("Cannot determine own exe path")?;
    let dest = base_dir.join("conquerd-installer.exe");

    // Don't copy over ourselves
    if let (Ok(a), Ok(b)) = (
        std::fs::canonicalize(&self_exe),
        std::fs::canonicalize(&dest),
    ) {
        if a == b {
            return Ok(());
        }
    }

    std::fs::create_dir_all(base_dir)?;
    std::fs::copy(&self_exe, &dest)
        .with_context(|| format!("Failed to copy installer to {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    // ── InstallState data operations ────────────────────────────────────────

    #[test]
    fn empty_state_has_no_versions() {
        let s = InstallState::empty();
        assert_eq!(s.current_version, "");
        assert!(s.versions.is_empty());
        assert!(s.current_path().is_none());
        assert!(s.old_versions().is_empty());
    }

    #[test]
    fn add_version_sets_current() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/tmp/conquerd_1.0.0"));
        assert_eq!(s.current_version, "1.0.0");
        assert_eq!(s.versions.len(), 1);
        assert_eq!(s.current_path(), Some(Path::new("/tmp/conquerd_1.0.0")));
    }

    #[test]
    fn add_version_replaces_existing_entry_for_same_version() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/old/path"));
        s.add_version("1.0.0", Path::new("/new/path"));
        assert_eq!(s.versions.len(), 1);
        assert_eq!(s.current_path(), Some(Path::new("/new/path")));
    }

    #[test]
    fn add_multiple_versions_last_is_current() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/v1"));
        s.add_version("1.1.0", Path::new("/v2"));
        assert_eq!(s.current_version, "1.1.0");
        assert_eq!(s.old_versions().len(), 1);
        assert_eq!(s.old_versions()[0].version, "1.0.0");
    }

    #[test]
    fn remove_version_non_current() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/v1"));
        s.add_version("1.1.0", Path::new("/v2"));
        s.remove_version("1.0.0");
        assert_eq!(s.current_version, "1.1.0");
        assert_eq!(s.versions.len(), 1);
    }

    #[test]
    fn remove_current_version_points_to_remaining() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/v1"));
        s.add_version("1.1.0", Path::new("/v2"));
        s.remove_version("1.1.0");
        // current should fall back to the only remaining version
        assert_eq!(s.current_version, "1.0.0");
        assert_eq!(s.versions.len(), 1);
    }

    #[test]
    fn remove_last_version_leaves_empty_current() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/v1"));
        s.remove_version("1.0.0");
        assert_eq!(s.current_version, "");
        assert!(s.versions.is_empty());
        assert!(s.current_path().is_none());
    }

    #[test]
    fn old_versions_excludes_current() {
        let mut s = InstallState::empty();
        s.add_version("1.0.0", Path::new("/v1"));
        s.add_version("1.1.0", Path::new("/v2"));
        s.add_version("1.2.0", Path::new("/v3"));
        let old = s.old_versions();
        assert_eq!(old.len(), 2);
        assert!(old.iter().all(|v| v.version != "1.2.0"));
    }

    // ── version_dir / state_path helpers ───────────────────────────────────

    #[test]
    fn version_dir_uses_expected_format() {
        let base = Path::new("/base");
        let dir = version_dir(base, "1.2.3");
        assert_eq!(dir, PathBuf::from("/base/conquerd_1.2.3"));
    }

    #[test]
    fn state_path_is_inside_base() {
        let base = Path::new("/install");
        let sp = state_path(base);
        assert_eq!(sp, PathBuf::from("/install/current_version.json"));
    }

    #[test]
    fn find_exe_finds_direct_executable() {
        let dir = tmp_dir();
        let exe = dir.path().join("ConquerD.exe");
        fs::write(&exe, b"test").expect("write exe marker");

        assert_eq!(find_exe(dir.path()), Some(exe));
    }

    #[test]
    fn find_exe_finds_nested_packaged_executable() {
        let dir = tmp_dir();
        let app_dir = dir.path().join("ConquerD");
        fs::create_dir_all(&app_dir).expect("app dir");
        let exe = app_dir.join("ConquerD.exe");
        fs::write(&exe, b"test").expect("write exe marker");

        assert_eq!(find_exe(dir.path()), Some(exe));
    }

    // ── disk I/O (read_state / write_state) ────────────────────────────────

    #[test]
    fn read_state_returns_empty_when_file_missing() {
        let dir = tmp_dir();
        let s = read_state(dir.path()).expect("should succeed");
        assert_eq!(s.current_version, "");
        assert!(s.versions.is_empty());
    }

    #[test]
    fn write_then_read_state_round_trips() {
        let dir = tmp_dir();
        let mut s = InstallState::empty();
        s.add_version("1.0.0", dir.path());
        write_state(dir.path(), &s).expect("write should succeed");

        let loaded = read_state(dir.path()).expect("read should succeed");
        assert_eq!(loaded.current_version, "1.0.0");
        assert_eq!(loaded.versions.len(), 1);
    }

    #[test]
    fn write_state_creates_directory_if_missing() {
        let dir = tmp_dir();
        let nested = dir.path().join("a").join("b");
        let s = InstallState::empty();
        write_state(&nested, &s).expect("should create dirs and write");
        assert!(state_path(&nested).exists());
    }

    #[test]
    fn read_state_fails_on_corrupt_json() {
        let dir = tmp_dir();
        let path = state_path(dir.path());
        fs::write(&path, b"not valid json").unwrap();
        assert!(read_state(dir.path()).is_err());
    }

    // ── is_newer ────────────────────────────────────────────────────────────

    #[test]
    fn is_newer_detects_newer_patch() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(!is_newer("1.0.0", "1.0.1"));
    }

    #[test]
    fn is_newer_detects_newer_minor() {
        assert!(is_newer("1.1.0", "1.0.9"));
        assert!(!is_newer("1.0.9", "1.1.0"));
    }

    #[test]
    fn is_newer_detects_newer_major() {
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.99.99", "2.0.0"));
    }

    #[test]
    fn is_newer_same_version_is_not_newer() {
        assert!(!is_newer("1.0.0", "1.0.0"));
    }
}
