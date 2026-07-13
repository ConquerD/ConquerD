//! Rust side of the `conquerd://` custom URL scheme handler.
//!
//! Registers a process-global callback that [`scheme.cpp`]'s
//! `ConquerdSchemeHandler::requestStarted` calls synchronously
//! (from Qt's internal IO thread) via [`conquerd_fetch_sync`].
//!
//! # Setup (called from the AppBridge during initialisation)
//!
//! ```rust,ignore
//! crate::ui::scheme::register_fetch_callback(Arc::clone(&cmd_tx), rt_handle.clone());
//! ```
//!
//! The callback uses [`tokio::runtime::Handle::block_on`] on the **caller's
//! own thread** (Qt IO thread, not a tokio task), so it's safe to block.
//! `block_on` on a non-tokio thread drives the future to completion while
//! the runtime's other tasks continue on their executor threads.
//!
//! # Memory contract with C++
//!
//! The `out_content_type` and `out_body` buffers are allocated here with
//! `Box::into_raw(Box<[u8]>)` and must be freed by the C++ caller with
//! `std::free()`. `Box<[u8]>` uses the global allocator, which on all
//! supported platforms (MSVC, GCC, Clang) is compatible with `free()`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use base64::Engine;
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::connection_manager::ConnectionCommand;
use crate::web_app_client::WebAppResponse;

/// Max queued inbound game datagrams per supernode (portal poll drain).
const PORTAL_GAME_QUEUE_CAP: usize = 256;

// ── Global state ──────────────────────────────────────────────────────────────

/// Process-global sender into the ConnectionManager.
/// Set once during `AppBridge::initialize_backend`.
static CMD_TX: OnceLock<mpsc::Sender<ConnectionCommand>> = OnceLock::new();

/// Tokio runtime handle for `block_on` from non-async C++ threads.
static RT_HANDLE: OnceLock<Handle> = OnceLock::new();

/// Our own peer ID, set when identity unlocks so `/_conquerd/ctx.json` can
/// include it without a round-trip to the supernode.
static PORTAL_PEER_ID: OnceLock<String> = OnceLock::new();

/// Case-insensitive lookup table: `lowercase(peer_id) → original peer_id`.
///
/// Chromium lower-cases the authority of every `scheme://AUTHORITY/path`
/// URL, which would destroy the case-sensitive base64url peer IDs that
/// flow through `conquerd://`.  Whenever the UI opens a portal we record
/// the canonical form here so [`parse_conquerd_url`] can recover it from
/// the lower-cased authority Chromium hands the scheme handler.
static PORTAL_ID_MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn portal_map() -> &'static Mutex<HashMap<String, String>> {
    PORTAL_ID_MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register a portal peer ID so the scheme handler can recover the
/// original (mixed-case) form from the lower-cased authority Chromium
/// passes to `requestStarted`.  Idempotent.
pub fn register_portal_peer_id(peer_id: &str) {
    let lowered = peer_id.to_ascii_lowercase();
    if let Ok(mut map) = portal_map().lock() {
        map.entry(lowered).or_insert_with(|| peer_id.to_owned());
    }
}

/// Inbound portal game datagrams keyed by supernode id (base64url payloads).
static PORTAL_GAME_INBOUND: OnceLock<Mutex<HashMap<String, VecDeque<Vec<u8>>>>> = OnceLock::new();

fn portal_game_queues() -> &'static Mutex<HashMap<String, VecDeque<Vec<u8>>>> {
    PORTAL_GAME_INBOUND.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Push an opaque `game.relay.v1` frame for the portal page to poll.
/// Called from the AppBridge event path when the QUIC relay delivers a frame.
pub fn push_portal_game_datagram(supernode_id: &str, payload: Vec<u8>) {
    if payload.is_empty() {
        return;
    }
    if let Ok(mut map) = portal_game_queues().lock() {
        let q = map.entry(supernode_id.to_owned()).or_default();
        if q.len() >= PORTAL_GAME_QUEUE_CAP {
            q.pop_front();
        }
        q.push_back(payload);
    }
}

fn drain_portal_game_datagrams(supernode_id: &str, max: usize) -> Vec<Vec<u8>> {
    let Ok(mut map) = portal_game_queues().lock() else {
        return Vec::new();
    };
    let Some(q) = map.get_mut(supernode_id) else {
        return Vec::new();
    };
    let n = max.min(q.len());
    q.drain(..n).collect()
}

/// Store our peer ID for injection into portal pages via `/_conquerd/ctx.json`.
/// Called from `AppBridge` after the identity is unlocked.
pub fn set_portal_peer_id(peer_id: &str) {
    let _ = PORTAL_PEER_ID.set(peer_id.to_owned());
}

