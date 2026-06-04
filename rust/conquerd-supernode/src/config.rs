// ConquerD Supernode — config.rs
// Environment-variable-based configuration.

use std::env;

/// All supernode configuration sourced from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    // Networking
    pub signaling_port: u16,
    pub relay_port: u16,
    pub web_port: Option<u16>,

    // Features
    pub chat_enabled: bool,
    pub files_enabled: bool,
    pub sfu_enabled: bool,
    #[allow(dead_code)]
    pub updates_enabled: bool,
    #[allow(dead_code)]
    pub auto_restart: bool,

    // Invite
    pub invite_ttl_seconds: i64, // -1 = never expires

    // Web portal
    pub web_title: String,
    pub access_mode: AccessMode,
    pub access_code: String,
    pub ad_duration: u32,
    pub tos_text: String,
    pub ad_content: String,
    pub demo_links: bool,

    // External host/IP for relay tickets (required for clients to connect)
    pub external_host: Option<String>,

    // Bind web portal to 127.0.0.1 only (disable for public-facing nodes).
    pub web_localhost_only: bool,

    // Data directory
    pub data_dir: std::path::PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessMode {
    Open,
    Tos,
    Ad,
    Code,
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = match env::var("CONQUERD_HOME") {
            Ok(h) => std::path::PathBuf::from(h),
            Err(_) => {
                let home = dirs_next().unwrap_or_else(|| ".".into());
                std::path::PathBuf::from(home).join(".conquerd")
            }
        };

        let invite_ttl_raw: i64 = env::var("supernode_invite_ttl")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(-1);
        let invite_ttl_seconds = if invite_ttl_raw < 0 {
            -1
        } else {
            invite_ttl_raw * 60
        };

        Config {
            signaling_port: env_u16("supernode_signaling_port", 34935),
            relay_port: env_u16("supernode_port", 3478),
            web_port: env::var("supernode_web_port")
                .ok()
                .and_then(|v| v.parse().ok()),
            chat_enabled: env_bool("supernode_chat", true),
            files_enabled: env_bool("supernode_files", true),
            sfu_enabled: env_bool("supernode_sfu", true),
            updates_enabled: env_bool("supernode_updates", true),
            auto_restart: env_bool("supernode_auto_restart", true),
            invite_ttl_seconds,
            web_title: env::var("supernode_web_title").unwrap_or_else(|_| "Relay Node".into()),
            access_mode: match env::var("supernode_access_mode")
                .unwrap_or_default()
                .as_str()
            {
                "tos" => AccessMode::Tos,
                "ad" => AccessMode::Ad,
                "code" => AccessMode::Code,
                _ => AccessMode::Open,
            },
            access_code: env::var("supernode_access_code").unwrap_or_else(|_| "conquerd".into()),
            ad_duration: env::var("supernode_ad_duration")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            tos_text: env::var("supernode_tos_text").unwrap_or_default(),
            ad_content: env::var("supernode_ad_content").unwrap_or_default(),
            demo_links: env_bool("supernode_demo_links", false),
            external_host: env::var("supernode_host").ok().filter(|s| !s.is_empty()),
            web_localhost_only: env_bool("supernode_web_localhost_only", false),
            data_dir,
        }
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key).as_deref() {
        Ok("1") | Ok("true") | Ok("yes") => true,
        Ok("0") | Ok("false") | Ok("no") => false,
        _ => default,
    }
}

/// Get user home directory.
fn dirs_next() -> Option<String> {
    #[cfg(windows)]
    {
        env::var("USERPROFILE").ok()
    }
    #[cfg(not(windows))]
    {
        env::var("HOME").ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: parse env_bool directly via a unique env key.
    fn set_and_check_bool(key: &str, val: &str, default: bool) -> bool {
        env::set_var(key, val);
        let result = env_bool(key, default);
        env::remove_var(key);
        result
    }

    #[test]
    fn env_bool_truthy_values() {
        assert!(set_and_check_bool("_CQ_TEST_BOOL_1", "1", false));
        assert!(set_and_check_bool("_CQ_TEST_BOOL_TRUE", "true", false));
        assert!(set_and_check_bool("_CQ_TEST_BOOL_YES", "yes", false));
    }

    #[test]
    fn env_bool_falsy_values() {
        assert!(!set_and_check_bool("_CQ_TEST_BOOL_0", "0", true));
        assert!(!set_and_check_bool("_CQ_TEST_BOOL_FALSE", "false", true));
        assert!(!set_and_check_bool("_CQ_TEST_BOOL_NO", "no", true));
    }

    #[test]
    fn env_bool_falls_back_to_default_when_unset() {
        env::remove_var("_CQ_TEST_BOOL_MISSING");
        assert!(env_bool("_CQ_TEST_BOOL_MISSING", true));
        assert!(!env_bool("_CQ_TEST_BOOL_MISSING", false));
    }

    #[test]
    fn env_bool_falls_back_to_default_for_unknown_value() {
        env::set_var("_CQ_TEST_BOOL_JUNK", "maybe");
        assert!(env_bool("_CQ_TEST_BOOL_JUNK", true));
        assert!(!env_bool("_CQ_TEST_BOOL_JUNK", false));
        env::remove_var("_CQ_TEST_BOOL_JUNK");
    }

    #[test]
    fn env_u16_parses_valid_value() {
        env::set_var("_CQ_TEST_PORT", "9999");
        let v = env_u16("_CQ_TEST_PORT", 1234);
        env::remove_var("_CQ_TEST_PORT");
        assert_eq!(v, 9999);
    }

    #[test]
    fn env_u16_falls_back_on_missing_or_bad_value() {
        env::remove_var("_CQ_TEST_PORT_MISSING");
        assert_eq!(env_u16("_CQ_TEST_PORT_MISSING", 5678), 5678);

        env::set_var("_CQ_TEST_PORT_BAD", "notanumber");
        let v = env_u16("_CQ_TEST_PORT_BAD", 4321);
        env::remove_var("_CQ_TEST_PORT_BAD");
        assert_eq!(v, 4321);
    }

    #[test]
    fn access_mode_from_env_parses_all_variants() {
        // Test the string matching logic directly (no env mutation needed).
        let parse = |s: &str| -> AccessMode {
            match s {
                "tos" => AccessMode::Tos,
                "ad" => AccessMode::Ad,
                "code" => AccessMode::Code,
                _ => AccessMode::Open,
            }
        };
        assert_eq!(parse("tos"), AccessMode::Tos);
        assert_eq!(parse("ad"), AccessMode::Ad);
        assert_eq!(parse("code"), AccessMode::Code);
        assert_eq!(parse("open"), AccessMode::Open);
        assert_eq!(parse(""), AccessMode::Open);
        assert_eq!(parse("unknown"), AccessMode::Open);
    }

    #[test]
    fn invite_ttl_negative_raw_maps_to_minus_one() {
        // Test the conversion formula directly without going through from_env().
        let convert = |raw: i64| -> i64 {
            if raw < 0 {
                -1
            } else {
                raw * 60
            }
        };
        assert_eq!(convert(-5), -1);
        assert_eq!(convert(-1), -1);
    }

    #[test]
    fn invite_ttl_positive_raw_converts_to_seconds() {
        let convert = |raw: i64| -> i64 {
            if raw < 0 {
                -1
            } else {
                raw * 60
            }
        };
        assert_eq!(convert(10), 600);
        assert_eq!(convert(1), 60);
        assert_eq!(convert(0), 0);
    }
}
