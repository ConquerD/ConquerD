// ConquerD Supernode — relay.rs
// QUIC relay server: accept connections, validate tickets, forward datagrams.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::RwLock;
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use conquerd_features::{feature_for_fixed_tag, FeatureRegistry};

use crate::wire;

/// Relay idle timeout.
const IDLE_TIMEOUT_S: u64 = 120;
/// Keepalive interval.
const KEEPALIVE_INTERVAL_S: u64 = 10;
/// Max relay peers.
const MAX_PEERS: usize = 255;
/// Peer cleanup interval.
const CLEANUP_INTERVAL_S: u64 = 60;

/// State for a connected relay peer.
#[allow(dead_code)]
struct RelayPeer {
    peer_id: String, // identity_pub (base64url)
    peer_index: u8,  // 1-byte index (1-254)
    connection: quinn::Connection,
    room_id: Option<String>,
    connected_at: Instant,
    bytes_relayed: u64,
    remote_addr: SocketAddr,
}

/// Shared relay state.
pub struct RelayState {
    /// Authorized peer IDs (full relay access: rooms, datagrams, signaling).
    allowed: HashSet<String>,
    /// Portal-only guests: trusted peers that have not yet passed the access
    /// gate. Their QUIC relay connection is admitted, but restricted to the
    /// `web.host.app.v1` portal stream — no room join, no datagram forwarding,
    /// no reliable signaling. Promoted into `allowed` once the gate is passed.
    portal_allowed: HashSet<String>,
    /// Connected peers: identity_pub → RelayPeer.
    peers: HashMap<String, RelayPeer>,
    /// Reverse map: peer_index → identity_pub.
    index_to_peer: HashMap<u8, String>,
    /// Room membership: room_id → set of identity_pubs.
    rooms: HashMap<String, HashSet<String>>,
    /// Game-session membership (independent of SFU voice rooms): session_id → peers.
    /// Portal games join here via `GameRelayJoin` so voice room membership is
    /// not displaced when a peer opens a game demo.
    game_sessions: HashMap<String, HashSet<String>>,
    /// peer → active game session id (at most one portal game session per peer).
    peer_game_session: HashMap<String, String>,
    /// Next available peer index.
    next_index: u8,
    /// Total bytes relayed since start.
    total_bytes_relayed: u64,
}

impl RelayState {
    fn new() -> Self {
        Self {
            allowed: HashSet::new(),
            portal_allowed: HashSet::new(),
            peers: HashMap::new(),
            index_to_peer: HashMap::new(),
            rooms: HashMap::new(),
            game_sessions: HashMap::new(),
            peer_game_session: HashMap::new(),
            next_index: 1,
            total_bytes_relayed: 0,
        }
    }

    /// Allocate the next available peer index (1-254).
    fn allocate_index(&mut self) -> Option<u8> {
        let start = self.next_index;
        loop {
            if !self.index_to_peer.contains_key(&self.next_index) {
                let idx = self.next_index;
                self.next_index = if self.next_index == 254 {
                    1
                } else {
                    self.next_index + 1
                };
                return Some(idx);
            }
            self.next_index = if self.next_index == 254 {
                1
            } else {
                self.next_index + 1
            };
            if self.next_index == start {
                return None; // All indices exhausted
            }
        }
    }

    fn remove_peer(&mut self, peer_id: &str) {
        if let Some(peer) = self.peers.remove(peer_id) {
            self.index_to_peer.remove(&peer.peer_index);
            // Remove from any room
            if let Some(ref room_id) = peer.room_id {
                if let Some(members) = self.rooms.get_mut(room_id) {
                    members.remove(peer_id);
                    if members.is_empty() {
                        self.rooms.remove(room_id);
                    }
                }
            }
        }
        // Also clean up room entries even if the peer's room_id wasn't set
        // (join_room may have added to rooms map before relay connected).
        self.rooms.retain(|_, members| {
            members.remove(peer_id);
            !members.is_empty()
        });
        // Drop game-session membership so fan-out never targets a dead peer.
        if let Some(session_id) = self.peer_game_session.remove(peer_id) {
            if let Some(members) = self.game_sessions.get_mut(&session_id) {
                members.remove(peer_id);
                if members.is_empty() {
                    self.game_sessions.remove(&session_id);
                }
            }
        } else {
            self.game_sessions.retain(|_, members| {
                members.remove(peer_id);
                !members.is_empty()
            });
        }
    }
}

/// Stats snapshot for /api/stats.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RelayStats {
    pub peers_connected: usize,
    pub bytes_relayed_total: u64,
    pub active_rooms: usize,
    pub active_tickets: usize,
    pub rooms: Vec<RelayRoomStats>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RelayRoomStats {
    pub room_id: String,
    pub members: usize,
}

/// The QUIC relay server.
pub struct QUICRelayServer {
    state: Arc<RwLock<RelayState>>,
    shutdown: Arc<Notify>,
    identity_pub_id: String,
    bidi_hook: Arc<RwLock<Option<BidiStreamHook>>>,
    signal_hook: Arc<RwLock<Option<SignalStreamHook>>>,
    room_audio_bridge: Arc<RwLock<Option<RoomAudioBridgeHook>>>,
    features: Arc<FeatureRegistry>,
}

/// Fire-and-forget hook invoked for every server-accepted `web.host.app.v1`
/// bidi stream from a peer connection. The relay reads the leading `u32`
/// length-prefix (to disambiguate stream kinds — see [`SignalStreamHook`])
/// and passes it as `prefetched_len` so the hook does not re-read it. The
/// hook takes ownership of the streams, spawns its own task, and applies
/// per-feature validation. See [`crate::web_app_module::WebAppHostModule`].
pub type BidiStreamHook =
    Arc<dyn Fn(String, quinn::SendStream, quinn::RecvStream, u32) + Send + Sync + 'static>;

/// Fire-and-forget hook invoked for a server-accepted **reliable signaling**
/// bidi stream — i.e. one whose leading `u32` equals
/// [`conquerd_features::channel_frame::RELAY_SIGNAL_STREAM_MAGIC`]. Carries
/// `room.chat.v1` / `room.file.v1` broadcasts both directions over the
/// already-identity-verified relay connection. The hook owns the streams and
/// spawns its own task. See `handle_relay_signaling_stream` in `main.rs`.
pub type SignalStreamHook =
    Arc<dyn Fn(String, quinn::SendStream, quinn::RecvStream) + Send + Sync + 'static>;

/// Hook invoked when the relay receives a broadcast `room.audio.sfu` datagram.
///
/// The supernode uses it to fan the (already end-to-end signed) frame out to
/// *all* SFU room members by their best transport — relay datagram for
/// relay-connected members, WebSocket for the rest — so a member that joined
/// over WS but never opened a relay session is never partitioned. Arguments:
/// `(from_peer, sender_index, room_id, inner_payload)` where `inner_payload`
/// is `[ROOM_AUDIO_TAG][signed SfuAudio JSON]` (the bytes after the datagram's
/// leading target-index byte). See `install_room_audio_bridge` in `main.rs`.
pub type RoomAudioBridgeHook = Arc<dyn Fn(String, u8, String, Vec<u8>) + Send + Sync + 'static>;

impl QUICRelayServer {
    pub fn new(identity_pub_id: String, features: Arc<FeatureRegistry>) -> Self {
        Self {
            state: Arc::new(RwLock::new(RelayState::new())),
            shutdown: Arc::new(Notify::new()),
            identity_pub_id,
            bidi_hook: Arc::new(RwLock::new(None)),
            signal_hook: Arc::new(RwLock::new(None)),
            room_audio_bridge: Arc::new(RwLock::new(None)),
            features,
        }
    }

    /// Install (or replace) the bidi-stream hook. Must be called before
    /// [`start`] for the hook to be visible to the first accepted
    /// connection; later updates apply to new connections only.
    pub fn set_bidi_hook(&self, hook: BidiStreamHook) {
        *self.bidi_hook.write() = Some(hook);
    }

    /// Install (or replace) the reliable-signaling-stream hook. Like
    /// [`set_bidi_hook`](Self::set_bidi_hook) the hook is re-read per stream,
    /// so registering it after `start` still covers existing connections.
    pub fn set_signal_hook(&self, hook: SignalStreamHook) {
        *self.signal_hook.write() = Some(hook);
    }

    /// Install (or replace) the room-audio datagram bridge hook. Re-read per
    /// inbound datagram, so registering it after `start` still applies.
    pub fn set_room_audio_bridge(&self, hook: RoomAudioBridgeHook) {
        *self.room_audio_bridge.write() = Some(hook);
    }

