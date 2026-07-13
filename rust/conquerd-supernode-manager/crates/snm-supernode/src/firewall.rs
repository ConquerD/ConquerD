use anyhow::{bail, Result};
use snm_core::FirewallMode;
use snm_transport::{shell_escape, SshTransport, Transport};

use crate::layout::NetworkEnv;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRule {
    pub port: u16,
    pub proto: &'static str,
    pub tag: &'static str,
}

pub fn port_rules(network: &NetworkEnv) -> Vec<PortRule> {
    let mut rules = vec![
        PortRule {
            port: network.relay_port,
            proto: "udp",
            tag: "relay",
        },
        PortRule {
            port: network.ws_port,
            proto: "tcp",
            tag: "ws",
        },
    ];
    if let Some(web_port) = network.web_port {
        rules.push(PortRule {
            port: web_port,
            proto: "udp",
            tag: "web-udp",
        });
        rules.push(PortRule {
            port: web_port,
            proto: "tcp",
            tag: "web-tcp",
        });
    }
    rules
}

pub fn format_port_list(network: &NetworkEnv) -> String {
    port_rules(network)
        .iter()
        .map(|r| format!("{}/{}", r.port, r.proto))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn ufw_comment(host_name: &str, instance_id: &str, tag: &str) -> String {
    format!("snm:{host_name}/{instance_id}:{tag}")
}

pub fn render_ufw_ensure_script(
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    network: &NetworkEnv,
) -> String {
    let instance_needle = shell_escape(&format!("snm:{host_name}/{instance_id}"));
    let mut lines = vec![
        "set -e".into(),
        format!(
            "if ! command -v ufw >/dev/null 2>&1; then echo 'ufw not installed; open ports: {}'; exit 0; fi",
            format_port_list(network)
        ),
        // Remove any stale rules for this instance (e.g. from a previous
        // install when ports were different) before adding fresh ones.
        format!(
            "{prefix}ufw status numbered 2>/dev/null | grep -F {instance_needle} | sed -E 's/^\\[ *([0-9]+)\\].*/\\1/' | sort -rn | while read -r n; do [ -n \"$n\" ] && {prefix}ufw --force delete \"$n\"; done"
        ),
    ];

    for rule in port_rules(network) {
        let comment = ufw_comment(host_name, instance_id, rule.tag);
        let comment_escaped = shell_escape(&comment);
        lines.push(format!(
            "{prefix}ufw allow {}/{} comment {comment_escaped}",
            rule.port, rule.proto
        ));
    }

    lines.push(format!(
        "if ! {prefix}ufw status 2>/dev/null | grep -Fq 'Status: active'; then echo 'ufw rules added but firewall is inactive — run: {prefix}ufw enable'; fi"
    ));

    lines.join("\n")
}

pub fn render_ufw_remove_script(prefix: &str, host_name: &str, instance_id: &str) -> String {
    let needle = shell_escape(&format!("snm:{host_name}/{instance_id}"));
    format!(
        r#"set -e
if ! command -v ufw >/dev/null 2>&1; then exit 0; fi
{prefix}ufw status numbered 2>/dev/null | grep -F {needle} | sed -E 's/^\[ *([0-9]+)\].*/\1/' | sort -rn | while read -r n; do
  [ -n "$n" ] && {prefix}ufw --force delete "$n"
done
"#
    )
}

pub async fn apply_firewall_on_install(
    transport: &SshTransport,
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    network: &NetworkEnv,
    mode: FirewallMode,
    label: &str,
) -> Result<()> {
    for line in apply_firewall_on_install_report(
        transport,
        prefix,
        host_name,
        instance_id,
        network,
        mode,
        label,
    )
    .await?
    {
        println!("{line}");
    }
    Ok(())
}

pub async fn apply_firewall_on_install_report(
    transport: &SshTransport,
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    network: &NetworkEnv,
    mode: FirewallMode,
    label: &str,
) -> Result<Vec<String>> {
    let mut report = Vec::new();
    match mode {
        FirewallMode::Off => {}
        FirewallMode::Report => {
            report.push(format!(
                "{label}: open firewall ports: {}",
                format_port_list(network)
            ));
        }
        FirewallMode::Ufw => {
            let script = render_ufw_ensure_script(prefix, host_name, instance_id, network);
            let output = transport.run(&script).await?;
            let stdout = output.stdout.trim();
            if !stdout.is_empty() {
                for line in stdout.lines() {
                    report.push(format!("{label}: {line}"));
                }
            }
            if output.exit_code != 0 {
                bail!("ufw setup failed for {label}: {}", output.stderr.trim());
            }
            report.push(format!(
                "{label}: ufw rules ensured ({})",
                format_port_list(network)
            ));
        }
    }
    Ok(report)
}

pub async fn apply_firewall_on_uninstall(
    transport: &SshTransport,
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    mode: FirewallMode,
    label: &str,
) -> Result<()> {
    for line in
        apply_firewall_on_uninstall_report(transport, prefix, host_name, instance_id, mode, label)
            .await?
    {
        println!("{line}");
    }
    Ok(())
}

pub async fn apply_firewall_on_uninstall_report(
    transport: &SshTransport,
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    mode: FirewallMode,
    label: &str,
) -> Result<Vec<String>> {
    if !matches!(mode, FirewallMode::Ufw) {
        return Ok(Vec::new());
    }

    let mut report = Vec::new();
    let script = render_ufw_remove_script(prefix, host_name, instance_id);
    let output = transport.run(&script).await?;
    if output.exit_code != 0 {
        report.push(format!(
            "{label}: warning: failed to remove ufw rules: {}",
            output.stderr.trim()
        ));
    } else {
        report.push(format!(
            "{label}: removed ufw rules tagged snm:{host_name}/{instance_id}:*"
        ));
    }
    Ok(report)
}

/// Add a cluster-port ufw rule restricted to specific source IPs (member peers only).
///
/// Runs on the *host* that owns `instance_id`; `peer_ips` are the `public_host`
/// values of the *other* members.  Each rule is tagged so it can be removed on
/// uninstall.
pub async fn apply_cluster_firewall_report(
    transport: &SshTransport,
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    cluster_port: u16,
    peer_ips: &[String],
    mode: FirewallMode,
    label: &str,
) -> Result<Vec<String>> {
    let mut report = Vec::new();
    match mode {
        FirewallMode::Off => {}
        FirewallMode::Report => {
            report.push(format!(
                "{label}: cluster port {cluster_port}/udp restricted to: {}",
                peer_ips.join(", ")
            ));
        }
        FirewallMode::Ufw => {
            let script =
                render_cluster_ufw_script(prefix, host_name, instance_id, cluster_port, peer_ips);
            let output = transport.run(&script).await?;
            let stdout = output.stdout.trim();
            if !stdout.is_empty() {
                for line in stdout.lines() {
                    report.push(format!("{label}: {line}"));
                }
            }
            if output.exit_code != 0 {
                bail!("cluster ufw setup failed for {label}: {}", output.stderr.trim());
            }
            report.push(format!(
                "{label}: cluster ufw rules ensured (port {cluster_port}/udp from {} peers)",
                peer_ips.len()
            ));
        }
    }
    Ok(report)
}

/// Render a ufw script that opens `cluster_port/udp` **only** from each `peer_ip`.
pub fn render_cluster_ufw_script(
    prefix: &str,
    host_name: &str,
    instance_id: &str,
    cluster_port: u16,
    peer_ips: &[String],
) -> String {
    let mut lines = vec!["set -e".into()];
    if peer_ips.is_empty() {
        return lines.join("\n");
    }
    for ip in peer_ips {
        let comment = ufw_comment(host_name, instance_id, "cluster");
        let comment_escaped = shell_escape(&comment);
        let ip_escaped = shell_escape(ip);
        lines.push(format!(
            "if ! {prefix}ufw status verbose 2>/dev/null | grep -Fq {comment_escaped}; then \
             {prefix}ufw allow from {ip_escaped} to any port {cluster_port} proto udp comment {comment_escaped}; fi"
        ));
    }
    lines.join("\n")
}


#[cfg(test)]
mod tests {
    use super::*;

    fn sample_network() -> NetworkEnv {
        NetworkEnv {
            relay_port: 3578,
            ws_port: 35035,
            web_port: None, // no public web port (portal is QUIC-only)
            public_host: "155.138.244.189".into(),
            access_mode: "open".into(),
        }
    }

    #[test]
    fn lists_required_ports() {
        let rules = port_rules(&sample_network());
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].port, 3578);
        assert_eq!(rules[0].proto, "udp");
        assert_eq!(rules[1].port, 35035);
        assert_eq!(rules[1].proto, "tcp");
    }

    #[test]
    fn ensure_script_is_idempotent_and_tagged() {
        let script = render_ufw_ensure_script("", "acdc", "a", &sample_network());
        // Stale instance rules are wiped by comment needle, then re-added.
        assert!(script.contains("grep -F 'snm:acdc/a'"));
        assert!(script.contains("ufw allow 3578/udp comment 'snm:acdc/a:relay'"));
        assert!(!script.contains("web-tcp"));
        assert!(!script.contains("web-udp"));
    }

    #[test]
    fn remove_script_deletes_numbered_rules() {
        let script = render_ufw_remove_script("sudo ", "acdc", "a");
        assert!(script.contains("grep -F 'snm:acdc/a'"));
        assert!(script.contains("sudo ufw --force delete"));
    }
}
