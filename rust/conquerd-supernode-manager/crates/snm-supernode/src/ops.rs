use std::path::Path;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use snm_core::{resolve_supernode_config, ClusterDef, Inventory, PrivilegeMode, ResolvedInstance};
use snm_transport::{
    shell_escape, upload_local_file, RemoteOutput, SshBackend, SshTransport, Transport,
    TransportError,
};

use crate::binary_probe::{
    binary_probe_command, format_pinned_version_display, parse_binary_probe_output,
};
use crate::cluster_cache::ClusterCache;
use crate::firewall::{
    apply_cluster_firewall_report, apply_firewall_on_install_report,
    apply_firewall_on_uninstall_report,
};
use crate::invite::{collect_identity_pub, fetch_invite, InviteInfo};
use crate::layout::{
    instance_label, journalctl_command, privilege_prefix, systemctl_command, InstanceLayout,
    NetworkEnv,
};
use crate::manifest::{render_supernode_toml_with_cluster, ClusterMemberEntry, ClusterRoster};
use crate::release::{download_supernode_artifact, DownloadedSupernode};
use crate::systemd::{
    render_unit_dropin, render_unit_template, unit_dropin_dir, unit_dropin_path, unit_template_path,
};

#[derive(Debug, Clone)]
pub struct HostProbe {
    pub os: String,
    pub arch: String,
    pub platform: String,
}

pub async fn connect_host(transport: &SshTransport) -> Result<()> {
    run_checked(transport, "true").await?;
    Ok(())
}

pub async fn ping_host(transport: &SshTransport) -> Result<HostProbe> {
    let os = run_checked(transport, "uname -s")
        .await?
        .stdout
        .trim()
        .to_string();
    let arch = run_checked(transport, "uname -m")
        .await?
        .stdout
        .trim()
        .to_string();
    let platform = map_platform(&os, &arch)?;
    Ok(HostProbe { os, arch, platform })
}

pub async fn install_instance(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    local_binary: &Path,
    cluster_roster: Option<&ClusterRoster>,
) -> Result<()> {
    print_report(install_instance_report(transport, resolved, local_binary, cluster_roster).await?);
    Ok(())
}

pub async fn install_instance_report(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    local_binary: &Path,
    cluster_roster: Option<&ClusterRoster>,
) -> Result<Vec<String>> {
    if resolved.defaults.privilege == PrivilegeMode::RootlessSystemd {
        bail!("rootless-systemd install is not implemented in the prototype");
    }

    let mut report = Vec::new();
    let layout = InstanceLayout::from_resolved(resolved);
    let network = NetworkEnv::from_resolved(resolved);
    let label = instance_label(&resolved.host.name, resolved.instance);
    let prefix = privilege_prefix(resolved.defaults.privilege);

    ensure_service_user(transport, prefix, &layout.service_user).await?;
    ensure_directories(transport, prefix, &layout).await?;

    let remote_binary = &layout.versioned_binary;
    let staging_binary = format!("{remote_binary}.snm-staging");
    upload_local_file(transport, local_binary, &staging_binary, 0o755)
        .await
        .with_context(|| format!("upload binary to {staging_binary}"))?;
    let promote_binary = format!(
        "{prefix}mv -f {} {}",
        shell_escape(&staging_binary),
        shell_escape(remote_binary)
    );
    run_checked(transport, &promote_binary)
        .await
        .context("promote staged binary")?;

    let link_cmd = format!(
        "{prefix}ln -sfn {} {}",
        shell_escape(remote_binary),
        shell_escape(&layout.current_binary_link)
    );
    run_checked(transport, &link_cmd).await?;

    let config = resolve_supernode_config(
        resolved.defaults,
        resolved.instance,
        network.relay_port,
        network.ws_port,
    );
    let manifest = render_supernode_toml_with_cluster(&config, cluster_roster);
    upload_text_file(transport, prefix, &layout.manifest_path, &manifest, 0o644)
        .await
        .context("upload supernode.toml")?;

    let unit = render_unit_template(&layout, resolved.defaults);
    upload_text_file(transport, prefix, unit_template_path(), &unit, 0o644)
        .await
        .context("upload systemd unit template")?;

    let dropin = render_unit_dropin(&layout, &network);
    let dropin_path = unit_dropin_path(&layout.instance_id);
    upload_text_file(transport, prefix, &dropin_path, &dropin, 0o644)
        .await
        .context("upload systemd instance drop-in")?;

    run_checked(transport, &format!("{prefix}systemctl daemon-reload")).await?;

    report.extend(
        apply_firewall_on_install_report(
            transport,
            prefix,
            &resolved.host.name,
            &resolved.instance.id,
            &network,
            resolved.defaults.firewall,
            &label,
        )
        .await?,
    );

    run_checked(
        transport,
        &systemctl_command(resolved.defaults, &format!("enable {}", layout.unit_name)),
    )
    .await?;
    run_checked(
        transport,
        &systemctl_command(resolved.defaults, &format!("restart {}", layout.unit_name)),
    )
    .await?;

    report.push(format!("installed {label}"));
    Ok(report)
}

