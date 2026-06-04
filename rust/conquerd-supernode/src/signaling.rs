// ConquerD Supernode — signaling.rs
// WebSocket signaling server: accept connections, verify signatures, relay messages.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use parking_lot::RwLock;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::protocol::{MessageType, SignalingMessage};
use conquerd_features::ReplayGuard;

/// A connected peer's write channel.
type PeerTx = mpsc::UnboundedSender<String>;

/// Shared signaling state.
pub struct SignalingState {
    /// identity_pub → sender channel
    pub peer_sockets: HashMap<String, PeerTx>,
    /// Number of connected peers
    pub connected_count: usize,
}

impl SignalingState {
    pub fn new() -> Self {
        Self {
            peer_sockets: HashMap::new(),
            connected_count: 0,
        }
    }
}

/// Callback trait for the supernode to handle messages.
pub trait SignalingHandler: Send + Sync + 'static {
    /// Called for every verified message targeting us or broadcast.
    fn on_message(&self, msg: SignalingMessage, raw: &str);

    /// Called when a peer first connects.
    fn on_peer_connected(&self, identity_pub: &str);

    /// Called when a peer disconnects.
    fn on_peer_disconnected(&self, identity_pub: &str);
}

/// The WebSocket signaling server.
pub struct SignalingServer {
    state: Arc<RwLock<SignalingState>>,
    our_id: String,
    /// Sliding-window replay guard shared across all connections. Rejects
    /// re-delivery of an already-seen signed message within the freshness
    /// window, complementing the per-message `is_fresh` timestamp check.
    replay_guard: Arc<ReplayGuard>,
}

impl SignalingServer {
    pub fn new(our_id: String) -> Self {
        Self {
            state: Arc::new(RwLock::new(SignalingState::new())),
            our_id,
            replay_guard: Arc::new(ReplayGuard::new(300.0)),
        }
    }

    pub(crate) fn state(&self) -> Arc<RwLock<SignalingState>> {
        self.state.clone()
    }

    /// Send a raw JSON message to a specific peer.
    pub fn send_to_peer(&self, identity_pub: &str, json: &str) -> bool {
        let st = self.state.read();
        if let Some(tx) = st.peer_sockets.get(identity_pub) {
            tx.send(json.to_string()).is_ok()
        } else {
            false
        }
    }

    /// Check if a peer is currently connected.
    pub fn is_peer_connected(&self, identity_pub: &str) -> bool {
        self.state.read().peer_sockets.contains_key(identity_pub)
    }

    /// Get all connected peer IDs.
    pub fn connected_peer_ids(&self) -> Vec<String> {
        self.state.read().peer_sockets.keys().cloned().collect()
    }

    /// Start the signaling server. Returns bound port.
    pub async fn start(
        &self,
        bind_addr: SocketAddr,
        handler: Arc<dyn SignalingHandler>,
    ) -> std::io::Result<u16> {
        let listener = TcpListener::bind(bind_addr).await?;
        let port = listener.local_addr()?.port();
        info!("WebSocket signaling on {}", listener.local_addr()?);

        let state = self.state.clone();
        let our_id = self.our_id.clone();
        let replay_guard = self.replay_guard.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        let state = state.clone();
                        let handler = handler.clone();
                        let our_id = our_id.clone();
                        let replay_guard = replay_guard.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_ws_connection(
                                stream,
                                addr,
                                state,
                                handler,
                                &our_id,
                                replay_guard,
                            )
                            .await
                            {
                                debug!("WS connection error from {}: {}", addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        error!("WS accept error: {}", e);
                    }
                }
            }
        });

        Ok(port)
    }
}

