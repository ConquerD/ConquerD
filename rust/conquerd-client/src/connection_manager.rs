//! Connection manager — signaling, QUIC transport, and invite handshake.
//!
//! Architecture:
//! - One `ConnectionManager` per session, owned by the application.
//! - An async `tokio::task` drives the WebSocket signaling loop.
//! - A `quinn::Endpoint` handles peer-to-peer QUIC connections.
//! - `mpsc` channels carry inbound events to the application layer.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use conquerd_features::{
    channel_frame::{self, FrameClass},
    client_modules::register_client_modules,
    wellknown, AuthTier, CapabilityDescriptor, FeatureRegistry, InvocationContext, ReplayGuard,
};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

use crate::avatar_config::AvatarConfig as PeerAvatarConfig;
use crate::error::Result;
use crate::feature_trust::{FeatureTrustGate, FeatureTrustStore, TrustDecision};
use crate::file_transfer::{FileTransferManager, TransferEvent};
use crate::identity::Identity;
use crate::peer_store::PeerStore;
use crate::protocol::{MessageType, SignalingMessage};
use crate::quic_relay_client::QuicRelayClient;
use crate::quic_tls;
use crate::session_state::PeerSessionState;
use crate::web_app_client::{self, WebAppResponse};

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

const _CONNECT_TIMEOUT_S: f64 = 4.0;
const WS_RECONNECT_DELAY_S: u64 = 5;
const PING_INTERVAL_S: u64 = 30;

/// Channel tag for `core.audio.opus` datagrams.
///
/// Aliases the shared first-party tag in `conquerd_features::channel_frame`
/// (reserved low range 0x01–0x0F) so the audio datagram path doesn't
/// sprinkle magic numbers and stays in lock-step with the other channels.
const AUDIO_CHANNEL_TAG: u8 = channel_frame::AUDIO_TAG;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Events emitted by the connection manager to the application layer.
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    /// A new peer connected via signaling or QUIC.
    PeerConnected(String),
    /// A peer's session ended.
    PeerDisconnected(String),
    /// An inbound signaling message for the app layer to handle.
    SignalingMessage(SignalingMessage),
    /// A text chat message arrived.
    ChatMessage {
        peer_id: String,
        message_id: String,
        body: String,
        timestamp: f64,
        sender_handle: String,
    },
    /// Chat delivery ack.
    ChatAck { peer_id: String, message_id: String },
    /// A locally-authored chat message could not be handed to a connected route.
    ChatSendFailed {
        peer_id: String,
        message_id: String,
        reason: String,
    },
    /// Call request from a remote peer.
    CallRequest { peer_id: String },
    /// Remote peer accepted our call request.
    CallAccepted { peer_id: String },
    /// Remote peer rejected or ended the call.
    CallEnded { peer_id: String },
    /// Supernode relay ticket received.
    RelayGranted {
        supernode_id: String,
        ticket: String,
        relay_host: String,
        relay_port: u16,
    },
    /// Connection to a supernode WebSocket established.
    SupernodeConnected(String),
    /// Connection to a supernode WebSocket lost.
    SupernodeDisconnected(String),
    /// Session state update for a peer.
    SessionStateUpdate(PeerSessionState),
    /// Typing indicator from a peer.
    TypingIndicator { peer_id: String, is_typing: bool },
    /// Room member list changed (full snapshot from SFU_MEMBERS).
    RoomMembersChanged(Vec<String>),
    /// A peer joined the current SFU room.
    RoomPeerJoined { peer_id: String },
    /// A peer left the current SFU room.
    RoomPeerLeft { peer_id: String },
    /// A text chat message arrived in an SFU room.
    RoomChatMessage {
        room_id: String,
        sender_id: String,
        sender_handle: String,
        body: String,
        timestamp: f64,
    },
    /// A peer updated their display handle.
    HandleUpdated { peer_id: String, handle: String },
    /// A peer sent their avatar visual config.
    AvatarConfigUpdated { peer_id: String },
    /// A peer sent their capability list.
    CapabilityAnnounced {
        peer_id: String,
        /// Raw JSON array of capability descriptors.
        caps_json: String,
    },
    /// A peer invoked a capability that passed all framework gates and was
    /// dispatched to the local feature module (if any).
    CapabilityInvoked {
        peer_id: String,
        feature_id: String,
        params: Value,
    },
    /// A peer invoked a bespoke (non-first-party) capability that has no
    /// stored trust decision. The UI must prompt the user, then call
    /// [`ConnectionCommand::SetFeatureTrust`] with the decision and re-send
    /// the invoke if allowed (via [`ConnectionCommand::SendCapabilityInvoke`]
    /// or by re-driving the original invoke flow).
    CapabilityInvokePending {
        peer_id: String,
        feature_id: String,
        params: Value,
    },
    /// A peer broadcast an endpoint update.
    EndpointUpdated {
        peer_id: String,
        endpoints: Vec<String>,
    },
    /// Supernode sent its homepage / portal information.
    SupernodeInfoReceived {
        supernode_id: String,
        homepage_url: String,
        title: String,
        /// WebTransport base URL (e.g. `https://host:8443`) from the supernode;
        /// empty string when the supernode does not advertise `web.host.h3.v1`.
        wt_url: String,
        /// SHA-256 fingerprint (lowercase hex) of the supernode's self-signed
        /// WebTransport TLS cert.  Passed to game pages so they can use
        /// `serverCertificateHashes` — no CA cert needed.
        cert_fingerprint: String,
    },
    /// Supernode requires a portal visit before granting relay access.
    RelayPaymentRequired {
        supernode_id: String,
        portal_url: String,
    },
    /// Supernode sent a list of available SFU rooms.
    RoomListReceived {
        supernode_id: String,
        /// Raw JSON array of room descriptors.
        rooms_json: String,
    },
    /// A peer sent a presence update.
    PresenceUpdated { peer_id: String, status: String },
    /// Inbound SFU_AUDIO relayed from the supernode (Opus bytes from a room peer).
    SfuAudioReceived { peer_id: String, opus_data: Vec<u8> },
    /// Inbound direct-peer audio (Opus bytes from a 1:1 QUIC session).
    DirectAudioReceived { peer_id: String, opus_data: Vec<u8> },
    /// An invite handshake completed and the peer was added to the store.
    InviteAccepted { peer_id: String, handle: String },
    /// Remote peer sent a file offer.
    FileOffered {
        transfer_id: String,
        peer_id: String,
        rel_path: String,
        size: usize,
        purpose: String,
        is_self: bool,
    },
    /// Progress update for an active transfer (0.0–1.0).
    FileProgress { transfer_id: String, progress: f64 },
    /// Transfer complete; `data` is the verified original file bytes.
    FileComplete {
        transfer_id: String,
        data: Vec<u8>,
        rel_path: String,
    },
    /// Transfer failed or was rejected.
    FileFailed { transfer_id: String, reason: String },
    /// Periodic transport statistics for a connected peer.
    /// `json` = `{peer_id, rtt_ms, packet_loss_pct, jitter_ms, relay, bandwidth_kbps}`.
    ConnectionStats { peer_id: String, json: String },
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Commands the app layer sends to the connection manager.
#[derive(Debug)]
pub enum ConnectionCommand {
    /// Send a signaling message to a peer via the best available path.
    SendMessage(SignalingMessage),
    /// Initiate a direct QUIC connection to a peer endpoint.
    ConnectDirect {
        peer_id: String,
        host: String,
        port: u16,
    },
    /// Request a relay slot from a connected supernode.
    RequestRelay {
        supernode_id: String,
    },
    /// Start listening for incoming QUIC connections.
    StartQuicServer {
        port: u16,
    },
    /// Join an SFU room for both voice and chat (sends `SfuJoin` signaling).
    JoinRoom {
        supernode_id: String,
        room_id: String,
    },
    /// Leave the current SFU room (sends `SfuLeave` signaling).
    LeaveRoom {
        supernode_id: String,
    },
    /// Subscribe to SFU room text chat only — no voice participation.
    /// Sends `SfuSubscribe`; the supernode will deliver `SfuChat` messages
    /// without adding this peer to the voice-participant list.
    SubscribeRoomChat {
        supernode_id: String,
        room_id: String,
    },
    /// Send an Opus audio frame to a specific peer over QUIC datagrams.
    SendAudioFrame {
        peer_id: String,
        opus_data: Vec<u8>,
    },
    /// Send a typing indicator to a peer.
    SendTyping {
        peer_id: String,
        is_typing: bool,
    },
    /// Send a text chat message to an SFU room via the supernode.
    SendSfuChat {
        supernode_id: String,
        room_id: String,
        body: String,
        sender_handle: String,
    },
    /// Send a file to every recipient subscribed to an SFU room's chat.
    SendSfuFile {
        supernode_id: String,
        room_id: String,
        rel_path: String,
        data: Vec<u8>,
        purpose: String,
    },
    /// Block a peer (prevent further inbound messages; update peer store).
    BlockPeer {
        peer_id: String,
    },
    UnblockPeer {
        peer_id: String,
    },
    /// Send our capability list to a peer after handshake.
    SendCapabilityAnnounce {
        peer_id: String,
    },
    /// Send a `CAPABILITY_INVOKE` to *peer_id* for *feature_id*.
    SendCapabilityInvoke {
        peer_id: String,
        feature_id: String,
        params: Value,
        /// Optional `"datagram"` / `"stream"` hint included in the payload.
        channel_hint: Option<String>,
    },
    /// Record a user-supplied trust decision for a bespoke `(feature, peer)`
    /// pair. Subsequent invokes consult this decision instead of emitting
    /// [`ConnectionEvent::CapabilityInvokePending`].
    SetFeatureTrust {
        peer_id: String,
        feature_id: String,
        allow: bool,
    },
    /// Replace the current room-member set used by the `room-member` auth
    /// tier check. Empty set = no room joined.
    SetRoomMembers {
        members: Vec<String>,
    },
    /// Request the SFU room list from a supernode.
    RequestRoomList {
        supernode_id: String,
    },
    /// Accept an incoming invite URL (`conquerd://<b64>`) and initiate the
    /// invite handshake with the inviter.
    AcceptInvite {
        invite_url: String,
    },
    /// Send a file to a peer.
    SendFile {
        peer_id: String,
        rel_path: String,
        data: Vec<u8>,
        purpose: String,
    },
    /// Accept an inbound file offer.
    AcceptFile {
        transfer_id: String,
    },
    /// Reject an inbound file offer.
    RejectFile {
        transfer_id: String,
    },
    /// Cancel an active transfer (inbound or outbound).
    CancelFile {
        transfer_id: String,
    },
    /// Create (and immediately join) a new SFU room on a supernode.
    CreateRoom {
        supernode_id: String,
        room_name: String,
    },
    /// Send an Opus audio frame to the current SFU room via the supernode.
    /// Used as a WebSocket fallback when direct QUIC is unavailable.
    SendRoomAudio {
        opus_data: Vec<u8>,
    },
    /// Fetch an in-app portal asset (`web.host.app.v1`) from a supernode
    /// over the cached QUIC relay connection. The reply is delivered on
    /// `reply_tx`; the error string is human-readable (logging hint), the
    /// scheme handler is expected to surface a generic failure to Chromium.
    FetchWebApp {
        supernode_id: String,
        path: String,
        query: Option<String>,
        reply_tx: tokio::sync::oneshot::Sender<std::result::Result<WebAppResponse, String>>,
    },
    /// Broadcast our avatar config to a specific trusted peer.
    BroadcastAvatarConfig {
        peer_id: String,
        config_json: String,
    },
    /// Broadcast our avatar config to every currently-connected peer.
    BroadcastAvatarConfigToAll {
        config_json: String,
    },
    /// Graceful shutdown.
    Shutdown,
}

// ---------------------------------------------------------------------------
// Peer connection tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerConnectionState {
    /// No active connection.
    Disconnected,
    /// QUIC or WebSocket handshake in progress.
    Connecting,
    /// Fully established session.
    Connected,
    /// Relay-assisted connection active.
    Relay,
}

#[derive(Debug)]
struct PeerConnection {
    peer_id: String,
    state: PeerConnectionState,
    /// QUIC signaling stream send side (when QUIC is connected).
    quic_sig_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// Supernode WebSocket sender for forwarding messages (WS fallback).
    ws_tx: Option<mpsc::Sender<WsMessage>>,
    connected_at: Option<Instant>,
}