pub async fn resolve_install_binary(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
) -> Result<DownloadedSupernode> {
    if resolved.defaults.version == "local" {
        let path = resolved
            .defaults
            .binary_path
            .clone()
            .context("defaults.binary_path is required when version = \"local\"")?;
        return Ok(DownloadedSupernode {
            binary_path: path,
            artifact: crate::release::ReleaseArtifact {
                version: "local".into(),
                platform: "local".into(),
                asset_name: "local".into(),
                asset_url: "local".into(),
                sha256_url: "local".into(),
            },
        });
    }

    let platform = if let Some(platform) = &resolved.host.arch {
        platform.clone()
    } else {
        ping_host(transport).await?.platform
    };
    download_supernode_artifact(resolved.defaults, &platform).await
}

pub async fn push_config_instance(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    restart: bool,
    cluster_roster: Option<&ClusterRoster>,
) -> Result<()> {
    print_report(push_config_instance_report(transport, resolved, restart, cluster_roster).await?);
    Ok(())
}

pub async fn push_config_instance_report(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    restart: bool,
    cluster_roster: Option<&ClusterRoster>,
) -> Result<Vec<String>> {
    let layout = InstanceLayout::from_resolved(resolved);
    let network = NetworkEnv::from_resolved(resolved);
    let label = instance_label(&resolved.host.name, resolved.instance);
    let prefix = privilege_prefix(resolved.defaults.privilege);

    let config = resolve_supernode_config(
        resolved.defaults,
        resolved.instance,
        network.relay_port,
        network.ws_port,
    );
    let manifest = render_supernode_toml_with_cluster(&config, cluster_roster);
    upload_text_file(transport, prefix, &layout.manifest_path, &manifest, 0o644)
        .await
        .context("upload supernode.toml")?;

    let dropin = render_unit_dropin(&layout, &network);
    upload_text_file(
        transport,
        prefix,
        &unit_dropin_path(&layout.instance_id),
        &dropin,
        0o644,
    )
    .await
    .context("upload systemd instance drop-in")?;

    run_checked(transport, &format!("{prefix}systemctl daemon-reload")).await?;

    if restart {
        run_checked(
            transport,
            &systemctl_command(resolved.defaults, &format!("restart {}", layout.unit_name)),
        )
        .await?;
        Ok(vec![format!("config pushed and restarted {label}")])
    } else {
        Ok(vec![format!(
            "config pushed {label} (restart required for changes to apply)"
        )])
    }
}

pub async fn lifecycle(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    action: LifecycleAction,
) -> Result<()> {
    print_report(lifecycle_report(transport, resolved, action).await?);
    Ok(())
}

