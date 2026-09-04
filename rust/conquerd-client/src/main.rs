//! conquerd-client — native Rust desktop client entry point.
//!
//! When built with `--features qt-ui`, starts a Qt/QML window and runs the
//! tokio runtime on a background thread (AppBridge handles startup).
//!
//! Without `qt-/* ui */`, runs headlessly — useful for integration testing.
//!
//! By default the Windows console window is suppressed (`windows_subsystem = "windows"`).
//! Build with `--features console` to keep the console attached for debugging.

// Suppress the Windows console window unless the `console` feature is enabled.
#![cfg_attr(all(windows, not(feature = "console")), windows_subsystem = "windows")]
// Shared by both entry points.
use conquerd_client::{logging, platform};
use tracing::{error, info};

#[cfg(feature = "qt-ui")]
use conquerd_client::{ui, video};

// Headless-only. Under `qt-ui` all of this is driven by AppBridge instead, so
// gate it to keep the Qt build free of dead code.
#[cfg(not(feature = "qt-ui"))]
use std::collections::{HashMap, HashSet};
#[cfg(not(feature = "qt-ui"))]
use std::sync::Arc;

#[cfg(not(feature = "qt-ui"))]
use conquerd_client::{
    call_controller::{self, CallController},
    chat_store::{self, ChatStore},
    connection_manager::{ConnectionCommand, ConnectionEvent, ConnectionManager},
    crypto, error, github_updater,
    identity::{self, Identity},
    ollama_module,
    peer_store::PeerStore,
    protocol,
    room_store::RoomStore,
    sfu_client::SfuClient,
};
#[cfg(not(feature = "qt-ui"))]
use parking_lot::RwLock;
#[cfg(not(feature = "qt-ui"))]
use tokio::sync::mpsc;
#[cfg(not(feature = "qt-ui"))]
use tracing::warn;

/// Where a headless Ollama auto-reply should be posted.
#[derive(Debug, Clone)]
#[cfg(not(feature = "qt-ui"))]
enum HeadlessAutoTarget {
    Direct {
        peer_id: String,
    },
    Room {
        supernode_id: String,
        room_id: String,
    },
}

/// On HiDPI Windows displays (e.g. 4K at 150%) Qt honours the OS DPI scale
/// factor, which makes Material's touch-sized controls (48 dp) appear very
/// large on desktop.  Setting `QT_SCALE_FACTOR=0.75` before `QGuiApplication`
/// is constructed gives `0.75 × 1.5 = 1.125` effective scale — compact and
/// sharp on 4K, no change on non-HiDPI monitors (96 DPI / 100%).
///
/// Only applied when the caller has not already set `QT_SCALE_FACTOR`.
/// Override with e.g. `set QT_SCALE_FACTOR=1.0` to disable.
#[cfg(all(feature = "qt-ui", target_os = "windows"))]
fn maybe_apply_hidpi_scale() {
    if std::env::var("QT_SCALE_FACTOR").is_ok() {
        return;
    }
    // Query screen DPI before any Qt objects exist.
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSX};
    let dpi = unsafe {
        let hdc = GetDC(HWND::default());
        let d = GetDeviceCaps(hdc, LOGPIXELSX);
        ReleaseDC(HWND::default(), hdc);
        d
    };
    if dpi > 96 {
        // QT_SCALE_FACTOR is read once during QGuiApplication construction.
        std::env::set_var("QT_SCALE_FACTOR", "0.75");
    }
}

#[cfg(all(feature = "qt-ui", not(target_os = "windows")))]
fn maybe_apply_hidpi_scale() {}

#[cfg(feature = "qt-ui")]
fn run_qt_ui() {
    use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

    extern "C" {
        fn conquerd_install_qt_message_handler();
        fn conquerd_qml_post_load_check(engine: *mut std::ffi::c_void);
    }

    // On HiDPI displays set QT_SCALE_FACTOR before Qt is initialised so
    // Material controls render at desktop-compact sizes.
    maybe_apply_hidpi_scale();

    // Mirror Qt/QML warnings and errors to stderr (visible with the `console` feature).
    unsafe {
        conquerd_install_qt_message_handler();
    }

    // Windows taskbar / alt-tab icon — set via C++ shim so that
    // QGuiApplication::setWindowIcon() is called before exec().
    #[cfg(target_os = "windows")]
    extern "C" {
        fn conquerd_set_app_icon();
    }

    // Single-instance guard.  If a `conquerd://` URL was passed on argv and
    // another ConquerD is already running (e.g. Chromium inside our embedded
    // BrowserPanel handed a URL to the OS via its external-protocol
    // fallback), exit silently before any window is created.  The running
    // instance keeps everything in-process.
    if platform::should_exit_as_duplicate_instance() {
        std::process::exit(0);
    }

    // Register the conquerd:// URL scheme BEFORE QGuiApplication is
    // created — this is a QtWebEngine requirement. No-op without webengine.
    // Portal pages use web.host.app.v1 over the native QUIC session (no
    // browser WebTransport / Chromium QUIC flags required).
    #[cfg(feature = "webengine")]
    unsafe {
        ui::scheme::conquerd_register_scheme();
    }

    let mut app = QGuiApplication::new();

    // Set the application icon now that QGuiApplication exists.
    #[cfg(target_os = "windows")]
    unsafe {
        conquerd_set_app_icon();
    }

    // Install the conquerd:// scheme handler on the default WebEngine
    // profile AFTER QGuiApplication exists. No-op without webengine.
    #[cfg(feature = "webengine")]
    unsafe {
        ui::scheme::conquerd_install_scheme_handler();
    }

    // Must precede `engine.load()`: QML resolves `import ConquerD.Native` at
    // parse time, so registering afterwards would leave VideoTile unable to
    // find the registry.
    video::sink::register_singleton();

    let mut engine = QQmlApplicationEngine::new();
    if let Some(mut engine) = engine.as_mut() {
        let engine_ptr = unsafe {
            std::ptr::from_mut(std::pin::Pin::get_unchecked_mut(engine.as_mut()))
                as *mut std::ffi::c_void
        };
        engine.load(&QUrl::from(
            "qrc:/qt/qml/ConquerD/Client/qml/MainWindow.qml",
        ));
        unsafe {
            // On Windows this also installs the snap-friendly frame filter
            // (see window_chrome.cpp via qml_startup.cpp).
            conquerd_qml_post_load_check(engine_ptr);
        }
    } else {
        error!("QQmlApplicationEngine::new() returned null — UI cannot start");
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
}

fn main() {
    // Logging — seeded from the persisted `debug_logging` setting, runtime
    // reloadable via the Settings toggle; an explicit RUST_LOG always wins.
    logging::init(logging::load_debug_logging_setting());

    info!("conquerd-client {} starting", env!("CARGO_PKG_VERSION"));

    // Qt UI path: AppBridge handles identity unlock and tokio startup.
    // After exec() returns (last window closed), terminate the process.
    // Background tokio tasks and the PTT polling thread do not automatically
    // stop when their JoinHandles are dropped (detached), so we use
    // process::exit to guarantee a clean termination.
    #[cfg(feature = "qt-ui")]
    {
        run_qt_ui();
        std::process::exit(0);
    }

    // Headless path: block main thread on the tokio runtime.
    #[cfg(not(feature = "qt-ui"))]
    {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("Failed to start tokio runtime: {e}");
                std::process::exit(1);
            }
        };
        rt.block_on(headless_main());
    }
}

