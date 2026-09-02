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

use crate::crypto::normalize_public_id;
use crate::protocol::{MessageType, SignalingMessage};
use conquerd_features::ReplayGuard;

/// Bulk file-payload frames: the chunk stream and its terminating COMPLETE.
///
/// These are metered on a separate, much larger per-connection budget than
/// control traffic. A transfer is inherently thousands of frames, and no layer
/// retransmits a dropped chunk — losing one strands the receiver forever, so
/// they must not share the small control-message budget.
///
/// Offer / request / accept / reject / revoke frames are deliberately absent:
/// they are control traffic and stay on the control budget.
pub(crate) fn is_bulk_file_data(mt: MessageType) -> bool {
    matches!(
        mt,
        MessageType::FileTransferChunk
            | MessageType::FileTransferComplete
            | MessageType::SfuFileChunk
            | MessageType::SfuFileComplete
    )
}

/// Steady-state ceiling on bulk file frames from one connection, in frames
/// per second. At the client's 64 KiB chunk size this is ~12.8 MB/s.
const FILE_FRAMES_PER_SEC: f64 = 200.0;

/// Frames a connection may burst before pacing starts.
const FILE_FRAME_BURST: f64 = 400.0;

/// Paces bulk file frames without stranding them and without stalling the
/// connection they share.
///
/// Two properties pull against each other here. Nothing retransmits a file
/// chunk, so this must never *drop* — it has to push back on the sender, and
/// the only backpressure available is declining to read. But the read loop it
/// sits in also carries that peer's `SfuAudio`, `SfuChat`, call control and
/// room membership, so parking it until a rate window rolls over silently
/// breaks voice and chat for as long as the stall lasts.
///
/// A token bucket squares the two: over budget it waits out the deficit for
/// the single frame in hand — at most `1 / FILE_FRAMES_PER_SEC`, i.e. ~5 ms —
/// rather than sleeping off the remainder of a window. Bandwidth is capped
/// just the same; collateral delay on everything else stays in milliseconds.
pub(crate) struct FileFrameLimiter {
    tokens: f64,
    last_refill: std::time::Instant,
}

impl Default for FileFrameLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl FileFrameLimiter {
    pub(crate) fn new() -> Self {
        Self {
            tokens: FILE_FRAME_BURST,
            last_refill: std::time::Instant::now(),
        }
    }

    /// Account for one file frame, waiting out any deficit first.
    pub(crate) async fn admit(&mut self) {
        self.refill();
        if self.tokens < 1.0 {
            let wait =
                std::time::Duration::from_secs_f64((1.0 - self.tokens) / FILE_FRAMES_PER_SEC);
            tokio::time::sleep(wait).await;
            self.refill();
        }
        self.tokens = (self.tokens - 1.0).max(0.0);
    }

    fn refill(&mut self) {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * FILE_FRAMES_PER_SEC).min(FILE_FRAME_BURST);
    }
}

/// Hard ceiling on bytes queued for one peer's writer task.
///
/// The queue stays unbounded — `send_to_peer` runs under a read lock in sync
/// context and cannot await — so this counter is what actually bounds memory.
/// It has to: file payload frames are deliberately never dropped by a quota
/// (see `file_payload_bypasses_quota` in `main.rs`), which leaves a receiver on
/// a slower link than the sender as the only thing between this process and
/// unbounded growth. A 250 MB transfer is ~340 MB of base64 JSON, per lagging
/// recipient, per room member.
///
/// Past this mark the peer's transport is closed rather than the frame
/// dropped: the peer reconnects and re-requests, which is recoverable, where a
/// silent drop strands the transfer forever with no NACK to recover it.
pub(crate) const PEER_QUEUE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// A connected peer's write channel, bounded by queued bytes.
#[derive(Clone)]
pub struct PeerTx {
    tx: mpsc::UnboundedSender<String>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    overflow: Arc<tokio::sync::Notify>,
}

/// Receiving half of a [`PeerTx`]. Draining it releases the queued-byte
/// accounting, and it reports end-of-stream once the ceiling has been breached.
pub struct PeerRx {
    rx: mpsc::UnboundedReceiver<String>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    overflow: Arc<tokio::sync::Notify>,
}