pub async fn lifecycle_report(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    action: LifecycleAction,
) -> Result<Vec<String>> {
    let layout = InstanceLayout::from_resolved(resolved);
    let cmd = systemctl_command(
        resolved.defaults,
        &format!("{} {}", action.as_str(), layout.unit_name),
    );
    run_checked(transport, &cmd).await?;
    Ok(vec![format!(
        "{} {}/{}",
        action.as_str(),
        resolved.host.name,
        resolved.instance.id
    )])
}

pub async fn status_instance(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
) -> Result<InstanceStatus> {
    let layout = InstanceLayout::from_resolved(resolved);
    let active_cmd = systemctl_command(
        resolved.defaults,
        &format!("is-active {}", layout.unit_name),
    );
    let active = transport.run(&active_cmd).await?;
    let (active_running, systemd_state) = parse_systemctl_is_active(&active)?;
    let probe_out = transport
        .run(&binary_probe_command(&layout.current_binary_link))
        .await?;
    let identity = parse_binary_probe_output(&probe_out.stdout);
    let pinned_version = resolved.defaults.version.clone();
    let binary_path = identity
        .path
        .clone()
        .unwrap_or_else(|| layout.current_binary_link.clone());
    Ok(InstanceStatus {
        label: instance_label(&resolved.host.name, resolved.instance),
        active: active_running,
        systemd_state,
        binary_path,
        pinned_version,
        binary_sha256: identity.sha256_short,
        binary_modified: identity.modified,
        build_id: identity.build_id,
    })
}

/// `systemctl is-active` uses exit 0 = active, 3 = not active, 4 = unit missing.
fn parse_systemctl_is_active(output: &RemoteOutput) -> Result<(bool, String)> {
    let state = output.stdout.trim().to_string();
    match output.exit_code {
        0 => Ok((true, state)),
        3 => Ok((false, state)),
        4 => Ok((
            false,
            if state.is_empty() {
                "not-installed".into()
            } else {
                state
            },
        )),
        exit => Err(TransportError::CommandFailed {
            exit,
            stderr: output.stderr.clone(),
            stdout: output.stdout.clone(),
        }
        .into()),
    }
}

pub async fn invite_instance(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
) -> Result<InviteInfo> {
    let layout = InstanceLayout::from_resolved(resolved);
    let label = instance_label(&resolved.host.name, resolved.instance);
    fetch_invite(transport, &layout, &label).await
}

pub async fn logs_instance(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    follow: bool,
    lines: u32,
) -> Result<()> {
    let text = logs_instance_text(transport, resolved, follow, lines).await?;
    print!("{text}");
    Ok(())
}

pub async fn uninstall_instance(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    purge: bool,
) -> Result<()> {
    print_report(uninstall_instance_report(transport, resolved, purge).await?);
    Ok(())
}

pub async fn uninstall_instance_report(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    purge: bool,
) -> Result<Vec<String>> {
    if resolved.defaults.privilege == PrivilegeMode::RootlessSystemd {
        bail!("rootless-systemd uninstall is not implemented in the prototype");
    }

    let mut report = Vec::new();
    let layout = InstanceLayout::from_resolved(resolved);
    let label = instance_label(&resolved.host.name, resolved.instance);
    let prefix = privilege_prefix(resolved.defaults.privilege);

    let stop = systemctl_command(resolved.defaults, &format!("stop {}", layout.unit_name));
    run_tolerant(transport, &stop).await?;
    let disable = systemctl_command(resolved.defaults, &format!("disable {}", layout.unit_name));
    run_tolerant(transport, &disable).await?;

    let dropin_dir = unit_dropin_dir(&layout.instance_id);
    let remove_dropin = format!("{prefix}rm -rf {}", shell_escape(&dropin_dir));
    run_tolerant(transport, &remove_dropin).await?;
    run_tolerant(transport, &format!("{prefix}systemctl daemon-reload")).await?;

    if purge {
        let cmd = format!("{prefix}rm -rf {}", shell_escape(&layout.data_dir));
        run_checked(transport, &cmd).await?;
    }

    report.extend(
        apply_firewall_on_uninstall_report(
            transport,
            prefix,
            &resolved.host.name,
            &resolved.instance.id,
            resolved.defaults.firewall,
            &label,
        )
        .await?,
    );

    report.push(format!(
        "uninstalled {label}{}",
        if purge { " (purged)" } else { "" }
    ));
    Ok(report)
}

