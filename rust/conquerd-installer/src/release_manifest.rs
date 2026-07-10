use anyhow::{Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// Hex-encoded Ed25519 public key of the ConquerD release signer.
/// This is the public half of the key generated for manifest signing
/// (see keys/release-signer-public.pem; private key is kept out-of-repo).
///
/// The corresponding private key must be used (via a secure process or
/// the `sign-release-manifest` binary) to produce the `signature` field in
/// releases_manifest.json.
///
/// Usage (from repo root):
///   cargo run -p conquerd-installer --bin sign-release-manifest -- \
///     -i path/to/unsigned.json -o releases_manifest.json \
///     --private-key /secure/release-signer-private.pem
///
/// The --pubkey argument defaults to this constant; pass --pubkey explicitly
/// if/when rotating keys in the future (must match the const used by verifiers).
pub const RELEASE_SIGNER_PUBKEY_HEX: &str =
    "d31f43fcfba1fae04313d384d7fba026bd52796550c57def6cf47b069c18043f";

/// One entry in the release manifest.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ManifestEntry {
    pub version: String,
    pub platform: String,
    /// Hash of the distributed archive (e.g. the .7z). Verified by installer.
    pub build_hash: String,
    /// Optional reproducible build identifier embedded in the binaries
    /// (e.g. "release-1.0.0-18eae80" or git sha). Used for P2P build attestation.
    #[serde(default)]
    pub build_id: String,
    #[serde(default)]
    pub published_at: f64,
}

/// The full signed release manifest downloaded alongside each GitHub release.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ReleaseManifest {
    pub releases: Vec<ManifestEntry>,
    #[serde(default)]
    pub signed_at: f64,
    #[serde(default)]
    pub signer_pubkey: String,
    /// Base64url-encoded Ed25519 signature over the canonical JSON (no `signature` field).
    #[serde(default)]
    pub signature: String,
}

impl ReleaseManifest {
    /// Parse and verify a manifest from raw JSON bytes.
    ///
    /// Returns an error if:
    ///  - JSON is malformed
    ///  - The Ed25519 signature is invalid for the configured RELEASE_SIGNER_PUBKEY_HEX
    pub fn parse_and_verify(raw_json: &str) -> Result<Self> {
        let manifest: ReleaseManifest =
            serde_json::from_str(raw_json).context("Failed to parse releases_manifest.json")?;

        if RELEASE_SIGNER_PUBKEY_HEX == "0".repeat(64) {
            anyhow::bail!(
                "RELEASE_SIGNER_PUBKEY_HEX is still the all-zero placeholder — \
                this must be replaced with the real release signer public key"
            );
        }

        let canonical = canonical_json(raw_json)?;
        let pubkey_bytes: [u8; 32] = hex::decode(RELEASE_SIGNER_PUBKEY_HEX)
            .context("Invalid RELEASE_SIGNER_PUBKEY_HEX")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Public key must be 32 bytes"))?;
        verify_signature(&manifest.signature, &canonical, &pubkey_bytes)
            .context("Release manifest signature verification failed")?;

        Ok(manifest)
    }

    /// Parse an unsigned manifest (nightly builds). JSON structure is validated;
    /// Ed25519 signature verification is skipped.
    pub fn parse_unsigned(raw_json: &str) -> Result<Self> {
        let manifest: ReleaseManifest =
            serde_json::from_str(raw_json).context("Failed to parse releases_manifest.json")?;

        if manifest.releases.is_empty() {
            anyhow::bail!("Release manifest contains no release entries");
        }

        Ok(manifest)
    }

    /// Parse and verify using the channel-appropriate trust model.
    pub fn parse_for_channel(raw_json: &str, nightly: bool) -> Result<Self> {
        if nightly {
            Self::parse_unsigned(raw_json)
        } else {
            Self::parse_and_verify(raw_json)
        }
    }

    /// Return true if *(version, build_hash)* appears for any platform.
    pub fn contains(&self, version: &str, build_hash: &str) -> bool {
        self.releases.iter().any(|e| {
            e.version == version && e.build_hash.to_lowercase() == build_hash.to_lowercase()
        })
    }