/// Handle a single WebSocket connection.
async fn handle_ws_connection(
    stream: TcpStream,
    addr: SocketAddr,
    state: Arc<RwLock<SignalingState>>,
    handler: Arc<dyn SignalingHandler>,
    our_id: &str,
    replay_guard: Arc<ReplayGuard>,
) -> anyhow::Result<()> {
    let ws = accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    // Writer task: forward queued messages to WebSocket
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_tx.send(Message::Text(msg)).await.is_err() {
                break;
            }
        }
    });

    let mut peer_id: Option<String> = None;

    // Per-connection rate limiter: max 60 signaling messages per 10 seconds.
    const RATE_MAX: u32 = 60;
    const RATE_WINDOW_SECS: u64 = 10;
    let mut rate_count: u32 = 0;
    let mut rate_window_start = std::time::Instant::now();

    // Read loop
    while let Some(msg) = ws_rx.next().await {
        let msg = match msg {
            Ok(Message::Text(t)) => t.to_string(),
            Ok(Message::Close(_)) => break,
            Ok(_) => continue,
            Err(_) => break,
        };

        // Enforce 256 KiB per-message size cap before any allocation/parsing.
        if msg.len() > 262_144 {
            warn!(
                "Oversized signaling message from {} ({} bytes) — dropping",
                addr,
                msg.len()
            );
            continue;
        }

        // Quick parse to extract message type before rate-limit decision.
        // Signature verification (expensive) happens below after the rate check.
        let Ok(parsed) = SignalingMessage::from_json(&msg) else {
            continue;
        };

        // Per-connection rate limit for control messages.
        // SFU audio frames arrive at up to 50 Hz and must not be counted
        // against the control-message budget — they get their own budget.
        if parsed.msg_type != MessageType::SfuAudio {
            if rate_window_start.elapsed().as_secs() >= RATE_WINDOW_SECS {
                rate_window_start = std::time::Instant::now();
                rate_count = 0;
            }
            rate_count += 1;
            if rate_count > RATE_MAX {
                warn!(
                    "Signaling rate limit exceeded from {} — dropping message",
                    addr
                );
                continue;
            }
        }

        // Verify Ed25519 signature + basic timestamp freshness (P0 replay protection).
        if !parsed.verify() || !parsed.is_fresh(300.0) {
            let canonical = parsed.canonical_bytes();
            let canonical_hex = {
                use sha2::{Digest, Sha256};
                let mut hasher = Sha256::new();
                hasher.update(&canonical);
                hex::encode(hasher.finalize())
            };
            warn!(
                "Invalid signature from {} — type={:?} sender_len={} sig={} canonical_len={} canonical_sha256={} raw_json={}",
                addr,
                parsed.msg_type,
                parsed.sender.len(),
                parsed.signature.as_deref().unwrap_or("NONE"),
                canonical.len(),
                canonical_hex,
                &msg[..msg.len().min(500)],
            );
            continue;
        }

        // Sliding-window replay guard: drop re-delivery of an already-seen
        // signed message within the freshness window. Real-time SFU audio is
        // exempt (high rate, ephemeral, already covered by the freshness
        // window) so it cannot flood the per-sender window.
        if parsed.msg_type != MessageType::SfuAudio {
            let fresh = parsed
                .signature
                .as_deref()
                .map(|sig| replay_guard.check_and_record(&parsed.sender, sig.as_bytes()))
                .unwrap_or(false);
            if !fresh {
                warn!(
                    "Replayed or unsigned {:?} from {} — dropping",
                    parsed.msg_type,
                    &parsed.sender[..12.min(parsed.sender.len())],
                );
                continue;
            }
        }

        // Register peer socket on first message
        if peer_id.is_none() {
            peer_id = Some(parsed.sender.clone());
            let mut st = state.write();
            let replaced = st.peer_sockets.insert(parsed.sender.clone(), tx.clone());
            if replaced.is_none() {
                st.connected_count += 1;
            } else {
                debug!(
                    "Peer {} reconnected from {} — replacing previous socket",
                    &parsed.sender[..12.min(parsed.sender.len())],
                    addr,
                );
            }
            drop(st);
            handler.on_peer_connected(&parsed.sender);
            debug!(
                "Peer connected via WS: {} from {}",
                &parsed.sender[..12.min(parsed.sender.len())],
                addr
            );
        }

        // Relay to target if not for us
        if let Some(ref target) = parsed.target {
            if target != our_id {
                let st = state.read();
                if let Some(target_tx) = st.peer_sockets.get(target) {
                    let _ = target_tx.send(msg.clone());
                    debug!(
                        "Relayed {:?} from {} → {}",
                        parsed.msg_type,
                        &parsed.sender[..12.min(parsed.sender.len())],
                        &target[..12.min(target.len())],
                    );
                } else {
                    debug!(
                        "Relay target {} not connected — dropping {:?} from {}",
                        &target[..12.min(target.len())],
                        parsed.msg_type,
                        &parsed.sender[..12.min(parsed.sender.len())],
                    );
                }
                // Also deliver to handler for bookkeeping (e.g. endpoint_update store)
            }
        }

        // Deliver to handler
        handler.on_message(parsed, &msg);
    }

    // Cleanup — only remove socket if it's still ours (not replaced by a newer connection)
    if let Some(ref pid) = peer_id {
        let mut st = state.write();
        let is_ours = st
            .peer_sockets
            .get(pid)
            .is_some_and(|stored| stored.same_channel(&tx));
        if is_ours {
            st.peer_sockets.remove(pid);
            st.connected_count = st.connected_count.saturating_sub(1);
            drop(st);
            replay_guard.forget_peer(pid);
            handler.on_peer_disconnected(pid);
            debug!(
                "Peer disconnected: {} from {}",
                &pid[..12.min(pid.len())],
                addr
            );
        } else {
            drop(st);
            debug!(
                "Peer {} socket already replaced — skipping disconnect from {}",
                &pid[..12.min(pid.len())],
                addr,
            );
        }
    }

    write_task.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    // ── SignalingState ──────────────────────────────────────────────────────

    #[test]
    fn signaling_state_new_is_empty() {
        let s = SignalingState::new();
        assert!(s.peer_sockets.is_empty());
        assert_eq!(s.connected_count, 0);
    }

    // ── SignalingServer synchronous methods ─────────────────────────────────

    #[test]
    fn new_server_has_no_connected_peers() {
        let srv = SignalingServer::new("supernode-id".into());
        assert!(srv.connected_peer_ids().is_empty());
    }

    #[test]
    fn is_peer_connected_returns_false_for_unknown_peer() {
        let srv = SignalingServer::new("supernode-id".into());
        assert!(!srv.is_peer_connected("peer-x"));
    }

    #[test]
    fn send_to_peer_returns_false_for_unknown_peer() {
        let srv = SignalingServer::new("supernode-id".into());
        assert!(!srv.send_to_peer("peer-x", r#"{"type":"ping"}"#));
    }

    #[test]
    fn connected_peer_ids_returns_empty_for_fresh_server() {
        let srv = SignalingServer::new("supernode-id".into());
        let ids = srv.connected_peer_ids();
        assert!(ids.is_empty());
    }

    #[test]
    fn is_peer_connected_returns_true_after_manual_register() {
        let srv = SignalingServer::new("supernode-id".into());
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        {
            let mut st = srv.state.write();
            st.peer_sockets.insert("peer-a".into(), tx);
            st.connected_count += 1;
        }
        assert!(srv.is_peer_connected("peer-a"));
        assert!(!srv.is_peer_connected("peer-b"));
    }

    #[test]
    fn send_to_peer_returns_true_while_channel_is_open() {
        let srv = SignalingServer::new("supernode-id".into());
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        {
            let mut st = srv.state.write();
            st.peer_sockets.insert("peer-a".into(), tx);
        }
        let ok = srv.send_to_peer("peer-a", r#"{"type":"ping"}"#);
        assert!(ok);
        // verify the message actually arrived
        let msg = rx.try_recv().expect("message should be in channel");
        assert_eq!(msg, r#"{"type":"ping"}"#);
    }

    #[test]
    fn send_to_peer_returns_false_after_receiver_dropped() {
        let srv = SignalingServer::new("supernode-id".into());
        let (tx, rx) = mpsc::unbounded_channel::<String>();
        {
            let mut st = srv.state.write();
            st.peer_sockets.insert("peer-a".into(), tx);
        }
        drop(rx); // close the receiving end
        assert!(!srv.send_to_peer("peer-a", r#"{"type":"ping"}"#));
    }

    #[test]
    fn connected_peer_ids_lists_all_registered_peers() {
        let srv = SignalingServer::new("supernode-id".into());
        let (tx1, _rx1) = mpsc::unbounded_channel::<String>();
        let (tx2, _rx2) = mpsc::unbounded_channel::<String>();
        {
            let mut st = srv.state.write();
            st.peer_sockets.insert("peer-a".into(), tx1);
            st.peer_sockets.insert("peer-b".into(), tx2);
        }
        let mut ids = srv.connected_peer_ids();
        ids.sort();
        assert_eq!(ids, vec!["peer-a".to_string(), "peer-b".to_string()]);
    }
}