/// Create a byte-bounded peer write channel.
pub(crate) fn peer_channel() -> (PeerTx, PeerRx) {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let overflow = Arc::new(tokio::sync::Notify::new());
    (
        PeerTx {
            tx,
            queued: queued.clone(),
            overflow: overflow.clone(),
        },
        PeerRx {
            rx,
            queued,
            overflow,
        },
    )
}

impl PeerTx {
    /// Queue `json` for delivery.
    ///
    /// `false` means the peer is gone, or is too far behind to keep up and its
    /// transport is being torn down — never a silent drop of a frame the
    /// caller believed was delivered.
    pub fn send(&self, json: &str) -> bool {
        use std::sync::atomic::Ordering;
        let len = json.len();
        if self.queued.fetch_add(len, Ordering::AcqRel) + len > PEER_QUEUE_MAX_BYTES {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            // Wakes the writer task, which then closes the transport. A stored
            // permit covers the case where it is not parked yet.
            self.overflow.notify_one();
            return false;
        }
        if self.tx.send(json.to_owned()).is_err() {
            self.queued.fetch_sub(len, Ordering::AcqRel);
            return false;
        }
        true
    }

    /// True when both handles belong to the same underlying channel.
    pub fn same_channel(&self, other: &PeerTx) -> bool {
        self.tx.same_channel(&other.tx)
    }
}

impl PeerRx {
    /// Next queued frame, or `None` once the peer disconnected or overran
    /// [`PEER_QUEUE_MAX_BYTES`].
    pub async fn recv(&mut self) -> Option<String> {
        use std::sync::atomic::Ordering;
        let PeerRx {
            rx,
            queued,
            overflow,
        } = self;
        tokio::select! {
            biased;
            _ = overflow.notified() => None,
            msg = rx.recv() => {
                let json = msg?;
                queued.fetch_sub(json.len(), Ordering::AcqRel);
                Some(json)
            }
        }
    }

    #[cfg(test)]
    fn try_recv(&mut self) -> Option<String> {
        use std::sync::atomic::Ordering;
        let json = self.rx.try_recv().ok()?;
        self.queued.fetch_sub(json.len(), Ordering::AcqRel);
        Some(json)
    }
}

/// Candidate map keys for an identity (exact, padded, bare) so pad variants
/// of the same Ed25519 public_id resolve to one live socket.
fn identity_key_variants(identity_pub: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(3);
    out.push(identity_pub.to_string());
    let canon = normalize_public_id(identity_pub);
    if canon != identity_pub {
        out.push(canon);
    }
    let bare = identity_pub.trim_end_matches('=');
    if bare != identity_pub {
        out.push(bare.to_string());
    }
    out
}

/// Shared signaling state.
pub struct SignalingState {
    /// identity_pub → WebSocket sender channel
    pub peer_sockets: HashMap<String, PeerTx>,
    /// identity_pub → reliable QUIC relay signaling-stream sender channel.
    /// Populated by the relay's signaling-stream hook; preferred over the
    /// WebSocket socket by [`SignalingServer::send_to_peer`] for lower-latency,
    /// head-of-line-blocking-free room broadcast delivery.
    pub quic_senders: HashMap<String, PeerTx>,
    /// Number of connected peers
    pub connected_count: usize,
}

