//! Cryptographic helpers for conquerd-client.
//!
//! Shared primitives for identity, encryption, and key derivation;
//! on-disk data (identity files, peer store, chat store) is fully compatible.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use crate::error::{ClientError, Result};

// ---------------------------------------------------------------------------
// Constants — must match conquerd-crypto/src/store.rs
// ---------------------------------------------------------------------------

const ENVELOPE_VERSION: u8 = 0x01;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 1 + NONCE_LEN; // version || nonce
const KEY_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Encrypted-at-rest envelope  (AES-256-GCM)
// ---------------------------------------------------------------------------

/// Encrypt `plaintext` into a versioned AES-256-GCM envelope.
///
/// Envelope format:
/// ```text
/// [0x01][12-byte nonce][ciphertext || 16-byte GCM tag]
/// ```
/// This is wire-compatible with `conquerd_crypto.EncryptedStore.encrypt_blob`.
pub fn encrypt_blob(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    if key.len() != KEY_LEN {
        return Err(ClientError::Crypto(format!(
            "key must be {KEY_LEN} bytes, got {}",
            key.len()
        )));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| ClientError::Crypto("AES-GCM encryption failed".into()))?;
    let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
    out.push(ENVELOPE_VERSION);
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypt a versioned AES-256-GCM envelope produced by [`encrypt_blob`].
pub fn decrypt_blob(key: &[u8], envelope: &[u8]) -> Result<Vec<u8>> {
    if key.len() != KEY_LEN {
        return Err(ClientError::Crypto(format!(
            "key must be {KEY_LEN} bytes, got {}",
            key.len()
        )));
    }
    if envelope.len() < HEADER_LEN {
        return Err(ClientError::Crypto("envelope too short".into()));
    }
    if envelope[0] != ENVELOPE_VERSION {
        return Err(ClientError::Crypto(format!(
            "unsupported envelope version 0x{:02x}",
            envelope[0]
        )));
    }
    let nonce = &envelope[1..HEADER_LEN];
    let ct = &envelope[HEADER_LEN..];
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| ClientError::Crypto("AES-GCM decryption failed".into()))
}

/// Encrypt plaintext bytes with a raw AES-256-GCM key, providing associated data.
///
/// Returns `nonce || ciphertext_with_tag` (no envelope version byte).
/// Used for the identity file v2 format which stores nonce separately.
pub fn aesgcm_encrypt(key: &[u8], plaintext: &[u8], aad: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if key.len() != KEY_LEN {
        return Err(ClientError::Crypto("key must be 32 bytes".into()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    use aes_gcm::aead::Payload;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| ClientError::Crypto("AES-GCM encryption failed".into()))?;
    Ok((nonce_bytes.to_vec(), ct))
}

/// Decrypt using a raw nonce + ciphertext (identity file v2 format).
pub fn aesgcm_decrypt(key: &[u8], nonce: &[u8], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if key.len() != KEY_LEN {
        return Err(ClientError::Crypto("key must be 32 bytes".into()));
    }
    if nonce.len() != NONCE_LEN {
        return Err(ClientError::Crypto("nonce must be 12 bytes".into()));
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    use aes_gcm::aead::Payload;
    cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| ClientError::Crypto("AES-GCM decryption failed — wrong key?".into()))
}

// ---------------------------------------------------------------------------
// HKDF-SHA256 key derivation
// ---------------------------------------------------------------------------

/// Derive a 32-byte subkey via HKDF-SHA256.
///
/// `ikm` is the input key material (e.g. Ed25519 seed).
/// `info` is a domain-separation label.
///
/// Matches `Identity.derive_store_key` in conquerd-crypto/src/identity.rs.
pub fn hkdf_derive_key(ikm: &[u8], info: &[u8]) -> Result<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut okm = [0u8; 32];
    hk.expand(info, &mut okm)
        .map_err(|e| ClientError::Crypto(format!("HKDF expand failed: {e}")))?;
    Ok(okm)
}

// ---------------------------------------------------------------------------
// SHA-256 helpers
// ---------------------------------------------------------------------------

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha256(data))
}

// ---------------------------------------------------------------------------
// base64url helpers
// ---------------------------------------------------------------------------

pub fn b64url_encode(data: &[u8]) -> String {
    URL_SAFE.encode(data)
}

pub fn b64url_encode_nopad(data: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(data)
}

const INVITE_SESSION_KEY_INFO: &[u8] = b"conquerd-invite-session-v2";

/// Ephemeral X25519 keypair for invite handshake (joiner side).
pub struct EphemeralKeyPair {
    pub secret: StaticSecret,
    pub public: PublicKey,
}

pub fn generate_ephemeral_keypair() -> EphemeralKeyPair {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = PublicKey::from(&secret);
    EphemeralKeyPair { secret, public }
}

pub fn x25519_exchange(secret: &StaticSecret, peer_public: &PublicKey) -> [u8; 32] {
    secret.diffie_hellman(peer_public).to_bytes()
}

