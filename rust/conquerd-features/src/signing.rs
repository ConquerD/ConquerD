//! Module signing format — Ed25519 over a canonical manifest.
//!
//! A third-party `FeatureModule` is distributed as a pair:
//!   * `<name>.cdylib` — the native shared library (`.so` / `.dll` / `.dylib`)
//!   * `<name>.module.toml` — the signed manifest describing the module,
//!     who signed it, and a SHA-256 digest of the library binary.
//!
//! The author signs with their Ed25519 signing key; users add the author's
//! *verifying* key to their trust store (see [`TrustedKeyStore`]). The
//! `NativeModuleLoader` verifies the signature before opening the library.
//!
//! # Canonical signing payload
//!
//! ```text
//! serde_json::to_string(BTreeMap {
//!   "author"          => string,
//!   "capability"      => { "auth": ..., "id": ..., "kind": ..., "version": ... },
//!   "cdylib_sha256"   => string (lowercase hex SHA-256 of the cdylib binary),
//!   "id"              => string (must equal capability.id),
//!   "schema_version"  => u32,
//!   "signer_pubkey"   => string (base64url-no-pad 32-byte Ed25519 key),
//!   "version"         => string,
//! })
//! ```
//!
//! Keys are sorted alphabetically (enforced by `BTreeMap`) for a stable,
//! language-agnostic canonical form. The `signature` field is never included.

use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::descriptor::{AuthTier, CapabilityDescriptor, ChannelKind};

/// TOML schema version for module manifests. Bump on any breaking change.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

/// A signed manifest for a third-party native module.
///
/// On-disk shape (`<name>.module.toml`):
///
/// ```toml
/// schema_version = 1
/// id = "x.acme.matchmaker"
/// version = "1.0"
/// author = "Acme Corp"
/// signer_pubkey = "base64url-no-pad-32-byte-ed25519-pubkey"
/// cdylib_sha256  = "lowercase-hex-sha256"
/// signature      = "base64url-no-pad-64-byte-ed25519-signature"
///
/// [capability]
/// id      = "x.acme.matchmaker"
/// version = "1.0"
/// kind    = "request"
/// auth    = "trusted-peer"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleManifest {
    /// Schema version — loaders reject anything != [`MANIFEST_SCHEMA_VERSION`].
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Capability id this module implements (reverse-DNS, must contain `.`).
    /// Third-party modules should use the `x.<vendor>.*` namespace.
    pub id: String,
    /// Version string (semver-compatible).
    pub version: String,
    /// Human-readable author name shown in trust prompts.
    pub author: String,
    /// Base64url-no-pad-encoded 32-byte Ed25519 verifying key of the signer.
    pub signer_pubkey: String,
    /// Lowercase hex SHA-256 of the cdylib binary the loader will verify.
    pub cdylib_sha256: String,
    /// Base64url-no-pad-encoded 64-byte Ed25519 signature over the
    /// canonical payload (see module-level docs).
    pub signature: String,
    /// The capability this module advertises.
    pub capability: ManifestCapability,
}

/// Capability record embedded in a module manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestCapability {
    pub id: String,
    pub version: String,
    pub kind: ChannelKind,
    #[serde(default)]
    pub auth: AuthTier,
}

impl ManifestCapability {
    /// Convert to a [`CapabilityDescriptor`] for registration in a
    /// [`crate::FeatureRegistry`].
    pub fn to_descriptor(&self) -> CapabilityDescriptor {
        CapabilityDescriptor::new(&self.id, &self.version, self.kind).with_auth(self.auth)
    }
}

fn default_schema_version() -> u32 {
    MANIFEST_SCHEMA_VERSION
}

/// Errors raised during manifest sign/verify operations.
#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error("unsupported schema version {found}, expected {expected}")]
    UnsupportedSchema { found: u32, expected: u32 },
    #[error("invalid signer_pubkey: {0}")]
    InvalidPubkey(String),
    #[error("invalid signature encoding: {0}")]
    InvalidSignature(String),
    #[error("signature verification failed")]
    VerificationFailed,
    #[error("cdylib hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("manifest id mismatch: manifest.id={manifest_id}, capability.id={cap_id}")]
    IdMismatch { manifest_id: String, cap_id: String },
    #[error("json serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("base64 decode error: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),
}

