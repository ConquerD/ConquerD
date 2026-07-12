//! Typed `supernode.toml` feature manifest.
//!
//! Replaces ad-hoc env-var feature toggles with a single declarative
//! manifest that lists every capability the supernode wants to host. The
//! manifest declares operator-selected hosted features. Startup code may still
//! upsert built-in first-party descriptors so relay/quota accounting can
//! classify core, room, and game traffic when those entries are omitted.
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
//! When the file is missing the loader returns a full first-party
//! [`SupernodeManifest::default_manifest`]. Operators and the supernode-manager
//! should write `supernode.toml` for durable control.

use std::path::Path;

use conquerd_features::descriptor::{AuthTier, CapabilityDescriptor, ChannelKind};
use conquerd_features::registry::FeatureRegistry;
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

    /// QUIC relay bind (`host:port`). Written by supernode-manager.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_addr: Option<String>,

    /// WebSocket signaling bind (`host:port`). Written by supernode-manager.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_listen_addr: Option<String>,

    /// WebTransport / portal UDP-TCP port. Written by supernode-manager.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_port: Option<u16>,

    /// Relative path to the node identity inside `CONQUERD_HOME`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,

    /// Node join access mode (`open`, `tos`, `access_code`, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<String>,

    /// Optional `[cluster]` section: link this supernode with others into one
    /// logical node. Absent ⇒ standalone. Additive (no schema bump): older
    /// builds that don't know the field ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster: Option<crate::cluster::ClusterConfig>,

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

    /// Load `<data_dir>/supernode.toml` if it exists, else return the full
    /// first-party [`default_manifest`].
    pub fn load_or_default(data_dir: &Path) -> Result<Self, ManifestError> {
        let path = data_dir.join("supernode.toml");
        if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            return Self::from_toml_str(&raw);
        }
        Ok(Self::default_manifest())
    }

    /// Full first-party capability set used when no `supernode.toml` is present.
    pub fn default_manifest() -> Self {
        SupernodeManifest {
            schema_version: SCHEMA_VERSION,
            features: vec![
                FeatureEntry::just("transport.quic.relay.v1"),
                FeatureEntry::just("transport.quic.stream.v1"),
                FeatureEntry::just("transport.quic.feature_datagram.v1"),
                FeatureEntry::just("core.chat.v1"),
                FeatureEntry::just("room.chat.v1"),
                FeatureEntry::just("core.file.v1"),
                FeatureEntry::just("room.file.v1"),
                FeatureEntry::just("room.audio.sfu"),
                FeatureEntry::just("web.host.h3.v1"),
                FeatureEntry::just("web.host.app.v1"),
            ],
            ..Default::default()
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

    /// Whether the manifest enables any `web.host.*` portal capability.
    pub fn portal_enabled(&self) -> bool {
        self.features
            .iter()
            .any(|f| f.enabled && f.id.starts_with("web.host"))
    }

    /// Resolve the effective WebTransport port from top-level `web_port` or
    /// `web.host.h3.v1` feature params.
    pub fn resolved_web_port(&self) -> Option<u16> {
        self.web_port.or_else(|| {
            self.features
                .iter()
                .find(|f| f.enabled && f.id == "web.host.h3.v1")
                .and_then(|f| f.params.get("port"))
                .and_then(|v| v.as_u64())
                .filter(|&p| p <= u16::MAX as u64)
                .map(|p| p as u16)
        })
    }

    /// Merge network fields from the manifest into runtime [`Config`].
    ///
    /// Env vars win when the manifest omits a field. When portal features are
    /// enabled but no web port is declared anywhere, defaults to `8443`.
    pub fn apply_to_config(&self, config: &mut Config) {
        if let Some(addr) = &self.listen_addr {
            if let Some(port) = parse_socket_port(addr) {
                config.relay_port = port;
            }
        }
        if let Some(addr) = &self.ws_listen_addr {
            if let Some(port) = parse_socket_port(addr) {
                config.signaling_port = port;
            }
        }
        if let Some(port) = self.resolved_web_port() {
            config.web_port = Some(port);
        } else if self.portal_enabled() && config.web_port.is_none() {
            config.web_port = Some(8443);
        }
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

/// Operator policy for which SFU room types peers may materialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SfuRoomCreationPolicy {
    pub allow_public: bool,
    pub allow_private: bool,
}

impl Default for SfuRoomCreationPolicy {
    fn default() -> Self {
        Self {
            allow_public: false,
            allow_private: true,
        }
    }
}

impl SfuRoomCreationPolicy {
    /// Denial reason for creating a *new* room of the given kind.
    /// `public == true` → public room; `false` → private.
    /// Returns `None` when the create is allowed under this operator policy.
    pub fn deny_reason_for_new_room(&self, public: bool) -> Option<&'static str> {
        if public && !self.allow_public {
            Some("public_rooms_disabled")
        } else if !public && !self.allow_private {
            Some("private_rooms_disabled")
        } else {
            None
        }
    }
}

/// Read a boolean operator param from a merged capability params object.
pub fn param_bool(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

/// Resolve SFU room-creation policy from the registered `room.audio.sfu`
/// descriptor (merged manifest params over well-known defaults).
pub fn sfu_room_creation_policy(registry: &FeatureRegistry) -> SfuRoomCreationPolicy {
    let params = registry
        .get("room.audio.sfu")
        .map(|c| c.params.clone())
        .unwrap_or(Value::Null);
    SfuRoomCreationPolicy {
        allow_public: param_bool(&params, "allow_public_rooms", false),
        allow_private: param_bool(&params, "allow_private_rooms", true),
    }
}

/// Parse the port component from `host:port` listen addresses.
fn parse_socket_port(addr: &str) -> Option<u16> {
    let (_, port) = addr.rsplit_once(':')?;
    port.parse().ok()
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
    use conquerd_features::registry::FeatureRegistry;
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
    fn parses_optional_cluster_section() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "room.chat.v1"

            [cluster]
            cluster_id = "acme-us"

            [[cluster.member]]
            identity_pub = "NODE_A"
            relay_addr = "a.example:3478"
            ws_addr = "a.example:34935"

            [[cluster.member]]
            identity_pub = "NODE_B"
            relay_addr = "b.example:3478"
            web_port = 8443
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let cluster = m.cluster.expect("cluster section present");
        assert_eq!(cluster.cluster_id, "acme-us");
        assert_eq!(cluster.members.len(), 2);
        assert_eq!(cluster.members[0].identity_pub, "NODE_A");
        assert_eq!(
            cluster.members[0].ws_addr.as_deref(),
            Some("a.example:34935")
        );
        assert_eq!(cluster.members[1].web_port, Some(8443));
    }

    #[test]
    fn cluster_section_absent_by_default() {
        let toml = "schema_version = 1\n[[feature]]\nid = \"core.chat.v1\"\n";
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        assert!(m.cluster.is_none());
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
    fn apply_to_config_reads_network_fields_from_manifest() {
        let toml = r#"
            schema_version = 1
            listen_addr = "0.0.0.0:3578"
            ws_listen_addr = "0.0.0.0:35035"
            web_port = 8543
            [[feature]]
            id = "web.host.app.v1"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let mut config = legacy_config_for(std::path::Path::new("."));
        config.relay_port = 3478;
        config.signaling_port = 34935;
        config.web_port = None;
        m.apply_to_config(&mut config);
        assert_eq!(config.relay_port, 3578);
        assert_eq!(config.signaling_port, 35035);
        assert_eq!(config.web_port, Some(8543));
    }

    #[test]
    fn apply_to_config_defaults_web_port_when_portal_enabled() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "web.host.app.v1"
            [[feature]]
            id = "web.host.h3.v1"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let mut config = legacy_config_for(std::path::Path::new("."));
        config.web_port = None;
        m.apply_to_config(&mut config);
        assert_eq!(config.web_port, Some(8443));
    }

    #[test]
    fn room_create_deny_table_matches_acdc_defaults() {
        // acdc / default operator policy: private ok, public denied.
        let policy = SfuRoomCreationPolicy::default();
        assert_eq!(
            policy.deny_reason_for_new_room(true),
            Some("public_rooms_disabled")
        );
        assert_eq!(policy.deny_reason_for_new_room(false), None);

        // Fully open.
        let open = SfuRoomCreationPolicy {
            allow_public: true,
            allow_private: true,
        };
        assert_eq!(open.deny_reason_for_new_room(true), None);
        assert_eq!(open.deny_reason_for_new_room(false), None);

        // Fully closed.
        let closed = SfuRoomCreationPolicy {
            allow_public: false,
            allow_private: false,
        };
        assert_eq!(
            closed.deny_reason_for_new_room(true),
            Some("public_rooms_disabled")
        );
        assert_eq!(
            closed.deny_reason_for_new_room(false),
            Some("private_rooms_disabled")
        );

        // Public-only (unusual but allowed by the table).
        let public_only = SfuRoomCreationPolicy {
            allow_public: true,
            allow_private: false,
        };
        assert_eq!(public_only.deny_reason_for_new_room(true), None);
        assert_eq!(
            public_only.deny_reason_for_new_room(false),
            Some("private_rooms_disabled")
        );
    }

    #[test]
    fn sfu_room_policy_defaults_private_only() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "room.audio.sfu"
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let registry = FeatureRegistry::new();
        for cap in m.enabled_capabilities() {
            registry.register(cap).unwrap();
        }
        let policy = sfu_room_creation_policy(&registry);
        assert!(!policy.allow_public);
        assert!(policy.allow_private);
    }

    #[test]
    fn sfu_room_policy_honors_manifest_params() {
        let toml = r#"
            schema_version = 1
            [[feature]]
            id = "room.audio.sfu"
            params = { allow_public_rooms = false, allow_private_rooms = true }
        "#;
        let m = SupernodeManifest::from_toml_str(toml).unwrap();
        let registry = FeatureRegistry::new();
        for cap in m.enabled_capabilities() {
            registry.register(cap).unwrap();
        }
        let policy = sfu_room_creation_policy(&registry);
        assert!(!policy.allow_public);
        assert!(policy.allow_private);
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
    fn default_manifest_includes_first_party_stack() {
        let m = SupernodeManifest::default_manifest();
        let ids: Vec<&str> = m.features.iter().map(|f| f.id.as_str()).collect();
        for expected in [
            "core.chat.v1",
            "room.chat.v1",
            "core.file.v1",
            "room.file.v1",
            "room.audio.sfu",
            "web.host.h3.v1",
            "web.host.app.v1",
            "transport.quic.relay.v1",
        ] {
            assert!(ids.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn load_or_default_when_missing() {
        let tmp = tempdir();
        let m = SupernodeManifest::load_or_default(&tmp).unwrap();
        assert!(!m.features.is_empty());
        assert_eq!(
            m.features.len(),
            SupernodeManifest::default_manifest().features.len()
        );
    }

    #[test]
    fn load_or_default_reads_existing_file() {
        let tmp = tempdir();
        std::fs::write(
            tmp.join("supernode.toml"),
            "schema_version = 1\n[[feature]]\nid = \"core.chat.v1\"\n",
        )
        .unwrap();
        let m = SupernodeManifest::load_or_default(&tmp).unwrap();
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