#[cfg(not(feature = "qt-ui"))]
async fn headless_main() {
    // Resolve key directory (can be overridden via CONQUERD_KEY_DIR)
    let key_dir = std::env::var("CONQUERD_KEY_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Identity::default_key_dir());

    // Ollama-only smoke (no identity / supernode). Prefer:
    //   scripts\test_ollama_auto_reply.ps1 -Profile .clientA
    // Env: CONQUERD_OLLAMA_ONLY=1 CONQUERD_SIMULATE_INBOUND_CHAT="…"
    if std::env::var("CONQUERD_OLLAMA_ONLY").ok().as_deref() == Some("1") {
        ollama_only_auto_reply_test().await;
        return;
    }

    // ------------------------------------------------------------------
    // Identity unlock
    // ------------------------------------------------------------------
    // In headless mode, read passphrase from CONQUERD_PASSPHRASE env var.
    // When the Qt UI is wired in, this will be replaced by an unlock dialog.
    let identity = match unlock_identity(&key_dir) {
        Ok(id) => Arc::new(id),
        Err(e) => {
            error!("Failed to unlock identity: {}", e);
            eprintln!("\nPress Enter to exit...");
            let _ = std::io::stdin().read_line(&mut String::new());
            std::process::exit(1);
        }
    };

    info!(
        "Identity unlocked: {} ({})",
        identity.public_id(),
        identity.peer_id()
    );

    // ------------------------------------------------------------------
    // Peer store
    // ------------------------------------------------------------------
    let peer_store = match PeerStore::open(&identity, None) {
        Ok(s) => Arc::new(RwLock::new(s)),
        Err(e) => {
            error!("Failed to open peer store: {}", e);
            std::process::exit(1);
        }
    };
    info!("Peer store loaded: {} peers", peer_store.read().len());
    for sn in peer_store.read().supernodes() {
        info!(
            "  trusted supernode: {}…",
            &sn.identity_pub[..12.min(sn.identity_pub.len())]
        );
    }

    // ------------------------------------------------------------------
    // Chat store
    // ------------------------------------------------------------------
    let chat_store = match ChatStore::open(&identity, None) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            error!("Failed to open chat store: {}", e);
            std::process::exit(1);
        }
    };
    info!("Chat store opened");

    // ------------------------------------------------------------------
    // Room store (client-owned definitions — rematerialized on connect)
    // ------------------------------------------------------------------
    let room_store = match RoomStore::open(&identity, None) {
        Ok(s) => Arc::new(RwLock::new(s)),
        Err(e) => {
            error!("Failed to open room store: {}", e);
            std::process::exit(1);
        }
    };
    {
        let rs = room_store.read();
        info!(
            "Room store loaded: {} saved room definition(s)",
            rs.list().len()
        );
        for e in rs.list() {
            info!(
                "  room '{}' ({}) on {}… type={} hidden={}",
                e.room_name,
                e.room_id,
                &e.supernode_id[..12.min(e.supernode_id.len())],
                e.room_type,
                rs.is_hidden_from_sidebar(&e.supernode_id, &e.room_id)
            );
        }
    }

    // ------------------------------------------------------------------
    // Connection manager
    // ------------------------------------------------------------------
    let (cmd_tx, event_rx, cm_fut) =
        ConnectionManager::split(Arc::clone(&identity), Arc::clone(&peer_store));
    tokio::spawn(cm_fut);

    // Scripted/CI hook (mirrors CONQUERD_PASSPHRASE): accept an invite URL on
    // startup so a headless client can join a supernode without a UI.
    if let Ok(invite_url) = std::env::var("CONQUERD_ACCEPT_INVITE") {
        if !invite_url.is_empty() {
            info!("Accepting invite from CONQUERD_ACCEPT_INVITE");
            let _ = cmd_tx.try_send(ConnectionCommand::AcceptInvite { invite_url });
        }
    }

    // ------------------------------------------------------------------
    // GitHub updater
    // ------------------------------------------------------------------
    let installer_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("conquerd-installer.exe")));
    let (updater_cmd_tx, _updater_event_rx, updater_fut) = github_updater::Updater::split(
        env!("CARGO_PKG_VERSION"),
        github_updater::DEFAULT_REPO,
        installer_path,
    );
    tokio::spawn(updater_fut);

    // ------------------------------------------------------------------
    // Ollama AI module (settings-driven; same flags as the Qt UI)
    // ------------------------------------------------------------------
    let ollama_settings = ollama_module::read_assistant_settings();
    info!(
        "Ollama settings: enabled={} model={} auto_direct={} auto_room={}",
        ollama_settings.enabled,
        ollama_settings.model,
        ollama_settings.auto_respond_direct,
        ollama_settings.auto_respond_room
    );
    let (ollama_cmd_tx, ollama_event_rx, ollama_fut) =
        ollama_module::OllamaModule::split(ollama_settings.to_config());
    tokio::spawn(ollama_fut);
    if ollama_settings.enabled {
        info!("x.ollama.v1 task running (headless)");
    } else {
        info!("x.ollama.v1 task running but ollama_enabled=false (auto-reply off)");
    }

    // ------------------------------------------------------------------
    // Call controller (audio + Opus pipeline)
    // ------------------------------------------------------------------
    let (call_cmd_tx, _call_event_rx, call_fut) = CallController::split(Some(cmd_tx.clone()));
    tokio::spawn(call_fut);

    // ------------------------------------------------------------------
    // SFU client (room membership tracker)
    // ------------------------------------------------------------------
    let (_sfu_cmd_tx, _sfu_event_rx, sfu_fut) = SfuClient::split(Some(cmd_tx.clone()));
    tokio::spawn(sfu_fut);

    // ------------------------------------------------------------------
    // Application event loop (headless until UI is wired in)
    // ------------------------------------------------------------------
    run_headless(
        cmd_tx,
        event_rx,
        chat_store,
        peer_store,
        room_store,
        Arc::clone(&identity),
        updater_cmd_tx,
        ollama_cmd_tx,
        ollama_event_rx,
        call_cmd_tx,
    )
    .await;
}

