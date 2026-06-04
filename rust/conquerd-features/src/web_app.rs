//! Wire format for `web.host.app.v1` — the in-app QUIC-stream portal.
//!
//! The desktop client's embedded Chromium view renders supernode portal
//! pages via a custom `conquerd://<supernode_pub>/<path>` URL scheme. The
//! scheme handler opens a **fresh QUIC bidirectional stream** tagged with
//! `web.host.app.v1` for every page asset request and walks this protocol:
//!
//! 1. Client writes one length-prefixed JSON [`WebAppRequest`] frame.
//! 2. Supernode writes one length-prefixed JSON [`WebAppResponseHeader`]
//!    frame describing the response.
//! 3. Supernode writes zero or more length-prefixed **binary body
//!    chunks**, terminated by a single zero-length chunk.
//! 4. Either side closes the stream.
//!
//! The QUIC connection is already identity-verified (the supernode
//! learned the client's Ed25519 pub key during the handshake), so
//! individual request frames are *not* re-signed. The capability auth
//! tier is [`AuthTier::Public`](crate::descriptor::AuthTier::Public): any
//! peer with a live session may fetch portal assets, subject to the
//! per-feature quota enforced by [`crate::FeatureRegistry`].
//!
//! Framing rationale: a u32 BE length prefix is symmetric with
//! [`wellknown::transport_quic_stream_v1`](crate::wellknown::transport_quic_stream_v1)
//! and trivially streamable from both Rust and the C++ scheme-handler
//! shim on the client side. Body chunks are raw bytes (no per-chunk
//! envelope) so large images / wasm blobs avoid base64 overhead.

use serde::{Deserialize, Serialize};

/// Hard cap on a single request/header JSON frame (16 KiB). Pages cannot
/// influence this; it bounds the parser's heap on either side.
pub const WEB_APP_MAX_FRAME_BYTES: usize = 16 * 1024;

/// Hard cap on a single body chunk (1 MiB). Large assets are split into
/// multiple chunks.
pub const WEB_APP_MAX_CHUNK_BYTES: usize = 1024 * 1024;

/// Hard cap on a complete response body (32 MiB). Larger assets must be
/// streamed via a different feature (e.g. `core.file.v1`).
pub const WEB_APP_MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// Client → supernode opening frame.
///
/// `path` is the supernode-relative asset path (URL-decoded, must start
/// with `/`, must not contain `..` or `\\` — the server re-validates
/// regardless). `method` is currently always `"GET"`; future revisions
/// may add `"HEAD"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAppRequest {
    pub path: String,
    #[serde(default = "default_method")]
    pub method: String,
    /// Optional per-page query string (raw, without leading `?`). The
    /// supernode may ignore it for static assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

fn default_method() -> String {
    "GET".to_string()
}

/// Supernode → client response header.
///
/// `status` follows HTTP conventions (200 OK, 404 Not Found, 416 Range
/// Not Satisfiable, 500 Internal). `total_len` is the total body byte
/// count (sum of all subsequent chunks); zero means "no body".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebAppResponseHeader {
    pub status: u16,
    pub content_type: String,
    pub total_len: u64,
}

/// Errors raised while parsing a wire frame.
#[derive(Debug, thiserror::Error)]
pub enum WebAppWireError {
    #[error("frame exceeds {limit}-byte limit")]
    FrameTooLarge { limit: usize },
    #[error("chunk exceeds {limit}-byte limit")]
    ChunkTooLarge { limit: usize },
    #[error("body exceeds {limit}-byte limit")]
    BodyTooLarge { limit: u64 },
    #[error("invalid json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("path is not a safe portal-relative path")]
    UnsafePath,
}

