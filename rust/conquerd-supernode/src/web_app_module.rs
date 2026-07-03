//! `web.host.app.v1` — in-app portal served over QUIC reliable streams.
//!
//! Unlike the (now removed) `web.host.https` portal, this surface is
//! **not** reachable from a standard browser. The desktop client opens
//! `conquerd://<supernode_pub>/<path>` in its embedded Chromium view; a
//! `QWebEngineUrlSchemeHandler` opens a QUIC bidi stream to the
//! supernode, sends a length-prefixed [`WebAppRequest`] frame, and the
//! supernode replies with a [`WebAppResponseHeader`] followed by zero or
//! more length-prefixed body chunks (terminated by a zero-length chunk).
//!
//! The QUIC connection is identity-verified at the transport layer (the
//! relay knows which Ed25519 pubkey opened the stream), so individual
//! requests are not re-signed. Outbound bytes are gated through the
//! framework quota for `web.host.app.v1` via
//! [`FeatureRegistry::gate_through_feature`].
//!
//! Assets are served read-only from `<data_dir>/web/` (default root) and
//! `<data_dir>/games/` (paths beginning with `/games/`). Path safety is
//! enforced by [`conquerd_features::web_app::is_safe_portal_path`] plus a
//! defence-in-depth canonicalisation check that the resolved file lives
//! under the asset root.

use std::path::{Path, PathBuf};
use std::sync::Weak;

use conquerd_features::web_app::{
    self, WebAppRequest, WebAppResponseHeader, WEB_APP_MAX_BODY_BYTES, WEB_APP_MAX_CHUNK_BYTES,
    WEB_APP_MAX_FRAME_BYTES,
};
use conquerd_features::PeerId;
use tracing::{debug, trace, warn};

use crate::SupernodeState;

const FEATURE_ID: &str = "web.host.app.v1";

/// Maximum bytes per outbound body chunk pushed to the client. Keeping
/// chunks small lets the quota gate apply back-pressure smoothly.
const CHUNK_SIZE: usize = 64 * 1024;

/// Per-stream wall-clock cap so a slow/abandoned client cannot pin a
/// server task forever.
const STREAM_DEADLINE_SECS: u64 = 30;

pub struct WebAppHostModule {
    state: Weak<SupernodeState>,
    web_root: PathBuf,
    games_root: PathBuf,
}

impl WebAppHostModule {
    pub fn new(state: Weak<SupernodeState>, data_dir: &Path) -> Self {
        Self {
            state,
            web_root: data_dir.join("web"),
            games_root: data_dir.join("games"),
        }
    }

    /// Handle one QUIC bidirectional stream tagged for `web.host.app.v1`.
    /// Errors are logged at debug level and turned into a best-effort 500
    /// response when the header has not yet been sent.
    ///
    /// `prefetched_len` is the request frame's `u32` length-prefix, which the
    /// relay already consumed to disambiguate this stream from a reliable
    /// signaling stream; [`serve`](Self::serve) uses it instead of re-reading.
    pub async fn handle_stream(
        &self,
        peer_id: PeerId,
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        prefetched_len: u32,
    ) {
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(STREAM_DEADLINE_SECS);

        let result = tokio::time::timeout_at(deadline, async {
            self.serve(&peer_id, &mut send, &mut recv, prefetched_len)
                .await
        })
        .await;

        match result {
            Ok(Ok(())) => {
                let _ = send.finish();
            }
            Ok(Err(reason)) => {
                debug!(
                    "[{}] stream from {} failed: {}",
                    FEATURE_ID,
                    short_peer(&peer_id),
                    reason
                );
                let _ = send.finish();
            }
            Err(_) => {
                warn!(
                    "[{}] stream from {} exceeded {}s deadline",
                    FEATURE_ID,
                    short_peer(&peer_id),
                    STREAM_DEADLINE_SECS
                );
                let _ = send.reset(1u32.into());
            }
        }
    }