pub async fn logs_instance_text(
    transport: &SshTransport,
    resolved: &ResolvedInstance<'_>,
    follow: bool,
    lines: u32,
) -> Result<String> {
    let layout = InstanceLayout::from_resolved(resolved);
    let cmd = journalctl_command(resolved.defaults, &layout.unit_name, follow, lines);
    let output = transport.run(&cmd).await?;
    if output.exit_code != 0 && !follow {
        bail!(
            "journalctl failed for {} (exit {}): {}",
            layout.unit_name,
            output.exit_code,
            output.stderr.trim()
        );
    }
    let mut text = output.stdout;
    if !output.stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&output.stderr);
    }
    Ok(text)
}

pub fn resolve_local_binary(inv: &Inventory) -> Result<std::path::PathBuf> {
    if inv.defaults.version == "local" {
        inv.defaults
            .binary_path
            .clone()
            .context("defaults.binary_path is required when version = \"local\"")
    } else {
        bail!(
            "prototype only supports version = \"local\"; set defaults.binary_path to a local conquerd-supernode binary"
        )
    }
}

async fn ensure_service_user(transport: &SshTransport, prefix: &str, user: &str) -> Result<()> {
    let cmd = format!(
        "{prefix}id -u {} >/dev/null 2>&1 || {prefix}useradd --system --create-home --shell /usr/sbin/nologin {}",
        shell_escape(user),
        shell_escape(user)
    );
    let out = transport.run(&cmd).await?;
    if out.exit_code != 0 {
        bail!(
            "failed to ensure service user {user}: {}",
            out.stderr.trim()
        );
    }
    Ok(())
}

async fn ensure_directories(
    transport: &SshTransport,
    prefix: &str,
    layout: &InstanceLayout,
) -> Result<()> {
    let cmd = format!(
        "{prefix}install -d -o {} -g {} -m 0755 {} {} {}",
        shell_escape(&layout.service_user),
        shell_escape(&layout.service_user),
        shell_escape(&layout.binary_dir),
        shell_escape(&layout.data_dir),
        shell_escape(
            &layout
                .current_binary_link
                .rsplit_once('/')
                .map(|(p, _)| p)
                .unwrap_or(&layout.binary_dir)
        ),
    );
    run_checked(transport, &cmd).await?;
    Ok(())
}

async fn run_tolerant(transport: &SshTransport, command: &str) -> Result<()> {
    let _ = transport.run(command).await?;
    Ok(())
}

/// Push small text files over SSH (avoids flaky SFTP on some hosts).
async fn upload_text_file(
    transport: &SshTransport,
    prefix: &str,
    remote_path: &str,
    contents: &str,
    mode: u32,
) -> Result<()> {
    let parent = remote_path
        .rsplit_once('/')
        .map(|(p, _)| p)
        .filter(|p| !p.is_empty());
    let tmp = format!("{remote_path}.snm-upload");
    let encoded = BASE64.encode(contents.as_bytes());
    let script = if let Some(parent) = parent {
        format!(
            "mkdir -p {parent} && echo {encoded} | base64 -d > {tmp} && chmod {mode:o} {tmp} && mv -f {tmp} {remote_path}"
        )
    } else {
        format!(
            "echo {encoded} | base64 -d > {tmp} && chmod {mode:o} {tmp} && mv -f {tmp} {remote_path}"
        )
    };
    let cmd = format!("{prefix}bash -lc {}", shell_escape(&script));
    run_checked(transport, &cmd).await?;
    Ok(())
}