    /// Forward a pre-built room-audio datagram to `recipient` over their relay
    /// connection, charging the outbound `room.audio.sfu` quota. Returns:
    /// * `None` — `recipient` has no live relay session (caller should use WS);
    /// * `Some(true)` — delivered;
    /// * `Some(false)` — relay-connected but dropped (quota exceeded or send
    ///   error); the caller must NOT also send over WS, to avoid duplicate
    ///   delivery / double quota accounting.
    pub fn send_room_datagram(&self, recipient: &str, fwd: &[u8]) -> Option<bool> {
        // Normalize: relay peers are keyed by the un-padded base64url id (see
        // `extract_peer_id`), but callers pass the SFU's padded `public_id`.
        // Without this strip the lookup misses and every relay-connected
        // recipient silently falls back to the WebSocket path.
        let recipient = recipient.trim_end_matches('=');
        let st = self.state.read();
        let peer = st.peers.get(recipient)?;
        if peer.connection.close_reason().is_some() {
            return None;
        }
        if !self
            .features
            .gate_through_feature("room.audio.sfu", recipient, fwd.len())
        {
            return Some(false);
        }
        Some(
            peer.connection
                .send_datagram(Bytes::copy_from_slice(fwd))
                .is_ok(),
        )
    }

    /// Authorize a peer to connect via QUIC relay.
    pub fn allow_peer(&self, peer_id: &str) {
        self.state
            .write()
            .allowed
            .insert(peer_id.trim_end_matches('=').to_string());
    }

    /// Re-authorize (update ticket without disconnecting). Full grant also
    /// clears any portal-only guest entry so the peer's live connection is
    /// upgraded to full access without needing to reconnect.
    pub fn allow_peer_update(&self, peer_id: &str) {
        let normalized = peer_id.trim_end_matches('=').to_string();
        let mut state = self.state.write();
        state.portal_allowed.remove(&normalized);
        state.allowed.insert(normalized);
    }

    /// Admit a peer as a **portal-only guest**: its relay connection is
    /// accepted but restricted to the `web.host.app.v1` portal stream until it
    /// passes the access gate and is promoted via [`allow_peer_update`].
    pub fn allow_portal_peer(&self, peer_id: &str) {
        self.state
            .write()
            .portal_allowed
            .insert(peer_id.trim_end_matches('=').to_string());
    }

    /// Revoke access and disconnect immediately.
    pub fn revoke_peer(&self, peer_id: &str) {
        let normalized = peer_id.trim_end_matches('=');
        let mut state = self.state.write();
        state.allowed.remove(normalized);
        if let Some(peer) = state.peers.get(normalized) {
            peer.connection.close(0u32.into(), b"revoked");
        }
        state.remove_peer(normalized);
        drop(state);
        // Quota symmetry: clear per-(feature, peer) buckets on removal.
        self.features.clear_peer_quotas(normalized);
        self.features.clear_peer_outbound_quotas(normalized);
    }

    /// Join a peer into a relay room for broadcast routing.
    /// If the peer already has a QUIC connection, sends bidirectional
    /// peer_joined notifications to all room members.
    pub fn join_room(&self, peer_id: &str, room_id: &str) {
        // Normalize: strip base64url padding to match extract_peer_id format
        let peer_id = peer_id.trim_end_matches('=');
        let peer_index = {
            let mut state = self.state.write();
            // Leave previous room if any
            let old_room = state.peers.get(peer_id).and_then(|p| p.room_id.clone());
            if let Some(ref old) = old_room {
                if let Some(members) = state.rooms.get_mut(old) {
                    members.remove(peer_id);
                    if members.is_empty() {
                        state.rooms.remove(old);
                    }
                }
            }
            let idx = state.peers.get(peer_id).map(|p| p.peer_index);
            if let Some(peer) = state.peers.get_mut(peer_id) {
                peer.room_id = Some(room_id.to_string());
            }
            state
                .rooms
                .entry(room_id.to_string())
                .or_default()
                .insert(peer_id.to_string());
            idx
        };
        // If already QUIC-connected, send bidirectional peer_joined notifications
        if let Some(idx) = peer_index {
            notify_room_peer_joined(&self.state, peer_id, idx);
        }
    }

    /// Remove a peer from their current room.
    pub fn leave_room(&self, peer_id: &str) {
        // Normalize: strip base64url padding to match extract_peer_id format
        let peer_id = peer_id.trim_end_matches('=');
        let mut state = self.state.write();
        if let Some(peer) = state.peers.get_mut(peer_id) {
            if let Some(ref room_id) = peer.room_id.take() {
                if let Some(members) = state.rooms.get_mut(room_id) {
                    members.remove(peer_id);
                    if members.is_empty() {
                        state.rooms.remove(room_id);
                    }
                }
            }
        }
    }

    /// Join a portal game session for opaque `game.relay.v1` fan-out.
    ///
    /// Independent of SFU / voice room membership — a peer may be in a voice
    /// room and a game session at the same time. Replacing the active game
    /// session leaves the previous one.
    pub fn join_game_session(&self, peer_id: &str, session_id: &str) {
        let peer_id = peer_id.trim_end_matches('=');
        if session_id.is_empty() || session_id.len() > 128 {
            return;
        }
        let mut state = self.state.write();
        if let Some(old) = state.peer_game_session.remove(peer_id) {
            if old != session_id {
                if let Some(members) = state.game_sessions.get_mut(&old) {
                    members.remove(peer_id);
                    if members.is_empty() {
                        state.game_sessions.remove(&old);
                    }
                }
            }
        }
        state
            .game_sessions
            .entry(session_id.to_owned())
            .or_default()
            .insert(peer_id.to_owned());
        state
            .peer_game_session
            .insert(peer_id.to_owned(), session_id.to_owned());
    }

    /// Leave the peer's active portal game session (if any).
    pub fn leave_game_session(&self, peer_id: &str) {
        let peer_id = peer_id.trim_end_matches('=');
        let mut state = self.state.write();
        if let Some(session_id) = state.peer_game_session.remove(peer_id) {
            if let Some(members) = state.game_sessions.get_mut(&session_id) {
                members.remove(peer_id);
                if members.is_empty() {
                    state.game_sessions.remove(&session_id);
                }
            }
        }
    }

    /// Get a peer's observed remote address (for hole punch).
    pub fn get_peer_remote_addr(&self, peer_id: &str) -> Option<SocketAddr> {
        let peer_id = peer_id.trim_end_matches('=');
        self.state.read().peers.get(peer_id).map(|p| p.remote_addr)
    }

    /// Get peer IDs in a relay room.
    pub fn get_room_peers(&self, room_id: &str) -> Vec<String> {
        self.state
            .read()
            .rooms
            .get(room_id)
            .map(|members| members.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Collect stats snapshot.
    pub(crate) fn stats(&self) -> RelayStats {
        let state = self.state.read();
        RelayStats {
            peers_connected: state.peers.len(),
            bytes_relayed_total: state.total_bytes_relayed,
            active_rooms: state.rooms.len(),
            active_tickets: state.allowed.len(),
            rooms: state
                .rooms
                .iter()
                .map(|(id, members)| RelayRoomStats {
                    room_id: id.clone(),
                    members: members.len(),
                })
                .collect(),
        }
    }

    /// Start the relay server. Returns the bound port.
    pub async fn start(&self, bind_addr: SocketAddr) -> anyhow::Result<u16> {
        let (server_config, _cert_der) = build_quinn_server_config(&self.identity_pub_id)?;
        let endpoint = Endpoint::server(server_config, bind_addr)?;
        let port = endpoint.local_addr()?.port();
        info!("QUIC relay listening on {}", endpoint.local_addr()?);

        let state = self.state.clone();
        let shutdown = self.shutdown.clone();
        let bidi_hook = self.bidi_hook.clone();
        let signal_hook = self.signal_hook.clone();
        let room_audio_bridge = self.room_audio_bridge.clone();
        let features = self.features.clone();

        // Accept loop
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    incoming = endpoint.accept() => {
                        let Some(incoming) = incoming else { break };
                        let state = state.clone();
                        // Pass the live Arc so handle_connection reads the
                        // hook per-stream, not once at connection-accept time.
                        // This fixes a race where peers connecting before
                        // set_bidi_hook() is called get hook=None forever.
                        let hook = bidi_hook.clone();
                        let signal_hook = signal_hook.clone();
                        let room_audio_bridge = room_audio_bridge.clone();
                        let features = features.clone();
                        tokio::spawn(async move {
                            if let Err(e) =
                                handle_connection(incoming, state, hook, signal_hook, room_audio_bridge, features).await
                            {
                                debug!("Relay connection error: {e}");
                            }
                        });
                    }
                    _ = shutdown.notified() => {
                        info!("QUIC relay shutting down");
                        endpoint.close(0u32.into(), b"shutdown");
                        break;
                    }
                }
            }
        });

        // Cleanup task
        let state_cleanup = self.state.clone();
        let shutdown_cleanup = self.shutdown.clone();
        let features_cleanup = self.features.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(CLEANUP_INTERVAL_S));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        cleanup_stale_peers(&state_cleanup, &features_cleanup);
                    }
                    _ = shutdown_cleanup.notified() => break,
                }
            }
        });

        Ok(port)
    }

    /// Signal shutdown.
    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
    }
}