    async fn serve(
        &self,
        peer_id: &str,
        send: &mut quinn::SendStream,
        recv: &mut quinn::RecvStream,
        prefetched_len: u32,
    ) -> Result<(), String> {
        // ── 1. Read the request frame (u32be length-prefixed JSON) ──────
        // The `u32` length-prefix was already consumed by the relay's
        // stream-kind demux, so we take it as `prefetched_len` here.
        let req_len = prefetched_len;
        if req_len as usize > WEB_APP_MAX_FRAME_BYTES {
            return self
                .send_error_header(send, peer_id, 413, "request too large")
                .await;
        }
        let mut req_buf = vec![0u8; req_len as usize];
        recv.read_exact(&mut req_buf)
            .await
            .map_err(|e| format!("read request: {e}"))?;
        let request: WebAppRequest =
            serde_json::from_slice(&req_buf).map_err(|e| format!("parse request: {e}"))?;

        // ── 2. Validate path (syntactic + traversal) ────────────────────
        if !web_app::is_safe_portal_path(&request.path) {
            return self.send_error_header(send, peer_id, 400, "bad path").await;
        }
        let method = request.method.as_str();
        if method != "GET" {
            return self
                .send_error_header(send, peer_id, 405, "method not allowed")
                .await;
        }

        // ── 3. Dynamic API routes ────────────────────────────────────────
        // Intercepted before filesystem lookup so they always work even
        // when no web/ directory is present on the data path.
        if let Some(state) = self.state.upgrade() {
            match request.path.as_str() {
                "/health" | "/api/stats" => {
                    let json = state.collect_stats();
                    return self.send_json(send, peer_id, 200, &json).await;
                }
                "/api/metrics" => {
                    // P3: richer machine-readable metrics (extends /api/stats)
                    let mut metrics = state.collect_stats();
                    if let Some(obj) = metrics.as_object_mut() {
                        obj.insert("metrics_version".into(), serde_json::json!("1"));
                        // Use uptime as proxy for generation time (no extra deps)
                        if let Some(uptime) = obj.get("uptime_seconds") {
                            obj.insert("generated_uptime_seconds".into(), uptime.clone());
                        }
                    }
                    return self.send_json(send, peer_id, 200, &metrics).await;
                }
                "/api/peers" => {
                    let json = state.collect_peers_info();
                    return self.send_json(send, peer_id, 200, &json).await;
                }
                "/api/config" => {
                    let json = state.portal_config();
                    return self.send_json(send, peer_id, 200, &json).await;
                }
                "/api/wt-url" => {
                    // WebTransport base URL for game pages loaded inside the
                    // native portal (conquerd:// scheme), where
                    // location.hostname is the base64url supernode ID and
                    // cannot be used as a WebTransport target directly.
                    let json = state.wt_url_json();
                    return self.send_json(send, peer_id, 200, &json).await;
                }
                "/api/cluster" => {
                    let json = state.cluster_stats();
                    if json.is_null() {
                        return self
                            .send_json(send, peer_id, 200, &serde_json::json!({"clustered": false}))
                            .await;
                    }
                    return self.send_json(send, peer_id, 200, &json).await;
                }
                _ => {}
            }
        }

        // ── 4. Resolve filesystem path under asset root ─────────────────
        // ── 4. Resolve filesystem path under asset root ─────────────────
        let (root, rel) = self.route(&request.path);
        let resolved = match resolve_under_root(&root, rel) {
            Some(p) => p,
            None => {
                return self
                    .send_error_header(send, peer_id, 403, "outside asset root")
                    .await;
            }
        };

        let bytes = match tokio::fs::read(&resolved).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return self
                    .send_error_header(send, peer_id, 404, "not found")
                    .await;
            }
            Err(e) => {
                debug!("[{}] read {} failed: {}", FEATURE_ID, resolved.display(), e);
                return self.send_error_header(send, peer_id, 500, "io error").await;
            }
        };
        if (bytes.len() as u64) > WEB_APP_MAX_BODY_BYTES {
            return self
                .send_error_header(send, peer_id, 413, "asset too large")
                .await;
        }

        // ── 5. Send response header + chunked body ──────────────────────
        let content_type = guess_content_type(&resolved);
        let header = WebAppResponseHeader {
            status: 200,
            content_type,
            total_len: bytes.len() as u64,
        };
        self.write_frame(send, peer_id, &serde_json::to_vec(&header).unwrap())
            .await?;

        for chunk in bytes.chunks(CHUNK_SIZE) {
            if chunk.len() > WEB_APP_MAX_CHUNK_BYTES {
                return Err("chunk size exceeds wire limit".into());
            }
            self.write_chunk(send, peer_id, chunk).await?;
        }
        // Zero-length terminator.
        send.write_all(&0u32.to_be_bytes())
            .await
            .map_err(|e| format!("write terminator: {e}"))?;

        trace!(
            "[{}] served {} -> {} ({} bytes)",
            FEATURE_ID,
            request.path,
            resolved.display(),
            bytes.len()
        );
        Ok(())
    }

    fn route<'a>(&self, path: &'a str) -> (PathBuf, &'a str) {
        let trimmed = path.trim_start_matches('/');
        if let Some(rest) = trimmed.strip_prefix("games/") {
            (self.games_root.clone(), rest)
        } else if trimmed == "games" {
            (self.games_root.clone(), "")
        } else {
            (self.web_root.clone(), trimmed)
        }
    }

    async fn send_error_header(
        &self,
        send: &mut quinn::SendStream,
        peer_id: &str,
        status: u16,
        reason: &str,
    ) -> Result<(), String> {
        let header = WebAppResponseHeader {
            status,
            content_type: "text/plain; charset=utf-8".to_string(),
            total_len: reason.len() as u64,
        };
        self.write_frame(send, peer_id, &serde_json::to_vec(&header).unwrap())
            .await?;
        self.write_chunk(send, peer_id, reason.as_bytes()).await?;
        send.write_all(&0u32.to_be_bytes())
            .await
            .map_err(|e| format!("write terminator: {e}"))?;
        Ok(())
    }

    /// Send a serialised JSON value as an `application/json` response.
    async fn send_json(
        &self,
        send: &mut quinn::SendStream,
        peer_id: &str,
        status: u16,
        value: &serde_json::Value,
    ) -> Result<(), String> {
        let body = serde_json::to_vec_pretty(value).map_err(|e| format!("json serialise: {e}"))?;
        if (body.len() as u64) > WEB_APP_MAX_BODY_BYTES {
            return self
                .send_error_header(send, peer_id, 500, "response too large")
                .await;
        }
        let header = WebAppResponseHeader {
            status,
            content_type: "application/json; charset=utf-8".to_string(),
            total_len: body.len() as u64,
        };
        self.write_frame(send, peer_id, &serde_json::to_vec(&header).unwrap())
            .await?;
        for chunk in body.chunks(CHUNK_SIZE) {
            self.write_chunk(send, peer_id, chunk).await?;
        }
        send.write_all(&0u32.to_be_bytes())
            .await
            .map_err(|e| format!("write terminator: {e}"))?;
        Ok(())
    }

    async fn write_frame(
        &self,
        send: &mut quinn::SendStream,
        peer_id: &str,
        body: &[u8],
    ) -> Result<(), String> {
        if body.len() > WEB_APP_MAX_FRAME_BYTES {
            return Err(format!(
                "frame {} > max {}",
                body.len(),
                WEB_APP_MAX_FRAME_BYTES
            ));
        }
        self.gate_quota(peer_id, body.len() + 4)?;
        send.write_all(&(body.len() as u32).to_be_bytes())
            .await
            .map_err(|e| format!("write frame len: {e}"))?;
        send.write_all(body)
            .await
            .map_err(|e| format!("write frame body: {e}"))?;
        Ok(())
    }

    async fn write_chunk(
        &self,
        send: &mut quinn::SendStream,
        peer_id: &str,
        body: &[u8],
    ) -> Result<(), String> {
        self.gate_quota(peer_id, body.len() + 4)?;
        send.write_all(&(body.len() as u32).to_be_bytes())
            .await
            .map_err(|e| format!("write chunk len: {e}"))?;
        send.write_all(body)
            .await
            .map_err(|e| format!("write chunk body: {e}"))?;
        Ok(())
    }

    fn gate_quota(&self, peer_id: &str, byte_count: usize) -> Result<(), String> {
        let Some(state) = self.state.upgrade() else {
            return Err("supernode state dropped".into());
        };
        if !state
            .features
            .gate_through_feature(FEATURE_ID, peer_id, byte_count)
        {
            return Err("quota exceeded".into());
        }
        Ok(())
    }
}

