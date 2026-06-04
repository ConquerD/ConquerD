//! Typed `supernode.toml` feature manifest.
//!
//! Replaces ad-hoc env-var feature toggles with a single declarative
//! manifest that lists every capability the supernode wants to host. The
//! manifest is the source of truth for what gets advertised in the
//! supernode's `CAPABILITY_ANNOUNCE` and (in a follow-up) what gets
//! exposed over WebTransport.
//!
//! On-disk shape (`<data_dir>/supernode.toml`):
//!
//! ```toml
//! schema_version = 1
//!
//! [[feature]]
//! id = "core.chat.v1"
//! enabled = true
//!
//! [[feature]]
//! id = "core.audio.opus"
//! enabled = true
//! params = { codec = "opus", quota_bytes_per_sec = 32768 }
//!
//! [[feature]]
//! id = "x.acme.matchmaker"
//! enabled = false
//! version = "1.0"
//! kind = "request"
//! ```
//!
//! When the file is missing the manifest is *derived* from the legacy
//! env-var toggles in [`crate::config::Config`] for back-compat. New
//! installs should commit a `supernode.toml` and stop relying on env vars.

use std::path::Path;

use conquerd_features::descriptor::{AuthTier, CapabilityDescriptor, ChannelKind};
use conquerd_features::wellknown;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;

/// Currently-supported manifest schema version. Bump on a breaking change.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level manifest document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SupernodeManifest {
    /// Schema version. Loaders refuse anything they don't understand.
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,

    /// Declared features. Order is preserved on round-trip.
    #[serde(default, rename = "feature")]
    pub features: Vec<FeatureEntry>,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// One entry in the `[[feature]]` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureEntry {
    /// Reverse-DNS capability id, e.g. `core.chat.v1`.
    pub id: String,
    /// Whether to host this feature. Disabled entries are kept so
    /// operators can flip them back on without re-typing the descriptor.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional explicit version override. Falls back to the well-known
    /// default for first-party ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Optional explicit channel kind override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ChannelKind>,
    /// Optional auth tier override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthTier>,
    /// Free-form per-feature params (codec, bitrate, quota_*, etc.).
    /// Merged on top of the well-known defaults so operators can tune
    /// individual fields without redeclaring the whole map.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// Marks the feature as experimental.
    #[serde(default, skip_serializing_if = "is_false")]
    pub experimental: bool,
    /// Optional path to a signed `*.module.toml` manifest for a native
    /// cdylib that implements this feature. When set the supernode attempts
    /// to load the library at startup via
    /// [`conquerd_features::NativeModuleLoader`]. The signer key must appear
    /// in `<data_dir>/trusted_module_keys.txt` or the feature is skipped
    /// with a warning.
    ///
    /// Example:
    /// ```toml
    /// [[feature]]
    /// id = "x.acme.matchmaker"
    /// cdylib_manifest = "/opt/supernodes/acme.module.toml"
    /// ```
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cdylib_manifest: Option<std::path::PathBuf>,
}

