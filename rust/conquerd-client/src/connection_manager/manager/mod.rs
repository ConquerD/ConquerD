//! [`ConnectionManager`] implementation.
//!
//! Split across focused child modules (still one type / one `run_inner` loop):
//! - [`routing`] — outbound path pick, fan-out, relay wrap
//! - [`inbound`] — signed inbound dispatch + file transfer hooks
//! - [`room_session`] — rooms, SFU, group keys, cluster failover
//! - [`peer_session`] — direct QUIC, aliases, reconnect, direct audio
//! - [`invite`] — peer + room invite URLs and handshake

mod inbound;
mod invite;
mod peer_session;
mod room_session;
mod routing;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use conquerd_features::{
    channel_frame::{self, FrameClass},
    client_modules::register_client_modules,
    wellknown, CapabilityDescriptor, FeatureRegistry, ReplayGuard,
};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

use crate::feature_trust::FeatureTrustStore;
use crate::file_transfer::FileTransferManager;
use crate::group_key::SenderKeysGroup;
use crate::identity::Identity;
use crate::peer_store::PeerStore;
use crate::protocol::{MessageType, SignalingMessage};
use crate::quic_relay_client::{QuicRelayClient, RelayGameInbound, RelaySignalingInbound};
use crate::quic_tls;
use crate::web_app_client::{self, WebAppResponse};

use super::events::{ConnectionCommand, ConnectionEvent};
use super::internal::{
    InternalEvent, PeerConnection, PeerConnectionState, PeerOutbound, PeerTransportStats,
    PendingInvite, SupernodePingTracker, SupernodeSession, INVITE_TTL,
};
use super::ws::supernode_ws_task;

use crate::connection_fallback::{
    build_ws_candidates_from_hints, DirectFallbackCoordinator, PendingFallback,
};
use crate::session_state::{PeerSessionState, VoiceMode};

use peer_session::load_direct_p2p_settings;
use room_session::RoomCreateRequest;

// ---- shared constants -----------------------------------------------------

pub(super) const PING_INTERVAL_S: u64 = 30;
/// How often the elected keyer re-sends un-acked `SfuGroupKey` envelopes.
pub(super) const GROUP_KEY_RETRY_INTERVAL_MS: u64 = 750;
/// Stop resealing to a member after this many send attempts (incl. first).
pub(super) const GROUP_KEY_MAX_ATTEMPTS: u8 = 16;
/// How often we scan for due direct-QUIC peer reconnects.
pub(super) const PEER_RECONNECT_TICK_S: u64 = 1;
/// Cap on exponential backoff between direct-QUIC peer reconnect attempts.
pub(super) const PEER_RECONNECT_MAX_BACKOFF_S: u64 = 60;
/// After a callee accepts, how long the caller waits for a direct QUIC session
/// before falling back to a temporary private SFU room.
pub(super) const DIRECT_CALL_FALLBACK_GRACE_S: u64 = 5;
pub(super) const AUDIO_CHANNEL_TAG: u8 = channel_frame::AUDIO_TAG;
pub(super) const DEFAULT_QUIC_LISTENER_PORT: u16 = 61_045;
pub(super) const QUIC_PORT_SEARCH_LIMIT: u16 = 128;
pub(super) const QUIC_PORT_FILE: &str = "quic_listener_port";

pub(super) fn unix_now_f64() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ---- re-exports (tests import via `connection_manager::manager::…`) --------

pub use invite::{build_room_invite_url, parse_room_invite, RoomInvitePayload, ROOM_INVITE_SCHEMA};
pub use peer_session::{parse_quic_lan_hint, peer_quic_endpoint, peer_reconnect_backoff};
pub use room_session::{
    accept_group_key_epoch, is_elected_keyer, may_send_room_e2e_content, normalize_room_type,
    plan_cluster_failover, room_scope_key, should_auto_join_on_room_created,
    should_mint_first_room_key, should_track_pending_materialize, should_use_private_room_invite,
    union_members_for_room, FailoverPlan,
};
pub use routing::should_fanout_peer_relay;