async fn run_checked(transport: &SshTransport, command: &str) -> Result<RemoteOutput> {
    let output = transport.run(command).await?;
    if output.exit_code != 0 {
        return Err(TransportError::CommandFailed {
            exit: output.exit_code,
            stderr: output.stderr,
            stdout: output.stdout,
        }
        .into());
    }
    Ok(output)
}

fn print_report(report: Vec<String>) {
    for line in report {
        println!("{line}");
    }
}

fn map_platform(os: &str, arch: &str) -> Result<String> {
    let os_key = match os {
        "Linux" => "linux",
        "Darwin" => "macos",
        other => bail!("unsupported remote OS: {other}"),
    };
    let arch_key = match arch {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => bail!("unsupported remote arch: {other}"),
    };
    Ok(format!("{os_key}-{arch_key}"))
}

#[derive(Debug, Clone, Copy)]
pub enum LifecycleAction {
    Start,
    Stop,
    Restart,
}

impl LifecycleAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstanceStatus {
    pub label: String,
    pub active: bool,
    pub systemd_state: String,
    pub binary_path: String,
    /// Inventory pin (`nightly`, `1.0.0`, `local`, …).
    pub pinned_version: String,
    /// First 12 hex chars of the running binary SHA-256.
    pub binary_sha256: Option<String>,
    /// Last modification timestamp of the running binary (from remote `stat`).
    pub binary_modified: Option<String>,
    /// Embedded `CONQUERD_BUILD_ID` when present in the binary.
    pub build_id: Option<String>,
}

impl InstanceStatus {
    /// Short label for fleet tables: `nightly@878696fc·06-14`.
    pub fn version_display(&self) -> String {
        format_pinned_version_display(
            &self.pinned_version,
            self.binary_sha256.as_deref(),
            self.binary_modified.as_deref(),
        )
    }

    /// Verbose status line for CLI output.
    pub fn version_detail(&self) -> String {
        let mut parts = vec![self.version_display()];
        if let Some(ref build_id) = self.build_id {
            parts.push(format!("build={build_id}"));
        }
        if let Some(ref modified) = self.binary_modified {
            parts.push(format!("mtime={modified}"));
        }
        parts.push(format!("bin={}", self.binary_path));
        parts.join(" ")
    }
}

