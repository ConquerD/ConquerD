use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::selector::Selector;
use crate::supernode_config::{default_instance_features, FeatureSpec, SupernodeDefaults};

pub const DEFAULT_INVENTORY_PATH: &str = "inventory.toml";

/// Resolve `inventory.toml` from CWD, then by walking up from the executable.
pub fn resolve_inventory_path(requested: PathBuf) -> PathBuf {
    if requested.is_absolute() && requested.exists() {
        return requested;
    }
    if requested.exists() {
        return requested;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(mut dir) = exe.parent().map(|p| p.to_path_buf()) {
            for _ in 0..8 {
                let candidate = dir.join(&requested);
                if candidate.exists() {
                    return candidate;
                }
                if !dir.pop() {
                    break;
                }
            }
        }
    }
    requested
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inventory {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub host: Vec<Host>,
    /// Optional cluster definitions grouping instances into logical supernodes.
    #[serde(default, rename = "cluster", skip_serializing_if = "Vec::is_empty")]
    pub clusters: Vec<ClusterDef>,
}

/// A cluster declaration: a named group of `host/instance` members that form
/// one logical supernode.  The manager uses this to render `[cluster]` sections
/// in every member's `supernode.toml` and to apply restricted firewall rules.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterDef {
    /// Stable identifier shared by every member (becomes `cluster_id` in the manifest).
    pub id: String,
    /// Member keys in `"hostname/instance_id"` format, e.g. `"acdc/a"`.
    pub members: Vec<String>,
    /// Base UDP port for the supernode↔supernode QUIC link.
    /// Per-instance port = `cluster_port + 100 * instance_index`.
    /// Defaults to `4478` (relay + 1000) when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_port: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defaults {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_access_mode")]
    pub access_mode: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_install_root")]
    pub install_root: String,
    #[serde(default = "default_data_root")]
    pub data_root: String,
    /// GitHub `owner/repo` that publishes conquerd-supernode release assets.
    #[serde(default = "default_release_repo")]
    pub release_repo: String,
    #[serde(default = "default_privilege")]
    pub privilege: PrivilegeMode,
    /// `ufw` (default), `off`, or `report` (print required ports only).
    #[serde(default = "default_firewall")]
    pub firewall: FirewallMode,
    /// Local path to `conquerd-supernode` binary when `version = "local"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<PathBuf>,
    /// Local path to the `conquerd-supernode` Cargo package for `build-deploy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_source: Option<PathBuf>,
    /// Rust target triple for cross-compilation (e.g. `"x86_64-unknown-linux-musl"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_target: Option<String>,
    /// Build front-end: `"cargo"` (default), `"zigbuild"`, or `"cross"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_tool: Option<String>,
    /// Supernode manifest defaults applied to new instances unless overridden.
    #[serde(default)]
    pub supernode: SupernodeDefaults,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FirewallMode {
    Off,
    Ufw,
    Report,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivilegeMode {
    Sudo,
    RootlessSystemd,
    Root,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub ssh: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    #[serde(default, rename = "instance")]
    pub instances: Vec<Instance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub public_host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws_port: Option<u16>,
    /// Override `[defaults.supernode].listen_bind` for this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_bind: Option<String>,
    /// Override `[defaults].access_mode` for this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_mode: Option<String>,
    /// Override `[defaults.supernode].identity_file` for this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    /// Override fleet default: allow public SFU room creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_public_rooms: Option<bool>,
    /// Override fleet default: allow private SFU room creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_private_rooms: Option<bool>,
    /// UDP port for the supernode↔supernode cluster QUIC link.
    /// Only required when this instance is part of a `[[cluster]]`.
    /// Auto-allocated as `4478 + 100 * index` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_port: Option<u16>,
    #[serde(
        default,
        deserialize_with = "crate::supernode_config::deserialize_feature_list"
    )]
    pub features: Vec<FeatureSpec>,
}

