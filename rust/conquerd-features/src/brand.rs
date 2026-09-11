//! Product branding and invite/portal URI helpers.
//!
//! The shipped product is **DoubleSlash**. Its protocol is branded **D://**.
//!
//! **An invite is an https link and nothing else**:
//! `https://doubleslash.space/i#<payload>`. Custom schemes are not auto-linked
//! by chat clients and do nothing at all when the app is missing, so a shared
//! `d://` link is dead text half the time. The payload rides in the URL
//! **fragment**, which browsers never send to the server, so the landing page
//! host never sees the invite secret and a link scanner that pre-visits the
//! URL cannot burn a single-use token.
//!
//! Two custom schemes survive, neither of them something a user ever sees or
//! shares:
//!
//! * [`URI_SCHEME_ALT`] (`doubleslash://`) is how the landing page hands an
//!   invite to an installed desktop client — a web page cannot launch a native
//!   app any other way.
//! * [`URI_SCHEME`] (`d://`) is how the in-app portal navigates
//!   supernode-hosted pages, with [`URI_SCHEME_LEGACY`] (`conquerd://`) still
//!   registered as the QtWebEngine alias that dodges Chromium's drive-letter
//!   fixup.
//!
//! Wire identifiers (QUIC TLS server name `"conquerd"`, HKDF info
//! `conquerd-invite-session-v2`, crate names) are intentionally not here —
//! changing those would split the network.

use std::borrow::Cow;

/// User-facing product name.
pub const PRODUCT_NAME: &str = "DoubleSlash";
/// Spoken / UI name for the protocol.
pub const PROTOCOL_NAME: &str = "D://";
/// Public website.
pub const WEBSITE: &str = "https://doubleslash.space";

/// Scheme the in-app portal navigates with (`d://<supernode>/path`).
/// Internal to the client; never shared, never minted for an invite.
pub const URI_SCHEME: &str = "d";
/// Long-form scheme. The https landing page uses this to hand an invite to an
/// installed desktop client: Chromium's Windows URL fixup reads the one-letter
/// `d:` as drive D:, so `d://` is unreliable from an external browser.
pub const URI_SCHEME_ALT: &str = "doubleslash";
/// Portal alias registered with QtWebEngine (see `ui/scheme.cpp`). Plumbing
/// only — it is not accepted as an invite.
pub const URI_SCHEME_LEGACY: &str = "conquerd";

/// Every scheme the portal may navigate with, **longest first** so `d://` is
/// never matched inside `doubleslash://` or `conquerd://`.
pub const URI_SCHEMES: &[&str] = &[URI_SCHEME_ALT, URI_SCHEME_LEGACY, URI_SCHEME];

/// Schemes an invite hand-off is accepted on, longest first. `conquerd` is
/// deliberately absent: pre-release, no such invite was ever published, and
/// nothing mints a scheme-form invite any more.
const HANDOFF_SCHEMES: &[&str] = &[URI_SCHEME_ALT, URI_SCHEME];

/// Host serving the https invite landing page.
pub const INVITE_HTTPS_HOST: &str = "doubleslash.space";

/// Invite paths as `(action, path)`. `action` is the prefix the invite code
/// routes on; `path` is what the landing page serves. Kept short so QR codes
/// and SMS stay small.
const HTTPS_INVITE_PATHS: &[(&str, &str)] = &[("invite", "/i"), ("room", "/r")];

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

/// Prefix used when minting portal URLs (`d://`).
pub fn uri_prefix() -> String {
    format!("{URI_SCHEME}://")
}

/// Mint a portal URL. `rest` is everything after `d://` — for the portal that
/// is `<supernode_id>/path`.
///
/// Not for invites. Use [`mint_invite_https`] for anything a human shares.
pub fn mint_uri(rest: &str) -> String {
    format!("{URI_SCHEME}://{rest}")
}

/// Mint an invite link. `action` is `"invite"` or `"room"`; `encoded` is the
/// base64url payload. Unknown actions fall back to the peer-invite path.
///
/// This is the only shape an invite is ever published in.
pub fn mint_invite_https(action: &str, encoded: &str) -> String {
    let path = HTTPS_INVITE_PATHS
        .iter()
        .find(|(a, _)| *a == action)
        .map_or("/i", |(_, p)| *p);
    format!("https://{INVITE_HTTPS_HOST}{path}#{encoded}")
}

/// Mint the hand-off URL the landing page navigates to in order to open an
/// installed desktop client. `rest` is the normalized remainder
/// (`invite#<payload>` / `room#<payload>`).
pub fn mint_handoff_uri(rest: &str) -> String {
    format!("{URI_SCHEME_ALT}://{rest}")
}

/// True when `url` is an invite this client will act on.
pub fn looks_like_app_url(url: &str) -> bool {
    normalize_app_url(url).is_some()
}

