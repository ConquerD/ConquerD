// ConquerD Supernode — identity.rs
// Ed25519 identity: keypair generation, persistence, signing, verification.
// Also X25519 ephemeral keys for handshake ECDH.

use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::crypto::{b64url_decode, b64url_encode, derive_peer_id};

/// Persistent Ed25519 identity.
#[derive(Clone)]
pub struct Identity {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

#[derive(Serialize, Deserialize)]
struct IdentityFile {
    version: u32,
    public_key: String,
    private_key: String,
}

impl Identity {
    /// Generate a fresh random identity.
    pub fn generate() -> Self {
        let signing_key = SigningKey::generate(&mut rand::thread_rng());
        let verifying_key = signing_key.verifying_key();
        Self {
            signing_key,
            verifying_key,
        }
    }

    /// Load from disk or generate + save.
    pub fn load_or_create(dir: &Path) -> std::io::Result<Self> {
        let path = dir.join("identity.json");
        if path.exists() {
            Self::load(&path)
        } else {
            let id = Self::generate();
            std::fs::create_dir_all(dir)?;
            id.save(&path)?;
            Ok(id)
        }
    }

    /// Load from identity.json.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let data = std::fs::read_to_string(path)?;
        let file: IdentityFile = serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        let seed_bytes = b64url_decode(&file.private_key)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        if seed_bytes.len() != 32 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "private key seed must be 32 bytes",
            ));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&seed_bytes);
        let signing_key = SigningKey::from_bytes(&seed);
        let verifying_key = signing_key.verifying_key();
        Ok(Self {
            signing_key,
            verifying_key,
        })
    }

    /// Save to identity.json.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let file = IdentityFile {
            version: 1,
            public_key: b64url_encode(self.verifying_key.as_bytes()),
            private_key: b64url_encode(&self.signing_key.to_bytes()),
        };
        let json = serde_json::to_string_pretty(&file).map_err(std::io::Error::other)?;
        std::fs::write(path, &json)?;
        // Restrict file to owner-only (rw-------) — best-effort on Windows.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(path, perms);
        }
        Ok(())
    }

    /// base64url-encoded public key (used as sender/target in protocol).
    pub fn public_id(&self) -> String {
        b64url_encode(self.verifying_key.as_bytes())
    }

    /// SHA-256 hex of public key (stable peer identifier).
    pub fn peer_id(&self) -> String {
        derive_peer_id(self.verifying_key.as_bytes())
    }

    /// Raw public key bytes.
    pub fn public_key_bytes(&self) -> &[u8; 32] {
        self.verifying_key.as_bytes()
    }

    /// Sign data with Ed25519.
    pub fn sign(&self, data: &[u8]) -> [u8; 64] {
        self.signing_key.sign(data).to_bytes()
    }

    /// Verify an Ed25519 signature against a public key.
    pub fn verify_with_pub(pub_bytes: &[u8], signature: &[u8], data: &[u8]) -> bool {
        if pub_bytes.len() != 32 || signature.len() != 64 {
            return false;
        }
        let Ok(vk) = VerifyingKey::from_bytes(pub_bytes.try_into().unwrap()) else {
            return false;
        };
        let sig_bytes: [u8; 64] = match signature.try_into() {
            Ok(b) => b,
            Err(_) => return false,
        };
        let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
        vk.verify(data, &sig).is_ok()
    }
}

/// Generate an X25519 ephemeral keypair for handshake ECDH.
pub fn generate_x25519_keypair() -> (x25519_dalek::StaticSecret, x25519_dalek::PublicKey) {
    let secret = x25519_dalek::StaticSecret::random_from_rng(rand::thread_rng());
    let public = x25519_dalek::PublicKey::from(&secret);
    (secret, public)
}

/// Perform X25519 ECDH key exchange.
pub fn x25519_exchange(
    our_secret: &x25519_dalek::StaticSecret,
    their_public: &x25519_dalek::PublicKey,
) -> [u8; 32] {
    our_secret.diffie_hellman(their_public).to_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_roundtrip() {
        let id = Identity::generate();
        assert_eq!(id.public_id().len(), 43); // base64url of 32 bytes
        assert_eq!(id.peer_id().len(), 64); // SHA-256 hex

        // Sign and verify
        let data = b"hello";
        let sig = id.sign(data);
        assert!(Identity::verify_with_pub(id.public_key_bytes(), &sig, data));
        assert!(!Identity::verify_with_pub(
            id.public_key_bytes(),
            &sig,
            b"wrong"
        ));
    }

    #[test]
    fn test_identity_persistence() {
        let dir = std::env::temp_dir().join("conquerd_test_identity");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let id1 = Identity::load_or_create(&dir).unwrap();
        let id2 = Identity::load_or_create(&dir).unwrap();
        assert_eq!(id1.public_id(), id2.public_id());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_x25519_exchange() {
        let (secret_a, public_a) = generate_x25519_keypair();
        let (secret_b, public_b) = generate_x25519_keypair();
        let shared_a = x25519_exchange(&secret_a, &public_b);
        let shared_b = x25519_exchange(&secret_b, &public_a);
        assert_eq!(shared_a, shared_b);
    }
}