/// Unlock or create a new identity.
#[cfg(not(feature = "qt-ui"))]
fn unlock_identity(key_dir: &std::path::Path) -> error::Result<Identity> {
    // Try v2 encrypted identity
    let dat = key_dir.join(identity::IDENTITY_FILENAME);
    if dat.exists() {
        // CONQUERD_PASSPHRASE and/or CONQUERD_PASSPHRASE_FILE env vars take
        // precedence (CI / scripted use).  Either or both may be set.
        let env_pass = std::env::var("CONQUERD_PASSPHRASE").unwrap_or_default();
        let env_file = std::env::var("CONQUERD_PASSPHRASE_FILE").unwrap_or_default();
        if !env_pass.is_empty() || !env_file.is_empty() {
            let material = crypto::build_passphrase_material(&env_pass, &env_file)?;
            return Identity::load_with_passphrase(&material, key_dir);
        }
        // Try OS keyring (silent)
        if let Ok((id, _)) = Identity::load_with_keyring_or_passphrase(b"", key_dir) {
            return Ok(id);
        }
        // Keyring unavailable — prompt on stdin
        let typed = stdin_prompt("Passphrase: ");
        if !typed.is_empty() {
            return Identity::load_with_passphrase(typed.as_bytes(), key_dir);
        }
        return Err(error::ClientError::Identity(
            "Passphrase required. Set CONQUERD_PASSPHRASE / CONQUERD_PASSPHRASE_FILE or enter it when prompted.".into(),
        ));
    }

    // ── First launch: no identity exists yet ──────────────────────────────
    first_launch_setup(key_dir)
}

/// Interactive first-launch setup: generate a new identity and encrypt it.
#[cfg(not(feature = "qt-ui"))]
fn first_launch_setup(key_dir: &std::path::Path) -> error::Result<Identity> {
    eprintln!("\nWelcome to ConquerD!");
    eprintln!("No identity found. A new one will be generated now.");
    eprintln!("Choose a passphrase to protect it (press Enter for no passphrase):\n");

    let pass1 = stdin_prompt("New passphrase: ");
    let pass2 = if pass1.is_empty() {
        String::new()
    } else {
        stdin_prompt("Confirm passphrase: ")
    };

    if pass1 != pass2 {
        return Err(error::ClientError::Identity(
            "Passphrases do not match".into(),
        ));
    }

    std::fs::create_dir_all(key_dir)
        .map_err(|e| error::ClientError::Identity(format!("Cannot create key dir: {e}")))?;

    let id = Identity::generate();
    id.save_encrypted(pass1.as_bytes(), key_dir)?;
    if pass1.is_empty() {
        eprintln!("\nIdentity created (no passphrase).");
    } else {
        eprintln!("\nIdentity created and encrypted with your passphrase.");
    }
    info!(
        "New identity generated: {} ({})",
        id.public_id(),
        id.peer_id()
    );
    Ok(id)
}

/// Read a line from stdin with a visible prompt (does not echo — use for
/// passphrases in a real terminal; no TTY suppression needed in headless mode).
#[cfg(not(feature = "qt-ui"))]
fn stdin_prompt(prompt: &str) -> String {
    eprint!("{prompt}");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim_end_matches(['\n', '\r']).to_string()
}

/// Settings-driven Ollama auto-reply smoke test (no identity unlock).
///
/// Reads `$CONQUERD_HOME/settings.json` (or defaults), requires
/// `ollama_enabled` + `ollama_auto_respond_direct`, runs one Query using the
/// configured model (e.g. gemma3:latest), prints the reply, exits 0/1/2.
#[cfg(not(feature = "qt-ui"))]
async fn ollama_only_auto_reply_test() {
    let settings = ollama_module::read_assistant_settings();
    let prompt = std::env::var("CONQUERD_SIMULATE_INBOUND_CHAT")
        .unwrap_or_else(|_| "Reply with exactly the single word: pong".to_owned());
    info!(
        "[ollama-only] settings path={:?} enabled={} model={} auto_direct={} base={}",
        Identity::default_key_dir().join("settings.json"),
        settings.enabled,
        settings.model,
        settings.auto_respond_direct,
        settings.base_url
    );
    println!(
        "[ollama-only] model={} enabled={} auto_direct={}",
        settings.model, settings.enabled, settings.auto_respond_direct
    );
    println!("[ollama-only] prompt={prompt}");

    if !settings.enabled {
        error!("[ollama-only] ollama_enabled=false in settings.json");
        std::process::exit(2);
    }
    if !settings.auto_respond_direct {
        error!("[ollama-only] ollama_auto_respond_direct=false in settings.json");
        std::process::exit(2);
    }
    if settings.model.is_empty() {
        error!("[ollama-only] ollama_model is empty");
        std::process::exit(2);
    }

    let (cmd_tx, mut event_rx, fut) = ollama_module::OllamaModule::split(settings.to_config());
    tokio::spawn(fut);

    let request_id = "auto-direct-sim-test".to_owned();
    let _ = cmd_tx
        .send(ollama_module::OllamaCommand::SetConfig(
            settings.to_config(),
        ))
        .await;
    let _ = cmd_tx
        .send(ollama_module::OllamaCommand::Query {
            request_id: request_id.clone(),
            prompt,
            system_prompt: settings.system_prompt,
        })
        .await;

    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            error!("[ollama-only] timed out waiting for reply");
            let _ = cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
            std::process::exit(1);
        }
        match tokio::time::timeout(left, event_rx.recv()).await {
            Ok(Some(ollama_module::OllamaEvent::Chunk(c))) if c.request_id == request_id => {
                if !c.text.is_empty() {
                    buf.push_str(&c.text);
                    eprint!("{}", c.text);
                }
                if c.done {
                    println!();
                    let reply = buf.trim();
                    if reply.is_empty() {
                        error!("[ollama-only] empty reply");
                        let _ = cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
                        std::process::exit(1);
                    }
                    println!(
                        "\n=== OLLAMA AUTO-REPLY OK ===\n{reply}\n===========================\n"
                    );
                    info!("[ollama-only] success chars={}", reply.chars().count());
                    let _ = cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
                    std::process::exit(0);
                }
            }
            Ok(Some(ollama_module::OllamaEvent::Error {
                request_id: rid,
                message,
            })) if rid == request_id => {
                error!("[ollama-only] error: {message}");
                println!("\n=== OLLAMA AUTO-REPLY ERROR ===\n{message}\n");
                let _ = cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
                std::process::exit(1);
            }
            Ok(Some(_)) => {}
            Ok(None) => {
                error!("[ollama-only] event channel closed");
                std::process::exit(1);
            }
            Err(_) => {
                error!("[ollama-only] timed out waiting for reply");
                let _ = cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
                std::process::exit(1);
            }
        }
    }
}

