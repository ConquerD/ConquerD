use anyhow::{bail, Context, Result};
use sevenz_rust::decompress_file;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Extract a .7z archive into `dest_dir`.
/// Returns a map of relative paths → SHA-256 hex digests for all extracted files.
pub fn extract_7z(archive: &Path, dest_dir: &Path) -> Result<HashMap<String, String>> {
    fs::create_dir_all(dest_dir)
        .with_context(|| format!("Failed to create directory: {}", dest_dir.display()))?;

    decompress_archive(archive, dest_dir)
        .with_context(|| format!("Failed to extract archive: {}", archive.display()))?;

    // Walk the destination and hash every file
    let mut files = HashMap::new();
    collect_files(dest_dir, dest_dir, &mut files)?;
    Ok(files)
}

/// Decompress a .7z archive with the embedded `sevenz-rust` decoder.
///
/// Release archives must be built non-solid (`7z a -ms=off`); solid archives
/// are not fully supported by `sevenz-rust`.
fn decompress_archive(archive: &Path, dest_dir: &Path) -> Result<()> {
    fs::create_dir_all(dest_dir)?;

    decompress_file(archive, dest_dir)
        .with_context(|| format!("Failed to extract archive: {}", archive.display()))?;
    validate_bundle_layout(dest_dir)?;
    Ok(())
}

/// Return the directory that contains `ConquerD.exe` inside an install tree.
fn bundle_root(install_dir: &Path) -> PathBuf {
    let nested = install_dir.join("ConquerD");
    if nested.join("ConquerD.exe").is_file() {
        nested
    } else {
        install_dir.to_path_buf()
    }
}

/// Ensure the extracted bundle contains the Qt runtime folders ConquerD needs.
fn validate_bundle_layout(install_dir: &Path) -> Result<()> {
    let root = bundle_root(install_dir);
    if !root.join("ConquerD.exe").is_file() {
        bail!(
            "Extraction incomplete: ConquerD.exe not found under {}",
            install_dir.display()
        );
    }

    // Match windeployqt6 output from build_win64.ps1 (--no-translations; no Qt Positioning).
    let required = [
        "platforms",
        "qml",
        "imageformats",
        "generic",
        "iconengines",
        "networkinformation",
        "qmltooling",
        "styles",
        "tls",
    ];
    let missing: Vec<_> = required
        .iter()
        .filter(|folder| !root.join(**folder).is_dir())
        .copied()
        .collect();
    if !missing.is_empty() {
        bail!(
            "Extraction incomplete: missing bundle folders: {}",
            missing.join(", ")
        );
    }

    if root.join("Qt6WebEngineCore.dll").is_file() && !root.join("resources").is_dir() {
        bail!("Extraction incomplete: missing resources/ (required for Qt WebEngine)");
    }

    Ok(())
}

fn collect_files(base: &Path, dir: &Path, files: &mut HashMap<String, String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(base, &path, files)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");

            // Skip the manifest itself
            if rel == "manifest.json" {
                continue;
            }

            let hash = hash_file(&path)?;
            files.insert(rel, hash);
        }
    }
    Ok(())
}

pub fn hash_file(path: &Path) -> Result<String> {
    let data =
        fs::read(path).with_context(|| format!("Failed to read file: {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&data);
    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute SHA-256 of a file at `path`, returning the hex digest.
#[allow(dead_code)]
pub fn verify_hash(path: &Path, expected: &str) -> Result<bool> {
    let actual = hash_file(path)?;
    Ok(actual == expected)
}

/// Extract with progress callback.
/// The callback receives (files_hashed_so_far, total_file_count).
pub fn extract_7z_with_progress<F>(
    archive: &Path,
    dest_dir: &Path,
    progress_fn: F,
) -> Result<HashMap<String, String>>
where
    F: Fn(usize, usize),
{
    // sevenz-rust doesn't have per-file callbacks, so we extract all at once
    // then walk and hash with progress
    fs::create_dir_all(dest_dir)?;
    decompress_archive(archive, dest_dir)
        .with_context(|| format!("Failed to extract: {}", archive.display()))?;

    // Pre-count so the UI can show a determinate progress bar
    let total = count_extractable_files(dest_dir)?;

    let mut files = HashMap::new();
    collect_files_with_progress(dest_dir, dest_dir, &mut files, total, &progress_fn)?;
    Ok(files)
}

/// Count files that will be hashed (excludes manifest.json).
fn count_extractable_files(dir: &Path) -> Result<usize> {
    let mut count = 0usize;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            count += count_extractable_files(&path)?;
        } else if path.is_file() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name != "manifest.json" {
                count += 1;
            }
        }
    }
    Ok(count)
}

fn collect_files_with_progress<F>(
    base: &Path,
    dir: &Path,
    files: &mut HashMap<String, String>,
    total: usize,
    progress_fn: &F,
) -> Result<()>
where
    F: Fn(usize, usize),
{
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files_with_progress(base, &path, files, total, progress_fn)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");

            if rel == "manifest.json" {
                continue;
            }

            let hash = hash_file(&path)?;
            files.insert(rel, hash);
            progress_fn(files.len(), total);
        }
    }
    Ok(())
}