#[derive(Debug, Clone)]
pub struct ResolvedInstance<'a> {
    pub host: &'a Host,
    pub instance: &'a Instance,
    pub defaults: &'a Defaults,
    pub relay_port: u16,
    pub ws_port: u16,
    /// Resolved cluster QUIC port (only meaningful when the instance is in a cluster).
    pub cluster_port: u16,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            version: default_version(),
            access_mode: default_access_mode(),
            user: default_user(),
            install_root: default_install_root(),
            data_root: default_data_root(),
            release_repo: default_release_repo(),
            privilege: default_privilege(),
            firewall: default_firewall(),
            binary_path: None,
            build_source: None,
            build_target: None,
            build_tool: None,
            supernode: SupernodeDefaults::default(),
        }
    }
}

impl Default for FirewallMode {
    fn default() -> Self {
        Self::Ufw
    }
}

fn default_version() -> String {
    "nightly".into()
}

fn default_access_mode() -> String {
    "open".into()
}

fn default_user() -> String {
    "conquerd".into()
}

fn default_install_root() -> String {
    "/opt/conquerd".into()
}

fn default_data_root() -> String {
    "/var/lib/conquerd".into()
}

fn default_release_repo() -> String {
    "ConquerD/ConquerD".into()
}

fn default_privilege() -> PrivilegeMode {
    PrivilegeMode::Sudo
}

fn default_firewall() -> FirewallMode {
    FirewallMode::Ufw
}

impl Inventory {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read inventory {}", path.display()))?;
        let inv: Inventory =
            toml::from_str(&raw).with_context(|| format!("parse inventory {}", path.display()))?;
        inv.validate()?;
        Ok(inv)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let raw = toml::to_string_pretty(self).context("serialize inventory")?;
        std::fs::write(path, raw).with_context(|| format!("write inventory {}", path.display()))?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        for host in &self.host {
            if host.name.trim().is_empty() {
                bail!("host name must not be empty");
            }
            if host.ssh.trim().is_empty() {
                bail!("host {} is missing ssh target", host.name);
            }
            if host.instances.is_empty() {
                bail!(
                    "host {} must contain at least one [[host.instance]]",
                    host.name
                );
            }
            let mut seen = std::collections::HashSet::new();
            for inst in &host.instances {
                if inst.id.trim().is_empty() {
                    bail!("host {} has an instance with an empty id", host.name);
                }
                if inst.public_host.trim().is_empty() {
                    bail!(
                        "host {} instance {} is missing public_host",
                        host.name,
                        inst.id
                    );
                }
                if !seen.insert(inst.id.as_str()) {
                    bail!("host {} has duplicate instance id {}", host.name, inst.id);
                }
            }
        }
        for cluster in &self.clusters {
            if cluster.id.trim().is_empty() {
                bail!("cluster id must not be empty");
            }
            if cluster.members.is_empty() {
                bail!("cluster {} must have at least one member", cluster.id);
            }
            for key in &cluster.members {
                if key.split_once('/').is_none() {
                    bail!(
                        "cluster {} member {key:?} is not in 'hostname/instance_id' format",
                        cluster.id
                    );
                }
            }
        }
        Ok(())
    }

    pub fn resolve_instances<'a>(
        &'a self,
        selector: &Selector,
    ) -> Result<Vec<ResolvedInstance<'a>>> {
        let mut out = Vec::new();
        for host in &self.host {
            if let Some(name) = &selector.host {
                if host.name != *name {
                    continue;
                }
            }
            for (index, instance) in host.instances.iter().enumerate() {
                if let Some(id) = &selector.instance {
                    if instance.id != *id {
                        continue;
                    }
                }
                let relay_port = instance
                    .relay_port
                    .unwrap_or_else(|| default_relay_port(index));
                let ws_port = instance.ws_port.unwrap_or_else(|| default_ws_port(index));
                let cluster_port = instance
                    .cluster_port
                    .unwrap_or_else(|| default_cluster_port(index));
                out.push(ResolvedInstance {
                    host,
                    instance,
                    defaults: &self.defaults,
                    relay_port,
                    ws_port,
                    cluster_port,
                });
            }
        }
        if out.is_empty() {
            bail!("no instances matched selector {:?}", selector);
        }
        Ok(out)
    }

    /// Resolve all `ResolvedInstance`s that are members of the given cluster.
    ///
    /// Each member key has the form `"hostname/instance_id"`.  Returns an error
    /// if any key does not resolve to a known host+instance pair.
    pub fn resolve_cluster_members<'a>(
        &'a self,
        cluster: &ClusterDef,
    ) -> Result<Vec<ResolvedInstance<'a>>> {
        let mut out = Vec::new();
        for key in &cluster.members {
            let (host_name, inst_id) = key.split_once('/').ok_or_else(|| {
                anyhow::anyhow!("cluster member key {key:?} must be 'hostname/instance_id'")
            })?;
            let host = self
                .host
                .iter()
                .find(|h| h.name == host_name)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cluster member {key}: host {host_name:?} not found in inventory"
                    )
                })?;
            let (index, instance) = host
                .instances
                .iter()
                .enumerate()
                .find(|(_, i)| i.id == inst_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "cluster member {key}: instance {inst_id:?} not found on host {host_name}"
                    )
                })?;
            let relay_port = instance
                .relay_port
                .unwrap_or_else(|| default_relay_port(index));
            let ws_port = instance.ws_port.unwrap_or_else(|| default_ws_port(index));
            // Use the cluster's base port if the instance doesn't pin its own.
            let cluster_port = instance
                .cluster_port
                .unwrap_or_else(|| cluster.cluster_port.unwrap_or(4478) + (index as u16) * 100);
            out.push(ResolvedInstance {
                host,
                instance,
                defaults: &self.defaults,
                relay_port,
                ws_port,
                cluster_port,
            });
        }
        Ok(out)
    }

    /// Return the cluster(s) that contain `"hostname/instance_id"`.
    pub fn clusters_for_instance(&self, host_name: &str, instance_id: &str) -> Vec<&ClusterDef> {
        let key = format!("{host_name}/{instance_id}");
        self.clusters
            .iter()
            .filter(|c| c.members.contains(&key))
            .collect()
    }
}