/// Headless event loop — processes `ConnectionEvent`s until shutdown.
#[allow(clippy::too_many_arguments)]
#[cfg(not(feature = "qt-ui"))]
async fn run_headless(
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    mut event_rx: mpsc::Receiver<ConnectionEvent>,
    chat_store: Arc<ChatStore>,
    peer_store: Arc<RwLock<PeerStore>>,
    room_store: Arc<RwLock<RoomStore>>,
    identity: Arc<Identity>,
    updater_cmd_tx: mpsc::Sender<github_updater::UpdaterCommand>,
    ollama_cmd_tx: mpsc::Sender<ollama_module::OllamaCommand>,
    mut ollama_event_rx: mpsc::Receiver<ollama_module::OllamaEvent>,
    call_cmd_tx: mpsc::Sender<call_controller::CallCommand>,
) {
    use tokio::signal;

    info!("Running in headless mode. Press Ctrl+C to exit.");
    info!(
        "Identity {} ({}) - same profile as GUI when CONQUERD_HOME matches",
        identity.public_id(),
        identity.peer_id()
    );
    warn!(
        "[headless] Do NOT run GUI ClientA at the same time as this process. \
         The supernode keeps one socket per peer id per node — a human must use a \
         *different* identity/profile to talk to this bot."
    );

    // Register URI scheme handler so `conquerd://` links open this process.
    platform::register_uri_scheme();

    // Trigger an immediate update check at startup
    let _ = updater_cmd_tx.try_send(github_updater::UpdaterCommand::Check);

    // Optional auto-reply smoke test without a second peer:
    //   CONQUERD_SIMULATE_INBOUND_CHAT="hello, reply with pong only"
    // Uses ClientA settings (model/system prompt/auto flags). Exits after reply
    // when CONQUERD_SIMULATE_EXIT=1 (default).
    if let Ok(sim) = std::env::var("CONQUERD_SIMULATE_INBOUND_CHAT") {
        if !sim.is_empty() {
            let otx = ollama_cmd_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let settings = ollama_module::read_assistant_settings();
                info!(
                    "[ollama-sim] simulating inbound chat; model={} auto_direct={}",
                    settings.model, settings.auto_respond_direct
                );
                if !settings.enabled || !settings.auto_respond_direct {
                    error!(
                        "[ollama-sim] ollama_enabled={} auto_respond_direct={} — enable both in settings.json",
                        settings.enabled, settings.auto_respond_direct
                    );
                    if std::env::var("CONQUERD_SIMULATE_EXIT").unwrap_or_else(|_| "1".into()) == "1"
                    {
                        std::process::exit(2);
                    }
                    return;
                }
                let _ = otx
                    .send(ollama_module::OllamaCommand::SetConfig(
                        settings.to_config(),
                    ))
                    .await;
                let _ = otx
                    .send(ollama_module::OllamaCommand::Query {
                        request_id: "auto-direct-sim-test".to_owned(),
                        prompt: sim,
                        system_prompt: settings.system_prompt,
                    })
                    .await;
                info!("[ollama-sim] Query dispatched (auto-direct-sim-test)");
            });
        }
    }

    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };

    tokio::pin!(ctrl_c);

    // Pending auto-reply streams (request_id → target).
    let mut auto_pending: HashMap<String, HeadlessAutoTarget> = HashMap::new();
    let mut auto_buf: HashMap<String, String> = HashMap::new();
    // Hosts (pad-stripped) already rematerialized this session.
    let mut rematerialized_hosts: HashSet<String> = HashSet::new();
    // Cluster rosters: member_id → sibling public_ids (for multi-home rematerialize).
    let mut cluster_siblings: HashMap<String, Vec<String>> = HashMap::new();
    let sim_exit_on_done = std::env::var("CONQUERD_SIMULATE_INBOUND_CHAT").is_ok()
        && std::env::var("CONQUERD_SIMULATE_EXIT").unwrap_or_else(|_| "1".into()) == "1";
    let local_handle = read_local_handle_setting();

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                info!("Shutdown signal received");
                let _ = cmd_tx.send(ConnectionCommand::Shutdown).await;
                let _ = updater_cmd_tx.send(github_updater::UpdaterCommand::Shutdown).await;
                let _ = ollama_cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
                break;
            }
            Some(ev) = event_rx.recv() => {
                handle_event(
                    ev,
                    &chat_store,
                    &peer_store,
                    &room_store,
                    &identity,
                    &cmd_tx,
                    &call_cmd_tx,
                    &ollama_cmd_tx,
                    &mut auto_pending,
                    &mut auto_buf,
                    &mut rematerialized_hosts,
                    &mut cluster_siblings,
                    &local_handle,
                ).await;
            }
            Some(ev) = ollama_event_rx.recv() => {
                let done = handle_ollama_event(
                    ev,
                    &cmd_tx,
                    &chat_store,
                    &mut auto_pending,
                    &mut auto_buf,
                    &local_handle,
                    identity.public_id().as_str(),
                ).await;
                if done && sim_exit_on_done {
                    info!("[ollama-sim] auto-reply simulation complete — exiting");
                    let _ = ollama_cmd_tx.send(ollama_module::OllamaCommand::Shutdown).await;
                    let _ = cmd_tx.send(ConnectionCommand::Shutdown).await;
                    break;
                }
            }
        }
    }
}