impl ModuleManifest {
    /// Parse a manifest from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self, SigningError> {
        Ok(toml::from_str(s)?)
    }

    /// Load a manifest from a file path.
    pub fn load(path: &std::path::Path) -> Result<Self, SigningError> {
        let s = std::fs::read_to_string(path)?;
        Self::from_toml_str(&s)
    }

    /// Compute the canonical, deterministic signing payload for this manifest.
    ///
    /// The payload is `serde_json::to_string` of a `BTreeMap` (alphabetical
    /// key order) over a fixed set of fields. The `signature` field is never
    /// included in the payload.
    pub fn canonical_payload(&self) -> Result<Vec<u8>, SigningError> {
        // Embed capability as a nested BTreeMap for stable key ordering.
        let mut cap_map: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
        cap_map.insert("auth", serde_json::to_value(self.capability.auth)?);
        cap_map.insert("id", serde_json::Value::String(self.capability.id.clone()));
        cap_map.insert("kind", serde_json::to_value(self.capability.kind)?);
        cap_map.insert(
            "version",
            serde_json::Value::String(self.capability.version.clone()),
        );

        let mut map: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
        map.insert("author", serde_json::Value::String(self.author.clone()));
        map.insert("capability", serde_json::to_value(cap_map)?);
        map.insert(
            "cdylib_sha256",
            serde_json::Value::String(self.cdylib_sha256.clone()),
        );
        map.insert("id", serde_json::Value::String(self.id.clone()));
        map.insert(
            "schema_version",
            serde_json::Value::Number(self.schema_version.into()),
        );
        map.insert(
            "signer_pubkey",
            serde_json::Value::String(self.signer_pubkey.clone()),
        );
        map.insert("version", serde_json::Value::String(self.version.clone()));

        Ok(serde_json::to_string(&map)?.into_bytes())
    }

    /// Verify the Ed25519 signature and check basic consistency rules.
    ///
    /// Does **not** perform any disk I/O; call [`Self::verify_cdylib`]
    /// separately once you have the cdylib bytes.
    pub fn verify_signature(&self) -> Result<(), SigningError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(SigningError::UnsupportedSchema {
                found: self.schema_version,
                expected: MANIFEST_SCHEMA_VERSION,
            });
        }
        if self.id != self.capability.id {
            return Err(SigningError::IdMismatch {
                manifest_id: self.id.clone(),
                cap_id: self.capability.id.clone(),
            });
        }

        // Decode the signer's verifying key.
        let pk_bytes = URL_SAFE_NO_PAD
            .decode(&self.signer_pubkey)
            .map_err(|e| SigningError::InvalidPubkey(e.to_string()))?;
        let pk_bytes: [u8; 32] = pk_bytes
            .try_into()
            .map_err(|_| SigningError::InvalidPubkey("expected 32 bytes".into()))?;
        let pubkey = VerifyingKey::from_bytes(&pk_bytes)
            .map_err(|e| SigningError::InvalidPubkey(e.to_string()))?;

        // Decode the signature.
        let sig_bytes = URL_SAFE_NO_PAD
            .decode(&self.signature)
            .map_err(|e| SigningError::InvalidSignature(e.to_string()))?;
        let sig_bytes: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| SigningError::InvalidSignature("expected 64 bytes".into()))?;
        let sig = Signature::from_bytes(&sig_bytes);

        let payload = self.canonical_payload()?;
        pubkey
            .verify(&payload, &sig)
            .map_err(|_| SigningError::VerificationFailed)
    }

    /// Verify the SHA-256 of a cdylib binary against `self.cdylib_sha256`.
    pub fn verify_cdylib(&self, cdylib_bytes: &[u8]) -> Result<(), SigningError> {
        let actual = format!("{:x}", Sha256::digest(cdylib_bytes));
        if actual != self.cdylib_sha256 {
            return Err(SigningError::HashMismatch {
                expected: self.cdylib_sha256.clone(),
                actual,
            });
        }
        Ok(())
    }

    /// Compute the SHA-256 hex string for a cdylib binary.
    ///
    /// Use this when building manifests for distribution:
    /// ```rust,ignore
    /// manifest.cdylib_sha256 = ModuleManifest::hash_cdylib(&std::fs::read("my_module.so")?);
    /// ```
    pub fn hash_cdylib(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
}

