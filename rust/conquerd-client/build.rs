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

fn main() {
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
        let out_dir = std::path::Path::new(&manifest_path)
            .parent()
            .unwrap() // …/cxxqtbuild/
            .parent()
            .unwrap(); // …/out/
        println!("cargo:rustc-link-search=native={}", out_dir.display());
        println!("cargo:rustc-link-lib=static=cxx-qt-lib-cxxqt-generated");
        println!("cargo:rustc-link-lib=static:+whole-archive=cxx-qt-call-init-crate_cxx_qt_lib");
    }

    // Assemble the QML file list. WebEngine-dependent files are only included
    // when the `webengine` feature is active and Qt WebEngine is installed.
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
        QmlFile::from("qml/SettingsPage.qml"),
        QmlFile::from("qml/TitleBar.qml"),
        QmlFile::from("qml/CreateRoomDialog.qml"),
        QmlFile::from("qml/SettingsDialog.qml"),
        QmlFile::from("qml/OnboardingWizard.qml"),
        QmlFile::from("qml/SidebarItem.qml"),
        QmlFile::from("qml/ParticipantWidget.qml"),
        QmlFile::from("qml/StatsPanel.qml"),
        QmlFile::from("qml/VoiceRail.qml"),
        QmlFile::from("qml/Avatar.qml"),
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
}

#[cfg(all(feature = "qt-ui", target_os = "windows"))]
fn compile_app_icon_cpp() {
    use std::path::PathBuf;

    println!("cargo:rerun-if-changed=src/ui/app_icon.cpp");

    let qt_prefix = if let Ok(q) = std::env::var("QMAKE") {
        PathBuf::from(q)
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
            .expect("QMAKE path should have at least two components")
    } else if let Ok(prefix) = std::env::var("CMAKE_PREFIX_PATH") {
        PathBuf::from(prefix.split(';').next().unwrap_or(&prefix).trim())
    } else {
        eprintln!(
            "cargo:warning=app_icon.cpp: cannot find Qt prefix; set QMAKE or CMAKE_PREFIX_PATH"
        );
        return;
    };

    let include_root = qt_prefix.join("include");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());

    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("src/ui/app_icon.cpp")
        .include(&include_root)
        .include(include_root.join("QtCore"))
        .include(include_root.join("QtGui"))
        .flag("/EHsc")
        .flag("/Zc:__cplusplus")
        .flag("/permissive-")
        .compile("conquerd_app_icon");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=conquerd_app_icon");
}

#[cfg(all(feature = "qt-ui", feature = "webengine"))]
fn compile_scheme_cpp() {
    use std::path::PathBuf;
    use std::process::Command;

    // ── Locate Qt via the same env vars cxx-qt-build uses ────────────────
    let qt_prefix = if let Ok(q) = std::env::var("QMAKE") {
        // QMAKE=/path/to/qmake6.exe → /path/to/../..
        PathBuf::from(q)
            .parent() // bin/
            .and_then(|p| p.parent()) // Qt root
            .map(|p| p.to_path_buf())
            .expect("QMAKE path should have at least two components")
    } else if let Ok(prefix) = std::env::var("CMAKE_PREFIX_PATH") {
        PathBuf::from(prefix.split(';').next().unwrap_or(&prefix))
    } else {
        eprintln!(
            "cargo:warning=scheme.cpp: cannot find Qt prefix; set QMAKE or CMAKE_PREFIX_PATH"
        );
        return;
    };

    // ── Probe for Qt WebEngine headers ───────────────────────────────────────
    // Qt WebEngine is a separate component in the Qt installer.
    // If it is missing, emit a clear error rather than a cryptic C1083.
    let we_probe = qt_prefix
        .join("include")
        .join("QtWebEngineCore")
        .join("QWebEngineProfile.h");
    if !we_probe.exists() {
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
    let src_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("src/ui");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

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
    let include_root = qt_prefix.join("include");
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file(src_dir.join("scheme.cpp"))
        // The generated .moc file is #include-d by scheme.cpp (via `#include "scheme.moc"`)
        // so the OUT_DIR must be on the include path.
        .include(&out_dir)
        .include(&include_root)
        .include(include_root.join("QtCore"))
        .include(include_root.join("QtWebEngineCore"))
        .include(include_root.join("QtWebEngineQuick"));

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