pub fn default_relay_port(index: usize) -> u16 {
    3478 + (index as u16) * 100
}

pub fn default_ws_port(index: usize) -> u16 {
    34935 + (index as u16) * 100
}

pub fn default_cluster_port(index: usize) -> u16 {
    4478 + (index as u16) * 100
}

impl Inventory {
    pub fn instance_count(&self) -> usize {
        self.host.iter().map(|h| h.instances.len()).sum()
    }

    /// Add an instance to an existing host or create a new host entry.
    pub fn push_instance(&mut self, host_name: &str, ssh: &str, instance: Instance) -> Result<()> {
        if let Some(host) = self.host.iter_mut().find(|h| h.name == host_name) {
            if host.instances.iter().any(|i| i.id == instance.id) {
                bail!("host {} already has instance {}", host_name, instance.id);
            }
            if host.ssh != ssh {
                bail!(
                    "host {} already exists with ssh {}; not overwriting",
                    host_name,
                    host.ssh
                );
            }
            host.instances.push(instance);
        } else {
            self.host.push(Host {
                name: host_name.into(),
                ssh: ssh.into(),
                arch: None,
                instances: vec![instance],
            });
        }
        self.validate()?;
        Ok(())
    }

    /// Update an existing instance. Can change host name, SSH target, instance id, and ports.
    /// Preserves the instance's feature list.
    pub fn update_instance(
        &mut self,
        old_host_name: &str,
        old_instance_id: &str,
        new_host_name: &str,
        new_ssh: &str,
        mut updated: Instance,
    ) -> Result<()> {
        let host_idx = self
            .host
            .iter()
            .position(|h| h.name == old_host_name)
            .ok_or_else(|| anyhow::anyhow!("host {old_host_name} not found"))?;
        let inst_idx = self.host[host_idx]
            .instances
            .iter()
            .position(|i| i.id == old_instance_id)
            .ok_or_else(|| {
                anyhow::anyhow!("instance {old_instance_id} not found on {old_host_name}")
            })?;

        let existing = &self.host[host_idx].instances[inst_idx];
        if updated.features.is_empty() {
            updated.features = existing.features.clone();
        }
        if updated.listen_bind.is_none() {
            updated.listen_bind = existing.listen_bind.clone();
        }
        if updated.access_mode.is_none() {
            updated.access_mode = existing.access_mode.clone();
        }
        if updated.identity_file.is_none() {
            updated.identity_file = existing.identity_file.clone();
        }

        if new_host_name == old_host_name {
            if updated.id != old_instance_id
                && self.host[host_idx]
                    .instances
                    .iter()
                    .any(|i| i.id == updated.id)
            {
                bail!("host {new_host_name} already has instance {}", updated.id);
            }
            let host = &mut self.host[host_idx];
            host.ssh = new_ssh.into();
            host.instances[inst_idx] = updated;
        } else {
            if self.host.iter().any(|h| h.name == new_host_name) {
                let target = self
                    .host
                    .iter()
                    .find(|h| h.name == new_host_name)
                    .expect("checked existence");
                if target.ssh != new_ssh {
                    bail!(
                        "host {new_host_name} already exists with ssh {}; not overwriting",
                        target.ssh
                    );
                }
                if target.instances.iter().any(|i| i.id == updated.id) {
                    bail!("host {new_host_name} already has instance {}", updated.id);
                }
            }
            self.remove_instance(old_host_name, old_instance_id)?;
            self.push_instance(new_host_name, new_ssh, updated)?;
        }

        self.validate()?;
        Ok(())
    }

