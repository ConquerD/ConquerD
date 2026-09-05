// ConquerD Supernode — ticket.rs
// Relay ticket creation, signing, validation.

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default ticket lifetime (1 hour).
pub const TICKET_TTL_S: f64 = 3600.0;
/// Renewal window: renew when ≤ 600s remaining.
pub const RENEWAL_WINDOW_S: f64 = 600.0;

/// Ed25519-signed relay ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayTicket {
    pub peer_id: String, // base64url Ed25519 pub key
    pub relay_host: String,
    pub relay_port: u16,
    pub expires_at: f64,
    #[serde(serialize_with = "ser_b64", deserialize_with = "de_b64", default)]
    pub signature: Vec<u8>,
}

fn ser_b64<S: serde::Serializer>(data: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    s.serialize_str(&STANDARD.encode(data))
}

fn de_b64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let s = String::deserialize(d)?;
    STANDARD.decode(&s).map_err(serde::de::Error::custom)
}

impl RelayTicket {
    /// Canonical bytes for signing: "peer_id:relay_host:relay_port:expires_at"
    pub fn canonical_bytes(&self) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}",
            self.peer_id, self.relay_host, self.relay_port, self.expires_at
        )
        .into_bytes()
    }

    /// Create and sign a new ticket.
    pub fn create(
        peer_id: &str,
        relay_host: &str,
        relay_port: u16,
        signing_key: &SigningKey,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let mut ticket = Self {
            peer_id: peer_id.to_string(),
            relay_host: relay_host.to_string(),
            relay_port,
            expires_at: now + TICKET_TTL_S,
            signature: Vec::new(),
        };
        let sig = signing_key.sign(&ticket.canonical_bytes());
        ticket.signature = sig.to_bytes().to_vec();
        ticket
    }

    /// Verify ticket signature against the supernode's public key.
    #[cfg_attr(not(test), expect(dead_code, reason = "exercised by unit tests only"))]
    pub fn verify(&self, verifying_key: &VerifyingKey) -> bool {
        if self.signature.len() != 64 {
            return false;
        }
        let sig_bytes: [u8; 64] = match self.signature.as_slice().try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        verifying_key.verify(&self.canonical_bytes(), &sig).is_ok()
    }

    /// Check if ticket has expired.
    #[cfg_attr(not(test), expect(dead_code, reason = "exercised by unit tests only"))]
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        self.expires_at < now
    }

    /// Check if ticket needs renewal (within RENEWAL_WINDOW_S of expiry).
    #[cfg_attr(not(test), expect(dead_code, reason = "exercised by unit tests only"))]
    pub fn needs_renewal(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        (self.expires_at - now) <= RENEWAL_WINDOW_S
    }

    /// Convert to JSON-safe dict for protocol messages.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::b64url_encode;

    #[test]
    fn test_ticket_create_verify() {
        let signing_key = SigningKey::generate(&mut rand::thread_rng());
        let verifying_key = signing_key.verifying_key();
        let peer_pub_id = b64url_encode(verifying_key.as_bytes());

        let ticket = RelayTicket::create(&peer_pub_id, "127.0.0.1", 3478, &signing_key);
        assert!(ticket.verify(&verifying_key));
        assert!(!ticket.is_expired());
    }

    #[test]
    fn test_ticket_expired() {
        let signing_key = SigningKey::generate(&mut rand::thread_rng());
        let mut ticket = RelayTicket::create("test", "127.0.0.1", 3478, &signing_key);
        ticket.expires_at = 0.0; // Force expired
        assert!(ticket.is_expired());
    }

    #[test]
    fn verify_returns_false_for_wrong_key() {
        let key1 = SigningKey::generate(&mut rand::thread_rng());
        let key2 = SigningKey::generate(&mut rand::thread_rng());
        let ticket = RelayTicket::create("peer", "127.0.0.1", 3478, &key1);
        assert!(!ticket.verify(&key2.verifying_key()));
    }

    #[test]
    fn verify_returns_false_for_short_signature() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let mut ticket = RelayTicket::create("peer", "127.0.0.1", 3478, &key);
        ticket.signature = vec![0u8; 32]; // too short
        assert!(!ticket.verify(&key.verifying_key()));
    }

    #[test]
    fn verify_returns_false_for_tampered_field() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let verifying = key.verifying_key();
        let mut ticket = RelayTicket::create("peer", "relay.example.com", 3478, &key);
        ticket.relay_port = 9999; // tamper after signing
        assert!(!ticket.verify(&verifying));
    }

    #[test]
    fn canonical_bytes_contains_all_fields() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let ticket = RelayTicket::create("mypeer", "relay.host", 1234, &key);
        let bytes = ticket.canonical_bytes();
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("mypeer"));
        assert!(s.contains("relay.host"));
        assert!(s.contains("1234"));
    }

    #[test]
    fn to_value_round_trips_fields() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let ticket = RelayTicket::create("peer-x", "relay.example.com", 3478, &key);
        let v = ticket.to_value();
        assert_eq!(v["peer_id"], "peer-x");
        assert_eq!(v["relay_host"], "relay.example.com");
        assert_eq!(v["relay_port"], 3478);
    }

    #[test]
    fn needs_renewal_when_close_to_expiry() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let mut ticket = RelayTicket::create("peer", "host", 1234, &key);
        // Set expiry to 300s from now (within the 600s renewal window)
        ticket.expires_at = now + 300.0;
        assert!(ticket.needs_renewal());
    }

    #[test]
    fn does_not_need_renewal_when_fresh() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let mut ticket = RelayTicket::create("peer", "host", 1234, &key);
        // Set expiry to 3600s from now (outside the 600s renewal window)
        ticket.expires_at = now + 3600.0;
        assert!(!ticket.needs_renewal());
    }

    #[test]
    fn not_expired_when_fresh() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let ticket = RelayTicket::create("peer", "host", 1234, &key);
        assert!(!ticket.is_expired());
    }

    /// P0 smoke test for relay ticket renewal (the exact flow a QuicRelayClient
    /// performs on connect and then periodically).
    #[test]
    fn p0_smoke_relay_ticket_renewal_flow() {
        let key = SigningKey::generate(&mut rand::thread_rng());
        let verifying = key.verifying_key();
        let peer = b64url_encode(verifying.as_bytes());

        let mut ticket = RelayTicket::create(&peer, "127.0.0.1", 3478, &key);
        assert!(ticket.verify(&verifying));
        assert!(!ticket.needs_renewal()); // fresh ticket

        // Simulate time passing close to expiry (client would call renew)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        ticket.expires_at = now + 400.0; // inside 600s window
        assert!(ticket.needs_renewal());

        // Client would request a new ticket here (simulated)
        let renewed = RelayTicket::create(&peer, "127.0.0.1", 3478, &key);
        assert!(renewed.verify(&verifying));
        assert!(!renewed.needs_renewal());
        assert!(!renewed.is_expired());
    }
}
