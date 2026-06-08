//! SettingsModel — writable app-settings QObject singleton.
//!
//! Compiled only when the `qt-ui` Cargo feature is enabled.
//!
//! Settings are persisted to `~/.conquerd/settings.json` (or
//! `$CONQUERD_HOME/settings.json` when the env var is set).

use std::path::PathBuf;
use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(bool, notifications_enabled)]
        #[qproperty(bool, auto_connect)]
        #[qproperty(bool, start_minimized)]
        #[qproperty(bool, push_to_talk)]
        #[qproperty(bool, noise_suppression)]
        #[qproperty(bool, voice_activation)]
        #[qproperty(i32, jitter_buffer_depth)]
        #[qproperty(i32, input_volume)]
        #[qproperty(i32, output_volume)]
        #[qproperty(QString, voice_bitrate)]
        #[qproperty(QString, ptt_key)]
        #[qproperty(QString, audio_input_device)]
        #[qproperty(QString, audio_output_device)]
        #[qproperty(QString, local_handle)]
        #[qproperty(bool, update_check_enabled)]
        #[qproperty(i32, relay_port)]
        #[qproperty(bool, ollama_enabled)]
        #[qproperty(QString, ollama_base_url)]
        #[qproperty(QString, ollama_model)]
        #[qproperty(QString, ollama_system_prompt)]
        #[qproperty(bool, ollama_auto_respond_direct)]
        #[qproperty(bool, ollama_auto_respond_room)]
        /// Show a click-to-open preview card for YouTube links in chat.
        /// Privacy-safe: no thumbnail requests are made.
        #[qproperty(bool, youtube_preview_enabled)]
        /// True once the user has acknowledged the YouTube inline-embed disclosure.
        #[qproperty(bool, youtube_inline_ack)]
        /// Noise suppression strength: "off" | "mild" | "moderate" | "aggressive" | "max".
        #[qproperty(QString, noise_strength)]
        /// UI theme: "system" | "dark" | "light".
        #[qproperty(QString, theme)]
        /// Allow relay-gated (non-direct) connections.
        #[qproperty(bool, relay_allow_gated)]
        /// Auto-renew relay tickets before they expire.
        #[qproperty(bool, relay_auto_renew)]
        /// Enable UPnP automatic port mapping.
        #[qproperty(bool, upnp_enabled)]
        /// Build-attestation verification policy: "off" | "warn" | "strict".
        #[qproperty(QString, attestation_policy)]
        /// Last-seen window width (0 = use default).
        #[qproperty(i32, window_width)]
        /// Last-seen window height (0 = use default).
        #[qproperty(i32, window_height)]
        /// Own avatar configuration serialised as a JSON object.
        /// Empty string means "use defaults".
        #[qproperty(QString, avatar_config_json)]
        type SettingsModel = super::SettingsModelRust;

        /// Persist settings to disk (JSON).
        #[qinvokable]
        fn save(self: Pin<&mut Self>);

        /// Load settings from disk, updating properties.
        #[qinvokable]
        fn load(self: Pin<&mut Self>);
    }
}

pub use ffi::SettingsModel;

