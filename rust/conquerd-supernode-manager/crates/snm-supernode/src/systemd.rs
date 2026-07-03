use snm_core::Defaults;
use snm_transport::shell_escape;

use crate::layout::{InstanceLayout, NetworkEnv};

pub const UNIT_TEMPLATE_NAME: &str = "conquerd-supernode@.service";
const UNIT_DROPIN_FILENAME: &str = "override.conf";

/// Shared templated unit — no per-instance ports or data dir (those live in drop-ins).
pub fn render_unit_template(layout: &InstanceLayout, _defaults: &Defaults) -> String {
    format!(
        r#"[Unit]
Description=ConquerD Supernode (%i)
After=network.target

[Service]
User={user}
ExecStart={exec_start}
Restart=on-failure
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
"#,
        user = layout.service_user,
        exec_start = shell_escape(&layout.current_binary_link),
    )
}

/// Per-instance override: data dir and public relay ticket host.
/// Ports and access mode live in `supernode.toml`.
pub fn render_unit_dropin(layout: &InstanceLayout, network: &NetworkEnv) -> String {
    let mut lines = vec![
        "[Service]".into(),
        format!(
            "Environment=CONQUERD_HOME={}",
            shell_escape(&layout.data_dir)
        ),
        format!(
            "Environment=supernode_host={}",
            shell_escape(&network.public_host)
        ),
        format!("Environment=supernode_port={}", network.relay_port),
        format!("Environment=supernode_signaling_port={}", network.ws_port),
    ];
    if let Some(web_port) = network.web_port {
        lines.push(format!("Environment=supernode_web_port={web_port}"));
    }
    format!("{}\n", lines.join("\n"))
}

pub fn unit_template_path() -> &'static str {
    "/etc/systemd/system/conquerd-supernode@.service"
}

pub fn unit_dropin_path(instance_id: &str) -> String {
    format!("/etc/systemd/system/conquerd-supernode@{instance_id}.service.d/{UNIT_DROPIN_FILENAME}")
}

pub fn unit_dropin_dir(instance_id: &str) -> String {
    format!("/etc/systemd/system/conquerd-supernode@{instance_id}.service.d")
}

#[cfg(test)]
mod tests {
    use snm_core::Defaults;

    use crate::layout::{InstanceLayout, NetworkEnv};

    use super::{render_unit_dropin, render_unit_template};

    fn sample_layout() -> InstanceLayout {
        InstanceLayout {
            instance_id: "a".into(),
            unit_name: "conquerd-supernode@a.service".into(),
            data_dir: "/var/lib/conquerd/a".into(),
            binary_dir: "/opt/conquerd/bin".into(),
            versioned_binary: "/opt/conquerd/bin/conquerd-supernode-1.0.0".into(),
            current_binary_link: "/opt/conquerd/bin/current".into(),
            manifest_path: "/var/lib/conquerd/a/supernode.toml".into(),
            service_user: "conquerd".into(),
        }
    }

    fn sample_network() -> NetworkEnv {
        NetworkEnv {
            relay_port: 3478,
            ws_port: 34935,
            web_port: Some(8443),
            public_host: "edge1.example.net".into(),
            access_mode: "open".into(),
        }
    }

    #[test]
    fn template_is_generic_without_instance_env() {
        let unit = render_unit_template(&sample_layout(), &Defaults::default());
        assert!(unit.contains("ExecStart=/opt/conquerd/bin/current"));
        assert!(unit.contains("User=conquerd"));
        assert!(!unit.contains("CONQUERD_HOME"));
        assert!(!unit.contains("supernode_port"));
    }

    #[test]
    fn dropin_carries_home_public_host_and_ports() {
        let dropin = render_unit_dropin(&sample_layout(), &sample_network());
        assert!(dropin.contains("Environment=CONQUERD_HOME=/var/lib/conquerd/a"));
        assert!(dropin.contains("Environment=supernode_host=edge1.example.net"));
        assert!(dropin.contains("Environment=supernode_port=3478"));
        assert!(dropin.contains("Environment=supernode_signaling_port=34935"));
        assert!(dropin.contains("Environment=supernode_web_port=8443"));
    }

    #[test]
    fn two_instances_get_distinct_dropin_paths() {
        assert_ne!(super::unit_dropin_path("a"), super::unit_dropin_path("b"));
    }
}