/// Register the global connection-manager sender and runtime handle.
///
/// Must be called once, before any `conquerd://` URL is loaded.
/// Calling it a second time is a no-op (OnceLock semantics).
pub fn register_fetch_callback(cmd_tx: mpsc::Sender<ConnectionCommand>, rt_handle: Handle) {
    let _ = CMD_TX.set(cmd_tx);
    let _ = RT_HANDLE.set(rt_handle);
}

// ── C entry points (called from scheme.cpp) ───────────────────────────────────

// Register the `conquerd://` URL scheme with QtWebEngine.
// Must be called **before** `QGuiApplication::new()`.
extern "C" {
    pub fn conquerd_register_scheme();
    pub fn conquerd_install_scheme_handler();
}

/// Perform a blocking fetch of a `conquerd://` URL.
///
/// Called from `ConquerdSchemeHandler::requestStarted` (Qt IO thread, NOT a
/// tokio task). Allocates response buffers with the global allocator; the C++
/// caller is responsible for freeing them with `std::free()`.
///
/// # Safety
/// `url` must be a valid UTF-8 pointer of `url_len` bytes.
/// Output pointer-to-pointer arguments must not be null.
#[no_mangle]
pub unsafe extern "C" fn conquerd_fetch_sync(
    url: *const u8,
    url_len: usize,
    out_content_type: *mut *mut u8,
    out_ct_len: *mut usize,
    out_body: *mut *mut u8,
    out_body_len: *mut usize,
) -> bool {
    // Initialise output slots.
    unsafe {
        *out_content_type = std::ptr::null_mut();
        *out_ct_len = 0;
        *out_body = std::ptr::null_mut();
        *out_body_len = 0;
    }

    // ── Parse the URL ─────────────────────────────────────────────────────
    let url_bytes = unsafe { std::slice::from_raw_parts(url, url_len) };
    let url_str = match std::str::from_utf8(url_bytes) {
        Ok(s) => s,
        Err(_) => {
            error!("[scheme] non-UTF-8 URL from C++");
            return false;
        }
    };
    debug!("[scheme] requestStarted: {url_str}");
    // Temporarily promoted to info! while debugging the embedded portal so
    // the visible log shows whether QtWebEngine reaches the handler.
    info!("[scheme] fetch_sync url={}", url_str);

    // conquerd://<supernode_id>/<path>
    let (supernode_id, path, query) = match parse_conquerd_url(url_str) {
        Some(v) => v,
        None => {
            error!("[scheme] cannot parse conquerd URL: {url_str}");
            return false;
        }
    };

    // ── Built-in local endpoints (served without a relay round-trip) ──────
    // conquerd://<any_supernode>/_conquerd/ctx.json
    //   Returns the client's own peer ID and version so portal JS can
    //   populate `window.conquerd` without an extra network hop.
    //   `nativeTransport: true` — games use identity-path channel APIs only.
    if path == "/_conquerd/ctx.json" {
        let peer_id = PORTAL_PEER_ID.get().map(String::as_str).unwrap_or("");
        let json = format!(
            "{{\"myPeerId\":\"{}\",\"version\":\"{}\",\"nativeTransport\":true}}",
            peer_id.replace('"', "\\\""),
            env!("CARGO_PKG_VERSION"),
        );
        let ct = b"application/json; charset=utf-8";
        unsafe {
            *out_content_type = libc_alloc(ct);
            *out_ct_len = ct.len();
            *out_body = libc_alloc(json.as_bytes());
            *out_body_len = json.len();
        }
        return true;
    }

    // Portal game channel (identity QUIC relay — no WebTransport cert).
    // Paths:
    //   /_conquerd/channel/open?room=<lobby>
    //   /_conquerd/channel/send?b64=<base64url payload>
    //   /_conquerd/channel/poll
    //   /_conquerd/channel/close
    if path.starts_with("/_conquerd/channel/") {
        return serve_portal_channel(
            &supernode_id,
            &path,
            query.as_deref(),
            out_content_type,
            out_ct_len,
            out_body,
            out_body_len,
        );
    }

    // ── Fetch via ConnectionManager ───────────────────────────────────────
    let (cmd_tx, rt) = match (CMD_TX.get(), RT_HANDLE.get()) {
        (Some(tx), Some(rt)) => (tx, rt),
        _ => {
            error!("[scheme] fetch callback not yet registered (call register_fetch_callback before loading conquerd:// URLs)");
            return false;
        }
    };

    info!(
        "[scheme] dispatching FetchWebApp sn={} path={}",
        supernode_id, path
    );

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let cmd = ConnectionCommand::FetchWebApp {
        supernode_id,
        path,
        query,
        reply_tx,
    };
    if cmd_tx.blocking_send(cmd).is_err() {
        error!("[scheme] ConnectionManager channel closed");
        return false;
    }

    // Block this (Qt IO) thread until the fetch resolves.
    // `Handle::block_on` drives the future on the runtime without entering
    // the `#[tokio::main]` context — safe on non-tokio threads.
    let response: WebAppResponse = match rt.block_on(async move {
        reply_rx
            .await
            .unwrap_or_else(|_| Err("reply channel dropped".to_owned()))
    }) {
        Ok(r) if r.status == 200 => r,
        Ok(r) => {
            error!("[scheme] supernode returned status {}", r.status);
            return false;
        }
        Err(e) => {
            error!("[scheme] fetch error: {e}");
            return false;
        }
    };

    // ── Copy content-type into C-malloc'd buffer ──────────────────────────
    let ct_bytes = response.content_type.into_bytes();
    let ct_len = ct_bytes.len();
    let ct_ptr = libc_alloc(ct_bytes.as_slice());
    unsafe {
        *out_content_type = ct_ptr;
        *out_ct_len = ct_len;
    }

    // ── Copy body into C-malloc'd buffer ──────────────────────────────────
    let body_len = response.body.len();
    let body_ptr = libc_alloc(&response.body);
    unsafe {
        *out_body = body_ptr;
        *out_body_len = body_len;
    }

    true
}