// ---------------------------------------------------------------------------
// Serializable settings snapshot (serde mirror of the QObject properties)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SettingsSnapshot {
    #[serde(default = "default_true")]
    notifications_enabled: bool,
    #[serde(default)]
    auto_connect: bool,
    #[serde(default)]
    start_minimized: bool,
    #[serde(default)]
    push_to_talk: bool,
    #[serde(default = "default_true")]
    noise_suppression: bool,
    #[serde(default)]
    voice_activation: bool,
    #[serde(default = "default_jitter")]
    jitter_buffer_depth: i32,
    #[serde(default = "default_100")]
    input_volume: i32,
    #[serde(default = "default_100")]
    output_volume: i32,
    #[serde(default = "default_voice_bitrate")]
    voice_bitrate: String,
    #[serde(default = "default_ptt_key")]
    ptt_key: String,
    #[serde(default)]
    audio_input_device: String,
    #[serde(default)]
    audio_output_device: String,
    #[serde(default)]
    local_handle: String,
    #[serde(default = "default_true")]
    update_check_enabled: bool,
    #[serde(default)]
    relay_port: i32,
    #[serde(default)]
    ollama_enabled: bool,
    #[serde(default = "default_ollama_url")]
    ollama_base_url: String,
    #[serde(default = "default_ollama_model")]
    ollama_model: String,
    #[serde(default = "default_ollama_system_prompt")]
    ollama_system_prompt: String,
    #[serde(default)]
    ollama_auto_respond_direct: bool,
    #[serde(default)]
    ollama_auto_respond_room: bool,
    #[serde(default = "default_noise_strength")]
    noise_strength: String,
    #[serde(default = "default_theme")]
    theme: String,
    #[serde(default = "default_true")]
    relay_allow_gated: bool,
    #[serde(default = "default_true")]
    relay_auto_renew: bool,
    #[serde(default = "default_true")]
    upnp_enabled: bool,
    #[serde(default = "default_attestation_policy")]
    attestation_policy: String,
    #[serde(default = "default_true")]
    youtube_preview_enabled: bool,
    #[serde(default)]
    youtube_inline_ack: bool,
    #[serde(default)]
    window_width: i32,
    #[serde(default)]
    window_height: i32,
    #[serde(default)]
    avatar_config_json: String,
}

