//! Native Rust QUIC relay client (slim).
//!
//! Connects to a supernode's QUIC relay listener over mTLS using the
//! caller's Ed25519 identity. The relay grants access based on the
//! certificate CN (peer_id) — no explicit ticket payload is sent on the
//! wire; the ticket is validated client-side only (expiry / host / port).
//!
//! This client is used by:
//!   * [`crate::web_app_client`] — to open a bidirectional QUIC stream
//!     tagged `web.host.app.v1` and fetch in-app portal assets.
//!   * Room voice (`room.audio.sfu`) — to send/receive Opus frames as
//!     unreliable QUIC datagrams instead of base64/JSON over the WebSocket
//!     signaling channel. Datagrams avoid TCP head-of-line blocking, which
//!     is the dominant source of room-audio latency on the WS path. The
//!     frames stay **end-to-end signed** (the datagram carries the same
//!     signed `SfuAudio` JSON the WS path sends), so the receiver verifies
//!     the sender's Ed25519 signature exactly as before — the supernode
//!     remains a dumb forwarder that cannot forge a member's voice.
//!
//! Wire format reused for relay command stream draining (best-effort,
//! discarded): supernode pushes length-prefixed JSON frames via
//! `conn.open_uni()` (welcome, peer_joined, peer_left, relay_punch).
//! See `rust/conquerd-supernode/src/wire.rs::encode_relay_cmd`.
//!
//! Room-audio datagram wire shape (sent by us → relay → fanned to members):
//!   * outbound: `[BROADCAST_INDEX][ROOM_AUDIO_TAG][signed SfuAudio JSON]`
//!   * inbound (forwarded by relay): `[sender_index][ROOM_AUDIO_TAG][signed
//!     SfuAudio JSON]`. We ignore `sender_index` — the signed JSON is
//!     self-describing (`msg.sender`) — and hand the JSON to the connection
//!     manager, which verifies + dispatches it on the normal inbound path.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use bytes::Bytes;
use conquerd_features::channel_frame::{
    GAME_RELAY_TAG, RELAY_SIGNAL_STREAM_MAGIC, ROOM_AUDIO_TAG, ROOM_VIDEO_TAG,
};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Max time we'll wait for the mTLS handshake to complete.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-relay-cmd frame size sanity cap (matches supernode wire).
const RELAY_CMD_MAX_FRAME: usize = 64 * 1024;

/// Per reliable-signaling-frame size cap (matches the supernode side).
const SIGNAL_MAX_FRAME: usize = 262_144;

/// Bound on queued outbound signaling frames before `send_signaling` reports
/// back-pressure (and the caller falls back to WebSocket).
const SIGNAL_OUT_CAPACITY: usize = 512;

/// Signed signaling JSON received over a QUIC relay, tagged with the hosting
/// supernode's identity pubkey so the connection manager can attribute room
/// broadcasts correctly when multiple supernodes are connected.
#[derive(Debug)]
pub struct RelaySignalingInbound {
    pub supernode_id: String,
    pub json: Vec<u8>,
}

/// Opaque `game.relay.v1` payload received over a QUIC relay (portal games).
#[derive(Debug, Clone)]
pub struct RelayGameInbound {
    pub supernode_id: String,
    pub payload: Vec<u8>,
}

/// Where a relay connection delivers each kind of inbound traffic.
///
/// Grouped rather than passed as loose parameters because they are one
/// cohesive thing — the caller's inbound routing table — and because each
/// optional channel independently enables a feature: leaving `video` as `None`
/// disables inbound room video without touching audio or chat.
pub struct RelayInboundSinks {
    /// Signed signaling frames (room chat/file broadcasts, room audio).
    pub signaling: UnboundedSender<RelaySignalingInbound>,
    /// Opaque `game.relay.v1` payloads for in-app portal games.
    pub game: Option<UnboundedSender<RelayGameInbound>>,
    /// `room.video.sfu` fragments, pre-reassembly.
    pub video: Option<UnboundedSender<RelayVideoInbound>>,
}

/// One `room.video.sfu` fragment received over a QUIC relay.
///
/// Carries a single fragment rather than a whole frame: reassembly, signature
/// verification, and unsealing all happen in the connection manager, which owns
/// the per-sender reassembly state.
#[derive(Debug, Clone)]
pub struct RelayVideoInbound {
    pub supernode_id: String,
    pub fragment: Vec<u8>,
}

