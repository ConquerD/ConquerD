use anyhow::{Context, Result};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;

/// Hex-encoded Ed25519 public key of the ConquerD release signer.
/// Replace with the actual publisher key before shipping.
const RELEASE_SIGNER_PUBKEY_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// One entry in the release manifest.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ManifestEntry {
    pub version: String,
    pub platform: String,
    pub build_hash: String,
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
    ///  - The Ed25519 signature is invalid (unless the embedded key is all-zero,
    ///    in which case verification is skipped for development builds)
    pub fn parse_and_verify(raw_json: &str) -> Result<Self> {
        let manifest: ReleaseManifest =
            serde_json::from_str(raw_json).context("Failed to parse releases_manifest.json")?;

        if RELEASE_SIGNER_PUBKEY_HEX == "0".repeat(64) {
            anyhow::bail!(
                "RELEASE_SIGNER_PUBKEY_HEX is the all-zero placeholder — \
                set a real Ed25519 public key before releasing"
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

    /// Return true if *(version, build_hash)* appears for any platform.
    pub fn contains(&self, version: &str, build_hash: &str) -> bool {
        self.releases.iter().any(|e| {
            e.version == version && e.build_hash.to_lowercase() == build_hash.to_lowercase()
        })
    }
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

    // -----------------------------------------------------------------------
    // parse_and_verify
    // -----------------------------------------------------------------------

    #[test]
    fn parse_and_verify_rejects_placeholder_signing_key() {
        // The codebase ships with the all-zero placeholder; verify it always errors.
        let raw = r#"{"releases":[],"signed_at":0.0,"signer_pubkey":"","signature":""}"#;
        let result = ReleaseManifest::parse_and_verify(raw);
        assert!(result.is_err(), "placeholder key must be rejected");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("placeholder") || msg.contains("real Ed25519"),
            "error should mention placeholder key; got: {msg}"
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