/// Handle a single QUIC connection from a relay client.
async fn handle_connection(
    incoming: quinn::Incoming,
    state: Arc<RwLock<RelayState>>,
    bidi_hook: Arc<RwLock<Option<BidiStreamHook>>>,
    signal_hook: Arc<RwLock<Option<SignalStreamHook>>>,
    room_audio_bridge: Arc<RwLock<Option<RoomAudioBridgeHook>>>,
    features: Arc<FeatureRegistry>,
) -> anyhow::Result<()> {
    let connection = incoming.await?;
    let remote_addr = connection.remote_address();

    // Extract peer_id from client certificate CN
    let peer_id = extract_peer_id(&connection)?;
    debug!(
        "Relay connection from {} (peer: {})",
        remote_addr,
        &peer_id[..12.min(peer_id.len())]
    );

    // Check authorization. A peer with full access joins the relay normally;
    // a portal-only guest (trusted but not yet access-granted) is admitted with
    // a restricted lane: it may open the `web.host.app.v1` portal stream to
    // pass the access gate, but gets no room join, no datagram forwarding, and
    // no reliable signaling until promoted into `allowed`.
    let (allowed, portal_only) = {
        let st = state.read();
        let full = st.allowed.contains(&peer_id);
        (full, !full && st.portal_allowed.contains(&peer_id))
    };
    if !allowed && !portal_only {
        warn!(
            "Unauthorized relay peer: {}",
            &peer_id[..12.min(peer_id.len())]
        );
        let cmd = serde_json::json!({"relay_cmd": "relay_error", "reason": "not_allowed"});
        let _ = send_cmd(&connection, &cmd).await;
        connection.close(0u32.into(), b"not_allowed");
        return Ok(());
    }
    if portal_only {
        debug!(
            "Portal-only guest relay connection: {}",
            &peer_id[..12.min(peer_id.len())]
        );
    }

    // Allocate peer index
    let peer_index = {
        let mut st = state.write();
        if st.peers.len() >= MAX_PEERS {
            connection.close(0u32.into(), b"full");
            return Ok(());
        }
        // Soft-replace on reconnect: close the stale connection (its exit
        // path is skipped via the stable_id guard) and reset quota buckets,
        // but **preserve room membership**. The old `remove_peer` path wiped
        // `rooms` + `peer.room_id`, so post-reconnect `room.audio.sfu`
        // datagrams were dropped while the client still treated the relay
        // send as success (no WS fallback) — one-way silence until re-join.
        let prior_room = st.peers.get(&peer_id).and_then(|p| p.room_id.clone());
        if let Some(old) = st.peers.remove(&peer_id) {
            old.connection.close(0u32.into(), b"reconnected");
            st.index_to_peer.remove(&old.peer_index);
            if let Some(ref room) = prior_room {
                st.rooms
                    .entry(room.clone())
                    .or_default()
                    .insert(peer_id.clone());
            }
            features.clear_peer_quotas(&peer_id);
            features.clear_peer_outbound_quotas(&peer_id);
        }
        let idx = st
            .allocate_index()
            .ok_or_else(|| anyhow::anyhow!("no peer indices"))?;
        // Prefer the room we just preserved; also cover SFU join that arrived
        // via WebSocket before this QUIC relay connection was up.
        let existing_room = prior_room.or_else(|| {
            st.rooms
                .iter()
                .find(|(_, members)| members.contains(&peer_id))
                .map(|(room_id, _)| room_id.clone())
        });
        st.index_to_peer.insert(idx, peer_id.clone());
        st.peers.insert(
            peer_id.clone(),
            RelayPeer {
                peer_id: peer_id.clone(),
                peer_index: idx,
                connection: connection.clone(),
                room_id: existing_room,
                connected_at: Instant::now(),
                bytes_relayed: 0,
                remote_addr,
            },
        );
        idx
    };

    // Send welcome
    let welcome = serde_json::json!({"relay_cmd": "welcome", "index": peer_index});
    send_cmd(&connection, &welcome).await?;

    // Notify room members if peer is already in a room. Portal-only guests are
    // never room members, so skip.
    if !portal_only {
        notify_room_peer_joined(&state, &peer_id, peer_index);
    }

    info!(
        "Relay peer connected: {} index={} from {}",
        &peer_id[..12.min(peer_id.len())],
        peer_index,
        remote_addr
    );

    // Bidi-stream accept loop. Each accepted stream is multiplexed by its
    // leading `u32`: `RELAY_SIGNAL_STREAM_MAGIC` routes to the reliable
    // signaling hook (`room.chat.v1` / `room.file.v1`); any other value is a
    // `web.host.app.v1` request length-prefix and is handed to the bidi hook
    // as `prefetched_len`. Runs concurrently with the datagram loop and ends
    // when the connection closes. Hooks are re-read per stream so ones
    // registered after this connection began (startup race) are picked up.
    {
        let conn_streams = connection.clone();
        let peer_id_streams = peer_id.clone();
        let hook_lock = bidi_hook.clone();
        let signal_lock = signal_hook.clone();
        let state_streams = state.clone();
        tokio::spawn(async move {
            while let Ok((send, mut recv)) = conn_streams.accept_bi().await {
                let peer = peer_id_streams.clone();
                let hook = hook_lock.read().clone();
                let signal = signal_lock.read().clone();
                // Full access is re-read per stream so a mid-connection grant
                // (portal guest → promoted) takes effect without reconnect.
                let full_access = state_streams.read().allowed.contains(&peer);
                // Read the discriminating prefix on its own task so a slow
                // client can't stall the accept loop for other streams.
                tokio::spawn(async move {
                    let mut len_buf = [0u8; 4];
                    if recv.read_exact(&mut len_buf).await.is_err() {
                        return;
                    }
                    let first = u32::from_be_bytes(len_buf);
                    if first == conquerd_features::channel_frame::RELAY_SIGNAL_STREAM_MAGIC {
                        // Reliable signaling (room chat/file) is full-access only.
                        // Portal-only guests get their signaling stream dropped.
                        if full_access {
                            if let Some(signal) = signal {
                                (signal)(peer, send, recv);
                            }
                        }
                    } else if let Some(hook) = hook {
                        // The portal (`web.host.app.v1`) is reachable by guests —
                        // it is how they pass the access gate.
                        (hook)(peer, send, recv, first);
                    }
                    // No matching hook → streams drop (remote gets a reset).
                });
            }
        });
    }

    // Handle datagrams and watch for connection close
    let state_clone = state.clone();
    let peer_id_clone = peer_id.clone();
    let features_clone = features.clone();
    let room_audio_bridge_clone = room_audio_bridge.clone();

    // Datagram forwarding loop
    loop {
        tokio::select! {
            dgram = connection.read_datagram() => {
                match dgram {
                    Ok(data) => {
                        // Datagram forwarding (room audio, peer relay) is
                        // full-access only; a portal-only guest's datagrams are
                        // dropped. Re-read per datagram so a mid-connection
                        // grant takes effect immediately.
                        if state_clone.read().allowed.contains(&peer_id_clone) {
                            handle_datagram(
                                &state_clone,
                                &features_clone,
                                &room_audio_bridge_clone,
                                &peer_id_clone,
                                &data,
                            );
                        }
                    }
                    Err(_) => break, // Connection closed
                }
            }
        }
    }

    // Cleanup on disconnect. Guard on stable_id: if this peer_id has
    // reconnected, the registered entry belongs to the *new* connection
    // and must not be torn down by this (old) connection's exit path.
    info!(
        "Relay peer disconnected: {} index={}",
        &peer_id[..12.min(peer_id.len())],
        peer_index
    );
    let registered = {
        let st = state.read();
        st.peers
            .get(&peer_id)
            .is_some_and(|p| p.connection.stable_id() == connection.stable_id())
    };
    if registered {
        let room_id = {
            let st = state.read();
            st.peers.get(&peer_id).and_then(|p| p.room_id.clone())
        };
        state.write().remove_peer(&peer_id);
        // Quota symmetry: clear per-(feature, peer) buckets on disconnect.
        features.clear_peer_quotas(&peer_id);
        features.clear_peer_outbound_quotas(&peer_id);
        // Notify remaining room members
        if let Some(room_id) = room_id {
            notify_room_peer_left(&state, &peer_id, &room_id);
        }
    }

    Ok(())
}