/// Datagram target index meaning "broadcast to all room members"
/// (mirrors `conquerd_supernode::wire::BROADCAST_INDEX`).
const BROADCAST_INDEX: u8 = 0xFF;

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
    /// Outbound queue for the reliable signaling stream (`room.chat.v1` /
    /// `room.file.v1`). `None` if the stream could not be opened or this is a
    /// portal-only guest (signaling withheld server-side) — callers then fall
    /// back to the WebSocket path. Bounded so a stalled stream surfaces as
    /// back-pressure rather than unbounded growth.
    signal_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Cleared when the signaling writer task exits so `send_signaling`
    /// returns false (WS fallback) instead of queueing into a dead channel.
    signal_live: Arc<AtomicBool>,
    /// Portal-only guest grant: no room chat/file/audio over this relay.
    portal_only: bool,
}

impl std::fmt::Debug for QuicRelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuicRelayClient")
            .field("supernode_id", &self.supernode_id)
            .field("relay_host", &self.relay_host)
            .field("relay_port", &self.relay_port)
            .field("alive", &self.connection.close_reason().is_none())
            .field("portal_only", &self.portal_only)
            .field("signal_live", &self.signal_live.load(Ordering::Relaxed))
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
    /// `sinks` routes inbound traffic (see [`RelayInboundSinks`]). Signaling
    /// frames — `SfuAudio` extracted from inbound `room.audio.sfu` datagrams,
    /// plus `room.chat.v1` / `room.file.v1` broadcasts on the reliable stream —
    /// each carry the hosting supernode's identity pubkey. The connection
    /// manager owns the receivers and re-injects signaling frames on its normal
    /// inbound path (signature verification + freshness + replay + quota +
    /// dispatch).
    ///
    /// `portal_only`: guest grant — portal streams only. Do **not** open the
    /// reliable signaling stream (the supernode drops it server-side); room
    /// chat/file must use WebSocket until a full-access ticket arrives.
    pub async fn connect(
        endpoint: &Endpoint,
        supernode_id: impl Into<String>,
        host: &str,
        port: u16,
        sinks: RelayInboundSinks,
        portal_only: bool,
    ) -> anyhow::Result<Self> {
        let RelayInboundSinks {
            signaling: reinject_tx,
            game: game_tx,
            video: video_tx,
        } = sinks;
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
            "[relay] connected to {}:{} (supernode {}{})",
            host,
            port,
            &supernode_id[..12.min(supernode_id.len())],
            if portal_only { ", portal-only" } else { "" }
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

        // Datagram receive loop — `room.audio.sfu` → signaling reinject;
        // `game.relay.v1` → portal game queue. Portal-only guests have
        // datagrams dropped server-side; the loop is cheap and still useful
        // after mid-connection promotion without reconnect.
        let conn_dgram = connection.clone();
        let shutdown_dgram = shutdown.clone();
        let sn_id_dgram = supernode_id.clone();
        let sn_short_dgram = supernode_id[..12.min(supernode_id.len())].to_owned();
        let reinject_dgram = reinject_tx.clone();
        tokio::spawn(async move {
            recv_room_datagrams(
                conn_dgram,
                shutdown_dgram,
                reinject_dgram,
                game_tx,
                video_tx,
                sn_id_dgram,
                sn_short_dgram,
            )
            .await;
        });

        // Reliable signaling stream — carries `room.chat.v1` / `room.file.v1`
        // broadcasts both directions. Portal-only guests must not open it:
        // the supernode drops the stream without hooking it, while a live
        // `signal_tx` would make `send_signaling` report success and skip
        // WebSocket fallback — silent room-chat loss.
        let signal_live = Arc::new(AtomicBool::new(false));
        let signal_tx = if portal_only {
            debug!(
                "[relay {}] portal-only — room chat/file via WS until full access",
                &supernode_id[..12.min(supernode_id.len())]
            );
            None
        } else {
            match connection.open_bi().await {
                Ok((send, recv)) => {
                    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>(SIGNAL_OUT_CAPACITY);
                    let shutdown_sig = shutdown.clone();
                    let sn_id_sig = supernode_id.clone();
                    let sn_short_sig = supernode_id[..12.min(supernode_id.len())].to_owned();
                    let live = signal_live.clone();
                    live.store(true, Ordering::Release);
                    tokio::spawn(async move {
                        run_signaling_stream(
                            send,
                            recv,
                            out_rx,
                            reinject_tx,
                            shutdown_sig,
                            sn_id_sig,
                            sn_short_sig,
                        )
                        .await;
                        live.store(false, Ordering::Release);
                    });
                    Some(out_tx)
                }
                Err(e) => {
                    warn!(
                        "[relay] could not open signaling stream: {e}; using WS for room chat/file"
                    );
                    None
                }
            }
        };

        Ok(Self {
            supernode_id,
            relay_host: host.to_owned(),
            relay_port: port,
            connection,
            shutdown,
            signal_tx,
            signal_live,
            portal_only,
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

    /// Portal-only guest grant (no room chat/file/audio over this relay).
    pub fn is_portal_only(&self) -> bool {
        self.portal_only
    }

    /// Send a signed `SfuAudio` JSON frame as a broadcast room-audio datagram.
    ///
    /// Builds `[BROADCAST_INDEX][ROOM_AUDIO_TAG][signed_json]` and hands it to
    /// quinn as an unreliable datagram. Returns `false` if the connection is
    /// gone or the frame exceeds the negotiated datagram size — the caller
    /// then falls back to the WebSocket SFU path so audio is never dropped
    /// solely because the relay datagram couldn't be sent.
    pub fn send_room_audio(&self, signed_json: &[u8]) -> bool {
        self.send_broadcast_tagged(ROOM_AUDIO_TAG, signed_json)
    }

    /// Send an opaque `game.relay.v1` payload as a broadcast relay datagram.
    ///
    /// Builds `[BROADCAST_INDEX][GAME_RELAY_TAG][payload]`. The supernode fans
    /// the frame to peers in the sender's active portal game session.
    pub fn send_game_relay(&self, payload: &[u8]) -> bool {
        self.send_broadcast_tagged(GAME_RELAY_TAG, payload)
    }

    /// Send one video fragment as a broadcast room-video datagram.
    ///
    /// Builds `[BROADCAST_INDEX][ROOM_VIDEO_TAG][fragment]`. Unlike
    /// [`send_room_audio`](Self::send_room_audio) the payload is the lean
    /// binary fragment from [`crate::video::fragment`], not signed JSON — the
    /// supernode forwards it opaquely without parsing.
    ///
    /// There is no WebSocket fallback for video. Returning `false` means this
    /// fragment is simply lost, which also loses the whole frame it belongs to;
    /// that is the intended trade, since a video frame that arrives late is
    /// worth less than the head-of-line blocking a reliable retry would cost.
    pub fn send_room_video(&self, fragment: &[u8]) -> bool {
        self.send_broadcast_tagged(ROOM_VIDEO_TAG, fragment)
    }

    /// Usable fragment payload for this connection, after the relay's index and
    /// tag bytes. Callers size fragments against this so
    /// [`send_broadcast_tagged`](Self::send_broadcast_tagged) never has to drop
    /// an oversized frame.
    pub fn max_video_fragment_len(&self) -> usize {
        // `send_broadcast_tagged` prepends BROADCAST_INDEX + tag.
        self.connection
            .max_datagram_size()
            .unwrap_or(crate::video::DEFAULT_MAX_DATAGRAM)
            .saturating_sub(2)
    }

    fn send_broadcast_tagged(&self, tag: u8, payload: &[u8]) -> bool {
        if self.connection.close_reason().is_some() {
            return false;
        }
        if let Some(max) = self.connection.max_datagram_size() {
            if payload.len() + 2 > max {
                debug!(
                    "[relay] frame {}B (tag={:#04x}) exceeds datagram max {}B",
                    payload.len() + 2,
                    tag,
                    max
                );
                return false;
            }
        }
        let mut buf = Vec::with_capacity(2 + payload.len());
        buf.push(BROADCAST_INDEX);
        buf.push(tag);
        buf.extend_from_slice(payload);
        self.connection.send_datagram(Bytes::from(buf)).is_ok()
    }

    /// Queue a signed signaling frame (`room.chat.v1` / `room.file.v1` JSON)
    /// for delivery over the reliable signaling stream. Returns `false` if the
    /// stream was never opened, is portal-only, the writer task has exited, the
    /// connection is gone, or the outbound queue is full — the caller then falls
    /// back to the WebSocket path so a frame is never dropped solely because the
    /// QUIC stream is unavailable/backed up.
    pub fn send_signaling(&self, json: &[u8]) -> bool {
        if self.portal_only {
            return false;
        }
        if self.connection.close_reason().is_some() {
            return false;
        }
        if !self.signal_live.load(Ordering::Acquire) {
            return false;
        }
        if json.len() > SIGNAL_MAX_FRAME {
            return false;
        }
        match &self.signal_tx {
            Some(tx) => tx.try_send(json.to_vec()).is_ok(),
            None => false,
        }
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

/// Receive forwarded relay datagrams: room-audio JSON → signaling reinject;
/// game-relay opaque payloads → portal queue.
async fn recv_room_datagrams(
    conn: Connection,
    shutdown: Arc<Notify>,
    reinject_tx: UnboundedSender<RelaySignalingInbound>,
    game_tx: Option<UnboundedSender<RelayGameInbound>>,
    video_tx: Option<UnboundedSender<RelayVideoInbound>>,
    supernode_id: String,
    sn_short: String,
) {
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            dgram = conn.read_datagram() => {
                match dgram {
                    Ok(data) => {
                        // [sender_index:1][tag:1][payload…]
                        if data.len() < 2 {
                            continue;
                        }
                        let tag = data[1];
                        let body = &data[2..];
                        if body.is_empty() {
                            continue;
                        }
                        if tag == ROOM_AUDIO_TAG {
                            if reinject_tx
                                .send(RelaySignalingInbound {
                                    supernode_id: supernode_id.clone(),
                                    json: body.to_vec(),
                                })
                                .is_err()
                            {
                                break;
                            }
                        } else if tag == GAME_RELAY_TAG {
                            if let Some(ref gtx) = game_tx {
                                if gtx
                                    .send(RelayGameInbound {
                                        supernode_id: supernode_id.clone(),
                                        payload: body.to_vec(),
                                    })
                                    .is_err()
                                {
                                    // Portal queue dropped — keep reading for audio.
                                }
                            }
                        } else if tag == ROOM_VIDEO_TAG {
                            if let Some(ref vtx) = video_tx {
                                if vtx
                                    .send(RelayVideoInbound {
                                        supernode_id: supernode_id.clone(),
                                        fragment: body.to_vec(),
                                    })
                                    .is_err()
                                {
                                    // Video queue dropped — keep reading for audio.
                                    // Losing video must never cost the call.
                                }
                            }
                        }
                    }
                    Err(quinn::ConnectionError::ApplicationClosed(_))
                    | Err(quinn::ConnectionError::LocallyClosed) => break,
                    Err(e) => {
                        debug!("[relay {}] read_datagram ended: {}", sn_short, e);
                        break;
                    }
                }
            }
        }
    }
    debug!("[relay {}] datagram task exiting", sn_short);
}

/// Drive the reliable signaling stream: write the [`RELAY_SIGNAL_STREAM_MAGIC`]
/// marker, then pump queued outbound frames (`[u32 BE len][json]`) while a
/// concurrent reader re-injects inbound broadcasts via `reinject_tx`.
async fn run_signaling_stream(
    mut send: SendStream,
    recv: RecvStream,
    mut out_rx: mpsc::Receiver<Vec<u8>>,
    reinject_tx: UnboundedSender<RelaySignalingInbound>,
    shutdown: Arc<Notify>,
    supernode_id: String,
    sn_short: String,
) {
    // Mark this bidi stream as the signaling channel so the supernode routes
    // it to the signaling hook instead of the `web.host.app.v1` handler.
    if let Err(e) = send
        .write_all(&RELAY_SIGNAL_STREAM_MAGIC.to_be_bytes())
        .await
    {
        debug!(
            "[relay {}] signaling stream open write failed: {}",
            sn_short, e
        );
        return;
    }

    let reader = {
        let shutdown = shutdown.clone();
        let sn_id = supernode_id.clone();
        let sn_short = sn_short.clone();
        tokio::spawn(async move {
            read_signaling_frames(recv, reinject_tx, shutdown, sn_id, sn_short).await
        })
    };

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            frame = out_rx.recv() => {
                let Some(frame) = frame else { break };
                if frame.len() > SIGNAL_MAX_FRAME {
                    continue;
                }
                if send
                    .write_all(&(frame.len() as u32).to_be_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
                if send.write_all(&frame).await.is_err() {
                    break;
                }
            }
        }
    }
    let _ = send.finish();
    reader.abort();
    debug!("[relay {}] signaling stream task exiting", sn_short);
}

/// Read length-prefixed signed signaling frames from the supernode and hand
/// each to the connection manager for re-injection on the normal inbound path.
async fn read_signaling_frames(
    mut recv: RecvStream,
    reinject_tx: UnboundedSender<RelaySignalingInbound>,
    shutdown: Arc<Notify>,
    supernode_id: String,
    sn_short: String,
) {
    loop {
        let mut len_buf = [0u8; 4];
        let read = tokio::select! {
            _ = shutdown.notified() => break,
            r = recv.read_exact(&mut len_buf) => r,
        };
        if read.is_err() {
            break;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 || len > SIGNAL_MAX_FRAME {
            break;
        }
        let mut buf = vec![0u8; len];
        if recv.read_exact(&mut buf).await.is_err() {
            break;
        }
        if reinject_tx
            .send(RelaySignalingInbound {
                supernode_id: supernode_id.clone(),
                json: buf,
            })
            .is_err()
        {
            break; // manager dropped the receiver
        }
    }
    debug!("[relay {}] signaling reader exiting", sn_short);
}
