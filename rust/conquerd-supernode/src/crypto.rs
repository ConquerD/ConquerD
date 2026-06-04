// ConquerD Supernode — crypto.rs
// Shared crypto primitives: base64url, SHA-256, HKDF, nonce generation.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

/// Base64url encode (no padding).
pub fn b64url_encode(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

/// Base64url decode (handles missing padding).
pub fn b64url_decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
    // Add padding if needed
    let padded = match s.len() % 4 {
        2 => format!("{s}=="),
        3 => format!("{s}="),
        _ => s.to_string(),
    };
    URL_SAFE_NO_PAD.decode(padded.trim_end_matches('='))
}

/// SHA-256 hash returning raw bytes.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

/// SHA-256 hash returning lowercase hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha256(data))
}

/// Derive peer_id from Ed25519 public key bytes (SHA-256 hex).
pub fn derive_peer_id(pub_key_bytes: &[u8]) -> String {
    sha256_hex(pub_key_bytes)
}

/// Derive room_id from creator public_id and room name (first 16 chars of SHA-256 hex).
pub fn derive_room_id(creator_pub_id: &str, room_name: &str) -> String {
    let input = format!("{creator_pub_id}:{room_name}");
    sha256_hex(input.as_bytes())[..16].to_string()
}

/// Generate random bytes.
pub fn generate_nonce(length: usize) -> Vec<u8> {
    use rand::RngCore;
    let mut buf = vec![0u8; length];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// Generate random hex string.
pub fn generate_nonce_hex(length: usize) -> String {
    hex::encode(generate_nonce(length))
}

/// HKDF-SHA256 key derivation.
pub fn hkdf_sha256(ikm: &[u8], info: &[u8], length: usize) -> Vec<u8> {
    use hkdf::Hkdf;
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = vec![0u8; length];
    hk.expand(info, &mut okm).expect("HKDF expand failed");
    okm
}

/// HMAC-SHA256 for web session tokens.
/// Used by future portal session management; suppressed until wired up.
#[allow(dead_code)]
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length");
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// Verify HMAC-SHA256 (constant-time).
/// Used by future portal session management; suppressed until wired up.
#[allow(dead_code)]
pub fn hmac_sha256_verify(key: &[u8], data: &[u8], expected: &[u8]) -> bool {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC key length");
    mac.update(data);
    mac.verify_slice(expected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_b64url_roundtrip() {
        let data = b"hello world";
        let encoded = b64url_encode(data);
        let decoded = b64url_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_sha256_hex() {
        let hash = sha256_hex(b"test");
        assert_eq!(hash.len(), 64);
        assert_eq!(
            hash,
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
    }

    #[test]
    fn test_derive_peer_id() {
        let pub_key = [0u8; 32];
        let pid = derive_peer_id(&pub_key);
        assert_eq!(pid.len(), 64);
    }

    #[test]
    fn test_hmac_sha256() {
        let key = b"secret";
        let data = b"message";
        let mac = hmac_sha256(key, data);
        assert!(hmac_sha256_verify(key, data, &mac));
        assert!(!hmac_sha256_verify(key, b"wrong", &mac));
    }
}