pub use invite::ROOM_INVITE_TTL_SECS;

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
    /// Room we are resuming via a live-sibling fan-out and awaiting an
    /// authoritative `SfuMembers` ack for. The sibling that answers is the one
    /// that still holds the room, so it becomes `current_supernode_id`. Cleared
    /// on the first ack (or when the resume otherwise completes).
    failover_pending_room: Option<String>,

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
    /// Current SFU **voice** room identifier (empty when not in a voice room).
    /// Used for outbound room audio routing; independent of multi-room text chat.
    current_room_id: String,
    /// Supernode we joined the current voice room on (empty when not in a room).
    current_supernode_id: String,
    /// Rooms we want to keep receiving text chat for (`supernode_id:room_id`).
    /// Survives voice leave so private rooms (and any room we subscribed to)
    /// keep getting `SfuChat` while we voice elsewhere.
    chat_active_rooms: HashSet<String>,
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
    pending_room_invite_entries: HashMap<String, invite::RoomInviteEntry>,
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
    /// Sender for opaque portal `game.relay.v1` datagrams from the relay.
    relay_game_tx: mpsc::UnboundedSender<RelayGameInbound>,
    /// Receiver side of [`Self::relay_game_tx`], polled in the run loop.
    relay_game_rx: mpsc::UnboundedReceiver<RelayGameInbound>,
    /// Sender-keys group keying for E2E room audio + room chat. The room's
    /// elected keyer (see [`Self::sync_room_membership`]) generates/rotates
    /// epoch keys and seals them to members over `SfuGroupKey`; every member
    /// installs keys it receives. See [`crate::group_key`].
    group_keys: SenderKeysGroup,
    /// Last-seen member set per room we're in (`supernode_id:room_id` → member
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
    /// Outstanding group-key distributions awaiting `SfuGroupKeyAck` from the
    /// member. Keyed by `(room_id, member_public_id)`. Cleared on ACK, leave,
    /// or max attempts. The elected keyer reseals on a short timer until ACK.
    pending_group_key_acks: HashMap<(String, String), PendingGroupKeyAck>,
    /// Trusted peers we will re-dial over direct QUIC after a disconnect.
    /// Keyed by peer_id (or provisional transport id until relabel).
    pending_peer_reconnects: HashMap<String, peer_session::PendingPeerReconnect>,
    /// Direct-call → temporary private SFU room fallback state machine.
    direct_fallback: DirectFallbackCoordinator,
    /// Callee accepted our call but no direct QUIC session exists yet: peer →
    /// deadline after which [`Self::start_direct_call_fallback`] fires. Checked
    /// on the 1 s reconnect tick; cleared on QUIC connect or call end.
    pending_call_fallback_checks: HashMap<String, Instant>,
}

/// One in-flight seal of epoch key material to a room member.
#[derive(Debug, Clone)]
pub(super) struct PendingGroupKeyAck {
    pub(super) epoch: u8,
    pub(super) last_sent: std::time::Instant,
    pub(super) attempts: u8,
}

