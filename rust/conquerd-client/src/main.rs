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
// Public API items are consumed by the Qt UI layer (qt-ui feature) and by
// integration tests. Suppress dead-code lints at the crate level.
#![allow(dead_code, unused_imports)]

mod avatar_config;
mod banner;
mod call_controller;
mod chat_manager;
mod chat_store;
mod connection_fallback;
mod connection_manager;
mod crypto;
mod error;
mod feature_trust;
mod file_transfer;
mod github_updater;
mod identity;
mod metrics;
mod network_monitor;
mod ollama_module;
mod peer_store;
mod platform;
mod plugin_manager;
mod plugin_runtime;
mod protocol;
mod quic_relay_client;
mod quic_tls;
mod relay;
mod ringtone;
mod room_manager;
mod room_store;
mod session_state;
mod sfu_client;
mod taskbar_badge;
mod ui;
mod upnp;
mod uri_scheme;
mod web_app_client;

use std::sync::Arc;

use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use crate::call_controller::CallController;
use crate::chat_store::ChatStore;
use crate::connection_manager::{ConnectionCommand, ConnectionEvent, ConnectionManager};
use crate::identity::Identity;
use crate::peer_store::PeerStore;
use crate::sfu_client::SfuClient;

/// On HiDPI Windows displays (e.g. 4K at 150%) Qt honours the OS DPI scale
/// factor, which makes Material's touch-sized controls (48 dp) appear very
/// large on desktop.  Setting `QT_SCALE_FACTOR=0.75` before `QGuiApplication`
/// is constructed gives `0.75 × 1.5 = 1.125` effective scale — compact and
/// sharp on 4K, no change on non-HiDPI monitors (96 DPI / 100%).
///
/// Only applied when the caller has not already set `QT_SCALE_FACTOR`.
/// Override with e.g. `set QT_SCALE_FACTOR=1.0` to disable.
#[cfg(target_os = "windows")]
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

#[cfg(not(target_os = "windows"))]
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
    if crate::platform::should_exit_as_duplicate_instance() {
        std::process::exit(0);
    }

    // Register the conquerd:// URL scheme BEFORE QGuiApplication is
    // created — this is a QtWebEngine requirement. No-op without webengine.
    #[cfg(feature = "webengine")]
    unsafe {
        ui::scheme::conquerd_register_scheme();
    }

    // Enable the Chromium QUIC stack so the embedded QtWebEngine can speak
    // HTTP/3 / WebTransport.  QtWebEngine ships with QUIC DISABLED by default,
    // so `new WebTransport(...)` constructs fine but the underlying QUIC
    // handshake never completes ("Opening handshake failed").  The flags must
    // be present in QTWEBENGINE_CHROMIUM_FLAGS before QtWebEngine reads the
    // Chromium command line (which happens during QGuiApplication setup).
    // We append to any user-provided value rather than overwrite it.
    #[cfg(feature = "webengine")]
    {
        let extra = "--enable-quic --enable-features=WebTransport";
        let merged = match std::env::var("QTWEBENGINE_CHROMIUM_FLAGS") {
            Ok(existing) if !existing.trim().is_empty() => {
                format!("{existing} {extra}")
            }
            _ => extra.to_owned(),
        };
        std::env::set_var("QTWEBENGINE_CHROMIUM_FLAGS", merged);
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
    // Logging — respects RUST_LOG env var
    fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("conquerd_client=info,warn")),
        )
        .init();

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
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(headless_main());
}

#[cfg(not(feature = "qt-ui"))]
async fn headless_main() {
    // Resolve key directory (can be overridden via CONQUERD_KEY_DIR)
    let key_dir = std::env::var("CONQUERD_KEY_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Identity::default_key_dir());

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
    // Connection manager
    // ------------------------------------------------------------------
    let (cmd_tx, event_rx, cm_fut) =
        ConnectionManager::split(Arc::clone(&identity), Arc::clone(&peer_store));
    tokio::spawn(cm_fut);

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
    // UPnP manager
    // ------------------------------------------------------------------
    let (upnp_cmd_tx, _upnp_event_rx, upnp_fut) = upnp::UPnPManager::split();
    tokio::spawn(upnp_fut);

    // ------------------------------------------------------------------
    // Ollama AI module
    // ------------------------------------------------------------------
    let (ollama_cmd_tx, _ollama_event_rx, ollama_fut) =
        ollama_module::OllamaModule::split(ollama_module::OllamaConfig::default());
    tokio::spawn(ollama_fut);

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
        upnp_cmd_tx,
        updater_cmd_tx,
        ollama_cmd_tx,
        call_cmd_tx,
    )
    .await;
}