    /// Remove an instance from the inventory. Drops the host entry if it has no instances left.
    pub fn remove_instance(&mut self, host_name: &str, instance_id: &str) -> Result<()> {
        let host_idx = self
            .host
            .iter()
            .position(|h| h.name == host_name)
            .ok_or_else(|| anyhow::anyhow!("host {host_name} not found"))?;
        let host = &mut self.host[host_idx];
        let inst_idx = host
            .instances
            .iter()
            .position(|i| i.id == instance_id)
            .ok_or_else(|| anyhow::anyhow!("instance {instance_id} not found on {host_name}"))?;
        host.instances.remove(inst_idx);
        if host.instances.is_empty() {
            self.host.remove(host_idx);
        }
        self.validate()?;
        Ok(())
    }
}

pub fn scaffold_inventory() -> Inventory {
    Inventory {
        defaults: Defaults::default(),
        host: vec![Host {
            name: "edge-1".into(),
            ssh: "conquerd@203.0.113.10".into(),
            arch: None,
            instances: vec![Instance {
                id: "a".into(),
                public_host: "edge1.example.net".into(),
                relay_port: Some(3478),
                ws_port: Some(34935),
                listen_bind: None,
                access_mode: None,
                identity_file: None,
                allow_public_rooms: None,
                allow_private_rooms: None,
                cluster_port: None,
                features: default_instance_features(),
            }],
        }],
        clusters: Vec::new(),
    }
}

pub fn scaffold_secrets_template() -> &'static str {
    r#"# secrets.toml — keep out of version control