fn default_true() -> bool {
    true
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Errors raised while loading or validating a manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("manifest io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported manifest schema version {found}, this build supports {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("duplicate feature id: {0}")]
    DuplicateId(String),
}

impl SupernodeManifest {
    /// Parse a manifest from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, ManifestError> {
        let m: SupernodeManifest = toml::from_str(s)?;
        m.validate()?;
        Ok(m)
    }

    /// Load `<data_dir>/supernode.toml` if it exists, else derive a
    /// manifest from the env-var-driven [`Config`] for back-compat.
    pub fn load_or_derive(data_dir: &Path, config: &Config) -> Result<Self, ManifestError> {
        let path = data_dir.join("supernode.toml");
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            return Self::from_toml_str(&raw);
        }
        Ok(Self::from_legacy_config(config))
    }

    /// Build a manifest reflecting today's env-var toggles. Used when no
    /// `supernode.toml` is present so operators can upgrade in place.
    pub fn from_legacy_config(config: &Config) -> Self {
        let mut features = vec![
            // Transport capabilities are always hosted — they describe
            // the wire format, not optional functionality.
            FeatureEntry::just("transport.quic.relay.v1"),
            FeatureEntry::just("transport.quic.stream.v1"),
            FeatureEntry::just("transport.quic.feature_datagram.v1"),
        ];
        if config.chat_enabled {
            features.push(FeatureEntry::just("core.chat.v1"));
            features.push(FeatureEntry::just("room.chat.v1"));
        }
        if config.files_enabled {
            features.push(FeatureEntry::just("core.file.v1"));
            features.push(FeatureEntry::just("room.file.v1"));
        }
        if config.sfu_enabled {
            features.push(FeatureEntry::just("room.audio.sfu"));
        }
        if config.web_port.is_some() {
            // Browser-game / WebTransport surface (HTTP/3) — shares the
            // same TLS cert under <data_dir>/web_{cert,key}.pem.
            features.push(FeatureEntry::just("web.host.h3.v1"));
            // In-app portal served over QUIC reliable streams to the
            // desktop client's embedded Chromium (`conquerd://` scheme).
            features.push(FeatureEntry::just("web.host.app.v1"));
        }
        SupernodeManifest {
            schema_version: SCHEMA_VERSION,
            features,
        }
    }

    /// Reject schemas we don't understand and duplicate ids.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema {
                found: self.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &self.features {
            if !seen.insert(entry.id.as_str()) {
                return Err(ManifestError::DuplicateId(entry.id.clone()));
            }
        }
        Ok(())
    }

    /// Resolve enabled entries to full [`CapabilityDescriptor`]s, merging
    /// well-known defaults with any per-entry overrides. Unknown ids are
    /// resolved using the entry's own fields (vendor/`x.*` plug-ins).
    pub fn enabled_capabilities(&self) -> Vec<CapabilityDescriptor> {
        self.features
            .iter()
            .filter(|f| f.enabled)
            .map(FeatureEntry::resolve)
            .collect()
    }

    /// Enabled entries that carry a `cdylib_manifest` path.
    ///
    /// The supernode's startup code iterates these to load native modules
    /// via [`conquerd_features::NativeModuleLoader`].
    pub fn native_module_entries(&self) -> impl Iterator<Item = &FeatureEntry> {
        self.features
            .iter()
            .filter(|f| f.enabled && f.cdylib_manifest.is_some())
    }
}

impl FeatureEntry {
    /// Build an enabled entry with no overrides.
    pub fn just(id: &str) -> Self {
        Self {
            id: id.to_string(),
            enabled: true,
            version: None,
            kind: None,
            auth: None,
            params: Value::Null,
            experimental: false,
            cdylib_manifest: None,
        }
    }

    /// Materialise the full descriptor for this entry.
    ///
    /// First-party ids start from their well-known descriptor; any
    /// overrides on the entry win. Unknown ids fall back to the entry's
    /// own `version` / `kind` / `auth` (defaulting to `1.0`, `request`,
    /// `trusted-peer` if unset).
    pub fn resolve(&self) -> CapabilityDescriptor {
        let mut base = wellknown_for(&self.id).unwrap_or_else(|| {
            CapabilityDescriptor::new(
                &self.id,
                self.version.clone().unwrap_or_else(|| "1.0".to_string()),
                self.kind.unwrap_or(ChannelKind::Request),
            )
        });
        if let Some(v) = &self.version {
            base.version = v.clone();
        }
        if let Some(k) = self.kind {
            base.kind = k;
        }
        if let Some(a) = self.auth {
            base.auth = a;
        }
        if !self.params.is_null() {
            base.params = merge_json(base.params, self.params.clone());
        }
        if self.experimental {
            base.experimental = true;
        }
        base
    }
}

/// Shallow JSON object merge: keys from `over` win.
fn merge_json(base: Value, over: Value) -> Value {
    match (base, over) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                b.insert(k, v);
            }
            Value::Object(b)
        }
        (_, over) => over,
    }
}

