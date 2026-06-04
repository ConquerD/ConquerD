//! Native Rust QUIC relay client (slim).
//!
//! Connects to a supernode's QUIC relay listener over mTLS using the
//! caller's Ed25519 identity. The relay grants access based on the
//! certificate CN (peer_id) — no explicit ticket payload is sent on the
//! wire; the ticket is validated client-side only (expiry / host / port).
//!
//! This client is intentionally minimal. It is used by:
//!   * [`crate::web_app_client`] — to open a bidirectional QUIC stream
//!     tagged `web.host.app.v1` and fetch in-app portal assets.
//!
//! Wire format reused for relay command stream draining (best-effort,
//! discarded): supernode pushes length-prefixed JSON frames via
//! `conn.open_uni()` (welcome, peer_joined, peer_left, relay_punch).
//! See `rust/conquerd-supernode/src/wire.rs::encode_relay_cmd`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use quinn::{Connection, Endpoint};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Max time we'll wait for the mTLS handshake to complete.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-relay-cmd frame size sanity cap (matches supernode wire).
const RELAY_CMD_MAX_FRAME: usize = 64 * 1024;

/// A live QUIC connection to a supernode's relay listener.
///
/// Holds a [`quinn::Connection`] handle plus a `shutdown` notifier that
/// stops the background drain task on drop / explicit close.
pub struct QuicRelayClient {
    supernode_id: String,
    relay_host: String,
    relay_port: u16,
    connection: Connection,
    shutdown: Arc<Notify>,
}

impl std::fmt::Debug for QuicRelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicRelayClient")
            .field("supernode_id", &self.supernode_id)
            .field("relay_host", &self.relay_host)
            .field("relay_port", &self.relay_port)
            .field("alive", &self.connection.close_reason().is_none())
            .finish()
    }
}

impl QuicRelayClient {
    /// Connect to `host:port` over the supplied `endpoint`. The endpoint
    /// must already be configured with the caller's Ed25519 client cert
    /// (see [`crate::quic_tls::make_quic_endpoint`]) — that cert's CN is
    /// what the supernode authorises against its `allowed` set.
    ///
    /// `supernode_id` is the supernode's identity pubkey (peer_id); kept
    /// here so callers can identify the relay later without re-parsing
    /// the cert.
    pub async fn connect(
        endpoint: &Endpoint,
        supernode_id: impl Into<String>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<Self> {
        let supernode_id = supernode_id.into();
        let addr: SocketAddr = format!("{host}:{port}")
            .parse()
            .with_context(|| format!("invalid relay address {host}:{port}"))?;

        // `server_name` is arbitrary for self-signed certs; the supernode
        // does not validate it. We use a fixed string for clarity.
        let connecting = endpoint
            .connect(addr, "conquerd")
            .with_context(|| format!("relay endpoint.connect({addr}) failed"))?;

        let connection = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| anyhow!("relay handshake to {addr} timed out"))?
            .with_context(|| format!("relay handshake to {addr} failed"))?;

        info!(
            "[relay] connected to {}:{} (supernode {})",
            host,
            port,
            &supernode_id[..12.min(supernode_id.len())]
        );

        let shutdown = Arc::new(Notify::new());

        // Background drain — keeps the recv side moving so quinn doesn't
        // stall and the supernode's bookkeeping (welcome, peer_joined,
        // ...) doesn't pile up. We don't act on the messages here; the
        // bookkeeping needed for `web.host.app.v1` fetches is none.
        let conn_drain = connection.clone();
        let shutdown_drain = shutdown.clone();
        let sn_short = supernode_id[..12.min(supernode_id.len())].to_owned();
        tokio::spawn(async move {
            drain_relay_commands(conn_drain, shutdown_drain, sn_short).await;
        });

        Ok(Self {
            supernode_id,
            relay_host: host.to_owned(),
            relay_port: port,
            connection,
            shutdown,
        })
    }

    /// Underlying connection handle (cheap clone). Used by feature
    /// clients (e.g. [`crate::web_app_client`]) to open new bidi
    /// streams.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Identity pubkey of the supernode this relay is hosted on.
    pub fn supernode_id(&self) -> &str {
        &self.supernode_id
    }

    pub fn relay_host(&self) -> &str {
        &self.relay_host
    }

    pub fn relay_port(&self) -> u16 {
        self.relay_port
    }

    /// True while quinn has not surfaced a close reason.
    pub fn is_alive(&self) -> bool {
        self.connection.close_reason().is_none()
    }

    /// Gracefully close the relay connection and stop the drain task.
    pub fn close(&self) {
        self.connection.close(0u32.into(), b"client_shutdown");
        self.shutdown.notify_waiters();
    }
}

impl Drop for QuicRelayClient {
    fn drop(&mut self) {
        // close_reason is cheap and idempotent. Always notify the drain
        // task so the tokio::spawn handle becomes joinable.
        if self.connection.close_reason().is_none() {
            self.connection.close(0u32.into(), b"dropped");
        }
        self.shutdown.notify_waiters();
    }
}

/// Best-effort consumer of the supernode's relay command stream.
/// We don't currently route these events anywhere (no SFU in native
/// client yet); the loop's sole purpose is to keep the recv buffer
/// drained so quinn doesn't apply backpressure.
async fn drain_relay_commands(conn: Connection, shutdown: Arc<Notify>, sn_short: String) {
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            accept = conn.accept_uni() => {
                match accept {
                    Ok(mut recv) => {
                        // Length-prefixed: 4-byte BE length + JSON. We
                        // cap the frame size to avoid runaway allocations.
                        let mut len_buf = [0u8; 4];
                        if recv.read_exact(&mut len_buf).await.is_err() {
                            continue;
                        }
                        let len = u32::from_be_bytes(len_buf) as usize;
                        if len == 0 || len > RELAY_CMD_MAX_FRAME {
                            warn!(
                                "[relay {}] bad cmd frame len={} — dropping stream",
                                sn_short, len
                            );
                            continue;
                        }
                        let mut body = vec![0u8; len];
                        if recv.read_exact(&mut body).await.is_err() {
                            continue;
                        }
                        debug!(
                            "[relay {}] cmd ({} bytes) drained",
                            sn_short,
                            body.len()
                        );
                    }
                    Err(quinn::ConnectionError::ApplicationClosed(_))
                    | Err(quinn::ConnectionError::LocallyClosed) => break,
                    Err(e) => {
                        debug!("[relay {}] accept_uni ended: {}", sn_short, e);
                        break;
                    }
                }
            }
        }
    }
    debug!("[relay {}] drain task exiting", sn_short);
}
