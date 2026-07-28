// build.rs — conquerd-client build script.
//
// When the `qt-ui` feature is enabled, this script invokes the cxx-qt
// build system to compile the C++/Qt bridge and QML resources into the
// binary. Without the feature it is a no-op.
//
// Requirements for `qt-ui`:
//   - Qt6 installed and QTDIR / CMAKE_PREFIX_PATH pointing at it.
//   - cmake on PATH.
//   - `cxx-qt` and `cxx-qt-lib` in Cargo.toml [dependencies] (optional).

/// Recursively collect regular files under a directory, skipping common
/// non-source dirs (target, .git, etc.). Returns relative paths sorted for
/// deterministic hashing.
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
                    // Only include likely source / build input files to keep the
                    // hash focused and fast.
                    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                        if matches!(
                            ext,
                            "rs" | "toml"
                                | "lock"
                                | "qml"
                                | "svg"
                                | "vert"
                                | "frag"
                                | "js"
                                | "mjs"
                                | "html"
                                | "css"
                        ) {
                            if let Ok(rel) =
                                path.strip_prefix(std::env::current_dir().unwrap_or_default())
                            {
                                files.push(rel.to_path_buf());
                            } else {
                                files.push(path);
                            }
                        }
                    } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        // Include key no-ext files like Cargo.toml (already covered by ext), but be safe.
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
/// Used for build attestation so that the attested ID depends on actual file
/// contents, thwarting simple env-var spoofing of the git-based build_id.
fn compute_source_hash() -> String {
    use sha2::{Digest, Sha256};

    let roots = if std::path::Path::new("src").exists() {
        vec![".", "qml"] // . for Cargo.* + build.rs + src; qml for UI sources
    } else {
        vec!["."]
    };

    let mut all_files: Vec<_> = roots.into_iter().flat_map(collect_source_files).collect();

    all_files.sort();
    all_files.dedup();

    let mut hasher = Sha256::new();
    for path in all_files {
        if let Ok(bytes) = std::fs::read(&path) {
            hasher.update(path.to_string_lossy().as_bytes());
            hasher.update([0u8]); // separator
            hasher.update(&bytes);
            hasher.update([0u8]);
        }
    }

    // Self-referential binding:
    // We also hash the *current* content of build.rs (the file that contains this
    // very hashing logic).
    //
    // Why this helps against spoofing:
    // - If an attacker modifies sources + edits compute_source_hash / collect_source_files
    //   (e.g. to hardcode a fake "good" hash or skip modified files), the build.rs
    //   they actually executed will have different content.
    // - The final source_hash will therefore embed "I used this (possibly lying)
    //   version of the hasher logic".
    // - A honest verifier who has the exact tree the peer claims can run the
    //   *exact same build.rs* from that tree and will only get a matching hash
    //   if both the other files *and* the hasher logic were identical.
    //
    // This is the "self-referential" part: the hash of the sources includes the
    // description of *how* the hash was computed.
    if let Ok(build_script_bytes) = std::fs::read("build.rs") {
        hasher.update(b"SELF:BUILD_SCRIPT_V1:");
        hasher.update(&build_script_bytes);
    }

    let digest = hasher.finalize();
    // Use first 12 hex chars (48 bits) — short but sufficient for attestation comparison.
    format!("{digest:x}")[..12].to_string()
}

