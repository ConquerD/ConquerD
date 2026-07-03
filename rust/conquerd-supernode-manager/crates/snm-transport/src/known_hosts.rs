use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use home::home_dir;
use russh::keys::PublicKey;

pub struct KnownHosts {
    path: PathBuf,
    accepted_keys: HashSet<String>,
}

impl KnownHosts {
    pub fn load() -> Result<Self> {
        let path = home_dir()
            .map(|h| h.join(".ssh").join("known_hosts"))
            .unwrap_or_else(|| PathBuf::from(".ssh/known_hosts"));
        let accepted_keys = if path.exists() {
            parse_known_hosts(&fs::read_to_string(&path).context("read known_hosts")?)?
        } else {
            HashSet::new()
        };
        Ok(Self {
            path,
            accepted_keys,
        })
    }

    /// OpenSSH `accept-new` semantics: trust known keys, add new ones.
    pub fn verify_or_accept_new(&mut self, host: &str, key: &PublicKey) -> Result<bool> {
        let canonical = canonical_key(key);
        if self.accepted_keys.contains(&canonical) {
            return Ok(true);
        }
        self.accepted_keys.insert(canonical);
        self.append_key(host, key)?;
        Ok(true)
    }

    fn append_key(&self, host: &str, key: &PublicKey) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).context("create ~/.ssh")?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        writeln!(
            file,
            "{host} {}",
            key.to_openssh().context("encode host key")?
        )
        .context("append known_hosts")?;
        Ok(())
    }
}

fn parse_known_hosts(raw: &str) -> Result<HashSet<String>> {
    let mut out = HashSet::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let _host = parts.next();
        let key_type = parts.next();
        let key_data = parts.next();
        if let (Some(kind), Some(data)) = (key_type, key_data) {
            out.insert(format!("{kind} {data}"));
        }
    }
    Ok(out)
}

fn canonical_key(key: &PublicKey) -> String {
    key.to_openssh().unwrap_or_else(|_| format!("{:?}", key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_lines() {
        let raw = "203.0.113.10 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAbcd\n";
        let keys = parse_known_hosts(raw).unwrap();
        assert!(keys.contains("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAbcd"));
    }
}
