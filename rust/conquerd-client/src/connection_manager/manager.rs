//! [`ConnectionManager`] implementation.

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
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

use crate::avatar_config::AvatarConfig as PeerAvatarConfig;
use crate::feature_trust::{FeatureTrustGate, FeatureTrustStore, TrustDecision};
use crate::file_transfer::{FileTransferManager, TransferEvent};
use crate::group_key::{GroupKeySource, SenderKeysGroup};
use crate::identity::Identity;
use crate::peer_store::PeerStore;
use crate::protocol::{MessageType, SignalingMessage};
use crate::quic_relay_client::{QuicRelayClient, RelaySignalingInbound};
use crate::quic_tls;
use crate::web_app_client::{self, WebAppResponse};

use super::events::{ConnectionCommand, ConnectionEvent};
use super::internal::{
    rewrite_loopback_wt_url, InternalEvent, PeerConnection, PeerConnectionState,
    PeerTransportStats, PendingInvite, SupernodePingTracker, SupernodeSession, INVITE_TTL,
};
use super::quic::run_quic_peer_session;
use super::ws::supernode_ws_task;

const _CONNECT_TIMEOUT_S: f64 = 4.0;
const WS_RECONNECT_DELAY_S: u64 = 5;
const PING_INTERVAL_S: u64 = 30;
const AUDIO_CHANNEL_TAG: u8 = channel_frame::AUDIO_TAG;
const DEFAULT_QUIC_LISTENER_PORT: u16 = 61_045;
const QUIC_PORT_SEARCH_LIMIT: u16 = 128;
const QUIC_PORT_FILE: &str = "quic_listener_port";

fn unix_now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(super) fn parse_quic_lan_hint(hint: &str) -> Option<(String, u16)> {
    let trimmed = hint.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("quic://")
        .or_else(|| trimmed.strip_prefix("udp://"))
        .unwrap_or(trimmed);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    if authority.is_empty() {
        return None;
    }
    if let Ok(addr) = authority.parse::<SocketAddr>() {
        return Some((addr.ip().to_string(), addr.port()));
    }
    let (host, port) = authority.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    let port = port.parse::<u16>().ok()?;
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((host.to_owned(), port))
}

/// Wire-schema version of the self-contained room invite payload. Bump on any
/// breaking change to [`build_room_invite_url`] / [`parse_room_invite`] and add
/// migration handling in the parser.
pub(super) const ROOM_INVITE_SCHEMA: u32 = 1;

/// URL-level freshness guard for a shared room invite (24h). The supernode's
/// own token TTL is authoritative; this just stops stale links from dialing.
const ROOM_INVITE_TTL_SECS: u64 = 24 * 60 * 60;

/// Decoded fields of a `conquerd://room#…` invite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoomInvitePayload {
    pub supernode_id: String,
    pub supernode_hint: String,
    pub room_id: String,
    pub room_name: String,
    pub room_type: String,
    pub invite_token: String,
    pub expires_at: u64,
    /// Space-tree proof-based admission (SPACE-MERKLE-DESIGN §4.4), each a JSON
    /// object as text; empty when the inviter didn't include one. Carried to the
    /// joiner, who forwards them on `SfuJoin` for the supernode to verify.
    pub space_root: String,
    pub space_proof: String,
    pub space_grant: String,
}

/// A pasted room invite awaiting its host supernode's WebSocket to connect.
#[derive(Debug, Clone)]
pub(super) struct RoomInviteEntry {
    pub room_id: String,
    pub room_name: String,
    pub room_type: String,
    pub invite_token: String,
    /// Space-tree parent node id (from the invite's inclusion proof) and the
    /// owning Space id (from its signed root). `""` for legacy/flat invites.
    pub parent_id: String,
    pub space_id: String,
}

/// Build a self-contained room invite URL: `conquerd://room#<base64url(JSON)>`.
///
/// Kept as a free function (separate from the `ConnectionManager` state) so the
/// wire format can be round-trip tested in isolation. See the golden field test
/// in `tests.rs`; any field rename here must update that test in lock-step.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_room_invite_url(
    supernode_id: &str,
    supernode_hint: &str,
    room_id: &str,
    room_name: &str,
    room_type: &str,
    invite_token: &str,
    expires_at: u64,
    space_root: &str,
    space_proof: &str,
    space_grant: &str,
) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let mut payload = serde_json::json!({
        "v": ROOM_INVITE_SCHEMA,
        "supernode_id": supernode_id,
        "supernode_hint": supernode_hint,
        "room_id": room_id,
        "room_name": room_name,
        "room_type": room_type,
        "invite_token": invite_token,
        "expires_at": expires_at,
    });
    // Embed the Space fields as nested JSON objects (not strings) when present,
    // so the joiner deserializes them straight into the space types. The owner
    // signatures are over the struct fields, so a JSON round-trip is safe.
    if let Some(obj) = payload.as_object_mut() {
        for (key, text) in [
            ("space_root", space_root),
            ("space_proof", space_proof),
            ("space_grant", space_grant),
        ] {
            if !text.is_empty() {
                if let Ok(v) = serde_json::from_str::<Value>(text) {
                    obj.insert(key.to_owned(), v);
                }
            }
        }
    }
    let encoded = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
    format!("conquerd://room#{encoded}")
}

/// Parse the base64url fragment of a `conquerd://room#…` invite (the part after
/// `room#`). Returns an error string suitable for `emit_invite_failed`.
pub(super) fn parse_room_invite(encoded: &str) -> Result<RoomInvitePayload, String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    if encoded.len() > 262_144 {
        return Err(format!("room invite too large ({} bytes)", encoded.len()));
    }
    let json_bytes = URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .map_err(|e| format!("base64 decode error: {e}"))?;
    let payload: serde_json::Value =
        serde_json::from_slice(&json_bytes).map_err(|e| format!("JSON parse error: {e}"))?;

    // Unknown future schema: refuse rather than silently misinterpret.
    if let Some(v) = payload.get("v").and_then(Value::as_u64) {
        if v > ROOM_INVITE_SCHEMA as u64 {
            return Err(format!("unsupported room invite version {v}"));
        }
    }

    let get = |k: &str| {
        payload
            .get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let supernode_id = get("supernode_id");
    let room_id = get("room_id");
    if supernode_id.is_empty() {
        return Err("room invite missing supernode_id".into());
    }
    if room_id.is_empty() {
        return Err("room invite missing room_id".into());
    }
    // `room_type` is additive within v1; invites minted before it existed were
    // always private, so that's the back-compat default.
    let room_type = match payload.get("room_type").and_then(Value::as_str) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => "private".to_owned(),
    };
    // Space fields: extract the nested objects back to JSON text ("" = absent).
    let get_obj = |k: &str| {
        payload
            .get(k)
            .filter(|v| v.is_object())
            .map(|v| v.to_string())
            .unwrap_or_default()
    };
    Ok(RoomInvitePayload {
        supernode_id,
        supernode_hint: get("supernode_hint"),
        room_id,
        room_name: get("room_name"),
        room_type,
        invite_token: get("invite_token"),
        expires_at: payload
            .get("expires_at")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        space_root: get_obj("space_root"),
        space_proof: get_obj("space_proof"),
        space_grant: get_obj("space_grant"),
    })
}

fn saved_quic_port_path() -> std::path::PathBuf {
    Identity::default_key_dir().join(QUIC_PORT_FILE)
}

fn load_saved_quic_port() -> Option<u16> {
    std::fs::read_to_string(saved_quic_port_path())
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

fn save_quic_port(port: u16) {
    let path = saved_quic_port_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("Could not create QUIC port state directory: {e}");
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, port.to_string()) {
        warn!(
            "Could not persist QUIC listener port to {}: {e}",
            path.display()
        );
    }
}

