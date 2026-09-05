// ConquerD Supernode — handshake.rs
// Invite creation/validation and handshake protocol.
// Handles INVITE_HANDSHAKE_INIT and produces INVITE_HANDSHAKE_ACCEPT.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::{b64url_decode, b64url_encode, generate_nonce_hex, hkdf_sha256, sha256_hex};
use crate::identity::{generate_x25519_keypair, x25519_exchange, Identity};

const SESSION_KEY_INFO: &[u8] = b"conquerd-invite-session-v2";

/// An invite payload for the Conquerd invite/handshake protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitePayload {
    pub inviter_peer_id: String,
    pub inviter_identity_pub: String,
    pub invite_id: String,
    pub expires_at: i64,
    pub inviter_ephemeral_pub: String, // base64url X25519 public key
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inviter_handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp_hole_punch_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lan_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_supernode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supernode_info: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supernode_relay_hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stun_hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl InvitePayload {
    /// Canonical bytes for signing: JSON with signature removed, all fields
    /// alphabetically sorted and serialised (None fields as null), ensuring
    /// consistent signatures across all clients.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        // Build a BTreeMap (alphabetically sorted) with all fields, including None→null.
        let mut map = serde_json::Map::new();
        map.insert("expires_at".into(), serde_json::json!(self.expires_at));
        map.insert("invite_id".into(), serde_json::json!(&self.invite_id));
        map.insert(
            "inviter_ephemeral_pub".into(),
            serde_json::json!(&self.inviter_ephemeral_pub),
        );
        map.insert(
            "inviter_handle".into(),
            match &self.inviter_handle {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "inviter_identity_pub".into(),
            serde_json::json!(&self.inviter_identity_pub),
        );
        map.insert(
            "inviter_peer_id".into(),
            serde_json::json!(&self.inviter_peer_id),
        );
        map.insert(
            "is_supernode".into(),
            match &self.is_supernode {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "lan_hint".into(),
            match &self.lan_hint {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "relay_hint".into(),
            match &self.relay_hint {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        // signature excluded
        map.insert(
            "stun_hints".into(),
            match &self.stun_hints {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "supernode_info".into(),
            match &self.supernode_info {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "supernode_relay_hints".into(),
            match &self.supernode_relay_hints {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "turn_hints".into(),
            match &self.turn_hints {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        map.insert(
            "udp_hole_punch_hint".into(),
            match &self.udp_hole_punch_hint {
                Some(v) => serde_json::json!(v),
                None => serde_json::Value::Null,
            },
        );
        serde_json::to_vec(&serde_json::Value::Object(map)).unwrap()
    }

    /// Sign the invite payload.
    pub fn sign(mut self, identity: &Identity) -> Self {
        let canonical = self.canonical_bytes();
        let sig = identity.sign(&canonical);
        self.signature = Some(b64url_encode(&sig));
        self
    }

    /// Verify the signature.
    #[cfg_attr(not(test), expect(dead_code, reason = "exercised by unit tests only"))]
    pub fn verify(&self) -> bool {
        let Some(ref sig_b64) = self.signature else {
            return false;
        };
        let Ok(sig_bytes) = b64url_decode(sig_b64) else {
            return false;
        };
        let Ok(pub_bytes) = b64url_decode(&self.inviter_identity_pub) else {
            return false;
        };
        let canonical = self.canonical_bytes();
        Identity::verify_with_pub(&pub_bytes, &sig_bytes, &canonical)
    }

    /// Encode as conquerd:// URI.
    pub fn to_uri(&self) -> String {
        let json = serde_json::to_string(self).unwrap();
        let encoded = b64url_encode(json.as_bytes());
        format!("conquerd://{encoded}")
    }
}

/// State for a pending handshake (one per invite_id), keyed by `invite_id`
/// in `HandshakeManager::pending`.
pub struct PendingInvite {
    pub ephemeral_secret: x25519_dalek::StaticSecret,
    pub ephemeral_public: x25519_dalek::PublicKey,
    /// Unix timestamp after which this invite is no longer valid.
    pub expires_at: i64,
}

/// Manages invite creation and handshake processing for the supernode.
pub struct HandshakeManager {
    identity: Identity,
    listener_url: String,
    /// invite_id → PendingInvite
    pending: HashMap<String, PendingInvite>,
    /// Reusable invite (if any)
    pub reusable_invite: Option<InvitePayload>,
    invite_ttl_seconds: i64,
    /// TURN relay hints embedded in invites (e.g. ["turn:1.2.3.4:3478"])
    pub turn_hints: Option<Vec<String>>,
    /// Human-readable name for this supernode (sent as inviter_handle in accept).
    pub node_title: String,
}

impl HandshakeManager {
    pub fn new(identity: Identity, listener_url: String, invite_ttl_seconds: i64) -> Self {
        Self {
            identity,
            listener_url,
            pending: HashMap::new(),
            reusable_invite: None,
            invite_ttl_seconds,
            turn_hints: None,
            node_title: "Supernode".into(),
        }
    }

    /// Create a new invite and store the ephemeral key.
    pub fn create_invite(&mut self, handle: Option<&str>) -> InvitePayload {
        let invite_id = generate_nonce_hex(16);
        let (secret, public) = generate_x25519_keypair();

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let expires_at = if self.invite_ttl_seconds < 0 {
            now + 100 * 365 * 86400 // ~100 years
        } else {
            now + self.invite_ttl_seconds
        };

        let payload = InvitePayload {
            inviter_peer_id: self.identity.peer_id(),
            inviter_identity_pub: self.identity.public_id(),
            invite_id: invite_id.clone(),
            expires_at,
            inviter_ephemeral_pub: b64url_encode(public.as_bytes()),
            relay_hint: Some(self.listener_url.clone()),
            inviter_handle: handle.map(String::from),
            udp_hole_punch_hint: None,
            lan_hint: None,
            is_supernode: Some(true),
            supernode_info: None,
            supernode_relay_hints: None,
            stun_hints: None,
            turn_hints: self.turn_hints.clone(),
            signature: None,
        }
        .sign(&self.identity);

        self.pending.insert(
            invite_id.clone(),
            PendingInvite {
                ephemeral_secret: secret,
                ephemeral_public: public,
                expires_at,
            },
        );

        payload
    }

    /// Create (or recall) a reusable invite.
    pub fn get_or_create_reusable_invite(&mut self, handle: Option<&str>) -> InvitePayload {
        if let Some(ref inv) = self.reusable_invite {
            return inv.clone();
        }
        let inv = self.create_invite(handle);
        self.reusable_invite = Some(inv.clone());
        inv
    }

    /// Save the reusable invite (including ephemeral secret) to disk so it
    /// survives supernode restarts.  File: `<data_dir>/reusable_invite.json`.
    pub fn save_reusable_invite(&self, data_dir: &Path) {
        let Some(ref inv) = self.reusable_invite else {
            return;
        };
        let Some(pending) = self.pending.get(&inv.invite_id) else {
            return;
        };
        let secret_bytes: &[u8; 32] = pending.ephemeral_secret.as_bytes();
        let obj = serde_json::json!({
            "invite": inv,
            "ephemeral_secret": b64url_encode(secret_bytes),
        });
        let path = data_dir.join("reusable_invite.json");
        if let Ok(data) = serde_json::to_string_pretty(&obj) {
            if let Err(e) = std::fs::write(&path, data) {
                tracing::warn!("Failed to save reusable invite: {}", e);
            } else {
                tracing::info!("Reusable invite persisted to {}", path.display());
            }
        }
    }

    /// Load a previously persisted reusable invite from disk, restoring both
    /// the invite payload and its ephemeral key into the pending map.
    /// Returns `true` if the invite was restored successfully.
    pub fn load_reusable_invite(&mut self, data_dir: &Path) -> bool {
        let path = data_dir.join("reusable_invite.json");
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return false,
        };
        let parsed: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to parse reusable invite: {}", e);
                return false;
            }
        };
        let invite: InvitePayload =
            match serde_json::from_value(parsed.get("invite").cloned().unwrap_or_default()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Failed to deserialize reusable invite payload: {}", e);
                    return false;
                }
            };
        let secret_b64 = match parsed.get("ephemeral_secret").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                tracing::warn!("Missing ephemeral_secret in reusable invite file");
                return false;
            }
        };
        let secret_bytes = match b64url_decode(secret_b64) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                tracing::warn!("Invalid ephemeral_secret in reusable invite file");
                return false;
            }
        };

        // Restore the pending entry so handshake can complete
        self.restore_pending(invite.invite_id.clone(), secret_bytes, invite.expires_at);
        self.reusable_invite = Some(invite);
        tracing::info!("Restored reusable invite from {}", path.display());
        true
    }

    /// Process an INVITE_HANDSHAKE_INIT from a joiner.
    /// Returns (accept_payload, session_key, joiner_identity_pub) on success.
    pub fn process_init(&self, payload: &Value) -> Result<(Value, Vec<u8>, String), String> {
        let invite_id = payload
            .get("invite_id")
            .and_then(|v| v.as_str())
            .ok_or("missing invite_id")?;

        let pending = self
            .pending
            .get(invite_id)
            .ok_or_else(|| format!("unknown invite_id: {invite_id}"))?;

        // Reject expired invites.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        if pending.expires_at < now_secs {
            return Err("invite_expired".into());
        }

        let joiner_identity_pub = payload
            .get("joiner_identity_pub")
            .and_then(|v| v.as_str())
            .ok_or("missing joiner_identity_pub")?;
        let _joiner_peer_id = payload
            .get("joiner_peer_id")
            .and_then(|v| v.as_str())
            .ok_or("missing joiner_peer_id")?;

        // X25519 ECDH is required — no legacy path without joiner_ephemeral_pub.
        let joiner_ephemeral_pub_b64 = payload
            .get("joiner_ephemeral_pub")
            .and_then(|v| v.as_str())
            .ok_or("missing joiner_ephemeral_pub")?;
        // Decode joiner's ephemeral X25519 public key
        let joiner_eph_bytes =
            b64url_decode(joiner_ephemeral_pub_b64).map_err(|e| e.to_string())?;
        if joiner_eph_bytes.len() != 32 {
            return Err("joiner_ephemeral_pub must be 32 bytes".into());
        }
        let mut eph_arr = [0u8; 32];
        eph_arr.copy_from_slice(&joiner_eph_bytes);
        let joiner_eph_pub = x25519_dalek::PublicKey::from(eph_arr);

        // ECDH
        let shared_secret = x25519_exchange(&pending.ephemeral_secret, &joiner_eph_pub);
        // A zero shared secret means the joiner supplied a low-order X25519 point
        // (small-subgroup attack).  Abort before deriving any key material.
        if shared_secret.iter().all(|&b| b == 0) {
            return Err("low-order X25519 point rejected".into());
        }

        // Transcript
        let transcript = serde_json::json!({
            "invite_id": invite_id,
            "inviter_ephemeral_pub": b64url_encode(pending.ephemeral_public.as_bytes()),
            "inviter_identity_pub": self.identity.public_id(),
            "joiner_ephemeral_pub": joiner_ephemeral_pub_b64,
            "joiner_identity_pub": joiner_identity_pub,
        });
        let transcript_bytes = serde_json::to_vec(&transcript).map_err(|e| e.to_string())?;
        let transcript_hash = sha256_hex(&transcript_bytes);

        // Session key via HKDF
        let mut ikm = Vec::new();
        ikm.extend_from_slice(&(shared_secret.len() as u32).to_be_bytes());
        ikm.extend_from_slice(&shared_secret);
        ikm.extend_from_slice(&(transcript_bytes.len() as u32).to_be_bytes());
        ikm.extend_from_slice(&transcript_bytes);
        let session_key = hkdf_sha256(&ikm, SESSION_KEY_INFO, 32);

        // Build INVITE_HANDSHAKE_ACCEPT payload
        let accept_payload = serde_json::json!({
            "invite_id": invite_id,
            "inviter_peer_id": self.identity.peer_id(),
            "inviter_identity_pub": self.identity.public_id(),
            "inviter_listener": self.listener_url,
            "inviter_quic_port": 0,
            "transcript_hash": transcript_hash,
            "inviter_handle": self.node_title.as_str(),
        });

        Ok((accept_payload, session_key, joiner_identity_pub.to_string()))
    }

    /// Restore a pending invite from persisted state.
    pub fn restore_pending(
        &mut self,
        invite_id: String,
        ephemeral_secret_bytes: [u8; 32],
        expires_at: i64,
    ) {
        let secret = x25519_dalek::StaticSecret::from(ephemeral_secret_bytes);
        let public = x25519_dalek::PublicKey::from(&secret);
        self.pending.insert(
            invite_id,
            PendingInvite {
                ephemeral_secret: secret,
                ephemeral_public: public,
                expires_at,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invite_create_and_uri() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let inv = mgr.create_invite(Some("TestNode"));
        assert!(inv.verify());
        let uri = inv.to_uri();
        assert!(uri.starts_with("conquerd://"));
    }

    #[test]
    fn test_invite_canonical_bytes_includes_all_fields() {
        // Verify canonical_bytes includes null-valued optional fields in sorted order,
        // canonical: JSON with signature removed, keys sorted, no extra whitespace
        // with signature removed.
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let inv = mgr.create_invite(Some("TestNode"));
        let canonical = std::str::from_utf8(&inv.canonical_bytes())
            .unwrap()
            .to_string();
        println!("Invite canonical: {}", canonical);
        // Must not contain "signature"
        assert!(
            !canonical.contains("\"signature\""),
            "canonical must not contain signature"
        );
        // Must contain null fields for missing optionals
        assert!(
            canonical.contains("\"udp_hole_punch_hint\":null"),
            "must include udp_hole_punch_hint:null"
        );
        assert!(
            canonical.contains("\"lan_hint\":null"),
            "must include lan_hint:null"
        );
        assert!(
            canonical.contains("\"supernode_relay_hints\":null"),
            "must include supernode_relay_hints:null"
        );
        // Keys must be in sorted order — verify a few orderings
        // inviter_ephemeral_pub < inviter_handle < inviter_identity_pub < inviter_peer_id < is_supernode < lan_hint
        let ep = canonical.find("\"inviter_ephemeral_pub\"").unwrap();
        let ih = canonical.find("\"inviter_handle\"").unwrap();
        let ii = canonical.find("\"inviter_identity_pub\"").unwrap();
        let ip = canonical.find("\"inviter_peer_id\"").unwrap();
        assert!(ep < ih && ih < ii && ii < ip, "keys not in sorted order");
        // Verify still verifiable
        assert!(
            inv.verify(),
            "invite should still verify after canonical_bytes change"
        );
    }

    #[test]
    fn test_handshake_flow() {
        let inviter_id = Identity::generate();
        let joiner_id = Identity::generate();

        let mut mgr = HandshakeManager::new(inviter_id.clone(), "ws://127.0.0.1:34935".into(), -1);
        let invite = mgr.create_invite(Some("TestNode"));

        // Joiner creates ephemeral keypair
        let (joiner_secret, joiner_public) = generate_x25519_keypair();

        // Joiner sends init
        let init_payload = serde_json::json!({
            "invite_id": invite.invite_id,
            "joiner_peer_id": joiner_id.peer_id(),
            "joiner_identity_pub": joiner_id.public_id(),
            "joiner_ephemeral_pub": b64url_encode(joiner_public.as_bytes()),
            "joiner_listener": "ws://192.168.1.2:12345",
            "joiner_handle": "Alice",
            "joiner_quic_port": 0,
        });

        let (accept_payload, session_key, joiner_pub) = mgr.process_init(&init_payload).unwrap();
        assert_eq!(joiner_pub, joiner_id.public_id());
        assert_eq!(session_key.len(), 32);
        assert!(accept_payload.get("transcript_hash").is_some());

        // Verify joiner can derive same session key
        let inv_eph_bytes = b64url_decode(&invite.inviter_ephemeral_pub).unwrap();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&inv_eph_bytes);
        let inviter_eph_pub = x25519_dalek::PublicKey::from(arr);
        let shared = x25519_exchange(&joiner_secret, &inviter_eph_pub);

        let transcript = serde_json::json!({
            "invite_id": invite.invite_id,
            "inviter_ephemeral_pub": invite.inviter_ephemeral_pub,
            "inviter_identity_pub": inviter_id.public_id(),
            "joiner_ephemeral_pub": b64url_encode(joiner_public.as_bytes()),
            "joiner_identity_pub": joiner_id.public_id(),
        });
        let transcript_bytes = serde_json::to_vec(&transcript).unwrap();

        let mut ikm = Vec::new();
        ikm.extend_from_slice(&(shared.len() as u32).to_be_bytes());
        ikm.extend_from_slice(&shared);
        ikm.extend_from_slice(&(transcript_bytes.len() as u32).to_be_bytes());
        ikm.extend_from_slice(&transcript_bytes);
        let joiner_key = hkdf_sha256(&ikm, SESSION_KEY_INFO, 32);
        assert_eq!(joiner_key, session_key);
    }

    // --- Invite expiry & TTL tests ---

    #[test]
    fn process_init_rejects_expired_invite() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        // Restore a pending invite whose expiry is Unix epoch (long past).
        mgr.restore_pending("expired-inv-001".into(), [0u8; 32], 0);

        let (_, joiner_public) = generate_x25519_keypair();
        let init_payload = serde_json::json!({
            "invite_id": "expired-inv-001",
            "joiner_peer_id": "peer-joiner",
            "joiner_identity_pub": b64url_encode(&[1u8; 32]),
            "joiner_ephemeral_pub": b64url_encode(joiner_public.as_bytes()),
        });
        let err = mgr.process_init(&init_payload).unwrap_err();
        assert_eq!(err, "invite_expired");
    }

    #[test]
    fn process_init_rejects_unknown_invite_id() {
        let id = Identity::generate();
        let mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let init_payload = serde_json::json!({
            "invite_id": "no-such-invite-xyz",
            "joiner_peer_id": "peer-joiner",
            "joiner_identity_pub": b64url_encode(&[1u8; 32]),
            "joiner_ephemeral_pub": b64url_encode(&[2u8; 32]),
        });
        let err = mgr.process_init(&init_payload).unwrap_err();
        assert!(err.contains("unknown invite_id"), "got: {err}");
    }

    #[test]
    fn process_init_rejects_low_order_ephemeral_key() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let invite = mgr.create_invite(None);

        // The all-zero X25519 public key is a low-order (small-subgroup) point.
        // DH with it always produces a zero shared secret.
        let init_payload = serde_json::json!({
            "invite_id": invite.invite_id,
            "joiner_peer_id": "peer-joiner",
            "joiner_identity_pub": b64url_encode(&[1u8; 32]),
            "joiner_ephemeral_pub": b64url_encode(&[0u8; 32]),
        });
        let err = mgr.process_init(&init_payload).unwrap_err();
        assert_eq!(err, "low-order X25519 point rejected");
    }

    #[test]
    fn process_init_rejects_missing_ephemeral_key() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let invite = mgr.create_invite(Some("TestNode"));
        let init_payload = serde_json::json!({
            "invite_id": invite.invite_id,
            "joiner_peer_id": "peer-joiner",
            "joiner_identity_pub": "JoinerPubIdExample",
        });
        let err = mgr.process_init(&init_payload).unwrap_err();
        assert_eq!(err, "missing joiner_ephemeral_pub");
    }

    #[test]
    fn process_init_rejects_missing_fields() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let invite = mgr.create_invite(None);

        // Missing joiner_identity_pub.
        let payload = serde_json::json!({
            "invite_id": invite.invite_id,
            "joiner_peer_id": "peer-joiner",
            "joiner_ephemeral_pub": b64url_encode(&[2u8; 32]),
        });
        assert!(mgr.process_init(&payload).is_err());

        // Missing invite_id.
        let payload2 = serde_json::json!({
            "joiner_peer_id": "peer-joiner",
            "joiner_identity_pub": b64url_encode(&[1u8; 32]),
            "joiner_ephemeral_pub": b64url_encode(&[2u8; 32]),
        });
        assert!(mgr.process_init(&payload2).is_err());
    }

    #[test]
    fn tampered_expires_at_invalidates_invite_signature() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), 3600);
        let mut inv = mgr.create_invite(None);
        assert!(inv.verify(), "fresh invite must verify");
        // Extend expiry — canonical bytes include expires_at, so sig must break.
        inv.expires_at += 86_400;
        assert!(
            !inv.verify(),
            "tampered expires_at must invalidate signature"
        );
    }

    #[test]
    fn invite_verify_returns_false_with_no_signature() {
        let id = Identity::generate();
        let mut mgr = HandshakeManager::new(id, "ws://127.0.0.1:34935".into(), -1);
        let mut inv = mgr.create_invite(None);
        inv.signature = None;
        assert!(!inv.verify(), "invite without signature must not verify");
    }
}
