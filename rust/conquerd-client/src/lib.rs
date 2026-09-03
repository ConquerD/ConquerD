//! ConquerD client framework and desktop application services.
//!
//! The executable in `main.rs` is one consumer of these modules. The Qt/QML
//! frontend and headless integration mode select different parts of this
//! library through Cargo features.

pub mod aec;
pub mod avatar_config;
pub mod call_controller;
pub mod chat_store;
pub mod cluster;
pub mod connection_fallback;
pub mod connection_manager;
pub mod content_audio;
pub mod content_capture;
pub mod content_playout;
pub mod content_sender;
pub mod crypto;
pub mod error;
pub mod feature_trust;
pub mod file_transfer;
pub mod github_updater;
pub mod group_key;
pub mod identity;
pub mod logging;
pub mod media_clock;
pub mod media_sync;
pub mod ollama_module;
pub mod peer_store;
pub mod platform;
#[cfg(feature = "qt-ui")]
pub mod plugin_manager;
#[cfg(feature = "qt-ui")]
pub mod plugin_runtime;
pub mod protocol;
pub mod quic_relay_client;
pub mod quic_tls;
pub mod room_store;
pub mod session_state;
pub mod sfu_client;
pub mod space;
#[cfg(feature = "qt-ui")]
pub mod taskbar_badge;
#[cfg(feature = "qt-ui")]
pub mod ui;
pub mod upnp;
pub mod uri_scheme;
pub mod video;
pub mod web_app_client;
