//! Product branding and invite/portal URI helpers.
//!
//! The shipped product is **DoubleSlash**. The invite and in-app portal
//! protocol is branded **D://** and minted as `d://…`. The legacy
//! `conquerd://` scheme is accepted forever so existing invites keep working.
//!
//! Wire identifiers (QUIC TLS server name `"conquerd"`, HKDF info
//! `conquerd-invite-session-v2`, crate names) are intentionally not here —
//! changing those would split the network.

/// User-facing product name.
pub const PRODUCT_NAME: &str = "DoubleSlash";
/// Spoken / UI name for the invite and portal protocol.
pub const PROTOCOL_NAME: &str = "D://";
/// Public website.
pub const WEBSITE: &str = "https://doubleslash.space";

/// Scheme used when minting new invite and portal URLs.
pub const URI_SCHEME: &str = "d";
/// Legacy scheme still accepted on parse and registered with the OS.
pub const URI_SCHEME_LEGACY: &str = "conquerd";

/// Default profile directory name under `$HOME` (`~/.doubleslash`).
pub const DEFAULT_PROFILE_DIR: &str = ".doubleslash";
/// Pre-rebrand profile directory (`~/.conquerd`).
pub const LEGACY_PROFILE_DIR: &str = ".conquerd";

pub const ENV_HOME: &str = "DOUBLESLASH_HOME";
pub const ENV_HOME_LEGACY: &str = "CONQUERD_HOME";
pub const ENV_KEY_DIR: &str = "DOUBLESLASH_KEY_DIR";
pub const ENV_KEY_DIR_LEGACY: &str = "CONQUERD_KEY_DIR";

/// Windows install-folder / portable-folder name.
pub const WINDOWS_INSTALL_DIR: &str = "DoubleSlash";
pub const WINDOWS_INSTALL_DIR_LEGACY: &str = "ConquerD";
/// Windows client executable name.
pub const WINDOWS_EXE: &str = "DoubleSlash.exe";
pub const WINDOWS_EXE_LEGACY: &str = "ConquerD.exe";

/// Prefix used when minting (`d://`).
pub fn uri_prefix() -> String {
    format!("{URI_SCHEME}://")
}

/// Mint a URL with the current scheme. `rest` is everything after `d://`
/// (for example `invite#…`, `room#…`, or `<peer>/path`).
pub fn mint_uri(rest: &str) -> String {
    format!("{URI_SCHEME}://{rest}")
}

/// True when `url` uses the current or legacy invite/portal scheme.
pub fn looks_like_app_url(url: &str) -> bool {
    strip_scheme(url).is_some()
}

/// Strip a known scheme (`d://` or `conquerd://`, ASCII case-insensitive)
/// and return the remainder. The longer legacy prefix is tried first so
/// `conquerd://…` is not misread as `d://` inside `conquerd`.
pub fn strip_scheme(url: &str) -> Option<&str> {
    strip_scheme_with_slashes(url, true).or_else(|| strip_scheme_with_slashes(url, false))
}

fn strip_scheme_with_slashes(url: &str, double_slash: bool) -> Option<&str> {
    let lower = url.get(..url.len().min(32))?.to_ascii_lowercase();
    let suffixes = if double_slash { "://" } else { ":/" };
    for scheme in [URI_SCHEME_LEGACY, URI_SCHEME] {
        let prefix = format!("{scheme}{suffixes}");
        if lower.starts_with(&prefix) && url.len() >= prefix.len() {
            // Confirm the original (possibly mixed-case) prefix is the scheme.
            if url.get(..prefix.len())?.eq_ignore_ascii_case(&prefix) {
                return Some(&url[prefix.len()..]);
            }
        }
    }
    None
}

/// Locate the first `d://` or `conquerd://` URL in `text` and return it
/// (trimmed at whitespace / quotes). `d://` is not matched inside
/// `conquerd://`.
pub fn find_app_url(text: &str) -> Option<&str> {
    let start = find_scheme_start(text)?;
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '>')
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

fn find_scheme_start(text: &str) -> Option<usize> {
    let lower = text.to_ascii_lowercase();
    if let Some(i) = lower.find("conquerd://") {
        return Some(i);
    }
    let bytes = lower.as_bytes();
    let mut start = 0;
    while let Some(rel) = lower[start..].find("d://") {
        let i = start + rel;
        let ok = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if ok {
            return Some(i);
        }
        start = i + 1;
    }
    None
}

/// First env var in `names` that is set and non-empty.
pub fn first_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_d_scheme() {
        assert_eq!(mint_uri("invite#abc"), "d://invite#abc");
        assert_eq!(mint_uri("room#xyz"), "d://room#xyz");
        assert_eq!(mint_uri("peerid/"), "d://peerid/");
    }

    #[test]
    fn strip_accepts_current_and_legacy() {
        assert_eq!(strip_scheme("d://invite#abc").unwrap(), "invite#abc");
        assert_eq!(strip_scheme("D://invite#abc").unwrap(), "invite#abc");
        assert_eq!(strip_scheme("conquerd://invite#abc").unwrap(), "invite#abc");
        assert_eq!(strip_scheme("ConquerD://invite#abc").unwrap(), "invite#abc");
        assert_eq!(strip_scheme("conquerd://payload").unwrap(), "payload");
        assert_eq!(strip_scheme("d://payload").unwrap(), "payload");
    }

    #[test]
    fn strip_accepts_single_slash_portal_form() {
        assert_eq!(
            strip_scheme("d:/peerid/index.html").unwrap(),
            "peerid/index.html"
        );
        assert_eq!(
            strip_scheme("conquerd:/peerid/index.html").unwrap(),
            "peerid/index.html"
        );
    }

    #[test]
    fn strip_rejects_other_schemes() {
        assert!(strip_scheme("https://example.com").is_none());
        assert!(strip_scheme("mailto:x").is_none());
        assert!(strip_scheme("").is_none());
    }

    #[test]
    fn find_prefers_legacy_over_embedded_d() {
        let text = "see conquerd://abc123 then more";
        assert_eq!(find_app_url(text), Some("conquerd://abc123"));
    }

    #[test]
    fn find_current_scheme() {
        let text = "invite: d://invite#abc trailing";
        assert_eq!(find_app_url(text), Some("d://invite#abc"));
    }

    #[test]
    fn looks_like_app_url_both_schemes() {
        assert!(looks_like_app_url("d://invite#x"));
        assert!(looks_like_app_url("conquerd://invite#x"));
        assert!(!looks_like_app_url("https://doubleslash.space"));
    }
}
