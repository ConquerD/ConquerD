/// Recursively collect regular files under a directory, skipping common
/// non-source dirs. Returns relative paths sorted for deterministic hashing.
fn collect_source_files(root: &str) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            if matches!(name, "target" | ".git" | "node_modules" | "dist" | ".cargo") {
                return;
            }
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, files);
                } else if path.is_file() {
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if matches!(ext, "rs" | "toml" | "lock") {
                            if let Ok(rel) =
                                path.strip_prefix(std::env::current_dir().unwrap_or_default())
                            {
                                files.push(rel.to_path_buf());
                            } else {
                                files.push(path);
                            }
                        }
                    } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if name == "Cargo.toml" || name == "Cargo.lock" || name == "build.rs" {
                            if let Ok(rel) =
                                path.strip_prefix(std::env::current_dir().unwrap_or_default())
                            {
                                files.push(rel.to_path_buf());
                            }
                        }
                    }
                }
            }
        }
    }
    walk(std::path::Path::new(root), &mut files);
    files.sort();
    files
}

/// Computes a short deterministic hash of the content of relevant source files.
fn compute_source_hash() -> String {
    use sha2::{Digest, Sha256};

    let mut all_files: Vec<_> = collect_source_files(".");
    all_files.sort();
    all_files.dedup();

    let mut hasher = Sha256::new();
    for path in all_files {
        if let Ok(bytes) = std::fs::read(&path) {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update([0u8]);
            hasher.update(&bytes);
            hasher.update([0u8]);
        }
    }

    // Self-referential binding (same rationale as in conquerd-client/build.rs).
    // The reported source hash depends on the actual content of the build script
    // that performed the hashing. Tampering with the hasher logic itself changes
    // the final hash in a detectable way for anyone who has the original sources.
    if let Ok(build_script_bytes) = std::fs::read("build.rs") {
        hasher.update(b"SELF:BUILD_SCRIPT_V1:");
        hasher.update(&build_script_bytes);
    }

    let digest = hasher.finalize();
    format!("{:x}", digest)[..12].to_string()
}

fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "ConquerD");
        res.set("FileDescription", "ConquerD Supernode (QUIC Relay + SFU)");
        res.set("LegalCopyright", "ConquerD Project");
        // Keep ProductVersion in sync with conquerd-client/Cargo.toml version.
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=Failed to set Windows resource: {e}");
        }
    }

    // Same build ID embedding as the client for P2P attestation of reproducible builds.
    // See client build.rs for the full explanation of the reproducible build intent.
    {
        let build_id = std::env::var("CONQUERD_BUILD_ID").unwrap_or_else(|_| {
            let tag = std::process::Command::new("git")
                .args(["describe", "--tags", "--exact-match", "HEAD"])
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            let sha = std::process::Command::new("git")
                .args(["rev-parse", "--short=12", "HEAD"])
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let base = if let Some(t) = tag {
                // Match the format used by GitHub release CI.
                format!("release-{}-{}", t.trim_start_matches('v'), sha)
            } else {
                sha
            };

            let is_dirty = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .ok()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);

            if is_dirty {
                format!("{}-dirty", base)
            } else {
                base
            }
        });
        println!("cargo:rustc-env=CONQUERD_BUILD_ID={}", build_id);
        println!(
            "cargo:rustc-env=CONQUERD_VERSION={}",
            env!("CARGO_PKG_VERSION")
        );

        if let Ok(proof) = std::env::var("CONQUERD_RELEASE_PROOF") {
            if !proof.trim().is_empty() {
                println!("cargo:rustc-env=CONQUERD_RELEASE_PROOF={}", proof.trim());
            }
        }

        let source_hash = compute_source_hash();
        println!("cargo:rustc-env=CONQUERD_SOURCE_HASH={}", source_hash);
    }
}