/// Sign a manifest with the given Ed25519 signing key.
///
/// Sets `manifest.signer_pubkey` and `manifest.signature`. The field
/// `manifest.cdylib_sha256` must already be populated before calling this
/// (see [`ModuleManifest::hash_cdylib`]).
pub fn sign_manifest(
    manifest: &mut ModuleManifest,
    signing_key: &SigningKey,
) -> Result<(), SigningError> {
    let pubkey = signing_key.verifying_key();
    manifest.signer_pubkey = URL_SAFE_NO_PAD.encode(pubkey.as_bytes());
    // Clear any prior signature so it doesn't pollute the canonical payload.
    manifest.signature = String::new();
    let payload = manifest.canonical_payload()?;
    let signature = signing_key.sign(&payload);
    manifest.signature = URL_SAFE_NO_PAD.encode(signature.to_bytes());
    Ok(())
}

/// In-memory set of Ed25519 verifying keys the user has trusted for
/// module loading.
///
/// The canonical representation is base64url-no-pad of the 32-byte key —
/// the same encoding used in [`ModuleManifest::signer_pubkey`]. Persistence
/// is the caller's responsibility; the common pattern is
/// `<data_dir>/trusted_module_keys.txt` (one key per line).
#[derive(Debug, Default, Clone)]
pub struct TrustedKeyStore {
    keys: std::collections::HashSet<String>,
}

