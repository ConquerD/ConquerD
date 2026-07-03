use snm_core::ResolvedSupernodeConfig;

pub const SCHEMA_VERSION: u32 = 1;

const TRANSPORT_FEATURES: &[&str] = &[
    "transport.quic.relay.v1",
    "transport.quic.stream.v1",
    "transport.quic.feature_datagram.v1",
];

/// One member entry in the `[cluster]` roster.
#[derive(Debug, Clone)]
pub struct ClusterMemberEntry {
    /// base64url Ed25519 public key — the member's trust anchor.
    pub identity_pub: String,
    /// Client-facing QUIC relay address `host:port`.
    pub relay_addr: String,
    /// Dedicated supernode↔supernode QUIC address `host:port`.
    pub cluster_addr: String,
    /// Client-facing WebSocket signaling address `host:port`.
    pub ws_addr: String,
    /// Optional WebTransport/portal port.
    pub web_port: Option<u16>,
}

/// The full cluster roster injected into a member's `supernode.toml`.
#[derive(Debug, Clone)]
pub struct ClusterRoster {
    /// Stable identifier shared by every member (`cluster_id` in the manifest).
    pub cluster_id: String,
    pub members: Vec<ClusterMemberEntry>,
}

#[derive(Debug, Clone)]
struct FeatureEntry {
    id: String,
    enabled: bool,
    params: Option<toml::Value>,
    cdylib_manifest: Option<String>,
}