#[cfg(not(feature = "qt-ui"))]
fn read_local_handle_setting() -> String {
    let path = Identity::default_key_dir().join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.get("local_handle")
                .and_then(|x| x.as_str())
                .map(|s| s.to_owned())
        })
        .unwrap_or_default()
}

/// Rematerialize client-owned rooms onto a live supernode and subscribe to chat
/// (including the built-in `default` room). Mirrors the GUI bridge path.
#[cfg(not(feature = "qt-ui"))]
fn headless_rematerialize_and_subscribe(
    room_store: &RoomStore,
    peer_store: &PeerStore,
    cmd_tx: &mpsc::Sender<ConnectionCommand>,
    live_host: &str,
    source_member_ids: &[String],
    my_public_id: &str,
    identity: &Identity,
) {
    let entries = if source_member_ids.is_empty() {
        room_store.list_for_supernode_resolved(peer_store, live_host)
    } else {
        room_store.list_for_cluster_members(peer_store, source_member_ids)
    };

    let mut subscribed: HashSet<String> = HashSet::new();

    // Built-in public room — always present on the supernode; chat-only subscribe.
    let _ = cmd_tx.try_send(ConnectionCommand::SubscribeRoomChat {
        supernode_id: live_host.to_owned(),
        room_id: "default".to_owned(),
    });
    subscribed.insert("default".to_owned());
    info!(
        "[headless] subscribed room chat: default on {}…",
        &live_host[..12.min(live_host.len())]
    );

    for entry in entries {
        if entry.room_id == "default" {
            continue;
        }
        let hidden = room_store.is_hidden_from_sidebar(&entry.supernode_id, &entry.room_id)
            || room_store.is_hidden_from_sidebar(live_host, &entry.room_id)
            || source_member_ids
                .iter()
                .any(|k| room_store.is_hidden_from_sidebar(k, &entry.room_id));
        if hidden {
            info!(
                "[headless] skip hidden room {} ({})",
                entry.room_name, entry.room_id
            );
            continue;
        }
        let creator_id = if entry.creator_id.is_empty() {
            my_public_id.to_owned()
        } else {
            entry.creator_id.clone()
        };
        info!(
            "[headless] rematerialize room '{}' ({}) type={} onto {}…",
            entry.room_name,
            entry.room_id,
            entry.room_type,
            &live_host[..12.min(live_host.len())]
        );
        let _ = cmd_tx.try_send(ConnectionCommand::CreateRoom {
            supernode_id: live_host.to_owned(),
            room_name: entry.room_name.clone(),
            room_type: entry.room_type.clone(),
            room_id: Some(entry.room_id.clone()),
            creator_id: Some(creator_id),
            materialize_only: true,
            invite_policy: entry.invite_policy.clone(),
            invite_token: entry.invite_token.clone(),
        });
        if subscribed.insert(entry.room_id.clone()) {
            let _ = cmd_tx.try_send(ConnectionCommand::SubscribeRoomChat {
                supernode_id: live_host.to_owned(),
                room_id: entry.room_id.clone(),
            });
            info!(
                "[headless] subscribed room chat: {} ({})",
                entry.room_name, entry.room_id
            );
        }
    }

    // Space-root re-broadcast (same as GUI) so cluster members learn the tree.
    let space_hosts: Vec<&str> = std::iter::once(live_host)
        .chain(source_member_ids.iter().map(String::as_str))
        .collect();
    for host in space_hosts {
        let space_id = RoomStore::space_id_for(my_public_id, host);
        if let Some(space) = room_store.get_space(&space_id) {
            let issued_at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let root = space.signed_root(issued_at, |b| identity.sign(b));
            if let Ok(root_json) = serde_json::to_string(&root) {
                let _ = cmd_tx.try_send(ConnectionCommand::AnnounceSpaceRoot {
                    supernode_id: live_host.to_owned(),
                    root_json,
                });
                info!("[headless] announced Space root for {space_id}");
            }
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
#[cfg(not(feature = "qt-ui"))]
async fn handle_event(
    ev: ConnectionEvent,
    chat_store: &ChatStore,
    peer_store: &RwLock<PeerStore>,
    room_store: &RwLock<RoomStore>,
    identity: &Identity,
    cmd_tx: &mpsc::Sender<ConnectionCommand>,
    call_cmd_tx: &mpsc::Sender<call_controller::CallCommand>,
    ollama_cmd_tx: &mpsc::Sender<ollama_module::OllamaCommand>,
    auto_pending: &mut HashMap<String, HeadlessAutoTarget>,
    auto_buf: &mut HashMap<String, String>,
    rematerialized_hosts: &mut HashSet<String>,
    cluster_siblings: &mut HashMap<String, Vec<String>>,
    local_handle: &str,
) {
    match ev {
        ConnectionEvent::PeerConnected(pid) => {
            info!("Peer connected: {}", pid);
            // Ensure call controller has session bookkeeping for this peer.
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::InitiatePeer {
                peer_id: pid,
                host: None,
                port: None,
            });
        }
        ConnectionEvent::PeerDisconnected(pid) => {
            info!("Peer disconnected: {}", pid);
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::RemovePeer { peer_id: pid });
        }
        ConnectionEvent::SupernodeConnected(u) => {
            info!("Supernode connected: {}", u);
            let host_key = u.trim_end_matches('=').to_owned();
            if rematerialized_hosts.contains(&host_key) {
                info!("[headless] already rematerialized on this host this session");
                return;
            }
            let mut sources: Vec<String> =
                cluster_siblings.get(&host_key).cloned().unwrap_or_default();
            // Also try pad-full id as cluster key.
            if sources.is_empty() {
                sources = cluster_siblings.get(&u).cloned().unwrap_or_default();
            }
            if sources.is_empty()
                || !sources
                    .iter()
                    .any(|s| s.trim_end_matches('=') == host_key.as_str())
            {
                sources.push(u.clone());
            }
            headless_rematerialize_and_subscribe(
                &room_store.read(),
                &peer_store.read(),
                cmd_tx,
                &u,
                &sources,
                identity.public_id().as_str(),
                identity,
            );
            rematerialized_hosts.insert(host_key);

            // After a cold start the elected keyer may have already sealed the
            // current epoch to our peer id *before* this process owned the WS
            // socket (e.g. a previous GUI session). Keys are in-memory only —
            // re-subscribe a moment later to force broadcast_sfu_members and a
            // fresh SfuGroupKey seal to *this* process.
            let cmd_tx_delayed = cmd_tx.clone();
            let host_delayed = u.clone();
            let rooms_delayed: Vec<String> = {
                let rs = room_store.read();
                let mut ids: Vec<String> = rs
                    .list()
                    .into_iter()
                    .filter(|e| !rs.is_hidden_from_sidebar(&e.supernode_id, &e.room_id))
                    .map(|e| e.room_id.clone())
                    .collect();
                if !ids.iter().any(|id| id == "default") {
                    ids.push("default".to_owned());
                }
                ids
            };
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                for room_id in rooms_delayed {
                    info!(
                        "[headless] delayed re-subscribe for key reseal: room={room_id} host={}…",
                        &host_delayed[..12.min(host_delayed.len())]
                    );
                    let _ = cmd_tx_delayed
                        .send(ConnectionCommand::SubscribeRoomChat {
                            supernode_id: host_delayed.clone(),
                            room_id,
                        })
                        .await;
                }
            });
        }
        ConnectionEvent::SupernodeDisconnected(u) => {
            info!("Supernode disconnected: {}", u);
            rematerialized_hosts.remove(u.trim_end_matches('='));
        }
        ConnectionEvent::ClusterMembersUpdated {
            supernode_id,
            members,
        } => {
            let key = supernode_id.trim_end_matches('=').to_owned();
            info!(
                "[headless] cluster roster from {}…: {} sibling(s)",
                &key[..12.min(key.len())],
                members.len()
            );
            cluster_siblings.insert(key.clone(), members.clone());
            // Rematerialize any live host that hasn't been done yet (roster often
            // arrives after SupernodeConnected).
            let mut sources = members;
            if !sources
                .iter()
                .any(|s| s.trim_end_matches('=') == key.as_str())
            {
                sources.push(supernode_id.clone());
            }
            // Prefer rematerializing onto the reporting member if not yet done.
            if !rematerialized_hosts.contains(&key) {
                headless_rematerialize_and_subscribe(
                    &room_store.read(),
                    &peer_store.read(),
                    cmd_tx,
                    &supernode_id,
                    &sources,
                    identity.public_id().as_str(),
                    identity,
                );
                rematerialized_hosts.insert(key);
            }
        }
        ConnectionEvent::ChatMessage {
            peer_id,
            message_id,
            body,
            timestamp,
            sender_handle,
        } => {
            info!("[direct] {} ({}): {}", sender_handle, peer_id, body);
            // Persist to chat store
            let msg = chat_store::ChatMessage {
                id: message_id.clone(),
                peer_id: peer_id.clone(),
                sender: peer_id.clone(),
                recipient: String::new(),
                body: body.clone(),
                timestamp,
                is_self: false,
                status: chat_store::MessageStatus::Delivered,
                kind: chat_store::MessageKind::Text,
                attachment_name: String::new(),
                attachment_path: String::new(),
                size_str: String::new(),
                status_note: String::new(),
                sender_handle,
            };
            if let Err(e) = chat_store.upsert(&msg) {
                error!("Failed to persist chat message: {}", e);
            }
            headless_maybe_auto_reply(
                ollama_cmd_tx,
                auto_pending,
                auto_buf,
                HeadlessAutoTarget::Direct {
                    peer_id: peer_id.clone(),
                },
                &message_id,
                &body,
            )
            .await;
        }
        ConnectionEvent::ChatAck {
            peer_id: _,
            message_id,
        } => {
            if let Err(e) =
                chat_store.update_status(&message_id, chat_store::MessageStatus::Delivered)
            {
                error!("Failed to update message status: {}", e);
            }
        }
        ConnectionEvent::ChatSendFailed {
            peer_id: _,
            message_id,
            reason,
        } => {
            if let Err(e) = chat_store.update_status_note(
                &message_id,
                chat_store::MessageStatus::Failed,
                &reason,
            ) {
                error!("Failed to update failed message status: {}", e);
            }
        }
        ConnectionEvent::CallRequest { peer_id, .. } => {
            info!("Incoming call from {}", peer_id);
            // Ring and ensure session exists. (The headless client ignores the
            // fallback_* room coordinates — room-mode call answering is a Qt
            // bridge flow.)
            platform::play_ringtone();
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::InitiatePeer {
                peer_id,
                host: None,
                port: None,
            });
        }
        ConnectionEvent::CallAccepted { peer_id } => {
            info!("Call accepted by {}", peer_id);
            platform::stop_ringtone();
            let va = std::fs::read_to_string(
                std::env::var("CONQUERD_HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| {
                        std::env::var("USERPROFILE")
                            .or_else(|_| std::env::var("HOME"))
                            .map(std::path::PathBuf::from)
                            .unwrap_or_else(|_| std::path::PathBuf::from("."))
                            .join(".conquerd")
                    })
                    .join("settings.json"),
            )
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.get("voice_activation").and_then(|x| x.as_bool()))
            .unwrap_or(false);
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::StartAudio {
                voice_activation: va,
            });
        }
        ConnectionEvent::CallEnded { peer_id } => {
            info!("Call ended with {}", peer_id);
            platform::stop_ringtone();
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::StopAudio);
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::RemovePeer { peer_id });
        }
        ConnectionEvent::CallFallbackRoomReady {
            peer_id,
            supernode_id,
            room_id,
        } => {
            info!(
                "Direct call to {} falling back to room {} on {}",
                peer_id, room_id, supernode_id
            );
            let _ = call_cmd_tx.try_send(call_controller::CallCommand::SetRoomMode {
                supernode_id,
                room_id,
            });
        }
        ConnectionEvent::RelayGranted {
            supernode_id,
            ticket: _,
            relay_host,
            relay_port,
            portal_only: _,
        } => {
            info!(
                "Relay granted by {} at {}:{}",
                supernode_id, relay_host, relay_port
            );
        }
        ConnectionEvent::RoomChatMessage {
            supernode_id,
            room_id,
            sender_id,
            sender_handle,
            body,
            timestamp,
            message_id,
        } => {
            let mine = !identity.public_id().is_empty()
                && (sender_id == identity.public_id() || sender_id == identity.peer_id());
            info!(
                "[room {}:{}] {} ({}): {}{}",
                &supernode_id[..8.min(supernode_id.len())],
                room_id,
                sender_handle,
                &sender_id[..8.min(sender_id.len())],
                body,
                if mine {
                    " [own — auto-reply skipped]"
                } else {
                    ""
                }
            );
            if mine {
                // Local SFU + cluster replicate paths skip the author; remaining
                // own frames are rare races or stale nodes without the skip.
                // This is *not* the human sharing this bot's identity.
                tracing::debug!(
                    "[headless] skipping own room message (local echo or multi-home race)"
                );
            }
            let message_id = if message_id.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                message_id
            };
            let store_key = format!("room:{supernode_id}:{room_id}");
            let msg = chat_store::ChatMessage {
                id: message_id.clone(),
                peer_id: store_key,
                sender: sender_id.clone(),
                recipient: room_id.clone(),
                body: body.clone(),
                timestamp,
                is_self: mine,
                status: chat_store::MessageStatus::Delivered,
                kind: chat_store::MessageKind::Text,
                attachment_name: String::new(),
                attachment_path: String::new(),
                size_str: String::new(),
                status_note: String::new(),
                sender_handle,
            };
            if let Err(e) = chat_store.upsert(&msg) {
                error!("Failed to persist room chat: {e}");
            }
            if !mine {
                headless_maybe_auto_reply(
                    ollama_cmd_tx,
                    auto_pending,
                    auto_buf,
                    HeadlessAutoTarget::Room {
                        supernode_id,
                        room_id,
                    },
                    &message_id,
                    &body,
                )
                .await;
            }
        }
        ConnectionEvent::RoomCreated {
            supernode_id,
            room_id,
            room_name,
            ..
        } => {
            info!(
                "[headless] room created/materialized: '{room_name}' ({room_id}) on {}...",
                &supernode_id[..12.min(supernode_id.len())]
            );
            // Re-subscribe after materialize completes - a Subscribe issued in
            // the same tick as CreateRoom can race the supernode ACL seed and
            // silently fail (private rooms).
            let _ = cmd_tx.try_send(ConnectionCommand::SubscribeRoomChat {
                supernode_id: supernode_id.clone(),
                room_id: room_id.clone(),
            });
            info!("[headless] re-subscribed room chat after materialize: {room_name} ({room_id})");
        }
        ConnectionEvent::SignalingMessage(msg) => {
            info!("Unhandled signaling message: {:?}", msg.msg_type);
        }
        ConnectionEvent::SessionStateUpdate(_) => {}
        // Bridge-only / less relevant for headless Ollama testing.
        ConnectionEvent::TypingIndicator { .. }
        | ConnectionEvent::HandleUpdated { .. }
        | ConnectionEvent::RoomMembersChanged { .. }
        | ConnectionEvent::RoomJoinRejected { .. }
        | ConnectionEvent::RoomPeerJoined { .. }
        | ConnectionEvent::RoomPeerLeft { .. }
        | ConnectionEvent::CapabilityAnnounced { .. }
        | ConnectionEvent::CapabilityInvoked { .. }
        | ConnectionEvent::CapabilityInvokePending { .. }
        | ConnectionEvent::EndpointUpdated { .. }
        | ConnectionEvent::RoomListReceived { .. }
        | ConnectionEvent::RoomInviteReady { .. }
        | ConnectionEvent::PresenceUpdated { .. }
        | ConnectionEvent::InviteAccepted { .. }
        | ConnectionEvent::InviteFailed { .. }
        | ConnectionEvent::FileOffered { .. }
        | ConnectionEvent::FileProgress { .. }
        | ConnectionEvent::FileComplete { .. }
        | ConnectionEvent::FileFailed { .. }
        | ConnectionEvent::SupernodeInfoReceived { .. }
        | ConnectionEvent::RelayPaymentRequired { .. }
        | ConnectionEvent::SfuAudioReceived { .. }
        | ConnectionEvent::DirectAudioReceived { .. }
        // Headless has no display, so video has nowhere to go and no camera to
        // send from — the indicator and keyframe events are equally moot.
        | ConnectionEvent::VideoFrameReceived { .. }
        | ConnectionEvent::ContentAudioReceived { .. }
        | ConnectionEvent::PeerVideoStateChanged { .. }
        | ConnectionEvent::VideoKeyframeRequested { .. }
        | ConnectionEvent::AvatarConfigUpdated { .. }
        | ConnectionEvent::RoomFailedOver { .. }
        | ConnectionEvent::ConnectionStats { .. }
        | ConnectionEvent::PortalGameDatagram { .. } => {}
    }
    let _ = local_handle;
}