/// Reduce an accepted invite link to the remainder the invite code parses:
/// `invite#…`, `room#…`, or a bare `<b64>` payload.
///
/// Accepts the https invite form and the `doubleslash://` / `d://` hand-off.
/// The pre-rebrand `conquerd://` is not an invite.
pub fn normalize_app_url(url: &str) -> Option<Cow<'_, str>> {
    if let Some((action, payload)) = strip_https_invite(url) {
        return Some(Cow::Owned(format!("{action}#{payload}")));
    }
    strip_scheme_in(url, HANDOFF_SCHEMES).map(Cow::Borrowed)
}

/// Strip a portal scheme (`d://`, `doubleslash://` or `conquerd://`, ASCII
/// case-insensitive) and return the remainder.
///
/// This is the *portal* parser — it accepts the QtWebEngine alias and is not
/// an invite check. Use [`normalize_app_url`] for user-supplied links.
pub fn strip_scheme(url: &str) -> Option<&str> {
    strip_scheme_in(url, URI_SCHEMES)
}

fn strip_scheme_in<'a>(url: &'a str, schemes: &[&str]) -> Option<&'a str> {
    // `://` before `:/` so the double-slash form wins when both could match;
    // schemes arrive longest-first so `d` never matches inside a longer one.
    for sep in ["://", ":/"] {
        for scheme in schemes {
            let n = scheme.len() + sep.len();
            let Some(head) = url.get(..n) else { continue };
            let (Some(got), Some(tail)) = (head.get(..scheme.len()), head.get(scheme.len()..))
            else {
                continue;
            };
            if got.eq_ignore_ascii_case(scheme) && tail == sep {
                return Some(&url[n..]);
            }
        }
    }
    None
}

/// Strip the https invite form and return `(action, payload)`.
///
/// Deliberately liberal about what a chat client or mail scanner may have done
/// to the link on its way here: `http`/`https`, an optional `www.`, and an
/// optional trailing slash before the `#` all parse.
fn strip_https_invite(url: &str) -> Option<(&'static str, &str)> {
    // `to_ascii_lowercase` maps only ASCII A-Z, so byte offsets computed
    // against the lowercase copy index the original string identically.
    let lower = url.to_ascii_lowercase();
    let mut off = 0usize;

    for prefix in ["https://", "http://"] {
        if lower[off..].starts_with(prefix) {
            off += prefix.len();
            break;
        }
    }
    if off == 0 {
        return None;
    }
    if lower[off..].starts_with("www.") {
        off += "www.".len();
    }
    if !lower[off..].starts_with(INVITE_HTTPS_HOST) {
        return None;
    }
    off += INVITE_HTTPS_HOST.len();

    let (action, path) = HTTPS_INVITE_PATHS
        .iter()
        .find(|(_, path)| lower[off..].starts_with(path))
        .copied()?;
    off += path.len();
    if lower[off..].starts_with('/') {
        off += 1;
    }

    let payload = url.get(off..)?.strip_prefix('#')?;
    (!payload.is_empty()).then_some((action, payload))
}

