use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::manifest::{ClusterMemberEntry, ClusterRoster};

/// On-disk TOML cache written after each `cluster-sync`.
/// Lives next to `inventory.toml` as `cluster_cache.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ClusterCache {
    #[serde(default, rename = "cluster")]
    pub clusters: Vec<CachedRoster>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRoster {
    pub cluster_id: String,
    #[serde(default, rename = "member")]
    pub members: Vec<CachedMember>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedMember {
    pub identity_pub: String,
    pub relay_addr: String,
    pub cluster_addr: String,
    pub ws_addr: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_port: Option<u16>,
}

impl ClusterCache {
    /// Load the cache file. Returns an empty cache if the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read cluster cache {}", path.display()))?;
        toml::from_str(&raw)
            .with_context(|| format!("parse cluster cache {}", path.display()))
    }

    /// Persist the cache to disk, overwriting the existing file.
    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self).context("serialize cluster cache")?;
        std::fs::write(path, raw)
            .with_context(|| format!("write cluster cache {}", path.display()))
    }

    /// Insert or replace the entry for this roster's cluster_id.
    pub fn upsert(&mut self, roster: &ClusterRoster) {
        let entry = CachedRoster {
            cluster_id: roster.cluster_id.clone(),
            members: roster
                .members
                .iter()
                .map(|m| CachedMember {
                    identity_pub: m.identity_pub.clone(),
                    relay_addr: m.relay_addr.clone(),
                    cluster_addr: m.cluster_addr.clone(),
                    ws_addr: m.ws_addr.clone(),
                    web_port: m.web_port,
                })
                .collect(),
        };
        match self
            .clusters
            .iter_mut()
            .find(|c| c.cluster_id == roster.cluster_id)
        {
            Some(existing) => *existing = entry,
            None => self.clusters.push(entry),
        }
    }

    /// Return the `ClusterRoster` for `cluster_id`, or `None` if not cached.
    pub fn find_roster(&self, cluster_id: &str) -> Option<ClusterRoster> {
        self.clusters
            .iter()
            .find(|c| c.cluster_id == cluster_id)
            .map(|c| ClusterRoster {
                cluster_id: c.cluster_id.clone(),
                members: c
                    .members
                    .iter()
                    .map(|m| ClusterMemberEntry {
                        identity_pub: m.identity_pub.clone(),
                        relay_addr: m.relay_addr.clone(),
                        cluster_addr: m.cluster_addr.clone(),
                        ws_addr: m.ws_addr.clone(),
                        web_port: m.web_port,
                    })
                    .collect(),
            })
    }
}