    /// Return true if *build_hash* appears for the given platform.
    pub fn contains_for_platform(&self, platform: &str, build_hash: &str) -> bool {
        self.releases.iter().any(|e| {
            e.platform == platform && e.build_hash.to_lowercase() == build_hash.to_lowercase()
        })
    }

    /// Return the published archive hash for a platform, if present.
    pub fn build_hash_for_platform(&self, platform: &str) -> Option<String> {
        self.entry_for_platform(platform)
            .map(|e| e.build_hash.to_lowercase())
    }

    /// Return the manifest entry for a platform, if present.
    pub fn entry_for_platform(&self, platform: &str) -> Option<&ManifestEntry> {
        self.releases.iter().find(|e| e.platform == platform)
    }
}

/// Cross-check a downloaded archive hash against the release manifest.
pub fn verify_archive_hash(
    raw_json: &str,
    nightly: bool,
    release_version: &str,
    platform: &str,
    archive_hash: &str,
) -> Result<()> {
    let mf = ReleaseManifest::parse_for_channel(raw_json, nightly)?;
    let ok = if nightly {
        mf.contains_for_platform(platform, archive_hash)
    } else {
        mf.contains(release_version, archive_hash)
    };

    if !ok {
        let target = if nightly {
            format!("platform {platform}")
        } else {
            format!("v{release_version}")
        };
        anyhow::bail!(
            "Archive hash {} not found in release manifest for {target}",
            &archive_hash[..archive_hash.len().min(12)]
        );
    }

    Ok(())
}

/// Rebuild the canonical form: same as the raw JSON but with the `signature`
/// key removed, and all top-level keys sorted.
fn canonical_json(raw: &str) -> Result<Vec<u8>> {
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(raw).context("Could not re-parse manifest as object")?;
    map.remove("signature");
    // serde_json::to_vec sorts keys when using BTreeMap; use Value to keep ordering.
    // We need sorted keys — convert to a BTreeMap representation.
    let sorted: std::collections::BTreeMap<_, _> = map.into_iter().collect();
    serde_json::to_vec(&sorted).context("Failed to re-serialize canonical manifest")
}

fn verify_signature(sig_b64: &str, canonical: &[u8], pubkey_bytes: &[u8; 32]) -> Result<()> {
    let vk = VerifyingKey::from_bytes(pubkey_bytes).context("Invalid Ed25519 public key")?;

    // Accept standard base64 or base64url (add padding if needed)
    let sig_bytes = base64_decode(sig_b64).context("Invalid signature base64")?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Signature must be 64 bytes"))?;

    let sig = Signature::from_bytes(&sig_arr);
    use ed25519_dalek::Verifier;
    vk.verify(canonical, &sig)
        .context("Signature verification failed")?;
    Ok(())
}

fn base64_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    // Try URL-safe first, then standard
    let padded = pad_base64(s);
    base64::engine::general_purpose::URL_SAFE
        .decode(&padded)
        .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&padded))
        .context("base64 decode failed")
}

