//! QUIC peer session task.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::internal::InternalEvent;
// ---------------------------------------------------------------------------
// QUIC peer session task
// ---------------------------------------------------------------------------

/// Manages a single QUIC peer connection.
///
/// - Opens a bidirectional signaling stream (stream 0).
/// - Forwards outbound bytes from `sig_tx` into the send side.
/// - Reads inbound bytes from the recv side, sending `InternalEvent::QuicSignalingData`.
/// - Sends `InternalEvent::QuicConnected` once the stream is open.
/// - Sends `InternalEvent::QuicDisconnected` when the session ends.
pub(super) async fn run_quic_peer_session(
    connection: quinn::Connection,
    peer_id: String,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    let (sig_tx, mut sig_rx) = mpsc::channel::<Vec<u8>>(64);

    let _ = internal_tx
        .send(InternalEvent::QuicConnected {
            peer_id: peer_id.clone(),
            sig_tx,
        })
        .await;

    // Sample QUIC transport stats for the connection stats overlay.
    let stats_conn = connection.clone();
    let stats_tx = internal_tx.clone();
    let stats_peer = peer_id.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        let mut prev_bytes: u64 = 0;
        let mut prev_rtt_ms: Option<f64> = None;
        loop {
            interval.tick().await;
            if stats_conn.close_reason().is_some() {
                break;
            }
            let s = stats_conn.stats();
            let rtt_ms = s.path.rtt.as_secs_f64() * 1000.0;
            let sent = s.path.sent_packets;
            let lost = s.path.lost_packets;
            let packet_loss_pct = if sent > 0 {
                (lost as f64 / sent as f64) * 100.0
            } else {
                0.0
            };
            let jitter_ms = prev_rtt_ms.map(|prev| (rtt_ms - prev).abs()).unwrap_or(0.0);
            prev_rtt_ms = Some(rtt_ms);
            let bytes = s.udp_tx.bytes;
            let bandwidth_kbps = if prev_bytes > 0 && bytes >= prev_bytes {
                ((bytes - prev_bytes) * 8) as f64 / 2000.0
            } else {
                0.0
            };
            prev_bytes = bytes;
            let _ = stats_tx.try_send(InternalEvent::QuicStats {
                peer_id: stats_peer.clone(),
                rtt_ms,
                packet_loss_pct,
                jitter_ms,
                bandwidth_kbps,
            });
        }
    });

    // Outbound write buffer
    let peer_id_w = peer_id.clone();
    let peer_id_r = peer_id.clone();

    // Spawn a task to write outbound messages
    let write_conn = connection.clone();
    let write_task = tokio::spawn(async move {
        while let Some(bytes) = sig_rx.recv().await {
            let mut send_stream = match write_conn.open_uni().await {
                Ok(stream) => stream,
                Err(e) => {
                    warn!(
                        "Failed to open QUIC signaling stream to {}: {e}",
                        &peer_id_w[..8.min(peer_id_w.len())]
                    );
                    break;
                }
            };
            // Length-prefix framing: 4-byte big-endian length + payload
            let len = (bytes.len() as u32).to_be_bytes();
            if send_stream.write_all(&len).await.is_err() {
                break;
            }
            if send_stream.write_all(&bytes).await.is_err() {
                break;
            }
            if send_stream.finish().is_err() {
                break;
            }
        }
        debug!(
            "QUIC write task ended for {}",
            &peer_id_w[..8.min(peer_id_w.len())]
        );
    });

    // Read loop: length-prefixed frames
    loop {
        tokio::select! {
            stream = connection.accept_uni() => {
                match stream {
                    Ok(recv_stream) => {
                        if !read_quic_signaling_stream(
                            recv_stream,
                            &peer_id_r,
                            &internal_tx,
                        ).await {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            stream = connection.accept_bi() => {
                match stream {
                    Ok((_send_stream, recv_stream)) => {
                        if !read_quic_signaling_stream(
                            recv_stream,
                            &peer_id_r,
                            &internal_tx,
                        ).await {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    write_task.abort();
    connection.close(0u32.into(), b"bye");
    let _ = internal_tx
        .send(InternalEvent::QuicDisconnected { peer_id: peer_id_r })
        .await;
}

async fn read_quic_signaling_stream(
    mut recv_stream: quinn::RecvStream,
    peer_id: &str,
    internal_tx: &mpsc::Sender<InternalEvent>,
) -> bool {
    let mut len_buf = [0u8; 4];
    loop {
        match recv_stream.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(_) => return true,
        }
        let frame_len = u32::from_be_bytes(len_buf) as usize;
        if frame_len > 4 * 1024 * 1024 {
            warn!(
                "QUIC signaling frame too large ({frame_len} bytes) from {}",
                &peer_id[..8.min(peer_id.len())]
            );
            return false;
        }
        let mut payload = vec![0u8; frame_len];
        match recv_stream.read_exact(&mut payload).await {
            Ok(()) => {
                let _ = internal_tx
                    .send(InternalEvent::QuicSignalingData {
                        peer_id: peer_id.to_owned(),
                        data: payload,
                    })
                    .await;
            }
            Err(_) => return true,
        }
    }
}
