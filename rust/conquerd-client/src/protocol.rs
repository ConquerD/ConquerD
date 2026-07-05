//! Protocol message types and wire structures.
//!
//! All signaling messages are JSON objects with a `type` field and a
//! base64 `signature` produced by the sender's Ed25519 key. The payload is
//! canonicalized (sorted keys, no extra whitespace) before signing.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Wire format version. Bump when a breaking change is made.
pub const PROTOCOL_VERSION: u32 = 2;

/// Every signaling message type used in the ConquerD protocol.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    // Authentication
    Hello,
    Welcome,

    // Room management
    RoomJoin,
    RoomLeave,
    RoomState,
    RoomPeerJoined,
    RoomPeerLeft,
    RoomListRequest,
    RoomListResponse,

    // Invite-only peer bootstrap
    InviteHandshakeInit,
    InviteHandshakeAccept,
    InviteHandshakeReject,

    // End-to-end encrypted signaling envelope
    EncryptedSignal,

    // Presence
    PresenceUpdate,

    // Speaking state broadcast
    SpeakingState,

    // Text chat
    ChatMessage,
    ChatAck,
    ChatTyping,

    // Voice call control
    CallRequest,
    CallAccept,
    CallReject,
    CallEnd,

    // Endpoint updates
    EndpointUpdate,

    // Handle (display name) updates
    HandleUpdate,

    // Avatar visual config (sent post-handshake to trusted peers only)
    AvatarConfig,

    // P2P file transfer
    FileTransferOffer,
    FileTransferAccept,
    FileTransferReject,
    FileTransferChunk,
    FileTransferComplete,
    FileTransferAck,
    FileTransferError,

    // QUIC relay
    RelayRequest,
    RelayGranted,
    RelayRevoke,

    // Relay access control
    RelayPaymentRequired,
    RelayAccessGranted,
    RelayAccessDenied,

    // Supernode homepage / info
    SupernodeInfo,
    SupernodeInfoRequest,

    // Supernode-assisted hole punch coordination
    PunchRegister,
    PunchReady,

    // SFU
    SfuRoomList,
    SfuJoin,
    SfuLeave,
    SfuMembers,
    SfuPeerJoined,
    SfuPeerLeft,
    SfuOffer,
    SfuAnswer,
    SfuChat,
    SfuFileOffer,
    SfuFileChunk,
    SfuFileComplete,
    SfuAudio,

    // E2E room group-key distribution (sealed per member; supernode forwards blind)
    SfuGroupKey,

    // Space Merkle tree: owner announces a signed root to its supernode, which
    // stores + cluster-gossips it (authenticated room-set sync).
    SpaceRootAnnounce,

    // Room creation
    SfuRoomCreate,
    SfuRoomCreated,
    SfuRoomInvite,
    SfuRoomInviteResult,
    SfuRoomInviteGenerate,

    // SFU text-chat subscription
    SfuSubscribe,
    SfuUnsubscribe,

    // Direct peer-to-peer room invite
    PeerRoomInvite,

    // Trust
    TrustRequest,
    TrustAccept,

    // Heartbeat
    Ping,
    Pong,

    // Version announcements
    VersionAnnounce,

    // Build attestation
    BuildAttestation,
    AttestationResponse,

    // Capability framework
    CapabilityAnnounce,
    CapabilityInvoke,

    // Errors
    Error,
}

