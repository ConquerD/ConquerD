use std::env;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl SshTarget {
    pub fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        let (user_host, user) = if let Some((user, host)) = raw.split_once('@') {
            (host, user.to_string())
        } else {
            (raw, default_username())
        };

        let (host, port) = match user_host.rsplit_once(':') {
            Some((host, port_str)) if port_str.chars().all(|c| c.is_ascii_digit()) => {
                match port_str.parse::<u16>() {
                    Ok(port) => (host.to_string(), port),
                    Err(_) => (user_host.to_string(), 22),
                }
            }
            _ => (user_host.to_string(), 22),
        };

        Self { user, host, port }
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
    }

    #[test]
    fn parses_user_host_port() {
        let t = SshTarget::parse("root@198.51.100.7:2222");
        assert_eq!(t.user, "root");
        assert_eq!(t.host, "198.51.100.7");
        assert_eq!(t.port, 2222);
    }
}