#
# [access_codes]
# edge-1-a = "your-access-code"
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_example_inventory() {
        let raw = r#"
[defaults]
version = "1.0.0"

[[host]]
name = "edge-fra-1"
ssh = "conquerd@203.0.113.10"

  [[host.instance]]
  id = "a"
  public_host = "fra1.example.net"
  features = ["core.chat.v1"]
"#;
        let inv: Inventory = toml::from_str(raw).unwrap();
        inv.validate().unwrap();
        assert_eq!(inv.host[0].instances[0].relay_port, None);
    }

    #[test]
    fn defaults_firewall_to_ufw() {
        let inv: Inventory = toml::from_str(
            r#"
[[host]]
name = "edge-1"
ssh = "root@1.2.3.4"

  [[host.instance]]
  id = "a"
  public_host = "h.example.net"
"#,
        )
        .unwrap();
        assert_eq!(inv.defaults.firewall, FirewallMode::Ufw);
    }

    #[test]
    fn parses_firewall_mode() {
        let inv: Inventory = toml::from_str(
            r#"
[defaults]
firewall = "off"

[[host]]
name = "edge-1"
ssh = "root@1.2.3.4"

  [[host.instance]]
  id = "a"
  public_host = "h.example.net"
"#,
        )
        .unwrap();
        assert_eq!(inv.defaults.firewall, FirewallMode::Off);
    }

    #[test]
    fn auto_allocates_ports() {
        let inv = scaffold_inventory();
        let selector = Selector::default();
        let resolved = inv.resolve_instances(&selector).unwrap();
        assert_eq!(resolved[0].relay_port, 3478);
        assert_eq!(resolved[0].ws_port, 34935);
    }

    #[test]
    fn rejects_duplicate_instance_ids() {
        let raw = r#"
[[host]]
name = "edge-1"
ssh = "root@1.2.3.4"

  [[host.instance]]
  id = "a"
  public_host = "h.example.net"

  [[host.instance]]
  id = "a"
  public_host = "h.example.net"
"#;
        let inv: Inventory = toml::from_str(raw).unwrap();
        assert!(inv.validate().is_err());
    }

    #[test]
    fn resolve_inventory_falls_back_to_cwd() {
        let path = resolve_inventory_path(PathBuf::from(DEFAULT_INVENTORY_PATH));
        if PathBuf::from(DEFAULT_INVENTORY_PATH).exists() {
            assert!(path.exists());
        }
    }

    #[test]
    fn remove_instance_drops_empty_host() {
        let mut inv = scaffold_inventory();
        inv.remove_instance("edge-1", "a").unwrap();
        assert!(inv.host.is_empty());
        assert_eq!(inv.instance_count(), 0);
    }

    #[test]
    fn push_instance_adds_host_and_instance() {
        let mut inv = scaffold_inventory();
        inv.push_instance(
            "edge-2",
            "root@198.51.100.7",
            Instance {
                id: "a".into(),
                public_host: "edge2.example.net".into(),
                relay_port: Some(3578),
                ws_port: Some(35935),
                cluster_port: None,
                listen_bind: None,
                access_mode: None,
                identity_file: None,
                allow_public_rooms: None,
                allow_private_rooms: None,
                features: default_instance_features(),
            },
        )
        .unwrap();
        assert_eq!(inv.instance_count(), 2);
    }

    #[test]
    fn selector_filters_host_and_instance() {
        let inv = scaffold_inventory();
        let selector = Selector::from_flags(Some("edge-1".into()), Some("a".into()), false);
        let resolved = inv.resolve_instances(&selector).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].instance.id, "a");
    }

    #[test]
    fn update_instance_changes_fields_in_place() {
        let mut inv = scaffold_inventory();
        inv.update_instance(
            "edge-1",
            "a",
            "edge-1",
            "conquerd@203.0.113.99",
            Instance {
                id: "a".into(),
                public_host: "edge1-new.example.net".into(),
                relay_port: Some(3479),
                ws_port: Some(34936),
                cluster_port: None,
                listen_bind: None,
                access_mode: None,
                identity_file: None,
                allow_public_rooms: None,
                allow_private_rooms: None,
                features: vec![],
            },
        )
        .unwrap();
        assert_eq!(inv.host[0].ssh, "conquerd@203.0.113.99");
        assert_eq!(
            inv.host[0].instances[0].public_host,
            "edge1-new.example.net"
        );
        assert_eq!(inv.host[0].instances[0].relay_port, Some(3479));
        assert_eq!(
            inv.host[0].instances[0].features,
            default_instance_features()
        );
    }

    #[test]
    fn update_instance_moves_to_new_host() {
        let mut inv = scaffold_inventory();
        inv.update_instance(
            "edge-1",
            "a",
            "edge-2",
            "root@198.51.100.7",
            Instance {
                id: "b".into(),
                public_host: "edge2.example.net".into(),
                relay_port: Some(3578),
                ws_port: Some(35935),
                cluster_port: None,
                listen_bind: None,
                access_mode: None,
                identity_file: None,
                allow_public_rooms: None,
                allow_private_rooms: None,
                features: vec![],
            },
        )
        .unwrap();
        assert_eq!(inv.host.len(), 1);
        assert_eq!(inv.host[0].name, "edge-2");
        assert_eq!(inv.host[0].instances[0].id, "b");
    }
}