fn pad_base64(s: &str) -> String {
    let rem = s.len() % 4;
    if rem == 0 {
        s.to_string()
    } else {
        format!("{}{}", s, "=".repeat(4 - rem))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // canonical_json
    // -----------------------------------------------------------------------

    #[test]
    fn canonical_json_removes_signature_field() {
        let raw = r#"{"releases":[],"signature":"abc123","signed_at":1234.0}"#;
        let canonical = canonical_json(raw).expect("canonical_json should succeed");
        let text = String::from_utf8(canonical).unwrap();
        assert!(!text.contains("\"signature\""), "signature must be removed");
        assert!(text.contains("\"releases\""));
    }

    #[test]
    fn canonical_json_sorts_keys_alphabetically() {
        let raw = r#"{"z_key":"val","releases":[],"a_key":"val","signed_at":0.0}"#;
        let canonical = canonical_json(raw).expect("canonical_json should succeed");
        let text = String::from_utf8(canonical).unwrap();
        let pos_a = text.find("\"a_key\"").expect("a_key present");
        let pos_z = text.find("\"z_key\"").expect("z_key present");
        assert!(pos_a < pos_z, "keys should be sorted: a_key before z_key");
    }

    #[test]
    fn canonical_json_rejects_malformed_input() {
        assert!(canonical_json("not json").is_err());
        assert!(canonical_json("").is_err());
    }

    #[test]
    fn canonical_json_without_signature_field_is_stable() {
        // If no signature key is present, output should still be valid JSON.
        let raw = r#"{"releases":[],"signed_at":0.0}"#;
        let canonical = canonical_json(raw).expect("should succeed");
        let reparsed: serde_json::Value =
            serde_json::from_slice(&canonical).expect("should be valid JSON");
        assert!(reparsed.as_object().is_some());
    }

    // -----------------------------------------------------------------------
    // pad_base64
    // -----------------------------------------------------------------------

    #[test]
    fn pad_base64_no_padding_when_aligned() {
        assert_eq!(pad_base64("abcd"), "abcd");
        assert_eq!(pad_base64("abcdabcd"), "abcdabcd");
    }

    #[test]
    fn pad_base64_adds_one_pad_for_remainder_3() {
        // len 3: 3 % 4 == 3, needs 1 '='
        assert_eq!(pad_base64("abc"), "abc=");
    }

    #[test]
    fn pad_base64_adds_two_pads_for_remainder_2() {
        // len 2: 2 % 4 == 2, needs 2 '='
        assert_eq!(pad_base64("ab"), "ab==");
    }

    #[test]
    fn pad_base64_adds_three_pads_for_remainder_1() {
        // len 1: 1 % 4 == 1, needs 3 '='
        assert_eq!(pad_base64("a"), "a===");
    }

    // -----------------------------------------------------------------------
    // ReleaseManifest::contains
    // -----------------------------------------------------------------------

    #[test]
    fn contains_matches_exact_version_and_hash() {
        let manifest = ReleaseManifest {
            releases: vec![ManifestEntry {
                version: "1.0.0".to_string(),
                platform: "win64".to_string(),
                build_hash: "DEADBEEF".to_string(),
                build_id: "release-1.0.0-abc123".to_string(),
                published_at: 0.0,
            }],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert!(manifest.contains("1.0.0", "DEADBEEF"));
    }

    #[test]
    fn contains_is_case_insensitive_for_hash() {
        let manifest = ReleaseManifest {
            releases: vec![ManifestEntry {
                version: "1.0.0".to_string(),
                platform: "linux".to_string(),
                build_hash: "DEADBEEF".to_string(),
                build_id: "release-1.0.0-abc123".to_string(),
                published_at: 0.0,
            }],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert!(
            manifest.contains("1.0.0", "deadbeef"),
            "lowercase hash should match"
        );
        assert!(
            manifest.contains("1.0.0", "DeAdBeEf"),
            "mixed-case hash should match"
        );
    }

    #[test]
    fn contains_returns_false_for_wrong_version() {
        let manifest = ReleaseManifest {
            releases: vec![ManifestEntry {
                version: "1.0.0".to_string(),
                platform: "win64".to_string(),
                build_hash: "DEADBEEF".to_string(),
                build_id: "release-1.0.0-abc123".to_string(),
                published_at: 0.0,
            }],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert!(!manifest.contains("1.0.1", "DEADBEEF"));
        assert!(!manifest.contains("", "DEADBEEF"));
    }

    #[test]
    fn contains_returns_false_for_wrong_hash() {
        let manifest = ReleaseManifest {
            releases: vec![ManifestEntry {
                version: "2.0.0".to_string(),
                platform: "macos".to_string(),
                build_hash: "CAFEBABE".to_string(),
                build_id: String::new(),
                published_at: 0.0,
            }],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert!(!manifest.contains("2.0.0", "DEADBEEF"));
    }

    #[test]
    fn contains_returns_false_for_empty_releases() {
        let manifest = ReleaseManifest {
            releases: vec![],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert!(!manifest.contains("1.0.0", "any"));
    }

    #[test]
    fn contains_for_platform_matches_platform_and_hash() {
        let manifest = ReleaseManifest {
            releases: vec![ManifestEntry {
                version: "nightly-20260609".to_string(),
                platform: "win64".to_string(),
                build_hash: "abc123".to_string(),
                build_id: "nightly-deadbeef-42".to_string(),
                published_at: 0.0,
            }],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert!(manifest.contains_for_platform("win64", "abc123"));
        assert!(!manifest.contains_for_platform("macos-arm64", "abc123"));
    }

    #[test]
    fn build_hash_for_platform_returns_lowercase_hash() {
        let manifest = ReleaseManifest {
            releases: vec![ManifestEntry {
                version: "nightly-20260609".to_string(),
                platform: "win64".to_string(),
                build_hash: "ABC123".to_string(),
                build_id: String::new(),
                published_at: 0.0,
            }],
            signed_at: 0.0,
            signer_pubkey: String::new(),
            signature: String::new(),
        };
        assert_eq!(
            manifest.build_hash_for_platform("win64").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn parse_unsigned_rejects_empty_releases() {
        let raw = r#"{"releases":[],"signed_at":0.0}"#;
        assert!(ReleaseManifest::parse_unsigned(raw).is_err());
    }

    #[test]
    fn parse_unsigned_accepts_nightly_shape_without_signature() {
        let raw = r#"{
            "comment": "Development nightly build",
            "releases": [
                {"version":"nightly-20260609","platform":"win64","build_hash":"abc","build_id":"nightly-1","published_at":0.0}
            ],
            "signed_at":0.0
        }"#;
        let mf = ReleaseManifest::parse_unsigned(raw).expect("nightly manifest");
        assert_eq!(mf.releases.len(), 1);
    }

    #[test]
    fn verify_archive_hash_accepts_nightly_platform_entry() {
        let raw = r#"{
            "releases": [
                {"version":"nightly-20260609","platform":"win64","build_hash":"abc123","build_id":"nightly-1","published_at":0.0}
            ],
            "signed_at":0.0
        }"#;
        verify_archive_hash(raw, true, "nightly", "win64", "abc123").expect("verify");
    }

    // -----------------------------------------------------------------------
    // parse_and_verify
    // -----------------------------------------------------------------------

    #[test]
    fn parse_and_verify_rejects_invalid_signature() {
        // A manifest with a signature that does not verify against the configured
        // RELEASE_SIGNER_PUBKEY_HEX must be rejected.
        let raw = r#"{"releases":[],"signed_at":0.0,"signer_pubkey":"","signature":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#;
        let result = ReleaseManifest::parse_and_verify(raw);
        assert!(result.is_err(), "invalid signature must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("signature") || msg.contains("verification"),
            "error should mention signature verification failure; got: {msg}"
        );
    }

    #[test]
    fn parse_and_verify_rejects_malformed_json() {
        assert!(ReleaseManifest::parse_and_verify("not json").is_err());
        assert!(ReleaseManifest::parse_and_verify("").is_err());
        assert!(ReleaseManifest::parse_and_verify("[]").is_err());
    }

    // -----------------------------------------------------------------------
    // verify_signature: direct tests with real Ed25519 keys
    // -----------------------------------------------------------------------

    #[test]
    fn verify_signature_accepts_valid_ed25519_signature() {
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};

        let signing = SigningKey::from_bytes(&[42u8; 32]);
        let canonical = b"canonical manifest bytes";
        let sig = signing.sign(canonical);
        let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_bytes());
        let pubkey: [u8; 32] = signing.verifying_key().to_bytes();

        assert!(verify_signature(&sig_b64, canonical, &pubkey).is_ok());
    }

    #[test]
    fn verify_signature_rejects_wrong_message() {
        use base64::Engine;
        use ed25519_dalek::{Signer, SigningKey};

        let signing = SigningKey::from_bytes(&[99u8; 32]);
        let sig = signing.sign(b"real content");
        let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig.to_bytes());
        let pubkey: [u8; 32] = signing.verifying_key().to_bytes();

        assert!(verify_signature(&sig_b64, b"tampered content", &pubkey).is_err());
    }

    #[test]
    fn verify_signature_rejects_invalid_base64() {
        let pubkey = [0u8; 32];
        assert!(verify_signature("!!!invalid!!!", b"data", &pubkey).is_err());
    }

    #[test]
    fn verify_signature_rejects_short_signature() {
        use base64::Engine;
        // Valid base64 that decodes to fewer than 64 bytes — should fail the length check.
        let short = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"too_short");
        let pubkey = [0u8; 32];
        assert!(verify_signature(&short, b"data", &pubkey).is_err());
    }
}