fn load_direct_p2p_settings() -> (bool, u16) {
    let path = Identity::default_key_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (
            true,
            load_saved_quic_port().unwrap_or(DEFAULT_QUIC_LISTENER_PORT),
        );
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (
            true,
            load_saved_quic_port().unwrap_or(DEFAULT_QUIC_LISTENER_PORT),
        );
    };
    let enabled = value
        .get("direct_p2p_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let port = load_saved_quic_port()
        .or_else(|| {
            value
                .get("direct_p2p_port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
        })
        .unwrap_or(DEFAULT_QUIC_LISTENER_PORT);
    (enabled, port)
}

pub(super) fn peer_quic_endpoint(record: &crate::peer_store::PeerRecord) -> Option<(String, u16)> {
    record
        .relay_hints
        .iter()
        .find_map(|hint| parse_quic_lan_hint(hint))
        .or_else(|| (record.quic_port != 0).then(|| ("127.0.0.1".to_owned(), record.quic_port)))
}

/// Parameters for `send_room_create`, bundled into one struct to keep the
/// function under clippy's argument-count lint — every field maps 1:1 to a
/// `SfuRoomCreate` wire field or a client-only replay/materialize flag, so
/// there is no natural way to shrink the field count further.
struct RoomCreateRequest<'a> {
    supernode_id: &'a str,
    room_name: &'a str,
    room_type: &'a str,
    room_id: Option<&'a str>,
    creator_id: Option<&'a str>,
    materialize_only: bool,
    invite_policy: &'a str,
}

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
    quic_peer_aliases: HashMap<String, String>,
    supernodes: HashMap<String, SupernodeSession>,
    /// Verified cluster siblings of each connected supernode (supernode id →
    /// sibling members), learned from the signed roster in `SUPERNODE_INFO`.
    /// Used as failover attach points when a supernode becomes unreachable.
    cluster_members: HashMap<String, Vec<crate::cluster::ClusterMember>>,
    /// Sibling sessions we opened for failover, awaiting connect to replay a
    /// room join: sibling supernode id → room id.
    pending_failover_rejoin: HashMap<String, String>,
    /// Supernodes we've already initiated cluster failover away from, so the
    /// per-retry `WsDisconnected` storm doesn't spawn duplicate attempts.
    /// Cleared when that supernode reconnects.
    failover_in_progress: HashSet<String>,

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
    /// WS Ping/Pong RTT trackers keyed by supernode identity pubkey.
    supernode_ping: HashMap<String, SupernodePingTracker>,
    /// `supernode_id:room_id` keys for in-flight materialize-only creates.
    /// `SfuRoomCreated` must not auto-join these rooms.
    pending_materialize: HashSet<String>,
    /// `supernode_id:room_id` keys waiting for private-room invite validation
    /// before sending the count-producing `SfuJoin`.
    pending_private_room_joins: HashSet<String>,
    /// Pasted room invites (`conquerd://room#…`) whose host supernode is still
    /// connecting. Keyed by supernode identity_pub; drained on `WsConnected`
    /// to emit [`ConnectionEvent::RoomInviteReady`] once the link is up.
    pending_room_invite_entries: HashMap<String, RoomInviteEntry>,
    /// Consecutive room-audio relay-datagram send failures. After a few in a
    /// row we stop trying the relay each frame and use WS for a cooldown.
    room_relay_fail_streak: u32,
    /// Remaining frames to send room audio over WS before re-trying the relay.
    /// Avoids per-frame relay/WS thrashing when the relay path is unhealthy.
    room_relay_cooldown_frames: u32,
    /// Sender handed to each [`QuicRelayClient`] so inbound `room.audio.sfu`
    /// datagrams (signed `SfuAudio` JSON) are re-injected on the normal
    /// inbound path. Cloned per relay connection.
    relay_signaling_tx: mpsc::UnboundedSender<RelaySignalingInbound>,
    /// Receiver side of [`Self::relay_signaling_tx`], polled in the run loop.
    relay_signaling_rx: mpsc::UnboundedReceiver<RelaySignalingInbound>,
    /// Sender-keys group keying for E2E room audio + room chat. The owner
    /// generates/rotates epoch keys and seals them to members over `SfuGroupKey`;
    /// members install received keys. See [`crate::group_key`].
    group_keys: SenderKeysGroup,
    /// Rooms we created (`supernode_id:room_id`) — we are the group-key owner for
    /// these and drive distribution/rotation from membership changes.
    created_rooms: HashSet<String>,
    /// Last-seen member set per owned room (`supernode_id:room_id` → member
    /// public_ids, excluding self), used to diff joins/leaves for rekeying.
    room_group_members: HashMap<String, HashSet<String>>,
    /// Monotonic per-send sequence for E2E room-audio frames, bound into the
    /// GCM AAD (`conv_id ‖ sender ‖ sequence`) and carried as the envelope
    /// `seq` field so the receiver can reconstruct the AAD.
    room_audio_seq: u64,
    /// Space proof-based admission creds carried by a pasted room invite, keyed
    /// by `room_id`, attached to the next `SfuJoin` for that room. JSON text
    /// `(space_root, space_proof, space_grant)`; `""` for any absent field.
    pending_join_space_creds: HashMap<String, (String, String, String)>,
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
        let (relay_signaling_tx, relay_signaling_rx) =
            mpsc::unbounded_channel::<RelaySignalingInbound>();

        let mgr = Self {
            identity,
            peer_store,
            event_tx,
            cmd_rx,
            peers: HashMap::new(),
            quic_peer_aliases: HashMap::new(),
            supernodes: HashMap::new(),
            cluster_members: HashMap::new(),
            pending_failover_rejoin: HashMap::new(),
            failover_in_progress: HashSet::new(),
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
            supernode_ping: HashMap::new(),
            pending_materialize: HashSet::new(),
            pending_private_room_joins: HashSet::new(),
            pending_room_invite_entries: HashMap::new(),
            relay_signaling_tx,
            relay_signaling_rx,
            room_relay_fail_streak: 0,
            room_relay_cooldown_frames: 0,
            group_keys: SenderKeysGroup::new(),
            created_rooms: HashSet::new(),
            room_group_members: HashMap::new(),
            room_audio_seq: 0,
            pending_join_space_creds: HashMap::new(),
        };
        (cmd_tx, event_rx, mgr.run_inner())
    }

    /// Lazily create the QUIC endpoint. Port 0 means the saved profile port,
    /// then the default. If that port is occupied, try consecutive ports.
    fn ensure_quic_endpoint(&mut self, port: u16) -> bool {
        if self.quic_endpoint.is_some() {
            return true;
        }
        let preferred = if port == 0 {
            load_saved_quic_port().unwrap_or(DEFAULT_QUIC_LISTENER_PORT)
        } else {
            port
        };
        let mut last_error = None;
        for offset in 0..QUIC_PORT_SEARCH_LIMIT {
            let Some(candidate) = preferred.checked_add(offset) else {
                break;
            };
            match quic_tls::make_quic_endpoint(self.identity.signing_key(), candidate) {
                Ok(ep) => {
                    info!(
                        "QUIC endpoint bound on {}",
                        ep.local_addr().map(|a| a.to_string()).unwrap_or_default()
                    );
                    save_quic_port(candidate);
                    self.quic_endpoint = Some(ep);
                    return true;
                }
                Err(e) => {
                    debug!("QUIC port {candidate} unavailable: {e}");
                    last_error = Some(e);
                }
            }
        }
        error!(
            "Failed to create QUIC endpoint starting at port {preferred}: {}",
            last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "no usable port in range".to_owned())
        );
        false
    }

    fn local_quic_hint(&self) -> Option<String> {
        let port = self.quic_endpoint.as_ref()?.local_addr().ok()?.port();
        let host = crate::platform::local_ip().unwrap_or_else(|| "127.0.0.1".to_owned());
        Some(format!("quic://{host}:{port}"))
    }

    // -- internal event loop -------------------------------------------------

    async fn run_inner(mut self) {
        info!("ConnectionManager started");

        // Every direct-P2P profile keeps a stable listener port when possible.
        // Multiple local profiles naturally occupy consecutive ports.
        let (direct_p2p_enabled, direct_p2p_port) = load_direct_p2p_settings();
        if direct_p2p_enabled {
            self.ensure_quic_endpoint(direct_p2p_port);
        } else {
            info!("Direct P2P listener disabled; using supernode connectivity");
        }

        // Reconnect trusted direct peers that were accepted with an endpoint.
        if direct_p2p_enabled {
            let direct_peers: Vec<(String, String, u16)> = {
                let store = self.peer_store.read();
                store
                    .auto_connect_peers()
                    .into_iter()
                    .filter(|record| !record.is_supernode)
                    .filter_map(|record| {
                        let (host, port) = peer_quic_endpoint(record)?;
                        Some((record.peer_id.clone(), host, port))
                    })
                    .collect()
            };
            for (peer_id, host, port) in direct_peers {
                self.connect_direct_quic(&peer_id, &host, port).await;
            }
        }

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
                        ConnectionCommand::ConfigureDirectP2p { enabled, port } => {
                            if let Some(endpoint) = self.quic_endpoint.take() {
                                endpoint.close(0u32.into(), b"listener reconfigured");
                            }
                            if enabled {
                                self.ensure_quic_endpoint(port);
                            } else {
                                info!("Direct P2P listener disabled by onboarding");
                            }
                        }
                        ConnectionCommand::JoinRoom { supernode_id, room_id } => {
                            self.current_supernode_id = supernode_id.clone();
                            self.current_room_id = room_id.clone();
                            self.send_room_join(&supernode_id, &room_id).await;
                            // Establish a QUIC relay session for low-latency room
                            // audio (datagrams instead of WS). Harmless if it
                            // never arrives — send_room_audio falls back to WS.
                            self.ensure_room_relay(&supernode_id).await;
                        }
                        ConnectionCommand::JoinRoomWithInvite { supernode_id, room_id, invite_token } => {
                            self.current_supernode_id = supernode_id.clone();
                            self.current_room_id = room_id.clone();
                            let key = format!("{supernode_id}:{room_id}");
                            self.pending_private_room_joins.insert(key);
                            self.send_room_invite(&supernode_id, &room_id, &invite_token).await;
                            self.ensure_room_relay(&supernode_id).await;
                        }
                        ConnectionCommand::LeaveRoom {
                            supernode_id,
                            room_id,
                        } => {
                            self.current_room_id.clear();
                            self.current_supernode_id.clear();
                            self.send_room_leave(&supernode_id, &room_id).await;
                        }
                        ConnectionCommand::RemoveSupernode { supernode_id } => {
                            self.remove_supernode(&supernode_id).await;
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
                        ConnectionCommand::AnnounceSpaceRoot { supernode_id, root_json } => {
                            self.send_space_root_announce(&supernode_id, &root_json).await;
                        }
                        ConnectionCommand::SendTyping { peer_id, is_typing } => {
                            self.send_typing(&peer_id, is_typing).await;
                        }
                        ConnectionCommand::SendSfuChat {
                            supernode_id,
                            room_id,
                            body,
                            sender_handle,
                            message_id,
                        } => {
                            self.send_sfu_chat(
                                &supernode_id,
                                &room_id,
                                &body,
                                &sender_handle,
                                &message_id,
                            )
                            .await;
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
                        ConnectionCommand::GenerateInvite { reply_tx } => {
                            let _ = reply_tx.send(self.generate_invite_url());
                        }
                        ConnectionCommand::GenerateRoomInvite {
                            supernode_id,
                            room_id,
                            room_name,
                            room_type,
                            invite_token,
                            space_root,
                            space_proof,
                            space_grant,
                            reply_tx,
                        } => {
                            let _ = reply_tx.send(self.generate_room_invite_url(
                                &supernode_id,
                                &room_id,
                                &room_name,
                                &room_type,
                                &invite_token,
                                &space_root,
                                &space_proof,
                                &space_grant,
                            ));
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
                        ConnectionCommand::CreateRoom {
                            supernode_id,
                            room_name,
                            room_type,
                            room_id,
                            creator_id,
                            materialize_only,
                            invite_policy,
                        } => {
                            self.send_room_create(RoomCreateRequest {
                                supernode_id: &supernode_id,
                                room_name: &room_name,
                                room_type: &room_type,
                                room_id: room_id.as_deref(),
                                creator_id: creator_id.as_deref(),
                                materialize_only,
                                invite_policy: &invite_policy,
                            })
                            .await;
                        }
                    }
                }
                // Internal events from QUIC and WS tasks
                Some(ev) = self.internal_rx.recv() => {
                    self.handle_internal_event(ev).await;
                }
                // Inbound signed signaling frames forwarded over a QUIC relay —
                // `SfuAudio` datagrams plus `room.chat.v1` / `room.file.v1`
                // frames from the reliable signaling stream. Re-inject each on
                // the normal inbound path so signature + freshness + quota +
                // dispatch run exactly as for the WebSocket route.
                Some(frame) = self.relay_signaling_rx.recv() => {
                    self.handle_relay_reinject(frame).await;
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
            // Per-peer transport stats are only collected for direct QUIC
            // sessions (see `transport_stats` insertion on QUIC connect);
            // relay-assisted peers are tracked separately in `quic_relays`
            // and never reach this loop, so a direct stats row is never relay.
            self.emit_connection_stats_row(peer_id, stats, false);
        }
        for (supernode_id, sn) in &self.supernodes {
            if !sn.connected {
                continue;
            }
            let Some(stats) = self.transport_stats.get(supernode_id) else {
                continue;
            };
            let relay = self
                .quic_relays
                .get(supernode_id)
                .is_some_and(|r| r.is_alive());
            self.emit_connection_stats_row(supernode_id, stats, relay);
        }
    }

    fn emit_connection_stats_row(&self, peer_id: &str, stats: &PeerTransportStats, relay: bool) {
        let payload = serde_json::json!({
            "peer_id": peer_id,
            "rtt_ms": stats.rtt_ms,
            "packet_loss_pct": stats.packet_loss_pct,
            "jitter_ms": stats.jitter_ms,
            "relay": relay,
            "bandwidth_kbps": stats.bandwidth_kbps,
        });
        let _ = self.event_tx.try_send(ConnectionEvent::ConnectionStats {
            peer_id: peer_id.to_owned(),
            json: payload.to_string(),
        });
    }

    // -- supernode WebSocket -------------------------------------------------

    async fn connect_supernode_ws(&mut self, peer_id: String, ws_url: String) {
        let identity = Arc::clone(&self.identity);
        let internal_tx = self.internal_tx.clone();
        let (send_tx, send_rx) = mpsc::channel::<WsMessage>(64);
        let peer_id_clone = peer_id.clone();
        let ws_url_clone = ws_url.clone();

        // Spawn a dedicated task for this supernode connection
        let ws_task = tokio::spawn(supernode_ws_task(
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
                ws_task,
            },
        );
    }

    /// Store the verified cluster siblings of `supernode_id` for failover. The
    /// roster has already been signature-checked against `supernode_id`.
    fn record_cluster_members(
        &mut self,
        supernode_id: &str,
        members: &[crate::cluster::ClusterMember],
    ) {
        if members.is_empty() {
            self.cluster_members.remove(supernode_id);
            return;
        }
        self.cluster_members
            .insert(supernode_id.to_owned(), members.to_vec());
        // Log the resolved failover attach points, reusing the same ws scheme as
        // the supernode we're connected to.
        let scheme = self
            .supernodes
            .get(supernode_id)
            .and_then(|sn| sn.ws_url.split("://").next())
            .unwrap_or("ws")
            .to_owned();
        let urls = self.cluster_failover_ws_urls(supernode_id, &scheme);
        debug!(
            "cluster: {} failover attach point(s) for supernode {}: {:?}",
            urls.len(),
            &supernode_id[..12.min(supernode_id.len())],
            urls
        );
    }

    /// Ordered WebSocket failover URLs for `supernode_id`'s verified siblings,
    /// excluding any sibling we already have a session with. Pure read of the
    /// stored, verified roster — the basis for live failover reconnection.
    fn cluster_failover_ws_urls(&self, supernode_id: &str, scheme: &str) -> Vec<String> {
        self.cluster_failover_targets(supernode_id, scheme)
            .into_iter()
            .map(|(_, url)| url)
            .collect()
    }

    /// Verified `(sibling_identity_pub, ws_url)` failover targets for
    /// `supernode_id`, excluding siblings we already have a session with.
    fn cluster_failover_targets(&self, supernode_id: &str, scheme: &str) -> Vec<(String, String)> {
        let Some(members) = self.cluster_members.get(supernode_id) else {
            return Vec::new();
        };
        members
            .iter()
            .map(|m| (m.identity_pub.trim_end_matches('=').to_owned(), m))
            .filter(|(id, _)| !self.supernodes.contains_key(id))
            .filter_map(|(id, m)| m.ws_url(scheme).map(|url| (id, url)))
            .collect()
    }

    /// When the supernode hosting our current room is lost, open a session to a
    /// verified cluster sibling and queue a room-join replay for when it
    /// connects. Guarded so the per-retry disconnect storm triggers this once.
    async fn maybe_failover_to_cluster(&mut self, lost_supernode: &str, room_id: &str) {
        if room_id.is_empty() || self.failover_in_progress.contains(lost_supernode) {
            return;
        }
        let scheme = self
            .supernodes
            .get(lost_supernode)
            .and_then(|sn| sn.ws_url.split("://").next())
            .unwrap_or("ws")
            .to_owned();
        let Some((sibling_id, ws_url)) = self
            .cluster_failover_targets(lost_supernode, &scheme)
            .into_iter()
            .next()
        else {
            return; // no verified sibling to fail over to
        };
        info!(
            "Cluster failover: supernode {} lost — attaching to sibling {} at {} to resume room",
            &lost_supernode[..12.min(lost_supernode.len())],
            &sibling_id[..12.min(sibling_id.len())],
            ws_url
        );
        self.failover_in_progress.insert(lost_supernode.to_owned());
        self.pending_failover_rejoin
            .insert(sibling_id.clone(), room_id.to_owned());
        self.connect_supernode_ws(sibling_id, ws_url).await;
    }

    /// Resolve a signaling `target` to a `supernodes` session key (`identity_pub`).
    fn resolve_supernode_ws_target(&self, target: &str) -> Option<String> {
        if self.supernodes.contains_key(target) {
            return Some(target.to_owned());
        }
        let canon = self
            .peer_store
            .read()
            .resolve_supernode_identity_pub(target)?;
        if self.supernodes.contains_key(&canon) {
            Some(canon)
        } else {
            None
        }
    }

    async fn remove_supernode(&mut self, supernode_id: &str) {
        if self.current_supernode_id == supernode_id {
            let room_id = self.current_room_id.clone();
            self.current_room_id.clear();
            self.current_supernode_id.clear();
            if !room_id.is_empty() {
                self.send_room_leave(supernode_id, &room_id).await;
            }
        }
        if let Some(sn) = self.supernodes.remove(supernode_id) {
            sn.ws_task.abort();
            let _ = sn.send_tx.try_send(WsMessage::Close(None));
        }
        self.quic_relays.remove(supernode_id);
        info!(
            "Supernode removed from trust store: {}",
            &supernode_id[..8.min(supernode_id.len())]
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

    /// Supernode-targeted room broadcast messages that may ride the reliable
    /// QUIC relay signaling stream (`room.chat.v1` / `room.file.v1`) instead of
    /// the WebSocket signaling path. `SfuAudio` is excluded — it rides the
    /// unreliable relay datagram path.
    fn is_relay_signaling_type(msg_type: &MessageType) -> bool {
        matches!(
            msg_type,
            MessageType::SfuChat
                | MessageType::SfuFileOffer
                | MessageType::SfuFileChunk
                | MessageType::SfuFileComplete
        )
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
            let direct_connected = self
                .peers
                .get(&peer_id)
                .map(|peer| peer.state == PeerConnectionState::Connected)
                .unwrap_or(false);
            if !direct_connected {
                // No direct QUIC peer session. Fall back to supernode relay
                // when a supernode WS session is connected: the supernode
                // forwards peer-targeted messages to the destination if it is
                // also connected there (see signaling.rs "Relay to target").
                // The recipient still verifies signature + replay + blocked
                // sender. For paired peers the relayed payload is wrapped in an
                // `EncryptedSignal` envelope (see `maybe_wrap_for_relay`), so the
                // supernode sees only opaque ciphertext + routing metadata; it
                // can neither read nor forge the 1:1 content.
                let relay_available = self.supernodes.values().any(|sn| sn.connected);
                if !relay_available {
                    warn!(
                        "No direct session or supernode relay for chat {} to {}",
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
                debug!(
                    "No direct session for chat {}; relaying to {} via supernode",
                    message_id,
                    &peer_id[..8.min(peer_id.len())]
                );
                // Fall through: quota gating + signing happen below, then the
                // supernode WS fallback route delivers the signed message.
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
                // room.chat.v1 covers SFU room text chat broadcast.
                MessageType::SfuChat => Some("room.chat.v1"),
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

        // For the supernode-relay fallback, wrap peer-targeted messages in a
        // signed `EncryptedSignal` envelope so the relaying supernode cannot
        // read the payload. The direct-QUIC lane (below) is already private and
        // always uses the plaintext `json`; `relay_json` is used only on the
        // supernode-WS relay routes. Falls back to plaintext when no pairwise
        // key is derivable (supernode target, not-yet-paired peer, broadcast).
        let relay_json = self
            .maybe_wrap_for_relay(&msg, &json)
            .unwrap_or_else(|| json.clone());

        // Route: QUIC direct > relay WS > supernode WS fallback
        if let Some(target) = &msg.target.clone() {
            // Clone the sender so we don't hold a borrow of `self.peers`
            // while emitting a failure event on `self.event_tx` below.
            let quic_sig_tx = self.peers.get(target).and_then(|peer| {
                if peer.state == PeerConnectionState::Connected {
                    peer.quic_sig_tx.clone()
                } else {
                    None
                }
            });
            if let Some(sig_tx) = quic_sig_tx {
                // Chat and file ride dedicated channel tags on the
                // QUIC peer stream instead of the pure (control)
                // signaling channel. Control messages stay untagged
                // (raw JSON) for backward compatibility — the inbound
                // classifier treats a leading `{` as control.
                let bytes = match Self::channel_tag_for(msg_type.clone()) {
                    Some(tag) => channel_frame::encode_frame(tag, json.as_bytes()),
                    None => json.as_bytes().to_vec(),
                };
                if sig_tx.try_send(bytes).is_ok() {
                    return;
                }
                // Peer-targeted chat: a full or closing QUIC channel should
                // not strand the message when a supernode relay path exists.
                if chat_attempt.is_some() && self.supernodes.values().any(|sn| sn.connected) {
                    debug!("QUIC signaling channel busy for chat; falling back to supernode relay");
                } else {
                    if let Some((peer_id, message_id)) = chat_attempt {
                        warn!(
                            "QUIC signaling channel unavailable for chat {} to {}",
                            message_id,
                            &peer_id[..8.min(peer_id.len())]
                        );
                        let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                            peer_id,
                            message_id,
                            reason: "connection busy".to_owned(),
                        });
                    }
                    return;
                }
            }
        }

        // Route supernode-targeted signaling to that supernode's WS session.
        // Without this, multi-supernode clients always hit the first connected
        // session — room creates/lists from SN-B would land on SN-A instead.
        if let Some(target) = msg.target.clone() {
            if let Some(sn_id) = self.resolve_supernode_ws_target(&target) {
                // Reliable room broadcasts (room.chat.v1 / room.file.v1) prefer
                // the QUIC relay signaling stream when a live relay session
                // exists — no TCP head-of-line blocking. Falls through to the
                // WebSocket route below if the stream is unavailable/backed up.
                if Self::is_relay_signaling_type(&msg_type) {
                    if let Some(relay) = self.quic_relays.get(&sn_id).filter(|r| r.is_alive()) {
                        if relay.send_signaling(json.as_bytes()) {
                            return;
                        }
                    }
                }
                match self.supernodes.get(&sn_id) {
                    Some(sn) if sn.connected => {
                        let _ = sn.send_tx.try_send(WsMessage::Text(json.clone()));
                        return;
                    }
                    _ => {
                        warn!(
                            "Supernode {} not connected; dropping {:?}",
                            &sn_id[..8.min(sn_id.len())],
                            msg_type
                        );
                        if let Some((peer_id, message_id)) = chat_attempt {
                            let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                                peer_id,
                                message_id,
                                reason: "supernode is offline".to_owned(),
                            });
                        }
                        return;
                    }
                }
            }
        }

        // Fall back: deliver via supernode WebSocket relay (legacy path for
        // untargeted or peer-targeted messages that missed QUIC).
        //
        // For peer-targeted chat we don't know *which* supernode the recipient
        // is connected to, so we fan the signed message out to every connected
        // supernode. Each one forwards it to the target only if that peer is
        // connected there (signaling.rs "Relay to target"); the rest drop it.
        // Inbound chat is idempotent (deduped by message_id), so multiple
        // copies arriving by different paths are shown exactly once.
        if chat_attempt.is_some() {
            let mut delivered_any = false;
            for sn in self.supernodes.values() {
                if sn.connected
                    && sn
                        .send_tx
                        .try_send(WsMessage::Text(relay_json.clone()))
                        .is_ok()
                {
                    delivered_any = true;
                }
            }
            if delivered_any {
                return;
            }
            warn!("No connected supernode accepted chat relay {:?}", msg_type);
            if let Some((peer_id, message_id)) = chat_attempt {
                let _ = self.event_tx.try_send(ConnectionEvent::ChatSendFailed {
                    peer_id,
                    message_id,
                    reason: "peer is offline".to_owned(),
                });
            }
            return;
        }

        // Non-chat messages that missed QUIC: first connected supernode that
        // accepts. Peer-targeted ones (e.g. file chunks, call control) ride the
        // encrypted `relay_json`; genuinely untargeted broadcasts fall back to
        // plaintext `json` (no pairwise key), which `relay_json` already equals.
        for sn in self.supernodes.values() {
            if sn.connected {
                // If this supernode's outbound queue is full or closed, try
                // the next connected supernode rather than dropping silently.
                if sn
                    .send_tx
                    .try_send(WsMessage::Text(relay_json.clone()))
                    .is_ok()
                {
                    return;
                }
            }
        }
        warn!("No connected path to deliver message {:?}", msg_type);
    }

    /// Wrap a signed, peer-targeted `inner` message in a signed
    /// `EncryptedSignal` envelope for the supernode-relay fallback, so the
    /// relaying supernode routes by envelope `target` only and never sees the
    /// payload. `inner_json` is the already-serialized plaintext form (reused to
    /// avoid re-serializing).
    ///
    /// Returns `None` (caller falls back to plaintext) when there is no pairwise
    /// key to use: the message is untargeted, the target is a supernode (the
    /// intended recipient), the peer is not yet in the local store, or `inner`
    /// is itself an envelope.
    fn maybe_wrap_for_relay(&self, inner: &SignalingMessage, inner_json: &str) -> Option<String> {
        if inner.msg_type == MessageType::EncryptedSignal {
            return None;
        }
        let target = inner.target.as_ref()?;
        let peer_identity_pub = {
            let store = self.peer_store.read();
            // Never encrypt toward a supernode — it is the recipient, not a relay.
            if store.is_supernode_id(target) {
                return None;
            }
            store
                .get(target)
                .or_else(|| store.get_by_identity(target))
                .map(|r| r.identity_pub.clone())?
        };
        if peer_identity_pub.is_empty() {
            return None;
        }
        let key = self
            .identity
            .derive_pairwise_relay_key(&peer_identity_pub)
            .ok()?;
        let ciphertext = crate::crypto::encrypt_blob(&key, inner_json.as_bytes()).ok()?;
        let ciphertext_b64 = crate::crypto::b64url_encode(&ciphertext);

        let mut env =
            SignalingMessage::new(MessageType::EncryptedSignal, self.identity.public_id());
        // Route by the same target the plaintext message would have used.
        env.target = inner.target.clone();
        env.payload
            .insert("ciphertext".to_owned(), Value::String(ciphertext_b64));
        let canonical = env.canonical_bytes().ok()?;
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        env.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(&sig));
        env.to_json().ok()
    }

    // -- E2E room group-key distribution (owner side) -----------------------

    /// Seal `inner` into an `EncryptedSignal` envelope addressed to `member_pub`
    /// (a room member's public_id, which *is* their Ed25519 identity key), using
    /// the deterministic pairwise key. Unlike [`Self::maybe_wrap_for_relay`] this
    /// does not consult the peer store, so it works for room members we have no
    /// prior relationship with. The supernode routes the envelope by `target` and
    /// never sees the sealed group key. Returns the signed envelope to dispatch.
    fn seal_signal_to_member(
        &self,
        inner: &SignalingMessage,
        member_pub: &str,
    ) -> Option<SignalingMessage> {
        let inner_json = inner.to_json().ok()?;
        let key = self.identity.derive_pairwise_relay_key(member_pub).ok()?;
        let ciphertext = crate::crypto::encrypt_blob(&key, inner_json.as_bytes()).ok()?;
        let ciphertext_b64 = crate::crypto::b64url_encode(&ciphertext);
        let mut env =
            SignalingMessage::new(MessageType::EncryptedSignal, self.identity.public_id());
        env.target = Some(member_pub.to_owned());
        env.payload
            .insert("ciphertext".to_owned(), Value::String(ciphertext_b64));
        Some(env)
    }

    /// Owner: seal the group key for `(room_id, epoch)` to each member and send
    /// it (inside an `EncryptedSignal` envelope) so the supernode forwards it
    /// blind. `members` must already exclude ourselves.
    async fn distribute_group_key(
        &mut self,
        room_id: &str,
        epoch: u8,
        key: &[u8; 32],
        members: &[String],
    ) {
        let sender = self.identity.public_id();
        let key_b64 = crate::crypto::b64url_encode(key);
        for member in members {
            let mut inner = SignalingMessage::new(MessageType::SfuGroupKey, sender.clone());
            inner
                .payload
                .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
            inner
                .payload
                .insert("epoch".to_owned(), Value::Number((epoch as u64).into()));
            inner
                .payload
                .insert("key".to_owned(), Value::String(key_b64.clone()));
            if let Some(env) = self.seal_signal_to_member(&inner, member) {
                self.dispatch_outbound(env).await;
            } else {
                warn!(
                    "[group-key] could not seal group key to {}",
                    &member[..8.min(member.len())]
                );
            }
        }
    }

    /// Owner: reconcile the group key against the current room membership.
    /// Generates the first epoch, rotates on any departure (forward secrecy /
    /// PCS), or seals the current epoch to newly-added members. No-op unless we
    /// own `room_id`. `members` is the authoritative set from the supernode.
    async fn owner_sync_group_key(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        members: &[String],
    ) {
        let room_key = format!("{supernode_id}:{room_id}");
        if !self.created_rooms.contains(&room_key) {
            return;
        }
        let me = self.identity.public_id();
        let new: HashSet<String> = members.iter().filter(|m| **m != me).cloned().collect();
        let old = self
            .room_group_members
            .get(&room_key)
            .cloned()
            .unwrap_or_default();
        let removed = old.difference(&new).count() > 0;
        let added: Vec<String> = new.difference(&old).cloned().collect();

        if !self.group_keys.has_key(room_id) {
            // First keying: generate epoch 0 and seal to everyone present.
            let (epoch, key) = self.group_keys.new_owner_epoch(room_id);
            let all: Vec<String> = new.iter().cloned().collect();
            self.distribute_group_key(room_id, epoch, &key, &all).await;
        } else if removed {
            // A member left → rotate for forward secrecy and reseal to the rest.
            let (epoch, key) = self.group_keys.rotate(room_id);
            let all: Vec<String> = new.iter().cloned().collect();
            self.distribute_group_key(room_id, epoch, &key, &all).await;
        } else if !added.is_empty() {
            // Pure join(s) → seal the current epoch to the newcomers only.
            let epoch = self.group_keys.current_epoch(room_id);
            if let Some(key) = self.group_keys.epoch_key(room_id, epoch) {
                self.distribute_group_key(room_id, epoch, &key, &added)
                    .await;
            }
        }
        self.room_group_members.insert(room_key, new);
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
        let Some(endpoint) = self.quic_endpoint.as_ref() else {
            error!("QUIC endpoint missing after ensure_quic_endpoint");
            return;
        };
        let endpoint = endpoint.clone();

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
            let actual_peer_id = connection
                .peer_identity()
                .and_then(|id| id.downcast::<Vec<rustls::pki_types::CertificateDer>>().ok())
                .and_then(|certs| certs.first().cloned())
                .and_then(|cert| quic_tls::cn_from_cert_der(&cert))
                .and_then(|cn| {
                    let pub_bytes = hex::decode(&cn).ok()?;
                    Some(quic_tls::peer_id_from_pub_bytes(&pub_bytes))
                });
            if actual_peer_id.as_deref() != Some(peer_id_owned.as_str()) {
                warn!(
                    "QUIC peer id mismatch for {addr}: expected {}, got {}; waiting for signed invite handshake",
                    &peer_id_owned[..8.min(peer_id_owned.len())],
                    actual_peer_id
                        .as_deref()
                        .map(|id| &id[..8.min(id.len())])
                        .unwrap_or("<none>")
                );
            }

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
                self.send_pending_invite_inits_for_peer(&peer_id).await;
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
                let peer_id = self.resolve_quic_peer_alias(&peer_id);
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
                let canonical_peer_id = self.resolve_quic_peer_alias(&peer_id);
                self.quic_peer_aliases.remove(&peer_id);
                self.transport_stats.remove(&canonical_peer_id);
                if let Some(conn) = self.peers.get_mut(&peer_id) {
                    conn.state = PeerConnectionState::Disconnected;
                    conn.quic_sig_tx = None;
                }
                if canonical_peer_id != peer_id {
                    if let Some(conn) = self.peers.get_mut(&canonical_peer_id) {
                        conn.state = PeerConnectionState::Disconnected;
                        conn.quic_sig_tx = None;
                    }
                }
                // Release inbound and outbound quota state so the next
                // connection starts with fresh token buckets.
                self.feature_registry.clear_peer_quotas(&canonical_peer_id);
                self.feature_registry
                    .clear_peer_outbound_quotas(&canonical_peer_id);
                // Release replay-window state for this peer.
                self.replay_guard.forget_peer(&canonical_peer_id);
                // Remove stale capability advertisement so a reconnecting
                // peer is forced to re-announce before invoking features.
                // Without this, entries accumulate for every connect/disconnect
                // cycle and the intersection check could honour capabilities
                // from a stale session.
                self.peer_capabilities.remove(&canonical_peer_id);
                info!(
                    "Peer {} QUIC disconnected",
                    &canonical_peer_id[..8.min(canonical_peer_id.len())]
                );
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::PeerDisconnected(canonical_peer_id));
            }
            InternalEvent::QuicSignalingData { peer_id, data } => {
                let canonical_peer_id = self.resolve_quic_peer_alias(&peer_id);
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
                                &canonical_peer_id,
                                opus_data.len(),
                            ) {
                                debug!(
                                    "[core.audio.opus] inbound quota exceeded for {}; dropping frame",
                                    &canonical_peer_id[..8.min(canonical_peer_id.len())]
                                );
                            } else {
                                let _ =
                                    self.event_tx
                                        .try_send(ConnectionEvent::DirectAudioReceived {
                                            peer_id: canonical_peer_id,
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
                                self.handle_inbound_from_quic(peer_id.clone(), msg).await;
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
                // This node came back — allow a future failover away from it.
                self.failover_in_progress.remove(&peer_id);
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::SupernodeConnected(peer_id.clone()));
                // Auto-request the SFU room list so the Rooms tab populates.
                self.send_room_list_request(&peer_id).await;
                // Request supernode info (portal URL, title) for the Nodes tab.
                self.send_supernode_info_request(&peer_id).await;
                // Tell the supernode our build attestation (reproducible build ID).
                self.send_build_attestation(&peer_id).await;
                // If we opened this session to fail over from a lost cluster
                // member, resume the room here now that it's connected. The
                // sibling already trusts us (client-auth was replicated) and has
                // the room ACL (room-grant replication).
                if let Some(room_id) = self.pending_failover_rejoin.remove(&peer_id) {
                    info!(
                        "Cluster failover: resuming room {} on sibling {}",
                        &room_id[..12.min(room_id.len())],
                        &peer_id[..12.min(peer_id.len())]
                    );
                    self.current_supernode_id = peer_id.clone();
                    self.current_room_id = room_id.clone();
                    self.send_room_join(&peer_id, &room_id).await;
                    self.ensure_room_relay(&peer_id).await;
                }
                // A pasted room invite was waiting on this supernode to connect —
                // now enter the room via the normal (token-validated) join path.
                if let Some(entry) = self.pending_room_invite_entries.remove(&peer_id) {
                    self.emit_room_invite_ready(&peer_id, &entry);
                }
            }
            InternalEvent::WsDisconnected { peer_id } => {
                if let Some(sn) = self.supernodes.get_mut(&peer_id) {
                    sn.connected = false;
                }
                self.transport_stats.remove(&peer_id);
                self.supernode_ping.remove(&peer_id);
                // If this supernode was hosting our current SFU room, tear down
                // local room tracking immediately. We cannot usefully send
                // SfuLeave over a dead link; the room is supernode-ephemeral.
                // Clearing here ensures subsequent SendRoomAudio / SFU ops
                // do not keep targeting the lost host while other supernodes
                // or direct sessions remain usable.
                // Capture the room this supernode was hosting before we clear it,
                // so we can replay it on a cluster sibling below.
                let lost_room = if self.current_supernode_id == peer_id {
                    let room = self.current_room_id.clone();
                    self.current_room_id.clear();
                    self.current_supernode_id.clear();
                    room
                } else {
                    String::new()
                };
                // Quota / replay / capability cleanup for the supernode id
                // (room.* features key quotas and replay on the supernode id
                // as "peer"/sender, just like direct peers use their id).
                // Must happen on WS disconnect paths for symmetry with
                // QuicDisconnected + the documented contract.
                self.feature_registry.clear_peer_quotas(&peer_id);
                self.feature_registry.clear_peer_outbound_quotas(&peer_id);
                self.replay_guard.forget_peer(&peer_id);
                self.peer_capabilities.remove(&peer_id);
                // The associated QUIC relay (if any) is likely also dead when
                // signaling is lost; drop the entry so next use re-discovers.
                self.quic_relays.remove(&peer_id);
                let _ = self
                    .event_tx
                    .try_send(ConnectionEvent::SupernodeDisconnected(peer_id.clone()));
                // Clustered supernode lost while hosting our room → fail over to a
                // verified sibling and resume there. No-op if not clustered.
                if !lost_room.is_empty() {
                    self.maybe_failover_to_cluster(&peer_id, &lost_room).await;
                }
            }
            InternalEvent::WsSignalingMessage { supernode_id, msg } => {
                self.handle_inbound_from_supernode(supernode_id, msg).await;
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
        let Some(endpoint) = self.quic_endpoint.as_ref() else {
            error!("[relay] QUIC endpoint missing after ensure_quic_endpoint");
            return;
        };
        let endpoint = endpoint.clone();
        let internal_tx = self.internal_tx.clone();
        let relay_signaling_tx = self.relay_signaling_tx.clone();
        let sn_id_for_task = supernode_id.clone();
        tokio::spawn(async move {
            let client = match QuicRelayClient::connect(
                &endpoint,
                sn_id_for_task.clone(),
                &relay_host,
                relay_port,
                relay_signaling_tx,
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

    /// Request a relay grant for `supernode_id` so room audio can ride QUIC
    /// datagrams. No-op when a live relay session already exists. The grant
    /// flow (`RelayGranted` → background connect) is best-effort; room audio
    /// transparently falls back to the WebSocket SFU path if it never lands.
    async fn ensure_room_relay(&mut self, supernode_id: &str) {
        if self
            .quic_relays
            .get(supernode_id)
            .is_some_and(|r| r.is_alive())
        {
            return;
        }
        self.request_relay(supernode_id).await;
    }

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
        // Attach Space proof-based admission creds carried by the invite we used
        // to reach this room (single-use), so the supernode can admit + materialize
        // it by proof on any cluster member. Absent → legacy ACL applies.
        if let Some((root, proof, grant)) = self.pending_join_space_creds.remove(room_id) {
            for (key, text) in [
                ("space_root", root),
                ("space_proof", proof),
                ("space_grant", grant),
            ] {
                if !text.is_empty() {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        msg.payload.insert(key.to_owned(), v);
                    }
                }
            }
        }
        self.dispatch_outbound(msg).await;
    }

    /// Announce a signed Space root to `supernode_id` (authenticated room-set
    /// sync). `root_json` is a serialized `SignedSpaceRoot`; the supernode
    /// verifies + stores + cluster-gossips it.
    async fn send_space_root_announce(&mut self, supernode_id: &str, root_json: &str) {
        let Ok(root) = serde_json::from_str::<Value>(root_json) else {
            return;
        };
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SpaceRootAnnounce, sender);
        msg.target = Some(supernode_id.to_owned());
        msg.payload.insert("root".to_owned(), root);
        self.dispatch_outbound(msg).await;
    }

    async fn send_room_leave(&mut self, supernode_id: &str, room_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuLeave, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("peer_id".to_owned(), Value::String(sender));
        let rid = if room_id.is_empty() {
            "default".to_owned()
        } else {
            room_id.to_owned()
        };
        msg.payload.insert("room_id".to_owned(), Value::String(rid));
        self.dispatch_outbound(msg).await;
    }

    async fn send_room_subscribe(&mut self, supernode_id: &str, room_id: &str) {
        // Establish a QUIC relay session (if not already up) so room chat/file
        // ride the reliable signaling stream rather than the WebSocket path —
        // even for chat-only rooms with no active voice. No-op if a live relay
        // already exists; room messaging still works over WS if the grant
        // never lands.
        self.ensure_room_relay(supernode_id).await;
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuSubscribe, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        self.dispatch_outbound(msg).await;
    }

    async fn send_room_invite(&mut self, supernode_id: &str, room_id: &str, invite_token: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuRoomInvite, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload.insert(
            "invite_token".to_owned(),
            Value::String(invite_token.to_owned()),
        );
        self.dispatch_outbound(msg).await;
    }

    async fn send_room_create(&mut self, req: RoomCreateRequest<'_>) {
        let RoomCreateRequest {
            supernode_id,
            room_name,
            room_type,
            room_id,
            creator_id,
            materialize_only,
            invite_policy,
        } = req;
        let normalized = match room_type.trim().to_ascii_lowercase().as_str() {
            "private" => "private",
            _ => "public",
        };
        if materialize_only {
            if let Some(rid) = room_id.filter(|s| !s.is_empty()) {
                let key = format!("{supernode_id}:{rid}");
                self.pending_materialize.insert(key);
            }
        }
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuRoomCreate, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_name".to_owned(), Value::String(room_name.to_owned()));
        msg.payload
            .insert("room_type".to_owned(), Value::String(normalized.to_owned()));
        if let Some(rid) = room_id.filter(|s| !s.is_empty()) {
            msg.payload
                .insert("room_id".to_owned(), Value::String(rid.to_owned()));
        }
        if let Some(cid) = creator_id.filter(|s| !s.is_empty()) {
            msg.payload
                .insert("creator_id".to_owned(), Value::String(cid.to_owned()));
        }
        if !invite_policy.is_empty() {
            msg.payload.insert(
                "invite_policy".to_owned(),
                Value::String(invite_policy.to_owned()),
            );
        }
        info!(
            "[cm] SfuRoomCreate: supernode={} name={room_name} type={normalized} materialize_only={materialize_only}",
            &supernode_id[..8.min(supernode_id.len())]
        );
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

    /// Send a room audio frame to the supernode for SFU fan-out.
    ///
    /// Prefers an unreliable QUIC **relay datagram** when a live relay session
    /// to the room's supernode exists: datagrams avoid the TCP head-of-line
    /// blocking that dominates room-audio latency on the WebSocket path. The
    /// frame is the *same signed `SfuAudio` JSON* either way, so the receiver
    /// verifies the sender's Ed25519 signature identically and the supernode
    /// stays a dumb forwarder. Falls back to the WebSocket SFU path when no
    /// relay session is available or the datagram could not be sent.
    ///
    /// Outbound quota uses `room.audio.sfu` (gated against the supernode peer id).
    /// See `send_audio_datagram` for the direct P2P `core.audio.opus` path.
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

        // E2E-seal the Opus frame under the room's group key: the base64 `audio`
        // field carries `[epoch][nonce][aesgcm(opus)]` instead of plaintext
        // Opus, with `AAD = room_id ‖ sender ‖ seq`. The relay forwards the
        // signed envelope blind. With real sender keys, no key means we haven't
        // been keyed into the room yet (or are mid-rekey) — drop the frame
        // rather than leak plaintext; a 20 ms gap is inaudible and keying is
        // near-instant at join.
        let seq = self.room_audio_seq;
        let Some(sealed) = crate::group_key::seal_voice_frame(
            &self.group_keys,
            &room_id,
            &sender,
            seq,
            &opus_data,
        ) else {
            debug!("[room.audio.sfu] no group key for room yet; dropping frame");
            return;
        };
        self.room_audio_seq = self.room_audio_seq.wrapping_add(1);
        let audio_b64 = base64::engine::general_purpose::URL_SAFE.encode(&sealed);
        let mut msg = SignalingMessage::new(MessageType::SfuAudio, sender);
        msg.target = Some(supernode_id.clone());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id));
        msg.payload
            .insert("audio".to_owned(), Value::String(audio_b64));
        msg.payload.insert("e2e".to_owned(), Value::Bool(true));
        msg.payload
            .insert("seq".to_owned(), Value::Number(seq.into()));

        // Fast path: relay datagram (no TCP head-of-line blocking), unless we're
        // in a WS cooldown after repeated relay failures (anti-thrash). The Arc
        // clone drops the `self.quic_relays` borrow before we sign / fall back.
        let try_relay = self.room_relay_cooldown_frames == 0;
        if self.room_relay_cooldown_frames > 0 {
            self.room_relay_cooldown_frames -= 1;
        }
        let relay = if try_relay {
            self.quic_relays
                .get(&supernode_id)
                .filter(|r| r.is_alive())
                .cloned()
        } else {
            None
        };
        if let Some(relay) = relay {
            if let Some(json) = self.sign_message_json(&mut msg) {
                if relay.send_room_audio(json.as_bytes()) {
                    self.room_relay_fail_streak = 0;
                    return;
                }
            }
            // Relay path is unhealthy; after a short streak, prefer WS for a
            // ~3 s cooldown (≈150 frames at 50 fps) rather than retrying — and
            // probably failing — on every frame.
            self.room_relay_fail_streak += 1;
            if self.room_relay_fail_streak >= 5 {
                self.room_relay_cooldown_frames = 150;
                self.room_relay_fail_streak = 0;
                debug!("[room.audio.sfu] relay datagram unhealthy; using WS for ~3 s");
            }
        }
        // Fallback: WebSocket SFU relay (re-signs; deterministic Ed25519 yields
        // the identical signature, so this is safe even after the attempt above).
        self.dispatch_outbound(msg).await;
    }

    /// Sign `msg` in place (Ed25519 over its canonical bytes) and return the
    /// serialized JSON, mirroring the signing step in [`Self::dispatch_outbound`].
    /// Returns `None` if canonicalization or serialization fails.
    fn sign_message_json(&self, msg: &mut SignalingMessage) -> Option<String> {
        let canonical = msg.canonical_bytes().ok()?;
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        msg.to_json().ok()
    }

    /// Re-inject a signed signaling JSON received over the QUIC relay — either
    /// an `SfuAudio` datagram or a `room.chat.v1` / `room.file.v1` frame from
    /// the reliable signaling stream — on the normal inbound path (signature
    /// verification + replay/freshness + per-feature quota + dispatch all run
    /// exactly as for the WebSocket route).
    async fn handle_relay_reinject(&mut self, frame: RelaySignalingInbound) {
        let text = match std::str::from_utf8(&frame.json) {
            Ok(s) => s,
            Err(_) => return,
        };
        match SignalingMessage::from_json(text) {
            Ok(msg) => {
                self.handle_inbound_from_supernode(frame.supernode_id, msg)
                    .await
            }
            Err(e) => debug!("[relay] dropping malformed relay signaling frame: {e}"),
        }
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
        message_id: &str,
    ) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuChat, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        // E2E-seal the body under the room group key (`AAD = room_id ‖ sender ‖
        // message_id`), carrying `nonce ‖ aesgcm(body)` in `body` plus `e2e`/
        // `epoch`. If we have no key yet (race right after join), fall back to
        // cleartext so the message isn't lost — the receiver auto-detects.
        match crate::group_key::seal_chat_body(
            &self.group_keys,
            room_id,
            &sender,
            message_id,
            body.as_bytes(),
        ) {
            Some((epoch, sealed)) => {
                msg.payload.insert(
                    "body".to_owned(),
                    Value::String(crate::crypto::b64url_encode(&sealed)),
                );
                msg.payload.insert("e2e".to_owned(), Value::Bool(true));
                msg.payload
                    .insert("epoch".to_owned(), Value::Number((epoch as u64).into()));
            }
            None => {
                warn!("[room.chat] no group key for room yet; sending cleartext body");
                msg.payload
                    .insert("body".to_owned(), Value::String(body.to_owned()));
            }
        }
        msg.payload.insert(
            "sender_handle".to_owned(),
            Value::String(sender_handle.to_owned()),
        );
        if !message_id.is_empty() {
            msg.payload.insert(
                "message_id".to_owned(),
                Value::String(message_id.to_owned()),
            );
        }
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
                    self.supernode_ping
                        .entry(sn.peer_id.clone())
                        .or_default()
                        .note_ping_sent();
                    let _ = sn.send_tx.try_send(WsMessage::Text(json.clone()));
                }
            }
        }
    }

    fn record_supernode_pong(&mut self, supernode_id: &str) {
        if !self.supernodes.contains_key(supernode_id) {
            return;
        }
        let Some(stats) = self
            .supernode_ping
            .entry(supernode_id.to_owned())
            .or_default()
            .note_pong()
        else {
            return;
        };
        self.transport_stats.insert(supernode_id.to_owned(), stats);
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

        if !msg.is_fresh(Self::MAX_MESSAGE_AGE_SECS) {
            warn!(
                "[signaling] dropping {:?} from {} — stale or future timestamp",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return false;
        }

        true
    }

    /// Test hook for signature + freshness verification on the client path.
    #[cfg(test)]
    pub(crate) fn verify_inbound_signature_for_test(msg: &SignalingMessage) -> bool {
        Self::verify_inbound_signature(msg)
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

    /// Returns `true` when `sender` (a base64url `public_id`, as it appears in
    /// `msg.sender`) resolves to a peer we mutually trust: present in the local
    /// trust store and neither `revoked` nor `blocked`.
    ///
    /// Peer records may be keyed by the hex `peer_id` *or* by the base64url
    /// `identity_pub` depending on whether the originating invite carried an
    /// explicit `inviter_peer_id`, so we probe by both `get` (hex key) and
    /// `get_by_identity` (base64url field) to resolve reliably.
    ///
    /// This is what makes supernode-assisted chat safe: a supernode will relay
    /// any peer-targeted message, but a receiver only honours chat/call
    /// signaling from peers it already trusts. Two mutually-trusted peers can
    /// therefore fall back to relay when no direct P2P path exists, while an
    /// untrusted peer that merely shares a supernode cannot inject signaling.
    pub(crate) fn is_trusted_sender(peer_store: &Arc<RwLock<PeerStore>>, sender: &str) -> bool {
        let store = peer_store.read();
        store
            .get(sender)
            .or_else(|| store.get_by_identity(sender))
            .is_some_and(|rec| !rec.blocked && !rec.revoked)
    }

    fn canonical_peer_id_for_sender(&self, sender: &str) -> String {
        let store = self.peer_store.read();
        store
            .get(sender)
            .or_else(|| store.get_by_identity(sender))
            .map(|rec| rec.peer_id.clone())
            .unwrap_or_else(|| sender.to_owned())
    }

    fn resolve_quic_peer_alias(&self, peer_id: &str) -> String {
        self.quic_peer_aliases
            .get(peer_id)
            .cloned()
            .unwrap_or_else(|| peer_id.to_owned())
    }

    fn relabel_quic_peer_session(&mut self, current_peer_id: &str, canonical_peer_id: &str) {
        if current_peer_id == canonical_peer_id || canonical_peer_id.is_empty() {
            return;
        }
        if canonical_peer_id == self.identity.peer_id() {
            warn!(
                "Ignoring QUIC relabel from {} to local peer id {}",
                &current_peer_id[..8.min(current_peer_id.len())],
                &canonical_peer_id[..8.min(canonical_peer_id.len())]
            );
            return;
        }

        let Some(mut provisional) = self.peers.remove(current_peer_id) else {
            return;
        };
        provisional.peer_id = canonical_peer_id.to_owned();
        let entry = self
            .peers
            .entry(canonical_peer_id.to_owned())
            .or_insert_with(|| PeerConnection::new(canonical_peer_id));
        entry.state = provisional.state;
        entry.quic_sig_tx = provisional.quic_sig_tx.take();
        entry.connected_at = provisional.connected_at;

        if let Some(stats) = self.transport_stats.remove(current_peer_id) {
            self.transport_stats
                .insert(canonical_peer_id.to_owned(), stats);
        }
        self.quic_peer_aliases
            .insert(current_peer_id.to_owned(), canonical_peer_id.to_owned());

        info!(
            "QUIC peer relabeled {} -> {} after signed invite handshake",
            &current_peer_id[..8.min(current_peer_id.len())],
            &canonical_peer_id[..8.min(canonical_peer_id.len())]
        );
    }

    async fn handle_inbound_from_quic(&mut self, transport_peer_id: String, msg: SignalingMessage) {
        self.handle_inbound_inner(msg, Some(transport_peer_id), None)
            .await;
    }

    async fn handle_inbound(&mut self, msg: SignalingMessage) {
        self.handle_inbound_inner(msg, None, None).await;
    }

    async fn handle_inbound_from_supernode(&mut self, supernode_id: String, msg: SignalingMessage) {
        self.handle_inbound_inner(msg, None, Some(supernode_id))
            .await;
    }

    async fn handle_inbound_inner(
        &mut self,
        msg: SignalingMessage,
        quic_peer_id: Option<String>,
        inbound_supernode_id: Option<String>,
    ) {
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
        // Positive mutual-trust gate for chat/call-class signaling. These
        // message types are only honoured from peers we already trust (present
        // in the local store, not revoked/blocked). This both closes the
        // blocked-peer hole and — crucially — bounds the supernode relay
        // fallback: a supernode forwards anything peer-targeted, so without a
        // receiver-side trust check an untrusted peer sharing the same
        // supernode could inject chat/call messages. With it, relay assist
        // works *only* between two mutually-trusted peers.
        if matches!(
            msg.msg_type,
            MessageType::ChatMessage
                | MessageType::ChatAck
                | MessageType::ChatTyping
                | MessageType::CallRequest
        ) && !Self::is_trusted_sender(&self.peer_store, &msg.sender)
        {
            debug!(
                "[signaling] dropping {:?} from untrusted or blocked peer {}",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        match msg.msg_type {
            // Supernode-relay E2E envelope: decrypt with the pairwise key derived
            // from our identity + the envelope sender's identity (`msg.sender`),
            // then re-dispatch the inner message through the full pipeline (its
            // own signature, freshness, replay, trust, and quota checks all run
            // again). Only the two paired peers can decrypt; a forged or foreign
            // envelope fails decryption and is dropped. The outer envelope has
            // already passed signature/freshness/replay above.
            MessageType::EncryptedSignal => {
                let Some(ciphertext_b64) = msg.payload.get("ciphertext").and_then(Value::as_str)
                else {
                    warn!(
                        "[signaling] EncryptedSignal from {} missing ciphertext — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                let key = match self.identity.derive_pairwise_relay_key(&msg.sender) {
                    Ok(k) => k,
                    Err(e) => {
                        warn!(
                            "[signaling] EncryptedSignal from {} — key derivation failed: {e}",
                            &msg.sender[..8.min(msg.sender.len())],
                        );
                        return;
                    }
                };
                let Ok(ciphertext) = crate::crypto::b64url_decode(ciphertext_b64) else {
                    warn!(
                        "[signaling] EncryptedSignal from {} — malformed ciphertext — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                let Ok(inner_bytes) = crate::crypto::decrypt_blob(&key, &ciphertext) else {
                    warn!(
                        "[signaling] EncryptedSignal from {} — could not decrypt (not a paired peer?) — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                let Some(inner) = std::str::from_utf8(&inner_bytes)
                    .ok()
                    .and_then(|s| SignalingMessage::from_json(s).ok())
                else {
                    warn!(
                        "[signaling] EncryptedSignal from {} — inner payload not a valid message — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                // Depth guard: a single layer only — never unwrap a nested envelope.
                if inner.msg_type == MessageType::EncryptedSignal {
                    warn!(
                        "[signaling] nested EncryptedSignal from {} — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                }
                // The envelope author must be the inner message's author; this
                // stops a paired peer from relaying a third party's signed
                // message wrapped under their own envelope.
                if inner.sender != msg.sender {
                    warn!(
                        "[signaling] EncryptedSignal inner/outer sender mismatch from {} — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                }
                Box::pin(self.handle_inbound_inner(inner, quic_peer_id, inbound_supernode_id))
                    .await;
            }
            MessageType::Pong => {
                debug!("Pong from {}", msg.sender);
                self.record_supernode_pong(&msg.sender);
            }
            MessageType::ChatMessage => {
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
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
                    ack.target = Some(sender_peer_id.clone());
                    ack.payload
                        .insert("message_id".to_string(), Value::String(msg_id.clone()));
                    self.dispatch_outbound(ack).await;
                }
                let _ = self.event_tx.try_send(ConnectionEvent::ChatMessage {
                    peer_id: sender_peer_id,
                    message_id: msg_id,
                    body,
                    timestamp: msg.timestamp,
                    sender_handle: handle,
                });
            }
            MessageType::ChatAck => {
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
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
                    peer_id: sender_peer_id,
                    message_id: msg_id,
                });
            }
            MessageType::CallRequest => {
                let _ = self.event_tx.try_send(ConnectionEvent::CallRequest {
                    peer_id: self.canonical_peer_id_for_sender(&msg.sender),
                });
            }
            MessageType::CallAccept => {
                let _ = self.event_tx.try_send(ConnectionEvent::CallAccepted {
                    peer_id: self.canonical_peer_id_for_sender(&msg.sender),
                });
            }
            MessageType::CallEnd | MessageType::CallReject => {
                let _ = self.event_tx.try_send(ConnectionEvent::CallEnded {
                    peer_id: self.canonical_peer_id_for_sender(&msg.sender),
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
                    peer_id: self.canonical_peer_id_for_sender(&msg.sender),
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
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                if !handle.is_empty() {
                    // Persist updated handle in peer store
                    let mut store = self.peer_store.write();
                    if let Some(rec) = store.get_mut(&sender_peer_id) {
                        rec.handle = handle.clone();
                    }
                    let _ = store.save();
                    drop(store);
                }
                let _ = self.event_tx.try_send(ConnectionEvent::HandleUpdated {
                    peer_id: sender_peer_id,
                    handle,
                });
            }
            MessageType::AvatarConfig => {
                if !Self::is_trusted_sender(&self.peer_store, &msg.sender) {
                    return;
                }
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                // Deserialize from the "config" sub-object in the payload.
                if let Some(cfg_val) = msg.payload.get("config") {
                    if let Ok(cfg) = serde_json::from_value::<PeerAvatarConfig>(cfg_val.clone()) {
                        let mut store = self.peer_store.write();
                        if let Some(rec) = store.get_mut(&sender_peer_id) {
                            rec.avatar_config = Some(cfg);
                        }
                        let _ = store.save();
                        drop(store);
                        let _ = self
                            .event_tx
                            .try_send(ConnectionEvent::AvatarConfigUpdated {
                                peer_id: sender_peer_id,
                            });
                    }
                }
            }
            MessageType::SfuGroupKey => {
                // A room group key sealed to us by the room owner. This arm only
                // runs after the outer `EncryptedSignal` was decrypted with our
                // pairwise key (see the `EncryptedSignal` handler), so the key
                // material never reached the supernode in the clear. We install
                // it keyed by `(room_id, epoch)`; a wrong/hostile key only breaks
                // our own decrypt (a DoS a room peer could already cause), not
                // confidentiality. Owner authenticity hardens with Space grants.
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let epoch = msg.payload.get("epoch").and_then(Value::as_u64);
                let key_b64 = msg.payload.get("key").and_then(Value::as_str);
                if let (false, Some(epoch), Some(key_b64)) = (room_id.is_empty(), epoch, key_b64) {
                    match crate::crypto::b64url_decode(key_b64) {
                        Ok(bytes) if bytes.len() == 32 => {
                            let mut key = [0u8; 32];
                            key.copy_from_slice(&bytes);
                            self.group_keys.install(room_id, epoch as u8, key);
                            debug!(
                                "[group-key] installed epoch {} for room {} from {}",
                                epoch,
                                &room_id[..8.min(room_id.len())],
                                &msg.sender[..8.min(msg.sender.len())]
                            );
                        }
                        _ => warn!(
                            "[group-key] malformed key from {}",
                            &msg.sender[..8.min(msg.sender.len())]
                        ),
                    }
                }
            }
            MessageType::SfuMembers => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
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
                // Owner: reconcile the room group key against this authoritative
                // member set (first keying / rotate-on-leave / seal-to-newcomer).
                self.owner_sync_group_key(&msg.sender, &room_id, &members)
                    .await;
                let _ = self.event_tx.try_send(ConnectionEvent::RoomMembersChanged {
                    supernode_id: msg.sender.clone(),
                    room_id,
                    members,
                });
            }
            MessageType::SfuPeerJoined => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let peer_id = msg
                    .payload
                    .get("peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                // Owner: seal the current epoch key to the newcomer.
                let room_key = format!("{}:{}", msg.sender, room_id);
                if self.created_rooms.contains(&room_key) {
                    let mut set = self
                        .room_group_members
                        .get(&room_key)
                        .cloned()
                        .unwrap_or_default();
                    set.insert(peer_id.clone());
                    let members: Vec<String> = set.into_iter().collect();
                    self.owner_sync_group_key(&msg.sender, &room_id, &members)
                        .await;
                }
                let _ = self.event_tx.try_send(ConnectionEvent::RoomPeerJoined {
                    supernode_id: msg.sender.clone(),
                    room_id,
                    peer_id,
                });
            }
            MessageType::SfuPeerLeft => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let peer_id = msg
                    .payload
                    .get("peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                // Owner: a departure → rotate the epoch key and reseal to those
                // who remain (forward secrecy + post-compromise security).
                let room_key = format!("{}:{}", msg.sender, room_id);
                if self.created_rooms.contains(&room_key) {
                    let mut set = self
                        .room_group_members
                        .get(&room_key)
                        .cloned()
                        .unwrap_or_default();
                    set.remove(&peer_id);
                    let members: Vec<String> = set.into_iter().collect();
                    self.owner_sync_group_key(&msg.sender, &room_id, &members)
                        .await;
                }
                let _ = self.event_tx.try_send(ConnectionEvent::RoomPeerLeft {
                    supernode_id: msg.sender.clone(),
                    room_id,
                    peer_id,
                });
            }
            MessageType::SfuChat => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let raw_body = msg
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
                let message_id = msg
                    .payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // E2E: when `e2e`, `body` is `b64(nonce ‖ aesgcm(body))` sealed
                // under the room group key with `AAD = room_id ‖ sender ‖
                // message_id`. Decrypt before surfacing; drop on failure. Absent
                // `e2e` → legacy cleartext (interop).
                let is_e2e = msg
                    .payload
                    .get("e2e")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let body = if is_e2e {
                    let epoch = msg
                        .payload
                        .get("epoch")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::MAX);
                    let sealed = match crate::crypto::b64url_decode(&raw_body) {
                        Ok(b) => b,
                        Err(_) => return,
                    };
                    let plaintext = epoch.try_into().ok().and_then(|e: u8| {
                        crate::group_key::open_chat_body(
                            &self.group_keys,
                            &room_id,
                            &msg.sender,
                            &message_id,
                            e,
                            &sealed,
                        )
                    });
                    match plaintext.and_then(|p| String::from_utf8(p).ok()) {
                        Some(s) => s,
                        None => {
                            debug!(
                                "[room.chat.v1] failed to open E2E body from {}; dropping",
                                &msg.sender[..8.min(msg.sender.len())]
                            );
                            return;
                        }
                    }
                } else {
                    raw_body
                };
                if !body.is_empty() {
                    // Enforce the room.chat.v1 per-sender inbound quota,
                    // symmetric with the outbound gate in dispatch_outbound
                    // and with room.audio.sfu / room.file.v1.
                    if !self.check_inbound_feature_quota(
                        "room.chat.v1",
                        &msg.sender,
                        body.len().max(64),
                    ) {
                        debug!(
                            "[room.chat.v1] inbound quota exceeded for {}; dropping message",
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                    let supernode_id = inbound_supernode_id
                        .or_else(|| msg.target.clone())
                        .unwrap_or_default();
                    let _ = self.event_tx.try_send(ConnectionEvent::RoomChatMessage {
                        supernode_id,
                        room_id,
                        sender_id: msg.sender.clone(),
                        sender_handle,
                        body,
                        timestamp: msg.timestamp,
                        message_id,
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
                let Ok(raw) = base64::engine::general_purpose::URL_SAFE.decode(audio_b64) else {
                    return;
                };
                if raw.is_empty() {
                    return;
                }
                // When the sender marked the frame E2E, `raw` is the sealed
                // `[epoch][nonce][aesgcm(opus)]`; open it under the room group
                // key, reconstructing `AAD = room_id ‖ sender ‖ seq` from the
                // (signature-authenticated) envelope. `e2e`/`seq` absent →
                // legacy cleartext Opus (interop / pre-E2E peers).
                let is_e2e = msg
                    .payload
                    .get("e2e")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let opus_data = if is_e2e {
                    let room_id = msg
                        .payload
                        .get("room_id")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let seq = msg
                        .payload
                        .get("seq")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::MAX);
                    match crate::group_key::open_voice_frame(
                        &self.group_keys,
                        room_id,
                        &msg.sender,
                        seq,
                        &raw,
                    ) {
                        Some(opus) => opus,
                        None => {
                            debug!(
                                "[room.audio.sfu] failed to open E2E frame from {}; dropping",
                                &msg.sender[..8.min(msg.sender.len())]
                            );
                            return;
                        }
                    }
                } else {
                    raw
                };
                if opus_data.is_empty() {
                    return;
                }
                if !self.check_inbound_feature_quota("room.audio.sfu", &msg.sender, opus_data.len())
                {
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
                    .or_else(|| msg.payload.get("app_url"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let title = msg
                    .payload
                    .get("title")
                    .or_else(|| msg.payload.get("node_title"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let sfu_enabled = msg
                    .payload
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .map(|caps| {
                        caps.iter().any(|c| {
                            c.get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id == "room.audio.sfu")
                        })
                    })
                    .unwrap_or(false);
                let public_rooms_enabled = msg
                    .payload
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .and_then(|caps| {
                        caps.iter().find(|c| {
                            c.get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id == "room.audio.sfu")
                        })
                    })
                    .and_then(|cap| cap.get("params"))
                    .and_then(|p| p.get("allow_public_rooms"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
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
                        sfu_enabled,
                        public_rooms_enabled,
                    });

                // Clustered supernode: parse + verify the signed sibling roster.
                // The signature must bind the roster to this supernode (which we
                // already trust), so a relay cannot inject bogus failover targets.
                if let Some(desc_val) = msg.payload.get("cluster") {
                    match serde_json::from_value::<crate::cluster::SignedClusterDescriptor>(
                        desc_val.clone(),
                    ) {
                        Ok(desc) => match desc.verified_members(&msg.sender) {
                            Some(members) => {
                                info!(
                                    "Supernode {} is in cluster '{}' with {} sibling member(s)",
                                    &msg.sender[..12.min(msg.sender.len())],
                                    desc.cluster_id,
                                    members.len()
                                );
                                self.record_cluster_members(&msg.sender, &members);
                            }
                            None => warn!(
                                "Ignoring cluster roster from {} — signature/signer check failed",
                                &msg.sender[..12.min(msg.sender.len())]
                            ),
                        },
                        Err(e) => debug!("Malformed cluster descriptor in SUPERNODE_INFO: {e}"),
                    }
                }
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
            MessageType::SfuRoomCreated => {
                if msg
                    .payload
                    .get("denied")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    let reason = msg
                        .payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("room_creation_denied");
                    let room_name = msg
                        .payload
                        .get("room_name")
                        .and_then(Value::as_str)
                        .unwrap_or("Room");
                    warn!(
                        "SFU room create denied by {} for '{room_name}': {reason}",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let room_name = msg
                    .payload
                    .get("room_name")
                    .and_then(Value::as_str)
                    .unwrap_or("Room")
                    .to_owned();
                let room_type = msg
                    .payload
                    .get("room_type")
                    .and_then(Value::as_str)
                    .unwrap_or("public")
                    .to_owned();
                let invite_token = msg
                    .payload
                    .get("invite_token")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if room_id.is_empty() {
                    warn!(
                        "SfuRoomCreated missing room_id from {}",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let supernode_id = msg.sender.clone();
                let materialize_key = format!("{supernode_id}:{room_id}");
                let materialize_only = self.pending_materialize.remove(&materialize_key);
                if materialize_only {
                    self.send_room_list_request(&supernode_id).await;
                } else {
                    self.current_supernode_id = supernode_id.clone();
                    self.current_room_id = room_id.clone();
                    // We created this room → we own its group key. Start a fresh
                    // epoch; it is sealed to members as they appear (SfuMembers).
                    let room_key = format!("{supernode_id}:{room_id}");
                    self.created_rooms.insert(room_key.clone());
                    self.room_group_members.remove(&room_key);
                    self.group_keys.forget(&room_id);
                    self.send_room_join(&supernode_id, &room_id).await;
                    // Join ack (SfuMembers) + supernode broadcast_room_list carry
                    // authoritative counts; an immediate list request can race and
                    // publish a pre-join participant_count to the sidebar bubble.
                    let _ = self.event_tx.try_send(ConnectionEvent::RoomCreated {
                        supernode_id,
                        room_id,
                        room_name,
                        room_type,
                        invite_token,
                    });
                }
            }
            MessageType::SfuRoomInviteResult => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let accepted = msg
                    .payload
                    .get("accepted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if room_id.is_empty() {
                    return;
                }
                let supernode_id = msg.sender.clone();
                let key = format!("{supernode_id}:{room_id}");
                if accepted {
                    let was_pending = self.pending_private_room_joins.remove(&key);
                    if was_pending {
                        self.current_supernode_id = supernode_id.clone();
                        self.current_room_id = room_id.clone();
                        self.send_room_join(&supernode_id, &room_id).await;
                        // Counts follow from SfuMembers + post-join broadcast; a
                        // list request here often lands before SfuJoin completes.
                    }
                } else {
                    self.pending_private_room_joins.remove(&key);
                    let reason = msg
                        .payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("invalid_token");
                    warn!(
                        "Private room invite rejected by {} for room {}: {}",
                        &supernode_id[..8.min(supernode_id.len())],
                        room_id,
                        reason
                    );
                }
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
                let joiner_quic_port = msg
                    .payload
                    .get("joiner_quic_port")
                    .and_then(Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok())
                    .unwrap_or(0);
                let joiner_lan_hint = msg
                    .payload
                    .get("joiner_lan_hint")
                    .and_then(Value::as_str)
                    .filter(|hint| parse_quic_lan_hint(hint).is_some())
                    .unwrap_or("")
                    .to_owned();
                if let Some(ref transport_peer_id) = quic_peer_id {
                    self.relabel_quic_peer_session(transport_peer_id, &joiner_peer_id);
                }
                info!(
                    "InviteHandshakeInit from {} (id={})",
                    &joiner_identity_pub[..8.min(joiner_identity_pub.len())],
                    &invite_id[..8.min(invite_id.len())]
                );

                // Add joiner to peer store
                {
                    let mut store = self.peer_store.write();
                    if let Some(record) = store.get_mut(&joiner_peer_id) {
                        record.last_seen_at = unix_now_f64();
                        record.auto_connect = true;
                        if joiner_quic_port != 0 {
                            record.quic_port = joiner_quic_port;
                        }
                        if !joiner_lan_hint.is_empty()
                            && !record.relay_hints.contains(&joiner_lan_hint)
                        {
                            record.relay_hints.push(joiner_lan_hint.clone());
                        }
                    } else {
                        store.upsert(crate::peer_store::PeerRecord {
                            peer_id: joiner_peer_id.clone(),
                            identity_pub: joiner_identity_pub.clone(),
                            handle: joiner_handle.clone(),
                            relay_hints: if joiner_lan_hint.is_empty() {
                                vec![]
                            } else {
                                vec![joiner_lan_hint.clone()]
                            },
                            auto_connect: true,
                            quic_port: joiner_quic_port,
                            created_at: unix_now_f64(),
                            last_seen_at: unix_now_f64(),
                            ..Default::default()
                        });
                    }
                    let _ = store.save();
                }

                // Send INVITE_HANDSHAKE_ACCEPT back
                let sender = self.identity.public_id();
                let peer_id_str = self.identity.peer_id();
                let mut reply =
                    SignalingMessage::new(MessageType::InviteHandshakeAccept, sender.clone());
                let direct_joiner_connected = self
                    .peers
                    .get(&joiner_peer_id)
                    .map(|peer| peer.state == PeerConnectionState::Connected)
                    .unwrap_or(false);
                reply.target = Some(if direct_joiner_connected {
                    joiner_peer_id.clone()
                } else {
                    joiner_identity_pub.clone()
                });
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
                    if inviter_identity_pub != pending.inviter_identity_pub
                        || inviter_peer_id != pending.inviter_peer_id
                    {
                        warn!(
                            "InviteHandshakeAccept identity mismatch for invite_id={invite_id}: expected {}/{}, got {}/{}",
                            &pending.inviter_identity_pub
                                [..8.min(pending.inviter_identity_pub.len())],
                            &pending.inviter_peer_id[..8.min(pending.inviter_peer_id.len())],
                            &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
                            &inviter_peer_id[..8.min(inviter_peer_id.len())],
                        );
                        return;
                    }
                    if let Some(ref transport_peer_id) = quic_peer_id {
                        self.relabel_quic_peer_session(transport_peer_id, &inviter_peer_id);
                    }
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
                        if pending.is_supernode && !pending.relay_hint.is_empty() {
                            store.grandfather_supernode_ws_hint_sharing(&pending.relay_hint);
                        }
                        store.upsert_from_invite(crate::peer_store::PeerRecord {
                            peer_id: inviter_peer_id.clone(),
                            identity_pub: inviter_identity_pub.clone(),
                            handle: inviter_handle.clone(),
                            relay_hints,
                            auto_connect: !pending.is_supernode,
                            quic_port: parse_quic_lan_hint(&pending.lan_hint)
                                .map(|(_, port)| port)
                                .unwrap_or(0),
                            is_supernode: pending.is_supernode,
                            supernode_from_invite: pending.is_supernode,
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

    fn generate_invite_url(&mut self) -> Option<String> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        if !self.ensure_quic_endpoint(0) {
            return None;
        }
        let port = self
            .quic_endpoint
            .as_ref()
            .and_then(|ep| ep.local_addr().ok())
            .map(|addr| addr.port())?;
        if port == 0 {
            return None;
        }

        let invite_id = uuid::Uuid::new_v4().to_string();
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + 900;
        let payload = serde_json::json!({
            "inviter_peer_id": self.identity.peer_id(),
            "inviter_identity_pub": self.identity.public_id(),
            "invite_id": invite_id,
            "expires_at": expires_at,
            "lan_hint": self.local_quic_hint()
                .unwrap_or_else(|| format!("quic://127.0.0.1:{port}")),
        });
        let encoded = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        Some(format!("conquerd://invite#{encoded}"))
    }

    /// Build a self-contained room invite URL for a room hosted on
    /// `supernode_id`. Returns `None` if we don't know a signaling address for
    /// that supernode (so we can fall back to sharing the bare token).
    #[allow(clippy::too_many_arguments)]
    fn generate_room_invite_url(
        &self,
        supernode_id: &str,
        room_id: &str,
        room_name: &str,
        room_type: &str,
        invite_token: &str,
        space_root: &str,
        space_proof: &str,
        space_grant: &str,
    ) -> Option<String> {
        if supernode_id.is_empty() || room_id.is_empty() {
            return None;
        }
        // Prefer the live session's ws_url; fall back to a persisted relay hint
        // (e.g. the room was created earlier this session but the socket churned).
        let supernode_hint = self
            .supernodes
            .get(supernode_id)
            .map(|sn| sn.ws_url.clone())
            .or_else(|| {
                self.peer_store
                    .read()
                    .get(supernode_id)
                    .and_then(|r| r.relay_hints.first().cloned())
            })
            .filter(|h| !h.is_empty())?;
        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            + ROOM_INVITE_TTL_SECS;
        Some(build_room_invite_url(
            supernode_id,
            &supernode_hint,
            room_id,
            room_name,
            room_type,
            invite_token,
            expires_at,
            space_root,
            space_proof,
            space_grant,
        ))
    }

    fn emit_invite_failed(&self, reason: impl Into<String>) {
        let reason = reason.into();
        warn!("AcceptInvite: {reason}");
        let _ = self
            .event_tx
            .try_send(ConnectionEvent::InviteFailed { reason });
    }

    fn build_invite_handshake_init(
        &self,
        pending: &PendingInvite,
        target: String,
    ) -> SignalingMessage {
        let sender = self.identity.public_id();
        let joiner_peer_id = self.identity.peer_id();
        let joiner_eph = crate::crypto::generate_ephemeral_keypair();
        let joiner_ephemeral_pub = crate::crypto::b64url_encode_nopad(joiner_eph.public.as_bytes());
        let joiner_quic_port = self
            .quic_endpoint
            .as_ref()
            .and_then(|ep| ep.local_addr().ok())
            .map(|addr| addr.port())
            .unwrap_or(0);

        let mut msg = SignalingMessage::new(MessageType::InviteHandshakeInit, sender.clone());
        msg.target = Some(target);
        msg.payload
            .insert("invite_id".into(), Value::String(pending.invite_id.clone()));
        msg.payload
            .insert("joiner_identity_pub".into(), Value::String(sender.clone()));
        msg.payload
            .insert("joiner_peer_id".into(), Value::String(joiner_peer_id));
        msg.payload.insert(
            "joiner_ephemeral_pub".into(),
            Value::String(joiner_ephemeral_pub),
        );
        msg.payload.insert(
            "joiner_quic_port".into(),
            Value::Number(joiner_quic_port.into()),
        );
        if let Some(hint) = self.local_quic_hint() {
            msg.payload
                .insert("joiner_lan_hint".into(), Value::String(hint));
        }
        msg
    }

    async fn send_pending_invite_inits_for_peer(&mut self, peer_id: &str) {
        let invite_ids: Vec<String> = self
            .pending_invites
            .iter()
            .filter(|(_, pending)| !pending.is_supernode && pending.inviter_peer_id == peer_id)
            .map(|(invite_id, _)| invite_id.clone())
            .collect();

        for invite_id in invite_ids {
            let Some(pending) = self.pending_invites.get(&invite_id) else {
                continue;
            };
            let msg = self.build_invite_handshake_init(pending, peer_id.to_owned());
            self.dispatch_outbound(msg).await;
        }
    }

    /// Accept a pasted self-contained room invite: connect to the embedded
    /// host supernode (if not already), then hand the room off to the UI to
    /// join. `encoded` is the base64url fragment after `room#`.
    async fn handle_accept_room_invite(&mut self, encoded: &str) {
        let payload = match parse_room_invite(encoded) {
            Ok(p) => p,
            Err(e) => {
                self.emit_invite_failed(e);
                return;
            }
        };

        if payload.expires_at != 0 {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if payload.expires_at < now {
                self.emit_invite_failed("room invite expired");
                return;
            }
        }

        let RoomInvitePayload {
            supernode_id,
            supernode_hint,
            room_id,
            room_name,
            room_type,
            invite_token,
            space_root,
            space_proof,
            space_grant,
            ..
        } = payload;

        // Pull the Space-tree linkage out of the proof/root before they're moved
        // into the pending join creds, so the joiner's sidebar can nest the room:
        // `parent_id` is the room's parent node in the owner's tree (a room id, or
        // "default"/the Server node for a top-level room); `space_id` names the
        // owning Space. Absent for legacy flat invites → "".
        let space_parent_id = serde_json::from_str::<Value>(&space_proof)
            .ok()
            .and_then(|v| {
                v.get("node")
                    .and_then(|n| n.get("parent_id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_default();
        let space_tree_id = serde_json::from_str::<Value>(&space_root)
            .ok()
            .and_then(|v| v.get("space_id").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default();

        // Stash any Space proof-based admission creds from the invite; they are
        // attached (single-use) to the SfuJoin for this room so the supernode can
        // admit + materialize it by proof on any cluster member.
        if !space_proof.is_empty() {
            self.pending_join_space_creds
                .insert(room_id.clone(), (space_root, space_proof, space_grant));
        }

        info!(
            "Accepting room invite for room {} on supernode {}",
            &room_id[..12.min(room_id.len())],
            &supernode_id[..8.min(supernode_id.len())]
        );

        // Persist the host supernode so the room-store join path (which resolves
        // the supernode via the peer store) can find it, and so it survives a
        // restart / shows in the Nodes tab. Mirrors the supernode-invite path.
        if !supernode_hint.is_empty() {
            let mut store = self.peer_store.write();
            store.grandfather_supernode_ws_hint_sharing(&supernode_hint);
            store.upsert_from_invite(crate::peer_store::PeerRecord {
                peer_id: supernode_id.clone(),
                identity_pub: supernode_id.clone(),
                relay_hints: vec![supernode_hint.clone()],
                is_supernode: true,
                supernode_from_invite: true,
                created_at: unix_now_f64(),
                last_seen_at: unix_now_f64(),
                ..Default::default()
            });
            let _ = store.save();
        }

        let entry = RoomInviteEntry {
            room_id,
            room_name,
            room_type,
            invite_token,
            parent_id: space_parent_id,
            space_id: space_tree_id,
        };

        let connected = self
            .supernodes
            .get(&supernode_id)
            .map(|sn| sn.connected)
            .unwrap_or(false);

        if connected {
            // Link is already up — enter the room immediately.
            self.emit_room_invite_ready(&supernode_id, &entry);
        } else {
            // Stash until WsConnected fires; open the session if we have no
            // task for this supernode yet.
            if !self.supernodes.contains_key(&supernode_id) {
                if supernode_hint.is_empty() {
                    self.emit_invite_failed("room invite missing supernode address");
                    return;
                }
                self.connect_supernode_ws(supernode_id.clone(), supernode_hint.clone())
                    .await;
            }
            self.pending_room_invite_entries.insert(supernode_id, entry);
        }
    }

    fn emit_room_invite_ready(&self, supernode_id: &str, entry: &RoomInviteEntry) {
        let _ = self.event_tx.try_send(ConnectionEvent::RoomInviteReady {
            supernode_id: supernode_id.to_owned(),
            room_id: entry.room_id.clone(),
            room_name: entry.room_name.clone(),
            room_type: entry.room_type.clone(),
            invite_token: entry.invite_token.clone(),
            parent_id: entry.parent_id.clone(),
            space_id: entry.space_id.clone(),
        });
    }

    async fn handle_accept_invite(&mut self, invite_url: String) {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        const SCHEME: &str = "conquerd://";
        let Some(rest) = invite_url.strip_prefix(SCHEME) else {
            self.emit_invite_failed(format!("invalid scheme in '{invite_url}'"));
            return;
        };

        // Invite URLs carry an optional `action#` prefix before the base64url
        // fragment: `conquerd://invite#<b64>`, `conquerd://room#<b64>`, or the
        // bare legacy `conquerd://<b64>`. Split it off so the payload decodes.
        let (action, encoded) = match rest.split_once('#') {
            Some((action, payload)) => (action, payload),
            None => ("", rest),
        };

        if action == "room" {
            self.handle_accept_room_invite(encoded).await;
            return;
        }

        if encoded.len() > 262_144 {
            self.emit_invite_failed(format!("invite URL too large ({} bytes)", encoded.len()));
            return;
        }

        let json_bytes = match URL_SAFE_NO_PAD.decode(encoded.trim_end_matches('=')) {
            Ok(b) => b,
            Err(e) => {
                self.emit_invite_failed(format!("base64 decode error: {e}"));
                return;
            }
        };

        let payload: serde_json::Value = match serde_json::from_slice(&json_bytes) {
            Ok(v) => v,
            Err(e) => {
                self.emit_invite_failed(format!("JSON parse error: {e}"));
                return;
            }
        };

        let inviter_identity_pub = match payload.get("inviter_identity_pub").and_then(Value::as_str)
        {
            Some(s) => s.to_owned(),
            None => {
                self.emit_invite_failed("missing inviter_identity_pub");
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
            .map(|s| s.to_owned())
            .unwrap_or_default();
        let lan_hint = payload
            .get("lan_hint")
            .and_then(Value::as_str)
            .map(|s| s.to_owned())
            .unwrap_or_default();
        let supernode_hint = if relay_hint.is_empty() {
            lan_hint.clone()
        } else {
            relay_hint.clone()
        };
        let is_supernode = payload
            .get("is_supernode")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let inviter_ephemeral_pub = payload
            .get("inviter_ephemeral_pub")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        if invite_id.is_empty() {
            self.emit_invite_failed("missing invite_id");
            return;
        }
        if inviter_identity_pub == self.identity.public_id() {
            self.emit_invite_failed("cannot use own invite");
            return;
        }

        if let Some(expires_at) = payload.get("expires_at").and_then(Value::as_i64) {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            if expires_at < now {
                self.emit_invite_failed("invite expired");
                return;
            }
        }

        let inviter_handle = payload
            .get("inviter_handle")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        info!(
            "Accepting invite from {} (id={})",
            &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
            &invite_id[..8.min(invite_id.len())]
        );

        // Supernode invites: trust + persist immediately from the signed URL
        // payload so the Rooms sidebar updates even if the WS handshake is slow
        // or the supernode no longer has this invite_id in its pending map.
        if is_supernode {
            {
                let mut store = self.peer_store.write();
                let relay_hints = if supernode_hint.is_empty() {
                    vec![]
                } else {
                    vec![supernode_hint.clone()]
                };
                if !supernode_hint.is_empty() {
                    store.grandfather_supernode_ws_hint_sharing(&supernode_hint);
                }
                store.upsert_from_invite(crate::peer_store::PeerRecord {
                    peer_id: inviter_peer_id.clone(),
                    identity_pub: inviter_identity_pub.clone(),
                    handle: inviter_handle.clone(),
                    relay_hints,
                    is_supernode: true,
                    supernode_from_invite: true,
                    created_at: unix_now_f64(),
                    last_seen_at: unix_now_f64(),
                    ..Default::default()
                });
                let _ = store.save();
            }
            let _ = self.event_tx.try_send(ConnectionEvent::InviteAccepted {
                peer_id: inviter_peer_id.clone(),
                handle: inviter_handle.clone(),
            });
        }

        // Store pending invite (matched when INVITE_HANDSHAKE_ACCEPT arrives)
        self.pending_invites.insert(
            invite_id.clone(),
            PendingInvite {
                inviter_peer_id: inviter_peer_id.clone(),
                inviter_identity_pub: inviter_identity_pub.clone(),
                invite_id: invite_id.clone(),
                relay_hint: supernode_hint.clone(),
                lan_hint: lan_hint.clone(),
                is_supernode,
                created_at: Instant::now(),
            },
        );

        // Open a signaling session only for supernode invites. Ordinary peers
        // may carry a ws relay hint for NAT traversal — that must not register
        // them in the Rooms sidebar or key a WS session under their identity.
        if is_supernode && !supernode_hint.is_empty() {
            if let Some(sn) = self.supernodes.remove(&inviter_identity_pub) {
                sn.ws_task.abort();
            }
            self.connect_supernode_ws(inviter_identity_pub.clone(), supernode_hint.clone())
                .await;
        }

        if !is_supernode {
            if inviter_ephemeral_pub.is_empty() {
                self.emit_invite_failed(
                    "invite missing inviter_ephemeral_pub; generate a fresh invite",
                );
                return;
            }
            if let Some((host, port)) = parse_quic_lan_hint(&lan_hint) {
                self.connect_direct_quic(&inviter_peer_id, &host, port)
                    .await;
            } else {
                self.emit_invite_failed(
                    "invite has no reachable local QUIC hint; generate a fresh invite",
                );
            }
            return;
        }

        // Build + sign INVITE_HANDSHAKE_INIT and queue directly on the WS send
        // channel (the message will be delivered once the WS connection is up).
        let sender = self.identity.public_id();
        let joiner_peer_id = self.identity.peer_id();
        let joiner_eph = crate::crypto::generate_ephemeral_keypair();
        let joiner_ephemeral_pub = crate::crypto::b64url_encode_nopad(joiner_eph.public.as_bytes());
        if inviter_ephemeral_pub.is_empty() {
            self.emit_invite_failed(
                "invite missing inviter_ephemeral_pub; generate a fresh invite",
            );
            return;
        }
        if let Err(e) = crate::crypto::derive_invite_session_key(
            &joiner_eph.secret,
            &inviter_ephemeral_pub,
            &invite_id,
            &inviter_identity_pub,
            &sender,
            &joiner_ephemeral_pub,
        ) {
            warn!("AcceptInvite: session key derivation failed: {e}");
        }
        let joiner_quic_port = self
            .quic_endpoint
            .as_ref()
            .and_then(|ep| ep.local_addr().ok())
            .map(|addr| addr.port())
            .unwrap_or(0);
        let mut msg = SignalingMessage::new(MessageType::InviteHandshakeInit, sender.clone());
        msg.target = Some(inviter_identity_pub.clone());
        msg.payload
            .insert("invite_id".into(), Value::String(invite_id));
        msg.payload
            .insert("joiner_identity_pub".into(), Value::String(sender.clone()));
        msg.payload
            .insert("joiner_peer_id".into(), Value::String(joiner_peer_id));
        msg.payload.insert(
            "joiner_ephemeral_pub".into(),
            Value::String(joiner_ephemeral_pub),
        );
        msg.payload.insert(
            "joiner_quic_port".into(),
            Value::Number(joiner_quic_port.into()),
        );
        if let Some(hint) = self.local_quic_hint() {
            msg.payload
                .insert("joiner_lan_hint".into(), Value::String(hint));
        }

        if let Ok(canonical) = msg.canonical_bytes() {
            let sig = self.identity.sign(&canonical);
            use base64::Engine;
            msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        }

        if let Ok(json) = msg.to_json() {
            if let Some(sn) = self.supernodes.get(&inviter_identity_pub) {
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
        let raw_data = msg
            .payload
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("");
        let probe = vec![0u8; raw_data.len()];
        if !self.gate_through_feature("room.file.v1", &msg.sender, &probe) {
            return;
        }
        // E2E: when `e2e`, `data` is `base64(nonce ‖ aesgcm(data))` sealed
        // under the room group key with `AAD = room_id ‖ sender ‖
        // transfer_id ‖ chunk_index`. Decrypt and re-encode as plain base64
        // (the format `FileTransferManager::on_chunk_received` expects)
        // before handing off; drop on failure. Absent `e2e` → legacy
        // cleartext (interop).
        let is_e2e = msg
            .payload
            .get("e2e")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let data_owned;
        let data: &str = if is_e2e {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD;
            let room_id = msg
                .payload
                .get("room_id")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let epoch = msg
                .payload
                .get("epoch")
                .and_then(Value::as_u64)
                .unwrap_or(u64::MAX);
            let Ok(sealed) = b64.decode(raw_data) else {
                return;
            };
            let plaintext = epoch.try_into().ok().and_then(|e: u8| {
                crate::group_key::open_file_chunk(
                    &self.group_keys,
                    room_id,
                    &msg.sender,
                    &tid,
                    idx as u64,
                    e,
                    &sealed,
                )
            });
            match plaintext {
                Some(p) => {
                    data_owned = b64.encode(p);
                    data_owned.as_str()
                }
                None => {
                    debug!(
                        "[room.file.v1] failed to open E2E chunk from {}; dropping",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
            }
        } else {
            raw_data
        };
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
                    // E2E-seal the chunk `data` under the room group key
                    // (`AAD = room_id ‖ sender ‖ transfer_id ‖ chunk_index`),
                    // mirroring `room.chat.v1` body sealing. Falls back to
                    // cleartext if no key is available yet (race right after
                    // join) so the transfer isn't lost — the receiver
                    // auto-detects via the `e2e` flag.
                    if room_msg_type == MessageType::SfuFileChunk {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD;
                        let transfer_id = payload
                            .get("transfer_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let chunk_index = payload
                            .get("chunk_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if let Some(raw) = payload
                            .get("data")
                            .and_then(Value::as_str)
                            .and_then(|s| b64.decode(s).ok())
                        {
                            match crate::group_key::seal_file_chunk(
                                &self.group_keys,
                                room_id,
                                &sender,
                                &transfer_id,
                                chunk_index,
                                &raw,
                            ) {
                                Some((epoch, sealed)) => {
                                    payload.insert(
                                        "data".to_owned(),
                                        Value::String(b64.encode(&sealed)),
                                    );
                                    payload.insert("e2e".to_owned(), Value::Bool(true));
                                    payload.insert(
                                        "epoch".to_owned(),
                                        Value::Number((epoch as u64).into()),
                                    );
                                }
                                None => {
                                    warn!(
                                        "[room.file.v1] no group key for room yet; sending cleartext chunk"
                                    );
                                }
                            }
                        }
                    }
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