fn main() {
    // Declared unconditionally: the cfg is *set* only in the qt-ui path, but it
    // is *read* by src/video/sink.rs in every build, so a non-qt-ui build would
    // otherwise warn about an unexpected cfg name.
    println!("cargo:rustc-check-cfg=cfg(qt_multimedia)");

    // Windows PE version / signing metadata.
    // Keep ProductVersion in sync with conquerd-client/Cargo.toml version.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set("ProductName", "ConquerD");
        res.set(
            "FileDescription",
            "ConquerD — Privacy-First Peer Connectivity",
        );
        res.set("LegalCopyright", "ConquerD Project");
        res.set("ProductVersion", env!("CARGO_PKG_VERSION"));
        res.set("FileVersion", env!("CARGO_PKG_VERSION"));
        res.set_icon("../../assets/conquerd.ico");
        if let Err(e) = res.compile() {
            eprintln!("cargo:warning=Failed to set Windows resource: {e}");
        }
    }

    // Embed a build identifier for P2P build attestation / reproducible build verification.
    // The intent: if you and another peer built from the *exact same source commit*
    // (clean `git checkout <tag-or-sha> && cargo build`), your reported build_id should match.
    // Official CI releases can (and should) set CONQUERD_BUILD_ID to a clear value.
    {
        let build_id = std::env::var("CONQUERD_BUILD_ID").unwrap_or_else(|_| {
            // Prefer an exact tag if we're on one (common for release checkouts).
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
                // Match the format we set in GitHub release CI so that a clean
                // local build of the release tag reports the same build_id as
                // the official CI binary.
                format!("release-{}-{}", t.trim_start_matches('v'), sha)
            } else {
                sha
            };

            // Dirty tree? Mark it — this is not a clean reproducible build.
            let is_dirty = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .ok()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);

            if is_dirty {
                format!("{base}-dirty")
            } else {
                base
            }
        });
        println!("cargo:rustc-env=CONQUERD_BUILD_ID={build_id}");
        println!(
            "cargo:rustc-env=CONQUERD_VERSION={}",
            env!("CARGO_PKG_VERSION")
        );

        // Optional: a base64 signature from the release private key over the
        // build claim. Only present for official CI releases. Used to prove
        // the binary is not a local rebuild spoofing the build_id.
        if let Ok(proof) = std::env::var("CONQUERD_RELEASE_PROOF") {
            if !proof.trim().is_empty() {
                println!("cargo:rustc-env=CONQUERD_RELEASE_PROOF={}", proof.trim());
            }
        }

        // Compute a content hash of the actual source files.
        // This makes the attested value depend on the *contents* of the sources,
        // not just the git commit. Even if an attacker sets CONQUERD_BUILD_ID
        // via env var after modifying sources, the source_hash will differ.
        let source_hash = compute_source_hash();
        println!("cargo:rustc-env=CONQUERD_SOURCE_HASH={source_hash}");
    }

    #[cfg(feature = "qt-ui")]
    {
        build_qt_ui();
    }
}

