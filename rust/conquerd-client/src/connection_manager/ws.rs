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
// ---------------------------------------------------------------------------
// Supernode WebSocket task
// ---------------------------------------------------------------------------

/// Long-running tokio task that maintains a WebSocket connection to a
/// supernode, sends identity hello, and routes inbound messages back to
/// the connection manager via `internal_tx`.
pub(super) async fn supernode_ws_task(
    identity: Arc<Identity>,
    peer_id: String,
    ws_url: String,
    mut send_rx: mpsc::Receiver<WsMessage>,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    let mut backoff = Duration::from_secs(1);

    loop {
        info!("Connecting to supernode {} at {}", peer_id, ws_url);
        match connect_and_run_ws(&identity, &peer_id, &ws_url, &mut send_rx, &internal_tx).await {
            Ok(()) => {
                info!("Supernode {} WebSocket closed cleanly", peer_id);
                break; // clean shutdown
            }
            Err(e) => {
                warn!(
                    "Supernode {} WebSocket error: {}; retry in {:?}",
                    peer_id, e, backoff
                );
                let _ = internal_tx.try_send(InternalEvent::WsDisconnected {
                    peer_id: peer_id.clone(),
                });
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
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
) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;

    let (ws_stream, _) = connect_async(ws_url).await?;
    info!("WebSocket connected to supernode {}", peer_id);
    let (mut ws_sink, mut ws_stream) = ws_stream.split();

    // Send HELLO
    let hello = build_hello(identity)?;
    ws_sink.send(WsMessage::Text(hello)).await?;

    let _ = internal_tx.try_send(InternalEvent::WsConnected {
        peer_id: peer_id.to_owned(),
    });

    // I/O loop
    let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_S));

    loop {
        tokio::select! {
            // Outbound
            Some(msg) = send_rx.recv() => {
                ws_sink.send(msg).await?;
            }

            // Inbound
            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        match serde_json::from_str::<SignalingMessage>(&text) {
                            Ok(sm) => {
                                let _ = internal_tx.try_send(InternalEvent::WsSignalingMessage { msg: sm });
                            }
                            Err(e) => {
                                debug!("Ignoring non-signaling WS message: {}", e);
                            }
                        }
                    }
                    Some(Ok(WsMessage::Close(_))) | None => {
                        break;
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
    Ok(())
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