/// Map a relayed inner payload to the capability id used for quota accounting.
fn relay_datagram_feature(payload: &[u8]) -> (&'static str, usize) {
    if payload.is_empty() {
        return ("game.relay.v1", 0);
    }
    if let Some(fid) = feature_for_fixed_tag(payload[0]) {
        return (fid, payload.len());
    }
    // Dynamic tags (`0x10..=0xEF`) and untagged opaque room/game payloads.
    ("game.relay.v1", payload.len())
}

/// Extract `room_id` from a `room.audio.sfu` inner payload
/// (`[ROOM_AUDIO_TAG][signed SfuAudio JSON]`). Returns `None` when the frame is
/// too short, not JSON, or missing the field — the caller may still fall back
/// to the relay peer's join-time room assignment.
fn room_id_from_room_audio_payload(payload: &[u8]) -> Option<String> {
    if payload.len() < 2 {
        return None;
    }
    // Skip the fixed channel tag; body is the signed JSON envelope.
    let json = std::str::from_utf8(&payload[1..]).ok()?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.get("payload")
        .and_then(|p| p.get("room_id"))
        .and_then(|r| r.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_owned())
}

/// Try to forward `fwd` to `recipient` if outbound `room.*` / core / game quota
/// allows it. Returns true when the datagram was sent.
fn try_forward_datagram(
    features: &FeatureRegistry,
    feature_id: &str,
    recipient: &str,
    fwd: &[u8],
    member: &RelayPeer,
) -> bool {
    if !features.gate_through_feature(feature_id, recipient, fwd.len()) {
        return false;
    }
    member
        .connection
        .send_datagram(Bytes::copy_from_slice(fwd))
        .is_ok()
}

/// Forward a datagram from a peer with symmetric per-feature quota gating.
fn handle_datagram(
    state: &Arc<RwLock<RelayState>>,
    features: &FeatureRegistry,
    room_audio_bridge: &Arc<RwLock<Option<RoomAudioBridgeHook>>>,
    from_peer: &str,
    data: &[u8],
) {
    let Some((target_idx, payload)) = wire::parse_datagram(data) else {
        return;
    };

    let (feature_id, quota_bytes) = relay_datagram_feature(payload);
    if quota_bytes > 0 && !features.gate_inbound_through_feature(feature_id, from_peer, quota_bytes)
    {
        debug!(
            "[relay] inbound quota exceeded for {} on {}; dropping datagram",
            &from_peer[..12.min(from_peer.len())],
            feature_id
        );
        return;
    }

    let st = state.read();
    let Some(from) = st.peers.get(from_peer) else {
        return;
    };
    let sender_index = from.peer_index;

    // Room audio broadcast: hand off to the SFU-aware bridge so the frame
    // reaches *every* room member — relay datagram for relay-connected peers,
    // WebSocket for the rest — rather than only relay-connected members, which
    // would silently partition WS-only members. The frame stays end-to-end
    // signed; the supernode just chooses each member's transport.
    if target_idx == wire::BROADCAST_INDEX && feature_id == "room.audio.sfu" {
        let bridge = room_audio_bridge.read().clone();
        if let Some(bridge) = bridge {
            // Prefer room_id from the signed SfuAudio payload (always present
            // on a well-formed frame). Fall back to the relay peer's last
            // join_room assignment — which can be missing after a reconnect
            // race or if SfuJoin beat the QUIC session by a few ms.
            let payload_room = room_id_from_room_audio_payload(payload);
            let room_id = payload_room.clone().or_else(|| from.room_id.clone());
            // Heal a missing peer.room_id so subsequent frames and bookkeeping
            // stay consistent without waiting for another SfuJoin.
            if from.room_id.is_none() {
                if let Some(ref rid) = payload_room {
                    drop(st);
                    {
                        let mut stw = state.write();
                        if let Some(peer) = stw.peers.get_mut(from_peer) {
                            peer.room_id = Some(rid.clone());
                        }
                        stw.rooms
                            .entry(rid.clone())
                            .or_default()
                            .insert(from_peer.to_string());
                    }
                } else {
                    drop(st);
                }
            } else {
                drop(st);
            }
            let inner = payload.to_vec();
            if let Some(room_id) = room_id {
                (bridge)(from_peer.to_string(), sender_index, room_id, inner);
            } else {
                debug!(
                    "[relay] room.audio.sfu from {} has no room_id (peer state or payload); dropping",
                    &from_peer[..12.min(from_peer.len())]
                );
            }
            return;
        }
        // No bridge installed (e.g. unit tests): fall through to the generic
        // relay-only broadcast below.
    }

    let mut relayed = 0u64;

    // Portal game relay: when the sender has an active game session (from
    // `GameRelayJoin`), fan-out only within that session — independent of SFU
    // voice room membership. Untagged opaque payloads still classified as
    // `game.relay.v1` for *quota* fall through to the SFU-room broadcast path
    // below so voice-room smoke tests and legacy room-scoped datagrams keep
    // working without a portal game join.
    if target_idx == wire::BROADCAST_INDEX && feature_id == "game.relay.v1" {
        if let Some(session_id) = st.peer_game_session.get(from_peer).cloned() {
            if let Some(members) = st.game_sessions.get(&session_id) {
                let fwd = wire::build_forwarded_datagram(sender_index, payload);
                for member_id in members {
                    if member_id == from_peer {
                        continue;
                    }
                    if let Some(member) = st.peers.get(member_id) {
                        if try_forward_datagram(features, feature_id, member_id, &fwd, member) {
                            relayed += fwd.len() as u64;
                        }
                    }
                }
                if relayed == 0 {
                    return;
                }
                drop(st);
                let mut st = state.write();
                st.total_bytes_relayed += relayed;
                if let Some(peer) = st.peers.get_mut(from_peer) {
                    peer.bytes_relayed += relayed;
                }
                return;
            }
        }
        // No active portal game session → SFU room broadcast below.
    }

    if target_idx == wire::BROADCAST_INDEX {
        // Broadcast to all room members except sender
        if let Some(ref room_id) = from.room_id {
            if let Some(members) = st.rooms.get(room_id) {
                let fwd = wire::build_forwarded_datagram(sender_index, payload);
                for member_id in members {
                    if member_id != from_peer {
                        if let Some(member) = st.peers.get(member_id) {
                            if try_forward_datagram(features, feature_id, member_id, &fwd, member) {
                                relayed += fwd.len() as u64;
                            }
                        }
                    }
                }
            }
        }
    } else {
        // Point-to-point forward — enforce same-room membership to prevent
        // cross-room datagram injection by a connected-but-wrong-room peer.
        if let Some(target_peer_id) = st.index_to_peer.get(&target_idx) {
            if let Some(target) = st.peers.get(target_peer_id) {
                // Both sender and target must be in the same non-None room.
                let same_room = match (&from.room_id, &target.room_id) {
                    (Some(a), Some(b)) => a == b,
                    _ => false,
                };
                if same_room {
                    let fwd = wire::build_forwarded_datagram(sender_index, payload);
                    if try_forward_datagram(features, feature_id, target_peer_id, &fwd, target) {
                        relayed += fwd.len() as u64;
                    }
                } else {
                    warn!(
                        "Dropping cross-room datagram from {} (idx={}) → idx={}",
                        &from_peer[..12.min(from_peer.len())],
                        sender_index,
                        target_idx
                    );
                }
            }
        }
    }

    if relayed == 0 {
        return;
    }

    // Update stats (drop read lock, take write)
    drop(st);
    let mut st = state.write();
    st.total_bytes_relayed += relayed;
    if let Some(peer) = st.peers.get_mut(from_peer) {
        peer.bytes_relayed += relayed;
    }
}

/// Send a JSON command on the relay command stream (stream 1).
async fn send_cmd(conn: &quinn::Connection, cmd: &serde_json::Value) -> anyhow::Result<()> {
    let data = wire::encode_relay_cmd(cmd);
    let mut send = conn.open_uni().await?;
    send.write_all(&data).await?;
    send.finish()?;
    Ok(())
}

