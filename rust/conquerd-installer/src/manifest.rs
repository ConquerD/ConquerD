use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Manifest stored at `<install_dir>/manifest.json` tracking installed files.
#[derive(Debug, Serialize, Deserialize)]
pub struct InstallManifest {
    pub version: String,
    pub files: HashMap<String, String>, // rel_path → sha256
}

/// Read an existing manifest from disk. Returns `None` if the file doesn't exist.
pub fn read_manifest(manifest_path: &Path) -> Result<Option<InstallManifest>> {
    if !manifest_path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let m: InstallManifest = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    Ok(Some(m))
}

/// Write a manifest after installation.
pub fn write_manifest(
    _install_dir: &Path,
    files: &HashMap<String, String>,
    manifest_path: &Path,
) -> Result<()> {
    let m = InstallManifest {
        version: env!("CARGO_PKG_VERSION").to_string(),
        files: files.clone(),
    };
    let json = serde_json::to_string_pretty(&m)?;
    fs::write(manifest_path, json)
        .with_context(|| format!("Failed to write manifest: {}", manifest_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("conquerd_manifest_test_{name}"))
    }

    #[test]
    fn read_manifest_returns_none_for_nonexistent_file() {
        let path = tmp_path("nonexistent.json");
        let _ = fs::remove_file(&path);
        let result = read_manifest(&path).expect("should succeed (not error)");
        assert!(result.is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let dir = tmp_path("roundtrip_dir");
        fs::create_dir_all(&dir).unwrap();
        let manifest_path = dir.join("manifest.json");
        let _ = fs::remove_file(&manifest_path);

        let mut files = HashMap::new();
        files.insert("bin/conquerd".to_string(), "abc123".to_string());
        files.insert("lib/audio.so".to_string(), "def456".to_string());

        write_manifest(&dir, &files, &manifest_path).expect("write should succeed");

        let loaded = read_manifest(&manifest_path)
            .expect("read should succeed")
            .expect("manifest should be Some");

        assert_eq!(
            loaded.files.get("bin/conquerd").map(String::as_str),
            Some("abc123")
        );
        assert_eq!(
            loaded.files.get("lib/audio.so").map(String::as_str),
            Some("def456")
        );
        assert!(
            !loaded.version.is_empty(),
            "version should be set from CARGO_PKG_VERSION"
        );

        let _ = fs::remove_file(&manifest_path);
    }

    #[test]
    fn read_manifest_rejects_malformed_json() {
        let path = tmp_path("bad_json.json");
        fs::write(&path, b"not json").unwrap();
        assert!(read_manifest(&path).is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn read_manifest_rejects_wrong_schema() {
        let path = tmp_path("wrong_schema.json");
        // JSON object but missing the required `version` and `files` fields.
        fs::write(&path, br#"{"unrelated":"data"}"#).unwrap();
        assert!(read_manifest(&path).is_err());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn install_manifest_serializes_to_valid_json() {
        let mut files = HashMap::new();
        files.insert("foo".to_string(), "bar".to_string());
        let m = InstallManifest {
            version: "1.0.0".to_string(),
            files,
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let reparsed: InstallManifest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(reparsed.version, "1.0.0");
        assert_eq!(reparsed.files.get("foo").map(String::as_str), Some("bar"));
    }
}