/// Two-phase cluster provisioning for a single `ClusterDef`.
///
/// Phase 1: collect `identity_pub` from every member over SSH.
/// Phase 2: render the full `[cluster]` roster into every member's
///          `supernode.toml` and restart the service.
///
/// Returns a flat report of all operations performed.
pub async fn cluster_sync_report(
    inventory: &Inventory,
    cluster: &ClusterDef,
    backend: SshBackend,
    cache_path: &Path,
) -> Result<Vec<String>> {
    let members = inventory.resolve_cluster_members(cluster)?;
    if members.is_empty() {
        return Ok(vec![format!("cluster {}: no members to sync", cluster.id)]);
    }

    let mut report = Vec::new();

    // ---- Phase 1: collect identity keys ---------------------------------
    report.push(format!(
        "cluster {}: collecting identity keys …",
        cluster.id
    ));
    let mut identity_pubs: Vec<String> = Vec::with_capacity(members.len());
    for resolved in &members {
        let layout = InstanceLayout::from_resolved(resolved);
        let label = instance_label(&resolved.host.name, resolved.instance);
        let transport = SshTransport::new(&resolved.host.ssh, backend);
        let pub_key = collect_identity_pub(&transport, &layout)
            .await
            .with_context(|| {
                format!("collect identity for {label} (must have started at least once)")
            })?;
        report.push(format!(
            "  {label}: identity_pub={}",
            &pub_key[..pub_key.len().min(16)]
        ));
        identity_pubs.push(pub_key);
    }

    // ---- Build shared roster -------------------------------------------
    let roster_members: Vec<ClusterMemberEntry> = members
        .iter()
        .zip(identity_pubs.iter())
        .map(|(r, pub_key)| ClusterMemberEntry {
            identity_pub: pub_key.clone(),
            relay_addr: format!("{}:{}", r.instance.public_host, r.relay_port),
            cluster_addr: format!("{}:{}", r.instance.public_host, r.cluster_port),
            ws_addr: format!("{}:{}", r.instance.public_host, r.ws_port),
        })
        .collect();
    let roster = ClusterRoster {
        cluster_id: cluster.id.clone(),
        members: roster_members.clone(),
    };

    // Persist the roster locally so install/config-push can use it without
    // reading the remote file.
    let mut cache = ClusterCache::load(cache_path).unwrap_or_default();
    cache.upsert(&roster);
    cache
        .save(cache_path)
        .with_context(|| format!("save cluster cache to {}", cache_path.display()))?;

    // ---- Phase 2: push config + restart ---------------------------------
    report.push(format!(
        "cluster {}: pushing roster to {} members …",
        cluster.id,
        members.len()
    ));
    for (resolved, member_entry) in members.iter().zip(roster_members.iter()) {
        let layout = InstanceLayout::from_resolved(resolved);
        let network = NetworkEnv::from_resolved(resolved);
        let label = instance_label(&resolved.host.name, resolved.instance);
        let prefix = privilege_prefix(resolved.defaults.privilege);
        let transport = SshTransport::new(&resolved.host.ssh, backend);

        let config = resolve_supernode_config(
            resolved.defaults,
            resolved.instance,
            network.relay_port,
            network.ws_port,
        );
        let manifest = render_supernode_toml_with_cluster(&config, Some(&roster));
        upload_text_file(&transport, prefix, &layout.manifest_path, &manifest, 0o644)
            .await
            .with_context(|| format!("upload supernode.toml for {label}"))?;

        // Apply restricted cluster-port firewall rules (from peer IPs only).
        let peer_ips: Vec<String> = roster_members
            .iter()
            .filter(|m| m.identity_pub != member_entry.identity_pub)
            .map(|m| m.relay_addr.split(':').next().unwrap_or("").to_string())
            .filter(|s| !s.is_empty())
            .collect();
        report.extend(
            apply_cluster_firewall_report(
                &transport,
                prefix,
                &resolved.host.name,
                &resolved.instance.id,
                resolved.cluster_port,
                &peer_ips,
                resolved.defaults.firewall,
                &label,
            )
            .await?,
        );

        run_checked(
            &transport,
            &systemctl_command(resolved.defaults, &format!("restart {}", layout.unit_name)),
        )
        .await
        .with_context(|| format!("restart {label}"))?;

        report.push(format!("  {label}: config pushed and restarted"));
    }

    report.push(format!(
        "cluster {}: sync complete ({} members)",
        cluster.id,
        members.len()
    ));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(exit_code: i32, stdout: &str) -> RemoteOutput {
        RemoteOutput {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code,
        }
    }

    #[test]
    fn is_active_treats_exit_3_as_not_running() {
        let (active, state) = parse_systemctl_is_active(&output(3, "inactive\n")).unwrap();
        assert!(!active);
        assert_eq!(state, "inactive");
    }

    #[test]
    fn is_active_treats_exit_0_as_running() {
        let (active, state) = parse_systemctl_is_active(&output(0, "active\n")).unwrap();
        assert!(active);
        assert_eq!(state, "active");
    }

    #[test]
    fn is_active_treats_exit_4_as_missing_unit() {
        let (active, state) = parse_systemctl_is_active(&output(4, "")).unwrap();
        assert!(!active);
        assert_eq!(state, "not-installed");
    }
}