#[cfg(feature = "qt-ui")]
fn build_qt_ui() {
    use cxx_qt_build::{CxxQtBuilder, QmlFile, QmlModule};

    // cxx-qt-lib provides C++ Qt type bindings (QString, QColor, etc.).
    // Its Rust types are not directly referenced in our bridge, so the rlib
    // is excluded from the compile graph and its build-script link-lib
    // directives don't propagate automatically.  We must emit them here so
    // the linker can find `cxx_qt_init_crate_cxx_qt_lib` and related symbols.
    //
    // DEP_CXX_QT_LIB_CXX_QT_MANIFEST_PATH is set by cxx-qt-lib's build
    // script (via cargo::metadata=CXX_QT_MANIFEST_PATH=...) because
    // cxx-qt-lib has `links = "cxx-qt-lib"` in its Cargo.toml.
    if let Ok(manifest_path) = std::env::var("DEP_CXX_QT_LIB_CXX_QT_MANIFEST_PATH") {
        let manifest_path = std::path::Path::new(&manifest_path);
        if let Some(out_dir) = manifest_path.parent().and_then(std::path::Path::parent) {
            println!("cargo:rustc-link-search=native={}", out_dir.display());
            println!("cargo:rustc-link-lib=static=cxx-qt-lib-cxxqt-generated");
            println!(
                "cargo:rustc-link-lib=static:+whole-archive=cxx-qt-call-init-crate_cxx_qt_lib"
            );
        } else {
            eprintln!(
                "cargo:warning=Unable to resolve cxx-qt-lib output directory from {}",
                manifest_path.display()
            );
        }
    }

    // Assemble the QML file list. WebEngine-dependent files are only included
    // when the `webengine` feature is active and Qt WebEngine is installed.
    // Probe the Qt installation for the Multimedia component. Uses the same
    // lib directory cxx-qt-build will link against, so the answer here matches
    // what the linker would actually find.
    fn qt_multimedia_available() -> bool {
        let Some(lib_dir) = qt_lib_dir() else {
            return false;
        };
        [
            "Qt6Multimedia.lib",
            "libQt6Multimedia.so",
            "libQt6Multimedia.dylib",
        ]
        .iter()
        .any(|name| lib_dir.join(name).exists())
            || lib_dir.join("QtMultimedia.framework").exists()
    }

    /// Resolve Qt's `lib` directory via qmake, mirroring how the Qt build
    /// scripts locate the installation.
    fn qt_lib_dir() -> Option<std::path::PathBuf> {
        let qmake = std::env::var("QMAKE").unwrap_or_else(|_| "qmake".to_string());
        let out = std::process::Command::new(qmake)
            .args(["-query", "QT_INSTALL_LIBS"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!path.is_empty()).then(|| std::path::PathBuf::from(path))
    }

    let qml_files: Vec<QmlFile> = vec![
        QmlFile::from("qml/Theme.qml").singleton(true),
        QmlFile::from("qml/MainWindow.qml"),
        QmlFile::from("qml/ChatPanel.qml"),
        QmlFile::from("qml/ChatRichMessageDelegate.qml"),
        QmlFile::from("qml/RichChatComposer.qml"),
        QmlFile::from("qml/CallPanel.qml"),
        QmlFile::from("qml/PassphraseDialog.qml"),
        QmlFile::from("qml/IncomingCallDialog.qml"),
        QmlFile::from("qml/JoinRoomDialog.qml"),
        QmlFile::from("qml/PeerList.qml"),
        QmlFile::from("qml/RoomPanel.qml"),
        QmlFile::from("qml/SessionBanner.qml"),
        QmlFile::from("qml/StyledButton.qml"),
        QmlFile::from("qml/StyledTextField.qml"),
        QmlFile::from("qml/EmptyState.qml"),
        QmlFile::from("qml/SettingsCard.qml"),
        QmlFile::from("qml/SettingsPage.qml"),
        QmlFile::from("qml/SettingsSectionHeader.qml"),
        QmlFile::from("qml/SettingsSidebar.qml"),
        QmlFile::from("qml/SettingSwitch.qml"),
        QmlFile::from("qml/TitleBar.qml"),
        QmlFile::from("qml/CreateRoomDialog.qml"),
        QmlFile::from("qml/ComponentGallery.qml"),
        QmlFile::from("qml/OnboardingWizard.qml"),
        QmlFile::from("qml/SidebarItem.qml"),
        QmlFile::from("qml/ParticipantWidget.qml"),
        QmlFile::from("qml/StatsPanel.qml"),
        QmlFile::from("qml/ConnectionStatsChip.qml"),
        QmlFile::from("qml/VoiceRail.qml"),
        QmlFile::from("qml/Avatar.qml"),
        QmlFile::from("qml/PeerVolumePopup.qml"),
    ];
    #[cfg(feature = "webengine")]
    let qml_files: Vec<QmlFile> = {
        let mut v = qml_files;
        v.extend([
            QmlFile::from("qml/ConquerdWebView.qml"),
            QmlFile::from("qml/FilePreviewPanel.qml"),
            QmlFile::from("qml/BrowserPanel.qml"),
        ]);
        v
    };

    let has_multimedia = qt_multimedia_available();

    // VideoTile.qml has `import QtMultimedia`, so it is only compiled into the
    // qrc when that module exists — otherwise qmlcachegen fails the entire
    // build over an unresolvable import, turning an optional feature into a
    // hard build break.
    let qml_files: Vec<QmlFile> = {
        let mut v = qml_files;
        if has_multimedia {
            v.push(QmlFile::from("qml/VideoTile.qml"));
            // VideoRegion instantiates VideoTile, so it is equally
            // multimedia-dependent.
            v.push(QmlFile::from("qml/VideoRegion.qml"));
            v.push(QmlFile::from("qml/VideoPopoutWindow.qml"));
        }
        v
    };

    let builder = CxxQtBuilder::new_qml_module(
        QmlModule::new("ConquerD.Client")
            .version(1, 0)
            .qml_files(qml_files),
    )
    .qrc("icons.qrc")
    .qrc("assets.qrc")
    .qt_module("Qml")
    .qt_module("Quick")
    .qt_module("QuickControls2")
    .qt_module("Svg");

    // Multimedia supplies QVideoSink / QVideoFrame and the QtMultimedia QML
    // module behind `VideoOutput`. Only the base module is linked: the video
    // path pushes pre-decoded frames into a sink, so QMediaPlayer / QCamera and
    // the FFmpeg media backend plugin are never exercised. MultimediaQuick is
    // deliberately absent — it ships no public headers and its QML plugin is
    // resolved at runtime.
    //
    // Linked **only when present**. Qt Multimedia is a separate component of
    // the Qt installation, and unconditionally requesting it makes the whole
    // application fail to link with `LNK1181: cannot open Qt6Multimedia.lib` on
    // any machine that has not added it — turning an optional feature into a
    // hard build break. Instead the video surface degrades: `cfg(qt_multimedia)`
    // gates the Rust side and `Theme.hasMultimedia` gates the QML side.
    // Re-run detection when the Qt lib directory changes, so installing the
    // module later actually takes effect. Without this the probe result is
    // cached in the build script's output and a fresh Qt Multimedia install is
    // silently ignored until some unrelated edit forces build.rs to re-run.
    if let Some(lib_dir) = qt_lib_dir() {
        println!("cargo:rerun-if-changed={}", lib_dir.display());
    }
    let builder = if has_multimedia {
        println!("cargo:rustc-cfg=qt_multimedia");
        builder.qt_module("Multimedia")
    } else {
        println!(
            "cargo:warning=Qt Multimedia not found — video rendering is disabled. \
             Add it with: python -m aqt install-qt windows desktop 6.8.3 win64_msvc2022_64 \
             -m qtmultimedia -O C:\\Qt   (or run scripts/install_qt_windows.ps1)"
        );
        builder
    };
    #[cfg(feature = "webengine")]
    let builder = builder
        .qt_module("WebEngineCore")
        .qt_module("WebEngineQuick")
        .qt_module("WebChannel");

    builder
        .files([
            "src/ui/bridge.rs",
            "src/ui/peer_list_model.rs",
            "src/ui/chat_model.rs",
            "src/ui/call_model.rs",
            "src/ui/room_model.rs",
            "src/ui/settings_model.rs",
            "src/ui/file_transfer_model.rs",
        ])
        .build();

    // Compile the scheme handler C++ shim when WebEngine is enabled.
    // This is a plain Qt class (not a cxx-qt bridge), so it is compiled
    // separately via the `cc` crate with Qt include paths inferred from
    // qmake / CMAKE_PREFIX_PATH.
    #[cfg(feature = "webengine")]
    compile_scheme_cpp();

    // Set the application icon on Windows so the taskbar, alt-tab switcher,
    // and title bar show the ConquerD logo instead of the generic Qt icon.
    #[cfg(target_os = "windows")]
    compile_app_icon_cpp();

    #[cfg(target_os = "windows")]
    compile_window_chrome_cpp();

    compile_qml_startup_cpp();

    if has_multimedia {
        compile_video_sink_cpp();
    }
}

#[cfg(feature = "qt-ui")]
fn resolve_qt_prefix() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(q) = std::env::var("QMAKE") {
        let qmake = PathBuf::from(q);
        if let Some(prefix) = qmake.parent().and_then(|p| p.parent()) {
            return Some(prefix.to_path_buf());
        }
    }
    if let Ok(qt_dir) = std::env::var("QT_DIR") {
        let path = PathBuf::from(qt_dir);
        if path.is_dir() {
            return Some(path);
        }
    }
    if let Ok(prefix) = std::env::var("CMAKE_PREFIX_PATH") {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let first = prefix.split(sep).next().unwrap_or(prefix.as_str()).trim();
        if !first.is_empty() {
            return Some(PathBuf::from(first));
        }
    }
    None
}

