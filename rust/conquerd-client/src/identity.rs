//! Identity management — Ed25519 keypair, on-disk persistence.
//!
//! Identities are stored as AES-256-GCM encrypted JSON (`identity.dat`)
//! using Argon2id KDF with optional OS keyring auto-unlock.

use base64::engine::general_purpose::URL_SAFE;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use keyring::Entry;
use rand::rngs::OsRng;
use rand::RngCore;
use std::path::{Path, PathBuf};

use zeroize::Zeroizing;

use crate::crypto::{
    aesgcm_decrypt, aesgcm_encrypt, argon2id_kdf, b64url_decode, derive_peer_id, derive_public_id,
    hkdf_derive_key, sha256,
};
use crate::error::{ClientError, Result};

// Default KDF parameters: t=3, m=65536 (64 MiB), p=4.
const KDF_T: u32 = 3;
const KDF_M: u32 = 65536; // 64 MiB
const KDF_P: u32 = 4;
const KEYRING_SERVICE: &str = "conquerd";

pub const DEFAULT_KEY_DIR_SUFFIX: &str = ".conquerd";
pub const IDENTITY_FILENAME: &str = "identity.dat";

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Owner of a Conquerd Ed25519 keypair.
///
/// The secret seed is held inside `SigningKey` which zeroizes on drop.
pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    // -- constructors -------------------------------------------------------

    /// Create an identity from a raw 32-byte Ed25519 secret seed.
    pub fn from_seed(seed: &[u8]) -> Result<Self> {
        let arr: [u8; 32] = seed
            .try_into()
            .map_err(|_| ClientError::Identity("seed must be 32 bytes".into()))?;
        Ok(Self {
            signing: SigningKey::from_bytes(&arr),
        })
    }

    /// Generate a fresh random identity.
    pub fn generate() -> Self {
        let mut seed = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(seed.as_mut());
        let signing = SigningKey::from_bytes(&seed);
        Self { signing }
    }

    // -- accessors ----------------------------------------------------------

    /// Raw 32-byte Ed25519 secret seed.
    pub fn private_key_bytes(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(self.signing.to_bytes())
    }

    /// Raw 32-byte Ed25519 public key.
    pub fn public_key_bytes(&self) -> [u8; 32] {
        *self.signing.verifying_key().as_bytes()
    }

    /// Borrow the underlying Ed25519 signing key (e.g. for QUIC TLS cert generation).
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing
    }

    /// Base64url (with padding) encoded public key — the identity "name" on the wire.
    pub fn public_id(&self) -> String {
        derive_public_id(self.signing.verifying_key().as_bytes())
    }

    /// Hex-encoded SHA-256 of the public key — stable peer identifier.
    pub fn peer_id(&self) -> String {
        derive_peer_id(self.signing.verifying_key().as_bytes())
    }

    /// Hex fingerprint (colon-separated bytes).
    pub fn fingerprint(&self) -> String {
        self.signing
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    // -- signing ------------------------------------------------------------

    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        self.signing.sign(data).to_bytes().to_vec()
    }

    pub fn verify(&self, signature: &[u8], data: &[u8]) -> bool {
        crate::crypto::ed25519_verify(self.signing.verifying_key().as_bytes(), signature, data)
    }

    pub fn verify_with_public_key(public_key_bytes: &[u8], signature: &[u8], data: &[u8]) -> bool {
        crate::crypto::ed25519_verify(public_key_bytes, signature, data)
    }

    // -- key derivation -----------------------------------------------------

    /// Derive a 32-byte at-rest storage subkey via HKDF-SHA256.
    ///
    /// `info` is a domain-separation label (use `"conquerd-store/<name>/v<n>"`).
    pub fn derive_store_key(&self, info: &str) -> Result<[u8; 32]> {
        let seed = self.signing.to_bytes();
        hkdf_derive_key(&seed, info.as_bytes())
    }

    /// Derive the deterministic pairwise key shared with `peer_identity_pub_b64`
    /// (a base64url Ed25519 public key), used to encrypt `EncryptedSignal`
    /// envelopes on the supernode-relay fallback path. See
    /// [`crate::crypto::derive_pairwise_relay_key`].
    pub fn derive_pairwise_relay_key(&self, peer_identity_pub_b64: &str) -> Result<[u8; 32]> {
        let scalar = self.signing.to_scalar_bytes();
        crate::crypto::derive_pairwise_relay_key(&scalar, &self.public_id(), peer_identity_pub_b64)
    }

    // -- persistence --------------------------------------------------------

    /// Default key directory: `~/.conquerd`
    pub fn default_key_dir() -> PathBuf {
        if let Ok(path) =
            std::env::var("CONQUERD_KEY_DIR").or_else(|_| std::env::var("CONQUERD_HOME"))
        {
            return PathBuf::from(path);
        }
        dirs_or_home().join(DEFAULT_KEY_DIR_SUFFIX)
    }

    /// Save as AES-256-GCM encrypted (`identity.dat`).
    ///
    /// The passphrase bytes are hashed with Argon2id; the resulting key encrypts
    /// the private seed with AES-256-GCM using the public_id as AAD.
    /// Use `crypto::build_passphrase_material` to combine a text passphrase and/or
    /// a keyfile into the `passphrase` bytes before calling this.
    pub fn save_encrypted(&self, passphrase: &[u8], directory: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(directory)?;
        let path = directory.join(IDENTITY_FILENAME);
        let pub_b64 = self.public_id();
        let aad = pub_b64.as_bytes();

        // Derive AES key from passphrase
        let mut salt = [0u8; 32];
        OsRng.fill_bytes(&mut salt);
        let aes_key = argon2id_kdf(passphrase, &salt, KDF_T, KDF_M, KDF_P)?;

        // Encrypt the Ed25519 seed
        let seed = self.signing.to_bytes();
        let (nonce, ciphertext) = aesgcm_encrypt(&aes_key, &seed, aad)?;

        let data = serde_json::json!({
            "version": 2,
            "identity_pub": pub_b64,
            "kdf": { "t": KDF_T, "m": KDF_M, "p": KDF_P },
            "salt": URL_SAFE.encode(salt),
            "nonce": URL_SAFE.encode(nonce),
            "ciphertext": URL_SAFE.encode(ciphertext),
        });
        std::fs::write(&path, serde_json::to_string_pretty(&data)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(path)
    }

    /// Load from the encrypted identity file using a derived AES key.
    pub fn load_encrypted(aes_key: &[u8], directory: &Path) -> Result<Self> {
        let path = directory.join(IDENTITY_FILENAME);
        let text = std::fs::read_to_string(&path)?;
        let data: serde_json::Value = serde_json::from_str(&text)?;

        if data["version"].as_u64() != Some(2) {
            return Err(ClientError::Identity(format!(
                "unsupported identity file version: {:?}",
                data["version"]
            )));
        }
        let pub_b64 = data["identity_pub"]
            .as_str()
            .ok_or_else(|| ClientError::Identity("missing identity_pub".into()))?;
        let nonce = b64url_decode(
            data["nonce"]
                .as_str()
                .ok_or_else(|| ClientError::Identity("missing nonce".into()))?,
        )?;
        let ciphertext = b64url_decode(
            data["ciphertext"]
                .as_str()
                .ok_or_else(|| ClientError::Identity("missing ciphertext".into()))?,
        )?;
        let aad = pub_b64.as_bytes();
        let seed = aesgcm_decrypt(aes_key, &nonce, &ciphertext, aad)?;
        let identity = Self::from_seed(&seed)?;
        if identity.public_id() != pub_b64 {
            return Err(ClientError::Identity(
                "identity file integrity check failed".into(),
            ));
        }
        Ok(identity)
    }

    /// Load from v2 encrypted file using passphrase bytes.
    /// Use `crypto::build_passphrase_material` to combine a text passphrase and/or
    /// a keyfile into the `passphrase` bytes before calling this.
    pub fn load_with_passphrase(passphrase: &[u8], directory: &Path) -> Result<Self> {
        let path = directory.join(IDENTITY_FILENAME);
        let text = std::fs::read_to_string(&path)?;
        let data: serde_json::Value = serde_json::from_str(&text)?;

        let salt = b64url_decode(
            data["salt"]
                .as_str()
                .ok_or_else(|| ClientError::Identity("missing salt".into()))?,
        )?;
        let kdf = &data["kdf"];
        // Read KDF parameters, falling back to the current defaults when the
        // field is absent (older identity files written before the kdf block
        // was introduced).  If the field IS present with a value below the
        // required minimum we reject it — that indicates a tampered file that
        // has been deliberately weakened.
        let t = kdf["t"].as_u64().unwrap_or(KDF_T as u64) as u32;
        let m = kdf["m"].as_u64().unwrap_or(KDF_M as u64) as u32;
        let p = kdf["p"].as_u64().unwrap_or(KDF_P as u64) as u32;
        // Only enforce the minimum when the caller has explicitly written a
        // value (i.e. the value was present and parseable).  A value equal to
        // the default passes: `kdf["t"].as_u64().is_some()` is true only when
        // the field exists AND is a JSON integer.
        let t_present = kdf["t"].as_u64().is_some();
        let m_present = kdf["m"].as_u64().is_some();
        let p_present = kdf["p"].as_u64().is_some();
        if (t_present && t < KDF_T) || (m_present && m < KDF_M) || (p_present && p < KDF_P) {
            return Err(ClientError::Identity(format!(
                "identity KDF parameters below required minimum \
                 (t={t} m={m} p={p}; need t>={KDF_T} m>={KDF_M} p>={KDF_P})"
            )));
        }
        let aes_key = argon2id_kdf(passphrase, &salt, t, m, p)?;
        Self::load_encrypted(&aes_key, directory)
    }

    /// Try to load from the OS keyring cache, falling back to passphrase bytes.
    ///
    /// The keyring stores the derived AES key so Argon2id is only run once.
    /// Pass `b""` for keyring-only attempts (no passphrase prompt).
    pub fn load_with_keyring_or_passphrase(
        passphrase: &[u8],
        directory: &Path,
    ) -> Result<(Self, Option<[u8; 32]>)> {
        // Read public_id first without decrypting
        let pub_b64 = Self::read_public_id_from_dat(directory)?
            .ok_or_else(|| ClientError::Identity("identity.dat not found".into()))?;

        if let Some(aes_key) = keyring_load_aes_key(&pub_b64) {
            if let Ok(id) = Self::load_encrypted(&aes_key, directory) {
                return Ok((id, Some(aes_key)));
            }
            // keyring stale — fall through to passphrase
        }
        let identity = Self::load_with_passphrase(passphrase, directory)?;
        Ok((identity, None))
    }

    /// Read only the public_id field from `identity.dat` (without decryption).
    pub fn read_public_id_from_dat(directory: &Path) -> Result<Option<String>> {
        let path = directory.join(IDENTITY_FILENAME);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        let data: serde_json::Value = serde_json::from_str(&text)?;
        Ok(data["identity_pub"].as_str().map(str::to_owned))
    }

    /// Load from `identity.dat`.
    ///
    /// Returns `Err(Io(NotFound))` when no identity file exists.
    pub fn load(passphrase: Option<&str>, directory: &Path) -> Result<Self> {
        let dat = directory.join(IDENTITY_FILENAME);
        if !dat.exists() {
            return Err(ClientError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no identity file found",
            )));
        }
        let pass = passphrase.ok_or_else(|| {
            ClientError::Identity("passphrase required for encrypted identity".into())
        })?;
        Self::load_with_passphrase(pass.as_bytes(), directory)
    }
}