/// Derive the invite session key on the joiner side (matches supernode handshake).
pub fn derive_invite_session_key(
    joiner_secret: &StaticSecret,
    inviter_ephemeral_pub_b64: &str,
    invite_id: &str,
    inviter_identity_pub: &str,
    joiner_identity_pub: &str,
    joiner_ephemeral_pub_b64: &str,
) -> Result<([u8; 32], String)> {
    let inv_eph_bytes = b64url_decode(inviter_ephemeral_pub_b64)?;
    if inv_eph_bytes.len() != 32 {
        return Err(ClientError::Crypto(
            "inviter_ephemeral_pub must be 32 bytes".into(),
        ));
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&inv_eph_bytes);
    let inviter_eph_pub = PublicKey::from(arr);
    let shared_secret = x25519_exchange(joiner_secret, &inviter_eph_pub);

    let transcript = serde_json::json!({
        "invite_id": invite_id,
        "inviter_ephemeral_pub": inviter_ephemeral_pub_b64,
        "inviter_identity_pub": inviter_identity_pub,
        "joiner_ephemeral_pub": joiner_ephemeral_pub_b64,
        "joiner_identity_pub": joiner_identity_pub,
    });
    let transcript_bytes = serde_json::to_vec(&transcript)
        .map_err(|e| ClientError::Crypto(format!("transcript serialize: {e}")))?;
    let transcript_hash = sha256_hex(&transcript_bytes);

    let mut ikm = Vec::new();
    ikm.extend_from_slice(&(shared_secret.len() as u32).to_be_bytes());
    ikm.extend_from_slice(&shared_secret);
    ikm.extend_from_slice(&(transcript_bytes.len() as u32).to_be_bytes());
    ikm.extend_from_slice(&transcript_bytes);
    let session_key = hkdf_derive_key(&ikm, INVITE_SESSION_KEY_INFO)?;
    Ok((session_key, transcript_hash))
}

pub fn b64url_decode(s: &str) -> Result<Vec<u8>> {
    // Try with padding first, then without
    URL_SAFE
        .decode(s)
        .or_else(|_| URL_SAFE_NO_PAD.decode(s))
        .map_err(|e| ClientError::Crypto(format!("base64url decode error: {e}")))
}

// ---------------------------------------------------------------------------
// Ed25519 wrappers
// ---------------------------------------------------------------------------

/// Derive the hex peer_id from a 32-byte Ed25519 public key.
///
/// Matches `Identity.peer_id` in conquerd-crypto: SHA-256 of the public key, hex-encoded.
pub fn derive_peer_id(public_key_bytes: &[u8]) -> String {
    hex::encode(sha256(public_key_bytes))
}

/// Derive the base64url public_id from a 32-byte Ed25519 public key.
///
/// Matches `Identity.public_id` in conquerd-crypto: URL_SAFE (with padding) base64 of public key.
pub fn derive_public_id(public_key_bytes: &[u8]) -> String {
    URL_SAFE.encode(public_key_bytes)
}

/// Sign `data` with a 32-byte Ed25519 seed, returning the 64-byte signature.
///
/// Returns an error if `seed` is not exactly 32 bytes.
pub fn ed25519_sign(seed: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let Ok(arr): std::result::Result<[u8; 32], _> = seed.try_into() else {
        return Err(ClientError::Crypto("seed must be 32 bytes".into()));
    };
    let signing_key = SigningKey::from_bytes(&arr);
    Ok(signing_key.sign(data).to_bytes().to_vec())
}

/// Verify an Ed25519 signature against `data` using the raw 32-byte public key.
///
/// Returns `false` if `public_key_bytes` is not exactly 32 bytes or
/// `signature_bytes` is not exactly 64 bytes. No silent fallbacks.
pub fn ed25519_verify(public_key_bytes: &[u8], signature_bytes: &[u8], data: &[u8]) -> bool {
    let Ok(pk_arr): std::result::Result<&[u8; 32], _> = public_key_bytes.try_into() else {
        return false;
    };
    let Ok(pk) = VerifyingKey::from_bytes(pk_arr) else {
        return false;
    };
    let Ok(sig_arr): std::result::Result<[u8; 64], _> = signature_bytes.try_into() else {
        return false;
    };
    let sig = Signature::from_bytes(&sig_arr);
    pk.verify(data, &sig).is_ok()
}

/// Hex-encoded Ed25519 public key of the ConquerD release signer.
/// Must be kept in sync with the one in conquerd-installer/src/release_manifest.rs.
const RELEASE_SIGNER_PUBKEY_HEX: &str =
    "d31f43fcfba1fae04313d384d7fba026bd52796550c57def6cf47b069c18043f";