impl SignalingState {
    pub fn new() -> Self {
        Self {
            peer_sockets: HashMap::new(),
            quic_senders: HashMap::new(),
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
    /// Broadcast fired on graceful shutdown: every connection's writer task
    /// sends a proper WS Close frame (code 1001 "going away") so clients take
    /// their clean-close reconnect path instead of seeing a TCP reset.
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl SignalingServer {
    pub fn new(our_id: String) -> Self {
        let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
        Self {
            state: Arc::new(RwLock::new(SignalingState::new())),
            our_id,
            replay_guard: Arc::new(ReplayGuard::new(300.0)),
            shutdown_tx,
        }
    }

    /// Graceful shutdown: ask every connected signaling client's writer task
    /// to send a WS Close frame. Call before process exit and give the writer
    /// tasks a brief moment to flush.
    pub fn close_all(&self) {
        let _ = self.shutdown_tx.send(());
    }

    pub(crate) fn state(&self) -> Arc<RwLock<SignalingState>> {
        self.state.clone()
    }

    /// Send a raw JSON message to a specific peer.
    ///
    /// Prefers the peer's reliable QUIC relay signaling stream when one is
    /// registered (room broadcasts avoid TCP head-of-line blocking that way),
    /// falling back to the WebSocket socket if no QUIC stream exists or its
    /// channel has closed.
    pub fn send_to_peer(&self, identity_pub: &str, json: &str) -> bool {
        let st = self.state.read();
        for key in identity_key_variants(identity_pub) {
            if let Some(tx) = st.quic_senders.get(&key) {
                if tx.send(json) {
                    return true;
                }
            }
            // Fall through on failure rather than returning: a stale socket
            // left under one padding variant must not mask a live one under
            // another. (The QUIC branch above always did; this one did not.)
            if let Some(tx) = st.peer_sockets.get(&key) {
                if tx.send(json) {
                    return true;
                }
            }
        }
        false
    }

    /// Register a peer's reliable QUIC relay signaling-stream sender. Called
    /// by the relay signaling hook when a peer opens its signaling stream.
    pub fn register_quic_sender(&self, identity_pub: &str, tx: PeerTx) {
        self.state
            .write()
            .quic_senders
            .insert(identity_pub.to_string(), tx);
    }

    /// Remove a peer's QUIC signaling sender, but only if it is still the
    /// `tx` registered (guards against tearing down a newer stream after a
    /// reconnect replaced this one).
    pub fn unregister_quic_sender(&self, identity_pub: &str, tx: &PeerTx) {
        let mut st = self.state.write();
        if st
            .quic_senders
            .get(identity_pub)
            .is_some_and(|stored| stored.same_channel(tx))
        {
            st.quic_senders.remove(identity_pub);
        }
    }

    /// Parse, verify (Ed25519 signature + 5-minute freshness), and run the
    /// shared sliding-window replay guard over a raw signaling frame. Returns
    /// the parsed message when it should be routed; `None` (logging the
    /// reason) when it must be dropped. Used by the reliable QUIC relay
    /// signaling stream so it enforces exactly the same checks as the WS path
    /// (and shares the replay guard, so a frame replayed across transports is
    /// still caught). `SfuAudio` is never expected here (it rides datagrams).
    pub fn accept_signed(&self, raw: &str) -> Option<SignalingMessage> {
        if raw.len() > 262_144 {
            warn!(
                "Oversized relay signaling frame ({} bytes) — dropping",
                raw.len()
            );
            return None;
        }
        let parsed = SignalingMessage::from_json(raw).ok()?;
        if !parsed.verify() || !parsed.is_fresh(300.0) {
            warn!(
                "Invalid signature/freshness on relay signaling {:?} from {} — dropping",
                parsed.msg_type,
                &parsed.sender[..12.min(parsed.sender.len())],
            );
            return None;
        }
        let fresh = parsed
            .signature
            .as_deref()
            .map(|sig| {
                self.replay_guard
                    .check_and_record(&parsed.sender, sig.as_bytes())
            })
            .unwrap_or(false);
        if !fresh {
            warn!(
                "Replayed or unsigned relay signaling {:?} from {} — dropping",
                parsed.msg_type,
                &parsed.sender[..12.min(parsed.sender.len())],
            );
            return None;
        }
        Some(parsed)
    }

    /// Check if a peer is currently connected (via WebSocket or QUIC relay
    /// signaling stream). Pad-tolerant so peer-store canonical ids match WS
    /// senders that may use a different base64url padding form.
    pub fn is_peer_connected(&self, identity_pub: &str) -> bool {
        let st = self.state.read();
        identity_key_variants(identity_pub)
            .into_iter()
            .any(|key| st.peer_sockets.contains_key(&key) || st.quic_senders.contains_key(&key))
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
        let shutdown_tx = self.shutdown_tx.clone();

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, addr)) => {
                        let state = state.clone();
                        let handler = handler.clone();
                        let our_id = our_id.clone();
                        let replay_guard = replay_guard.clone();
                        let shutdown_rx = shutdown_tx.subscribe();
                        tokio::spawn(async move {
                            if let Err(e) = handle_ws_connection(
                                stream,
                                addr,
                                state,
                                handler,
                                &our_id,
                                replay_guard,
                                shutdown_rx,
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
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let ws = accept_async(stream).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    let (tx, mut rx) = peer_channel();

    // Writer task: forward queued messages to WebSocket. On graceful shutdown
    // send a proper Close frame (1001 "going away") so the client takes its
    // clean-close reconnect path instead of seeing a TCP reset.
    let write_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // `None` is disconnect *or* an overrun of
                // PEER_QUEUE_MAX_BYTES; both mean close the socket.
                msg = rx.recv() => match msg {
                    Some(msg) => {
                        if ws_tx.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
                _ = shutdown_rx.recv() => {
                    use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
                    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
                    let _ = ws_tx
                        .send(Message::Close(Some(CloseFrame {
                            code: CloseCode::Away,
                            reason: "shutdown".into(),
                        })))
                        .await;
                    break;
                }
            }
        }
    });

    let mut peer_id: Option<String> = None;

    // Per-connection rate limiter: max 60 signaling messages per 10 seconds.
    const RATE_MAX: u32 = 60;
    const RATE_WINDOW_SECS: u64 = 10;
    let mut rate_count: u32 = 0;
    let mut rate_window_start = std::time::Instant::now();

    // Bulk file data gets its own budget. A file transfer is a burst of
    // `size / CHUNK_SIZE` frames (a 250 MB file is ~4 000 at the client's
    // 64 KiB chunk size), so charging chunks to the 60/10 s control budget
    // dropped everything past the first ~60 — and nothing retransmits, so the
    // sender reached 100 % while the receiver stalled at ~1 %.
    //
    // Paced, never dropped, and in millisecond slices so the rest of this
    // connection's traffic is not held up behind it. See [`FileFrameLimiter`].
    let mut file_limiter = FileFrameLimiter::new();

    // Read loop
    while let Some(msg) = ws_rx.next().await {
        // The writer is gone: either the socket died or this peer overran
        // PEER_QUEUE_MAX_BYTES. Splitting the stream means dropping the write
        // half alone does not close the connection, so stop reading too —
        // otherwise the peer keeps talking into a socket that answers nothing.
        if write_task.is_finished() {
            debug!("Writer for {} exited — closing read side", addr);
            break;
        }
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
        // Bulk file data is likewise metered separately (see FILE_RATE_MAX):
        // a multi-thousand-frame transfer is normal traffic, not a flood.
        if is_bulk_file_data(parsed.msg_type) {
            file_limiter.admit().await;
        } else if parsed.msg_type != MessageType::SfuAudio {
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
        // signed message within the freshness window.
        //
        // Two classes are exempt, for the same reason: they are high-rate and
        // idempotent, so deduplicating them buys nothing while filling the
        // per-sender window would cost everything. `ReplayGuard` caps a sender
        // at MAX_ENTRIES_PER_SENDER signatures and *fails closed* past that —
        // so a sender who fills it stops being able to send anything at all,
        // chat and call control included, until entries age out.
        //
        //  * Real-time SFU audio (~50 Hz), covered by the freshness window and
        //    the jitter buffer.
        //  * Bulk file payload. At the client's ~96 chunks/s pacing a sustained
        //    transfer fills 16 384 entries in under three minutes — roughly a
        //    gigabyte — and blacked the peer out for the ~2 minutes it took the
        //    oldest entries to expire. Chunks are idempotent by
        //    (transfer_id, chunk_index) and COMPLETE only acts on a transfer
        //    still `Transferring`, so a replay is a no-op at the receiver. Both
        //    still carry signature verification and the freshness window.
        if !is_bulk_file_data(parsed.msg_type) && parsed.msg_type != MessageType::SfuAudio {
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
                    if target_tx.send(&msg) {
                        debug!(
                            "Relayed {:?} from {} → {}",
                            parsed.msg_type,
                            &parsed.sender[..12.min(parsed.sender.len())],
                            &target[..12.min(target.len())],
                        );
                    } else {
                        warn!(
                            "Relay target {} is closed or backed up past {} bytes — dropping {:?}",
                            &target[..12.min(target.len())],
                            PEER_QUEUE_MAX_BYTES,
                            parsed.msg_type,
                        );
                    }
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
        let (tx, _rx) = peer_channel();
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
        let (tx, mut rx) = peer_channel();
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
        let (tx, rx) = peer_channel();
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
        let (tx1, _rx1) = peer_channel();
        let (tx2, _rx2) = peer_channel();
        {
            let mut st = srv.state.write();
            st.peer_sockets.insert("peer-a".into(), tx1);
            st.peer_sockets.insert("peer-b".into(), tx2);
        }
        let mut ids = srv.connected_peer_ids();
        ids.sort();
        assert_eq!(ids, vec!["peer-a".to_string(), "peer-b".to_string()]);
    }

    /// Architecture: room broadcasts prefer QUIC signaling stream over WS so
    /// control traffic avoids TCP head-of-line blocking when a relay session exists.
    #[test]
    fn send_to_peer_prefers_quic_sender_over_websocket() {
        let srv = SignalingServer::new("supernode-id".into());
        let (quic_tx, mut quic_rx) = peer_channel();
        let (ws_tx, mut ws_rx) = peer_channel();
        {
            let mut st = srv.state.write();
            st.quic_senders.insert("peer-a".into(), quic_tx);
            st.peer_sockets.insert("peer-a".into(), ws_tx);
        }
        assert!(srv.send_to_peer("peer-a", r#"{"type":"ping"}"#));
        assert_eq!(
            quic_rx.try_recv().expect("delivered on QUIC path"),
            r#"{"type":"ping"}"#
        );
        assert!(
            ws_rx.try_recv().is_none(),
            "must not also fan out to WebSocket when QUIC succeeds"
        );
    }

    /// If the preferred QUIC channel is closed, fall back to WebSocket.
    #[test]
    fn send_to_peer_falls_back_to_ws_when_quic_closed() {
        let srv = SignalingServer::new("supernode-id".into());
        let (quic_tx, quic_rx) = peer_channel();
        let (ws_tx, mut ws_rx) = peer_channel();
        drop(quic_rx);
        {
            let mut st = srv.state.write();
            st.quic_senders.insert("peer-a".into(), quic_tx);
            st.peer_sockets.insert("peer-a".into(), ws_tx);
        }
        assert!(srv.send_to_peer("peer-a", r#"{"type":"ping"}"#));
        assert_eq!(ws_rx.try_recv().expect("WS fallback"), r#"{"type":"ping"}"#);
    }

    /// A peer that stops draining is cut off at the byte ceiling rather than
    /// being allowed to grow the queue without bound. File payload is never
    /// dropped by a quota, so this counter is the only thing bounding memory.
    #[test]
    fn peer_queue_is_bounded_by_bytes() {
        let (tx, _rx) = peer_channel();
        let frame = "x".repeat(64 * 1024);
        let mut queued = 0usize;
        while tx.send(&frame) {
            queued += frame.len();
            assert!(
                queued <= PEER_QUEUE_MAX_BYTES,
                "queue grew past the ceiling: {queued}"
            );
        }
        assert!(
            queued > PEER_QUEUE_MAX_BYTES - 2 * frame.len(),
            "cut off far too early at {queued}"
        );
    }

    /// Draining releases the accounting, so a peer that keeps up can send far
    /// more than the ceiling in total.
    #[test]
    fn draining_frees_queue_budget() {
        let (tx, mut rx) = peer_channel();
        let frame = "y".repeat(1024 * 1024);
        for _ in 0..64 {
            assert!(tx.send(&frame), "steady drain must never hit the ceiling");
            assert!(rx.try_recv().is_some());
        }
    }

    /// Overrunning the ceiling closes the peer out: `recv` reports
    /// end-of-stream so the writer task tears the transport down, rather than
    /// silently dropping a chunk nothing would ever retransmit.
    #[tokio::test]
    async fn queue_overflow_closes_the_peer() {
        let (tx, mut rx) = peer_channel();
        let frame = "z".repeat(64 * 1024);
        while tx.send(&frame) {}
        assert!(rx.recv().await.is_none(), "overflow must close the stream");
    }

    /// A stale socket parked under one padding variant must not hide a live
    /// one under another.
    #[test]
    fn send_to_peer_tries_remaining_key_variants_after_a_dead_socket() {
        let srv = SignalingServer::new("supernode-id".into());
        let padded = "AAAA=";
        let bare = "AAAA";
        let (dead_tx, dead_rx) = peer_channel();
        let (live_tx, mut live_rx) = peer_channel();
        drop(dead_rx);
        {
            let mut st = srv.state.write();
            st.peer_sockets.insert(padded.into(), dead_tx);
            st.peer_sockets.insert(bare.into(), live_tx);
        }
        assert!(srv.send_to_peer(padded, r#"{"type":"ping"}"#));
        assert_eq!(
            live_rx
                .try_recv()
                .expect("delivered on the bare-key socket"),
            r#"{"type":"ping"}"#
        );
    }

    /// The file limiter caps throughput without ever parking the connection
    /// long enough to stall the voice and control traffic sharing it.
    #[tokio::test(start_paused = true)]
    async fn file_limiter_paces_without_long_stalls() {
        let mut lim = FileFrameLimiter::new();
        let start = tokio::time::Instant::now();
        // Drain the burst allowance, then keep going well past it.
        for _ in 0..(FILE_FRAME_BURST as usize + 400) {
            let before = tokio::time::Instant::now();
            lim.admit().await;
            assert!(
                before.elapsed() <= std::time::Duration::from_millis(10),
                "single-frame wait must stay in milliseconds, was {:?}",
                before.elapsed()
            );
        }
        // 800 frames with a 400 burst leaves 400 to pace at 200/s => ~2 s.
        let elapsed = start.elapsed();
        assert!(
            elapsed >= std::time::Duration::from_millis(1_800),
            "rate ceiling not enforced: {elapsed:?}"
        );
    }

    /// accept_signed: unsigned / malformed frames are dropped (security).
    #[test]
    fn accept_signed_rejects_unsigned_and_malformed() {
        let srv = SignalingServer::new("supernode-id".into());
        assert!(srv.accept_signed("not-json").is_none());
        assert!(srv
            .accept_signed(r#"{"type":"ping","sender":"x","timestamp":1.0,"v":2}"#)
            .is_none());
    }

    /// accept_signed: valid signed fresh message is accepted once; replay denied.
    #[test]
    fn accept_signed_accepts_fresh_and_rejects_replay() {
        use crate::identity::Identity;
        use crate::protocol::{MessageType, SignalingMessage};

        let srv = SignalingServer::new("supernode-id".into());
        let id = Identity::generate();
        let msg = SignalingMessage::new(MessageType::Ping, &id.public_id(), serde_json::json!({}))
            .sign(&id);
        let raw = msg.to_json();

        let first = srv
            .accept_signed(&raw)
            .expect("fresh signed frame must be accepted");
        assert_eq!(first.msg_type, MessageType::Ping);

        // Same signature within the freshness window is a replay.
        assert!(
            srv.accept_signed(&raw).is_none(),
            "replay of the same signed frame must be dropped"
        );
    }

    #[test]
    fn file_payload_frames_are_not_on_the_control_budget() {
        // The chunk stream and its COMPLETE ride the large file budget.
        assert!(is_bulk_file_data(MessageType::FileTransferChunk));
        assert!(is_bulk_file_data(MessageType::FileTransferComplete));
        assert!(is_bulk_file_data(MessageType::SfuFileChunk));
        assert!(is_bulk_file_data(MessageType::SfuFileComplete));

        // Control frames stay metered at 60 / 10 s.
        assert!(!is_bulk_file_data(MessageType::FileTransferOffer));
        assert!(!is_bulk_file_data(MessageType::FileTransferAccept));
        assert!(!is_bulk_file_data(MessageType::SfuFileOffer));
        assert!(!is_bulk_file_data(MessageType::SfuFileRequest));
        assert!(!is_bulk_file_data(MessageType::ChatMessage));
    }
}