/// Locate the first invite link in `text` and return it (trimmed at
/// whitespace / quotes / angle brackets). An unrelated `https://` URL is
/// skipped rather than returned, and a scheme never continues a word.
pub fn find_app_url(text: &str) -> Option<&str> {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();

    // Collect every plausible start, then take them left to right — so the
    // earliest real link in the text wins regardless of which form it uses.
    let mut starts: Vec<usize> = Vec::new();
    for needle in ["https://", "http://", "doubleslash://", "d://"] {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(needle) {
            let at = from + rel;
            // `nerd://x` is not `d://x`.
            if at == 0 || !bytes[at - 1].is_ascii_alphanumeric() {
                starts.push(at);
            }
            from = at + 1;
        }
    }
    starts.sort_unstable();
    starts.dedup();

    starts.into_iter().find_map(|start| {
        let rest = &text[start..];
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        looks_like_app_url(candidate).then_some(candidate)
    })
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
    fn mints_d_scheme_for_portal() {
        assert_eq!(mint_uri("peerid/"), "d://peerid/");
        assert_eq!(mint_uri("peerid/games/x/"), "d://peerid/games/x/");
    }

    #[test]
    fn mints_https_for_every_invite() {
        assert_eq!(
            mint_invite_https("invite", "abc"),
            "https://doubleslash.space/i#abc"
        );
        assert_eq!(
            mint_invite_https("room", "xyz"),
            "https://doubleslash.space/r#xyz"
        );
        // An invite with no action prefix is a peer invite.
        assert_eq!(
            mint_invite_https("", "abc"),
            "https://doubleslash.space/i#abc"
        );
    }

    #[test]
    fn https_invites_round_trip_through_normalize() {
        for (action, encoded) in [("invite", "abc"), ("room", "xyz")] {
            let url = mint_invite_https(action, encoded);
            assert_eq!(
                normalize_app_url(&url).unwrap(),
                format!("{action}#{encoded}")
            );
        }
    }

    #[test]
    fn landing_page_handoff_round_trips() {
        // What the landing page builds from the fragment, and what the client
        // receives on argv, must be the string the invite code already parses.
        let rest = "invite#abc";
        let handoff = mint_handoff_uri(rest);
        assert_eq!(handoff, "doubleslash://invite#abc");
        assert_eq!(normalize_app_url(&handoff).unwrap(), rest);
        // `d://` is accepted too — it is what the in-app portal already emits.
        assert_eq!(normalize_app_url("d://invite#abc").unwrap(), rest);
        assert_eq!(normalize_app_url("D://invite#abc").unwrap(), rest);
        assert_eq!(normalize_app_url("DoubleSlash://invite#abc").unwrap(), rest);
    }

    #[test]
    fn portal_parser_still_accepts_every_scheme() {
        // `strip_scheme` serves the QtWebEngine portal, which navigates with
        // the `conquerd://` alias to dodge Chromium's drive-letter fixup.
        for url in [
            "d://peerid/index.html",
            "D://peerid/index.html",
            "doubleslash://peerid/index.html",
            "conquerd://peerid/index.html",
            "ConquerD://peerid/index.html",
            // Single-slash form: Chromium hands the handler this shape too.
            "d:/peerid/index.html",
            "doubleslash:/peerid/index.html",
            "conquerd:/peerid/index.html",
        ] {
            assert_eq!(strip_scheme(url).unwrap(), "peerid/index.html", "url={url}");
        }
    }

    #[test]
    fn strip_rejects_other_schemes() {
        assert!(strip_scheme("https://example.com").is_none());
        assert!(strip_scheme("mailto:x").is_none());
        assert!(strip_scheme("").is_none());
        // The https invite form is normalize_app_url's job, not strip_scheme's.
        assert!(strip_scheme("https://doubleslash.space/i#abc").is_none());
    }

    #[test]
    fn normalize_tolerates_mangled_https_links() {
        for url in [
            "https://doubleslash.space/i#abc",
            "http://doubleslash.space/i#abc",
            "https://www.doubleslash.space/i#abc",
            "https://DoubleSlash.Space/I#abc",
            "https://doubleslash.space/i/#abc",
        ] {
            assert_eq!(normalize_app_url(url).unwrap(), "invite#abc", "url={url}");
        }
    }

    #[test]
    fn normalize_rejects_unrelated_and_empty_links() {
        assert!(normalize_app_url("https://example.com/i#abc").is_none());
        assert!(normalize_app_url("https://doubleslash.space/i#").is_none());
        assert!(normalize_app_url("https://doubleslash.space/i").is_none());
        assert!(normalize_app_url("https://doubleslash.space/download").is_none());
        assert!(normalize_app_url("mailto:x").is_none());
        assert!(normalize_app_url("").is_none());
        // The marketing site itself is not an invite.
        assert!(normalize_app_url(WEBSITE).is_none());
    }

    #[test]
    fn the_pre_rebrand_scheme_is_not_an_invite() {
        // Pre-release: nothing ever published a `conquerd://` invite, and the
        // site is the only way in. It survives as a portal alias only.
        assert!(normalize_app_url("conquerd://invite#abc").is_none());
        assert!(!looks_like_app_url("conquerd://invite#abc"));
        assert!(find_app_url("see conquerd://abc123 then more").is_none());
        // …while the portal parser still reads it.
        assert_eq!(strip_scheme("conquerd://invite#abc").unwrap(), "invite#abc");
    }

    #[test]
    fn normalize_preserves_payload_case() {
        let payload = "eyJhIjoiQi1fLXoifQ";
        let url = mint_invite_https("invite", payload);
        assert_eq!(
            normalize_app_url(&url).unwrap(),
            format!("invite#{payload}")
        );
    }

    #[test]
    fn find_handoff_scheme() {
        let text = "invite: d://invite#abc trailing";
        assert_eq!(find_app_url(text), Some("d://invite#abc"));
    }

    #[test]
    fn find_https_invite_in_chat_text() {
        let text = "join me: https://doubleslash.space/i#abc123 see you";
        assert_eq!(
            find_app_url(text),
            Some("https://doubleslash.space/i#abc123")
        );
    }

    #[test]
    fn find_skips_unrelated_https_links() {
        let text = "docs https://example.com/i#nope invite https://doubleslash.space/r#ok";
        assert_eq!(find_app_url(text), Some("https://doubleslash.space/r#ok"));
    }

    #[test]
    fn find_ignores_scheme_inside_a_word() {
        assert!(find_app_url("nerd://nope").is_none());
        assert!(find_app_url("no links here").is_none());
    }

    #[test]
    fn find_unwraps_html_context() {
        assert_eq!(
            find_app_url(r#"<a href="https://doubleslash.space/i#abc">join</a>"#),
            Some("https://doubleslash.space/i#abc")
        );
    }

    #[test]
    fn looks_like_app_url_covers_accepted_forms() {
        for url in [
            "https://doubleslash.space/i#x",
            "https://doubleslash.space/r#x",
            "doubleslash://invite#x",
            "d://invite#x",
        ] {
            assert!(looks_like_app_url(url), "url={url}");
        }
        assert!(!looks_like_app_url("https://doubleslash.space"));
        assert!(!looks_like_app_url("https://example.com/i#x"));
    }
}