#[cfg(feature = "qt-ui")]
fn qt_install_headers(qt_prefix: &std::path::Path) -> std::path::PathBuf {
    use std::process::Command;

    let qmake_name = if cfg!(windows) { "qmake6.exe" } else { "qmake" };
    let qmake = qt_prefix.join("bin").join(qmake_name);
    if qmake.exists() {
        if let Ok(out) = Command::new(&qmake)
            .arg("-query")
            .arg("QT_INSTALL_HEADERS")
            .output()
        {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path.is_empty() {
                    return std::path::PathBuf::from(path);
                }
            }
        }
    }
    qt_prefix.join("include")
}

#[cfg(feature = "qt-ui")]
fn configure_qt_cpp_build(build: &mut cc::Build, qt_prefix: &std::path::Path, modules: &[&str]) {
    let headers = qt_install_headers(qt_prefix);
    if headers.is_dir() {
        build.include(&headers);
    }
    for module in modules {
        let sub = headers.join(module);
        if sub.is_dir() {
            build.include(sub);
        }
        #[cfg(target_os = "macos")]
        {
            // Short includes like <QGuiApplication> live in the framework Headers dir.
            let fw_headers = qt_prefix
                .join("lib")
                .join(format!("{module}.framework"))
                .join("Headers");
            if fw_headers.is_dir() {
                build.include(fw_headers);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Framework-style includes like <QtGui/qtguiglobal.h> resolve via -F, not -I.
        // See https://forum.qt.io/topic/141436
        let fw_lib = qt_prefix.join("lib");
        if fw_lib.is_dir() {
            build.flag(format!("-F{}", fw_lib.display()));
        }
    }
}

#[cfg(all(feature = "qt-ui", feature = "webengine"))]
fn qt_header_exists(qt_prefix: &std::path::Path, module: &str, header: &str) -> bool {
    let headers = qt_install_headers(qt_prefix);
    if headers.join(module).join(header).exists() {
        return true;
    }
    #[cfg(target_os = "macos")]
    {
        let fw = qt_prefix
            .join("lib")
            .join(format!("{module}.framework"))
            .join("Headers")
            .join(header);
        if fw.exists() {
            return true;
        }
    }
    false
}

#[cfg(all(feature = "qt-ui", target_os = "windows"))]
fn compile_app_icon_cpp() {
    use std::path::PathBuf;

    println!("cargo:rerun-if-changed=src/ui/app_icon.cpp");

    let Some(qt_prefix) = resolve_qt_prefix() else {
        eprintln!(
            "cargo:warning=app_icon.cpp: cannot find Qt prefix; set QMAKE, QT_DIR, or CMAKE_PREFIX_PATH"
        );
        return;
    };

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        eprintln!("cargo:warning=app_icon.cpp: OUT_DIR is not set; skipping icon shim");
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("src/ui/app_icon.cpp")
        .flag("/EHsc")
        .flag("/Zc:__cplusplus")
        .flag("/permissive-");
    configure_qt_cpp_build(&mut build, &qt_prefix, &["QtCore", "QtGui"]);
    build.compile("conquerd_app_icon");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=conquerd_app_icon");
}

#[cfg(all(feature = "qt-ui", target_os = "windows"))]
fn compile_window_chrome_cpp() {
    use std::path::PathBuf;

    println!("cargo:rerun-if-changed=src/ui/window_chrome.cpp");

    let Some(qt_prefix) = resolve_qt_prefix() else {
        eprintln!(
            "cargo:warning=window_chrome.cpp: cannot find Qt prefix; set QMAKE, QT_DIR, or CMAKE_PREFIX_PATH"
        );
        return;
    };

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        eprintln!("cargo:warning=window_chrome.cpp: OUT_DIR is not set; skipping chrome shim");
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("src/ui/window_chrome.cpp")
        .flag("/EHsc")
        .flag("/Zc:__cplusplus")
        .flag("/permissive-");
    configure_qt_cpp_build(&mut build, &qt_prefix, &["QtCore", "QtGui"]);
    // dwmapi is pulled via #pragma comment in the cpp; also link explicitly.
    println!("cargo:rustc-link-lib=dwmapi");
    build.compile("conquerd_window_chrome");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=conquerd_window_chrome");
}

/// Compile the QVideoSink registry shim.
///
/// Only built when Qt Multimedia is present; without it there is no
/// `QVideoSink` to compile against, and the app links fine without video.
#[cfg(feature = "qt-ui")]
fn compile_video_sink_cpp() {
    use std::path::PathBuf;

    println!("cargo:rerun-if-changed=src/ui/video_sink_bridge.cpp");
    println!("cargo:rerun-if-changed=src/ui/video_sink_bridge.h");

    let Some(qt_prefix) = resolve_qt_prefix() else {
        eprintln!("cargo:warning=video_sink_bridge: cannot find Qt prefix; skipping");
        return;
    };
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        eprintln!("cargo:warning=video_sink_bridge: OUT_DIR is not set; skipping");
        return;
    };
    let out_dir = PathBuf::from(out_dir);

    // The registry exposes Q_INVOKABLE methods to QML, which requires moc —
    // `cc` does not run it, so generate the meta-object source explicitly and
    // compile it alongside.
    let moc = qt_prefix
        .join("bin")
        .join(if cfg!(windows) { "moc.exe" } else { "moc" });
    let moc_out = out_dir.join("moc_video_sink_bridge.cpp");
    let status = std::process::Command::new(&moc)
        .arg("src/ui/video_sink_bridge.h")
        .arg("-o")
        .arg(&moc_out)
        .status();
    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!(
                "cargo:warning=video_sink_bridge: moc failed at {}; skipping video shim",
                moc.display()
            );
            return;
        }
    }

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("src/ui/video_sink_bridge.cpp")
        .file(&moc_out)
        .include("src/ui");
    if cfg!(target_env = "msvc") {
        build
            .flag("/EHsc")
            .flag("/Zc:__cplusplus")
            .flag("/permissive-");
    }
    configure_qt_cpp_build(
        &mut build,
        &qt_prefix,
        &["QtCore", "QtGui", "QtQml", "QtMultimedia"],
    );
    build.compile("conquerd_video_sink");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=conquerd_video_sink");
}