impl MessageType {
    /// The string value used on the wire (snake_case).
    pub fn as_wire_str(&self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::Welcome => "welcome",
            Self::RoomJoin => "room_join",
            Self::RoomLeave => "room_leave",
            Self::RoomState => "room_state",
            Self::RoomPeerJoined => "room_peer_joined",
            Self::RoomPeerLeft => "room_peer_left",
            Self::RoomListRequest => "room_list_request",
            Self::RoomListResponse => "room_list_response",
            Self::InviteHandshakeInit => "invite_handshake_init",
            Self::InviteHandshakeAccept => "invite_handshake_accept",
            Self::InviteHandshakeReject => "invite_handshake_reject",
            Self::EncryptedSignal => "encrypted_signal",
            Self::PresenceUpdate => "presence_update",
            Self::SpeakingState => "speaking_state",
            Self::ChatMessage => "chat_message",
            Self::ChatAck => "chat_ack",
            Self::ChatTyping => "chat_typing",
            Self::CallRequest => "call_request",
            Self::CallAccept => "call_accept",
            Self::CallReject => "call_reject",
            Self::CallEnd => "call_end",
            Self::EndpointUpdate => "endpoint_update",
            Self::HandleUpdate => "handle_update",
            Self::AvatarConfig => "avatar_config",
            Self::FileTransferOffer => "file_transfer_offer",
            Self::FileTransferAccept => "file_transfer_accept",
            Self::FileTransferReject => "file_transfer_reject",
            Self::FileTransferChunk => "file_transfer_chunk",
            Self::FileTransferComplete => "file_transfer_complete",
            Self::FileTransferAck => "file_transfer_ack",
            Self::FileTransferError => "file_transfer_error",
            Self::RelayRequest => "relay_request",
            Self::RelayGranted => "relay_granted",
            Self::RelayRevoke => "relay_revoke",
            Self::RelayPaymentRequired => "relay_payment_required",
            Self::RelayAccessGranted => "relay_access_granted",
            Self::RelayAccessDenied => "relay_access_denied",
            Self::SupernodeInfo => "supernode_info",
            Self::SupernodeInfoRequest => "supernode_info_request",
            Self::PunchRegister => "punch_register",
            Self::PunchReady => "punch_ready",
            Self::SfuRoomList => "sfu_room_list",
            Self::SfuJoin => "sfu_join",
            Self::SfuLeave => "sfu_leave",
            Self::SfuMembers => "sfu_members",
            Self::SfuPeerJoined => "sfu_peer_joined",
            Self::SfuPeerLeft => "sfu_peer_left",
            Self::SfuOffer => "sfu_offer",
            Self::SfuAnswer => "sfu_answer",
            Self::SfuChat => "sfu_chat",
            Self::SfuFileOffer => "sfu_file_offer",
            Self::SfuFileChunk => "sfu_file_chunk",
            Self::SfuFileComplete => "sfu_file_complete",
            Self::SfuAudio => "sfu_audio",
            Self::SfuGroupKey => "sfu_group_key",
            Self::SpaceRootAnnounce => "space_root_announce",
            Self::SfuRoomCreate => "sfu_room_create",
            Self::SfuRoomCreated => "sfu_room_created",
            Self::SfuRoomInvite => "sfu_room_invite",
            Self::SfuRoomInviteResult => "sfu_room_invite_result",
            Self::SfuRoomInviteGenerate => "sfu_room_invite_generate",
            Self::SfuSubscribe => "sfu_subscribe",
            Self::SfuUnsubscribe => "sfu_unsubscribe",
            Self::PeerRoomInvite => "peer_room_invite",
            Self::TrustRequest => "trust_request",
            Self::TrustAccept => "trust_accept",
            Self::Ping => "ping",
            Self::Pong => "pong",
            Self::VersionAnnounce => "version_announce",
            Self::BuildAttestation => "build_attestation",
            Self::AttestationResponse => "attestation_response",
            Self::CapabilityAnnounce => "capability_announce",
            Self::CapabilityInvoke => "capability_invoke",
            Self::Error => "error",
        }
    }
}

/// A single signaling protocol message.
///
/// ## Replay protection status (as of 2026-06 review)
/// - **Invite / handshake bootstrap**: Strong (Ed25519 signatures + `expires_at` +
///   transcript binding into the forward-secret keys).
/// - **Post-handshake ongoing signaling** (Chat*, Call*, EndpointUpdate, Capability*,
///   Room*, SFU*, etc.): Ed25519 signature over `canonical_bytes()`, **plus** a
///   timestamp freshness window (5 min) **plus** a per-sender sliding-window
///   replay guard keyed on the message signature.
/// - Freshness rejects anything outside the window; the sliding-window guard
///   ([`conquerd_features::ReplayGuard`]) rejects re-delivery of an already-seen
///   signature *within* the window. Together they close the replay gap for all
///   non-realtime signaling. Real-time `SfuAudio` frames are exempt from the
///   dedup guard only (ephemeral, high-rate, covered by the freshness window +
///   jitter buffer); they are still subject to per-feature byte quotas on the
///   transport relay path.
///
/// See `connection_manager::verify_inbound_signature` (freshness) +
/// `connection_manager::check_replay` (dedup) and the supernode WS handler,
/// which applies the same two checks on its inbound path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalingMessage {
    /// Message type discriminator.
    #[serde(rename = "type")]
    pub msg_type: MessageType,

    /// `public_id` of the sender.
    pub sender: String,

    /// Arbitrary per-type payload fields.
    #[serde(default)]
    pub payload: BTreeMap<String, Value>,

    /// Unix timestamp (seconds) of message creation.
    #[serde(default = "default_timestamp")]
    pub timestamp: f64,

    /// `public_id` of the intended recipient (`None` = broadcast).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,

    /// Base64-encoded Ed25519 signature (added after construction).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Wire format version. Sent on the wire as "v" to match the supernode convention.
    #[serde(rename = "v", default = "default_protocol_version")]
    pub protocol_version: u32,
}