// ---------------------------------------------------------------------------
// OS keyring helpers
// ---------------------------------------------------------------------------

/// Compute the keyring username for a given public_id.
fn keyring_username(pub_b64: &str) -> String {
    let machine_id = machine_id_hex();
    format!("identity-key:{pub_b64}:{machine_id}")
}

/// Get MAC-address-based machine id (12 hex digits), matches Python `format(uuid.getnode(), "012x")`.
fn machine_id_hex() -> String {
    // Use a stable file-based machine ID if available, otherwise fallback.
    // For simplicity, try to read /etc/machine-id or use hostname hash.
    #[cfg(target_os = "linux")]
    if let Ok(id) = std::fs::read_to_string("/etc/machine-id") {
        let trimmed = id.trim();
        if trimmed.len() >= 12 {
            return trimmed[..12].to_owned();
        }
    }
    // Fallback: hash hostname
    let hostname = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".to_owned());
    let h = sha256(hostname.as_bytes());
    hex::encode(&h[..6]) // 12 hex chars
}

fn keyring_load_aes_key(pub_b64: &str) -> Option<[u8; 32]> {
    let entry = Entry::new(KEYRING_SERVICE, &keyring_username(pub_b64)).ok()?;
    let encoded = entry.get_password().ok()?;
    let decoded = URL_SAFE.decode(encoded.trim()).ok()?;
    if decoded.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&decoded);
    Some(arr)
}

