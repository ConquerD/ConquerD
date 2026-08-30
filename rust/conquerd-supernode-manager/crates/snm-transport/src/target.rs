use std::env;

use crate::auth_prompt::{default_user_from_env, per_host_user_from_env};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub user: String,
    pub host: String,
    pub port: u16,
    /// Inventory host name this target came from, used to look up per-host
    /// secrets (`SNM_SSH_USER_<HOST>`, `SNM_SSH_PASSWORD_<HOST>`).
    pub label: Option<String>,
}

impl SshTarget {
    pub fn parse(raw: &str) -> Self {
        Self::parse_for_host(raw, None)
    }

    /// Parse `user@host:port`, letting the secrets file name the login user.
    ///
    /// `SNM_SSH_USER_<HOST>` outranks an explicit `user@` in `inventory.toml`;
    /// the bare `SNM_SSH_USER` only applies when the string has no `user@`.
    pub fn parse_for_host(raw: &str, label: Option<&str>) -> Self {
        let raw = raw.trim();
        let (user_host, user) = if let Some((user, host)) = raw.split_once('@') {
            (host, user.to_string())
        } else {
            (
                raw,
                default_user_from_env().unwrap_or_else(default_username),
            )
        };
        let user = per_host_user_from_env(label).unwrap_or(user);

        let (host, port) = match user_host.rsplit_once(':') {
            Some((host, port_str)) if port_str.chars().all(|c| c.is_ascii_digit()) => {
                match port_str.parse::<u16>() {
                    Ok(port) => (host.to_string(), port),
                    Err(_) => (user_host.to_string(), 22),
                }
            }
            _ => (user_host.to_string(), 22),
        };

        Self {
            user,
            host,
            port,
            label: label.map(str::to_string),
        }
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Key for the in-process password cache. Scoped to the exact login so a
    /// password typed for one server is never replayed against another.
    pub fn cache_key(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }

    pub fn display(&self) -> String {
        if self.port == 22 {
            format!("{}@{}", self.user, self.host)
        } else {
            format!("{}@{}:{}", self.user, self.host, self.port)
        }
    }
}

fn default_username() -> String {
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_host() {
        let t = SshTarget::parse("conquerd@203.0.113.10");
        assert_eq!(t.user, "conquerd");
        assert_eq!(t.host, "203.0.113.10");
        assert_eq!(t.port, 22);
        assert_eq!(t.label, None);
    }

    #[test]
    fn parses_user_host_port() {
        let t = SshTarget::parse("root@198.51.100.7:2222");
        assert_eq!(t.user, "root");
        assert_eq!(t.host, "198.51.100.7");
        assert_eq!(t.port, 2222);
    }

    #[test]
    fn per_host_user_env_overrides_inventory() {
        env::set_var("SNM_SSH_USER_TARGETTEST", "deploy");
        let t = SshTarget::parse_for_host("root@203.0.113.10", Some("targettest"));
        assert_eq!(t.user, "deploy");
        assert_eq!(t.label(), Some("targettest"));
        env::remove_var("SNM_SSH_USER_TARGETTEST");
    }

    #[test]
    fn cache_key_includes_user_and_port() {
        let t = SshTarget::parse("root@198.51.100.7:2222");
        assert_eq!(t.cache_key(), "root@198.51.100.7:2222");
        let t = SshTarget::parse("root@198.51.100.7");
        assert_eq!(t.cache_key(), "root@198.51.100.7:22");
    }
}