/// Start an auto-reply when settings allow it (direct or room).
#[cfg(not(feature = "qt-ui"))]
async fn headless_maybe_auto_reply(
    ollama_cmd_tx: &mpsc::Sender<ollama_module::OllamaCommand>,
    auto_pending: &mut HashMap<String, HeadlessAutoTarget>,
    auto_buf: &mut HashMap<String, String>,
    target: HeadlessAutoTarget,
    message_id: &str,
    body: &str,
) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    let settings = ollama_module::read_assistant_settings();
    if !settings.enabled {
        return;
    }
    let want = match &target {
        HeadlessAutoTarget::Direct { .. } => settings.auto_respond_direct,
        HeadlessAutoTarget::Room { .. } => settings.auto_respond_room,
    };
    if !want {
        return;
    }
    let request_id = match &target {
        HeadlessAutoTarget::Direct { peer_id } => {
            format!("auto-direct-{peer_id}-{message_id}")
        }
        HeadlessAutoTarget::Room {
            supernode_id,
            room_id,
        } => format!("auto-room-{supernode_id}-{room_id}-{message_id}"),
    };
    // Cancel prior reply to the same logical target. Room targets match on
    // room_id only so multi-home deliveries of the same room don't run two
    // concurrent Ollama streams with split history.
    let stale: Vec<String> = auto_pending
        .iter()
        .filter(|(_, t)| match (t, &target) {
            (
                HeadlessAutoTarget::Direct { peer_id: a },
                HeadlessAutoTarget::Direct { peer_id: b },
            ) => a == b,
            (
                HeadlessAutoTarget::Room { room_id: ra, .. },
                HeadlessAutoTarget::Room { room_id: rb, .. },
            ) => ra == rb,
            _ => false,
        })
        .map(|(id, _)| id.clone())
        .collect();
    for old in stale {
        let _ = ollama_cmd_tx
            .send(ollama_module::OllamaCommand::Cancel {
                request_id: old.clone(),
            })
            .await;
        auto_pending.remove(&old);
        auto_buf.remove(&old);
    }
    let conversation_id = match &target {
        HeadlessAutoTarget::Direct { peer_id } => ollama_module::conversation_id_direct(peer_id),
        HeadlessAutoTarget::Room { room_id, .. } => ollama_module::conversation_id_room(room_id),
    };
    let sys = if settings.system_prompt.trim().is_empty() {
        "You are a helpful assistant in a private peer-to-peer chat. \
         Remember earlier turns in this conversation and reply with continuity. \
         Keep replies concise."
            .to_owned()
    } else {
        format!(
            "{}\n\n(You are in a multi-turn chat; use prior messages in this conversation for context.)",
            settings.system_prompt.trim()
        )
    };
    info!(
        "[ollama] auto-reply start rid={request_id} model={} conv={conversation_id} target={target:?}",
        settings.model
    );
    let _ = ollama_cmd_tx
        .send(ollama_module::OllamaCommand::SetConfig(
            settings.to_config(),
        ))
        .await;
    if ollama_cmd_tx
        .send(ollama_module::OllamaCommand::Chat {
            request_id: request_id.clone(),
            conversation_id,
            user_message: body.to_owned(),
            system_prompt: sys,
        })
        .await
        .is_err()
    {
        error!("[ollama] auto-reply Chat send failed");
        return;
    }
    auto_pending.insert(request_id.clone(), target);
    auto_buf.insert(request_id, String::new());
}