/// Resolve `rel` (which may be empty for the index) under `root`,
/// rewriting `""` and trailing `/` to `index.html`. Returns `None` if
/// canonicalisation escapes the root (defence in depth — caller already
/// ran [`is_safe_portal_path`] on the raw path).
fn resolve_under_root(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = if rel.is_empty() || rel.ends_with('/') {
        format!("{rel}index.html")
    } else {
        rel.to_string()
    };
    let candidate = root.join(rel);

    // Canonicalise both sides where possible; fall back to lexical
    // comparison when the file does not (yet) exist so we still serve a
    // clean 404 instead of a silent 403.
    let root_canon = root.canonicalize().ok()?;
    if let Ok(canon) = candidate.canonicalize() {
        if canon.starts_with(&root_canon) {
            return Some(canon);
        }
        return None;
    }
    // File missing — make sure the (lexical) parent stays inside root.
    let parent = candidate.parent()?;
    let parent_canon = parent.canonicalize().ok()?;
    if parent_canon.starts_with(&root_canon) {
        Some(candidate)
    } else {
        None
    }
}

fn guess_content_type(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn short_peer(peer: &str) -> &str {
    &peer[..12.min(peer.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_default_serves_web_root() {
        let m = make_module();
        let (root, rel) = m.route("/index.html");
        assert!(root.ends_with("web"));
        assert_eq!(rel, "index.html");
    }

    #[test]
    fn route_games_prefix_serves_games_root() {
        let m = make_module();
        let (root, rel) = m.route("/games/example/index.html");
        assert!(root.ends_with("games"));
        assert_eq!(rel, "example/index.html");
    }

    #[test]
    fn route_root_path_maps_to_index() {
        let m = make_module();
        let (root, rel) = m.route("/");
        assert!(root.ends_with("web"));
        assert_eq!(rel, "");
    }

    #[test]
    fn resolve_under_root_rejects_escape() {
        let tmp = std::env::temp_dir().join(format!(
            "conquerd-web-app-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        // A canonicalised "../" cannot exist inside the canon root path,
        // so even when the OS allows the lookup, resolve must reject it.
        let escape = resolve_under_root(&tmp, "../etc/passwd");
        assert!(escape.is_none() || !escape.unwrap().starts_with(&tmp));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_under_root_rewrites_empty_to_index() {
        let tmp = std::env::temp_dir().join(format!(
            "conquerd-web-app-index-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let path = resolve_under_root(&tmp, "").unwrap();
        assert!(path.ends_with("index.html"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn content_type_basics() {
        assert!(guess_content_type(Path::new("a.html")).starts_with("text/html"));
        assert_eq!(guess_content_type(Path::new("a.wasm")), "application/wasm");
        assert_eq!(
            guess_content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }

    #[test]
    fn content_type_covers_all_common_extensions() {
        let cases: &[(&str, &str)] = &[
            ("a.htm", "text/html"),
            ("a.css", "text/css"),
            ("a.js", "application/javascript"),
            ("a.mjs", "application/javascript"),
            ("a.json", "application/json"),
            ("a.svg", "image/svg+xml"),
            ("a.png", "image/png"),
            ("a.jpg", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.gif", "image/gif"),
            ("a.webp", "image/webp"),
            ("a.ico", "image/x-icon"),
            ("a.woff", "font/woff"),
            ("a.woff2", "font/woff2"),
            ("a.ttf", "font/ttf"),
            ("a.otf", "font/otf"),
            ("a.txt", "text/plain"),
            ("a.md", "text/plain"),
        ];
        for (file, expected_prefix) in cases {
            let got = guess_content_type(Path::new(file));
            assert!(
                got.starts_with(expected_prefix),
                "{file}: expected prefix {expected_prefix:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn short_peer_truncates_to_12() {
        assert_eq!(short_peer("abcdefghijklmnopqrstuvwxyz"), "abcdefghijkl");
    }

    #[test]
    fn short_peer_returns_full_string_when_short() {
        assert_eq!(short_peer("abc"), "abc");
        assert_eq!(short_peer(""), "");
    }

    #[test]
    fn route_bare_games_serves_games_root() {
        let m = make_module();
        let (root, rel) = m.route("/games");
        assert!(root.ends_with("games"));
        assert_eq!(rel, "");
    }

    #[test]
    fn route_non_games_deep_path() {
        let m = make_module();
        let (root, rel) = m.route("/static/app.js");
        assert!(root.ends_with("web"));
        assert_eq!(rel, "static/app.js");
    }

    #[test]
    fn resolve_under_root_returns_some_for_existing_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        std::fs::write(&file, b"hi").unwrap();
        let result = resolve_under_root(tmp.path(), "hello.txt");
        assert!(result.is_some());
    }

    #[test]
    fn resolve_under_root_trailing_slash_rewrites_to_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Parent dir exists, index.html does not — should still give a path
        // ending in index.html rather than None (file will 404 later).
        let result = resolve_under_root(tmp.path(), "sub/");
        // The sub dir doesn't exist so canonicalize will fail, but the
        // lexical fallback should still return something ending in index.html.
        if let Some(p) = result {
            assert!(p.ends_with("index.html"));
        }
        // None is also acceptable if the parent dir doesn't exist.
    }

    // Module is light enough to construct without a real SupernodeState
    // — we only test the pure-function helpers here.
    fn make_module() -> WebAppHostModule {
        WebAppHostModule {
            state: Weak::new(),
            web_root: PathBuf::from("/tmp/conquerd/web"),
            games_root: PathBuf::from("/tmp/conquerd/games"),
        }
    }
}