/// Notify room members that a peer has joined (bidirectional).
/// Sends the new peer's info to all existing members AND sends
/// all existing members' info back to the new peer so both sides
/// can map sender indices to peer IDs for audio routing.
fn notify_room_peer_joined(state: &Arc<RwLock<RelayState>>, peer_id: &str, peer_index: u8) {
    let st = state.read();
    let Some(peer) = st.peers.get(peer_id) else {
        return;
    };
    let Some(ref room_id) = peer.room_id else {
        return;
    };
    let Some(members) = st.rooms.get(room_id) else {
        return;
    };

    // New peer info → existing members
    let new_peer_cmd =
        serde_json::json!({"relay_cmd": "peer_joined", "peer_id": peer_id, "index": peer_index});
    let new_peer_data = wire::encode_relay_cmd(&new_peer_cmd);

    let new_peer_conn = peer.connection.clone();

    for member_id in members {
        if member_id == peer_id {
            continue;
        }
        if let Some(member) = st.peers.get(member_id) {
            // Tell existing member about the new peer
            let conn = member.connection.clone();
            let data = new_peer_data.clone();
            tokio::spawn(async move {
                if let Ok(mut send) = conn.open_uni().await {
                    let _ = send.write_all(&data).await;
                    let _ = send.finish();
                }
            });

            // Tell the new peer about this existing member
            let existing_cmd = serde_json::json!({
                "relay_cmd": "peer_joined",
                "peer_id": member_id,
                "index": member.peer_index,
            });
            let existing_data = wire::encode_relay_cmd(&existing_cmd);
            let conn_to_new = new_peer_conn.clone();
            tokio::spawn(async move {
                if let Ok(mut send) = conn_to_new.open_uni().await {
                    let _ = send.write_all(&existing_data).await;
                    let _ = send.finish();
                }
            });
        }
    }
}

/// Notify room members that a peer has left.
fn notify_room_peer_left(state: &Arc<RwLock<RelayState>>, peer_id: &str, room_id: &str) {
    let st = state.read();
    let Some(members) = st.rooms.get(room_id) else {
        return;
    };

    let cmd = serde_json::json!({"relay_cmd": "peer_left", "peer_id": peer_id});
    let data = wire::encode_relay_cmd(&cmd);

    for member_id in members {
        if member_id != peer_id {
            if let Some(member) = st.peers.get(member_id) {
                let conn = member.connection.clone();
                let data = data.clone();
                tokio::spawn(async move {
                    if let Ok(mut send) = conn.open_uni().await {
                        let _ = send.write_all(&data).await;
                        let _ = send.finish();
                    }
                });
            }
        }
    }
}

/// Remove disconnected peers.
fn cleanup_stale_peers(state: &Arc<RwLock<RelayState>>, features: &FeatureRegistry) {
    let mut st = state.write();
    let stale: Vec<String> = st
        .peers
        .iter()
        .filter(|(_, p)| p.connection.close_reason().is_some())
        .map(|(id, _)| id.clone())
        .collect();
    for id in stale {
        debug!("Cleaning up stale relay peer: {}", &id[..12.min(id.len())]);
        st.remove_peer(&id);
        // Quota symmetry: clear per-(feature, peer) buckets on removal.
        features.clear_peer_quotas(&id);
        features.clear_peer_outbound_quotas(&id);
    }
}

/// Extract peer_id from client TLS certificate CN.
/// The client's self-signed cert has CN = hex(ed25519_pub_bytes).
/// We convert to base64url to match the identity format used in the allowed set.
pub(crate) fn extract_peer_id(conn: &quinn::Connection) -> anyhow::Result<String> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| anyhow::anyhow!("no peer certificate"))?;
    let certs = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| anyhow::anyhow!("unexpected cert type"))?;
    let cert = certs
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty cert chain"))?;
    // Parse the certificate to extract CN (hex-encoded public key)
    let cn_hex = x509_parser_lite(cert.as_ref())?;
    // Convert hex CN to base64url peer_id (matching Identity.public_id format)
    let pub_bytes =
        hex::decode(&cn_hex).map_err(|e| anyhow::anyhow!("invalid hex CN '{cn_hex}': {e}"))?;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    Ok(URL_SAFE_NO_PAD.encode(&pub_bytes))
}

/// Minimal X.509 CN extraction — the CN is the base64url public key.
/// In our protocol, the self-signed cert has CN = hex(ed25519_pub) or base64url(pub).
/// We accept both formats and normalize to base64url.
fn x509_parser_lite(der: &[u8]) -> anyhow::Result<String> {
    // Use rcgen to parse? No — rcgen only generates. Use ring or manual ASN.1.
    // For simplicity, use the rustls-internal cert parsing or a lightweight approach.
    // The CN is embedded in the subject field. Let's use a simple ASN.1 walk.
    // Actually, quinn gives us the raw certs. Let's parse the subject CN manually.
    // rcgen sets CN via distinguished_name. The CN OID is 2.5.4.3.
    let cn_oid = &[0x55, 0x04, 0x03]; // OID bytes for CN
                                      // Walk the DER looking for the CN OID followed by a UTF8String/PrintableString
    if let Some(pos) = find_subsequence(der, cn_oid) {
        // After OID, there's a tag+length for the value
        let after_oid = pos + cn_oid.len();
        if after_oid + 2 <= der.len() {
            let tag = der[after_oid];
            let len = der[after_oid + 1] as usize;
            if (tag == 0x0C || tag == 0x13) && after_oid + 2 + len <= der.len() {
                let cn = std::str::from_utf8(&der[after_oid + 2..after_oid + 2 + len])
                    .map_err(|e| anyhow::anyhow!("invalid CN UTF-8: {e}"))?;
                return Ok(cn.to_string());
            }
        }
    }
    Err(anyhow::anyhow!("CN not found in certificate"))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Build a quinn server config with a self-signed cert.
/// Requests client certificates (accepted without CA verification)
/// so we can extract peer_id from the client cert CN.
pub(crate) fn build_quinn_server_config(
    identity_pub_id: &str,
) -> anyhow::Result<(ServerConfig, CertificateDer<'static>)> {
    let mut params = rcgen::CertificateParams::new(vec![])?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, identity_pub_id);
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
    let cert = params.self_signed(&key_pair)?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    let verifier = Arc::new(AcceptAllClientCerts);
    let mut server_crypto = rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(vec![cert_der.clone()], key_der)?;
    server_crypto.alpn_protocols = vec![b"conquerd/1".to_vec()];

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        Duration::from_secs(IDLE_TIMEOUT_S).try_into().unwrap(),
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(KEEPALIVE_INTERVAL_S)));
    transport.max_concurrent_uni_streams(64u32.into());
    transport.datagram_receive_buffer_size(Some(2 * 1024 * 1024));

    let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?;
    let mut server_config = ServerConfig::with_crypto(Arc::new(quinn_crypto));
    server_config.transport_config(Arc::new(transport));

    Ok((server_config, cert_der))
}

/// Client certificate verifier that accepts any self-signed certificate.
/// We do our own peer authorization via the `allowed` set after extracting
/// the peer_id from the certificate CN.
#[derive(Debug)]
struct AcceptAllClientCerts;

impl ClientCertVerifier for AcceptAllClientCerts {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        // Accept any client certificate — we verify the peer_id ourselves.
        Ok(ClientCertVerified::assertion())
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

/// Server-certificate verifier that accepts any self-signed certificate.
/// Used by the dialing side of an intra-cluster link; the peer is authenticated
/// by its cert CN (`extract_peer_id`) checked against the cluster roster, and by
/// requiring Ed25519-signed cluster messages — not by a CA chain.
#[derive(Debug)]
pub(crate) struct AcceptAnyServerCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyServerCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![rustls::SignatureScheme::ED25519]
    }
}