/// Encode a JSON-serializable frame as `[u32 BE len][json]`. Returns
/// [`WebAppWireError::FrameTooLarge`] if the encoded payload exceeds
/// [`WEB_APP_MAX_FRAME_BYTES`].
pub fn encode_json_frame<T: Serialize>(frame: &T) -> Result<Vec<u8>, WebAppWireError> {
    let body = serde_json::to_vec(frame)?;
    if body.len() > WEB_APP_MAX_FRAME_BYTES {
        return Err(WebAppWireError::FrameTooLarge {
            limit: WEB_APP_MAX_FRAME_BYTES,
        });
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Encode a single body chunk as `[u32 BE len][bytes]`. Pass an empty
/// slice to produce the terminating zero-length chunk that ends a
/// response body.
pub fn encode_body_chunk(chunk: &[u8]) -> Result<Vec<u8>, WebAppWireError> {
    if chunk.len() > WEB_APP_MAX_CHUNK_BYTES {
        return Err(WebAppWireError::ChunkTooLarge {
            limit: WEB_APP_MAX_CHUNK_BYTES,
        });
    }
    let mut out = Vec::with_capacity(4 + chunk.len());
    out.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk);
    Ok(out)
}

/// Defence-in-depth path validation. The supernode-side serving code
/// must additionally canonicalize against the asset root before opening
/// any file. This function only rejects shapes that have no business in
/// a portal-relative request.
pub fn is_safe_portal_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    if path.len() > 2048 {
        return false;
    }
    for seg in path.split('/') {
        if seg == ".." || seg.contains('\\') || seg.contains('\0') {
            return false;
        }
    }
    !path.contains("//")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let r = WebAppRequest {
            path: "/index.html".into(),
            method: "GET".into(),
            query: None,
        };
        let enc = encode_json_frame(&r).unwrap();
        assert_eq!(&enc[..4], &(enc.len() as u32 - 4).to_be_bytes());
        let body: WebAppRequest = serde_json::from_slice(&enc[4..]).unwrap();
        assert_eq!(body.path, "/index.html");
        assert_eq!(body.method, "GET");
    }

    #[test]
    fn safe_path_accepts_simple_paths() {
        assert!(is_safe_portal_path("/"));
        assert!(is_safe_portal_path("/index.html"));
        assert!(is_safe_portal_path("/assets/logo.png"));
        assert!(is_safe_portal_path("/games/example/main.js"));
    }

    #[test]
    fn safe_path_rejects_traversal_and_weirdness() {
        assert!(!is_safe_portal_path(""));
        assert!(!is_safe_portal_path("index.html"));
        assert!(!is_safe_portal_path("/../etc/passwd"));
        assert!(!is_safe_portal_path("/foo/../bar"));
        assert!(!is_safe_portal_path("/foo\\bar"));
        assert!(!is_safe_portal_path("/foo//bar"));
        assert!(!is_safe_portal_path("/foo\0.html"));
        // 2 KiB limit
        assert!(!is_safe_portal_path(&format!("/{}", "a".repeat(3000))));
    }

    #[test]
    fn frame_too_large_rejected() {
        let huge = WebAppRequest {
            path: format!("/{}", "x".repeat(WEB_APP_MAX_FRAME_BYTES)),
            method: "GET".into(),
            query: None,
        };
        assert!(matches!(
            encode_json_frame(&huge).unwrap_err(),
            WebAppWireError::FrameTooLarge { .. }
        ));
    }

    #[test]
    fn body_chunk_round_trips() {
        let enc = encode_body_chunk(b"<html></html>").unwrap();
        assert_eq!(
            u32::from_be_bytes(enc[..4].try_into().unwrap()) as usize,
            13
        );
        assert_eq!(&enc[4..], b"<html></html>");
        // Zero-length terminator.
        let term = encode_body_chunk(b"").unwrap();
        assert_eq!(term, vec![0, 0, 0, 0]);
    }

    #[test]
    fn chunk_too_large_rejected() {
        let big = vec![0u8; WEB_APP_MAX_CHUNK_BYTES + 1];
        assert!(matches!(
            encode_body_chunk(&big).unwrap_err(),
            WebAppWireError::ChunkTooLarge { .. }
        ));
    }
}