/// Local portal channel API for identity-path `game.relay.v1`.
unsafe fn serve_portal_channel(
    supernode_id: &str,
    path: &str,
    query: Option<&str>,
    out_content_type: *mut *mut u8,
    out_ct_len: *mut usize,
    out_body: *mut *mut u8,
    out_body_len: *mut usize,
) -> bool {
    let (cmd_tx, rt) = match (CMD_TX.get(), RT_HANDLE.get()) {
        (Some(tx), Some(rt)) => (tx, rt),
        _ => {
            error!("[scheme] portal channel: fetch callback not registered");
            return false;
        }
    };

    let params: HashMap<String, String> = query
        .unwrap_or("")
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((
                k.to_owned(),
                urlencoding_decode(v).unwrap_or_else(|| v.to_owned()),
            ))
        })
        .collect();

    let json_ok = |body: String| -> bool {
        let ct = b"application/json; charset=utf-8";
        unsafe {
            *out_content_type = libc_alloc(ct);
            *out_ct_len = ct.len();
            *out_body = libc_alloc(body.as_bytes());
            *out_body_len = body.len();
        }
        true
    };

    match path {
        "/_conquerd/channel/open" => {
            let room = params
                .get("room")
                .cloned()
                .unwrap_or_else(|| "default".to_owned());
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let cmd = ConnectionCommand::PortalGameOpen {
                supernode_id: supernode_id.to_owned(),
                room,
                reply_tx,
            };
            if cmd_tx.blocking_send(cmd).is_err() {
                return false;
            }
            match rt.block_on(reply_rx) {
                Ok(Ok(())) => json_ok(r#"{"ok":true}"#.to_owned()),
                Ok(Err(e)) => {
                    error!("[scheme] portal game open failed: {e}");
                    json_ok(format!(
                        r#"{{"ok":false,"error":"{}"}}"#,
                        e.replace('"', "'")
                    ))
                }
                Err(_) => false,
            }
        }
        "/_conquerd/channel/send" => {
            let Some(b64) = params.get("b64") else {
                return json_ok(r#"{"ok":false,"error":"missing b64"}"#.to_owned());
            };
            let payload = match base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(b64.as_bytes())
            {
                Ok(p) => p,
                Err(_) => {
                    // Also accept standard URL_SAFE with padding.
                    match base64::engine::general_purpose::URL_SAFE.decode(b64.as_bytes()) {
                        Ok(p) => p,
                        Err(e) => {
                            return json_ok(format!(r#"{{"ok":false,"error":"bad b64: {e}"}}"#));
                        }
                    }
                }
            };
            if payload.len() > 64 * 1024 {
                return json_ok(r#"{"ok":false,"error":"payload too large"}"#.to_owned());
            }
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let cmd = ConnectionCommand::PortalGameSend {
                supernode_id: supernode_id.to_owned(),
                payload,
                reply_tx,
            };
            if cmd_tx.blocking_send(cmd).is_err() {
                return false;
            }
            match rt.block_on(reply_rx) {
                Ok(Ok(())) => json_ok(r#"{"ok":true}"#.to_owned()),
                Ok(Err(e)) => json_ok(format!(
                    r#"{{"ok":false,"error":"{}"}}"#,
                    e.replace('"', "'")
                )),
                Err(_) => false,
            }
        }
        "/_conquerd/channel/poll" => {
            let frames = drain_portal_game_datagrams(supernode_id, 64);
            let b64s: Vec<String> = frames
                .into_iter()
                .map(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(p))
                .collect();
            let body = serde_json::json!({ "frames": b64s }).to_string();
            json_ok(body)
        }
        "/_conquerd/channel/close" => {
            let cmd = ConnectionCommand::PortalGameClose {
                supernode_id: supernode_id.to_owned(),
            };
            let _ = cmd_tx.blocking_send(cmd);
            json_ok(r#"{"ok":true}"#.to_owned())
        }
        _ => {
            error!("[scheme] unknown portal channel path: {path}");
            false
        }
    }
}

/// Minimal percent-decode for query values (space as `+` or `%20`).
fn urlencoding_decode(s: &str) -> Option<String> {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let h = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
                let v = u8::from_str_radix(h, 16).ok()?;
                out.push(v);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Allocate a copy of `src` using the Rust global allocator (compatible with
/// C `free()` on all supported platforms).
fn libc_alloc(src: &[u8]) -> *mut u8 {
    let len = src.len().max(1);
    let Ok(layout) = std::alloc::Layout::from_size_align(len, 1) else {
        return std::ptr::null_mut();
    };
    if src.is_empty() {
        // Return a non-null sentinel that is safe to free.
        return unsafe { std::alloc::alloc(layout) };
    }
    let ptr = unsafe { std::alloc::alloc(layout) };
    if !ptr.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), ptr, src.len()) };
    }
    ptr
}

/// Parse `conquerd://<supernode_id>/<path>[?query]` into its components.
///
/// Chromium lower-cases the authority of every `scheme://` URL, so the
/// `supernode_id` returned here is normalised through
/// [`portal_map`]: if a matching mixed-case ID has been registered with
/// [`register_portal_peer_id`], that canonical form is returned;
/// otherwise the lower-cased authority is returned as-is.
///
/// Also accepts `conquerd:/PEERID/path` (single slash, no authority) for
/// robustness in case a relative-URL resolution produces it.
///
/// Returns `None` if the URL does not match the expected structure.
fn parse_conquerd_url(url: &str) -> Option<(String, String, Option<String>)> {
    let rest = url
        .strip_prefix("conquerd://")
        .or_else(|| url.strip_prefix("conquerd:/"))?;
    // authority = everything before the first '/'
    let (authority, path_and_query) = if let Some(idx) = rest.find('/') {
        (&rest[..idx], &rest[idx..])
    } else {
        // No path → treat as "/"
        (rest, "/")
    };
    if authority.is_empty() {
        return None;
    }
    let (path, query) = if let Some(idx) = path_and_query.find('?') {
        (
            path_and_query[..idx].to_owned(),
            Some(path_and_query[idx + 1..].to_owned()),
        )
    } else {
        (path_and_query.to_owned(), None)
    };
    // Recover the canonical mixed-case peer ID if we have one cached.
    let lowered = authority.to_ascii_lowercase();
    let canonical = portal_map()
        .lock()
        .ok()
        .and_then(|m| m.get(&lowered).cloned())
        .unwrap_or_else(|| authority.to_owned());
    Some((canonical, path, query))
}

#[cfg(test)]
mod tests {
    use super::parse_conquerd_url;

    #[test]
    fn basic_parse() {
        let (sn, path, q) = parse_conquerd_url("conquerd://abc123/index.html").unwrap();
        assert_eq!(sn, "abc123");
        assert_eq!(path, "/index.html");
        assert!(q.is_none());
    }

    #[test]
    fn with_query() {
        let (_, path, q) = parse_conquerd_url("conquerd://abc123/search?q=hello").unwrap();
        assert_eq!(path, "/search");
        assert_eq!(q.as_deref(), Some("q=hello"));
    }

    #[test]
    fn bare_authority_becomes_root() {
        let (sn, path, q) = parse_conquerd_url("conquerd://abc123").unwrap();
        assert_eq!(sn, "abc123");
        assert_eq!(path, "/");
        assert!(q.is_none());
    }

    #[test]
    fn rejects_empty_authority() {
        assert!(parse_conquerd_url("conquerd:///index.html").is_none());
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert!(parse_conquerd_url("https://example.com/").is_none());
    }
}
