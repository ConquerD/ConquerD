// ConquerD Supernode — protocol.rs
// Signaling message types, envelope serialization, signing/verification.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::{b64url_decode, b64url_encode};
use crate::identity::Identity;

/// Protocol version.
pub const PROTOCOL_VERSION: u32 = 2;

/// All message types in the ConquerD protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageType {
    // Auth
    #[serde(rename = "hello")]
    Hello,
    #[serde(rename = "welcome")]
    Welcome,

    // Room
    #[serde(rename = "room_join")]
    RoomJoin,
    #[serde(rename = "room_leave")]
    RoomLeave,
    #[serde(rename = "room_state")]
    RoomState,
    #[serde(rename = "room_peer_joined")]
    RoomPeerJoined,
    #[serde(rename = "room_peer_left")]
    RoomPeerLeft,
    #[serde(rename = "room_list_request")]
    RoomListRequest,
    #[serde(rename = "room_list_response")]
    RoomListResponse,

    // Invite handshake
    #[serde(rename = "invite_handshake_init")]
    InviteHandshakeInit,
    #[serde(rename = "invite_handshake_accept")]
    InviteHandshakeAccept,
    #[serde(rename = "invite_handshake_reject")]
    InviteHandshakeReject,

    // Encrypted envelope
    #[serde(rename = "encrypted_signal")]
    EncryptedSignal,

    // Presence
    #[serde(rename = "presence_update")]
    PresenceUpdate,
    #[serde(rename = "speaking_state")]
    SpeakingState,

    // Chat
    #[serde(rename = "chat_message")]
    ChatMessage,
    #[serde(rename = "chat_ack")]
    ChatAck,
    #[serde(rename = "chat_typing")]
    ChatTyping,

    // Calls
    #[serde(rename = "call_request")]
    CallRequest,
    #[serde(rename = "call_accept")]
    CallAccept,
    #[serde(rename = "call_reject")]
    CallReject,
    #[serde(rename = "call_end")]
    CallEnd,

    // Endpoint/Handle updates
    #[serde(rename = "endpoint_update")]
    EndpointUpdate,
    #[serde(rename = "handle_update")]
    HandleUpdate,

    // Version/Updates
    #[serde(rename = "version_announce")]
    VersionAnnounce,
    #[serde(rename = "update_offer")]
    UpdateOffer,
    #[serde(rename = "update_accept")]
    UpdateAccept,
    #[serde(rename = "update_reject")]
    UpdateReject,

    // File transfer
    #[serde(rename = "file_transfer_offer")]
    FileTransferOffer,
    #[serde(rename = "file_transfer_accept")]
    FileTransferAccept,
    #[serde(rename = "file_transfer_reject")]
    FileTransferReject,
    #[serde(rename = "file_transfer_chunk")]
    FileTransferChunk,
    #[serde(rename = "file_transfer_complete")]
    FileTransferComplete,
    #[serde(rename = "file_transfer_ack")]
    FileTransferAck,
    #[serde(rename = "file_transfer_error")]
    FileTransferError,

    // QUIC relay
    #[serde(rename = "relay_request")]
    RelayRequest,
    #[serde(rename = "relay_granted")]
    RelayGranted,
    #[serde(rename = "relay_revoke")]
    RelayRevoke,

    // Relay access control
    #[serde(rename = "relay_payment_required")]
    RelayPaymentRequired,
    #[serde(rename = "relay_access_granted")]
    RelayAccessGranted,
    #[serde(rename = "relay_access_denied")]
    RelayAccessDenied,

    // Supernode info
    #[serde(rename = "supernode_info")]
    SupernodeInfo,
    #[serde(rename = "supernode_info_request")]
    SupernodeInfoRequest,

    // Hole punch
    #[serde(rename = "punch_register")]
    PunchRegister,
    #[serde(rename = "punch_ready")]
    PunchReady,

    // SFU (group calls)
    #[serde(rename = "sfu_room_list")]
    SfuRoomList,
    #[serde(rename = "sfu_join")]
    SfuJoin,
    #[serde(rename = "sfu_leave")]
    SfuLeave,
    #[serde(rename = "sfu_members")]
    SfuMembers,
    #[serde(rename = "sfu_peer_joined")]
    SfuPeerJoined,
    #[serde(rename = "sfu_peer_left")]
    SfuPeerLeft,
    #[serde(rename = "sfu_offer")]
    SfuOffer,
    #[serde(rename = "sfu_answer")]
    SfuAnswer,
    #[serde(rename = "sfu_chat")]
    SfuChat,
    #[serde(rename = "sfu_file_offer")]
    SfuFileOffer,
    #[serde(rename = "sfu_file_chunk")]
    SfuFileChunk,
    #[serde(rename = "sfu_file_complete")]
    SfuFileComplete,
    #[serde(rename = "sfu_audio")]
    SfuAudio,
    /// Owner announces a signed Space root; the supernode verifies, stores, and
    /// cluster-gossips it (authenticated room-set sync).
    #[serde(rename = "space_root_announce")]
    SpaceRootAnnounce,
    #[serde(rename = "sfu_room_create")]
    SfuRoomCreate,
    #[serde(rename = "sfu_room_created")]
    SfuRoomCreated,
    #[serde(rename = "sfu_room_invite")]
    SfuRoomInvite,
    #[serde(rename = "sfu_room_invite_result")]
    SfuRoomInviteResult,
    #[serde(rename = "sfu_room_invite_generate")]
    SfuRoomInviteGenerate,

    // SFU text-chat subscription (no voice join required)
    #[serde(rename = "sfu_subscribe")]
    SfuSubscribe,
    #[serde(rename = "sfu_unsubscribe")]
    SfuUnsubscribe,

    // Trust promotion
    #[serde(rename = "trust_request")]
    TrustRequest,
    #[serde(rename = "trust_accept")]
    TrustAccept,

    // Direct peer room invite
    #[serde(rename = "peer_room_invite")]
    PeerRoomInvite,

    // Peer build attestation (relayed opaque peer-to-peer messages)
    #[serde(rename = "build_attestation")]
    BuildAttestation,
    #[serde(rename = "attestation_response")]
    AttestationResponse,

    // Heartbeat
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "pong")]
    Pong,

    // Capability framework (peer ↔ peer; supernode ↔ peer).
    // ANNOUNCE carries `capabilities[]` (full snapshot or delta).
    // INVOKE opens a feature channel: `{ id, version, params, channel_hint }`.
    #[serde(rename = "capability_announce")]
    CapabilityAnnounce,
    #[serde(rename = "capability_invoke")]
    CapabilityInvoke,

    // Error
    #[serde(rename = "error")]
    Error,
}

