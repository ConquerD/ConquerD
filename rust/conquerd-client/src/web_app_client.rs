//! Client for the supernode's `web.host.app.v1` feature.
//!
//! Opens a fresh QUIC bidirectional stream per request and walks the
//! wire protocol defined in [`conquerd_features::web_app`]:
//!
//!   1. Write `[u32 BE len][WebAppRequest JSON]`.
//!   2. Half-close the send side (FIN) so the server knows the request
//!      is complete.
//!   3. Read `[u32 BE len][WebAppResponseHeader JSON]`.
//!   4. Read `[u32 BE len][bytes]` chunks until a zero-length chunk
//!      terminates the body.
//!
//! Consumed by the Qt `QWebEngineUrlSchemeHandler` shim (added in a
//! later phase) which translates `conquerd://<supernode>/<path>` URLs
//! into `fetch()` calls against the active relay connection.

use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use conquerd_features::web_app::{
    encode_json_frame, is_safe_portal_path, WebAppRequest, WebAppResponseHeader,
    WEB_APP_MAX_BODY_BYTES, WEB_APP_MAX_CHUNK_BYTES, WEB_APP_MAX_FRAME_BYTES,
};
use quinn::Connection;
use tracing::debug;

pub const FEATURE_ID: &str = "web.host.app.v1";

/// End-to-end deadline for a single fetch — must match the supernode's
/// `WEB_APP_STREAM_DEADLINE` so neither side stalls the other.
pub const FETCH_DEADLINE: Duration = Duration::from_secs(30);

/// A successfully-fetched response from the supernode portal.
#[derive(Debug, Clone)]
pub struct WebAppResponse {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

/// Fetch `path` from the supernode reachable via `conn`.
///
/// `path` MUST start with `/` and pass [`is_safe_portal_path`]; this is
/// re-checked client-side as defence-in-depth before any bytes hit the
/// wire. The whole operation is bounded by [`FETCH_DEADLINE`].
///
/// Errors:
///   * `anyhow::Error("unsafe path")` if `path` fails validation.
///   * `anyhow::Error("deadline …")` if the supernode doesn't complete
///     the response in time.
///   * `anyhow::Error("stream …")` for any IO or framing fault.
pub async fn fetch(
    conn: &Connection,
    path: &str,
    query: Option<&str>,
) -> anyhow::Result<WebAppResponse> {
    if !is_safe_portal_path(path) {
        bail!("unsafe portal path: {path:?}");
    }

    tokio::time::timeout(FETCH_DEADLINE, fetch_inner(conn, path, query))
        .await
        .map_err(|_| anyhow!("fetch deadline elapsed for {path}"))?
}

async fn fetch_inner(
    conn: &Connection,
    path: &str,
    query: Option<&str>,
) -> anyhow::Result<WebAppResponse> {
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .with_context(|| format!("open_bi for {FEATURE_ID}"))?;

    // ── 1. Write request frame ──────────────────────────────────────────
    let request = WebAppRequest {
        path: path.to_owned(),
        method: "GET".to_owned(),
        query: query.map(|q| q.to_owned()),
    };
    let frame = encode_json_frame(&request).map_err(|e| anyhow!("encode request: {e}"))?;
    send.write_all(&frame)
        .await
        .with_context(|| "write request frame")?;
    // Half-close the send side so the supernode's `read_exact` for the
    // request frame completes without waiting for more bytes.
    send.finish().context("finish send side")?;

    // ── 2. Read response header ─────────────────────────────────────────
    let header_bytes = read_length_prefixed(&mut recv, WEB_APP_MAX_FRAME_BYTES)
        .await
        .context("read response header")?;
    let header: WebAppResponseHeader =
        serde_json::from_slice(&header_bytes).context("decode response header JSON")?;

    if header.total_len > WEB_APP_MAX_BODY_BYTES {
        bail!(
            "response total_len {} exceeds cap {}",
            header.total_len,
            WEB_APP_MAX_BODY_BYTES
        );
    }

    // ── 3. Read body chunks until zero-length terminator ────────────────
    let mut body: Vec<u8> = Vec::with_capacity(header.total_len.min(64 * 1024) as usize);
    loop {
        let mut len_buf = [0u8; 4];
        match recv.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(_)) => {
                bail!("supernode closed body stream without zero-length terminator");
            }
            Err(e) => return Err(anyhow!("read chunk len: {e}")),
        }
        let chunk_len = u32::from_be_bytes(len_buf) as usize;
        if chunk_len == 0 {
            break;
        }
        if chunk_len > WEB_APP_MAX_CHUNK_BYTES {
            bail!("chunk {chunk_len} exceeds cap {WEB_APP_MAX_CHUNK_BYTES}");
        }
        if (body.len() as u64 + chunk_len as u64) > WEB_APP_MAX_BODY_BYTES {
            bail!("body would exceed cap {WEB_APP_MAX_BODY_BYTES}");
        }
        let start = body.len();
        body.resize(start + chunk_len, 0);
        recv.read_exact(&mut body[start..])
            .await
            .map_err(|e| anyhow!("read chunk body: {e}"))?;
    }

    debug!(
        "[{FEATURE_ID}] GET {} -> {} ({} bytes, {})",
        path,
        header.status,
        body.len(),
        header.content_type,
    );

    Ok(WebAppResponse {
        status: header.status,
        content_type: header.content_type,
        body,
    })
}

/// Read `[u32 BE len][bytes]` where `len <= cap`.
async fn read_length_prefixed(recv: &mut quinn::RecvStream, cap: usize) -> anyhow::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow!("read len prefix: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(Vec::new());
    }
    if len > cap {
        bail!("framed payload {len} exceeds cap {cap}");
    }
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .map_err(|e| anyhow!("read framed body: {e}"))?;
    Ok(buf)
}
