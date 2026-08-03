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
        /// Accept direct peer-to-peer QUIC connections without a supernode.
        #[qproperty(bool, direct_p2p_enabled)]
        /// Preferred UDP listener port for direct peer-to-peer QUIC.
        #[qproperty(i32, direct_p2p_port)]
        #[qproperty(bool, start_minimized)]
        /// Hide the window to the system tray instead of quitting when it is
        /// closed or minimized. Only takes effect when a tray icon is available.
        #[qproperty(bool, minimize_to_tray)]
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
        #[qproperty(QString, video_input_device)]
        /// Picture-in-picture overlays drawn over `video_input_device`, as a
        /// JSON array of `{"id":..,"corner":..,"size":..}`. Empty means the
        /// single-source capture that predates PIP.
        #[qproperty(QString, video_overlays_json)]
        #[qproperty(bool, video_enabled)]
        /// Capture quality preset: `"low"` | `"balanced"` | `"high"` |
        /// `"custom"`. `"custom"` means the fields below say what the capture
        /// is; every other value means they are ignored.
        #[qproperty(QString, video_quality)]
        /// Encoded frame size as `"<width>x<height>"`, e.g. `"1280x720"`.
        /// Empty (or unparseable) means "follow the preset".
        #[qproperty(QString, video_resolution)]
        /// Encoded frame rate. `0` means "follow the preset".
        #[qproperty(i32, video_fps)]
        /// Target bitrate in kbps. `0` means "derive one from the resolution
        /// and frame rate" — the settings page's Auto.
        #[qproperty(i32, video_bitrate_kbps)]
        /// Maximum seconds between keyframes. Applies to every preset, not just
        /// custom: it trades how fast a joining receiver gets a picture against
        /// how much of the bitrate keyframes consume.
        #[qproperty(i32, video_keyframe_secs)]
        /// Preferred outgoing codec: `"auto"` | `"h264"` | `"vp8"`.
        ///
        /// `"auto"` negotiates. Anything else is honoured only when this build
        /// can encode it *and* the receiver can decode it — a preference is
        /// never a reason to send frames nobody can read, nor to send nothing.
        #[qproperty(QString, video_codec)]
        /// Let measured packet loss lower the video bitrate below the target.
        ///
        /// On by default. Turning it off pins the stream at the chosen bitrate,
        /// which is a real choice on a link the user knows is fine.
        #[qproperty(bool, video_adaptive_bitrate)]
        /// Which audio accompanies a shared video source.
        ///
        /// `"auto"` follows the video source — an app shares its own audio, a
        /// monitor shares the machine, a camera shares nothing. `"system"`
        /// always shares the whole machine; `"off"` never shares.
        #[qproperty(QString, content_audio_mode)]
        #[qproperty(QString, peer_audio_prefs_json)]
        #[qproperty(f32, video_region_ratio)]
        #[qproperty(QString, video_popout_geometry_json)]
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
        /// True after the first-run onboarding wizard has completed.
        #[qproperty(bool, onboarding_complete)]
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
        /// Verbose (debug-level) logging for troubleshooting. Applied live and
        /// seeded at next startup; an explicit RUST_LOG env var overrides it.
        #[qproperty(bool, debug_logging)]
        /// Whether the live settings differ from what is on disk.
        ///
        /// Read-only in practice: [`refresh_dirty`](SettingsModel::refresh_dirty)
        /// recomputes it, and save/load clear it. Drives the Save button, which
        /// otherwise looks identically clickable whether or not there is
        /// anything to save.
        #[qproperty(bool, dirty)]
        type SettingsModel = super::SettingsModelRust;

        /// Persist settings to disk (JSON).
        #[qinvokable]
        fn save(self: Pin<&mut Self>);

        /// Load settings from disk, updating properties.
        #[qinvokable]
        fn load(self: Pin<&mut Self>);

        /// Recompute [`dirty`](SettingsModel::dirty) by comparing the live
        /// properties with the last state written to (or read from) disk.
        ///
        /// Polled by the settings UI rather than maintained per edit. There are
        /// around a hundred places that write a setting, most of which save
        /// immediately and some of which do not; a flag each one had to set
        /// would be wrong the first time someone added the hundred-and-first.
        /// Comparing against the saved state cannot drift — the answer is
        /// derived from the same snapshot `save` writes.
        #[qinvokable]
        #[rust_name = "refresh_dirty"]
        fn refreshDirty(self: Pin<&mut Self>);

        /// The video encoder overrides as one JSON blob, for handing to
        /// `AppBridge::setVideoEnabled` / `setVideoPreviewEnabled`.
        ///
        /// One blob rather than six more arguments on two invokables and four
        /// call sites — and built here rather than in QML so there is exactly
        /// one place that knows how a resolution string becomes a width and a
        /// height. The preset itself is *not* in it: that is still the separate
        /// `quality` argument those invokables already take.
        #[qinvokable]
        #[rust_name = "video_encoder_json"]
        fn videoEncoderJson(self: Pin<&mut Self>) -> QString;

        /// What the encoder will actually be configured with, once the preset
        /// and the overrides are resolved together:
        /// `{"width":..,"height":..,"fps":..,"bitrate_bps":..,"keyframe_secs":..}`.
        ///
        /// The settings page shows these and syncs its combo boxes from them,
        /// so the preset table lives in Rust only — a copy in QML would drift
        /// from the one the encoder uses and quietly mislabel every preset.
        #[qinvokable]
        #[rust_name = "effective_video_quality_json"]
        fn effectiveVideoQualityJson(self: Pin<&mut Self>) -> QString;
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
    #[serde(default = "default_true")]
    direct_p2p_enabled: bool,
    #[serde(default = "default_direct_p2p_port")]
    direct_p2p_port: i32,
    #[serde(default)]
    start_minimized: bool,
    #[serde(default)]
    minimize_to_tray: bool,
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
    video_input_device: String,
    /// Overlay layout for picture-in-picture, as a JSON array. One blob rather
    /// than a field per overlay, which would fix the maximum in the schema.
    #[serde(default = "default_overlays_blob")]
    video_overlays_json: String,
    #[serde(default)]
    video_enabled: bool,
    #[serde(default = "default_video_quality")]
    video_quality: String,
    /// `"<width>x<height>"`, or empty to follow the preset.
    #[serde(default)]
    video_resolution: String,
    /// Zero means "follow the preset" in both of these — see the qproperties.
    #[serde(default)]
    video_fps: i32,
    #[serde(default)]
    video_bitrate_kbps: i32,
    #[serde(default = "default_video_keyframe_secs")]
    video_keyframe_secs: i32,
    #[serde(default = "default_video_codec")]
    video_codec: String,
    #[serde(default = "default_true")]
    video_adaptive_bitrate: bool,
    #[serde(default = "default_content_audio_mode")]
    content_audio_mode: String,
    /// Listener-local per-peer mute/volume, as
    /// `{"<peer>":{"muted":bool,"volume":int}}`. One blob rather than a field
    /// per peer, which would grow without bound.
    #[serde(default = "default_prefs_blob")]
    peer_audio_prefs_json: String,
    /// Fraction of the content area the shared video region occupies.
    #[serde(default = "default_video_region_ratio")]
    video_region_ratio: f32,
    /// Per-peer popout window geometry, `{"<peer>":{"x":..,"y":..,"w":..,"h":..}}`.
    /// One blob rather than four properties per peer, which would grow without
    /// bound as peers come and go.
    #[serde(default = "default_prefs_blob")]
    video_popout_geometry_json: String,
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
    onboarding_complete: bool,
    #[serde(default)]
    window_width: i32,
    #[serde(default)]
    window_height: i32,
    #[serde(default)]
    avatar_config_json: String,
    #[serde(default)]
    debug_logging: bool,
}