fn default_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn default_protocol_version() -> u32 {
    PROTOCOL_VERSION
}

impl SignalingMessage {
    /// Create a new message with the current timestamp.
    pub fn new(msg_type: MessageType, sender: String) -> Self {
        Self {
            msg_type,
            sender,
            payload: BTreeMap::new(),
            timestamp: default_timestamp(),
            target: None,
            signature: None,
            protocol_version: PROTOCOL_VERSION,
        }
    }

    /// Produce the canonical bytes for signing.
    ///
    /// `signaling_canonical_bytes` Rust function: a
    /// sorted-key JSON serialization of the message *without* the signature
    /// field, using serde_json's Ryu float formatting.
    pub fn canonical_bytes(&self) -> serde_json::Result<Vec<u8>> {
        // Build a map without the signature field so the canonical form
        // matches what conquerd_crypto::signaling_canonical_bytes produces.
        let mut map: BTreeMap<&str, Value> = BTreeMap::new();
        map.insert(
            "type",
            Value::String(self.msg_type.as_wire_str().to_owned()),
        );
        map.insert("sender", Value::String(self.sender.clone()));
        map.insert("payload", serde_json::to_value(&self.payload)?);
        map.insert(
            "timestamp",
            Value::Number(
                serde_json::Number::from_f64(self.timestamp)
                    .unwrap_or_else(|| serde_json::Number::from(0)),
            ),
        );
        map.insert("v", Value::Number(self.protocol_version.into()));
        if let Some(t) = &self.target {
            map.insert("target", Value::String(t.clone()));
        }
        serde_json::to_vec(&map)
    }

    /// Returns true if the message timestamp is within `max_age_secs` of now.
    /// Used for basic replay protection.
    pub fn is_fresh(&self, max_age_secs: f64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        (now - self.timestamp).abs() <= max_age_secs
    }

    /// Parse from a raw JSON string received over the wire.
    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    /// Serialize to a JSON string for sending over the wire.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{MessageType, SignalingMessage};

    const FRESHNESS_WINDOW_SECS: f64 = 300.0;

    fn msg_with_timestamp(ts: f64) -> SignalingMessage {
        let mut msg = SignalingMessage::new(MessageType::ChatMessage, "sender".to_owned());
        msg.timestamp = ts;
        msg
    }

    fn now_secs() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    }

    #[test]
    fn signed_message_survives_json_roundtrip_bit_exact() {
        // Regression guard: serde_json's default f64 parser is not
        // round-trip-accurate, which shifted a signed message's timestamp by
        // 1 ULP on `from_json` and broke Ed25519 verification for ~10% of
        // high-precision timestamps. The `float_roundtrip` feature must keep
        // `canonical_bytes()` stable across a serialize → parse cycle.
        for _ in 0..50_000 {
            let m = SignalingMessage::new(MessageType::ChatMessage, "s".to_owned());
            let m2 = SignalingMessage::from_json(&m.to_json().unwrap()).unwrap();
            assert_eq!(m.timestamp, m2.timestamp);
            assert_eq!(m.canonical_bytes().unwrap(), m2.canonical_bytes().unwrap());
        }
    }

    #[test]
    fn is_fresh_accepts_current_timestamp() {
        let msg = msg_with_timestamp(now_secs());
        assert!(msg.is_fresh(FRESHNESS_WINDOW_SECS));
    }

    #[test]
    fn is_fresh_rejects_stale_timestamp() {
        let msg = msg_with_timestamp(now_secs() - FRESHNESS_WINDOW_SECS - 1.0);
        assert!(!msg.is_fresh(FRESHNESS_WINDOW_SECS));
    }

    #[test]
    fn is_fresh_rejects_future_timestamp() {
        let msg = msg_with_timestamp(now_secs() + FRESHNESS_WINDOW_SECS + 1.0);
        assert!(!msg.is_fresh(FRESHNESS_WINDOW_SECS));
    }

    #[test]
    fn is_fresh_accepts_just_inside_window() {
        // One second inside the window avoids flaky boundary timing between
        // `now_secs()` and the second `SystemTime::now()` inside `is_fresh`.
        let msg = msg_with_timestamp(now_secs() - (FRESHNESS_WINDOW_SECS - 1.0));
        assert!(msg.is_fresh(FRESHNESS_WINDOW_SECS));
    }
}
