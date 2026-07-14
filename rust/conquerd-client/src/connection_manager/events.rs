//! Connection manager events and commands.

use serde_json::Value;
use std::sync::mpsc as std_mpsc;

use crate::protocol::SignalingMessage;
use crate::session_state::PeerSessionState;
use crate::web_app_client::WebAppResponse;
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
    ///
    /// When the caller could not open direct QUIC they may include a temporary
    /// private SFU room (`fallback_*`); the callee should join that room on
    /// accept instead of waiting for a P2P path.
    CallRequest {
        peer_id: String,
        fallback_supernode_id: String,
        fallback_room_id: String,
        fallback_invite_token: String,
    },
    /// Caller side of the direct-call fallback: the temporary private SFU room
    /// was created + joined and the invite was sent to `peer_id`. The local
    /// audio pipeline should switch to room mode on `supernode_id`/`room_id`
    /// instead of waiting for a direct QUIC path.
    CallFallbackRoomReady {
        peer_id: String,
        supernode_id: String,
        room_id: String,
    },
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
    /// The active room was resumed on a different cluster member after its host
    /// was lost. Tells the UI to re-point its active-room view at `supernode_id`
    /// (the sibling that accepted the rejoin) instead of tearing the room down —
    /// the cluster presents as one logical supernode, so this is a move, not a
    /// leave. `room_id` is unchanged; only the hosting member differs.
    RoomFailedOver {
        supernode_id: String,
        room_id: String,
    },
    /// Verified cluster sibling roster for a supernode, learned from its own
    /// signed `SUPERNODE_INFO` reply. `members` are sibling identity_pubs
    /// (excludes `supernode_id` itself). Lets the UI replay client-owned rooms
    /// saved under a sibling's identity onto this supernode too, since a
    /// cluster presents as one logical supernode to peers.
    ClusterMembersUpdated {
        supernode_id: String,
        members: Vec<String>,
    },
    /// Session state update for a peer.
    SessionStateUpdate(PeerSessionState),
    /// Typing indicator from a peer.
    TypingIndicator { peer_id: String, is_typing: bool },
    /// Room member list changed (full snapshot from `SfuMembers`).
    RoomMembersChanged {
        supernode_id: String,
        room_id: String,
        members: Vec<String>,
    },
    /// Supernode rejected our `SfuJoin` (`SfuJoinResult` accepted=false).
    /// UI must roll back optimistic voice / current-room state.
    RoomJoinRejected {
        supernode_id: String,
        room_id: String,
        reason: String,
    },
    /// A peer joined an SFU voice room.
    RoomPeerJoined {
        supernode_id: String,
        room_id: String,
        peer_id: String,
    },
    /// A peer left an SFU voice room.
    RoomPeerLeft {
        supernode_id: String,
        room_id: String,
        peer_id: String,
    },
    /// A text chat message arrived in an SFU room.
    RoomChatMessage {
        supernode_id: String,
        room_id: String,
        sender_id: String,
        sender_handle: String,
        body: String,
        timestamp: f64,
        message_id: String,
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
        /// True when the supernode advertises `room.audio.sfu` (SFU room hosting).
        sfu_enabled: bool,
        /// True when the supernode's SFU policy allows public room creation.
        public_rooms_enabled: bool,
    },
    /// Opaque `game.relay.v1` datagram from a portal peer via the QUIC relay
    /// (identity path — delivered to the in-app game page).
    PortalGameDatagram {
        supernode_id: String,
        payload: Vec<u8>,
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
    /// Supernode acknowledged a room we created (`SfuRoomCreated`).
    RoomCreated {
        supernode_id: String,
        room_id: String,
        room_name: String,
        room_type: String,
        invite_token: String,
    },
    /// A self-contained room invite (`conquerd://room#…`) was pasted and its
    /// host supernode is now connected — the UI should enter the room. The
    /// `invite_token` has already been persisted to the room store so the
    /// normal join path validates it automatically.
    RoomInviteReady {
        supernode_id: String,
        room_id: String,
        room_name: String,
        room_type: String,
        invite_token: String,
        /// Space-tree parent node id from the invite's inclusion proof, so the
        /// joiner's sidebar can nest the room. `""` for legacy / flat invites.
        parent_id: String,
        /// Owning Space id from the invite's signed root. `""` if absent.
        space_id: String,
    },

    /// A peer sent a presence update.
    PresenceUpdated { peer_id: String, status: String },
    /// Inbound SFU_AUDIO relayed from the supernode (Opus bytes from a room peer).
    SfuAudioReceived { peer_id: String, opus_data: Vec<u8> },
    /// Inbound direct-peer audio (Opus bytes from a 1:1 QUIC session).
    DirectAudioReceived { peer_id: String, opus_data: Vec<u8> },
    /// An invite handshake completed and the peer was added to the store.
    InviteAccepted { peer_id: String, handle: String },
    /// An invite could not be accepted or routed.
    InviteFailed { reason: String },
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
        /// Offer originator (1:1 peer, or room-file sender public id).
        peer_id: String,
        /// Non-empty when this was a room transfer.
        room_id: String,
        /// Supernode for room transfers (empty for 1:1).
        supernode_id: String,
        purpose: String,
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
    /// Direct QUIC is unavailable — create a temporary private SFU room on a
    /// trusted supernode, join it, and invite `peer_id` via `CallRequest`
    /// carrying the room coordinates.
    StartDirectCallFallback {
        peer_id: String,
    },
    /// Request a relay slot from a connected supernode.
    RequestRelay {
        supernode_id: String,
    },
    /// Start listening for incoming QUIC connections.
    StartQuicServer {
        port: u16,
    },
    /// Apply onboarding's direct-P2P listener choice immediately.
    ConfigureDirectP2p {
        enabled: bool,
        port: u16,
    },
    /// Join an SFU room for both voice and chat (sends `SfuJoin` signaling).
    JoinRoom {
        supernode_id: String,
        room_id: String,
    },
    /// Validate a private-room invite token, then join the SFU room.
    JoinRoomWithInvite {
        supernode_id: String,
        room_id: String,
        invite_token: String,
    },
    /// Leave an SFU voice room (sends `SfuLeave` signaling).
    LeaveRoom {
        supernode_id: String,
        room_id: String,
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
        message_id: String,
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
    /// Generate an invite URL from the transport layer so it can advertise
    /// the real local QUIC listener.
    GenerateInvite {
        reply_tx: std_mpsc::Sender<Option<String>>,
    },
    /// Generate a self-contained room invite URL (`conquerd://room#…`) that
    /// embeds the host supernode's signaling address alongside the room and
    /// token, so a joiner on any (or no) supernode can paste it and connect.
    GenerateRoomInvite {
        supernode_id: String,
        room_id: String,
        room_name: String,
        room_type: String,
        invite_token: String,
        /// Space-tree proof-based admission fields (JSON text; empty = omit),
        /// built by the owner from its Space (root/proof, + grant for a known
        /// grantee). Embedded in the invite for roster-free admission.
        space_root: String,
        space_proof: String,
        space_grant: String,
        reply_tx: std_mpsc::Sender<Option<String>>,
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
    /// Create a new SFU room on a supernode, or materialize a saved definition.
    CreateRoom {
        supernode_id: String,
        room_name: String,
        /// `"public"` or `"private"` (supernode `RoomType` wire shape).
        room_type: String,
        /// When set, recreate this exact room id (client replay on reconnect).
        room_id: Option<String>,
        /// Original creator when a non-creator peer materializes a saved room.
        creator_id: Option<String>,
        /// When true, do not auto-join on `SfuRoomCreated` (replay only).
        materialize_only: bool,
        /// Invite-mint policy: `"owner"` (default) or `"members"`. Empty is
        /// treated as unset (supernode defaults to `"owner"`).
        invite_policy: String,
        /// Durable invite credential from `RoomStore` (empty on first create).
        /// Replayed on rematerialize so the supernode can re-seed its in-memory
        /// token map after idle GC and re-admit returning members.
        invite_token: String,
    },
    /// Tear down a trusted supernode session and stop WS auto-reconnect.
    RemoveSupernode {
        supernode_id: String,
    },
    /// Send an Opus audio frame to the current SFU room via the supernode.
    /// Used as a WebSocket fallback when direct QUIC is unavailable.
    SendRoomAudio {
        opus_data: Vec<u8>,
    },
    /// Announce a freshly-signed Space root to `supernode_id`, which verifies,
    /// stores the highest epoch, and cluster-gossips it (authenticated room-set
    /// sync). `root_json` is a serialized `SignedSpaceRoot`.
    AnnounceSpaceRoot {
        supernode_id: String,
        root_json: String,
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
    /// Open a portal game session (`game.relay.v1`) on the identity QUIC
    /// relay — no WebTransport / self-signed cert. Ensures a relay grant,
    /// then sends `GameRelayJoin` for lobby `room`.
    PortalGameOpen {
        supernode_id: String,
        room: String,
        reply_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Send an opaque game.relay datagram for the portal page.
    PortalGameSend {
        supernode_id: String,
        payload: Vec<u8>,
        reply_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Leave the portal game session on the supernode.
    PortalGameClose {
        supernode_id: String,
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