pub fn keyring_store_aes_key(pub_b64: &str, aes_key: &[u8; 32]) -> bool {
    let Ok(entry) = Entry::new(KEYRING_SERVICE, &keyring_username(pub_b64)) else {
        return false;
    };
    let encoded = URL_SAFE.encode(aes_key);
    entry.set_password(&encoded).is_ok()
}

/// Remove the keyring entry for the given public_id. Used by "Lock Identity & Quit".
/// Returns `true` if the entry was found and deleted, `false` if absent or on error.
pub fn keyring_delete_aes_key(pub_b64: &str) -> bool {
    let Ok(entry) = Entry::new(KEYRING_SERVICE, &keyring_username(pub_b64)) else {
        return false;
    };
    entry.delete_credential().is_ok()
}

fn dirs_or_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn default_key_dir_honors_profile_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let old_key_dir = std::env::var("CONQUERD_KEY_DIR").ok();
        let old_home = std::env::var("CONQUERD_HOME").ok();
        let dir = tempdir().unwrap();
        let home_dir = dir.path().join("client_home");
        let key_dir = dir.path().join("client_keys");

        std::env::remove_var("CONQUERD_KEY_DIR");
        std::env::set_var("CONQUERD_HOME", &home_dir);
        assert_eq!(Identity::default_key_dir(), home_dir);

        std::env::set_var("CONQUERD_KEY_DIR", &key_dir);
        assert_eq!(Identity::default_key_dir(), key_dir);

        match old_key_dir {
            Some(value) => std::env::set_var("CONQUERD_KEY_DIR", value),
            None => std::env::remove_var("CONQUERD_KEY_DIR"),
        }
        match old_home {
            Some(value) => std::env::set_var("CONQUERD_HOME", value),
            None => std::env::remove_var("CONQUERD_HOME"),
        }
    }

    #[test]
    fn generate_and_roundtrip_v2() {
        let dir = tempdir().unwrap();
        let id = Identity::generate();
        let pub_id = id.public_id();
        id.save_encrypted(b"test-passphrase", dir.path()).unwrap();
        let loaded = Identity::load_with_passphrase(b"test-passphrase", dir.path()).unwrap();
        assert_eq!(loaded.public_id(), pub_id);
    }

    #[test]
    fn wrong_passphrase_v2_fails() {
        let dir = tempdir().unwrap();
        let id = Identity::generate();
        id.save_encrypted(b"correct", dir.path()).unwrap();
        assert!(Identity::load_with_passphrase(b"wrong", dir.path()).is_err());
    }

    #[test]
    fn sign_verify_roundtrip() {
        let id = Identity::generate();
        let data = b"test message";
        let sig = id.sign(data);
        assert!(id.verify(&sig, data));
        assert!(!id.verify(&sig, b"different"));
    }

    #[test]
    fn derive_store_key_domain_separation() {
        let id = Identity::generate();
        let k1 = id.derive_store_key("conquerd-store/peers/v1").unwrap();
        let k2 = id.derive_store_key("conquerd-store/chat/v1").unwrap();
        assert_ne!(k1, k2);
    }
}