impl TrustedKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Trust a verifying key given as its base64url-no-pad encoding.
    pub fn trust(&mut self, pubkey_b64: impl Into<String>) {
        self.keys.insert(pubkey_b64.into());
    }

    /// Remove a key. Returns `true` if it was present.
    pub fn revoke(&mut self, pubkey_b64: &str) -> bool {
        self.keys.remove(pubkey_b64)
    }

    /// `true` if the key is in the trust store.
    pub fn is_trusted(&self, pubkey_b64: &str) -> bool {
        self.keys.contains(pubkey_b64)
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Serialise to a newline-separated text file (one key per line).
    /// Lines are sorted for stable diffs.
    pub fn to_text(&self) -> String {
        let mut keys: Vec<&str> = self.keys.iter().map(String::as_str).collect();
        keys.sort();
        keys.join("\n")
    }

    /// Parse a text-format trust store (newline-separated base64url keys).
    /// Lines starting with `#` and blank lines are ignored.
    pub fn from_text(s: &str) -> Self {
        let mut store = Self::new();
        for line in s.lines() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                store.trust(line);
            }
        }
        store
    }

    /// Load from a file path. Returns an empty store if the file does not
    /// exist, and an error for other I/O failures.
    pub fn load(path: &std::path::Path) -> Result<Self, std::io::Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::from_text(&text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(e),
        }
    }

    /// Append a key to a file, creating it if necessary. Idempotent.
    pub fn append_to_file(path: &std::path::Path, pubkey_b64: &str) -> Result<(), std::io::Error> {
        // Read existing to avoid duplicates.
        let existing = Self::load(path)?;
        if existing.is_trusted(pubkey_b64) {
            return Ok(());
        }
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        writeln!(f, "{}", pubkey_b64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn gen_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    fn minimal_manifest(_key: &SigningKey, cdylib_bytes: &[u8]) -> ModuleManifest {
        ModuleManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            id: "x.test.thing".to_string(),
            version: "1.0".to_string(),
            author: "Test Author".to_string(),
            signer_pubkey: String::new(),
            cdylib_sha256: ModuleManifest::hash_cdylib(cdylib_bytes),
            signature: String::new(),
            capability: ManifestCapability {
                id: "x.test.thing".to_string(),
                version: "1.0".to_string(),
                kind: ChannelKind::Request,
                auth: AuthTier::TrustedPeer,
            },
        }
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let key = gen_key();
        let mut m = minimal_manifest(&key, b"fake library bytes");
        sign_manifest(&mut m, &key).unwrap();
        assert!(!m.signature.is_empty());
        assert!(!m.signer_pubkey.is_empty());
        m.verify_signature().unwrap();
    }

    #[test]
    fn tampered_author_fails_verification() {
        let key = gen_key();
        let mut m = minimal_manifest(&key, b"lib");
        sign_manifest(&mut m, &key).unwrap();
        m.author = "Attacker".to_string();
        assert!(matches!(
            m.verify_signature(),
            Err(SigningError::VerificationFailed)
        ));
    }

    #[test]
    fn tampered_capability_id_caught_before_sig_check() {
        let key = gen_key();
        let mut m = minimal_manifest(&key, b"lib");
        sign_manifest(&mut m, &key).unwrap();
        m.capability.id = "x.evil.override".to_string();
        assert!(matches!(
            m.verify_signature(),
            Err(SigningError::IdMismatch { .. })
        ));
    }

    #[test]
    fn wrong_signing_key_fails() {
        let key1 = gen_key();
        let key2 = gen_key();
        let mut m = minimal_manifest(&key1, b"lib");
        sign_manifest(&mut m, &key1).unwrap();
        // Swap signer_pubkey to key2 but keep key1's signature.
        m.signer_pubkey = URL_SAFE_NO_PAD.encode(key2.verifying_key().as_bytes());
        assert!(matches!(
            m.verify_signature(),
            Err(SigningError::VerificationFailed)
        ));
    }

    #[test]
    fn cdylib_hash_mismatch_detected() {
        let key = gen_key();
        let mut m = minimal_manifest(&key, b"real bytes");
        sign_manifest(&mut m, &key).unwrap();
        assert!(matches!(
            m.verify_cdylib(b"tampered bytes"),
            Err(SigningError::HashMismatch { .. })
        ));
    }

    #[test]
    fn cdylib_hash_matches() {
        let cdylib = b"the real bytes";
        let key = gen_key();
        let mut m = minimal_manifest(&key, cdylib);
        sign_manifest(&mut m, &key).unwrap();
        m.verify_cdylib(cdylib).unwrap();
    }

    #[test]
    fn toml_round_trip_preserves_signature() {
        let key = gen_key();
        let mut m = minimal_manifest(&key, b"lib");
        sign_manifest(&mut m, &key).unwrap();
        let toml_str = toml::to_string(&m).unwrap();
        let back = ModuleManifest::from_toml_str(&toml_str).unwrap();
        back.verify_signature().unwrap();
    }

    #[test]
    fn unsupported_schema_version_rejected() {
        let key = gen_key();
        let mut m = minimal_manifest(&key, b"lib");
        sign_manifest(&mut m, &key).unwrap();
        m.schema_version = 99;
        assert!(matches!(
            m.verify_signature(),
            Err(SigningError::UnsupportedSchema { .. })
        ));
    }

    #[test]
    fn manifest_capability_to_descriptor() {
        let cap = ManifestCapability {
            id: "x.acme.thing".to_string(),
            version: "2.0".to_string(),
            kind: ChannelKind::Datagram,
            auth: AuthTier::RoomMember,
        };
        let desc = cap.to_descriptor();
        assert_eq!(desc.id, "x.acme.thing");
        assert_eq!(desc.version, "2.0");
        assert_eq!(desc.kind, ChannelKind::Datagram);
        assert_eq!(desc.auth, AuthTier::RoomMember);
    }

    // ── TrustedKeyStore ──────────────────────────────────────────────────────

    #[test]
    fn trust_store_basic_operations() {
        let mut store = TrustedKeyStore::new();
        assert!(store.is_empty());
        store.trust("aabbcc");
        assert!(store.is_trusted("aabbcc"));
        assert!(!store.is_trusted("other"));
        assert!(store.revoke("aabbcc"));
        assert!(!store.is_trusted("aabbcc"));
    }

    #[test]
    fn trust_store_text_round_trip() {
        let mut store = TrustedKeyStore::new();
        store.trust("key1");
        store.trust("key2");
        let text = store.to_text();
        let back = TrustedKeyStore::from_text(&text);
        assert!(back.is_trusted("key1"));
        assert!(back.is_trusted("key2"));
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn trust_store_from_text_ignores_comments_and_blanks() {
        let text = "# my keys\n\nkey1\n  \nkey2\n";
        let store = TrustedKeyStore::from_text(text);
        assert_eq!(store.len(), 2);
        assert!(store.is_trusted("key1"));
    }

    #[test]
    fn trust_store_file_load_missing_returns_empty() {
        let dir = std::env::temp_dir();
        let path = dir.join("this_file_does_not_exist_conquerd_test.txt");
        let store = TrustedKeyStore::load(&path).unwrap();
        assert!(store.is_empty());
    }

    #[test]
    fn trust_store_append_to_file_is_idempotent() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "conquerd_trust_test_{}.txt",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        TrustedKeyStore::append_to_file(&path, "key1").unwrap();
        TrustedKeyStore::append_to_file(&path, "key1").unwrap(); // idempotent
        TrustedKeyStore::append_to_file(&path, "key2").unwrap();
        let store = TrustedKeyStore::load(&path).unwrap();
        assert_eq!(store.len(), 2);
        let _ = std::fs::remove_file(&path);
    }
}