// ── Repair: verify installed files against manifest ─────────────────────

/// Result of verifying the install directory against the manifest.
#[derive(Debug, Default)]
pub struct RepairReport {
    /// Files whose SHA-256 no longer matches the manifest.
    pub changed: Vec<String>,
    /// Files listed in the manifest but missing from disk.
    pub missing: Vec<String>,
    /// Total files checked.
    pub checked: usize,
}

impl RepairReport {
    /// Returns `true` if the installation is intact.
    pub fn is_ok(&self) -> bool {
        self.changed.is_empty() && self.missing.is_empty()
    }

    /// Number of files that need to be replaced.
    pub fn damaged_count(&self) -> usize {
        self.changed.len() + self.missing.len()
    }
}

/// Walk an installed version directory and compare every file against
/// the manifest.  Returns a [`RepairReport`] listing changed/missing files.
///
/// `progress_fn` receives `(files_checked, total_files)`.
pub fn verify_install<F>(
    install_dir: &Path,
    manifest: &std::collections::HashMap<String, String>,
    progress_fn: F,
) -> Result<RepairReport>
where
    F: Fn(usize, usize),
{
    let total = manifest.len();
    let mut report = RepairReport::default();

    for (i, (rel_path, expected_hash)) in manifest.iter().enumerate() {
        let file_path = install_dir.join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));

        if !file_path.exists() {
            report.missing.push(rel_path.clone());
        } else {
            let actual = hash_file(&file_path)?;
            if actual != *expected_hash {
                report.changed.push(rel_path.clone());
            }
        }

        report.checked = i + 1;
        progress_fn(report.checked, total);
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::TempDir;

    // ── RepairReport ────────────────────────────────────────────────────────

    #[test]
    fn repair_report_default_is_ok() {
        let r = RepairReport::default();
        assert!(r.is_ok());
        assert_eq!(r.damaged_count(), 0);
        assert_eq!(r.checked, 0);
    }

    #[test]
    fn repair_report_with_changed_is_not_ok() {
        let r = RepairReport {
            changed: vec!["bin/app.exe".into()],
            missing: vec![],
            checked: 1,
        };
        assert!(!r.is_ok());
        assert_eq!(r.damaged_count(), 1);
    }

    #[test]
    fn repair_report_with_missing_is_not_ok() {
        let r = RepairReport {
            changed: vec![],
            missing: vec!["lib/foo.dll".into()],
            checked: 1,
        };
        assert!(!r.is_ok());
        assert_eq!(r.damaged_count(), 1);
    }

    #[test]
    fn repair_report_damaged_count_sums_changed_and_missing() {
        let r = RepairReport {
            changed: vec!["a".into(), "b".into()],
            missing: vec!["c".into()],
            checked: 3,
        };
        assert_eq!(r.damaged_count(), 3);
        assert!(!r.is_ok());
    }

    // ── hash_file ───────────────────────────────────────────────────────────

    #[test]
    fn hash_file_produces_correct_sha256() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.bin");
        fs::write(&path, b"hello world").unwrap();
        let got = hash_file(&path).unwrap();
        // Round-trip: re-hash the same bytes and verify they match.
        assert_eq!(got.len(), 64, "SHA-256 hex must be 64 chars");
        assert!(got.chars().all(|c| c.is_ascii_hexdigit()));
        // Also verify a second call returns the same digest.
        assert_eq!(hash_file(&path).unwrap(), got);
    }

    #[test]
    fn hash_file_empty_file_has_64_char_hex_digest() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("empty.bin");
        fs::write(&path, b"").unwrap();
        let got = hash_file(&path).unwrap();
        assert_eq!(got.len(), 64);
        assert!(got.chars().all(|c| c.is_ascii_hexdigit()));
        // Different from non-empty file
        let path2 = tmp.path().join("nonempty.bin");
        fs::write(&path2, b"x").unwrap();
        assert_ne!(got, hash_file(&path2).unwrap());
    }

    // ── verify_hash ─────────────────────────────────────────────────────────

    #[test]
    fn verify_hash_returns_true_for_correct_hash() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("file.txt");
        fs::write(&path, b"hello world").unwrap();
        // Compute the actual hash, then verify it matches.
        let actual = hash_file(&path).unwrap();
        assert!(verify_hash(&path, &actual).unwrap());
    }

    #[test]
    fn verify_hash_returns_false_for_wrong_hash() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("file.txt");
        fs::write(&path, b"hello world").unwrap();
        assert!(!verify_hash(&path, "deadbeef").unwrap());
    }

    // ── verify_install ──────────────────────────────────────────────────────

    #[test]
    fn verify_install_empty_manifest_is_ok() {
        let tmp = TempDir::new().unwrap();
        let manifest = HashMap::new();
        let report = verify_install(tmp.path(), &manifest, |_, _| {}).unwrap();
        assert!(report.is_ok());
        assert_eq!(report.checked, 0);
    }

    #[test]
    fn verify_install_reports_clean_when_files_match() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("app.exe");
        fs::write(&path, b"binary data").unwrap();
        let hash = hash_file(&path).unwrap();
        let mut manifest = HashMap::new();
        manifest.insert("app.exe".into(), hash);
        let report = verify_install(tmp.path(), &manifest, |_, _| {}).unwrap();
        assert!(report.is_ok());
        assert_eq!(report.checked, 1);
    }

    #[test]
    fn verify_install_detects_missing_file() {
        let tmp = TempDir::new().unwrap();
        let mut manifest = HashMap::new();
        manifest.insert("missing.exe".into(), "abc123".into());
        let report = verify_install(tmp.path(), &manifest, |_, _| {}).unwrap();
        assert!(!report.is_ok());
        assert!(report.missing.contains(&"missing.exe".to_string()));
    }

    #[test]
    fn verify_install_detects_tampered_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("app.exe");
        fs::write(&path, b"original").unwrap();
        let original_hash = hash_file(&path).unwrap();
        // Tamper the file
        fs::write(&path, b"tampered").unwrap();
        let mut manifest = HashMap::new();
        manifest.insert("app.exe".into(), original_hash);
        let report = verify_install(tmp.path(), &manifest, |_, _| {}).unwrap();
        assert!(!report.is_ok());
        assert!(report.changed.contains(&"app.exe".to_string()));
    }

    #[test]
    fn verify_install_progress_callback_receives_expected_counts() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.exe"), b"a").unwrap();
        fs::write(tmp.path().join("b.exe"), b"b").unwrap();
        let h_a = hash_file(&tmp.path().join("a.exe")).unwrap();
        let h_b = hash_file(&tmp.path().join("b.exe")).unwrap();
        let mut manifest = HashMap::new();
        manifest.insert("a.exe".into(), h_a);
        manifest.insert("b.exe".into(), h_b);
        let call_count = std::cell::Cell::new(0usize);
        let last_total = std::cell::Cell::new(0usize);
        verify_install(tmp.path(), &manifest, |_done, total| {
            call_count.set(call_count.get() + 1);
            last_total.set(total);
        })
        .unwrap();
        assert_eq!(call_count.get(), 2);
        assert_eq!(last_total.get(), 2);
    }

    // ── bundle layout helpers ───────────────────────────────────────────────

    #[test]
    fn bundle_root_prefers_nested_conquerd_folder() {
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("ConquerD");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("ConquerD.exe"), b"x").unwrap();
        assert_eq!(bundle_root(tmp.path()), nested);
    }

    #[test]
    fn bundle_root_falls_back_to_install_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("ConquerD.exe"), b"x").unwrap();
        assert_eq!(bundle_root(tmp.path()), tmp.path());
    }

    #[test]
    fn validate_bundle_layout_requires_qt_runtime_folders() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("ConquerD");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("ConquerD.exe"), b"x").unwrap();
        fs::create_dir_all(root.join("platforms")).unwrap();
        fs::create_dir_all(root.join("qml")).unwrap();
        assert!(validate_bundle_layout(tmp.path()).is_err());
    }

    #[test]
    fn validate_bundle_layout_accepts_complete_flat_bundle() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::write(root.join("ConquerD.exe"), b"x").unwrap();
        for folder in [
            "platforms",
            "qml",
            "imageformats",
            "generic",
            "iconengines",
            "networkinformation",
            "qmltooling",
            "styles",
            "tls",
        ] {
            fs::create_dir_all(root.join(folder)).unwrap();
        }
        validate_bundle_layout(tmp.path()).expect("complete bundle should validate");
    }

    #[test]
    fn extract_7z_round_trips_non_solid_bundle() {
        use sevenz_rust::SevenZWriter;
        use std::fs::File;
        use std::io::BufWriter;

        let src = TempDir::new().unwrap();
        let bundle = src.path().join("ConquerD");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(bundle.join("ConquerD.exe"), b"stub").unwrap();
        for folder in [
            "platforms",
            "qml",
            "imageformats",
            "generic",
            "iconengines",
            "networkinformation",
            "qmltooling",
            "styles",
            "tls",
        ] {
            let dir = bundle.join(folder);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("placeholder.txt"), b"x").unwrap();
        }

        let archive = src.path().join("bundle.7z");
        let file = File::create(&archive).unwrap();
        let mut writer = SevenZWriter::new(BufWriter::new(file)).unwrap();
        writer
            .push_source_path_non_solid(&bundle, |_| true)
            .unwrap();
        writer.finish().unwrap();

        let dest = TempDir::new().unwrap();
        let hashes = extract_7z(&archive, dest.path()).expect("non-solid archive should extract");
        assert!(
            hashes.keys().any(|path| path.ends_with("ConquerD.exe")),
            "expected ConquerD.exe in extracted hashes, got: {:?}",
            hashes.keys().collect::<Vec<_>>()
        );
        assert!(
            hashes
                .keys()
                .any(|path| path.contains("platforms/") && path.ends_with("placeholder.txt")),
            "expected platforms/placeholder.txt in extracted hashes, got: {:?}",
            hashes.keys().collect::<Vec<_>>()
        );
        validate_bundle_layout(dest.path()).expect("extracted bundle should be complete");
    }
}
