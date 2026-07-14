//! Supernode WebSocket background tasks.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use serde_json::Value;

use crate::identity::Identity;
use crate::protocol::{MessageType, SignalingMessage};

use super::internal::InternalEvent;

const PING_INTERVAL_S: u64 = 30;
/// Base reconnect delay after a successful session ends (clean close or mid-session error).
const RECONNECT_BASE: Duration = Duration::from_secs(1);
/// Cap on exponential backoff after repeated connect failures.
const RECONNECT_MAX: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Supernode WebSocket task
// ---------------------------------------------------------------------------

/// Why `connect_and_run_ws` returned.
#[derive(Debug, PartialEq, Eq)]
enum WsSessionEnd {
    /// Manager dropped the outbound channel — stop the task (no reconnect).
    ChannelClosed,
    /// Remote closed the socket (or EOF). Reconnect with a short delay.
    RemoteClosed,
}

/// Long-running tokio task that maintains a WebSocket connection to a
/// supernode, rotating through ordered `candidates` on failure.
///
/// Reconnects on both clean remote close and I/O errors until the manager
/// drops the outbound send channel (intentional teardown) or the task is
/// aborted.
pub(super) async fn supernode_ws_task(
    identity: Arc<Identity>,
    peer_id: String,
    candidates: Vec<String>,
    mut send_rx: mpsc::Receiver<WsMessage>,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    if candidates.is_empty() {
        warn!(
            "Supernode {} has no WebSocket candidates — task exiting",
            peer_id
        );
        return;
    }

    let mut backoff = RECONNECT_BASE;
    let mut candidate_idx: usize = 0;

    loop {
        let ws_url = &candidates[candidate_idx % candidates.len()];
        info!(
            "Connecting to supernode {} at {} (candidate {}/{})",
            peer_id,
            ws_url,
            candidate_idx % candidates.len() + 1,
            candidates.len()
        );
        match connect_and_run_ws(&identity, &peer_id, ws_url, &mut send_rx, &internal_tx).await {
            Ok(WsSessionEnd::ChannelClosed) => {
                info!(
                    "Supernode {} WebSocket task stopping (outbound channel closed)",
                    peer_id
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                break;
            }
            Ok(WsSessionEnd::RemoteClosed) => {
                info!(
                    "Supernode {} WebSocket closed; reconnecting in {:?}",
                    peer_id, RECONNECT_BASE
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                // Rotate candidate after a successful session ends so we probe
                // alternate endpoints if the primary is flapping.
                candidate_idx = candidate_idx.wrapping_add(1);
                backoff = RECONNECT_BASE;
                sleep(RECONNECT_BASE).await;
            }
            Err(e) => {
                warn!(
                    "Supernode {} WebSocket error on {}: {}; retry in {:?}",
                    peer_id, ws_url, e, backoff
                );
                let _ = internal_tx
                    .send(InternalEvent::WsDisconnected {
                        peer_id: peer_id.clone(),
                    })
                    .await;
                // Prefer the next candidate after a connect/runtime failure.
                candidate_idx = candidate_idx.wrapping_add(1);
                sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX);
            }
        }
    }
}

async fn connect_and_run_ws(
    identity: &Identity,
    peer_id: &str,
    ws_url: &str,
    send_rx: &mut mpsc::Receiver<WsMessage>,
    internal_tx: &mpsc::Sender<InternalEvent>,
) -> std::result::Result<WsSessionEnd, Box<dyn std::error::Error + Send + Sync>> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;

    let (ws_stream, _) = connect_async(ws_url).await?;
    info!("WebSocket connected to supernode {}", peer_id);
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // Send HELLO
    let hello = build_hello(identity)?;
    ws_sink.send(WsMessage::Text(hello)).await?;

    let _ = internal_tx
        .send(InternalEvent::WsConnected {
            peer_id: peer_id.to_owned(),
        })
        .await;

    // I/O loop
    let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_S));

    loop {
        tokio::select! {
            // Outbound — `None` means the manager dropped send_tx (teardown).
            msg = send_rx.recv() => {
                match msg {
                    Some(msg) => {
                        ws_sink.send(msg).await?;
                    }
                    None => {
                        let _ = ws_sink.send(WsMessage::Close(None)).await;
                        return Ok(WsSessionEnd::ChannelClosed);
                    }
                }
            }

            // Inbound
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<SignalingMessage>(&text) {
                            Ok(sm) => {
                                // Awaited send: inbound signaling must not be
                                // silently dropped when the manager is busy —
                                // backpressure the socket read instead.
                                let _ = internal_tx
                                    .send(InternalEvent::WsSignalingMessage {
                                        supernode_id: peer_id.to_owned(),
                                        msg: sm,
                                    })
                                    .await;
                            }
                            Err(e) => {
                                debug!("Ignoring non-signaling WS message: {}", e);
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => {
                        return Ok(WsSessionEnd::RemoteClosed);
                    }
                    Some(Ok(_)) => {} // binary, ping, pong — ignore
                    Some(Err(e)) => {
                        return Err(Box::new(e));
                    }
                }
            }

            // Keepalive
            _ = ping_interval.tick() => {
                ws_sink.send(WsMessage::Ping(vec![])).await?;
            }
        }
    }
}

pub(super) fn build_hello(identity: &Identity) -> std::result::Result<String, serde_json::Error> {
    let sender = identity.public_id();
    let mut msg = SignalingMessage::new(MessageType::Hello, sender.clone());
    msg.payload
        .insert("public_id".to_owned(), Value::String(sender.clone()));
    msg.payload
        .insert("peer_id".to_owned(), Value::String(identity.peer_id()));
    // Sign
    if let Ok(canonical) = msg.canonical_bytes() {
        let sig = identity.sign(&canonical);
        use base64::Engine;
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
    }
    msg.to_json()
}