impl PeerConnection {
    fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            state: PeerConnectionState::Disconnected,
            quic_sig_tx: None,
            ws_tx: None,
            connected_at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal QUIC events (task → connection manager)
// ---------------------------------------------------------------------------

/// Events sent from spawned QUIC tasks **and** supernode WebSocket tasks back
/// to the connection manager task over the shared internal channel.
#[derive(Debug)]
enum InternalEvent {
    // ── QUIC events ────────────────────────────────────────────────────────
    /// QUIC handshake completed. `sig_tx` sends raw bytes over the signaling stream.
    QuicConnected {
        peer_id: String,
        sig_tx: mpsc::Sender<Vec<u8>>,
    },
    /// QUIC connection lost.
    QuicDisconnected { peer_id: String },
    /// Periodic transport stats sampled from `quinn::Connection::stats()`.
    QuicStats {
        peer_id: String,
        rtt_ms: f64,
        packet_loss_pct: f64,
        jitter_ms: f64,
        bandwidth_kbps: f64,
    },
    /// Inbound signaling payload from the QUIC peer.
    QuicSignalingData { peer_id: String, data: Vec<u8> },
    // ── WebSocket / supernode events ───────────────────────────────────────
    /// Supernode WebSocket connected (after HELLO was sent).
    WsConnected { peer_id: String },
    /// Supernode WebSocket disconnected or errored.
    WsDisconnected { peer_id: String },
    /// Inbound signaling message received over a supernode WebSocket.
    WsSignalingMessage { msg: SignalingMessage },
    /// A background `QuicRelayClient::connect` attempt finished.
    /// `client` is `None` on failure (logged inline).
    RelayClientReady {
        supernode_id: String,
        client: Option<Arc<QuicRelayClient>>,
    },
}

// ---------------------------------------------------------------------------
// SupernodeSession
// ---------------------------------------------------------------------------

struct SupernodeSession {
    peer_id: String,
    ws_url: String,
    /// Channel to send messages into this supernode connection.
    send_tx: mpsc::Sender<WsMessage>,
    connected: bool,
}

/// Extract the bare host (no scheme, port, or path) from a URL-ish string
/// such as a supernode signaling `ws_url` (`ws://host:port/...`,
/// `wss://host:port`, or even a bare `host:port`).  Returns `None` when no
/// host can be determined.
fn host_from_url(url: &str) -> Option<String> {
    // Strip scheme (`ws://`, `wss://`, `https://`, ...).
    let after_scheme = url.split("://").last().unwrap_or(url);
    // Authority ends at the first `/`, `?`, or `#`.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    if authority.is_empty() {
        return None;
    }
    // Strip credentials (`user@host`).
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    // IPv6 literal: `[::1]:port`.
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        // Strip `:port` for hostnames / IPv4.
        host_port.split(':').next().unwrap_or(host_port)
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

/// Returns `true` when `host` is a loopback or wildcard address that is
/// only meaningful to the machine that emitted it.
fn is_loopback_or_wildcard(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "::")
}

/// When a supernode advertises a WebTransport URL whose host is a loopback
/// or wildcard address (because it was started without `supernode_host`),
/// rewrite that host with the real address we used to reach the supernode
/// (its signaling `ws_url`), preserving the advertised scheme/port/path.
/// Returns `None` when no rewrite is needed or possible.
fn rewrite_loopback_wt_url(wt_url: &str, signaling_url: &str) -> Option<String> {
    let wt_host = host_from_url(wt_url)?;
    if !is_loopback_or_wildcard(&wt_host) {
        return None;
    }
    let real_host = host_from_url(signaling_url)?;
    if is_loopback_or_wildcard(&real_host) || real_host == wt_host {
        return None;
    }
    // Replace only the first occurrence of the host token to avoid touching
    // an identical substring elsewhere in the URL (e.g. a query parameter).
    Some(wt_url.replacen(&wt_host, &real_host, 1))
}

// ---------------------------------------------------------------------------
// PendingInvite
// ---------------------------------------------------------------------------

/// State for an in-flight invite acceptance (joiner side).
/// Keyed by `invite_id` and removed when `INVITE_HANDSHAKE_ACCEPT` arrives.
/// Entries that never receive a response are pruned after `INVITE_TTL`
/// to prevent the map from growing without bound if the inviter never
/// completes the handshake (abandoned invite, network partition, etc.).
struct PendingInvite {
    inviter_peer_id: String,
    inviter_identity_pub: String,
    invite_id: String,
    relay_hint: String,
    /// Wall-clock moment the invite was queued; used for TTL expiry.
    created_at: Instant,
}

/// How long to keep an unanswered outbound invite in `pending_invites`.
const INVITE_TTL: Duration = Duration::from_secs(5 * 60); // 5 minutes

// ---------------------------------------------------------------------------
// ConnectionManager
// ---------------------------------------------------------------------------

/// The central connection manager.
///
/// Call [`ConnectionManager::run`] in a tokio task to drive all I/O. The
/// application layer communicates via the returned channels.
pub struct ConnectionManager {
    identity: Arc<Identity>,
    peer_store: Arc<RwLock<PeerStore>>,

    event_tx: mpsc::Sender<ConnectionEvent>,
    cmd_rx: mpsc::Receiver<ConnectionCommand>,

    peers: HashMap<String, PeerConnection>,
    supernodes: HashMap<String, SupernodeSession>,

    /// Quinn QUIC endpoint (lazily created on first use).
    quic_endpoint: Option<quinn::Endpoint>,
    /// Internal event channel (QUIC tasks + WS tasks → this manager task).
    internal_tx: mpsc::Sender<InternalEvent>,
    internal_rx: mpsc::Receiver<InternalEvent>,
    /// Pending invite initiations: invite_id → invite data awaiting
    /// `INVITE_HANDSHAKE_ACCEPT` from the other party.
    pending_invites: HashMap<String, PendingInvite>,
    /// File-transfer state machine.
    file_mgr: FileTransferManager,
    room_file_mgr: FileTransferManager,
    /// In-process registry of capabilities this client advertises and the
    /// modules bound to them. Seeded from `register_client_modules`.
    feature_registry: Arc<FeatureRegistry>,
    /// Capabilities each remote peer has announced. Used for the
    /// intersection check in the `CAPABILITY_INVOKE` gate.
    peer_capabilities: HashMap<String, Vec<CapabilityDescriptor>>,
    /// Members of the current SFU room, used by the `room-member` auth tier
    /// gate. Updated via [`ConnectionCommand::SetRoomMembers`].
    room_members: HashSet<String>,
    /// Per-(feature, peer) consent decisions for non-first-party invokes.
    feature_trust: FeatureTrustStore,
    /// Current SFU room identifier (empty when not in a room).
    current_room_id: String,
    /// Supernode we joined the current room on (empty when not in a room).
    current_supernode_id: String,
    /// Live QUIC relay connections keyed by supernode identity pubkey.
    /// Populated lazily once a `RelayGranted` event arrives and the
    /// background connect succeeds. Used by [`ConnectionCommand::FetchWebApp`]
    /// to open `web.host.app.v1` streams.
    quic_relays: HashMap<String, Arc<QuicRelayClient>>,
    /// Sliding-window replay guard for inbound signaling. Complements the
    /// timestamp freshness window by rejecting re-delivery of an already-seen
    /// signed message *within* that window.
    replay_guard: ReplayGuard,
    /// Latest QUIC transport stats keyed by peer id.
    transport_stats: HashMap<String, PeerTransportStats>,
}

#[derive(Debug, Clone, Default)]
struct PeerTransportStats {
    rtt_ms: f64,
    packet_loss_pct: f64,
    jitter_ms: f64,
    bandwidth_kbps: f64,
}

impl ConnectionManager {
    /// Create a manager and split it into channels + a runnable future.
    ///
    /// Returns `(cmd_tx, event_rx, task_future)`. Call `tokio::spawn(task_future)`.
    pub fn split(
        identity: Arc<Identity>,
        peer_store: Arc<RwLock<PeerStore>>,
    ) -> (
        mpsc::Sender<ConnectionCommand>,
        mpsc::Receiver<ConnectionEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        // Build the feature registry and bind the three first-party
        // client modules. `register_client_modules` registers them as
        // advertisement-only — message hooks are wired separately by
        // higher-level managers (chat, file). Failures here are
        // unrecoverable configuration bugs.
        let feature_registry = Arc::new(FeatureRegistry::new());
        if let Err(e) = register_client_modules(&feature_registry) {
            error!("failed to seed feature registry: {e}");
        }
        let (cmd_tx, event_rx, fut) =
            Self::split_with_registry(identity, peer_store, Arc::clone(&feature_registry));
        // Drop the registry handle here — the manager owns its own clone.
        drop(feature_registry);
        (cmd_tx, event_rx, fut)
    }

    /// Like [`Self::split`] but reuses an externally constructed feature
    /// registry so callers (e.g. the Qt bridge) can register additional
    /// plugin descriptors after construction. The registry MUST already
    /// have the first-party `core.*` modules registered (call
    /// [`register_client_modules`] before passing it in).
    pub fn split_with_registry(
        identity: Arc<Identity>,
        peer_store: Arc<RwLock<PeerStore>>,
        feature_registry: Arc<FeatureRegistry>,
    ) -> (
        mpsc::Sender<ConnectionCommand>,
        mpsc::Receiver<ConnectionEvent>,
        impl std::future::Future<Output = ()> + Send,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<ConnectionEvent>(256);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ConnectionCommand>(64);
        let (internal_tx, internal_rx) = mpsc::channel::<InternalEvent>(128);

        let mgr = Self {
            identity,
            peer_store,
            event_tx,
            cmd_rx,
            peers: HashMap::new(),
            supernodes: HashMap::new(),
            quic_endpoint: None,
            internal_tx,
            internal_rx,
            pending_invites: HashMap::new(),
            file_mgr: FileTransferManager::new(),
            room_file_mgr: FileTransferManager::new(),
            feature_registry,
            peer_capabilities: HashMap::new(),
            room_members: HashSet::new(),
            feature_trust: FeatureTrustStore::new(),
            current_room_id: String::new(),
            current_supernode_id: String::new(),
            quic_relays: HashMap::new(),
            replay_guard: ReplayGuard::new(Self::MAX_MESSAGE_AGE_SECS),
            transport_stats: HashMap::new(),
        };
        (cmd_tx, event_rx, mgr.run_inner())
    }

    /// Lazily create the QUIC endpoint on the given port (0 = ephemeral).
    fn ensure_quic_endpoint(&mut self, port: u16) -> bool {
        if self.quic_endpoint.is_some() {
            return true;
        }
        match quic_tls::make_quic_endpoint(self.identity.signing_key(), port) {
            Ok(ep) => {
                info!(
                    "QUIC endpoint bound on {}",
                    ep.local_addr().map(|a| a.to_string()).unwrap_or_default()
                );
                self.quic_endpoint = Some(ep);
                true
            }
            Err(e) => {
                error!("Failed to create QUIC endpoint: {e}");
                false
            }
        }
    }

    // -- internal event loop -------------------------------------------------