/// Verify a claim that this binary is an official release build of the given
/// build_id and version (and optionally the exact source content hash).
///
/// The `release_sig_b64` (if present) should be a base64-encoded Ed25519 signature
/// produced by the release private key over a canonical claim:
///   `build_id=...,version=...,source_hash=...`
/// (source_hash is included when the official build provided one).
///
/// Returns true only if the release signer key is configured
/// and the signature verifies.
///
/// This is the main defense against an attacker who modifies sources and then
/// spoofs the build_id / source_hash via env var or post-build patching.
pub fn verify_official_release_build(
    build_id: &str,
    version: &str,
    source_hash: &str,
    release_sig_b64: Option<&str>,
) -> bool {
    if RELEASE_SIGNER_PUBKEY_HEX == "0".repeat(64) {
        // Safety net: no configured release signer key yet.
        return false;
    }
    let Some(sig_b64) = release_sig_b64 else {
        return false;
    };

    let pubkey_bytes: [u8; 32] = match hex::decode(RELEASE_SIGNER_PUBKEY_HEX) {
        Ok(b) if b.len() == 32 => match b.try_into() {
            Ok(arr) => arr,
            Err(_) => return false,
        },
        _ => return false,
    };

    let sig_bytes = match base64::engine::general_purpose::STANDARD.decode(sig_b64) {
        Ok(b) if b.len() == 64 => b,
        _ => return false,
    };

    // Canonical claim. Must match exactly what the release process signed.
    let claim = if !source_hash.is_empty() {
        format!("build_id={build_id},version={version},source_hash={source_hash}")
    } else {
        format!("build_id={build_id},version={version}")
    };

    ed25519_verify(&pubkey_bytes, &sig_bytes, claim.as_bytes())
}

/// Generate a cryptographically random nonce of `length` bytes.
pub fn generate_nonce(length: usize) -> Vec<u8> {
    let mut buf = vec![0u8; length];
    OsRng.fill_bytes(&mut buf);
    buf
}

/// Generate a hex-encoded nonce.
pub fn generate_nonce_hex(length: usize) -> String {
    hex::encode(generate_nonce(length))
}

// ---------------------------------------------------------------------------
// Argon2id key derivation (for passphrase-protected identity files)
// ---------------------------------------------------------------------------

/// Derive a 32-byte key from a passphrase using Argon2id.
///
/// Parameters: t=3, m=65536 (64 MiB), p=4.
pub fn argon2id_kdf(
    passphrase: &[u8],
    salt: &[u8],
    t_cost: u32,
    m_cost: u32,
    p_cost: u32,
) -> Result<[u8; 32]> {
    use argon2::{Algorithm, Argon2, Params, Version};
    let params = Params::new(m_cost, t_cost, p_cost, Some(32))
        .map_err(|e| ClientError::Crypto(format!("Argon2 params error: {e}")))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(passphrase, salt, &mut out)
        .map_err(|e| ClientError::Crypto(format!("Argon2 hash failed: {e}")))?;
    Ok(out)
}

/// Build combined Argon2id key material from a text passphrase and/or a keyfile.
///
/// Material = `text.as_bytes()` ++ `SHA-256(file_bytes)` (when `file_path` is non-empty).
/// Either argument may be empty, but both empty is an error.
/// The SHA-256 digest is a fixed 32 bytes regardless of file size.
pub fn build_passphrase_material(text: &str, file_path: &str) -> Result<Vec<u8>> {
    let mut material: Vec<u8> = Vec::new();
    material.extend_from_slice(text.as_bytes());
    if !file_path.is_empty() {
        let bytes = std::fs::read(file_path)
            .map_err(|e| ClientError::Crypto(format!("Cannot read keyfile '{file_path}': {e}")))?;
        let hash = Sha256::digest(&bytes);
        material.extend_from_slice(&hash);
    }
    if material.is_empty() {
        return Err(ClientError::Crypto(
            "Please enter a passphrase, choose a keyfile, or both.".to_string(),
        ));
    }
    Ok(material)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = generate_nonce(32);
        let plaintext = b"hello conquerd-client crypto";
        let blob = encrypt_blob(&key, plaintext).unwrap();
        let recovered = decrypt_blob(&key, &blob).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn hkdf_deterministic() {
        let seed = [0xABu8; 32];
        let k1 = hkdf_derive_key(&seed, b"conquerd-store/peers/v1").unwrap();
        let k2 = hkdf_derive_key(&seed, b"conquerd-store/peers/v1").unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn hkdf_domain_separation() {
        let seed = [0xABu8; 32];
        let k1 = hkdf_derive_key(&seed, b"conquerd-store/peers/v1").unwrap();
        let k2 = hkdf_derive_key(&seed, b"conquerd-store/chat/v1").unwrap();
        assert_ne!(k1, k2);
    }

    #[test]
    fn ed25519_sign_verify() {
        use ed25519_dalek::SigningKey;
        let key = SigningKey::generate(&mut OsRng);
        let seed = key.to_bytes();
        let data = b"test payload";
        let sig = ed25519_sign(&seed, data).unwrap();
        assert!(ed25519_verify(key.verifying_key().as_bytes(), &sig, data));
        assert!(!ed25519_verify(
            key.verifying_key().as_bytes(),
            &sig,
            b"wrong"
        ));
    }
}