/// Unlock or create a new identity.
fn unlock_identity(key_dir: &std::path::Path) -> error::Result<Identity> {
    // Try v2 encrypted identity
    let dat = key_dir.join(identity::IDENTITY_FILENAME);
    if dat.exists() {
        // CONQUERD_PASSPHRASE and/or CONQUERD_PASSPHRASE_FILE env vars take
        // precedence (CI / scripted use).  Either or both may be set.
        let env_pass = std::env::var("CONQUERD_PASSPHRASE").unwrap_or_default();
        let env_file = std::env::var("CONQUERD_PASSPHRASE_FILE").unwrap_or_default();
        if !env_pass.is_empty() || !env_file.is_empty() {
            let material = crate::crypto::build_passphrase_material(&env_pass, &env_file)?;
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

    // Try v1 plaintext (legacy migration)
    let json = key_dir.join(identity::KEY_FILENAME);
    if json.exists() {
        return Identity::load_v1(key_dir);
    }

    // ── First launch: no identity exists yet ──────────────────────────────
    first_launch_setup(key_dir)
}

/// Interactive first-launch setup: generate a new identity and encrypt it.
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
    if pass1.is_empty() {
        id.save_v1(key_dir)?;
        eprintln!("\nIdentity created (no passphrase).");
    } else {
        id.save_encrypted(pass1.as_bytes(), key_dir)?;
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
fn stdin_prompt(prompt: &str) -> String {
    eprint!("{prompt}");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim_end_matches(['\n', '\r']).to_string()
}

/// Headless event loop — processes `ConnectionEvent`s until shutdown.
async fn run_headless(
    cmd_tx: mpsc::Sender<ConnectionCommand>,
    mut event_rx: mpsc::Receiver<ConnectionEvent>,
    chat_store: Arc<ChatStore>,
    upnp_cmd_tx: mpsc::Sender<upnp::UpnpCommand>,
    updater_cmd_tx: mpsc::Sender<github_updater::UpdaterCommand>,
    _ollama_cmd_tx: mpsc::Sender<ollama_module::OllamaCommand>,
    call_cmd_tx: mpsc::Sender<call_controller::CallCommand>,
) {
    use tokio::signal;

    info!("Running in headless mode. Press Ctrl+C to exit.");

    // Register URI scheme handler so `conquerd://` links open this process.
    platform::register_uri_scheme();

    // Trigger an immediate update check at startup
    let _ = updater_cmd_tx.try_send(github_updater::UpdaterCommand::Check);

    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };

    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            _ = &mut ctrl_c => {
                info!("Shutdown signal received");
                let _ = cmd_tx.send(ConnectionCommand::Shutdown).await;
                let _ = upnp_cmd_tx.send(upnp::UpnpCommand::Shutdown).await;
                let _ = updater_cmd_tx.send(github_updater::UpdaterCommand::Shutdown).await;
                break;
            }
            Some(ev) = event_rx.recv() => {
                handle_event(ev, &chat_store, &upnp_cmd_tx, &call_cmd_tx).await;
            }
        }
    }
}

async fn handle_event(
    ev: ConnectionEvent,
    chat_store: &ChatStore,
    upnp_cmd_tx: &mpsc::Sender<upnp::UpnpCommand>,
    call_cmd_tx: &mpsc::Sender<call_controller::CallCommand>,
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
        ConnectionEvent::SupernodeConnected(u) => info!("Supernode connected: {}", u),
        ConnectionEvent::SupernodeDisconnected(u) => info!("Supernode disconnected: {}", u),
        ConnectionEvent::ChatMessage {
            peer_id,
            message_id,
            body,
            timestamp,
            sender_handle,
        } => {
            info!("[{}] {}: {}", peer_id, sender_handle, body);
            // Persist to chat store
            let msg = chat_store::ChatMessage {
                id: message_id,
                peer_id: peer_id.clone(),
                sender: peer_id,
                recipient: String::new(),
                body,
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
        ConnectionEvent::CallRequest { peer_id } => {
            info!("Incoming call from {}", peer_id);
            // Ring and ensure session exists.
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
        ConnectionEvent::RelayGranted {
            supernode_id,
            ticket: _,
            relay_host,
            relay_port,
        } => {
            info!(
                "Relay granted by {} at {}:{}",
                supernode_id, relay_host, relay_port
            );
            // Request UPnP mapping for the relay port so peers can reach us
            let _ = upnp_cmd_tx.try_send(upnp::UpnpCommand::AddMapping {
                internal_port: relay_port,
                external_port: relay_port,
                protocol: upnp::Protocol::Udp,
                description: "ConquerD QUIC relay".to_string(),
            });
        }
        ConnectionEvent::SignalingMessage(msg) => {
            info!("Unhandled signaling message: {:?}", msg.msg_type);
        }
        ConnectionEvent::SessionStateUpdate(_) => {}
        // New events handled by the Qt bridge's dispatch_event — ignore here.
        ConnectionEvent::TypingIndicator { .. }
        | ConnectionEvent::HandleUpdated { .. }
        | ConnectionEvent::RoomMembersChanged(_)
        | ConnectionEvent::RoomPeerJoined { .. }
        | ConnectionEvent::RoomPeerLeft { .. }
        | ConnectionEvent::RoomChatMessage { .. }
        | ConnectionEvent::CapabilityAnnounced { .. }
        | ConnectionEvent::CapabilityInvoked { .. }
        | ConnectionEvent::CapabilityInvokePending { .. }
        | ConnectionEvent::EndpointUpdated { .. }
        | ConnectionEvent::RoomListReceived { .. }
        | ConnectionEvent::PresenceUpdated { .. }
        | ConnectionEvent::InviteAccepted { .. }
        | ConnectionEvent::FileOffered { .. }
        | ConnectionEvent::FileProgress { .. }
        | ConnectionEvent::FileComplete { .. }
        | ConnectionEvent::FileFailed { .. }
        | ConnectionEvent::SupernodeInfoReceived { .. }
        | ConnectionEvent::RelayPaymentRequired { .. }
        | ConnectionEvent::SfuAudioReceived { .. }
        | ConnectionEvent::DirectAudioReceived { .. }
        | ConnectionEvent::AvatarConfigUpdated { .. }
        | ConnectionEvent::ConnectionStats { .. } => {}
    }
}