#[cfg(feature = "qt-ui")]
fn compile_qml_startup_cpp() {
    use std::path::PathBuf;

    println!("cargo:rerun-if-changed=src/ui/qml_startup.cpp");

    let Some(qt_prefix) = resolve_qt_prefix() else {
        eprintln!(
            "cargo:warning=qml_startup.cpp: cannot find Qt prefix; set QMAKE, QT_DIR, or CMAKE_PREFIX_PATH"
        );
        return;
    };

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        eprintln!("cargo:warning=qml_startup.cpp: OUT_DIR is not set; skipping startup shim");
        return;
    };
    let out_dir = PathBuf::from(out_dir);
    let mut build = cc::Build::new();
    build.cpp(true).std("c++17").file("src/ui/qml_startup.cpp");
    configure_qt_cpp_build(
        &mut build,
        &qt_prefix,
        &["QtCore", "QtGui", "QtQml", "QtQuick"],
    );

    #[cfg(windows)]
    build
        .flag("/EHsc")
        .flag("/Zc:__cplusplus")
        .flag("/permissive-");
    #[cfg(not(windows))]
    build.flag("-fPIC");

    build.compile("conquerd_qml_startup");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=conquerd_qml_startup");
}

#[cfg(all(feature = "qt-ui", feature = "webengine"))]
fn compile_scheme_cpp() {
    use std::path::PathBuf;
    use std::process::Command;

    let Some(qt_prefix) = resolve_qt_prefix() else {
        eprintln!(
            "cargo:warning=scheme.cpp: cannot find Qt prefix; set QMAKE, QT_DIR, or CMAKE_PREFIX_PATH"
        );
        return;
    };

    // ── Probe for Qt WebEngine headers ───────────────────────────────────────
    // Qt WebEngine is a separate component in the Qt installer.
    // If it is missing, emit a clear error rather than a cryptic C1083.
    if !qt_header_exists(&qt_prefix, "QtWebEngineCore", "QWebEngineProfile.h") {
        panic!(
            "\n\n\
             ╔══════════════════════════════════════════════════════════════╗\n\
             ║  Qt WebEngine is NOT installed.                             ║\n\
             ║                                                             ║\n\
             ║  The `webengine` feature requires the Qt WebEngine module.  ║\n\
             ║  Install it via the Qt Maintenance Tool:                    ║\n\
             ║    Qt 6.x  →  Additional Libraries  →  Qt WebEngine        ║\n\
             ║                                                             ║\n\
             ║  Or rebuild without the portal:                             ║\n\
             ║    cargo build --features qt-ui                             ║\n\
             ╚══════════════════════════════════════════════════════════════╝"
        );
    }

    // Run moc on scheme.cpp to generate the MOC output (needed for Q_OBJECT).
    let moc = qt_prefix
        .join("bin")
        .join(if cfg!(windows) { "moc.exe" } else { "moc" });
    let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        eprintln!("cargo:warning=scheme.cpp: CARGO_MANIFEST_DIR is not set; skipping scheme shim");
        return;
    };
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        eprintln!("cargo:warning=scheme.cpp: OUT_DIR is not set; skipping scheme shim");
        return;
    };
    let src_dir = PathBuf::from(manifest_dir).join("src/ui");
    let out_dir = PathBuf::from(out_dir);

    // Tell cargo to rerun when the source changes.
    println!("cargo:rerun-if-changed=src/ui/scheme.cpp");

    // Generate moc output: moc scheme.cpp -o <out>/scheme.moc
    let moc_out = out_dir.join("scheme.moc");
    if moc.exists() {
        let status = Command::new(&moc)
            .arg(src_dir.join("scheme.cpp"))
            .arg("-o")
            .arg(&moc_out)
            .status();
        if !status.map(|s| s.success()).unwrap_or(false) {
            eprintln!("cargo:warning=moc failed for scheme.cpp — handler may not link");
        }
    } else {
        eprintln!(
            "cargo:warning=moc not found at {}; skipping moc for scheme.cpp",
            moc.display()
        );
    }

    // Compile scheme.cpp.
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file(src_dir.join("scheme.cpp"))
        // The generated .moc file is #include-d by scheme.cpp (via `#include "scheme.moc"`)
        // so the OUT_DIR must be on the include path.
        .include(&out_dir);
    configure_qt_cpp_build(
        &mut build,
        &qt_prefix,
        &["QtCore", "QtWebEngineCore", "QtWebEngineQuick"],
    );

    #[cfg(windows)]
    build
        .flag("/EHsc")
        .flag("/Zc:__cplusplus")
        .flag("/permissive-");
    #[cfg(not(windows))]
    build.flag("-fPIC");

    build.compile("conquerd_scheme");

    // Belt-and-suspenders: cc::Build::compile() should print these, but on
    // some Cargo/build-script cache paths the link directives are not
    // re-emitted when only feature flags change.  Re-emit explicitly so the
    // bin always picks up the static library and finds
    // conquerd_register_scheme / conquerd_install_scheme_handler.
    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=conquerd_scheme");
}