impl ConnectionManager {
    /// Freshness window for post-handshake signaling (seconds).
    pub(super) const MAX_MESSAGE_AGE_SECS: f64 = 300.0;

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
        let (cmd_tx, event_rx, mgr) = Self::construct(identity, peer_store, feature_registry);
        (cmd_tx, event_rx, mgr.run_inner())
    }

    /// Build a manager plus its command/event channel endpoints. Shared by
    /// [`Self::split_with_registry`] (which spawns `run_inner`) and the test
    /// harness (which drives the manager's methods directly).
    fn construct(
        identity: Arc<Identity>,
        peer_store: Arc<RwLock<PeerStore>>,
        feature_registry: Arc<FeatureRegistry>,
    ) -> (
        mpsc::Sender<ConnectionCommand>,
        mpsc::Receiver<ConnectionEvent>,
        Self,
    ) {
        let (event_tx, event_rx) = mpsc::channel::<ConnectionEvent>(256);
        let (cmd_tx, cmd_rx) = mpsc::channel::<ConnectionCommand>(64);
        let (internal_tx, internal_rx) = mpsc::channel::<InternalEvent>(128);
        let (relay_signaling_tx, relay_signaling_rx) =
            mpsc::unbounded_channel::<RelaySignalingInbound>();
        let (relay_game_tx, relay_game_rx) = mpsc::unbounded_channel::<RelayGameInbound>();

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
            failover_pending_room: None,
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
            chat_active_rooms: HashSet::new(),
            quic_relays: HashMap::new(),
            replay_guard: ReplayGuard::new(Self::MAX_MESSAGE_AGE_SECS),
            transport_stats: HashMap::new(),
            supernode_ping: HashMap::new(),
            pending_materialize: HashSet::new(),
            pending_private_room_joins: HashSet::new(),
            pending_room_invite_entries: HashMap::new(),
            relay_signaling_tx,
            relay_signaling_rx,
            relay_game_tx,
            relay_game_rx,
            room_relay_fail_streak: 0,
            room_relay_cooldown_frames: 0,
            group_keys: SenderKeysGroup::new(),
            room_group_members: HashMap::new(),
            room_audio_seq: 0,
            pending_join_space_creds: HashMap::new(),
            pending_group_key_acks: HashMap::new(),
            pending_peer_reconnects: HashMap::new(),
            direct_fallback: DirectFallbackCoordinator::new(),
            pending_call_fallback_checks: HashMap::new(),
        };
        (cmd_tx, event_rx, mgr)
    }

    /// Test-only constructor: returns the manager itself (no run loop) so
    /// tests can call its `pub(super)` methods directly, plus the app event
    /// receiver for asserting emissions. The command channel is dropped —
    /// tests drive the manager through method calls, not commands.
    #[cfg(test)]
    pub(super) fn new_for_test(
        identity: Arc<Identity>,
        peer_store: Arc<RwLock<PeerStore>>,
    ) -> (Self, mpsc::Receiver<ConnectionEvent>) {
        let feature_registry = Arc::new(FeatureRegistry::new());
        if let Err(e) = register_client_modules(&feature_registry) {
            panic!("failed to seed feature registry for test: {e}");
        }
        let (_cmd_tx, event_rx, mgr) = Self::construct(identity, peer_store, feature_registry);
        (mgr, event_rx)
    }

    /// Test-only: register a fake, already-connected supernode WS session and
    /// return the receiver side of its outbound queue, so tests can assert
    /// exactly which frames the manager routed to which supernode.
    #[cfg(test)]
    pub(super) fn test_add_supernode_session(
        &mut self,
        supernode_id: &str,
    ) -> mpsc::Receiver<WsMessage> {
        let (send_tx, send_rx) = mpsc::channel::<WsMessage>(64);
        self.supernodes.insert(
            supernode_id.to_owned(),
            SupernodeSession {
                peer_id: supernode_id.to_owned(),
                ws_url: "ws://test.invalid:34935".to_owned(),
                send_tx,
                connected: true,
                ws_task: tokio::spawn(async {}),
            },
        );
        send_rx
    }

    pub(super) async fn run_inner(mut self) {
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
                        let (host, port) = peer_session::peer_quic_endpoint(record)?;
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
            // Ordered, de-duplicated WS candidates (rotate on failure).
            let candidates = build_ws_candidates_from_hints(None, &hints);
            if candidates.is_empty() {
                warn!(
                    "Supernode {} has no relay hints — skipping WS connect",
                    &identity_pub[..8.min(identity_pub.len())]
                );
                continue;
            }
            self.connect_supernode_ws(identity_pub, candidates).await;
        }

        // Main event loop
        let mut ping_interval = tokio::time::interval(Duration::from_secs(PING_INTERVAL_S));
        let mut stats_interval = tokio::time::interval(Duration::from_secs(2));
        let mut group_key_retry =
            tokio::time::interval(Duration::from_millis(GROUP_KEY_RETRY_INTERVAL_MS));
        // Don't immediately fire a full retry storm on startup.
        group_key_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut peer_reconnect_interval =
            tokio::time::interval(Duration::from_secs(PEER_RECONNECT_TICK_S));
        peer_reconnect_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
                            // Manual dial resets automatic reconnect backoff.
                            self.cancel_peer_reconnect(&peer_id);
                            self.direct_fallback.cancel();
                            self.connect_direct_quic(&peer_id, &host, port).await;
                            self.emit_peer_session_state(&peer_id);
                        }
                        ConnectionCommand::StartDirectCallFallback { peer_id } => {
                            self.start_direct_call_fallback(&peer_id).await;
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
                                self.pending_peer_reconnects.clear();
                                info!("Direct P2P listener disabled by onboarding");
                            }
                        }
                        ConnectionCommand::JoinRoom { supernode_id, room_id } => {
                            self.current_supernode_id = supernode_id.clone();
                            self.current_room_id = room_id.clone();
                            // Voice join also receives room chat while present.
                            self.chat_active_rooms
                                .insert(room_scope_key(&supernode_id, &room_id));
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
                            self.chat_active_rooms.insert(key.clone());
                            self.pending_private_room_joins.insert(key);
                            self.send_room_invite(&supernode_id, &room_id, &invite_token).await;
                            self.ensure_room_relay(&supernode_id).await;
                        }
                        ConnectionCommand::LeaveRoom {
                            supernode_id,
                            room_id,
                        } => {
                            // Voice leave only — do not tear down multi-room text
                            // chat. Clear voice routing scope only when it matches
                            // the room being left.
                            if self.current_room_id == room_id
                                && self.current_supernode_id == supernode_id
                            {
                                self.current_room_id.clear();
                                self.current_supernode_id.clear();
                            }
                            let room_key = room_scope_key(&supernode_id, &room_id);
                            let keep_chat = self.chat_active_rooms.contains(&room_key);
                            if !keep_chat {
                                // Fully leaving this room's content surface.
                                self.group_keys.forget(&room_id);
                                self.pending_group_key_acks
                                    .retain(|(r, _), _| r != &room_id);
                                self.room_group_members.remove(&room_key);
                            }
                            self.send_room_leave(&supernode_id, &room_id).await;
                            // SfuLeave drops voice participation only; text chat
                            // requires an explicit subscriber entry once we are
                            // no longer a participant. Re-subscribe so private
                            // (and any chat-active) rooms keep receiving messages
                            // while we voice elsewhere.
                            if keep_chat {
                                self.send_room_subscribe(&supernode_id, &room_id).await;
                            }
                        }
                        ConnectionCommand::RemoveSupernode { supernode_id } => {
                            self.remove_supernode(&supernode_id).await;
                        }
                        ConnectionCommand::SubscribeRoomChat { supernode_id, room_id } => {
                            self.chat_active_rooms
                                .insert(room_scope_key(&supernode_id, &room_id));
                            self.send_room_subscribe(&supernode_id, &room_id).await;
                        }
                        ConnectionCommand::UnsubscribeRoomChat { supernode_id, room_id } => {
                            let room_key = room_scope_key(&supernode_id, &room_id);
                            self.chat_active_rooms.remove(&room_key);
                            // Drop keys only when we are not still voicing this room.
                            let still_voice = self.current_room_id == room_id
                                && self.current_supernode_id == supernode_id;
                            if !still_voice {
                                self.group_keys.forget(&room_id);
                                self.pending_group_key_acks
                                    .retain(|(r, _), _| r != &room_id);
                                self.room_group_members.remove(&room_key);
                            }
                            self.send_room_unsubscribe(&supernode_id, &room_id).await;
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
                                    self.emit_event(ConnectionEvent::FileOffered {
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
                            {
                                let mut store = self.peer_store.write();
                                if let Some(rec) = store.get_mut(&peer_id) {
                                    rec.blocked = true;
                                }
                                let _ = store.save();
                            }
                            self.cancel_peer_reconnect(&peer_id);
                            self.pending_call_fallback_checks.remove(&peer_id);
                            if self.direct_fallback.is_pending_for(&peer_id) {
                                self.direct_fallback.cancel();
                            }
                            // Drop any live direct session so we stop sending.
                            if let Some(conn) = self.peers.get_mut(&peer_id) {
                                conn.state = PeerConnectionState::Disconnected;
                                conn.quic_out_tx = None;
                            }
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
                            let size = data.len();
                            let old = self.file_mgr.get_old_data(&rel_path);
                            let old_ref: Option<&[u8]> = old.as_deref();
                            match self.file_mgr.offer_file(&peer_id, &rel_path, data, &purpose, old_ref, false) {
                                Ok((transfer_id, evs)) => {
                                    self.emit_event(ConnectionEvent::FileOffered {
                                        transfer_id,
                                        peer_id: peer_id.clone(),
                                        rel_path,
                                        size,
                                        purpose,
                                        is_self: true,
                                    });
                                    self.dispatch_transfer_events(evs).await;
                                }
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
                        ConnectionCommand::PortalGameOpen { supernode_id, room, reply_tx } => {
                            let result = self.handle_portal_game_open(&supernode_id, &room).await;
                            let _ = reply_tx.send(result);
                        }
                        ConnectionCommand::PortalGameSend { supernode_id, payload, reply_tx } => {
                            let result = self.handle_portal_game_send(&supernode_id, &payload);
                            let _ = reply_tx.send(result);
                        }
                        ConnectionCommand::PortalGameClose { supernode_id } => {
                            self.handle_portal_game_close(&supernode_id).await;
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
                        ConnectionCommand::BroadcastHandleUpdate { peer_id, handle } => {
                            self.send_handle_update_with(&peer_id, &handle).await;
                        }
                        ConnectionCommand::BroadcastHandleUpdateToAll { handle } => {
                            let connected: Vec<String> = self
                                .peers
                                .iter()
                                .filter(|(_, p)| p.state == PeerConnectionState::Connected)
                                .map(|(id, _)| id.clone())
                                .collect();
                            for peer_id in connected {
                                self.send_handle_update_with(&peer_id, &handle).await;
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
                            invite_token,
                        } => {
                            self.send_room_create(RoomCreateRequest {
                                supernode_id: &supernode_id,
                                room_name: &room_name,
                                room_type: &room_type,
                                room_id: room_id.as_deref(),
                                creator_id: creator_id.as_deref(),
                                materialize_only,
                                invite_policy: &invite_policy,
                                invite_token: &invite_token,
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
                Some(game) = self.relay_game_rx.recv() => {
                    self.emit_event(ConnectionEvent::PortalGameDatagram {
                        supernode_id: game.supernode_id,
                        payload: game.payload,
                    });
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
                _ = group_key_retry.tick() => {
                    self.retry_pending_group_keys().await;
                }
                _ = peer_reconnect_interval.tick() => {
                    self.tick_peer_reconnects().await;
                    self.tick_call_fallback_checks().await;
                }
            }
        }
        info!("ConnectionManager stopped");
    }

    /// Deliver an event to the app layer without blocking the manager loop.
    /// Drops (channel full) are counted in `drop_metrics::APP_EVENTS` and
    /// warn!-logged at power-of-two totals. Audio frames are best-effort by
    /// design and are counted but never logged.
    pub(super) fn emit_event(&self, event: ConnectionEvent) {
        use super::internal::drop_metrics;
        if let Err(mpsc::error::TrySendError::Full(dropped)) = self.event_tx.try_send(event) {
            let total = drop_metrics::note(&drop_metrics::APP_EVENTS);
            let is_audio = matches!(
                dropped,
                ConnectionEvent::SfuAudioReceived { .. }
                    | ConnectionEvent::DirectAudioReceived { .. }
            );
            if !is_audio && total.is_power_of_two() {
                warn!("[cm] app event channel full — {total} events dropped so far");
            }
        }
    }

    /// Count a failed `try_send` into a supernode WS outbound queue and log at
    /// power-of-two totals. Non-blocking by design: the manager must never
    /// await into its own WS tasks — they use awaited sends back into the
    /// manager, so a blocking send here would deadlock under mutual pressure.
    pub(super) fn note_ws_outbound_drop(&self, context: &str) {
        use super::internal::drop_metrics;
        let total = drop_metrics::note(&drop_metrics::WS_OUTBOUND);
        if total.is_power_of_two() {
            warn!(
                "[cm] supernode WS outbound queue full ({context}) — {total} frames dropped so far"
            );
        }
    }

    pub(super) fn emit_connection_stats(&self) {
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

    pub(super) fn emit_connection_stats_row(
        &self,
        peer_id: &str,
        stats: &PeerTransportStats,
        relay: bool,
    ) {
        let payload = serde_json::json!({
            "peer_id": peer_id,
            "rtt_ms": stats.rtt_ms,
            "packet_loss_pct": stats.packet_loss_pct,
            "jitter_ms": stats.jitter_ms,
            "relay": relay,
            "bandwidth_kbps": stats.bandwidth_kbps,
        });
        self.emit_event(ConnectionEvent::ConnectionStats {
            peer_id: peer_id.to_owned(),
            json: payload.to_string(),
        });
    }

    /// Open (or replace) a supernode WebSocket session. `candidates` is an
    /// ordered, de-duplicated URL list (see `build_ws_candidates`); the spawned
    /// task rotates through it on failure. The first entry is recorded as the
    /// session's primary `ws_url` (used for scheme/hints elsewhere).
    pub(super) async fn connect_supernode_ws(&mut self, peer_id: String, candidates: Vec<String>) {
        let Some(ws_url) = candidates.first().cloned() else {
            warn!(
                "Supernode {} has no WebSocket candidates — not connecting",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        };
        let identity = Arc::clone(&self.identity);
        let internal_tx = self.internal_tx.clone();
        let (send_tx, send_rx) = mpsc::channel::<WsMessage>(64);
        let peer_id_clone = peer_id.clone();

        // Spawn a dedicated task for this supernode connection
        let ws_task = tokio::spawn(supernode_ws_task(
            identity,
            peer_id_clone,
            candidates,
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

    pub(super) async fn remove_supernode(&mut self, supernode_id: &str) {
        if self.current_supernode_id == supernode_id {
            let room_id = self.current_room_id.clone();
            self.current_room_id.clear();
            self.current_supernode_id.clear();
            if !room_id.is_empty() {
                self.send_room_leave(supernode_id, &room_id).await;
            }
        }
        let prefix = format!("{supernode_id}:");
        self.chat_active_rooms.retain(|k| !k.starts_with(&prefix));
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

    pub(super) async fn handle_internal_event(&mut self, event: InternalEvent) {
        match event {
            // ── QUIC events ──────────────────────────────────────────────────────────────
            InternalEvent::QuicConnected { peer_id, out_tx } => {
                let entry = self
                    .peers
                    .entry(peer_id.clone())
                    .or_insert_with(|| PeerConnection::new(&peer_id));
                entry.state = PeerConnectionState::Connected;
                entry.quic_out_tx = Some(out_tx);
                entry.connected_at = Some(Instant::now());
                // Successful session clears reconnect backoff.
                self.cancel_peer_reconnect(&peer_id);
                info!("Peer {} QUIC connected", &peer_id[..8.min(peer_id.len())]);
                self.emit_event(ConnectionEvent::PeerConnected(peer_id.clone()));
                self.send_pending_invite_inits_for_peer(&peer_id).await;
                // Send capability announce to the newly-connected peer.
                self.send_capability_announce(&peer_id).await;
                // Also send build attestation so the peer knows our reproducible build ID.
                self.send_build_attestation(&peer_id).await;
                // Advertise our display handle so the peer list shows names even
                // when the original invite handshake stored an empty handle.
                self.send_handle_update(&peer_id).await;
                // Direct path recovered — a pending private-room call fallback
                // (or an armed grace-period check) for this peer is moot.
                if self.direct_fallback.is_pending_for(&peer_id) {
                    self.direct_fallback.cancel();
                }
                self.pending_call_fallback_checks.remove(&peer_id);
                self.emit_peer_session_state(&peer_id);
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
                    peer_id.clone(),
                    PeerTransportStats {
                        rtt_ms,
                        packet_loss_pct,
                        jitter_ms,
                        bandwidth_kbps,
                    },
                );
                self.emit_peer_session_state(&peer_id);
            }
            InternalEvent::QuicDisconnected { peer_id } => {
                let canonical_peer_id = self.resolve_quic_peer_alias(&peer_id);
                self.quic_peer_aliases.remove(&peer_id);
                self.transport_stats.remove(&canonical_peer_id);
                if let Some(conn) = self.peers.get_mut(&peer_id) {
                    conn.state = PeerConnectionState::Disconnected;
                    conn.quic_out_tx = None;
                }
                if canonical_peer_id != peer_id {
                    if let Some(conn) = self.peers.get_mut(&canonical_peer_id) {
                        conn.state = PeerConnectionState::Disconnected;
                        conn.quic_out_tx = None;
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
                self.emit_event(ConnectionEvent::PeerDisconnected(canonical_peer_id.clone()));
                // Re-dial trusted peers that still have a stored endpoint.
                self.schedule_peer_reconnect(&canonical_peer_id);
                if peer_id != canonical_peer_id {
                    self.pending_peer_reconnects.remove(&peer_id);
                }
                self.emit_peer_session_state(&canonical_peer_id);
            }
            InternalEvent::QuicSignalingData { peer_id, data } => {
                let canonical_peer_id = self.resolve_quic_peer_alias(&peer_id);
                // The QUIC peer stream multiplexes channels via a 1-byte
                // leading tag. Untagged frames are rejected.
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
                                self.emit_event(ConnectionEvent::DirectAudioReceived {
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
                        FrameClass::Chat(body) | FrameClass::File(body) | FrameClass::Control(body),
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
                self.emit_event(ConnectionEvent::SupernodeConnected(peer_id.clone()));
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
                    // Several siblings may have been armed for this room (all
                    // were down). This one won the race — disarm the rest so a
                    // later reconnect doesn't seize the room a second time, and
                    // cancel any live-sibling fan-out awaiting confirmation.
                    self.pending_failover_rejoin.retain(|_, r| *r != room_id);
                    if self.failover_pending_room.as_deref() == Some(room_id.as_str()) {
                        self.failover_pending_room = None;
                    }
                    self.current_supernode_id = peer_id.clone();
                    self.current_room_id = room_id.clone();
                    self.send_room_join(&peer_id, &room_id).await;
                    self.ensure_room_relay(&peer_id).await;
                    // Tell the UI the room moved to this member so it follows the
                    // failover instead of showing offline / no room.
                    self.emit_event(ConnectionEvent::RoomFailedOver {
                        supernode_id: peer_id.clone(),
                        room_id: room_id.clone(),
                    });
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
                self.emit_event(ConnectionEvent::SupernodeDisconnected(peer_id.clone()));
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

    /// Kick off a background `QuicRelayClient::connect` for `supernode_id`.
    /// On success the resulting handle is delivered via
    /// [`InternalEvent::RelayClientReady`] and cached in `self.quic_relays`.
    ///
    /// Called from the `RelayGranted` inbound handler — by the time we get
    /// here the supernode has already added our peer_id to its `allowed`
    /// set, so a plain mTLS handshake using our existing client cert is
    /// all that's required.
    pub(super) fn spawn_relay_client_connect(
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
        let relay_game_tx = self.relay_game_tx.clone();
        let sn_id_for_task = supernode_id.clone();
        tokio::spawn(async move {
            let client = match QuicRelayClient::connect(
                &endpoint,
                sn_id_for_task.clone(),
                &relay_host,
                relay_port,
                relay_signaling_tx,
                Some(relay_game_tx),
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

    /// Ensure relay + `GameRelayJoin` for a portal game lobby.
    pub(super) async fn handle_portal_game_open(
        &mut self,
        supernode_id: &str,
        room: &str,
    ) -> Result<(), String> {
        self.ensure_room_relay(supernode_id).await;
        // Wait briefly for the relay grant to land so the first datagrams
        // are not sent before game-session membership is useful.
        for _ in 0..20 {
            if self
                .quic_relays
                .get(supernode_id)
                .is_some_and(|r| r.is_alive())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::GameRelayJoin, sender);
        msg.target = Some(supernode_id.to_owned());
        msg.payload.insert(
            "room".to_owned(),
            Value::String(if room.is_empty() {
                "default".to_owned()
            } else {
                room.to_owned()
            }),
        );
        self.dispatch_outbound(msg).await;
        Ok(())
    }

    pub(super) fn handle_portal_game_send(
        &self,
        supernode_id: &str,
        payload: &[u8],
    ) -> Result<(), String> {
        let Some(relay) = self.quic_relays.get(supernode_id).filter(|r| r.is_alive()) else {
            return Err("no live QUIC relay to supernode".into());
        };
        if !self
            .feature_registry
            .gate_through_feature("game.relay.v1", supernode_id, payload.len())
        {
            return Err("game.relay.v1 outbound quota exceeded".into());
        }
        if relay.send_game_relay(payload) {
            Ok(())
        } else {
            Err("relay send_game_relay failed".into())
        }
    }

    pub(super) async fn handle_portal_game_close(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::GameRelayLeave, sender);
        msg.target = Some(supernode_id.to_owned());
        self.dispatch_outbound(msg).await;
    }

    /// Service a `FetchWebApp` command by opening a fresh QUIC bidi stream
    /// against the cached supernode relay and walking the `web.host.app.v1`
    /// wire protocol via [`web_app_client::fetch`].
    pub(super) async fn handle_fetch_web_app(
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

    /// Re-inject a signed signaling JSON received over the QUIC relay — either
    /// an `SfuAudio` datagram or a `room.chat.v1` / `room.file.v1` frame from
    /// the reliable signaling stream — on the normal inbound path (signature
    /// verification + replay/freshness + per-feature quota + dispatch all run
    /// exactly as for the WebSocket route).
    pub(super) async fn handle_relay_reinject(&mut self, frame: RelaySignalingInbound) {
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

    pub(super) async fn send_typing(&mut self, peer_id: &str, is_typing: bool) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::ChatTyping, sender);
        msg.target = Some(peer_id.to_owned());
        msg.payload
            .insert("typing".to_owned(), Value::Bool(is_typing));
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_capability_announce(&mut self, peer_id: &str) {
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
    pub(super) async fn send_build_attestation(&mut self, peer_id: &str) {
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

    /// Send our local display handle to a peer (`HandleUpdate`).
    pub(super) async fn send_handle_update(&mut self, peer_id: &str) {
        let handle = peer_session::read_local_display_handle();
        self.send_handle_update_with(peer_id, &handle).await;
    }

    pub(super) async fn send_handle_update_with(&mut self, peer_id: &str, handle: &str) {
        if handle.is_empty() {
            return;
        }
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::HandleUpdate, sender);
        msg.target = Some(peer_id.to_owned());
        msg.payload
            .insert("handle".to_owned(), Value::String(handle.to_owned()));
        self.dispatch_outbound(msg).await;
    }

    /// Broadcast our avatar config to a single trusted peer.
    ///
    /// Called after capability announce once the peer has a non-empty
    /// `transcript_hash` (meaning the Ed25519 handshake completed).
    /// The `config_json` string comes from `SettingsModel::avatar_config_json`.
    pub(super) async fn send_avatar_config(&mut self, peer_id: &str, config_json: &str) {
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

    pub(super) async fn send_capability_invoke(
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

    pub(super) async fn send_pings(&mut self) {
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

    pub(super) fn record_supernode_pong(&mut self, supernode_id: &str) {
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
}