impl fmt::Display for MessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{self:?}"));
        f.write_str(&s)
    }
}

/// Signaling message envelope.
#[derive(Debug, Clone)]
pub struct SignalingMessage {
    pub msg_type: MessageType,
    pub sender: String,            // base64url public key
    pub payload: Value,            // type-specific JSON
    pub timestamp: f64,            // unix timestamp
    pub target: Option<String>,    // optional recipient
    pub signature: Option<String>, // base64url Ed25519 signature
    pub protocol_version: u32,
    /// Raw JSON Value for the timestamp, preserved from the wire to avoid
    /// f64 round-trip serialization differences when computing canonical bytes.
    timestamp_raw: Option<Value>,
}

impl SignalingMessage {
    /// Create a new unsigned message.
    pub fn new(msg_type: MessageType, sender: &str, payload: Value) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        Self {
            msg_type,
            sender: sender.to_string(),
            payload,
            timestamp: now,
            target: None,
            signature: None,
            protocol_version: PROTOCOL_VERSION,
            timestamp_raw: None,
        }
    }

    /// Set the target recipient.
    pub fn with_target(mut self, target: &str) -> Self {
        self.target = Some(target.to_string());
        self
    }

    /// Compute canonical bytes for signing (JSON without signature, sorted keys).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut map = serde_json::Map::new();
        map.insert("payload".into(), self.payload.clone());
        map.insert("sender".into(), Value::String(self.sender.clone()));
        // Use raw timestamp Value if available (preserves original wire format),
        // otherwise fall back to f64 reconstruction (for locally-created messages).
        let ts_val = self.timestamp_raw.clone().unwrap_or_else(|| {
            serde_json::Number::from_f64(self.timestamp)
                .map(Value::Number)
                .unwrap_or(Value::Null)
        });
        map.insert("timestamp".into(), ts_val);
        map.insert("type".into(), serde_json::to_value(self.msg_type).unwrap());
        map.insert("v".into(), Value::Number(self.protocol_version.into()));
        if let Some(ref target) = self.target {
            map.insert("target".into(), Value::String(target.clone()));
        }
        // serde_json::Map is BTreeMap → keys already sorted
        serde_json::to_vec(&Value::Object(map)).unwrap()
    }

    /// Sign this message with the given identity.
    pub fn sign(mut self, identity: &Identity) -> Self {
        let canonical = self.canonical_bytes();
        let sig = identity.sign(&canonical);
        self.signature = Some(b64url_encode(&sig));
        self
    }

    /// Verify the signature against the sender's public key.
    pub fn verify(&self) -> bool {
        let Some(ref sig_b64) = self.signature else {
            return false;
        };
        let Ok(sig_bytes) = b64url_decode(sig_b64) else {
            return false;
        };
        let Ok(pub_bytes) = b64url_decode(&self.sender) else {
            return false;
        };
        let canonical = self.canonical_bytes();
        Identity::verify_with_pub(&pub_bytes, &sig_bytes, &canonical)
    }

    /// Returns true if the message timestamp is within `max_age_secs` of now.
    /// Used for basic replay protection on the WS signaling path.
    pub fn is_fresh(&self, max_age_secs: f64) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        (now - self.timestamp).abs() <= max_age_secs
    }

    /// Serialize to JSON string (compact, sorted keys).
    pub fn to_json(&self) -> String {
        let mut map = serde_json::Map::new();
        map.insert("type".into(), serde_json::to_value(self.msg_type).unwrap());
        map.insert("sender".into(), Value::String(self.sender.clone()));
        map.insert("payload".into(), self.payload.clone());
        map.insert(
            "timestamp".into(),
            serde_json::Number::from_f64(self.timestamp)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        map.insert("v".into(), Value::Number(self.protocol_version.into()));
        if let Some(ref target) = self.target {
            map.insert("target".into(), Value::String(target.clone()));
        }
        if let Some(ref sig) = self.signature {
            map.insert("signature".into(), Value::String(sig.clone()));
        }
        serde_json::to_string(&Value::Object(map)).unwrap()
    }

    /// Parse from JSON string.
    pub fn from_json(raw: &str) -> Result<Self, serde_json::Error> {
        use serde::de::Error as _;
        let v: Value = serde_json::from_str(raw)?;
        let obj = v
            .as_object()
            .ok_or_else(|| serde_json::Error::custom("expected object"))?;

        let msg_type: MessageType = serde_json::from_value(
            obj.get("type")
                .cloned()
                .ok_or_else(|| serde_json::Error::custom("missing type"))?,
        )?;

        let sender = obj
            .get("sender")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let payload = obj
            .get("payload")
            .cloned()
            .unwrap_or(Value::Object(Default::default()));
        let timestamp_raw = obj.get("timestamp").cloned();
        let timestamp = timestamp_raw
            .as_ref()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let target = obj.get("target").and_then(|v| v.as_str()).map(String::from);
        let signature = obj
            .get("signature")
            .and_then(|v| v.as_str())
            .map(String::from);
        let protocol_version = obj
            .get("v")
            .and_then(|v| v.as_u64())
            .unwrap_or(PROTOCOL_VERSION as u64) as u32;

        Ok(Self {
            msg_type,
            sender,
            payload,
            timestamp,
            target,
            signature,
            protocol_version,
            timestamp_raw,
        })
    }
}

