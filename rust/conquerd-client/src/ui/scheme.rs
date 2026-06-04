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

#![cfg(feature = "webengine")]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::connection_manager::ConnectionCommand;
use crate::web_app_client::WebAppResponse;

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

/// Per-supernode WebTransport base URL (e.g. `https://relay.example:8443`).
/// Set when a `SUPERNODE_INFO` message with `wt_url` is received.
/// Served via `/_conquerd/ctx.json` so game pages loaded inside
/// `conquerd://` know the real address for `new WebTransport(url)`.
static SUPERNODE_WT_URLS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Per-supernode SHA-256 fingerprint of the self-signed WebTransport cert.
/// Delivered via the already-verified SUPERNODE_INFO channel so game pages
/// can use `serverCertificateHashes` without any CA.
static SUPERNODE_CERT_FPS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

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

/// Cache the WebTransport base URL for *supernode_id*.
/// Called from `bridge.rs` when a `SUPERNODE_INFO` message arrives.
pub fn set_supernode_wt_url(supernode_id: &str, wt_url: &str) {
    if let Ok(mut map) = SUPERNODE_WT_URLS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        map.insert(supernode_id.to_owned(), wt_url.to_owned());
    }
}

fn get_supernode_wt_url(supernode_id: &str) -> Option<String> {
    SUPERNODE_WT_URLS
        .get()?
        .lock()
        .ok()?
        .get(supernode_id)
        .cloned()
}

/// Cache the WebTransport cert fingerprint for *supernode_id*.
pub fn set_supernode_cert_fingerprint(supernode_id: &str, fingerprint: &str) {
    if let Ok(mut map) = SUPERNODE_CERT_FPS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        map.insert(supernode_id.to_owned(), fingerprint.to_owned());
    }
}

fn get_supernode_cert_fingerprint(supernode_id: &str) -> Option<String> {
    SUPERNODE_CERT_FPS
        .get()?
        .lock()
        .ok()?
        .get(supernode_id)
        .cloned()
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
    if path == "/_conquerd/ctx.json" {
        let peer_id = PORTAL_PEER_ID.get().map(String::as_str).unwrap_or("");
        let wt_base = get_supernode_wt_url(&supernode_id).unwrap_or_default();
        let cert_fp = get_supernode_cert_fingerprint(&supernode_id).unwrap_or_default();
        let wt_field = if wt_base.is_empty() {
            String::new()
        } else {
            format!(",\"wtBaseUrl\":\"{}\"", wt_base.replace('"', "\\\""))
        };
        let fp_field = if cert_fp.is_empty() {
            String::new()
        } else {
            format!(",\"wtCertHash\":\"{}\"", cert_fp.replace('"', "\\\""))
        };
        let json = format!(
            "{{\"myPeerId\":\"{}\",\"version\":\"{}\"{}{}}}",
            peer_id.replace('"', "\\\""),
            env!("CARGO_PKG_VERSION"),
            wt_field,
            fp_field,
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

/// Allocate a copy of `src` using the Rust global allocator (compatible with
/// C `free()` on all supported platforms).
fn libc_alloc(src: &[u8]) -> *mut u8 {
    if src.is_empty() {
        // Return a non-null sentinel that is safe to free.
        let layout = std::alloc::Layout::from_size_align(1, 1).unwrap();
        return unsafe { std::alloc::alloc(layout) };
    }
    let layout = std::alloc::Layout::from_size_align(src.len(), 1).expect("layout");
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