fn default_true() -> bool {
    true
}
fn default_direct_p2p_port() -> i32 {
    61_045
}
fn default_ollama_url() -> String {
    // Prefer 127.0.0.1 over localhost (Windows can resolve localhost → ::1 while
    // Ollama typically binds IPv4 only). fetch_model_list also rewrites localhost.
    "http://127.0.0.1:11434".to_string()
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
/// Capture quality preset: "low" (320x180), "balanced" (640x360), "high"
/// (1280x720). Balanced is the default because it is the size the encoder's
/// bitrate defaults are tuned for.
/// Default share of the content area for the video region: a little under
/// half, so chat stays usable without the video feeling cramped.
fn default_video_region_ratio() -> f32 {
    0.4
}

/// Follow the video source unless the user says otherwise — the pairing that
/// is right in almost every case.
fn default_content_audio_mode() -> String {
    "auto".to_owned()
}

fn default_video_quality() -> String {
    "balanced".to_string()
}

/// Negotiate by default: the codec a peer can actually decode is a fact about
/// their build, not something a local preference should be able to override
/// into a stream nobody can read.
fn default_video_codec() -> String {
    "auto".to_string()
}

fn default_video_keyframe_secs() -> i32 {
    crate::video::sender::DEFAULT_KEYFRAME_SECS as i32
}

/// Empty JSON object — no per-peer overrides.
fn default_prefs_blob() -> String {
    "{}".to_string()
}

/// Empty JSON array — no picture-in-picture overlays.
fn default_overlays_blob() -> String {
    "[]".to_string()
}

fn default_voice_bitrate() -> String {
    "ultra".to_string()
}
fn default_theme() -> String {
    "dark".to_string()
}
fn default_attestation_policy() -> String {
    "warn".to_string()
}

impl Default for SettingsSnapshot {
    fn default() -> Self {
        Self {
            notifications_enabled: true,
            auto_connect: false,
            direct_p2p_enabled: true,
            direct_p2p_port: default_direct_p2p_port(),
            start_minimized: false,
            minimize_to_tray: false,
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
            video_input_device: String::new(),
            video_overlays_json: default_overlays_blob(),
            video_enabled: false,
            video_quality: default_video_quality(),
            video_resolution: String::new(),
            video_fps: 0,
            video_bitrate_kbps: 0,
            video_keyframe_secs: default_video_keyframe_secs(),
            video_codec: default_video_codec(),
            video_adaptive_bitrate: true,
            content_audio_mode: default_content_audio_mode(),
            peer_audio_prefs_json: default_prefs_blob(),
            video_region_ratio: default_video_region_ratio(),
            video_popout_geometry_json: default_prefs_blob(),
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
            onboarding_complete: false,
            window_width: 0,
            window_height: 0,
            avatar_config_json: String::new(),
            debug_logging: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Rust backing struct
// ---------------------------------------------------------------------------

pub struct SettingsModelRust {
    notifications_enabled: bool,
    auto_connect: bool,
    direct_p2p_enabled: bool,
    direct_p2p_port: i32,
    start_minimized: bool,
    minimize_to_tray: bool,
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
    video_input_device: QString,
    video_overlays_json: QString,
    video_enabled: bool,
    video_quality: QString,
    /// See the matching qproperties for what each of these means and what a
    /// zero or empty value falls back to.
    video_resolution: QString,
    video_fps: i32,
    video_bitrate_kbps: i32,
    video_keyframe_secs: i32,
    video_codec: QString,
    video_adaptive_bitrate: bool,
    /// See the `content_audio_mode` qproperty.
    content_audio_mode: QString,
    peer_audio_prefs_json: QString,
    video_region_ratio: f32,
    video_popout_geometry_json: QString,
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
    onboarding_complete: bool,
    noise_strength: QString,
    theme: QString,
    relay_allow_gated: bool,
    relay_auto_renew: bool,
    upnp_enabled: bool,
    attestation_policy: QString,
    window_width: i32,
    window_height: i32,
    avatar_config_json: QString,
    debug_logging: bool,
    dirty: bool,
    /// The last state written to (or read from) disk, serialized compactly.
    ///
    /// Held as text rather than a `SettingsSnapshot` so the comparison is one
    /// string equality over every field at once — a field added to the snapshot
    /// is covered without touching this, which a hand-written `PartialEq` would
    /// not be.
    saved_json: String,
}

impl Default for SettingsModelRust {
    fn default() -> Self {
        let s = SettingsSnapshot::default();
        Self {
            notifications_enabled: s.notifications_enabled,
            auto_connect: s.auto_connect,
            direct_p2p_enabled: s.direct_p2p_enabled,
            direct_p2p_port: s.direct_p2p_port,
            start_minimized: s.start_minimized,
            minimize_to_tray: s.minimize_to_tray,
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
            video_input_device: QString::default(),
            video_overlays_json: QString::from(s.video_overlays_json.as_str()),
            video_enabled: s.video_enabled,
            video_quality: QString::from(s.video_quality.as_str()),
            video_resolution: QString::from(s.video_resolution.as_str()),
            video_fps: s.video_fps,
            video_bitrate_kbps: s.video_bitrate_kbps,
            video_keyframe_secs: s.video_keyframe_secs,
            video_codec: QString::from(s.video_codec.as_str()),
            video_adaptive_bitrate: s.video_adaptive_bitrate,
            content_audio_mode: QString::from(s.content_audio_mode.as_str()),
            peer_audio_prefs_json: QString::from(s.peer_audio_prefs_json.as_str()),
            video_region_ratio: s.video_region_ratio,
            video_popout_geometry_json: QString::from(s.video_popout_geometry_json.as_str()),
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
            onboarding_complete: s.onboarding_complete,
            noise_strength: QString::from(s.noise_strength.as_str()),
            theme: QString::from(s.theme.as_str()),
            relay_allow_gated: s.relay_allow_gated,
            relay_auto_renew: s.relay_auto_renew,
            upnp_enabled: s.upnp_enabled,
            attestation_policy: QString::from(s.attestation_policy.as_str()),
            window_width: s.window_width,
            window_height: s.window_height,
            avatar_config_json: QString::default(),
            debug_logging: s.debug_logging,
            dirty: false,
            // Deliberately not the serialized defaults: nothing has been read
            // from disk yet, so the first `load` is what establishes the
            // baseline. Leaving it empty means a model that is never loaded
            // reports dirty, which is the truthful answer — those values have
            // never been persisted.
            saved_json: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// File path helper
// ---------------------------------------------------------------------------

pub fn settings_file() -> PathBuf {
    crate::identity::Identity::default_key_dir().join("settings.json")
}

// ---------------------------------------------------------------------------
// Video encoder settings
// ---------------------------------------------------------------------------

// The ranges the Video settings page offers, as `i32` so the properties can be
// clamped without casting at every use. Derived from the encoder's own limits
// rather than restated, so widening one of those cannot leave the UI refusing a
// value the encoder would have accepted.
const MIN_FPS: i32 = crate::video::sender::MIN_FPS as i32;
const MAX_FPS: i32 = crate::video::sender::MAX_FPS as i32;
const MIN_KEYFRAME_SECS: i32 = crate::video::sender::MIN_KEYFRAME_SECS as i32;
const MAX_KEYFRAME_SECS: i32 = crate::video::sender::MAX_KEYFRAME_SECS as i32;
const MIN_BITRATE_KBPS: i32 = (crate::video::sender::MIN_VIDEO_BITRATE_BPS / 1000) as i32;
const MAX_BITRATE_KBPS: i32 = (crate::video::sender::MAX_VIDEO_BITRATE_BPS / 1000) as i32;

/// Clamp `value` into `[lo, hi]`, but leave zero alone.
///
/// Zero is the "follow the preset" sentinel on several of these settings, and
/// clamping it up to `lo` would silently turn "no opinion" into a hard override
/// at the lowest offered value.
fn clamp_or_zero(value: i32, lo: i32, hi: i32) -> i32 {
    if value <= 0 {
        0
    } else {
        value.clamp(lo, hi)
    }
}

/// Render the encoder overrides as the blob `AppBridge::setVideoEnabled` reads.
///
/// A free function so both ends of that QML round trip can be tested together —
/// the field names here and the ones
/// [`VideoEncoderSettings`](super::bridge) deserializes are a contract that
/// would otherwise only break at runtime, in the form of settings that silently
/// stop applying.
pub fn video_encoder_blob(
    overrides: crate::video::sender::QualityOverrides,
    codec: &str,
    adaptive: bool,
) -> String {
    serde_json::json!({
        "width": overrides.width,
        "height": overrides.height,
        "fps": overrides.fps,
        "bitrate_bps": overrides.bitrate_bps,
        "keyframe_secs": overrides.keyframe_interval_secs,
        "codec": codec,
        "adaptive": adaptive,
    })
    .to_string()
}

/// Split a `"<width>x<height>"` setting into even, in-range dimensions.
///
/// Returns `(0, 0)` for anything unusable — empty, malformed, or absurd — which
/// [`QualityOverrides`](crate::video::sender::QualityOverrides) reads as "follow
/// the preset". Total on purpose: this string is hand-editable.
fn parse_resolution(raw: &str) -> (u32, u32) {
    let Some((w, h)) = raw.trim().split_once(['x', 'X']) else {
        return (0, 0);
    };
    let (Ok(w), Ok(h)) = (w.trim().parse::<u32>(), h.trim().parse::<u32>()) else {
        return (0, 0);
    };
    let ok = |v: u32| {
        (crate::video::sender::MIN_DIMENSION..=crate::video::sender::MAX_DIMENSION).contains(&v)
    };
    if ok(w) && ok(h) {
        (w, h)
    } else {
        (0, 0)
    }
}

// ---------------------------------------------------------------------------
// Invokables
// ---------------------------------------------------------------------------

impl ffi::SettingsModel {
    /// The live properties as the struct that gets serialized.
    ///
    /// Shared by `save` and the dirty check so the two can never disagree about
    /// what "the current settings" are — a second, separately-maintained
    /// copy of this mapping is exactly how a Save button ends up claiming
    /// there is nothing to save.
    fn snapshot(&self) -> SettingsSnapshot {
        let r = self.rust();
        SettingsSnapshot {
            notifications_enabled: r.notifications_enabled,
            auto_connect: r.auto_connect,
            direct_p2p_enabled: r.direct_p2p_enabled,
            direct_p2p_port: r.direct_p2p_port,
            start_minimized: r.start_minimized,
            minimize_to_tray: r.minimize_to_tray,
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
            video_input_device: r.video_input_device.to_string(),
            video_overlays_json: r.video_overlays_json.to_string(),
            video_enabled: r.video_enabled,
            video_quality: r.video_quality.to_string(),
            video_resolution: r.video_resolution.to_string(),
            video_fps: r.video_fps,
            video_bitrate_kbps: r.video_bitrate_kbps,
            video_keyframe_secs: r.video_keyframe_secs,
            video_codec: r.video_codec.to_string(),
            video_adaptive_bitrate: r.video_adaptive_bitrate,
            content_audio_mode: r.content_audio_mode.to_string(),
            peer_audio_prefs_json: r.peer_audio_prefs_json.to_string(),
            video_region_ratio: r.video_region_ratio,
            video_popout_geometry_json: r.video_popout_geometry_json.to_string(),
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
            onboarding_complete: r.onboarding_complete,
            noise_strength: r.noise_strength.to_string(),
            theme: r.theme.to_string(),
            relay_allow_gated: r.relay_allow_gated,
            relay_auto_renew: r.relay_auto_renew,
            upnp_enabled: r.upnp_enabled,
            attestation_policy: r.attestation_policy.to_string(),
            window_width: r.window_width,
            window_height: r.window_height,
            avatar_config_json: r.avatar_config_json.to_string(),
            debug_logging: r.debug_logging,
        }
    }

    fn save(mut self: Pin<&mut Self>) {
        let snap = self.snapshot();
        // Apply the log-verbosity choice live so it takes effect without a restart.
        crate::logging::set_debug_logging(snap.debug_logging);
        let path = settings_file();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(&snap) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!("SettingsModel::save failed: {e}");
                    // Deliberately not marking clean: the file still holds the
                    // old settings, so there is genuinely something to save and
                    // the button must keep saying so.
                    return;
                }
                debug!("Settings saved to {}", path.display());
                self.as_mut().mark_saved(&snap);
            }
            Err(e) => warn!("SettingsModel::save serialize error: {e}"),
        }
    }

    /// Record `snap` as the state on disk and clear [`dirty`].
    fn mark_saved(mut self: Pin<&mut Self>, snap: &SettingsSnapshot) {
        // Compact, not pretty: this is only ever compared with itself, and the
        // comparison must not depend on how the file happens to be formatted.
        if let Ok(compact) = serde_json::to_string(snap) {
            self.as_mut().rust_mut().saved_json = compact;
        }
        self.as_mut().set_dirty(false);
    }

    /// The video overrides as [`QualityOverrides`], resolved from the raw
    /// property values.
    ///
    /// Shared by both invokables below so what the settings page *shows* and
    /// what the encoder is *given* can never disagree — the whole reason the
    /// preset table stayed in Rust.
    fn video_overrides(&self) -> crate::video::sender::QualityOverrides {
        let r = self.rust();
        let (width, height) = parse_resolution(&r.video_resolution.to_string());
        crate::video::sender::QualityOverrides {
            width,
            height,
            fps: clamp_or_zero(r.video_fps, MIN_FPS, MAX_FPS).max(0) as u32,
            // Stored in kbps because that is the unit the UI speaks; the
            // encoder wants bits per second.
            bitrate_bps: clamp_or_zero(r.video_bitrate_kbps, MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
                .max(0) as u32
                * 1000,
            keyframe_interval_secs: r.video_keyframe_secs.clamp(0, MAX_KEYFRAME_SECS).max(0) as u32,
        }
    }

    /// The resolved encoder settings — preset plus overrides.
    fn effective_video_quality(&self) -> crate::video::sender::Quality {
        crate::video::sender::Quality::resolve(
            &self.rust().video_quality.to_string(),
            self.video_overrides(),
        )
    }

    fn video_encoder_json(self: Pin<&mut Self>) -> QString {
        let overrides = self.video_overrides();
        let r = self.rust();
        QString::from(
            video_encoder_blob(
                overrides,
                &r.video_codec.to_string(),
                r.video_adaptive_bitrate,
            )
            .as_str(),
        )
    }

    fn effective_video_quality_json(self: Pin<&mut Self>) -> QString {
        let q = self.effective_video_quality();
        let json = serde_json::json!({
            "width": q.width,
            "height": q.height,
            "fps": q.fps,
            "bitrate_bps": q.bitrate_bps,
            "keyframe_secs": q.keyframe_interval_secs,
        });
        QString::from(json.to_string().as_str())
    }

    fn refresh_dirty(mut self: Pin<&mut Self>) {
        let live = serde_json::to_string(&self.snapshot()).unwrap_or_default();
        let changed = live != self.rust().saved_json;
        // Only write through a change: `set_dirty` emits, and this is polled.
        if changed != self.rust().dirty {
            self.as_mut().set_dirty(changed);
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
        self.as_mut()
            .set_direct_p2p_enabled(snap.direct_p2p_enabled);
        self.as_mut()
            .set_direct_p2p_port(snap.direct_p2p_port.clamp(1, u16::MAX as i32));
        self.as_mut().set_start_minimized(snap.start_minimized);
        self.as_mut().set_minimize_to_tray(snap.minimize_to_tray);
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
            .set_video_input_device(QString::from(snap.video_input_device.as_str()));
        self.as_mut()
            .set_video_overlays_json(QString::from(snap.video_overlays_json.as_str()));
        self.as_mut().set_video_enabled(snap.video_enabled);
        self.as_mut()
            .set_video_quality(QString::from(snap.video_quality.as_str()));
        self.as_mut()
            .set_video_resolution(QString::from(snap.video_resolution.as_str()));
        // Clamped on the way in for the same reason the port above is: these
        // reach a hardware encoder, and the file is hand-editable. Zero is kept
        // as-is in both — it is the "follow the preset" sentinel, not a value.
        self.as_mut()
            .set_video_fps(clamp_or_zero(snap.video_fps, MIN_FPS, MAX_FPS));
        self.as_mut().set_video_bitrate_kbps(clamp_or_zero(
            snap.video_bitrate_kbps,
            MIN_BITRATE_KBPS,
            MAX_BITRATE_KBPS,
        ));
        // Unlike the two above, zero is not a sentinel here — a keyframe
        // interval always has a value — so an absent or nonsense one becomes
        // the default rather than the shortest (and most expensive) interval.
        self.as_mut()
            .set_video_keyframe_secs(if snap.video_keyframe_secs <= 0 {
                default_video_keyframe_secs()
            } else {
                snap.video_keyframe_secs
                    .clamp(MIN_KEYFRAME_SECS, MAX_KEYFRAME_SECS)
            });
        self.as_mut()
            .set_video_codec(QString::from(snap.video_codec.as_str()));
        self.as_mut()
            .set_video_adaptive_bitrate(snap.video_adaptive_bitrate);
        // Restored like every other video setting. Omitting it left the choice
        // saved to disk but silently reset to "auto" on every launch.
        self.as_mut()
            .set_content_audio_mode(QString::from(snap.content_audio_mode.as_str()));
        self.as_mut()
            .set_peer_audio_prefs_json(QString::from(snap.peer_audio_prefs_json.as_str()));
        self.as_mut()
            .set_video_region_ratio(snap.video_region_ratio);
        self.as_mut().set_video_popout_geometry_json(QString::from(
            snap.video_popout_geometry_json.as_str(),
        ));
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
        self.as_mut()
            .set_onboarding_complete(snap.onboarding_complete);
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
        self.as_mut().set_debug_logging(snap.debug_logging);
        crate::logging::set_debug_logging(snap.debug_logging);
        // The baseline is taken from the *applied* properties, not from `snap`:
        // a few are clamped or derived on the way in (the port, and noise
        // suppression from its strength), so recording `snap` would leave the
        // model looking unsaved the moment it finished loading.
        let applied = self.snapshot();
        self.as_mut().mark_saved(&applied);
        debug!("Settings loaded from {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resolution_setting_parses_into_dimensions() {
        assert_eq!(parse_resolution("1280x720"), (1280, 720));
        assert_eq!(parse_resolution(" 640 x 360 "), (640, 360));
        assert_eq!(parse_resolution("1920X1080"), (1920, 1080));
    }

    /// This string is hand-editable and reaches a hardware encoder, so every
    /// unusable spelling has to fall back to the preset rather than through.
    #[test]
    fn an_unusable_resolution_setting_falls_back_to_the_preset() {
        for raw in [
            "",
            "  ",
            "720p",
            "1280",
            "1280x",
            "x720",
            "1280*720",
            "-640x-360",
            "0x0",
            "99999x99999",
            "64x64",
            "1280x99999",
        ] {
            assert_eq!(
                parse_resolution(raw),
                (0, 0),
                "{raw:?} must not be honoured"
            );
        }
    }

    /// Zero means "follow the preset" on these settings, so it must survive the
    /// clamp — raising it to the low bound would turn "no opinion" into a hard
    /// override at the lowest offered value.
    #[test]
    fn clamping_leaves_the_follow_the_preset_sentinel_alone() {
        assert_eq!(clamp_or_zero(0, 5, 60), 0);
        assert_eq!(clamp_or_zero(-7, 5, 60), 0);
        assert_eq!(clamp_or_zero(1, 5, 60), 5);
        assert_eq!(clamp_or_zero(30, 5, 60), 30);
        assert_eq!(clamp_or_zero(999, 5, 60), 60);
    }

    /// The saved defaults must round-trip through serde unchanged, or a fresh
    /// install would read back as something the defaults never described.
    #[test]
    fn video_defaults_round_trip_through_serde() {
        let snap = SettingsSnapshot::default();
        let json = serde_json::to_string(&snap).unwrap();
        let back: SettingsSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.video_quality, "balanced");
        assert_eq!(back.video_codec, "auto");
        assert_eq!(back.video_resolution, "");
        assert_eq!(back.video_fps, 0);
        assert_eq!(back.video_bitrate_kbps, 0);
        assert_eq!(
            back.video_keyframe_secs,
            crate::video::sender::DEFAULT_KEYFRAME_SECS as i32
        );
        assert!(back.video_adaptive_bitrate);
    }

    /// A settings file written before these fields existed must still load, and
    /// must land on the same behaviour that file already had.
    #[test]
    fn a_settings_file_without_the_video_fields_still_loads() {
        let snap: SettingsSnapshot =
            serde_json::from_str(r#"{"video_quality":"high"}"#).expect("older files must load");
        assert_eq!(snap.video_quality, "high");
        assert_eq!(
            snap.video_codec, "auto",
            "an older file must keep negotiating"
        );
        assert!(
            snap.video_adaptive_bitrate,
            "adaptation was always on before"
        );
        assert_eq!(
            crate::video::sender::Quality::resolve(
                &snap.video_quality,
                crate::video::sender::QualityOverrides {
                    keyframe_interval_secs: snap.video_keyframe_secs as u32,
                    ..Default::default()
                }
            ),
            crate::video::sender::Quality::from_name("high"),
            "an older file must encode exactly as it did before"
        );
    }
}
