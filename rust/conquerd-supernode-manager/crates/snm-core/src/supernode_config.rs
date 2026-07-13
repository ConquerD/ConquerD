use serde::{Deserialize, Deserializer, Serialize, Serializer};
use toml::Value;

/// Documented supernode access modes (`access.rs`).
pub const ACCESS_MODES: &[&str] = &["open", "tos", "access_code", "timer", "custom"];

/// Default SFU room-creation policy when inventory/manifest omit explicit values.
pub const DEFAULT_ALLOW_PUBLIC_ROOMS: bool = false;
pub const DEFAULT_ALLOW_PRIVATE_ROOMS: bool = true;

/// First-party and common feature IDs (see `SUPERNODE.md`).
pub const KNOWN_FEATURES: &[&str] = &[
    "core.chat.v1",
    "room.chat.v1",
    "room.audio.sfu",
    "room.file.v1",
    "core.file.v1",
    "web.host.app.v1",
    "game.relay.v1",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SupernodeDefaults {
    /// Bind address for QUIC relay and WebSocket (without port).
    #[serde(default = "default_listen_bind")]
    pub listen_bind: String,
    /// Relative path inside the instance data directory.
    #[serde(default = "default_identity_file")]
    pub identity_file: String,
    /// Default SFU policy: allow peers to materialize public rooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_public_rooms: Option<bool>,
    /// Default SFU policy: allow peers to materialize private rooms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_private_rooms: Option<bool>,
}

impl Default for SupernodeDefaults {
    fn default() -> Self {
        Self {
            listen_bind: default_listen_bind(),
            identity_file: default_identity_file(),
            allow_public_rooms: Some(DEFAULT_ALLOW_PUBLIC_ROOMS),
            allow_private_rooms: Some(DEFAULT_ALLOW_PRIVATE_ROOMS),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FeatureSpec {
    pub id: String,
    pub enabled: bool,
    pub params: Option<Value>,
    pub cdylib_manifest: Option<String>,
}

impl FeatureSpec {
    pub fn enabled(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            params: None,
            cdylib_manifest: None,
        }
    }

    pub fn from_id(id: impl Into<String>) -> Self {
        Self::enabled(id)
    }
}

impl Serialize for FeatureSpec {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.params.is_none() && self.cdylib_manifest.is_none() && self.enabled {
            return self.id.serialize(serializer);
        }
        #[derive(Serialize)]
        struct Full<'a> {
            id: &'a str,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            enabled: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            params: &'a Option<Value>,
            #[serde(skip_serializing_if = "Option::is_none")]
            cdylib_manifest: &'a Option<String>,
        }
        Full {
            id: &self.id,
            enabled: self.enabled,
            params: &self.params,
            cdylib_manifest: &self.cdylib_manifest,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FeatureSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Id(String),
            Full {
                id: String,
                #[serde(default = "default_true")]
                enabled: bool,
                #[serde(default)]
                params: Option<Value>,
                #[serde(default)]
                cdylib_manifest: Option<String>,
            },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Id(id) => Ok(Self::enabled(id)),
            Raw::Full {
                id,
                enabled,
                params,
                cdylib_manifest,
            } => Ok(Self {
                id,
                enabled,
                params,
                cdylib_manifest,
            }),
        }
    }
}

pub fn deserialize_feature_list<'de, D>(deserializer: D) -> Result<Vec<FeatureSpec>, D::Error>
where
    D: Deserializer<'de>,
{
    Vec::<FeatureSpec>::deserialize(deserializer)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSupernodeConfig {
    pub listen_bind: String,
    pub relay_port: u16,
    pub ws_port: u16,
    pub public_host: String,
    pub identity_file: String,
    pub access_mode: String,
    pub features: Vec<FeatureSpec>,
}

pub fn resolve_supernode_config(
    defaults: &crate::Defaults,
    instance: &crate::Instance,
    relay_port: u16,
    ws_port: u16,
) -> ResolvedSupernodeConfig {
    let features = if instance.features.is_empty() {
        default_instance_features()
    } else {
        instance.features.clone()
    };
    let features = enrich_feature_params(features, &defaults.supernode, instance);

    ResolvedSupernodeConfig {
        listen_bind: instance
            .listen_bind
            .clone()
            .unwrap_or_else(|| defaults.supernode.listen_bind.clone()),
        relay_port,
        ws_port,
        public_host: instance.public_host.clone(),
        identity_file: instance
            .identity_file
            .clone()
            .unwrap_or_else(|| defaults.supernode.identity_file.clone()),
        access_mode: instance
            .access_mode
            .clone()
            .unwrap_or_else(|| defaults.access_mode.clone()),
        features,
    }
}

/// Attach derived manifest params (SFU room policy) to features.
pub fn enrich_feature_params(
    mut features: Vec<FeatureSpec>,
    sn_defaults: &SupernodeDefaults,
    instance: &crate::Instance,
) -> Vec<FeatureSpec> {
    for feature in &mut features {
        if feature.id == "room.audio.sfu" && feature.enabled {
            apply_sfu_room_policy(feature, sn_defaults, instance);
        }
    }
    features
}

fn param_bool_from_table(params: Option<&Value>, key: &str) -> Option<bool> {
    params.and_then(|v| v.get(key)).and_then(|v| v.as_bool())
}

fn merge_feature_param(feature: &mut FeatureSpec, key: &str, value: Value) {
    let mut table = feature
        .params
        .as_ref()
        .and_then(|v| v.as_table().cloned())
        .unwrap_or_default();
    table.insert(key.into(), value);
    feature.params = Some(Value::Table(table));
}

fn set_sfu_room_policy_params(feature: &mut FeatureSpec, allow_public: bool, allow_private: bool) {
    if allow_public && allow_private {
        return;
    }
    merge_feature_param(feature, "allow_public_rooms", Value::Boolean(allow_public));
    merge_feature_param(
        feature,
        "allow_private_rooms",
        Value::Boolean(allow_private),
    );
}

fn apply_sfu_room_policy(
    feature: &mut FeatureSpec,
    sn_defaults: &SupernodeDefaults,
    instance: &crate::Instance,
) {
    let allow_public = param_bool_from_table(feature.params.as_ref(), "allow_public_rooms")
        .or(instance.allow_public_rooms)
        .or(sn_defaults.allow_public_rooms)
        .unwrap_or(DEFAULT_ALLOW_PUBLIC_ROOMS);
    let allow_private = param_bool_from_table(feature.params.as_ref(), "allow_private_rooms")
        .or(instance.allow_private_rooms)
        .or(sn_defaults.allow_private_rooms)
        .unwrap_or(DEFAULT_ALLOW_PRIVATE_ROOMS);
    set_sfu_room_policy_params(feature, allow_public, allow_private);
}

/// Attach non-default SFU room policy params to `room.audio.sfu` when present.
pub fn apply_room_policy_to_features(
    features: &mut [FeatureSpec],
    allow_public: bool,
    allow_private: bool,
) {
    if allow_public && allow_private {
        return;
    }
    for feature in features.iter_mut() {
        if feature.id == "room.audio.sfu" && feature.enabled {
            set_sfu_room_policy_params(feature, allow_public, allow_private);
            break;
        }
    }
}

/// Read SFU room policy from a feature list (for TUI / inventory display).
pub fn room_policy_from_features(features: &[FeatureSpec]) -> (bool, bool) {
    let sfu = features.iter().find(|f| f.id == "room.audio.sfu");
    let params = sfu.and_then(|f| f.params.as_ref());
    let allow_public =
        param_bool_from_table(params, "allow_public_rooms").unwrap_or(DEFAULT_ALLOW_PUBLIC_ROOMS);
    let allow_private =
        param_bool_from_table(params, "allow_private_rooms").unwrap_or(DEFAULT_ALLOW_PRIVATE_ROOMS);
    (allow_public, allow_private)
}

fn default_sfu_feature() -> FeatureSpec {
    let mut feature = FeatureSpec::enabled("room.audio.sfu");
    set_sfu_room_policy_params(
        &mut feature,
        DEFAULT_ALLOW_PUBLIC_ROOMS,
        DEFAULT_ALLOW_PRIVATE_ROOMS,
    );
    feature
}

pub fn default_instance_features() -> Vec<FeatureSpec> {
    vec![
        FeatureSpec::enabled("core.chat.v1"),
        FeatureSpec::enabled("room.chat.v1"),
        default_sfu_feature(),
        FeatureSpec::enabled("room.file.v1"),
        FeatureSpec::enabled("web.host.app.v1"),
        FeatureSpec::enabled("game.relay.v1"),
    ]
}

pub fn features_to_csv(features: &[FeatureSpec]) -> String {
    features
        .iter()
        .filter(|f| f.enabled)
        .map(|f| f.id.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn features_from_csv(raw: &str) -> Vec<FeatureSpec> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(FeatureSpec::from_id)
        .collect()
}

pub fn default_listen_bind() -> String {
    "0.0.0.0".into()
}

pub fn default_identity_file() -> String {
    "identity.json".into()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Defaults, Instance, Inventory};

    #[test]
    fn deserializes_string_features() {
        let raw = r#"
[[host]]
name = "edge-1"
ssh = "root@1.2.3.4"

  [[host.instance]]
  id = "a"
  public_host = "h.example.net"
  features = ["core.chat.v1", "room.audio.sfu"]
"#;
        let inv: Inventory = toml::from_str(raw).unwrap();
        assert_eq!(inv.host[0].instances[0].features.len(), 2);
        assert_eq!(inv.host[0].instances[0].features[0].id, "core.chat.v1");
    }

    #[test]
    fn deserializes_table_features() {
        let raw = r#"
[[host]]
name = "edge-1"
ssh = "root@1.2.3.4"

    [[host.instance]]
    id = "a"
    public_host = "h.example.net"
    features = [
      { id = "web.host.app.v1", enabled = true },
      { id = "game.relay.v1", enabled = true },
    ]
"#;
        let inv: Inventory = toml::from_str(raw).unwrap();
        let f = &inv.host[0].instances[0].features[0];
        assert_eq!(f.id, "web.host.app.v1");
    }

    #[test]
    fn default_instance_features_include_chat_portal_and_room_policy() {
        let features = default_instance_features();
        assert!(features.iter().any(|f| f.id == "room.chat.v1"));
        assert!(features.iter().any(|f| f.id == "web.host.app.v1"));
        assert!(features.iter().any(|f| f.id == "game.relay.v1"));
        assert!(features.iter().all(|f| f.id != "web.host.h3.v1"));
        let sfu = features.iter().find(|f| f.id == "room.audio.sfu").unwrap();
        let params = sfu.params.as_ref().unwrap();
        assert_eq!(
            params.get("allow_public_rooms").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            params.get("allow_private_rooms").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn enriches_sfu_room_policy_from_instance_override() {
        let features = vec![FeatureSpec::enabled("room.audio.sfu")];
        let sn_defaults = SupernodeDefaults::default();
        let instance = Instance {
            id: "a".into(),
            public_host: "h.example.net".into(),
            relay_port: None,
            ws_port: None,
            cluster_port: None,
            listen_bind: None,
            access_mode: None,
            identity_file: None,
            allow_public_rooms: Some(false),
            allow_private_rooms: None,
            features: vec![],
        };
        let out = enrich_feature_params(features, &sn_defaults, &instance);
        let params = out[0].params.as_ref().unwrap();
        assert_eq!(
            params.get("allow_public_rooms").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn resolves_instance_overrides() {
        let mut defaults = Defaults::default();
        defaults.access_mode = "open".into();
        defaults.supernode.listen_bind = "0.0.0.0".into();
        let instance = Instance {
            id: "a".into(),
            public_host: "edge.example.net".into(),
            relay_port: Some(3478),
            ws_port: Some(34935),
            cluster_port: None,
            listen_bind: Some("127.0.0.1".into()),
            access_mode: Some("access_code".into()),
            identity_file: None,
            allow_public_rooms: None,
            allow_private_rooms: None,
            features: default_instance_features(),
        };
        let resolved = resolve_supernode_config(&defaults, &instance, 3478, 34935);
        assert_eq!(resolved.listen_bind, "127.0.0.1");
        assert_eq!(resolved.access_mode, "access_code");
    }
}