/// Verify that *payload* is a UTF-8 JSON [`SignalingMessage`] whose
/// `sender` matches *expected_sender* and whose Ed25519 signature is
/// valid. Returns the parsed message on success, `None` otherwise.
///
/// Used by the WebTransport bridge to gate browser-originated envelopes
/// before relaying them onto the native signaling fabric. The supernode
/// never re-signs: browsers must present a fully-signed envelope and
/// the verified identity captured at handshake time must be the
/// envelope's `sender`.
pub fn verify_browser_envelope(
    expected_sender: Option<&str>,
    payload: &[u8],
) -> Option<SignalingMessage> {
    let raw = std::str::from_utf8(payload).ok()?;
    let msg = SignalingMessage::from_json(raw).ok()?;
    let expected = expected_sender?;
    if msg.sender != expected {
        return None;
    }
    if !msg.verify() {
        return None;
    }
    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_sign_verify() {
        let id = Identity::generate();
        let msg = SignalingMessage::new(MessageType::Ping, &id.public_id(), serde_json::json!({}))
            .sign(&id);

        assert!(msg.verify());
    }

    #[test]
    fn test_message_roundtrip() {
        let id = Identity::generate();
        let msg = SignalingMessage::new(
            MessageType::ChatMessage,
            &id.public_id(),
            serde_json::json!({"body": "hello"}),
        )
        .with_target("some_target")
        .sign(&id);

        let json = msg.to_json();
        let parsed = SignalingMessage::from_json(&json).unwrap();
        assert_eq!(parsed.msg_type, MessageType::ChatMessage);
        assert_eq!(parsed.sender, id.public_id());
        assert!(parsed.verify());
    }

    #[test]
    #[allow(clippy::approx_constant, clippy::excessive_precision)]
    fn test_float_serialization() {
        // Check that float serialization is lossless (round-trips exactly).
        // These literals are intentional test fixtures, not math constants.
        let floats: &[f64] = &[
            0.0,
            1.0,
            1744374958.493273,
            1775927479.7999392,
            1744374463.0102730,
            3.141592653589793,
            1234567890.1234567,
        ];
        for &f in floats {
            let n = serde_json::Number::from_f64(f).expect("valid float");
            let serialized = serde_json::to_string(&n).unwrap();
            let reparsed: f64 = serde_json::from_str(&serialized).unwrap();
            assert_eq!(f, reparsed, "float round-trip failed for {}", f);
            println!("f={} -> json={} -> f={}", f, serialized, reparsed);
        }
        // Spot check one: verify 1744374958.493273 serializes with full precision.
        let n = serde_json::Number::from_f64(1744374958.493273).unwrap();
        let s = serde_json::to_string(&n).unwrap();
        println!("Rust serializes 1744374958.493273 as: {}", s);
        // The canonical_bytes test below also covers this via full-message verify
    }

    #[test]
    fn test_cross_language_verify_signed_nonround_ts() {
        let id = Identity::generate();
        let msg = SignalingMessage::new(
            MessageType::VersionAnnounce,
            &id.public_id(),
            serde_json::json!({
                "version": "0.9.14",
                "manifest_hash": "abc123",
                "manifest_created_at": 1744374958.493273_f64
            }),
        )
        .with_target("AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA")
        .sign(&id);

        let original_canonical = std::str::from_utf8(&msg.canonical_bytes())
            .unwrap()
            .to_string();
        let json = msg.to_json();
        println!("wire JSON: {}", json);

        let parsed = SignalingMessage::from_json(&json).unwrap();
        let parsed_canonical = std::str::from_utf8(&parsed.canonical_bytes())
            .unwrap()
            .to_string();

        println!("Original canonical: {}", original_canonical);
        println!("Parsed   canonical: {}", parsed_canonical);
        println!(
            "Canonicals match:   {}",
            original_canonical == parsed_canonical
        );

        assert_eq!(
            original_canonical, parsed_canonical,
            "Canonical bytes differ after round-trip!"
        );
        assert!(parsed.verify(), "Rust round-trip verify FAILED");
    }

    #[test]
    fn test_cross_language_verify_signed() {
        // Reference wire JSON with deterministic seed bytes(range(32)).
        // Expected canonical:
        // {"payload":{"manifest_created_at":0.0,"manifest_hash":"abc","version":"0.9.14"},
        //  "sender":"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=",
        //  "target":"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA",
        //  "timestamp":1744374958.0,"type":"version_announce","v":2}
        let wire = r#"{"payload": {"manifest_created_at": 0.0, "manifest_hash": "abc", "version": "0.9.14"}, "sender": "A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=", "signature": "NwyqrltJKP6OTQXx9WbHUvMelJHBsfrwaK2-4wSvnH639-RJyVP7zPhigRTsp46ux67UjwP4lqbZmwN6I5kfBw", "target": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA", "timestamp": 1744374958.0, "type": "version_announce", "v": 2}"#;
        let parsed = SignalingMessage::from_json(wire).unwrap();
        let rust_canonical = parsed.canonical_bytes();
        let rust_canonical_str = std::str::from_utf8(&rust_canonical).unwrap();
        println!("Rust canonical:   {}", rust_canonical_str);
        println!("Expected canonical: {{\"payload\":{{\"manifest_created_at\":0.0,\"manifest_hash\":\"abc\",\"version\":\"0.9.14\"}},\"sender\":\"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=\",\"target\":\"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA\",\"timestamp\":1744374958.0,\"type\":\"version_announce\",\"v\":2}}");
        assert!(
            parsed.verify(),
            "Rust failed to verify reference-signed message; canonical='{}'",
            rust_canonical_str
        );
    }

    #[test]
    fn test_cross_language_endpoint_update() {
        // Reference wire JSON with deterministic seed bytes(range(32)).
        // EndpointUpdate with quic_port integer to verify integer serialization.
        // Sender WITHOUT padding.
        let wire = r#"{"payload": {"listener": "ws://104.54.197.38:52331", "peer_id": "d3ee5aa760f6163db7b06797d1073ae963b194533cddd44a98b35275fac34b8e", "quic_port": 53329}, "sender": "A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg", "signature": "DVMDzyhz_CaTw629dVH2CcOxT7UzDHFXw2O4zGAwlBFr7VbbLV-disl2bhny5QY60rOvtw7dNbr7PjFz3rsKAw", "target": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA", "timestamp": 1744396423.689, "type": "endpoint_update", "v": 2}"#;
        let parsed = SignalingMessage::from_json(wire).unwrap();
        let rust_canonical = parsed.canonical_bytes();
        let rust_canonical_str = std::str::from_utf8(&rust_canonical).unwrap();
        let expected_canonical = r#"{"payload":{"listener":"ws://104.54.197.38:52331","peer_id":"d3ee5aa760f6163db7b06797d1073ae963b194533cddd44a98b35275fac34b8e","quic_port":53329},"sender":"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg","target":"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA","timestamp":1744396423.689,"type":"endpoint_update","v":2}"#;
        println!("Rust canonical:     {}", rust_canonical_str);
        println!("Expected canonical: {}", expected_canonical);
        assert_eq!(
            rust_canonical_str, expected_canonical,
            "Canonical bytes mismatch!"
        );
        assert!(parsed.verify(), "Rust failed to verify EndpointUpdate");
    }

    #[test]
    fn test_cross_language_endpoint_update_padded_sender() {
        // Wire JSON with base64url-padded sender (= suffix present).
        // The padded form is the canonical sender format for verify compatibility.
        let wire = r#"{"payload": {"listener": "ws://104.54.197.38:52331", "peer_id": "d3ee5aa760f6163db7b06797d1073ae963b194533cddd44a98b35275fac34b8e", "quic_port": 53329}, "sender": "A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=", "signature": "FtS0UsjAEWpAGnJIE399AWCdsfy6Zri9ZHgtBi5vFDdJDpGtz7bCHLjyHy802VewRsYyg_OEi93q2b5pjAsfDA", "target": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA", "timestamp": 1744396423.689, "type": "endpoint_update", "v": 2}"#;
        let parsed = SignalingMessage::from_json(wire).unwrap();
        let rust_canonical = parsed.canonical_bytes();
        let rust_canonical_str = std::str::from_utf8(&rust_canonical).unwrap();
        // Canonical should include the padded sender exactly as received
        let expected_canonical = r#"{"payload":{"listener":"ws://104.54.197.38:52331","peer_id":"d3ee5aa760f6163db7b06797d1073ae963b194533cddd44a98b35275fac34b8e","quic_port":53329},"sender":"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=","target":"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA","timestamp":1744396423.689,"type":"endpoint_update","v":2}"#;
        println!("Rust canonical:     {}", rust_canonical_str);
        println!("Expected canonical: {}", expected_canonical);
        assert_eq!(
            rust_canonical_str, expected_canonical,
            "Canonical bytes mismatch with padded sender!"
        );
        assert!(
            parsed.verify(),
            "Rust failed to verify EndpointUpdate with padded sender"
        );
    }

    #[test]
    fn test_cross_language_realistic_timestamp() {
        // Wire JSON with a high-precision timestamp (7 decimal places).
        let wire = r#"{"payload": {"listener": "ws://104.54.197.38:52331", "peer_id": "d3ee5aa760f6163db7b06797d1073ae963b194533cddd44a98b35275fac34b8e", "quic_port": 53329}, "sender": "A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=", "signature": "Q5KbHGL3_vtCoJqikOf31v0CuHPUgU9Cz9hgLUJcpOJpvNvoewpGMcn-qhuhhguVFPwAZpXp96N4hvH-7saFBQ", "target": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA", "timestamp": 1775952222.5117724, "type": "endpoint_update", "v": 2}"#;
        let parsed = SignalingMessage::from_json(wire).unwrap();
        let rust_canonical = parsed.canonical_bytes();
        let rust_canonical_str = std::str::from_utf8(&rust_canonical).unwrap();
        let expected_canonical = r#"{"payload":{"listener":"ws://104.54.197.38:52331","peer_id":"d3ee5aa760f6163db7b06797d1073ae963b194533cddd44a98b35275fac34b8e","quic_port":53329},"sender":"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=","target":"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA","timestamp":1775952222.5117724,"type":"endpoint_update","v":2}"#;
        println!("Rust canonical:     {}", rust_canonical_str);
        println!("Expected canonical: {}", expected_canonical);
        assert_eq!(
            rust_canonical_str, expected_canonical,
            "Timestamp precision mismatch!"
        );
        assert!(
            parsed.verify(),
            "Rust failed to verify msg with realistic timestamp"
        );
    }

    #[test]
    fn test_cross_language_sfu_join_padded_sender() {
        // Reference wire JSON for SfuJoin with padded sender.
        // Generated with deterministic seed bytes(range(32)).
        // Expected canonical: {"payload":{"room_id":"test-room-123"},"sender":"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=","target":"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=","timestamp":1751890523.456789,"type":"sfu_join","v":2}
        let wire = r#"{"payload": {"room_id": "test-room-123"}, "sender": "A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=", "signature": "IFt_om1-DuP8HZ4pVhpNWmv9w9JObBwvQottTV1l8RZyi_A6M9A-qmru0qPY7XBKbwsQKFXblsHjtYA9d0Q9Bw", "target": "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=", "timestamp": 1751890523.456789, "type": "sfu_join", "v": 2}"#;
        let parsed = SignalingMessage::from_json(wire).unwrap();
        let rust_canonical = parsed.canonical_bytes();
        let rust_canonical_str = std::str::from_utf8(&rust_canonical).unwrap();
        let expected_canonical = r#"{"payload":{"room_id":"test-room-123"},"sender":"A6EHv_POEL4dcN0Y50vAmWfk1jCbpQ1fHdyGZBJVMbg=","target":"AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA=","timestamp":1751890523.456789,"type":"sfu_join","v":2}"#;
        println!("Rust canonical:     {}", rust_canonical_str);
        println!("Expected canonical: {}", expected_canonical);
        assert_eq!(
            rust_canonical_str, expected_canonical,
            "SfuJoin canonical bytes mismatch!"
        );
        assert!(
            parsed.verify(),
            "Rust failed to verify SfuJoin with padded sender"
        );
    }

    #[test]
    fn test_all_message_types_serialize() {
        // Ensure all message types can serialize to their serde string
        let types = [
            MessageType::Hello,
            MessageType::Welcome,
            MessageType::InviteHandshakeInit,
            MessageType::InviteHandshakeAccept,
            MessageType::RelayGranted,
            MessageType::SfuJoin,
            MessageType::Ping,
            MessageType::Pong,
            MessageType::Error,
        ];
        for mt in types {
            let v = serde_json::to_value(mt).unwrap();
            assert!(v.is_string(), "MessageType {:?} should serialize", mt);
            let back: MessageType = serde_json::from_value(v).unwrap();
            assert_eq!(back, mt);
        }
    }

    fn signed_envelope(id: &Identity, target: &str) -> String {
        SignalingMessage::new(
            MessageType::SfuAudio,
            &id.public_id(),
            serde_json::json!({"opus": "frame"}),
        )
        .with_target(target)
        .sign(id)
        .to_json()
    }

    #[test]
    fn verify_envelope_accepts_matching_signed_payload() {
        let id = Identity::generate();
        let raw = signed_envelope(&id, "native-1");
        let pk = id.public_id();
        let msg =
            verify_browser_envelope(Some(&pk), raw.as_bytes()).expect("valid envelope must verify");
        assert_eq!(msg.sender, pk);
        assert_eq!(msg.target.as_deref(), Some("native-1"));
    }

    #[test]
    fn verify_envelope_rejects_when_no_verified_identity() {
        let id = Identity::generate();
        let raw = signed_envelope(&id, "native-1");
        assert!(verify_browser_envelope(None, raw.as_bytes()).is_none());
    }

    #[test]
    fn verify_envelope_rejects_sender_mismatch() {
        let id = Identity::generate();
        let other = Identity::generate();
        let raw = signed_envelope(&id, "native-1");
        // Verified identity is "other" but envelope claims `id`.
        assert!(verify_browser_envelope(Some(&other.public_id()), raw.as_bytes()).is_none());
    }

    #[test]
    fn verify_envelope_rejects_unsigned_payload() {
        let id = Identity::generate();
        let raw = SignalingMessage::new(
            MessageType::SfuAudio,
            &id.public_id(),
            serde_json::json!({}),
        )
        .to_json();
        assert!(verify_browser_envelope(Some(&id.public_id()), raw.as_bytes()).is_none());
    }

    #[test]
    fn verify_envelope_rejects_garbage() {
        assert!(verify_browser_envelope(Some("anyone"), b"not json").is_none());
        assert!(verify_browser_envelope(Some("anyone"), &[0xff, 0xfe]).is_none());
    }

    /// Mirrors what the browser SDK in `web-sdk/conquerd.mjs` puts on
    /// the wire: integer-second `timestamp`, signature computed over
    /// canonical (alpha-sorted) JSON, but the *transmitted* JSON keys
    /// in a different order. The verifier must still accept it because
    /// `from_json` preserves `timestamp_raw` and `canonical_bytes()`
    /// re-sorts.
    #[test]
    fn verify_envelope_matches_js_sdk_wire_format() {
        use crate::crypto::{b64url_decode, b64url_encode};
        use crate::identity::Identity;

        let id = Identity::generate();
        let sender = id.public_id();
        let pub_bytes = b64url_decode(&sender).unwrap();

        // Canonical layout (alpha-sorted, no spaces) the JS SDK signs.
        let canonical = format!(
            r#"{{"payload":{{"opus":"frame"}},"sender":"{}","target":"native-1","timestamp":1714512345,"type":"sfu_audio","v":2}}"#,
            sender
        );
        let sig = id.sign(canonical.as_bytes());
        let sig_b64 = b64url_encode(&sig);

        // On-wire layout: `type` first (matches `to_json`), `signature`
        // last. Verifier must canonicalise before checking.
        let wire = format!(
            r#"{{"type":"sfu_audio","sender":"{}","payload":{{"opus":"frame"}},"timestamp":1714512345,"v":2,"target":"native-1","signature":"{}"}}"#,
            sender, sig_b64
        );

        let msg = verify_browser_envelope(Some(&sender), wire.as_bytes())
            .expect("JS-formatted envelope must verify");
        assert_eq!(msg.target.as_deref(), Some("native-1"));
        assert!(crate::identity::Identity::verify_with_pub(
            &pub_bytes,
            &b64url_decode(&sig_b64).unwrap(),
            canonical.as_bytes()
        ));
    }

    // --- Replay / signature edge-case tests ---

    #[test]
    fn verify_returns_false_with_no_signature() {
        let id = Identity::generate();
        let msg = SignalingMessage::new(MessageType::Ping, &id.public_id(), serde_json::json!({}));
        // Not signed — verify must return false.
        assert!(!msg.verify());
    }

    #[test]
    fn verify_returns_false_for_wrong_signing_key() {
        let id1 = Identity::generate();
        let id2 = Identity::generate();
        // Sender field claims id2's key but the message is signed by id1.
        let msg = SignalingMessage::new(MessageType::Ping, &id2.public_id(), serde_json::json!({}))
            .sign(&id1);
        assert!(!msg.verify());
    }

    #[test]
    fn verify_returns_false_for_tampered_payload() {
        let id = Identity::generate();
        let mut msg = SignalingMessage::new(
            MessageType::ChatMessage,
            &id.public_id(),
            serde_json::json!({"body": "hello"}),
        )
        .sign(&id);
        assert!(msg.verify(), "sanity: fresh signature must verify");
        // Tamper the payload after signing.
        msg.payload = serde_json::json!({"body": "TAMPERED"});
        assert!(!msg.verify());
    }

    #[test]
    fn verify_returns_false_for_tampered_sender() {
        let id = Identity::generate();
        let impersonator = Identity::generate();
        let mut msg = SignalingMessage::new(
            MessageType::ChatMessage,
            &id.public_id(),
            serde_json::json!({}),
        )
        .sign(&id);
        assert!(msg.verify(), "sanity: fresh signature must verify");
        // Replace sender — canonical bytes change, signature no longer matches.
        msg.sender = impersonator.public_id();
        assert!(!msg.verify());
    }

    #[test]
    fn verify_returns_false_for_truncated_signature() {
        let id = Identity::generate();
        let mut msg =
            SignalingMessage::new(MessageType::Ping, &id.public_id(), serde_json::json!({}))
                .sign(&id);
        // Replace 64-byte signature with 16 zero bytes.
        msg.signature = Some(b64url_encode(&[0u8; 16]));
        assert!(!msg.verify());
    }

    #[test]
    fn verify_returns_false_for_tampered_target() {
        let id = Identity::generate();
        let mut msg = SignalingMessage::new(
            MessageType::ChatMessage,
            &id.public_id(),
            serde_json::json!({}),
        )
        .with_target("peer-a")
        .sign(&id);
        assert!(msg.verify(), "sanity: fresh signature must verify");
        // target is included in canonical bytes — redirect must break the sig.
        msg.target = Some("peer-b".into());
        assert!(!msg.verify());
    }
}