/// Map a known capability id to its well-known descriptor constructor.
fn wellknown_for(id: &str) -> Option<CapabilityDescriptor> {
    match id {
        "transport.quic.audio.v1" => Some(wellknown::transport_quic_audio_v1()),
        "transport.quic.relay.v1" => Some(wellknown::transport_quic_relay_v1()),
        "transport.quic.stream.v1" => Some(wellknown::transport_quic_stream_v1()),
        "transport.quic.feature_datagram.v1" => {
            Some(wellknown::transport_quic_feature_datagram_v1())
        }
        "core.chat.v1" => Some(wellknown::core_chat_v1()),
        "core.file.v1" => Some(wellknown::core_file_v1()),
        "core.audio.opus" => Some(wellknown::core_audio_opus()),
        "room.audio.sfu" => Some(wellknown::room_audio_sfu()),
        "room.chat.v1" => Some(wellknown::room_chat_v1()),
        "room.file.v1" => Some(wellknown::room_file_v1()),
        "web.host.app.v1" => Some(wellknown::web_host_app_v1()),
        "web.host.h3.v1" => Some(wellknown::web_host_h3_v1()),
        "game.relay.v1" => Some(wellknown::game_relay_v1()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_minimal_manifest() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "core.chat.v1"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        assert_eq!(m.features.len(), 1);
        assert_eq!(m.features[0].id, "core.chat.v1");
        assert!(m.features[0].enabled);
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let toml = "schema_version = 99\n";
        let err = SupernodeManifest::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, ManifestError::UnsupportedSchema { .. }));
    }

    #[test]
    fn rejects_duplicate_ids() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "core.chat.v1"
            [[feature]]
            id = "core.chat.v1"
        "#;
        let err = SupernodeManifest::from_toml_str(toml).unwrap_err();
        assert!(matches!(err, ManifestError::DuplicateId(_)));
    }

    #[test]
    fn enabled_filter_skips_disabled() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "core.chat.v1"
            [[feature]]
            id = "core.file.v1"
            enabled = false
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let caps = m.enabled_capabilities();
        let ids: Vec<&str> = caps.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["core.chat.v1"]);
    }

    #[test]
    fn first_party_id_resolves_to_wellknown_defaults() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "core.audio.opus"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let caps = m.enabled_capabilities();
        assert_eq!(caps[0].id, "core.audio.opus");
        assert_eq!(caps[0].kind, ChannelKind::Datagram);
        assert_eq!(caps[0].auth, AuthTier::TrustedPeer);
        // Pulled from wellknown::core_audio_opus()
        assert_eq!(caps[0].params["codec"], json!("opus"));
    }

    #[test]
    fn entry_overrides_win_over_wellknown() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "core.audio.opus"
            version = "1.2"
            auth = "public"
            params = { quota_bytes_per_sec = 16384 }
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let cap = &m.enabled_capabilities()[0];
        assert_eq!(cap.version, "1.2");
        assert_eq!(cap.auth, AuthTier::Public);
        // Original codec param preserved by shallow merge.
        assert_eq!(cap.params["codec"], json!("opus"));
        assert_eq!(cap.params["quota_bytes_per_sec"], json!(16384));
    }

    #[test]
    fn unknown_id_uses_entry_fields() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "x.acme.matchmaker"
            version = "2.1"
            kind = "request"
            auth = "room-member"
            params = { region = "us-east" }
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let cap = &m.enabled_capabilities()[0];
        assert_eq!(cap.id, "x.acme.matchmaker");
        assert_eq!(cap.version, "2.1");
        assert_eq!(cap.kind, ChannelKind::Request);
        assert_eq!(cap.auth, AuthTier::RoomMember);
        assert_eq!(cap.params["region"], json!("us-east"));
    }

    #[test]
    fn legacy_config_includes_chat_when_enabled() {
        let mut config = Config {
            signaling_port: 0,
            relay_port: 0,
            web_port: None,
            chat_enabled: true,
            files_enabled: false,
            sfu_enabled: false,
            updates_enabled: false,
            auto_restart: false,
            invite_ttl_seconds: -1,
            web_title: String::new(),
            access_mode: crate::config::AccessMode::Open,
            access_code: String::new(),
            ad_duration: 0,
            tos_text: String::new(),
            ad_content: String::new(),
            demo_links: false,
            external_host: None,
            data_dir: std::path::PathBuf::from("."),
            web_localhost_only: false,
        };
        let m = SupernodeManifest::from_legacy_config(&config);
        let ids: Vec<&str> = m.features.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"core.chat.v1"));
        assert!(ids.contains(&"room.chat.v1"));
        assert!(!ids.contains(&"core.file.v1"));
        assert!(!ids.contains(&"room.file.v1"));
        assert!(!ids.contains(&"room.audio.sfu"));
        assert!(!ids.contains(&"web.host.app.v1"));

        config.files_enabled = true;
        config.sfu_enabled = true;
        config.web_port = Some(443);
        let m = SupernodeManifest::from_legacy_config(&config);
        let ids: Vec<&str> = m.features.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"core.file.v1"));
        assert!(ids.contains(&"room.file.v1"));
        assert!(ids.contains(&"room.audio.sfu"));
        assert!(!ids.contains(&"web.host.https"));
        assert!(ids.contains(&"web.host.h3.v1"));
        assert!(ids.contains(&"web.host.app.v1"));
    }

    #[test]
    fn load_or_derive_falls_back_when_missing() {
        let tmp = tempdir();
        let config = legacy_config_for(&tmp);
        let m = SupernodeManifest::load_or_derive(&tmp, &config).unwrap();
        // Default Config has chat+files+sfu enabled (env defaults).
        assert!(!m.features.is_empty());
    }

    #[test]
    fn load_or_derive_reads_existing_file() {
        let tmp = tempdir();
        std::fs::write(
            tmp.join("supernode.toml"),
            "schema_version = 1\n[[feature]]\nid = \"core.chat.v1\"\n",
        )
        .unwrap();
        let config = legacy_config_for(&tmp);
        let m = SupernodeManifest::load_or_derive(&tmp, &config).unwrap();
        assert_eq!(m.features.len(), 1);
        assert_eq!(m.features[0].id, "core.chat.v1");
    }

    // Tiny tempdir helper — we don't pull in the `tempfile` crate just
    // for two tests.
    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "conquerd-manifest-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn legacy_config_for(data_dir: &std::path::Path) -> Config {
        Config {
            signaling_port: 0,
            relay_port: 0,
            web_port: None,
            chat_enabled: true,
            files_enabled: true,
            sfu_enabled: true,
            updates_enabled: false,
            auto_restart: false,
            invite_ttl_seconds: -1,
            web_title: String::new(),
            access_mode: crate::config::AccessMode::Open,
            access_code: String::new(),
            ad_duration: 0,
            tos_text: String::new(),
            ad_content: String::new(),
            demo_links: false,
            external_host: None,
            data_dir: data_dir.to_path_buf(),
            web_localhost_only: false,
        }
    }

    // ── Phase 5: cdylib_manifest field + native_module_entries() ────────────

    #[test]
    fn parses_cdylib_manifest_field() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "x.acme.matchmaker"
            cdylib_manifest = "/opt/supernodes/acme.module.toml"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        assert_eq!(
            m.features[0].cdylib_manifest.as_deref(),
            Some(std::path::Path::new("/opt/supernodes/acme.module.toml"))
        );
    }

    #[test]
    fn cdylib_manifest_absent_by_default() {
        let toml = "schema_version = 1\n[[feature]]\nid = \"core.chat.v1\"\n";
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        assert!(m.features[0].cdylib_manifest.is_none());
    }

    #[test]
    fn native_module_entries_only_returns_cdylib_enabled() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "core.chat.v1"

            [[feature]]
            id = "x.acme.matchmaker"
            cdylib_manifest = "/tmp/acme.module.toml"

            [[feature]]
            id = "x.acme.other"
            enabled = false
            cdylib_manifest = "/tmp/other.module.toml"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let native: Vec<&str> = m.native_module_entries().map(|e| e.id.as_str()).collect();
        // Only the enabled entry with a cdylib_manifest.
        assert_eq!(native, vec!["x.acme.matchmaker"]);
    }

    #[test]
    fn enabled_capabilities_includes_cdylib_entry_descriptor() {
        // A cdylib entry still produces a capability descriptor for advertisement.
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "x.acme.matchmaker"
            version = "1.0"
            kind = "request"
            cdylib_manifest = "/tmp/acme.module.toml"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let caps = m.enabled_capabilities();
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].id, "x.acme.matchmaker");
    }
}