fn default_true() -> bool {
    true
}
fn default_ollama_url() -> String {
    "http://localhost:11434".to_string()
}
fn default_ollama_model() -> String {
    "llama3".to_string()
}
fn default_ollama_system_prompt() -> String {
    "You are a helpful assistant.".to_string()
}
fn default_jitter() -> i32 {
    3
}
fn default_100() -> i32 {
    100
}
fn default_ptt_key() -> String {
    "space".to_string()
}
fn default_noise_strength() -> String {
    "moderate".to_string()
}
fn default_voice_bitrate() -> String {
    "ultra".to_string()
}
fn default_theme() -> String {
    "system".to_string()
}
fn default_attestation_policy() -> String {
    "warn".to_string()
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            notifications_enabled: true,
            auto_connect: false,
            start_minimized: false,
            push_to_talk: false,
            noise_suppression: true,
            voice_activation: false,
            jitter_buffer_depth: 3,
            input_volume: 100,
            output_volume: 100,
            voice_bitrate: default_voice_bitrate(),
            ptt_key: "space".to_string(),
            audio_input_device: String::new(),
            audio_output_device: String::new(),
            local_handle: String::new(),
            update_check_enabled: true,
            relay_port: 0,
            ollama_enabled: false,
            ollama_base_url: default_ollama_url(),
            ollama_model: default_ollama_model(),
            ollama_system_prompt: default_ollama_system_prompt(),
            ollama_auto_respond_direct: false,
            ollama_auto_respond_room: false,
            noise_strength: default_noise_strength(),
            theme: default_theme(),
            relay_allow_gated: true,
            relay_auto_renew: true,
            upnp_enabled: true,
            attestation_policy: default_attestation_policy(),
            youtube_preview_enabled: true,
            youtube_inline_ack: false,
            window_width: 0,
            window_height: 0,
            avatar_config_json: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Rust backing struct
// ---------------------------------------------------------------------------

pub struct SettingsModelRust {
    notifications_enabled: bool,
    auto_connect: bool,
    start_minimized: bool,
    push_to_talk: bool,
    noise_suppression: bool,
    voice_activation: bool,
    jitter_buffer_depth: i32,
    input_volume: i32,
    output_volume: i32,
    voice_bitrate: QString,
    ptt_key: QString,
    audio_input_device: QString,
    audio_output_device: QString,
    local_handle: QString,
    update_check_enabled: bool,
    relay_port: i32,
    ollama_enabled: bool,
    ollama_base_url: QString,
    ollama_model: QString,
    ollama_system_prompt: QString,
    ollama_auto_respond_direct: bool,
    ollama_auto_respond_room: bool,
    youtube_preview_enabled: bool,
    youtube_inline_ack: bool,
    noise_strength: QString,
    theme: QString,
    relay_allow_gated: bool,
    relay_auto_renew: bool,
    upnp_enabled: bool,
    attestation_policy: QString,
    window_width: i32,
    window_height: i32,
    avatar_config_json: QString,
}

impl Default for SettingsModelRust {
    fn default() -> Self {
        let s = SettingsSnapshot::default();
        Self {
            notifications_enabled: s.notifications_enabled,
            auto_connect: s.auto_connect,
            start_minimized: s.start_minimized,
            push_to_talk: s.push_to_talk,
            noise_suppression: s.noise_suppression,
            voice_activation: s.voice_activation,
            jitter_buffer_depth: s.jitter_buffer_depth,
            input_volume: s.input_volume,
            output_volume: s.output_volume,
            voice_bitrate: QString::from(s.voice_bitrate.as_str()),
            ptt_key: QString::from(s.ptt_key.as_str()),
            audio_input_device: QString::default(),
            audio_output_device: QString::default(),
            local_handle: QString::default(),
            update_check_enabled: s.update_check_enabled,
            relay_port: s.relay_port,
            ollama_enabled: s.ollama_enabled,
            ollama_base_url: QString::from(s.ollama_base_url.as_str()),
            ollama_model: QString::from(s.ollama_model.as_str()),
            ollama_system_prompt: QString::from(s.ollama_system_prompt.as_str()),
            ollama_auto_respond_direct: s.ollama_auto_respond_direct,
            ollama_auto_respond_room: s.ollama_auto_respond_room,
            youtube_preview_enabled: s.youtube_preview_enabled,
            youtube_inline_ack: s.youtube_inline_ack,
            noise_strength: QString::from(s.noise_strength.as_str()),
            theme: QString::from(s.theme.as_str()),
            relay_allow_gated: s.relay_allow_gated,
            relay_auto_renew: s.relay_auto_renew,
            upnp_enabled: s.upnp_enabled,
            attestation_policy: QString::from(s.attestation_policy.as_str()),
            window_width: s.window_width,
            window_height: s.window_height,
            avatar_config_json: QString::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// File path helper
// ---------------------------------------------------------------------------

pub fn settings_file() -> PathBuf {
    let base = std::env::var("CONQUERD_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".conquerd")
        });
    base.join("settings.json")
}

// ---------------------------------------------------------------------------
// Invokables
// ---------------------------------------------------------------------------

impl ffi::SettingsModel {
    fn save(self: Pin<&mut Self>) {
        let r = self.rust();
        let snap = SettingsSnapshot {
            notifications_enabled: r.notifications_enabled,
            auto_connect: r.auto_connect,
            start_minimized: r.start_minimized,
            push_to_talk: r.push_to_talk,
            noise_suppression: r.noise_suppression,
            voice_activation: r.voice_activation,
            jitter_buffer_depth: r.jitter_buffer_depth,
            input_volume: r.input_volume,
            output_volume: r.output_volume,
            voice_bitrate: r.voice_bitrate.to_string(),
            ptt_key: r.ptt_key.to_string(),
            audio_input_device: r.audio_input_device.to_string(),
            audio_output_device: r.audio_output_device.to_string(),
            local_handle: r.local_handle.to_string(),
            update_check_enabled: r.update_check_enabled,
            relay_port: r.relay_port,
            ollama_enabled: r.ollama_enabled,
            ollama_base_url: r.ollama_base_url.to_string(),
            ollama_model: r.ollama_model.to_string(),
            ollama_system_prompt: r.ollama_system_prompt.to_string(),
            ollama_auto_respond_direct: r.ollama_auto_respond_direct,
            ollama_auto_respond_room: r.ollama_auto_respond_room,
            youtube_preview_enabled: r.youtube_preview_enabled,
            youtube_inline_ack: r.youtube_inline_ack,
            noise_strength: r.noise_strength.to_string(),
            theme: r.theme.to_string(),
            relay_allow_gated: r.relay_allow_gated,
            relay_auto_renew: r.relay_auto_renew,
            upnp_enabled: r.upnp_enabled,
            attestation_policy: r.attestation_policy.to_string(),
            window_width: r.window_width,
            window_height: r.window_height,
            avatar_config_json: r.avatar_config_json.to_string(),
        };
        let path = settings_file();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&snap) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!("SettingsModel::save failed: {e}");
                } else {
                    debug!("Settings saved to {}", path.display());
                }
            }
            Err(e) => warn!("SettingsModel::save serialize error: {e}"),
        }
    }

    fn load(mut self: Pin<&mut Self>) {
        let path = settings_file();
        let snap: SettingsSnapshot = match std::fs::read_to_string(&path) {
            Ok(txt) => serde_json::from_str(&txt).unwrap_or_else(|e| {
                warn!("SettingsModel::load parse error: {e} — using defaults");
                SettingsSnapshot::default()
            }),
            Err(_) => {
                debug!("No settings file at {} — using defaults", path.display());
                SettingsSnapshot::default()
            }
        };

        self.as_mut()
            .set_notifications_enabled(snap.notifications_enabled);
        self.as_mut().set_auto_connect(snap.auto_connect);
        self.as_mut().set_start_minimized(snap.start_minimized);
        self.as_mut().set_push_to_talk(snap.push_to_talk);
        // Derive noise_suppression from noise_strength.
        let ns_bool = snap.noise_strength != "off";
        self.as_mut()
            .set_noise_strength(QString::from(snap.noise_strength.as_str()));
        self.as_mut().set_noise_suppression(ns_bool);
        self.as_mut().set_voice_activation(snap.voice_activation);
        self.as_mut()
            .set_jitter_buffer_depth(snap.jitter_buffer_depth);
        self.as_mut().set_input_volume(snap.input_volume);
        self.as_mut().set_output_volume(snap.output_volume);
        self.as_mut()
            .set_voice_bitrate(QString::from(snap.voice_bitrate.as_str()));
        self.as_mut()
            .set_ptt_key(QString::from(snap.ptt_key.as_str()));
        self.as_mut()
            .set_audio_input_device(QString::from(snap.audio_input_device.as_str()));
        self.as_mut()
            .set_audio_output_device(QString::from(snap.audio_output_device.as_str()));
        self.as_mut()
            .set_local_handle(QString::from(snap.local_handle.as_str()));
        self.as_mut()
            .set_update_check_enabled(snap.update_check_enabled);
        self.as_mut().set_relay_port(snap.relay_port);
        self.as_mut().set_ollama_enabled(snap.ollama_enabled);
        self.as_mut()
            .set_ollama_base_url(QString::from(snap.ollama_base_url.as_str()));
        self.as_mut()
            .set_ollama_model(QString::from(snap.ollama_model.as_str()));
        self.as_mut()
            .set_ollama_system_prompt(QString::from(snap.ollama_system_prompt.as_str()));
        self.as_mut()
            .set_ollama_auto_respond_direct(snap.ollama_auto_respond_direct);
        self.as_mut()
            .set_ollama_auto_respond_room(snap.ollama_auto_respond_room);
        self.as_mut()
            .set_youtube_preview_enabled(snap.youtube_preview_enabled);
        self.as_mut()
            .set_youtube_inline_ack(snap.youtube_inline_ack);
        self.as_mut().set_window_width(snap.window_width);
        self.as_mut().set_window_height(snap.window_height);
        self.as_mut().set_theme(QString::from(snap.theme.as_str()));
        self.as_mut().set_relay_allow_gated(snap.relay_allow_gated);
        self.as_mut().set_relay_auto_renew(snap.relay_auto_renew);
        self.as_mut().set_upnp_enabled(snap.upnp_enabled);
        self.as_mut()
            .set_attestation_policy(QString::from(snap.attestation_policy.as_str()));
        self.as_mut()
            .set_avatar_config_json(QString::from(snap.avatar_config_json.as_str()));
        debug!("Settings loaded from {}", path.display());
    }
}