fn format_inline_table(table: &toml::map::Map<String, toml::Value>) -> String {
    let mut parts = Vec::new();
    for (key, value) in table {
        let rendered = match value {
            toml::Value::Boolean(b) => b.to_string(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::String(s) => format!("\"{s}\""),
            other => other.to_string(),
        };
        parts.push(format!("{key} = {rendered}"));
    }
    format!("{{ {} }}", parts.join(", "))
}

fn render_feature_entry(feature: &FeatureEntry) -> String {
    let mut lines = vec![format!("[[feature]]"), format!("id = \"{}\"", feature.id)];
    if feature.enabled {
        lines.push("enabled = true".into());
    }
    if let Some(params) = &feature.params {
        if let Some(table) = params.as_table() {
            if !table.is_empty() {
                lines.push(format!("params = {}", format_inline_table(table)));
            }
        }
    }
    if let Some(path) = &feature.cdylib_manifest {
        lines.push(format!("cdylib_manifest = \"{path}\""));
    }
    lines.join("\n")
}

pub fn render_supernode_toml(config: &ResolvedSupernodeConfig) -> String {
    render_supernode_toml_with_cluster(config, None)
}

pub fn render_supernode_toml_with_cluster(
    config: &ResolvedSupernodeConfig,
    cluster: Option<&ClusterRoster>,
) -> String {
    let mut features: Vec<FeatureEntry> = TRANSPORT_FEATURES
        .iter()
        .map(|id| FeatureEntry {
            id: (*id).into(),
            enabled: true,
            params: None,
            cdylib_manifest: None,
        })
        .collect();

    for feature in &config.features {
        if TRANSPORT_FEATURES.contains(&feature.id.as_str()) {
            continue;
        }
        features.push(FeatureEntry {
            id: feature.id.clone(),
            enabled: feature.enabled,
            params: feature.params.clone(),
            cdylib_manifest: feature.cdylib_manifest.clone(),
        });
    }

    let mut root = toml::map::Map::new();
    root.insert(
        "schema_version".into(),
        toml::Value::Integer(i64::from(SCHEMA_VERSION)),
    );
    root.insert(
        "listen_addr".into(),
        toml::Value::String(format!("{}:{}", config.listen_bind, config.relay_port)),
    );
    root.insert(
        "ws_listen_addr".into(),
        toml::Value::String(format!("{}:{}", config.listen_bind, config.ws_port)),
    );
    if let Some(web_port) = config.web_port {
        root.insert("web_port".into(), toml::Value::Integer(i64::from(web_port)));
    }
    root.insert(
        "identity_file".into(),
        toml::Value::String(config.identity_file.clone()),
    );
    root.insert(
        "access_mode".into(),
        toml::Value::String(config.access_mode.clone()),
    );
    let header =
        toml::to_string_pretty(&toml::Value::Table(root)).expect("manifest header serializes");
    let feature_blocks = features
        .iter()
        .map(render_feature_entry)
        .collect::<Vec<_>>()
        .join("\n\n");
    let cluster_block = cluster.map(render_cluster_section).unwrap_or_default();
    if cluster_block.is_empty() {
        format!("{header}\n\n{feature_blocks}")
    } else {
        format!("{header}\n\n{feature_blocks}\n\n{cluster_block}")
    }
}

/// Render the `[cluster]` + `[[cluster.member]]` section.
fn render_cluster_section(roster: &ClusterRoster) -> String {
    let mut lines = Vec::new();
    lines.push("[cluster]".into());
    lines.push(format!("cluster_id = {:?}", roster.cluster_id));
    lines.push(String::new());
    for member in &roster.members {
        lines.push("[[cluster.member]]".into());
        lines.push(format!("identity_pub = {:?}", member.identity_pub));
        lines.push(format!("relay_addr   = {:?}", member.relay_addr));
        lines.push(format!("cluster_addr = {:?}", member.cluster_addr));
        lines.push(format!("ws_addr      = {:?}", member.ws_addr));
        if let Some(web_port) = member.web_port {
            lines.push(format!("web_port     = {web_port}"));
        }
        lines.push(String::new());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use snm_core::{default_instance_features, FeatureSpec, ResolvedSupernodeConfig};

    use super::*;

    fn sample_config() -> ResolvedSupernodeConfig {
        ResolvedSupernodeConfig {
            listen_bind: "0.0.0.0".into(),
            relay_port: 3478,
            ws_port: 34935,
            web_port: Some(8443),
            public_host: "edge1.example.net".into(),
            identity_file: "identity.json".into(),
            access_mode: "open".into(),
            features: default_instance_features(),
        }
    }

    #[test]
    fn includes_network_access_and_features() {
        let raw = render_supernode_toml(&sample_config());
        assert!(raw.contains("schema_version = 1"));
        assert!(raw.contains("listen_addr = \"0.0.0.0:3478\""));
        assert!(raw.contains("ws_listen_addr = \"0.0.0.0:34935\""));
        assert!(raw.contains("web_port = 8443"));
        assert!(raw.contains("access_mode = \"open\""));
        assert!(raw.contains("core.chat.v1"));
        assert!(raw.contains("transport.quic.relay.v1"));
    }

    #[test]
    fn serializes_feature_params() {
        let mut config = sample_config();
        config.features = vec![FeatureSpec {
            id: "web.host.h3.v1".into(),
            enabled: true,
            params: Some(toml::Value::Table({
                let mut t = toml::map::Map::new();
                t.insert("port".into(), toml::Value::Integer(8443));
                t
            })),
            cdylib_manifest: None,
        }];
        let raw = render_supernode_toml(&config);
        assert!(raw.contains("params"));
        assert!(raw.contains("port = 8443"));
    }

    #[test]
    fn serializes_sfu_room_policy_params() {
        let mut config = sample_config();
        config.features = vec![FeatureSpec {
            id: "room.audio.sfu".into(),
            enabled: true,
            params: Some(toml::Value::Table({
                let mut t = toml::map::Map::new();
                t.insert("allow_public_rooms".into(), toml::Value::Boolean(false));
                t
            })),
            cdylib_manifest: None,
        }];
        let raw = render_supernode_toml(&config);
        assert!(raw.contains("allow_public_rooms = false"));
        assert!(
            !raw.contains("[feature.params]"),
            "params must be inline on the feature row, not a sibling [feature.params] table"
        );
    }
}