    async fn run_inner(mut self) {
        info!("ConnectionManager started");

        // Connect to known supernodes from peer store.
        // Key by identity_pub (base64url Ed25519 pubkey) so that outbound
        // signaling messages addressed to the supernode (target=identity_pub)
        // match the supernode's own `our_id` check and are handled directly
        // rather than relayed to a non-existent peer.
        let supernode_hints: Vec<(String, Vec<String>)> = {
            let store = self.peer_store.read();
            store
                .supernodes()
                .iter()
                .map(|r| (r.identity_pub.clone(), r.relay_hints.clone()))
                .collect()
        };
        for (identity_pub, hints) in supernode_hints {
            // Try the first hint only for now.
            if let Some(hint) = hints.into_iter().next() {
                self.connect_supernode_ws(identity_pub.clone(), hint).await;
            }
        }

        // Main event loop
        let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_S));
        let mut stats_interval = tokio::time::interval(Duration::from_secs(2));

        loop {
            tokio::select! {
                // App-layer commands
                Some(cmd) = self.cmd_rx.recv() => {
                    match cmd {
                        ConnectionCommand::Shutdown => {
                            info!("ConnectionManager shutting down");
                            if let Some(ep) = &self.quic_endpoint {
                                ep.close(0u32.into(), b"shutdown");
                            }
                            break;
                        }
                        ConnectionCommand::SendMessage(msg) => {
                            self.dispatch_outbound(msg).await;
                        }
                        ConnectionCommand::ConnectDirect { peer_id, host, port } => {
                            self.connect_direct_quic(&peer_id, &host, port).await;
                        }
                        ConnectionCommand::RequestRelay { supernode_id } => {
                            self.request_relay(&supernode_id).await;
                        }
                        ConnectionCommand::StartQuicServer { port } => {
                            self.ensure_quic_endpoint(port);
                            info!("QUIC server listening on port {port}");
                        }
                        ConnectionCommand::JoinRoom { supernode_id, room_id } => {
                            self.current_supernode_id = supernode_id.clone();
                            self.current_room_id = room_id.clone();
                            self.send_room_join(&supernode_id, &room_id).await;
                        }
                        ConnectionCommand::LeaveRoom { supernode_id } => {
                            self.current_room_id.clear();
                            self.current_supernode_id.clear();
                            self.send_room_leave(&supernode_id).await;
                        }
                        ConnectionCommand::SubscribeRoomChat { supernode_id, room_id } => {
                            self.send_room_subscribe(&supernode_id, &room_id).await;
                        }
                        ConnectionCommand::SendAudioFrame { peer_id, opus_data } => {
                            self.send_audio_datagram(&peer_id, opus_data).await;
                        }
                        ConnectionCommand::SendRoomAudio { opus_data } => {
                            self.send_room_audio(opus_data).await;
                        }
                        ConnectionCommand::SendTyping { peer_id, is_typing } => {
                            self.send_typing(&peer_id, is_typing).await;
                        }
                        ConnectionCommand::SendSfuChat { supernode_id, room_id, body, sender_handle } => {
                            self.send_sfu_chat(&supernode_id, &room_id, &body, &sender_handle).await;
                        }
                        ConnectionCommand::SendSfuFile { supernode_id, room_id, rel_path, data, purpose } => {
                            let size = data.len();
                            let old = self.room_file_mgr.get_old_data(&rel_path);
                            let old_ref: Option<&[u8]> = old.as_deref();
                            match self.room_file_mgr.offer_file(&room_id, &rel_path, data, &purpose, old_ref, true) {
                                Ok((transfer_id, evs)) => {
                                    let _ = self.event_tx.try_send(ConnectionEvent::FileOffered {
                                        transfer_id,
                                        peer_id: room_id.clone(),
                                        rel_path,
                                        size,
                                        purpose,
                                        is_self: true,
                                    });
                                    self.dispatch_room_transfer_events(evs, &supernode_id, &room_id).await;
                                }
                                Err(e) => warn!("SendSfuFile error: {e}"),
                            }
                        }
                        ConnectionCommand::BlockPeer { peer_id } => {
                            let mut store = self.peer_store.write();
                            if let Some(rec) = store.get_mut(&peer_id) {
                                rec.blocked = true;
                            }
                            let _ = store.save();
                            info!("Peer {} blocked", &peer_id[..8.min(peer_id.len())]);
                        }
                        ConnectionCommand::UnblockPeer { peer_id } => {
                            let mut store = self.peer_store.write();
                            if let Some(rec) = store.get_mut(&peer_id) {
                                rec.blocked = false;
                            }
                            let _ = store.save();
                            info!("Peer {} unblocked", &peer_id[..8.min(peer_id.len())]);
                        }
                        ConnectionCommand::SendCapabilityAnnounce { peer_id } => {
                            self.send_capability_announce(&peer_id).await;
                        }
                        ConnectionCommand::SendCapabilityInvoke { peer_id, feature_id, params, channel_hint } => {
                            self.send_capability_invoke(&peer_id, &feature_id, params, channel_hint).await;
                        }
                        ConnectionCommand::SetFeatureTrust { peer_id, feature_id, allow } => {
                            self.feature_trust.set(&feature_id, &peer_id, allow);
                            debug!(
                                "[feature_trust] decision recorded: feature='{}' peer={} allow={}",
                                feature_id,
                                &peer_id[..8.min(peer_id.len())],
                                allow
                            );
                        }
                        ConnectionCommand::SetRoomMembers { members } => {
                            self.room_members = members.into_iter().collect();
                            debug!("[capabilities] room member set updated ({} members)", self.room_members.len());
                        }
                        ConnectionCommand::RequestRoomList { supernode_id } => {
                            self.send_room_list_request(&supernode_id).await;
                        }
                        ConnectionCommand::AcceptInvite { invite_url } => {
                            self.handle_accept_invite(invite_url).await;
                        }
                        ConnectionCommand::SendFile { peer_id, rel_path, data, purpose } => {
                            let old = self.file_mgr.get_old_data(&rel_path);
                            let old_ref: Option<&[u8]> = old.as_deref();
                            match self.file_mgr.offer_file(&peer_id, &rel_path, data, &purpose, old_ref, false) {
                                Ok((_, evs)) => self.dispatch_transfer_events(evs).await,
                                Err(e) => warn!("SendFile error: {e}"),
                            }
                        }
                        ConnectionCommand::AcceptFile { transfer_id } => {
                            let evs = self.file_mgr.accept_transfer(&transfer_id);
                            self.dispatch_transfer_events(evs).await;
                        }
                        ConnectionCommand::RejectFile { transfer_id } => {
                            let evs = self.file_mgr.reject_transfer(&transfer_id, "user_rejected");
                            self.dispatch_transfer_events(evs).await;
                        }
                        ConnectionCommand::CancelFile { transfer_id } => {
                            let evs = self.file_mgr.cancel_transfer(&transfer_id);
                            self.dispatch_transfer_events(evs).await;
                        }
                        ConnectionCommand::FetchWebApp { supernode_id, path, query, reply_tx } => {
                            self.handle_fetch_web_app(supernode_id, path, query, reply_tx).await;
                        }
                        ConnectionCommand::BroadcastAvatarConfig { peer_id, config_json } => {
                            self.send_avatar_config(&peer_id, &config_json).await;
                        }
                        ConnectionCommand::BroadcastAvatarConfigToAll { config_json } => {
                            let connected: Vec<String> = self.peers.iter()
                                .filter(|(_, p)| p.state == PeerConnectionState::Connected)
                                .map(|(id, _)| id.clone())
                                .collect();
                            for peer_id in connected {
                                self.send_avatar_config(&peer_id, &config_json).await;
                            }
                        }
                        ConnectionCommand::CreateRoom { supernode_id, room_name } => {
                            // Generate a room ID from the name (slug + random suffix).
                            use std::time::{SystemTime, UNIX_EPOCH};
                            let ts = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let slug: String = room_name
                                .chars()
                                .filter(|c| c.is_alphanumeric() || *c == '-')
                                .map(|c| c.to_ascii_lowercase())
                                .collect();
                            let room_id = format!("{slug}-{ts}");
                            info!("[cm] CreateRoom: supernode={supernode_id} room_id={room_id}");
                            self.send_room_join(&supernode_id, &room_id).await;
                        }
                    }
                }
                // Internal events from QUIC and WS tasks
                Some(ev) = self.internal_rx.recv() => {
                    self.handle_internal_event(ev).await;
                }
                // Accept incoming QUIC connections
                incoming = async {
                    match &mut self.quic_endpoint {
                        Some(ep) => ep.accept().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(inc) = incoming {
                        self.handle_incoming_quic(inc).await;
                    }
                }
                _ = ping_interval.tick() => {
                    self.send_pings().await;
                }
                _ = stats_interval.tick() => {
                    self.emit_connection_stats();
                }
            }
        }
        info!("ConnectionManager stopped");
    }

    fn emit_connection_stats(&self) {
        for (peer_id, peer) in &self.peers {
            if peer.state != PeerConnectionState::Connected {
                continue;
            }
            let Some(stats) = self.transport_stats.get(peer_id) else {
                continue;
            };
            let relay = peer.state == PeerConnectionState::Relay;
            let payload = serde_json::json!({
                "peer_id": peer_id,
                "rtt_ms": stats.rtt_ms,
                "packet_loss_pct": stats.packet_loss_pct,
                "jitter_ms": stats.jitter_ms,
                "relay": relay,
                "bandwidth_kbps": stats.bandwidth_kbps,
            });
            let _ = self.event_tx.try_send(ConnectionEvent::ConnectionStats {
                peer_id: peer_id.clone(),
                json: payload.to_string(),
            });
        }
    }

    // -- supernode WebSocket -------------------------------------------------

    async fn connect_supernode_ws(&mut self, peer_id: String, ws_url: String) {
        let identity = Arc::clone(&self.identity);
        let internal_tx = self.internal_tx.clone();
        let (send_tx, send_rx) = mpsc::channel::<WsMessage>(64);
        let peer_id_clone = peer_id.clone();
        let ws_url_clone = ws_url.clone();

        // Spawn a dedicated task for this supernode connection
        tokio::spawn(supernode_ws_task(
            identity,
            peer_id_clone,
            ws_url_clone,
            send_rx,
            internal_tx,
        ));

        self.supernodes.insert(
            peer_id.clone(),
            SupernodeSession {
                peer_id,
                ws_url,
                send_tx,
                connected: false,
            },
        );
    }

    // -- outbound message routing --------------------------------------------

    /// Fixed first-party channel tag for a message type, if it rides a
    /// dedicated channel rather than the control/signaling channel.
    ///
    /// Chat (`core.chat.v1` → `CHAT_TAG`) and file transfer
    /// (`core.file.v1` → `FILE_TAG`) are multiplexed onto their own tags on
    /// the QUIC peer stream; everything else stays on the untagged control
    /// channel.
    fn channel_tag_for(msg_type: MessageType) -> Option<u8> {
        match msg_type {
            MessageType::ChatMessage | MessageType::ChatAck | MessageType::ChatTyping => {
                Some(channel_frame::CHAT_TAG)
            }
            MessageType::FileTransferOffer
            | MessageType::FileTransferAccept
            | MessageType::FileTransferReject
            | MessageType::FileTransferChunk
            | MessageType::FileTransferComplete
            | MessageType::FileTransferAck
            | MessageType::FileTransferError => Some(channel_frame::FILE_TAG),
            _ => None,
        }
    }

    async fn dispatch_outbound(&mut self, mut msg: SignalingMessage) {
        let chat_attempt = if msg.msg_type == MessageType::ChatMessage {
            msg.target.clone().and_then(|peer_id| {
                msg.payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .map(|message_id| (peer_id, message_id.to_owned()))
            })
        } else {
            None
        };
        if let Some((peer_id, message_id)) = chat_attempt.clone() {
            let connected = self
                .peers
                .get(&peer_id)
                .map(|peer| peer.state == PeerConnectionState::Connected)
                .unwrap_or(false);
            if !connected {
                warn!(
                    "No connected peer session for chat message {} to {}",
                    message_id,
                    &peer_id[..8.min(peer_id.len())]
                );
                let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                    peer_id,
                    message_id,
                    reason: "peer is offline".to_owned(),
                });
                return;
            }
        }

        // Gate outbound chat and file messages through their feature quota
        // before signing/sending.  This is symmetric with the inbound quota
        // enforcement in QuotaRegistry::try_consume that applies to inbound
        // messages dispatched via FeatureRegistry::dispatch_message.
        if let Some(ref target) = msg.target.clone() {
            let feature_gate = match msg.msg_type {
                // core.chat.v1 covers text chat and related control messages.
                MessageType::ChatMessage | MessageType::ChatAck | MessageType::ChatTyping => {
                    Some("core.chat.v1")
                }
                // core.file.v1 covers the full file-transfer handshake.
                MessageType::FileTransferOffer
                | MessageType::FileTransferAccept
                | MessageType::FileTransferReject
                | MessageType::FileTransferChunk
                | MessageType::FileTransferComplete
                | MessageType::FileTransferAck
                | MessageType::FileTransferError => Some("core.file.v1"),
                MessageType::SfuFileOffer
                | MessageType::SfuFileChunk
                | MessageType::SfuFileComplete => Some("room.file.v1"),
                _ => None,
            };
            if let Some(fid) = feature_gate {
                // Estimate outbound byte cost from the payload values so we
                // don't have to re-serialize the whole message.  A floor of
                // 64 bytes ensures a non-trivially small message still
                // consumes tokens (prevents quota-bypass via empty messages).
                let byte_est: usize = msg
                    .payload
                    .values()
                    .filter_map(|v| v.as_str())
                    .map(str::len)
                    .sum::<usize>()
                    .max(64);
                if !self
                    .feature_registry
                    .gate_through_feature(fid, target, byte_est)
                {
                    warn!(
                        "[gate_through_feature] {} outbound quota exceeded for {}; dropping {:?}",
                        fid,
                        &target[..8.min(target.len())],
                        msg.msg_type,
                    );
                    if let Some((peer_id, message_id)) = chat_attempt {
                        let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                            peer_id,
                            message_id,
                            reason: "quota exceeded".to_owned(),
                        });
                    }
                    return;
                }
            }
        }

        // Sign the message
        let canonical = match msg.canonical_bytes() {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to canonicalize message for signing: {}", e);
                return;
            }
        };
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(&sig));

        let msg_type = msg.msg_type.clone();

        let json = match msg.to_json() {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize message: {}", e);
                if let Some((peer_id, message_id)) = chat_attempt {
                    let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                        peer_id,
                        message_id,
                        reason: "message serialization failed".to_owned(),
                    });
                }
                return;
            }
        };

        // Route: QUIC direct > relay WS > supernode WS fallback
        if let Some(target) = &msg.target.clone() {
            if let Some(peer) = self.peers.get(target) {
                if peer.state == PeerConnectionState::Connected {
                    if let Some(sig_tx) = &peer.quic_sig_tx {
                        // Chat and file ride dedicated channel tags on the
                        // QUIC peer stream instead of the pure (control)
                        // signaling channel. Control messages stay untagged
                        // (raw JSON) for backward compatibility — the inbound
                        // classifier treats a leading `{` as control.
                        let bytes = match Self::channel_tag_for(msg_type) {
                            Some(tag) => channel_frame::encode_frame(tag, json.as_bytes()),
                            None => json.as_bytes().to_vec(),
                        };
                        let _ = sig_tx.try_send(bytes);
                        return;
                    }
                }
            }
        }

        // Fall back: send via first connected supernode WebSocket
        for sn in self.supernodes.values() {
            if sn.connected {
                let _ = sn.send_tx.try_send(WsMessage::Text(json.clone()));
                return;
            }
        }
        warn!("No connected path to deliver message {:?}", msg_type);
        if let Some((peer_id, message_id)) = chat_attempt {
            let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                peer_id,
                message_id,
                reason: "peer is offline".to_owned(),
            });
        }
    }

    // -- QUIC direct connect ------------------------------------------------

    async fn connect_direct_quic(&mut self, peer_id: &str, host: &str, port: u16) {
        info!(
            "QUIC outbound to {}:{} (peer {})",
            host,
            port,
            &peer_id[..8.min(peer_id.len())]
        );

        // Ensure we have a QUIC endpoint (ephemeral port).
        if !self.ensure_quic_endpoint(0) {
            return;
        }
        let endpoint = self.quic_endpoint.as_ref().unwrap().clone();

        let addr: SocketAddr = match format!("{host}:{port}").parse() {
            Ok(a) => a,
            Err(e) => {
                error!("Invalid peer address {host}:{port}: {e}");
                return;
            }
        };

        let peer_id_owned = peer_id.to_owned();
        let internal_tx = self.internal_tx.clone();

        tokio::spawn(async move {
            // Quinn server_name is arbitrary for self-signed certs; we verify by peer_id.
            let connecting = match endpoint.connect(addr, "conquerd") {
                Ok(c) => c,
                Err(e) => {
                    warn!("QUIC connect init to {addr} failed: {e}");
                    let _ = internal_tx
                        .send(InternalEvent::QuicDisconnected {
                            peer_id: peer_id_owned,
                        })
                        .await;
                    return;
                }
            };

            let connection = match connecting.await {
                Ok(c) => c,
                Err(e) => {
                    warn!("QUIC handshake with {addr} failed: {e}");
                    let _ = internal_tx
                        .send(InternalEvent::QuicDisconnected {
                            peer_id: peer_id_owned,
                        })
                        .await;
                    return;
                }
            };

            info!(
                "QUIC connected to {addr} (peer {})",
                &peer_id_owned[..8.min(peer_id_owned.len())]
            );
            run_quic_peer_session(connection, peer_id_owned, internal_tx).await;
        });

        // Register as connecting
        self.peers
            .entry(peer_id.to_owned())
            .or_insert_with(|| PeerConnection::new(peer_id))
            .state = PeerConnectionState::Connecting;
    }

    // -- Inbound QUIC connections -------------------------------------------

    async fn handle_incoming_quic(&mut self, incoming: quinn::Incoming) {
        let internal_tx = self.internal_tx.clone();

        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    warn!("Inbound QUIC handshake failed: {e}");
                    return;
                }
            };
            // Derive peer_id from the peer's certificate CN.
            let peer_id = connection
                .peer_identity()
                .and_then(|id| id.downcast::<Vec<rustls::pki_types::CertificateDer>>().ok())
                .and_then(|certs| certs.first().cloned())
                .and_then(|cert| quic_tls::cn_from_cert_der(&cert))
                .and_then(|cn| {
                    // CN = hex(pub_bytes), peer_id = hex(sha256(pub_bytes))
                    let pub_bytes = hex::decode(&cn).ok()?;
                    Some(quic_tls::peer_id_from_pub_bytes(&pub_bytes))
                })
                .unwrap_or_else(|| {
                    // Fallback: use remote address as identifier
                    connection.remote_address().to_string()
                });

            info!(
                "Inbound QUIC from {} (peer {})",
                connection.remote_address(),
                &peer_id[..8.min(peer_id.len())]
            );
            run_quic_peer_session(connection, peer_id, internal_tx).await;
        });
    }

    // -- Internal event handler (QUIC tasks + WS tasks) --------------------

    async fn handle_internal_event(&mut self, event: InternalEvent) {
        match event {
            // ── QUIC events ──────────────────────────────────────────────────────────────
            InternalEvent::QuicConnected { peer_id, sig_tx } => {
                let entry = self
                    .peers
                    .entry(peer_id.clone())
                    .or_insert_with(|| PeerConnection::new(&peer_id));
                entry.state = PeerConnectionState::Connected;
                entry.quic_sig_tx = Some(sig_tx);
                entry.connected_at = Some(Instant::now());
                info!("Peer {} QUIC connected", &peer_id[..8.min(peer_id.len())]);
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::PeerConnected(peer_id.clone()));
                // Send capability announce to the newly-connected peer.
                self.send_capability_announce(&peer_id).await;
                // Also send build attestation so the peer knows our reproducible build ID.
                self.send_build_attestation(&peer_id).await;
            }
            InternalEvent::QuicStats {
                peer_id,
                rtt_ms,
                packet_loss_pct,
                jitter_ms,
                bandwidth_kbps,
            } => {
                self.transport_stats.insert(
                    peer_id,
                    PeerTransportStats {
                        rtt_ms,
                        packet_loss_pct,
                        jitter_ms,
                        bandwidth_kbps,
                    },
                );
            }
            InternalEvent::QuicDisconnected { peer_id } => {
                self.transport_stats.remove(&peer_id);
                if let Some(conn) = self.peers.get_mut(&peer_id) {
                    conn.state = PeerConnectionState::Disconnected;
                    conn.quic_sig_tx = None;
                }
                // Release inbound and outbound quota state so the next
                // connection starts with fresh token buckets.
                self.feature_registry.clear_peer_quotas(&peer_id);
                self.feature_registry.clear_peer_outbound_quotas(&peer_id);
                // Release replay-window state for this peer.
                self.replay_guard.forget_peer(&peer_id);
                // Remove stale capability advertisement so a reconnecting
                // peer is forced to re-announce before invoking features.
                // Without this, entries accumulate for every connect/disconnect
                // cycle and the intersection check could honour capabilities
                // from a stale session.
                self.peer_capabilities.remove(&peer_id);
                info!(
                    "Peer {} QUIC disconnected",
                    &peer_id[..8.min(peer_id.len())]
                );
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::PeerDisconnected(peer_id));
            }
            InternalEvent::QuicSignalingData { peer_id, data } => {
                // The QUIC peer stream multiplexes several channels via a
                // 1-byte leading tag. `classify` accepts both the tagged
                // framing and legacy untagged JSON (leading `{`) so a peer
                // that predates tagging still interoperates on control.
                match channel_frame::classify(&data) {
                    // Direct peer audio: `[AUDIO_TAG][id_len][peer_id][opus]`.
                    Some(FrameClass::Audio(rest)) if rest.len() > 1 => {
                        let id_len = rest[0] as usize;
                        if rest.len() > 1 + id_len {
                            let opus_data = rest[1 + id_len..].to_vec();
                            // Use the session-level peer_id (verified from
                            // the handshake) rather than the embedded id.
                            if !self.check_inbound_feature_quota(
                                "core.audio.opus",
                                &peer_id,
                                opus_data.len(),
                            ) {
                                debug!(
                                    "[core.audio.opus] inbound quota exceeded for {}; dropping frame",
                                    &peer_id[..8.min(peer_id.len())]
                                );
                            } else {
                                let _ =
                                    self.event_tx
                                        .try_send(ConnectionEvent::DirectAudioReceived {
                                            peer_id,
                                            opus_data,
                                        });
                            }
                        }
                    }
                    // Chat / file / control all carry signed JSON; route
                    // through the common inbound path (signature + replay +
                    // freshness checks, then feature dispatch). The channel
                    // tag selects the transport lane, not the validation.
                    Some(
                        FrameClass::Chat(body)
                        | FrameClass::File(body)
                        | FrameClass::Control(body)
                        | FrameClass::UntaggedControl(body),
                    ) => {
                        if let Ok(text) = std::str::from_utf8(body) {
                            if let Ok(msg) = SignalingMessage::from_json(text) {
                                self.handle_inbound(msg).await;
                            } else {
                                debug!("Non-JSON QUIC signaling data from {peer_id}");
                            }
                        }
                    }
                    Some(FrameClass::Other(tag, _)) => {
                        debug!("Unhandled QUIC channel tag 0x{tag:02X} from {peer_id}");
                    }
                    // Empty frame, or an audio frame too short to carry a
                    // peer id + payload — nothing to dispatch.
                    _ => {}
                }
            }
            // ── WebSocket events ──────────────────────────────────────────────────────
            InternalEvent::WsConnected { peer_id } => {
                if let Some(sn) = self.supernodes.get_mut(&peer_id) {
                    sn.connected = true;
                }
                info!(
                    "Supernode {} WebSocket connected",
                    &peer_id[..8.min(peer_id.len())]
                );
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::SupernodeConnected(peer_id.clone()));
                // Auto-request the SFU room list so the Rooms tab populates.
                self.send_room_list_request(&peer_id).await;
                // Request supernode info (portal URL, title) for the Nodes tab.
                self.send_supernode_info_request(&peer_id).await;
                // Tell the supernode our build attestation (reproducible build ID).
                self.send_build_attestation(&peer_id).await;
            }
            InternalEvent::WsDisconnected { peer_id } => {
                if let Some(sn) = self.supernodes.get_mut(&peer_id) {
                    sn.connected = false;
                }
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::SupernodeDisconnected(peer_id));
            }
            InternalEvent::WsSignalingMessage { msg } => {
                self.handle_inbound(msg).await;
            }
            InternalEvent::RelayClientReady {
                supernode_id,
                client,
            } => {
                match client {
                    Some(c) => {
                        info!(
                            "[relay] QUIC relay ready for supernode {}",
                            &supernode_id[..12.min(supernode_id.len())]
                        );
                        self.quic_relays.insert(supernode_id, c);
                    }
                    None => {
                        // Connect failure already logged by the spawned task;
                        // make sure no stale entry survives a reconnect.
                        self.quic_relays.remove(&supernode_id);
                    }
                }
            }
        }
    }

    // -- web.host.app.v1 client (in-app portal fetch) ------------------------

    /// Kick off a background `QuicRelayClient::connect` for `supernode_id`.
    /// On success the resulting handle is delivered via
    /// [`InternalEvent::RelayClientReady`] and cached in `self.quic_relays`.
    ///
    /// Called from the `RelayGranted` inbound handler — by the time we get
    /// here the supernode has already added our peer_id to its `allowed`
    /// set, so a plain mTLS handshake using our existing client cert is
    /// all that's required.
    fn spawn_relay_client_connect(
        &mut self,
        supernode_id: String,
        relay_host: String,
        relay_port: u16,
    ) {
        if relay_host.is_empty() || relay_port == 0 {
            warn!(
                "[relay] skipping connect for {}: empty host/port",
                &supernode_id[..12.min(supernode_id.len())]
            );
            return;
        }
        if self.quic_relays.contains_key(&supernode_id) {
            // Reuse the existing live connection.
            return;
        }
        if !self.ensure_quic_endpoint(0) {
            error!("[relay] no QUIC endpoint — cannot connect to supernode relay");
            return;
        }
        let endpoint = self
            .quic_endpoint
            .as_ref()
            .expect("ensure_quic_endpoint returned true")
            .clone();
        let internal_tx = self.internal_tx.clone();
        let sn_id_for_task = supernode_id.clone();
        tokio::spawn(async move {
            let client = match QuicRelayClient::connect(
                &endpoint,
                sn_id_for_task.clone(),
                &relay_host,
                relay_port,
            )
            .await
            {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    error!(
                        "[relay] connect to {}:{} failed: {e:#}",
                        relay_host, relay_port
                    );
                    None
                }
            };
            let _ = internal_tx
                .send(InternalEvent::RelayClientReady {
                    supernode_id: sn_id_for_task,
                    client,
                })
                .await;
        });
    }

    /// Service a `FetchWebApp` command by opening a fresh QUIC bidi stream
    /// against the cached supernode relay and walking the `web.host.app.v1`
    /// wire protocol via [`web_app_client::fetch`].
    async fn handle_fetch_web_app(
        &mut self,
        supernode_id: String,
        path: String,
        query: Option<String>,
        reply_tx: tokio::sync::oneshot::Sender<std::result::Result<WebAppResponse, String>>,
    ) {
        let Some(relay) = self.quic_relays.get(&supernode_id).cloned() else {
            let _ = reply_tx.send(Err(format!(
                "no QUIC relay connection for supernode {}",
                &supernode_id[..12.min(supernode_id.len())]
            )));
            return;
        };
        if !relay.is_alive() {
            self.quic_relays.remove(&supernode_id);
            let _ = reply_tx.send(Err("relay connection closed".to_owned()));
            return;
        }
        // Run the fetch in its own task so a slow / hung supernode can't
        // block the manager's event loop. The relay handle is an `Arc` so
        // the spawned task keeps it alive even if the manager drops it.
        tokio::spawn(async move {
            let result = web_app_client::fetch(relay.connection(), &path, query.as_deref())
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = reply_tx.send(result);
        });
    }

    // -- relay request -------------------------------------------------------

    async fn request_relay(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::RelayRequest, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("requester".to_owned(), Value::String(sender));
        self.dispatch_outbound(msg).await;
    }

    // -- room join / leave ---------------------------------------------------

    async fn send_room_join(&mut self, supernode_id: &str, room_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuJoin, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload
            .insert("peer_id".to_owned(), Value::String(sender));
        self.dispatch_outbound(msg).await;
    }

    async fn send_room_leave(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuLeave, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("peer_id".to_owned(), Value::String(sender));
        self.dispatch_outbound(msg).await;
    }

    async fn send_room_subscribe(&mut self, supernode_id: &str, room_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuSubscribe, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        self.dispatch_outbound(msg).await;
    }

    // -- audio datagram ------------------------------------------------------

    /// Send a raw Opus frame to a peer via QUIC datagram.
    ///
    /// Send a real-time Opus audio frame to a directly-connected peer.
    ///
    /// ## Audio Dispatch Decision (P2 #9 - Option A)
    ///
    /// `core.audio.opus` deliberately uses a **dedicated low-tag datagram path**
    /// (AUDIO_CHANNEL_TAG in the reserved 0x01–0x0F range) instead of going
    /// through the generic feature datagram multiplexer.
    ///
    /// See the long rationale comment below for why we chose the permanent
    /// documented bypass (Option A) over routing through a normal FeatureModule.
    ///
    /// Frame format: `[AUDIO_CHANNEL_TAG, peer_id_len(1 byte), ...peer_id_utf8, ...opus_data]`.
    /// Private helper that **must** be called before every audio frame send.
    /// This makes the "always gate audio" invariant obvious and hard to violate.
    #[inline]
    fn check_audio_quota(&self, target: &str, byte_count: usize) -> bool {
        self.feature_registry
            .gate_through_feature("core.audio.opus", target, byte_count)
    }

    #[inline]
    fn check_room_audio_outbound_quota(&self, target: &str, byte_count: usize) -> bool {
        self.feature_registry
            .gate_through_feature("room.audio.sfu", target, byte_count)
    }

    #[inline]
    fn check_inbound_feature_quota(
        &self,
        feature_id: &str,
        sender: &str,
        byte_count: usize,
    ) -> bool {
        self.feature_registry
            .gate_inbound_through_feature(feature_id, sender, byte_count)
    }

    /// Send a real-time Opus audio frame to a directly-connected peer.
    ///
    /// ## Audio Dispatch Decision (P2 #9 - Option A)
    ///
    /// `core.audio.opus` deliberately uses a **dedicated low-tag datagram path**
    /// (AUDIO_CHANNEL_TAG in the reserved 0x01–0x0F range) instead of going
    /// through the generic feature datagram multiplexer (`transport.quic.feature_datagram.v1`
    /// + dynamic 0x10–0xEF tags) and a full `FeatureModule` implementation.
    ///
    /// Reasons (documented trade-off):
    /// - Real-time audio has extremely strict latency/jitter requirements
    ///   (target < 20-40 ms one-way for natural conversation).
    /// - The generic path (tag allocation, registry dispatch, module trait call)
    ///   adds unacceptable overhead and potential contention.
    /// - agents.md explicitly requires keeping "DSP and per-tick feature loops
    ///   within real-time budget" while also saying "don't shortcut around the
    ///   capability layer for convenience."
    ///
    /// What **is** honored (mandatory per guardrails):
    /// - Capability advertisement & negotiation (via `CoreAudioOpusModule`)
    /// - Per-feature outbound quota via `check_audio_quota` (see above)
    /// - Symmetric inbound quota enforcement on the receiver
    ///
    /// This is a **narrow, justified, permanent pragmatic bypass** for the
    /// real-time audio hot path only. Non-real-time audio use cases should
    /// use the normal feature path.
    ///
    /// If the peer has no QUIC connection the frame is silently dropped (UDP
    /// semantics — losing a frame is acceptable for real-time audio).
    async fn send_audio_datagram(&self, peer_id: &str, opus_data: Vec<u8>) {
        let conn = match self.peers.get(peer_id) {
            Some(c) => c,
            None => return,
        };
        // Outbound quota gate (via dedicated helper to make the invariant obvious).
        if !self.check_audio_quota(peer_id, opus_data.len()) {
            debug!(
                "[core.audio.opus] outbound quota exceeded for {}; dropping frame",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        }
        // Route via QUIC signaling tx when available (reuses the QUIC connection
        // but sends as a datagram channel-tagged byte payload).
        if let Some(ref qtx) = conn.quic_sig_tx {
            let id_bytes = peer_id.as_bytes();
            let mut frame = Vec::with_capacity(2 + id_bytes.len() + opus_data.len());
            frame.push(AUDIO_CHANNEL_TAG);
            frame.push(id_bytes.len() as u8);
            frame.extend_from_slice(id_bytes);
            frame.extend_from_slice(&opus_data);
            let _ = qtx.try_send(frame);
        }
        // If no QUIC, drop silently — audio is real-time; WS relay is too slow.
    }

    /// Send a room audio frame to the supernode via WebSocket SFU_AUDIO.
    ///
    /// Outbound quota uses `room.audio.sfu` (gated against the supernode peer id).
    /// See `send_audio_datagram` for the direct P2P `core.audio.opus` path.
    ///
    /// This is the fallback path for when no direct QUIC connections exist
    /// between room members (typical over the Internet behind separate NATs).
    /// The supernode relays the frame to all other room members.
    async fn send_room_audio(&mut self, opus_data: Vec<u8>) {
        if self.current_room_id.is_empty() || self.current_supernode_id.is_empty() {
            return; // Not in a room
        }
        if !self.check_room_audio_outbound_quota(&self.current_supernode_id, opus_data.len()) {
            debug!(
                "[room.audio.sfu] outbound quota exceeded for {}; dropping frame",
                &self.current_supernode_id[..8.min(self.current_supernode_id.len())]
            );
            return;
        }
        let sender = self.identity.public_id();
        let room_id = self.current_room_id.clone();
        let supernode_id = self.current_supernode_id.clone();
        use base64::Engine;
        let audio_b64 = base64::engine::general_purpose::URL_SAFE.encode(&opus_data);
        let mut msg = SignalingMessage::new(MessageType::SfuAudio, sender);
        msg.target = Some(supernode_id);
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id));
        msg.payload
            .insert("audio".to_owned(), Value::String(audio_b64));
        self.dispatch_outbound(msg).await;
    }

    // -- typing indicator ----------------------------------------------------

    async fn send_typing(&mut self, peer_id: &str, is_typing: bool) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::ChatTyping, sender);
        msg.target = Some(peer_id.to_owned());
        msg.payload
            .insert("typing".to_owned(), Value::Bool(is_typing));
        self.dispatch_outbound(msg).await;
    }

    // -- SFU room chat -------------------------------------------------------

    async fn send_sfu_chat(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        body: &str,
        sender_handle: &str,
    ) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuChat, sender);
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload
            .insert("body".to_owned(), Value::String(body.to_owned()));
        msg.payload.insert(
            "sender_handle".to_owned(),
            Value::String(sender_handle.to_owned()),
        );
        self.dispatch_outbound(msg).await;
    }

    // -- capability announce -------------------------------------------------

    async fn send_capability_announce(&mut self, peer_id: &str) {
        let sender = self.identity.public_id();
        // Snapshot of every capability registered locally (includes the
        // first-party `core.*` modules from `register_client_modules` plus
        // anything the application layer registered later).
        let mut descriptors = self.feature_registry.snapshot();
        // Always advertise the standard transport-layer capabilities so the
        // remote knows we speak the same wire formats.
        for d in [
            wellknown::transport_quic_audio_v1(),
            wellknown::transport_quic_stream_v1(),
            wellknown::transport_quic_feature_datagram_v1(),
            wellknown::transport_quic_uni_stream_v1(),
        ] {
            if !descriptors.iter().any(|c| c.id == d.id) {
                descriptors.push(d);
            }
        }
        let caps_json = match serde_json::to_value(&descriptors) {
            Ok(v) => v,
            Err(e) => {
                // Serialisation of a CapabilityDescriptor failing is a
                // local bug, not a wire condition. Surface it instead of
                // silently advertising an empty capability set (which the
                // peer would interpret as "this client has no features").
                error!(
                    "CAPABILITY_ANNOUNCE: failed to serialise capability descriptors ({e}); aborting send to {}",
                    &peer_id[..8.min(peer_id.len())]
                );
                return;
            }
        };
        let mut msg = SignalingMessage::new(MessageType::CapabilityAnnounce, sender);
        msg.target = Some(peer_id.to_owned());
        msg.payload.insert("capabilities".to_owned(), caps_json);
        self.dispatch_outbound(msg).await;
        debug!(
            "CAPABILITY_ANNOUNCE sent to {} ({} caps)",
            &peer_id[..8.min(peer_id.len())],
            descriptors.len()
        );
    }

    /// Send our build attestation (reproducible build ID + version) to a peer.
    /// This lets the remote verify we are running a build from a known / trusted
    /// source commit (or official release) per the user's intent for build attestation.
    async fn send_build_attestation(&mut self, peer_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::BuildAttestation, sender);
        msg.target = Some(peer_id.to_owned());
        msg.payload.insert(
            "build_id".to_owned(),
            Value::String(env!("CONQUERD_BUILD_ID").to_owned()),
        );
        msg.payload.insert(
            "version".to_owned(),
            Value::String(env!("CARGO_PKG_VERSION").to_owned()),
        );
        msg.payload.insert(
            "source_hash".to_owned(),
            Value::String(env!("CONQUERD_SOURCE_HASH").to_owned()),
        );
        if let Some(proof) = option_env!("CONQUERD_RELEASE_PROOF") {
            if !proof.is_empty() {
                msg.payload
                    .insert("release_sig".to_owned(), Value::String(proof.to_owned()));
            }
        }
        self.dispatch_outbound(msg).await;
        debug!(
            "BUILD_ATTESTATION sent to {}",
            &peer_id[..8.min(peer_id.len())]
        );
    }

    // -- avatar config (outbound) -------------------------------------------

    /// Broadcast our avatar config to a single trusted peer.
    ///
    /// Called after capability announce once the peer has a non-empty
    /// `transcript_hash` (meaning the Ed25519 handshake completed).
    /// The `config_json` string comes from `SettingsModel::avatar_config_json`.
    async fn send_avatar_config(&mut self, peer_id: &str, config_json: &str) {
        if config_json.is_empty() {
            return;
        }
        let cfg_val: Value = match serde_json::from_str(config_json) {
            Ok(v) => v,
            Err(e) => {
                warn!("send_avatar_config: invalid JSON — {e}");
                return;
            }
        };
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::AvatarConfig, sender);
        msg.target = Some(peer_id.to_owned());
        msg.payload.insert("config".to_owned(), cfg_val);
        self.dispatch_outbound(msg).await;
    }

    // -- capability invoke (outbound) ---------------------------------------

    async fn send_capability_invoke(
        &mut self,
        peer_id: &str,
        feature_id: &str,
        params: Value,
        channel_hint: Option<String>,
    ) {
        let sender = self.identity.public_id();
        let mut payload = serde_json::Map::new();
        payload.insert("id".to_owned(), Value::String(feature_id.to_owned()));
        payload.insert("params".to_owned(), params);
        if let Some(hint) = channel_hint {
            payload.insert("channel_hint".to_owned(), Value::String(hint));
        }
        let mut msg = SignalingMessage::new(MessageType::CapabilityInvoke, sender);
        msg.target = Some(peer_id.to_owned());
        for (k, v) in payload {
            msg.payload.insert(k, v);
        }
        self.dispatch_outbound(msg).await;
    }

    // -- capability invoke (inbound) ----------------------------------------

    /// Apply the three framework gates (intersection, auth tier, trust) and
    /// dispatch to the local module if all pass.
    fn handle_capability_invoke(&mut self, msg: &SignalingMessage) {
        let feature_id = match msg.payload.get("id").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                warn!(
                    "[capabilities] CAPABILITY_INVOKE from {} missing 'id' — dropped",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        // Validate `params` shape — modules expect either nothing or a JSON
        // object. Accepting arbitrary scalars (string, number, bool, array)
        // is a foot-gun for module authors and surfaces as a panic deep
        // inside whichever module handler runs.
        let params = match msg.payload.get("params") {
            None => Value::Null,
            Some(Value::Null) => Value::Null,
            Some(v) if v.is_object() => v.clone(),
            Some(_) => {
                warn!(
                    "[capabilities] CAPABILITY_INVOKE '{}' from {} has non-object params — dropped",
                    feature_id,
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };

        // Gate 1 — intersection: peer must have announced the feature.
        let peer_supports = self
            .peer_capabilities
            .get(&msg.sender)
            .map(|caps| caps.iter().any(|c| c.id == feature_id))
            .unwrap_or(false);
        if !peer_supports {
            info!(
                "[capabilities] peer {} invoked '{}' but did not announce it — dropped",
                &msg.sender[..8.min(msg.sender.len())],
                feature_id
            );
            return;
        }

        // Gate 2 — auth tier: only enforced when we have a local descriptor.
        if let Some(desc) = self.feature_registry.get(&feature_id) {
            match desc.auth {
                AuthTier::TrustedPeer => {
                    let trusted = self
                        .peer_store
                        .read()
                        .get_by_identity(&msg.sender)
                        .is_some();
                    if !trusted {
                        warn!(
                            "[capabilities] peer {} invoked '{}' (auth=trusted-peer) but is not trusted — dropped",
                            &msg.sender[..8.min(msg.sender.len())],
                            feature_id
                        );
                        return;
                    }
                }
                AuthTier::RoomMember => {
                    if !self.room_members.contains(&msg.sender) {
                        warn!(
                            "[capabilities] peer {} invoked '{}' (auth=room-member) but is not in room — dropped",
                            &msg.sender[..8.min(msg.sender.len())],
                            feature_id
                        );
                        return;
                    }
                }
                AuthTier::Public => {}
            }
        }

        // Gate 3 — feature trust: bespoke namespaces require user consent.
        match FeatureTrustGate::check(&feature_id, &msg.sender, &self.feature_trust) {
            TrustDecision::Allow => {}
            TrustDecision::Deny => {
                info!(
                    "[feature_trust] invoke of '{}' from {} denied (stored decision)",
                    feature_id,
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
            TrustDecision::Pending => {
                info!(
                    "[feature_trust] invoke of '{}' from {} pending user decision",
                    feature_id,
                    &msg.sender[..8.min(msg.sender.len())]
                );
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::CapabilityInvokePending {
                        peer_id: msg.sender.clone(),
                        feature_id,
                        params,
                    });
                return;
            }
        }

        // All gates passed — dispatch to the local module if one is bound.
        let ctx = InvocationContext {
            peer: msg.sender.clone(),
            params: params.clone(),
            channel_tag: None,
        };
        match self.feature_registry.dispatch_invoke(&feature_id, ctx) {
            Ok(()) => debug!(
                "[capabilities] invoked '{}' from {}",
                feature_id,
                &msg.sender[..8.min(msg.sender.len())]
            ),
            Err(e) => debug!(
                "[capabilities] no module bound for '{}' from {} ({e})",
                feature_id,
                &msg.sender[..8.min(msg.sender.len())]
            ),
        }
        let _ = self.event_tx.try_send(ConnectionEvent::CapabilityInvoked {
            peer_id: msg.sender.clone(),
            feature_id,
            params,
        });
    }

    // -- room list request ---------------------------------------------------

    async fn send_room_list_request(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuRoomList, sender);
        msg.target = Some(supernode_id.to_owned());
        self.dispatch_outbound(msg).await;
    }

    async fn send_supernode_info_request(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SupernodeInfoRequest, sender);
        msg.target = Some(supernode_id.to_owned());
        self.dispatch_outbound(msg).await;
    }

    // -- ping / keepalive ----------------------------------------------------

    async fn send_pings(&mut self) {
        // Sweep expired pending invites (abandoned / timed-out handshakes).
        // This runs on every ping tick so the map stays bounded even when
        // the inviter never completes the handshake.
        let now = Instant::now();
        let before = self.pending_invites.len();
        self.pending_invites
            .retain(|_, v| now.duration_since(v.created_at) < INVITE_TTL);
        let pruned = before - self.pending_invites.len();
        if pruned > 0 {
            info!("[invites] pruned {pruned} expired pending invite(s)");
        }

        let sender = self.identity.public_id();
        let mut ping_msg = SignalingMessage::new(MessageType::Ping, sender.clone());
        // Sign the Ping — the supernode rejects unsigned messages.
        if let Ok(canonical) = ping_msg.canonical_bytes() {
            let sig = self.identity.sign(&canonical);
            use base64::Engine;
            ping_msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(&sig));
        }
        if let Ok(json) = ping_msg.to_json() {
            for sn in self.supernodes.values() {
                if sn.connected {
                    let _ = sn.send_tx.try_send(WsMessage::Text(json.clone()));
                }
            }
        }
    }

    // -- inbound message handling --------------------------------------------

    /// Gate an inbound payload through the framework's per-feature quota.
    ///
    /// Legacy inbound gate used only by the old direct-emit paths for
    /// chat and file transfer.
    ///
    /// IMPORTANT (Audio Dispatch Decision - Option A):
    /// This wrapper is **not** used for `core.audio.opus`.
    /// Audio always calls `FeatureRegistry::gate_through_feature` directly
    /// on both the outbound send paths and (via dispatch) on inbound.
    ///
    /// The special case below (returning true when no module is bound) exists
    /// only so that advertisement-only first-party features like audio can
    /// continue using the legacy inbound chat/file dispatch paths without
    /// being dropped. Audio itself never goes through this wrapper.
    fn gate_through_feature(&self, feature_id: &str, sender: &str, payload: &[u8]) -> bool {
        if self.feature_registry.module(feature_id).is_none() {
            return true;
        }
        if self
            .feature_registry
            .dispatch_message(feature_id, sender.to_owned(), payload)
        {
            true
        } else {
            warn!(
                "[capabilities] '{}' from {} dropped — quota exhausted",
                feature_id,
                &sender[..8.min(sender.len())]
            );
            false
        }
    }

    /// Verify the Ed25519 signature on an inbound `SignalingMessage`.
    ///
    /// `msg.sender` is the sender's base64url-encoded Ed25519 public key
    /// (`public_id`). The signature must verify against the canonical bytes
    /// of the message under that key. Rejects any message that is unsigned,
    /// malformed, or whose sender field is not a 32-byte public key.
    ///
    /// This is the single trust boundary protecting every inbound code path
    /// in `handle_inbound`; without it any peer who can land bytes on the
    /// signaling stream could forge messages from any other peer.
    ///
    /// Replay protection (P0 improvement):
    /// - Signature check (existing)
    /// - Timestamp freshness window: messages older than MAX_MESSAGE_AGE_SECS
    ///   or more than a few minutes in the future are rejected as replays.
    const MAX_MESSAGE_AGE_SECS: f64 = 300.0; // 5 minutes

    fn verify_inbound_signature(msg: &SignalingMessage) -> bool {
        let Some(sig_b64) = msg.signature.as_deref() else {
            return false;
        };
        let Ok(sig_bytes) = crate::crypto::b64url_decode(sig_b64) else {
            return false;
        };
        let Ok(pub_bytes) = crate::crypto::b64url_decode(&msg.sender) else {
            return false;
        };
        if pub_bytes.len() != 32 {
            return false;
        }
        let canonical = match msg.canonical_bytes() {
            Ok(b) => b,
            Err(_) => return false,
        };
        if !crate::crypto::ed25519_verify(&pub_bytes, &sig_bytes, &canonical) {
            return false;
        }

        // Basic replay protection via timestamp window.
        // This catches old replays without requiring protocol changes or per-peer state.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);

        let age = (now - msg.timestamp).abs();
        if age > Self::MAX_MESSAGE_AGE_SECS {
            warn!(
                "[signaling] dropping {:?} from {} — stale or future timestamp (age={:.1}s)",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
                age
            );
            return false;
        }

        true
    }

    /// Sliding-window replay check, keyed on the message's Ed25519 signature.
    ///
    /// Returns `true` if the message is new (process it) and `false` if it is a
    /// replay of a signature already accepted from this sender within the
    /// freshness window. Must be called only after
    /// [`verify_inbound_signature`](Self::verify_inbound_signature) succeeds, so
    /// the signature is present and valid.
    fn check_replay(&self, msg: &SignalingMessage) -> bool {
        let Some(sig_b64) = msg.signature.as_deref() else {
            return false;
        };
        let Ok(sig_bytes) = crate::crypto::b64url_decode(sig_b64) else {
            return false;
        };
        self.replay_guard.check_and_record(&msg.sender, &sig_bytes)
    }

    fn is_blocked_sender(peer_store: &Arc<RwLock<PeerStore>>, sender: &str) -> bool {
        peer_store
            .read()
            .get(sender)
            .is_some_and(|rec| rec.blocked)
    }

    async fn handle_inbound(&mut self, msg: SignalingMessage) {
        // Enforce signed-transcript model: every inbound signaling message
        // MUST carry a valid Ed25519 signature over its canonical bytes,
        // signed by the key whose public_id is `msg.sender`. Drop silently
        // (with a warning) on any failure — never dispatch unverified data.
        if !Self::verify_inbound_signature(&msg) {
            warn!(
                "[signaling] dropping {:?} from {} — signature missing or invalid",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        // Sliding-window replay guard: reject re-delivery of an already-seen
        // signed message within the freshness window. Runs only after the
        // signature + freshness checks above have passed. Real-time audio
        // frames (SfuAudio, ~50 Hz) are exempt from signature dedup only —
        // they are ephemeral, already protected by the freshness window +
        // jitter buffer, and would otherwise flood the per-sender window.
        // Per-feature byte quotas still apply on the transport relay path.
        if msg.msg_type != MessageType::SfuAudio && !self.check_replay(&msg) {
            warn!(
                "[signaling] dropping {:?} from {} — replayed message",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        if Self::is_blocked_sender(&self.peer_store, &msg.sender)
            && matches!(
                msg.msg_type,
                MessageType::ChatMessage
                    | MessageType::ChatAck
                    | MessageType::ChatTyping
                    | MessageType::CallRequest
            )
        {
            debug!(
                "[signaling] dropping {:?} from blocked peer {}",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        match msg.msg_type {
            MessageType::Pong => {
                debug!("Pong from {}", msg.sender);
            }
            MessageType::ChatMessage => {
                // Approximate payload size for the chat-feature quota.
                let payload_size = msg
                    .payload
                    .get("body")
                    .and_then(Value::as_str)
                    .map(str::len)
                    .unwrap_or(0);
                let probe = vec![0u8; payload_size];
                if !self.gate_through_feature("core.chat.v1", &msg.sender, &probe) {
                    return;
                }
                let body = msg
                    .payload
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let msg_id = msg
                    .payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let handle = msg
                    .payload
                    .get("sender_handle")
                    .or_else(|| msg.payload.get("handle"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !msg_id.is_empty() {
                    let mut ack =
                        SignalingMessage::new(MessageType::ChatAck, self.identity.public_id());
                    ack.target = Some(msg.sender.clone());
                    ack.payload
                        .insert("message_id".to_string(), Value::String(msg_id.clone()));
                    self.dispatch_outbound(ack).await;
                }
                let _ = self.event_tx.try_send(ConnectionEvent::ChatMessage {
                    peer_id: msg.sender.clone(),
                    message_id: msg_id,
                    body,
                    timestamp: msg.timestamp,
                    sender_handle: handle,
                });
            }
            MessageType::ChatAck => {
                // Tiny payload — use a minimal probe so chat-ack stays under
                // the same quota umbrella as chat messages.
                if !self.gate_through_feature("core.chat.v1", &msg.sender, &[]) {
                    return;
                }
                let msg_id = msg
                    .payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let _ = self.event_tx.try_send(ConnectionEvent::ChatAck {
                    peer_id: msg.sender.clone(),
                    message_id: msg_id,
                });
            }
            MessageType::CallRequest => {
                let _ = self.event_tx.try_send(ConnectionEvent::CallRequest {
                    peer_id: msg.sender.clone(),
                });
            }
            MessageType::CallAccept => {
                let _ = self.event_tx.try_send(ConnectionEvent::CallAccepted {
                    peer_id: msg.sender.clone(),
                });
            }
            MessageType::CallEnd | MessageType::CallReject => {
                let _ = self.event_tx.try_send(ConnectionEvent::CallEnded {
                    peer_id: msg.sender.clone(),
                });
            }
            MessageType::ChatTyping => {
                if !self.gate_through_feature("core.chat.v1", &msg.sender, &[]) {
                    return;
                }
                let is_typing = msg
                    .payload
                    .get("typing")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let _ = self.event_tx.try_send(ConnectionEvent::TypingIndicator {
                    peer_id: msg.sender.clone(),
                    is_typing,
                });
            }
            MessageType::HandleUpdate => {
                let handle = msg
                    .payload
                    .get("handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !handle.is_empty() {
                    // Persist updated handle in peer store
                    let mut store = self.peer_store.write();
                    if let Some(rec) = store.get_mut(&msg.sender) {
                        rec.handle = handle.clone();
                    }
                    let _ = store.save();
                    drop(store);
                }
                let _ = self.event_tx.try_send(ConnectionEvent::HandleUpdated {
                    peer_id: msg.sender.clone(),
                    handle,
                });
            }
            MessageType::AvatarConfig => {
                // Only accept avatar configs from peers that have completed the
                // Ed25519 handshake, indicated by a non-empty identity_pub.
                // (transcript_hash was the intended field but is never populated.)
                let sender = msg.sender.clone();
                let trusted = {
                    let store = self.peer_store.read();
                    store
                        .get(&sender)
                        .map(|r| !r.identity_pub.is_empty())
                        .unwrap_or(false)
                };
                if trusted {
                    // Deserialize from the "config" sub-object in the payload.
                    if let Some(cfg_val) = msg.payload.get("config") {
                        if let Ok(cfg) = serde_json::from_value::<PeerAvatarConfig>(cfg_val.clone())
                        {
                            let mut store = self.peer_store.write();
                            if let Some(rec) = store.get_mut(&sender) {
                                rec.avatar_config = Some(cfg);
                            }
                            let _ = store.save();
                            drop(store);
                            let _ = self
                                .event_tx
                                .try_send(ConnectionEvent::AvatarConfigUpdated { peer_id: sender });
                        }
                    }
                }
            }
            MessageType::SfuMembers => {
                let members: Vec<String> = msg
                    .payload
                    .get("members")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::RoomMembersChanged(members));
            }
            MessageType::SfuPeerJoined => {
                let peer_id = msg
                    .payload
                    .get("peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::RoomPeerJoined { peer_id });
            }
            MessageType::SfuPeerLeft => {
                let peer_id = msg
                    .payload
                    .get("peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::RoomPeerLeft { peer_id });
            }
            MessageType::SfuChat => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let body = msg
                    .payload
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let sender_handle = msg
                    .payload
                    .get("sender_handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !body.is_empty() {
                    let _ = self.event_tx.try_send(ConnectionEvent::RoomChatMessage {
                        room_id,
                        sender_id: msg.sender.clone(),
                        sender_handle,
                        body,
                        timestamp: msg.timestamp,
                    });
                }
            }
            MessageType::SfuFileOffer => {
                self.handle_sfu_file_offer(&msg).await;
            }
            MessageType::SfuFileChunk => {
                self.handle_sfu_file_chunk(&msg).await;
            }
            MessageType::SfuFileComplete => {
                self.handle_sfu_file_complete(&msg).await;
            }
            MessageType::SfuAudio => {
                // Inbound room audio relayed by the supernode.  The `sender`
                // field is the originating peer (preserved by the supernode
                // broadcast).  Decode the base64 Opus payload, enforce the
                // room.audio.sfu per-sender inbound quota, then forward to
                // the call controller via a `SfuAudioReceived` event.
                use base64::Engine;
                let audio_b64 = msg
                    .payload
                    .get("audio")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if let Ok(opus_data) = base64::engine::general_purpose::URL_SAFE.decode(audio_b64) {
                    if !opus_data.is_empty() {
                        if !self.check_inbound_feature_quota(
                            "room.audio.sfu",
                            &msg.sender,
                            opus_data.len(),
                        ) {
                            debug!(
                                "[room.audio.sfu] inbound quota exceeded for {}; dropping frame",
                                &msg.sender[..8.min(msg.sender.len())]
                            );
                            return;
                        }
                        let _ = self.event_tx.try_send(ConnectionEvent::SfuAudioReceived {
                            peer_id: msg.sender.clone(),
                            opus_data,
                        });
                    }
                }
            }
            MessageType::RelayGranted => {
                let ticket = msg
                    .payload
                    .get("ticket")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let host = msg
                    .payload
                    .get("relay_host")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let port = msg
                    .payload
                    .get("relay_port")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u16;
                let _ = self.event_tx.try_send(ConnectionEvent::RelayGranted {
                    supernode_id: msg.sender.clone(),
                    ticket,
                    relay_host: host.clone(),
                    relay_port: port,
                });
                // Open the QUIC relay connection so subsequent
                // `web.host.app.v1` fetches (and future native SFU paths)
                // have a live `quinn::Connection` to multiplex over.
                self.spawn_relay_client_connect(msg.sender.clone(), host, port);
            }
            MessageType::CapabilityAnnounce => {
                let raw = msg
                    .payload
                    .get("capabilities")
                    .cloned()
                    .unwrap_or(Value::Null);
                let caps_json = raw.to_string();
                // Parse and cache for the intersection check on inbound
                // CAPABILITY_INVOKE. Unknown / malformed entries are
                // silently ignored — we keep what successfully parsed.
                let parsed: Vec<CapabilityDescriptor> = match raw {
                    Value::Array(arr) => arr
                        .into_iter()
                        .filter_map(|v| serde_json::from_value::<CapabilityDescriptor>(v).ok())
                        .collect(),
                    _ => Vec::new(),
                };
                debug!(
                    "CAPABILITY_ANNOUNCE from {}: {} cap(s)",
                    &msg.sender[..8.min(msg.sender.len())],
                    parsed.len()
                );
                self.peer_capabilities.insert(msg.sender.clone(), parsed);
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::CapabilityAnnounced {
                        peer_id: msg.sender.clone(),
                        caps_json,
                    });
            }
            MessageType::CapabilityInvoke => {
                self.handle_capability_invoke(&msg);
            }
            MessageType::EndpointUpdate => {
                let endpoints: Vec<String> = msg
                    .payload
                    .get("endpoints")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let _ = self.event_tx.try_send(ConnectionEvent::EndpointUpdated {
                    peer_id: msg.sender.clone(),
                    endpoints,
                });
            }
            MessageType::SupernodeInfo => {
                let homepage_url = msg
                    .payload
                    .get("homepage_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let title = msg
                    .payload
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let mut wt_url = msg
                    .payload
                    .get("wt_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // Self-heal: a supernode that was started without
                // `supernode_host` advertises its WebTransport URL with a
                // loopback/wildcard host (`localhost`, `127.0.0.1`,
                // `0.0.0.0`).  That host is meaningless to a remote client —
                // its embedded browser would dial its OWN machine and fail
                // ("Opening handshake failed").  We already reached this
                // supernode at a real address (its signaling `ws_url`), so
                // substitute that host while preserving the advertised port.
                if !wt_url.is_empty() {
                    if let Some(sn) = self.supernodes.get(&msg.sender) {
                        if let Some(fixed) = rewrite_loopback_wt_url(&wt_url, &sn.ws_url) {
                            debug!(
                                "Rewrote supernode wt_url {} -> {} using signaling host",
                                wt_url, fixed
                            );
                            wt_url = fixed;
                        }
                    }
                }
                let cert_fingerprint = msg
                    .payload
                    .get("cert_fingerprint")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // Belt-and-suspenders: populate the scheme-layer caches here
                // on the tokio thread, *before* the QUIC relay is established.
                // This ensures /_conquerd/ctx.json always serves a populated
                // wtBaseUrl/wtCertHash by the time any FetchWebApp can succeed.
                // The bridge.rs call on the Qt main thread is kept as well;
                // both writes are idempotent (Mutex-guarded HashMap inserts).
                #[cfg(feature = "webengine")]
                {
                    if !wt_url.is_empty() {
                        crate::ui::scheme::set_supernode_wt_url(&msg.sender, &wt_url);
                    }
                    if !cert_fingerprint.is_empty() {
                        crate::ui::scheme::set_supernode_cert_fingerprint(
                            &msg.sender,
                            &cert_fingerprint,
                        );
                    }
                }
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::SupernodeInfoReceived {
                        supernode_id: msg.sender.clone(),
                        homepage_url,
                        title,
                        wt_url,
                        cert_fingerprint,
                    });
            }
            MessageType::RelayPaymentRequired => {
                let portal_url = msg
                    .payload
                    .get("portal_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !portal_url.is_empty() {
                    info!(
                        "[relay] Portal required from {}: {}",
                        &msg.sender[..8.min(msg.sender.len())],
                        portal_url
                    );
                    let _ = self
                        .event_tx
                        .try_send(ConnectionEvent::RelayPaymentRequired {
                            supernode_id: msg.sender.clone(),
                            portal_url,
                        });
                }
            }
            MessageType::SfuRoomList => {
                let rooms_json = msg
                    .payload
                    .get("rooms")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "[]".to_owned());
                let _ = self.event_tx.try_send(ConnectionEvent::RoomListReceived {
                    supernode_id: msg.sender.clone(),
                    rooms_json,
                });
            }
            MessageType::PresenceUpdate => {
                let status = msg
                    .payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("online")
                    .to_owned();
                let _ = self.event_tx.try_send(ConnectionEvent::PresenceUpdated {
                    peer_id: msg.sender.clone(),
                    status,
                });
            }
            // ── Invite handshake (inviter side: we receive INIT from the joiner) ──
            MessageType::InviteHandshakeInit => {
                let invite_id = msg
                    .payload
                    .get("invite_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let joiner_identity_pub = msg.sender.clone();
                let joiner_peer_id = msg
                    .payload
                    .get("joiner_peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&joiner_identity_pub)
                    .to_owned();
                let joiner_handle = msg
                    .payload
                    .get("joiner_handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                info!(
                    "InviteHandshakeInit from {} (id={})",
                    &joiner_identity_pub[..8.min(joiner_identity_pub.len())],
                    &invite_id[..8.min(invite_id.len())]
                );

                // Add joiner to peer store
                {
                    let mut store = self.peer_store.write();
                    if !store.contains(&joiner_peer_id) {
                        store.upsert(crate::peer_store::PeerRecord {
                            peer_id: joiner_peer_id.clone(),
                            identity_pub: joiner_identity_pub.clone(),
                            handle: joiner_handle.clone(),
                            created_at: unix_now_f64(),
                            last_seen_at: unix_now_f64(),
                            ..Default::default()
                        });
                        let _ = store.save();
                    }
                }

                // Send INVITE_HANDSHAKE_ACCEPT back
                let sender = self.identity.public_id();
                let peer_id_str = self.identity.peer_id();
                let mut reply =
                    SignalingMessage::new(MessageType::InviteHandshakeAccept, sender.clone());
                reply.target = Some(joiner_identity_pub.clone());
                reply
                    .payload
                    .insert("invite_id".into(), Value::String(invite_id));
                reply
                    .payload
                    .insert("inviter_peer_id".into(), Value::String(peer_id_str));
                reply
                    .payload
                    .insert("inviter_identity_pub".into(), Value::String(sender));
                self.dispatch_outbound(reply).await;

                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::PeerConnected(joiner_peer_id.clone()));
                let _ = self.event_tx.try_send(ConnectionEvent::InviteAccepted {
                    peer_id: joiner_peer_id,
                    handle: joiner_handle,
                });
            }
            // ── Invite handshake (joiner side: we receive ACCEPT from the inviter) ──
            MessageType::InviteHandshakeAccept => {
                let invite_id = msg
                    .payload
                    .get("invite_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let inviter_identity_pub = msg
                    .payload
                    .get("inviter_identity_pub")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                let inviter_peer_id = msg
                    .payload
                    .get("inviter_peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&inviter_identity_pub)
                    .to_owned();
                let inviter_handle = msg
                    .payload
                    .get("inviter_handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();

                if let Some(pending) = self.pending_invites.remove(&invite_id) {
                    info!(
                        "InviteHandshakeAccept from {} (id={})",
                        &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
                        &invite_id[..8.min(invite_id.len())]
                    );
                    // Add inviter to peer store
                    {
                        let mut store = self.peer_store.write();
                        let relay_hints = if pending.relay_hint.is_empty() {
                            vec![]
                        } else {
                            vec![pending.relay_hint.clone()]
                        };
                        store.upsert(crate::peer_store::PeerRecord {
                            peer_id: inviter_peer_id.clone(),
                            identity_pub: inviter_identity_pub.clone(),
                            handle: inviter_handle.clone(),
                            relay_hints,
                            created_at: unix_now_f64(),
                            last_seen_at: unix_now_f64(),
                            ..Default::default()
                        });
                        let _ = store.save();
                    }
                    let _ = self
                        .event_tx
                        .try_send(ConnectionEvent::PeerConnected(inviter_peer_id.clone()));
                    let _ = self.event_tx.try_send(ConnectionEvent::InviteAccepted {
                        peer_id: inviter_peer_id,
                        handle: inviter_handle,
                    });
                } else {
                    warn!("InviteHandshakeAccept for unknown invite_id={invite_id}");
                }
            }
            MessageType::InviteHandshakeReject => {
                warn!(
                    "Invite rejected by {}",
                    &msg.sender[..8.min(msg.sender.len())]
                );
            }
            // ── File transfer ─────────────────────────────────────────────────────────────
            MessageType::FileTransferOffer => {
                if !self.gate_through_feature("core.file.v1", &msg.sender, &[]) {
                    return;
                }
                // Required fields. A peer that omits `transfer_id`, `sha256`,
                // or `size` cannot be honoured — silently coercing those to
                // empty/zero used to create ghost inbound transfers that
                // could never complete and never time out, leaking state.
                let tid = match msg.payload.get("transfer_id").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() => s.to_owned(),
                    _ => {
                        warn!(
                            "FILE_OFFER from {} missing transfer_id — dropped",
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let sha = match msg.payload.get("sha256").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() => s.to_owned(),
                    _ => {
                        warn!(
                            "FILE_OFFER {} from {} missing sha256 — dropped",
                            tid,
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let size = match msg.payload.get("size").and_then(Value::as_u64) {
                    Some(n) => n as usize,
                    None => {
                        warn!(
                            "FILE_OFFER {} from {} missing/non-numeric size — dropped",
                            tid,
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let rel = msg
                    .payload
                    .get("rel_path")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let tot = msg
                    .payload
                    .get("total_chunks")
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as usize;
                let purp = msg
                    .payload
                    .get("purpose")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_owned();
                let comp = msg
                    .payload
                    .get("compressed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let delt = msg
                    .payload
                    .get("is_delta")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let bsha = msg
                    .payload
                    .get("base_sha256")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_offer_received(
                    &msg.sender,
                    &tid,
                    &rel,
                    &sha,
                    size,
                    tot,
                    &purp,
                    comp,
                    delt,
                    &bsha,
                );
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferAccept => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_accepted(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferReject => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_rejected(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferChunk => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let idx = msg
                    .payload
                    .get("chunk_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let data = msg
                    .payload
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // Chunk size dominates the file feature's byte quota —
                // base64 expands by ~4/3 so the wire size approximates
                // `data.len()`.
                let probe = vec![0u8; data.len()];
                if !self.gate_through_feature("core.file.v1", &msg.sender, &probe) {
                    return;
                }
                let evs = self.file_mgr.on_chunk_received(&tid, idx, data);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferComplete => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_complete_received(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferAck => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_ack(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferError => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let reason = msg
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_error(&tid, &reason);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::BuildAttestation | MessageType::AttestationResponse => {
                // Store the peer's reported build info for reproducible-build / trusted-build attestation.
                // The message is already signature + replay verified by the caller.
                let build_id = msg
                    .payload
                    .get("build_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let version = msg
                    .payload
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let source_hash = msg
                    .payload
                    .get("source_hash")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let release_sig = msg.payload.get("release_sig").and_then(Value::as_str);

                if !build_id.is_empty() {
                    let is_official = crate::crypto::verify_official_release_build(
                        &build_id,
                        &version,
                        &source_hash,
                        release_sig,
                    );

                    let mut store = self.peer_store.write();
                    if let Some(rec) = store.get_mut(&msg.sender) {
                        rec.peer_build_hash = build_id.clone();
                        rec.peer_source_hash = source_hash.clone();
                        if !version.is_empty() {
                            rec.peer_version = version.clone();
                        }
                        rec.last_attestation_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs_f64())
                            .unwrap_or(0.0);
                        rec.attestation_status = if is_official {
                            "official".to_string()
                        } else {
                            "claimed".to_string()
                        };
                    }
                    let _ = store.save();
                    drop(store);

                    debug!(
                        "Build attestation from {}: build_id={}, source_hash={}, version={}, official={}",
                        &msg.sender[..8.min(msg.sender.len())],
                        build_id,
                        if source_hash.is_empty() { "n/a" } else { &source_hash },
                        if version.is_empty() { "n/a" } else { &version },
                        is_official
                    );

                    // Also forward so the UI layer (bridge, models) can react if desired
                    // (e.g. update peer list with build info, enforce policy).
                    let _ = self
                        .event_tx
                        .try_send(ConnectionEvent::SignalingMessage(msg));
                }
            }
            _ => {
                // Forward unhandled messages to the app layer
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::SignalingMessage(msg));
            }
        }
    }

    // -- invite acceptance ---------------------------------------------------

    async fn handle_accept_invite(&mut self, invite_url: String) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        const SCHEME: &str = "conquerd://";
        if !invite_url.starts_with(SCHEME) {
            warn!("AcceptInvite: invalid scheme in '{invite_url}'");
            return;
        }

        // Support both formats: conquerd://<b64> and legacy conquerd://invite#<b64>
        let encoded_raw = &invite_url[SCHEME.len()..];
        let encoded = encoded_raw.trim_start_matches("invite#");

        if encoded.len() > 262_144 {
            warn!(
                "AcceptInvite: invite URL too large ({} bytes)",
                encoded.len()
            );
            return;
        }

        let json_bytes = match URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')) {
            Ok(b) => b,
            Err(e) => {
                warn!("AcceptInvite: base64 decode error: {e}");
                return;
            }
        };

        let payload: serde_json::Value = match serde_json::from_slice(&json_bytes) {
            Ok(v) => v,
            Err(e) => {
                warn!("AcceptInvite: JSON parse error: {e}");
                return;
            }
        };

        let inviter_identity_pub = match payload.get("inviter_identity_pub").and_then(Value::as_str)
        {
            Some(s) => s.to_owned(),
            None => {
                warn!("AcceptInvite: missing inviter_identity_pub");
                return;
            }
        };
        let inviter_peer_id = payload
            .get("inviter_peer_id")
            .and_then(Value::as_str)
            .unwrap_or(&inviter_identity_pub)
            .to_owned();
        let invite_id = payload
            .get("invite_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let relay_hint = payload
            .get("relay_hint")
            .and_then(Value::as_str)
            .or_else(|| payload.get("lan_hint").and_then(Value::as_str))
            .map(|s| s.to_owned())
            .unwrap_or_default();

        if invite_id.is_empty() {
            warn!("AcceptInvite: missing invite_id");
            return;
        }
        if inviter_identity_pub == self.identity.public_id() {
            warn!("AcceptInvite: cannot use own invite");
            return;
        }

        info!(
            "Accepting invite from {} (id={})",
            &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
            &invite_id[..8.min(invite_id.len())]
        );

        // Store pending invite (matched when INVITE_HANDSHAKE_ACCEPT arrives)
        self.pending_invites.insert(
            invite_id.clone(),
            PendingInvite {
                inviter_peer_id: inviter_peer_id.clone(),
                inviter_identity_pub: inviter_identity_pub.clone(),
                invite_id: invite_id.clone(),
                relay_hint: relay_hint.clone(),
                created_at: Instant::now(),
            },
        );

        // Connect to the relay WS if not already connected
        if !relay_hint.is_empty() && !self.supernodes.contains_key(&inviter_peer_id) {
            self.connect_supernode_ws(inviter_peer_id.clone(), relay_hint.clone())
                .await;
        }

        // Build + sign INVITE_HANDSHAKE_INIT and queue directly on the WS send
        // channel (the message will be delivered once the WS connection is up).
        let sender = self.identity.public_id();
        let joiner_peer_id = self.identity.peer_id();
        let mut msg = SignalingMessage::new(MessageType::InviteHandshakeInit, sender.clone());
        msg.target = Some(inviter_identity_pub.clone());
        msg.payload
            .insert("invite_id".into(), Value::String(invite_id));
        msg.payload
            .insert("joiner_identity_pub".into(), Value::String(sender.clone()));
        msg.payload
            .insert("joiner_peer_id".into(), Value::String(joiner_peer_id));

        if let Ok(canonical) = msg.canonical_bytes() {
            let sig = self.identity.sign(&canonical);
            use base64::Engine;
            msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        }

        if let Ok(json) = msg.to_json() {
            if let Some(sn) = self.supernodes.get(&inviter_peer_id) {
                let _ = sn.send_tx.try_send(WsMessage::Text(json));
            } else {
                warn!("AcceptInvite: no WS session for inviter — message dropped");
            }
        }
    }

    async fn handle_sfu_file_offer(&mut self, msg: &SignalingMessage) {
        if !self.gate_through_feature("room.file.v1", &msg.sender, &[]) {
            return;
        }
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned();
        let tid = match msg.payload.get("transfer_id").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                warn!(
                    "SFU_FILE_OFFER from {} missing transfer_id",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        let sha = match msg.payload.get("sha256").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                warn!(
                    "SFU_FILE_OFFER {tid} from {} missing sha256",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        let size = match msg.payload.get("size").and_then(Value::as_u64) {
            Some(n) => n as usize,
            None => {
                warn!(
                    "SFU_FILE_OFFER {tid} from {} missing/non-numeric size",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        let rel = msg
            .payload
            .get("rel_path")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_owned();
        let total_chunks = msg
            .payload
            .get("total_chunks")
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let purpose = msg
            .payload
            .get("purpose")
            .and_then(Value::as_str)
            .unwrap_or("room_file")
            .to_owned();
        let compressed = msg
            .payload
            .get("compressed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let is_delta = msg
            .payload
            .get("is_delta")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let base_sha = msg
            .payload
            .get("base_sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        let mut evs = self.room_file_mgr.on_offer_received(
            &msg.sender,
            &tid,
            &rel,
            &sha,
            size,
            total_chunks,
            &purpose,
            compressed,
            is_delta,
            &base_sha,
        );
        evs.extend(self.room_file_mgr.accept_transfer_locally(&tid));
        self.dispatch_room_transfer_events(evs, "", &room_id).await;
    }

    async fn handle_sfu_file_chunk(&mut self, msg: &SignalingMessage) {
        let tid = msg
            .payload
            .get("transfer_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let idx = msg
            .payload
            .get("chunk_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let data = msg
            .payload
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("");
        let probe = vec![0u8; data.len()];
        if !self.gate_through_feature("room.file.v1", &msg.sender, &probe) {
            return;
        }
        let evs = self.room_file_mgr.on_chunk_received(&tid, idx, data);
        self.dispatch_room_transfer_events(evs, "", "").await;
    }

    async fn handle_sfu_file_complete(&mut self, msg: &SignalingMessage) {
        let tid = msg
            .payload
            .get("transfer_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let evs = self.room_file_mgr.on_complete_received(&tid);
        self.dispatch_room_transfer_events(evs, "", "").await;
    }
    // -- file transfer -------------------------------------------------------

    /// Dispatch a batch of [`TransferEvent`]s, routing outbound messages and
    /// emitting the appropriate [`ConnectionEvent`]s upward.
    async fn dispatch_transfer_events(&mut self, events: Vec<TransferEvent>) {
        for ev in events {
            match ev {
                TransferEvent::SendMessage {
                    peer_id,
                    message_type,
                    payload,
                } => {
                    let sender = self.identity.public_id();
                    let mut msg = SignalingMessage::new(message_type, sender);
                    msg.target = Some(peer_id);
                    msg.payload = payload.into_iter().collect();
                    self.dispatch_outbound(msg).await;
                }
                TransferEvent::Offered {
                    transfer_id,
                    peer_id,
                    rel_path,
                    size,
                    purpose,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileOffered {
                        transfer_id,
                        peer_id,
                        rel_path,
                        size,
                        purpose,
                        is_self: false,
                    });
                }
                TransferEvent::Progress {
                    transfer_id,
                    progress,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileProgress {
                        transfer_id,
                        progress,
                    });
                }
                TransferEvent::Complete {
                    transfer_id,
                    data,
                    rel_path,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileComplete {
                        transfer_id,
                        data,
                        rel_path,
                    });
                }
                TransferEvent::Failed {
                    transfer_id,
                    reason,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileFailed {
                        transfer_id,
                        reason,
                    });
                }
                TransferEvent::StateChanged { .. } => {
                    // Granular state changes; no top-level event for now.
                }
            }
        }
    }

    async fn dispatch_room_transfer_events(
        &mut self,
        events: Vec<TransferEvent>,
        supernode_id: &str,
        room_id: &str,
    ) {
        for ev in events {
            match ev {
                TransferEvent::SendMessage {
                    message_type,
                    mut payload,
                    ..
                } => {
                    let room_msg_type = match message_type {
                        MessageType::FileTransferOffer => MessageType::SfuFileOffer,
                        MessageType::FileTransferChunk => MessageType::SfuFileChunk,
                        MessageType::FileTransferComplete => MessageType::SfuFileComplete,
                        _ => continue,
                    };
                    payload.insert("room_id".into(), Value::String(room_id.to_owned()));
                    let sender = self.identity.public_id();
                    let mut msg = SignalingMessage::new(room_msg_type, sender);
                    msg.target = Some(supernode_id.to_owned());
                    msg.payload = payload.into_iter().collect();
                    self.dispatch_outbound(msg).await;
                }
                TransferEvent::Offered {
                    transfer_id,
                    rel_path,
                    size,
                    purpose,
                    ..
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileOffered {
                        transfer_id,
                        peer_id: room_id.to_owned(),
                        rel_path,
                        size,
                        purpose,
                        is_self: false,
                    });
                }
                TransferEvent::Progress {
                    transfer_id,
                    progress,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileProgress {
                        transfer_id,
                        progress,
                    });
                }
                TransferEvent::Complete {
                    transfer_id,
                    data,
                    rel_path,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileComplete {
                        transfer_id,
                        data,
                        rel_path,
                    });
                }
                TransferEvent::Failed {
                    transfer_id,
                    reason,
                } => {
                    let _ = self.event_tx.try_send(ConnectionEvent::FileFailed {
                        transfer_id,
                        reason,
                    });
                }
                TransferEvent::StateChanged { .. } => {}
            }
        }
    }
} // end impl ConnectionManager

// ---------------------------------------------------------------------------
// Supernode WebSocket task
// ---------------------------------------------------------------------------

/// Long-running tokio task that maintains a WebSocket connection to a
/// supernode, sends identity hello, and routes inbound messages back to
/// the connection manager via `internal_tx`.
async fn supernode_ws_task(
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

fn build_hello(identity: &Identity) -> std::result::Result<String, serde_json::Error> {
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

fn unix_now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

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
async fn run_quic_peer_session(
    connection: quinn::Connection,
    peer_id: String,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    let (mut send_stream, mut recv_stream) = match connection.open_bi().await {
        Ok(streams) => streams,
        Err(e) => {
            warn!(
                "Failed to open signaling stream to {}: {e}",
                &peer_id[..8.min(peer_id.len())]
            );
            let _ = internal_tx
                .send(InternalEvent::QuicDisconnected { peer_id })
                .await;
            return;
        }
    };

    // Channel for the connection manager to push outbound signaling bytes.
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
    let write_task = tokio::spawn(async move {
        while let Some(bytes) = sig_rx.recv().await {
            // Length-prefix framing: 4-byte big-endian length + payload
            let len = (bytes.len() as u32).to_be_bytes();
            if send_stream.write_all(&len).await.is_err() {
                break;
            }
            if send_stream.write_all(&bytes).await.is_err() {
                break;
            }
        }
        debug!(
            "QUIC write task ended for {}",
            &peer_id_w[..8.min(peer_id_w.len())]
        );
    });

    // Read loop: length-prefixed frames
    let mut len_buf = [0u8; 4];
    loop {
        match recv_stream.read_exact(&mut len_buf).await {
            Ok(()) => {}
            Err(_) => break,
        }
        let frame_len = u32::from_be_bytes(len_buf) as usize;
        if frame_len > 4 * 1024 * 1024 {
            // Guard: refuse >4 MiB signaling frames
            warn!(
                "QUIC signaling frame too large ({frame_len} bytes) from {}",
                &peer_id_r[..8.min(peer_id_r.len())]
            );
            break;
        }
        let mut payload = vec![0u8; frame_len];
        match recv_stream.read_exact(&mut payload).await {
            Ok(()) => {
                let _ = internal_tx
                    .send(InternalEvent::QuicSignalingData {
                        peer_id: peer_id_r.clone(),
                        data: payload,
                    })
                    .await;
            }
            Err(_) => break,
        }
    }

    write_task.abort();
    connection.close(0u32.into(), b"bye");
    let _ = internal_tx
        .send(InternalEvent::QuicDisconnected { peer_id: peer_id_r })
        .await;
}

#[cfg(test)]
mod tests {
    use super::{host_from_url, is_loopback_or_wildcard, rewrite_loopback_wt_url};

    #[test]
    fn host_from_url_variants() {
        assert_eq!(
            host_from_url("ws://1.2.3.4:34935/sig").as_deref(),
            Some("1.2.3.4")
        );
        assert_eq!(
            host_from_url("wss://relay.example:443").as_deref(),
            Some("relay.example")
        );
        assert_eq!(
            host_from_url("https://localhost:8443").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            host_from_url("relay.example:34935").as_deref(),
            Some("relay.example")
        );
        assert_eq!(
            host_from_url("ws://user@host:80/x").as_deref(),
            Some("host")
        );
        assert_eq!(host_from_url("https://[::1]:8443").as_deref(), Some("::1"));
        assert_eq!(
            host_from_url("https://[2001:db8::1]:8443").as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(host_from_url(""), None);
    }

    #[test]
    fn loopback_detection() {
        for h in ["localhost", "127.0.0.1", "0.0.0.0", "::1", "::"] {
            assert!(
                is_loopback_or_wildcard(h),
                "{h} should be loopback/wildcard"
            );
        }
        for h in ["1.2.3.4", "relay.example", "example.com"] {
            assert!(!is_loopback_or_wildcard(h), "{h} should be routable");
        }
    }

    #[test]
    fn rewrites_loopback_host_using_signaling_url() {
        let fixed = rewrite_loopback_wt_url("https://localhost:8443", "ws://203.0.113.7:34935/sig");
        assert_eq!(fixed.as_deref(), Some("https://203.0.113.7:8443"));
    }

    #[test]
    fn rewrites_wildcard_host_and_preserves_port() {
        let fixed = rewrite_loopback_wt_url("https://0.0.0.0:9000", "wss://relay.example:443");
        assert_eq!(fixed.as_deref(), Some("https://relay.example:9000"));
    }

    #[test]
    fn no_rewrite_when_wt_host_already_routable() {
        assert!(
            rewrite_loopback_wt_url("https://relay.example:8443", "ws://203.0.113.7:34935",)
                .is_none()
        );
    }

    #[test]
    fn no_rewrite_when_signaling_host_is_also_loopback() {
        assert!(
            rewrite_loopback_wt_url("https://localhost:8443", "ws://127.0.0.1:34935",).is_none()
        );
    }
}