/// Build a quinn client config that presents a self-signed Ed25519 cert whose
/// CN is the **hex** encoding of `own_identity_pub_bytes` — the form
/// [`extract_peer_id`] decodes on the accepting side. Reused by the
/// intra-cluster link dialer.
pub(crate) fn build_quinn_client_config(
    own_identity_pub_bytes: &[u8],
) -> anyhow::Result<quinn::ClientConfig> {
    let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
    let cn_hex = hex::encode(own_identity_pub_bytes);
    let mut params = rcgen::CertificateParams::new(vec![])?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn_hex);
    let cert = params.self_signed(&key_pair)?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
        .with_client_auth_cert(vec![cert_der], key_der)?;
    client_crypto.alpn_protocols = vec![b"conquerd/1".to_vec()];

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        Duration::from_secs(IDLE_TIMEOUT_S).try_into().unwrap(),
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(KEEPALIVE_INTERVAL_S)));

    let mut cfg = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
    ));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── RelayState ──────────────────────────────────────────────────────────

    #[test]
    fn relay_state_new_is_empty() {
        let s = RelayState::new();
        assert!(s.allowed.is_empty());
        assert!(s.peers.is_empty());
        assert!(s.rooms.is_empty());
        assert_eq!(s.next_index, 1);
        assert_eq!(s.total_bytes_relayed, 0);
    }

    #[test]
    fn allocate_index_returns_sequential() {
        let mut s = RelayState::new();
        assert_eq!(s.allocate_index(), Some(1));
        assert_eq!(s.allocate_index(), Some(2));
        assert_eq!(s.allocate_index(), Some(3));
    }

    #[test]
    fn allocate_index_skips_occupied_slots() {
        let mut s = RelayState::new();
        s.index_to_peer.insert(1, "peer-a".into());
        s.index_to_peer.insert(2, "peer-b".into());
        // next_index = 1, both 1 and 2 occupied → first free is 3
        assert_eq!(s.allocate_index(), Some(3));
    }

    #[test]
    fn allocate_index_exhaustion_returns_none() {
        let mut s = RelayState::new();
        // Fill all 254 slots (1..=254)
        for i in 1u8..=254 {
            s.index_to_peer.insert(i, format!("peer-{i}"));
        }
        assert_eq!(s.allocate_index(), None);
    }

    #[test]
    fn remove_peer_nonexistent_is_noop() {
        let mut s = RelayState::new();
        s.remove_peer("ghost"); // should not panic
        assert!(s.peers.is_empty());
    }

    #[test]
    fn remove_peer_cleans_up_room_membership() {
        let mut s = RelayState::new();
        s.rooms
            .entry("room-1".into())
            .or_default()
            .insert("peer-a".into());
        s.rooms
            .entry("room-1".into())
            .or_default()
            .insert("peer-b".into());
        s.remove_peer("peer-a");
        assert_eq!(s.rooms["room-1"].len(), 1);
        assert!(!s.rooms["room-1"].contains("peer-a"));
    }

    #[test]
    fn remove_peer_drops_empty_room() {
        let mut s = RelayState::new();
        s.rooms
            .entry("room-solo".into())
            .or_default()
            .insert("peer-only".into());
        s.remove_peer("peer-only");
        assert!(!s.rooms.contains_key("room-solo"));
    }

    fn test_features() -> Arc<FeatureRegistry> {
        let r = Arc::new(FeatureRegistry::new());
        for cap in [
            conquerd_features::wellknown::core_audio_opus(),
            conquerd_features::wellknown::core_chat_v1(),
            conquerd_features::wellknown::core_file_v1(),
            conquerd_features::wellknown::game_relay_v1(),
            conquerd_features::wellknown::room_audio_sfu(),
            conquerd_features::wellknown::core_video_vp8(),
            conquerd_features::wellknown::room_video_sfu(),
        ] {
            let _ = r.upsert(cap);
        }
        r
    }

    #[test]
    fn send_room_datagram_unconnected_peer_returns_none() {
        // A peer that holds no live relay connection must yield `None` so the
        // bridge falls back to the WebSocket SFU path rather than charging
        // quota or silently dropping the frame.
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        assert_eq!(
            srv.send_room_datagram("ghost-peer", &[0x01, 0x04, 0x7b]),
            None
        );
    }

    #[test]
    fn room_id_from_room_audio_payload_reads_envelope() {
        use conquerd_features::channel_frame::ROOM_AUDIO_TAG;
        let signed_json =
            br#"{"type":"sfu_audio","sender":"abc","payload":{"room_id":"default","seq":1}}"#;
        let mut payload = vec![ROOM_AUDIO_TAG];
        payload.extend_from_slice(signed_json);
        assert_eq!(
            room_id_from_room_audio_payload(&payload).as_deref(),
            Some("default")
        );
        // Missing field / not JSON → None (caller falls back to peer.room_id).
        assert!(room_id_from_room_audio_payload(&[ROOM_AUDIO_TAG, b'{']).is_none());
        assert!(room_id_from_room_audio_payload(&[]).is_none());
    }

    #[test]
    fn room_audio_datagram_wire_round_trip() {
        // End-to-end byte layout the client and relay agree on:
        //   client → relay:  [BROADCAST_INDEX][ROOM_AUDIO_TAG][signed json]
        //   relay  → member: [sender_index ][ROOM_AUDIO_TAG][signed json]
        use conquerd_features::channel_frame::ROOM_AUDIO_TAG;
        let signed_json = br#"{"type":"sfu_audio","sender":"abc"}"#;

        // What the client's `send_room_audio` puts on the wire.
        let mut outbound = vec![wire::BROADCAST_INDEX, ROOM_AUDIO_TAG];
        outbound.extend_from_slice(signed_json);

        // Relay strips the target index and classifies by the inner tag.
        let (target_idx, payload) = wire::parse_datagram(&outbound).unwrap();
        assert_eq!(target_idx, wire::BROADCAST_INDEX);
        assert_eq!(relay_datagram_feature(payload).0, "room.audio.sfu");

        // Relay re-frames with the sender's index for fan-out.
        let fwd = wire::build_forwarded_datagram(7, payload);
        assert_eq!(fwd[0], 7);
        assert_eq!(fwd[1], ROOM_AUDIO_TAG);
        // What the receiving client extracts: it ignores fwd[0], checks the
        // tag at fwd[1], and recovers the signed JSON from fwd[2..].
        assert_eq!(&fwd[2..], signed_json);
    }

    #[test]
    fn relay_datagram_feature_maps_channel_tags() {
        use conquerd_features::channel_frame::{encode_frame, AUDIO_TAG, CHAT_TAG, FILE_TAG};

        let (fid, n) = relay_datagram_feature(&encode_frame(AUDIO_TAG, b"opus"));
        assert_eq!(fid, "core.audio.opus");
        assert_eq!(n, 5);

        let (fid, _) = relay_datagram_feature(&encode_frame(CHAT_TAG, b"{}"));
        assert_eq!(fid, "core.chat.v1");

        let (fid, _) = relay_datagram_feature(&encode_frame(FILE_TAG, b"chunk"));
        assert_eq!(fid, "core.file.v1");

        // Room audio rides its own fixed tag so the relay attributes it to
        // `room.audio.sfu` quota, not the direct-call `core.audio.opus` bucket.
        let (fid, _) = relay_datagram_feature(&encode_frame(
            conquerd_features::channel_frame::ROOM_AUDIO_TAG,
            b"signed-json",
        ));
        assert_eq!(fid, "room.audio.sfu");

        // Portal game relay has a fixed first-party tag (identity path).
        let (fid, _) = relay_datagram_feature(&encode_frame(
            conquerd_features::channel_frame::GAME_RELAY_TAG,
            b"opaque-game",
        ));
        assert_eq!(fid, "game.relay.v1");

        // Untagged opaque payload still accounts as game.relay for quota.
        let (fid, _) = relay_datagram_feature(b"opaque-game-payload");
        assert_eq!(fid, "game.relay.v1");

        // Video rides its own fixed tags, so a congested video stream is
        // metered (and shed) independently of the call audio beside it.
        let (fid, _) = relay_datagram_feature(&encode_frame(
            conquerd_features::channel_frame::VIDEO_TAG,
            b"fragment",
        ));
        assert_eq!(fid, "core.video.vp8");

        let (fid, _) = relay_datagram_feature(&encode_frame(
            conquerd_features::channel_frame::ROOM_VIDEO_TAG,
            b"fragment",
        ));
        assert_eq!(fid, "room.video.sfu");
    }

    /// Room video needs **no dedicated arm** in `handle_datagram`: it is not
    /// `room.audio.sfu` (so it skips the SFU bridge, which would try to parse
    /// it as JSON and fail) and not `game.relay.v1` (so it skips the game
    /// session lookup), leaving it to the generic broadcast that forwards to
    /// `st.rooms[from.room_id]` opaquely.
    ///
    /// This test exists because that behaviour is load-bearing but invisible:
    /// adding an arm above the generic broadcast, or making the audio bridge
    /// match more broadly, would silently break room video.
    #[test]
    fn room_video_is_forwarded_opaquely_without_a_dedicated_arm() {
        use conquerd_features::channel_frame::{encode_frame, ROOM_VIDEO_TAG};

        // A binary fragment: deliberately not valid UTF-8, let alone JSON, so
        // any code path that tries to parse it would be caught here.
        let fragment = [0x01u8, 0xFF, 0x00, 0x80, 0xFE];
        let payload = encode_frame(ROOM_VIDEO_TAG, &fragment);

        let (fid, bytes) = relay_datagram_feature(&payload);
        assert_eq!(fid, "room.video.sfu");
        assert_eq!(bytes, payload.len());

        // Neither special-case arm in `handle_datagram` may claim it.
        assert_ne!(fid, "room.audio.sfu");
        assert_ne!(fid, "game.relay.v1");

        // The JSON room-id extractor must decline rather than panic, since it
        // is only reached for room audio.
        assert_eq!(room_id_from_room_audio_payload(&payload), None);

        // Fan-out re-frames with the sender index and leaves the body intact.
        let fwd = wire::build_forwarded_datagram(3, &payload);
        assert_eq!(fwd[0], 3);
        assert_eq!(&fwd[1..], &payload[..]);
    }

    // ── QUICRelayServer (no live QUIC needed) ───────────────────────────────

    #[test]
    fn allow_and_revoke_peer() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.allow_peer("peer-x");
        assert!(srv.state.read().allowed.contains("peer-x"));
        srv.revoke_peer("peer-x");
        assert!(!srv.state.read().allowed.contains("peer-x"));
    }

    #[test]
    fn allow_peer_strips_padding() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.allow_peer("peer-padded==");
        assert!(srv.state.read().allowed.contains("peer-padded"));
        assert!(!srv.state.read().allowed.contains("peer-padded=="));
    }

    #[test]
    fn revoke_peer_clears_quota_buckets() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        let features = srv.features.clone();
        srv.allow_peer("peer-x");
        // Exhaust peer-x's chat budget in both directions (32 KB/s).
        assert!(features.gate_through_feature("core.chat.v1", "peer-x", 32_768));
        assert!(!features.gate_through_feature("core.chat.v1", "peer-x", 32_768));
        assert!(features.gate_inbound_through_feature("core.chat.v1", "peer-x", 32_768));
        assert!(!features.gate_inbound_through_feature("core.chat.v1", "peer-x", 32_768));
        // Revoking must reset both directions so a future session starts fresh.
        srv.revoke_peer("peer-x");
        assert!(features.gate_through_feature("core.chat.v1", "peer-x", 32_768));
        assert!(features.gate_inbound_through_feature("core.chat.v1", "peer-x", 32_768));
    }

    #[test]
    fn join_room_creates_room_entry() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.allow_peer("peer-a");
        srv.join_room("peer-a", "room-1");
        assert!(srv.get_room_peers("room-1").contains(&"peer-a".to_string()));
    }

    #[test]
    fn join_room_peer_appears_in_multiple_rooms_without_quic_connection() {
        // Without a live QUIC connection the peer has no RelayPeer entry, so
        // join_room cannot look up the previous room_id.  The peer ends up in
        // every room it was asked to join rather than being moved between them.
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.allow_peer("peer-a");
        srv.join_room("peer-a", "room-1");
        srv.join_room("peer-a", "room-2");
        // peer-a is in room-2 (latest join always succeeds)
        assert!(srv.get_room_peers("room-2").contains(&"peer-a".to_string()));
    }

    #[test]
    fn leave_room_is_noop_without_quic_connection() {
        // Without a live QUIC connection the peer has no RelayPeer entry.
        // leave_room looks up room_id via state.peers, so it is a safe no-op.
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.allow_peer("peer-a");
        srv.join_room("peer-a", "room-1");
        srv.leave_room("peer-a"); // no-op — peer-a has no RelayPeer entry
                                  // peer-a is still tracked in the room set because leave_room only
                                  // removes via RelayPeer.room_id which requires a QUIC connection.
        assert!(srv.get_room_peers("room-1").contains(&"peer-a".to_string()));
    }

    #[test]
    fn get_room_peers_empty_room_returns_empty() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        assert!(srv.get_room_peers("nonexistent").is_empty());
    }

    #[test]
    fn stats_reflects_allowed_and_room_counts() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.allow_peer("peer-a");
        srv.allow_peer("peer-b");
        srv.join_room("peer-a", "room-1");
        srv.join_room("peer-b", "room-1");
        let s = srv.stats();
        assert_eq!(s.active_tickets, 2);
        assert_eq!(s.active_rooms, 1);
        assert_eq!(s.peers_connected, 0);
        assert_eq!(s.bytes_relayed_total, 0);
        assert_eq!(s.rooms.len(), 1);
        assert_eq!(s.rooms[0].members, 2);
    }

    #[test]
    fn shutdown_is_safe_to_call() {
        let srv = QUICRelayServer::new("test-id".into(), test_features());
        srv.shutdown();
    }

    // =====================================================================
    // P0 Expanded Smoke: Real localhost QUIC listener + authenticated client
    // =====================================================================

    use std::sync::Arc;
    use std::time::Duration;

    use ed25519_dalek::SigningKey;
    use quinn::{ClientConfig, Endpoint};
    use rcgen::KeyPair;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::DigitallySignedStruct;

    /// Minimal server cert verifier that accepts any certificate (for localhost tests).
    #[derive(Debug)]
    struct AcceptAnyServerCert;

    impl ServerCertVerifier for AcceptAnyServerCert {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![rustls::SignatureScheme::ED25519]
        }
    }

    /// Build a quinn client Endpoint for tests that presents a self-signed
    /// Ed25519 certificate whose CN is the **hex** encoding of the public key
    /// (matching what the real client and the relay's extract_peer_id expect).
    fn build_test_client_endpoint(
        client_pub_bytes: &[u8],
    ) -> anyhow::Result<(Endpoint, SigningKey)> {
        let signing_key = SigningKey::generate(&mut rand::thread_rng());
        let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)?;

        let cn_hex = hex::encode(client_pub_bytes);

        let mut params = rcgen::CertificateParams::new(vec![])?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, cn_hex);
        let cert = params.self_signed(&key_pair)?;

        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));

        let provider = Arc::new(rustls::crypto::ring::default_provider());

        let mut client_crypto = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyServerCert))
            .with_client_auth_cert(vec![cert_der], key_der)?;

        client_crypto.alpn_protocols = vec![b"conquerd/1".to_vec()];

        let client_cfg = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)?,
        ));

        // Bind to ephemeral port on localhost
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse()?)?;
        endpoint.set_default_client_config(client_cfg);

        Ok((endpoint, signing_key))
    }

    #[tokio::test]
    async fn p0_real_localhost_quic_relay_authenticated_connection_smoke() {
        // --- Server side (supernode identity) ---
        let server_signing_key = SigningKey::generate(&mut rand::thread_rng());
        let server_pub_bytes = server_signing_key.verifying_key().to_bytes();
        let server_id = crate::crypto::b64url_encode(&server_pub_bytes);

        let srv = QUICRelayServer::new(server_id.clone(), test_features());

        // --- Client identity that we will allow ---
        let client_signing_key = SigningKey::generate(&mut rand::thread_rng());
        let client_pub_bytes = client_signing_key.verifying_key().to_bytes();
        let client_peer_id = crate::crypto::b64url_encode(&client_pub_bytes);

        srv.allow_peer(&client_peer_id);

        // Start real QUIC listener on localhost ephemeral port
        let port = srv
            .start("127.0.0.1:0".parse().unwrap())
            .await
            .expect("failed to start relay listener");

        // --- Client side ---
        let (client_endpoint, _client_key) = build_test_client_endpoint(&client_pub_bytes)
            .expect("failed to build test client endpoint");

        // Real QUIC connection using the same endpoint we built for the test
        // (this exercises the exact mTLS + ALPN + transport config the real
        // QuicRelayClient would use).
        let connecting = client_endpoint
            .connect(format!("127.0.0.1:{}", port).parse().unwrap(), "conquerd")
            .expect("failed to initiate quinn connect");

        let connection = tokio::time::timeout(Duration::from_secs(5), connecting)
            .await
            .expect("connect timed out")
            .expect("QUIC handshake to localhost relay failed");

        // Basic liveness
        assert!(
            connection.close_reason().is_none(),
            "connection should be alive"
        );

        // Give the server a moment to register the peer in its state
        tokio::time::sleep(Duration::from_millis(150)).await;

        {
            let state = srv.state.read();
            assert!(
                state.peers.contains_key(&client_peer_id),
                "server should have registered the authenticated peer after real QUIC connection"
            );
        }

        // --- Exercise room/relay path over the live connection ---
        // This is the key expansion of the P0 smoke: we now drive actual
        // room membership and a room-broadcast datagram over a *real* QUIC
        // connection (not just in-memory state objects).
        let (_conn_b, peer_b, _ep_b) = connect_second_test_peer(&srv, port, &server_id).await;
        srv.join_room(&client_peer_id, "p0-smoke-room");
        srv.join_room(&peer_b, "p0-smoke-room");

        // Give the server a moment to process the room membership update
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stats_after_join = srv.stats();
        assert_eq!(
            stats_after_join.active_rooms, 1,
            "one room should be active"
        );
        assert_eq!(stats_after_join.rooms.len(), 1);
        assert_eq!(stats_after_join.rooms[0].members, 2);

        // Send a real broadcast datagram from the client (0xFF = room broadcast)
        // This exercises handle_datagram + room forwarding logic.
        let room_broadcast = vec![0xFF, 0x42, 0x43, 0x44]; // broadcast + dummy payload
        let _ = connection.send_datagram(Bytes::from(room_broadcast));

        // Let the datagram be processed
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stats_after_datagram = srv.stats();
        // bytes_relayed_total should have increased (at least the forwarded bytes)
        assert!(
            stats_after_datagram.bytes_relayed_total > 0,
            "relay should have processed at least one datagram"
        );

        // Leave the room (exercises leave path)
        srv.leave_room(&client_peer_id);

        // Clean shutdown from client side
        connection.close(0u32.into(), b"test_done");
        srv.shutdown();

        // Final state check after shutdown
        tokio::time::sleep(Duration::from_millis(50)).await;
        let final_state = srv.state.read();
        assert!(
            !final_state.peers.contains_key(&client_peer_id),
            "peer should be cleaned up after connection close"
        );
    }

    // =====================================================================
    // Additional P0 relay/room smoke tests over real localhost QUIC
    // =====================================================================

    /// Helper: Connect a second (or Nth) real test peer to a running relay server.
    async fn connect_second_test_peer(
        srv: &QUICRelayServer,
        port: u16,
        _server_id: &str,
    ) -> (quinn::Connection, String, quinn::Endpoint) {
        let signing_key = SigningKey::generate(&mut rand::thread_rng());
        let pub_bytes = signing_key.verifying_key().to_bytes();
        let peer_id = crate::crypto::b64url_encode(&pub_bytes);

        srv.allow_peer(&peer_id);

        let (endpoint, _key) = build_test_client_endpoint(&pub_bytes)
            .expect("failed to build second test client endpoint");

        let connecting = endpoint
            .connect(format!("127.0.0.1:{}", port).parse().unwrap(), "conquerd")
            .expect("failed to initiate connect for second peer");

        let conn = tokio::time::timeout(Duration::from_secs(5), connecting)
            .await
            .expect("second peer connect timed out")
            .expect("second peer QUIC handshake failed");

        // Wait for server to register
        tokio::time::sleep(Duration::from_millis(120)).await;

        (conn, peer_id, endpoint)
    }

    #[tokio::test]
    async fn p0_two_real_peers_room_broadcast_over_quic() {
        // Server
        let server_key = SigningKey::generate(&mut rand::thread_rng());
        let server_pub = server_key.verifying_key().to_bytes();
        let server_id = crate::crypto::b64url_encode(&server_pub);

        let srv = QUICRelayServer::new(server_id.clone(), test_features());
        let port = srv.start("127.0.0.1:0".parse().unwrap()).await.unwrap();

        // Peer A
        let (conn_a, peer_a, _ep_a) = {
            let key = SigningKey::generate(&mut rand::thread_rng());
            let pub_b = key.verifying_key().to_bytes();
            let pid = crate::crypto::b64url_encode(&pub_b);
            srv.allow_peer(&pid);
            let (ep, _) = build_test_client_endpoint(&pub_b).unwrap();
            let c = ep
                .connect(format!("127.0.0.1:{}", port).parse().unwrap(), "conquerd")
                .unwrap()
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            (c, pid, ep)
        };

        // Peer B
        let (conn_b, peer_b, _ep_b) = connect_second_test_peer(&srv, port, &server_id).await;

        // Both join the same room
        srv.join_room(&peer_a, "p0-multi-room");
        srv.join_room(&peer_b, "p0-multi-room");
        tokio::time::sleep(Duration::from_millis(50)).await;

        let stats = srv.stats();
        assert_eq!(stats.active_rooms, 1);
        assert_eq!(stats.rooms[0].members, 2);

        // Peer A sends a room broadcast datagram
        let payload = b"room-broadcast-payload-from-a";
        let datagram = [&[0xFF], payload.as_slice()].concat();
        conn_a.send_datagram(Bytes::from(datagram)).unwrap();

        // Give forwarding time
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Peer B should be able to read the forwarded datagram (sender index + original payload)
        let received = tokio::time::timeout(Duration::from_millis(300), conn_b.read_datagram())
            .await
            .expect("timeout waiting for forwarded datagram")
            .expect("failed to read datagram on peer B");

        assert!(
            received.len() > 1,
            "forwarded datagram should have sender index + payload"
        );
        // First byte should be the sender's index (not 0xFF anymore after forwarding)
        assert_ne!(
            received[0], 0xFF,
            "forwarded datagram should have sender index"
        );

        // Cleanup
        conn_a.close(0u32.into(), b"done");
        conn_b.close(0u32.into(), b"done");
        srv.shutdown();
    }

    #[tokio::test]
    async fn p0_unauthorized_peer_rejected() {
        let server_key = SigningKey::generate(&mut rand::thread_rng());
        let server_pub = server_key.verifying_key().to_bytes();
        let server_id = crate::crypto::b64url_encode(&server_pub);

        let srv = QUICRelayServer::new(server_id.clone(), test_features());
        let port = srv.start("127.0.0.1:0".parse().unwrap()).await.unwrap();

        // A peer that is NOT allowed
        let bad_key = SigningKey::generate(&mut rand::thread_rng());
        let bad_pub = bad_key.verifying_key().to_bytes();
        let (bad_endpoint, _) = build_test_client_endpoint(&bad_pub).unwrap();

        let connecting = bad_endpoint
            .connect(format!("127.0.0.1:{}", port).parse().unwrap(), "conquerd")
            .unwrap();

        // The connection may establish at TLS level, but the server should close it quickly
        // because the peer is not in the allowed set.
        let result = tokio::time::timeout(Duration::from_secs(3), connecting).await;

        match result {
            Ok(Ok(conn)) => {
                // If it connected, it should be closed almost immediately by the server
                tokio::time::sleep(Duration::from_millis(150)).await;
                assert!(
                    conn.close_reason().is_some(),
                    "unauthorized peer connection should be closed by server"
                );
            }
            Ok(Err(_)) => {
                // Handshake failed or was rejected — also acceptable
            }
            Err(_) => {
                // Timed out waiting — in practice the server closes fast, but we accept it
            }
        }

        srv.shutdown();
    }

    #[tokio::test]
    async fn p0_room_leave_and_rejoin() {
        // Similar setup to the main smoke but focused on leave/rejoin lifecycle
        let server_key = SigningKey::generate(&mut rand::thread_rng());
        let server_pub = server_key.verifying_key().to_bytes();
        let server_id = crate::crypto::b64url_encode(&server_pub);

        let srv = QUICRelayServer::new(server_id.clone(), test_features());
        let port = srv.start("127.0.0.1:0".parse().unwrap()).await.unwrap();

        let client_key = SigningKey::generate(&mut rand::thread_rng());
        let client_pub = client_key.verifying_key().to_bytes();
        let client_id = crate::crypto::b64url_encode(&client_pub);
        srv.allow_peer(&client_id);

        let (endpoint, _) = build_test_client_endpoint(&client_pub).unwrap();
        let connecting = endpoint
            .connect(format!("127.0.0.1:{}", port).parse().unwrap(), "conquerd")
            .unwrap();

        let conn = connecting.await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        srv.join_room(&client_id, "temp-room");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(srv.stats().active_rooms, 1);

        srv.leave_room(&client_id);
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(srv.stats().active_rooms, 0);

        srv.join_room(&client_id, "temp-room-2");
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(srv.stats().active_rooms, 1);

        conn.close(0u32.into(), b"done");
        srv.shutdown();
    }
}
