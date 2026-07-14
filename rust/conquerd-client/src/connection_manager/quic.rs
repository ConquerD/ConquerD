//! QUIC peer session task.
//!
//! Transport contract for a direct peer session:
//! - **Reliable signaling** rides one long-lived unidirectional stream per
//!   direction, with 4-byte big-endian length-prefixed tagged frames
//!   (control / chat / file).
//! - **Direct audio** (`core.audio.opus`) rides QUIC **datagrams** (same
//!   `[AUDIO_TAG][id_len][peer_id][opus]` layout as before) so frames are not
//!   head-of-line blocked and do not open a stream per 20 ms packet.
//!
//! Inbound still accepts additional uni/bi streams (interop / older peers that
//! open one stream per message) and datagrams.

use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::internal::{InternalEvent, PeerOutbound};

// ---------------------------------------------------------------------------
// QUIC peer session task
// ---------------------------------------------------------------------------

/// Manages a single QUIC peer connection.
///
/// - Opens a long-lived outbound uni stream for reliable signaling.
/// - Forwards [`PeerOutbound::Reliable`] frames with length-prefix framing.
/// - Sends [`PeerOutbound::Datagram`] via `Connection::send_datagram`.
/// - Reads inbound streams + datagrams into `InternalEvent::QuicSignalingData`.
/// - Emits `QuicConnected` once outbound channels are ready; `QuicDisconnected`
///   when the session ends.
pub(super) async fn run_quic_peer_session(
    connection: quinn::Connection,
    peer_id: String,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    let (out_tx, mut out_rx) = mpsc::channel::<PeerOutbound>(128);

    let _ = internal_tx
        .send(InternalEvent::QuicConnected {
            peer_id: peer_id.clone(),
            out_tx,
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

    let peer_id_w = peer_id.clone();
    let peer_id_r = peer_id.clone();

    // Outbound: one long-lived uni for reliable frames; datagrams for audio.
    let write_conn = connection.clone();
    let write_task = tokio::spawn(async move {
        let mut send_stream: Option<quinn::SendStream> = None;
        while let Some(msg) = out_rx.recv().await {
            match msg {
                PeerOutbound::Datagram(bytes) => {
                    if let Err(e) = write_conn.send_datagram(Bytes::from(bytes)) {
                        debug!(
                            "QUIC datagram send failed for {}: {e}",
                            &peer_id_w[..8.min(peer_id_w.len())]
                        );
                        // Datagram failure is best-effort; keep the session.
                    }
                }
                PeerOutbound::Reliable(bytes) => {
                    if send_stream.is_none() {
                        match write_conn.open_uni().await {
                            Ok(stream) => send_stream = Some(stream),
                            Err(e) => {
                                warn!(
                                    "Failed to open QUIC signaling stream to {}: {e}",
                                    &peer_id_w[..8.min(peer_id_w.len())]
                                );
                                break;
                            }
                        }
                    }
                    let Some(stream) = send_stream.as_mut() else {
                        break;
                    };
                    let len = (bytes.len() as u32).to_be_bytes();
                    if stream.write_all(&len).await.is_err()
                        || stream.write_all(&bytes).await.is_err()
                    {
                        // Stream died — try to reopen on the next reliable frame.
                        let _ = stream.finish();
                        send_stream = None;
                        // Retry once immediately on a fresh stream.
                        match write_conn.open_uni().await {
                            Ok(mut new_stream) => {
                                if new_stream.write_all(&len).await.is_ok()
                                    && new_stream.write_all(&bytes).await.is_ok()
                                {
                                    send_stream = Some(new_stream);
                                } else {
                                    warn!(
                                        "QUIC signaling stream write failed for {}",
                                        &peer_id_w[..8.min(peer_id_w.len())]
                                    );
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!(
                                    "Failed to reopen QUIC signaling stream to {}: {e}",
                                    &peer_id_w[..8.min(peer_id_w.len())]
                                );
                                break;
                            }
                        }
                    }
                }
            }
        }
        if let Some(mut stream) = send_stream.take() {
            let _ = stream.finish();
        }
        debug!(
            "QUIC write task ended for {}",
            &peer_id_w[..8.min(peer_id_w.len())]
        );
    });

    // Accept loop: spawn a reader per inbound stream so long-lived signaling
    // streams do not head-of-line-block datagram reception (direct audio).
    loop {
        tokio::select! {
            stream = connection.accept_uni() => {
                match stream {
                    Ok(recv_stream) => {
                        let peer = peer_id_r.clone();
                        let tx = internal_tx.clone();
                        tokio::spawn(async move {
                            let _ = read_quic_signaling_stream(recv_stream, &peer, &tx).await;
                        });
                    }
                    Err(_) => break,
                }
            }
            stream = connection.accept_bi() => {
                match stream {
                    Ok((_send_stream, recv_stream)) => {
                        let peer = peer_id_r.clone();
                        let tx = internal_tx.clone();
                        tokio::spawn(async move {
                            let _ = read_quic_signaling_stream(recv_stream, &peer, &tx).await;
                        });
                    }
                    Err(_) => break,
                }
            }
            dgram = connection.read_datagram() => {
                match dgram {
                    Ok(bytes) => {
                        let _ = internal_tx
                            .send(InternalEvent::QuicSignalingData {
                                peer_id: peer_id_r.clone(),
                                data: bytes.to_vec(),
                            })
                            .await;
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

/// Read length-prefixed frames until the stream ends.
///
/// Returns `false` only on a protocol hard-fail (frame too large) so the
/// session should tear down; stream EOF returns `true` (session may continue
/// on other streams / datagrams).
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