/// Handle Ollama stream events in headless mode.
/// Returns `true` when a simulation auto-reply finishes (for exit hook).
#[cfg(not(feature = "qt-ui"))]
async fn handle_ollama_event(
    ev: ollama_module::OllamaEvent,
    cmd_tx: &mpsc::Sender<ConnectionCommand>,
    chat_store: &ChatStore,
    auto_pending: &mut HashMap<String, HeadlessAutoTarget>,
    auto_buf: &mut HashMap<String, String>,
    local_handle: &str,
    my_public_id: &str,
) -> bool {
    use ollama_module::OllamaEvent;
    use protocol::{MessageType, SignalingMessage};

    match ev {
        OllamaEvent::Chunk(chunk) => {
            if !chunk.request_id.starts_with("auto-") {
                return false;
            }
            if !chunk.text.is_empty() {
                auto_buf
                    .entry(chunk.request_id.clone())
                    .or_default()
                    .push_str(&chunk.text);
            }
            if !chunk.done {
                return false;
            }
            let is_sim = chunk.request_id == "auto-direct-sim-test";
            let target = auto_pending.remove(&chunk.request_id).or_else(|| {
                if is_sim {
                    Some(HeadlessAutoTarget::Direct {
                        peer_id: "sim".to_owned(),
                    })
                } else {
                    None
                }
            });
            let body = auto_buf
                .remove(&chunk.request_id)
                .unwrap_or_default()
                .trim()
                .to_owned();
            let Some(target) = target else {
                return is_sim;
            };
            if body.is_empty() {
                warn!("[ollama] auto-reply {} empty body", chunk.request_id);
                return is_sim;
            }
            info!(
                "[ollama] auto-reply done rid={} chars={} target={target:?}",
                chunk.request_id,
                body.chars().count()
            );
            println!("\n=== OLLAMA AUTO-REPLY ===\n{body}\n=========================\n");

            match target {
                HeadlessAutoTarget::Direct { peer_id }
                    if peer_id != "sim" && !peer_id.is_empty() =>
                {
                    let message_id = uuid::Uuid::new_v4().to_string();
                    let now_ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);
                    let chat_msg = chat_store::ChatMessage {
                        id: message_id.clone(),
                        peer_id: peer_id.clone(),
                        sender: my_public_id.to_owned(),
                        recipient: peer_id.clone(),
                        body: body.clone(),
                        timestamp: now_ts,
                        is_self: true,
                        status: chat_store::MessageStatus::Sending,
                        kind: chat_store::MessageKind::Text,
                        attachment_name: String::new(),
                        attachment_path: String::new(),
                        size_str: String::new(),
                        status_note: String::new(),
                        sender_handle: local_handle.to_owned(),
                    };
                    if let Err(e) = chat_store.upsert(&chat_msg) {
                        error!("Failed to persist auto-reply: {e}");
                    }
                    let mut msg =
                        SignalingMessage::new(MessageType::ChatMessage, my_public_id.to_owned());
                    msg.target = Some(peer_id);
                    msg.payload
                        .insert("body".into(), serde_json::Value::String(body));
                    msg.payload
                        .insert("message_id".into(), serde_json::Value::String(message_id));
                    if !local_handle.is_empty() {
                        msg.payload.insert(
                            "sender_handle".into(),
                            serde_json::Value::String(local_handle.to_owned()),
                        );
                    }
                    let _ = cmd_tx.send(ConnectionCommand::SendMessage(msg)).await;
                }
                HeadlessAutoTarget::Room {
                    supernode_id,
                    room_id,
                } => {
                    let message_id = uuid::Uuid::new_v4().to_string();
                    info!(
                        "[ollama] dispatching room auto-reply via SN {}… room={} mid={}",
                        &supernode_id[..12.min(supernode_id.len())],
                        room_id,
                        &message_id[..8.min(message_id.len())]
                    );
                    if cmd_tx
                        .send(ConnectionCommand::SendSfuChat {
                            supernode_id,
                            room_id,
                            body,
                            sender_handle: local_handle.to_owned(),
                            message_id,
                        })
                        .await
                        .is_err()
                    {
                        error!(
                            "[ollama] SendSfuChat channel closed — auto-reply not delivered to peers"
                        );
                    }
                }
                HeadlessAutoTarget::Direct { .. } => {}
            }
            is_sim
        }
        OllamaEvent::Error {
            request_id,
            message,
        } => {
            if request_id.starts_with("auto-") {
                auto_pending.remove(&request_id);
                auto_buf.remove(&request_id);
                error!("[ollama] auto-reply {request_id} failed: {message}");
                println!("\n=== OLLAMA AUTO-REPLY ERROR ===\n{message}\n");
                return request_id == "auto-direct-sim-test";
            }
            false
        }
        OllamaEvent::Models { models, error } => {
            if error.is_empty() {
                info!("[ollama] models: {models:?}");
            } else {
                warn!("[ollama] models error: {error}");
            }
            false
        }
    }
}
