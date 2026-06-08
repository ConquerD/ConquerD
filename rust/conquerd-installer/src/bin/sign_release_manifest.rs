use anyhow::{Context, Result};
use clap::Parser;
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Must be kept in sync with rust/conquerd-installer/src/release_manifest.rs
/// RELEASE_SIGNER_PUBKEY_HEX (and the committed keys/release-signer-public.pem).
const RELEASE_SIGNER_PUBKEY_HEX: &str =
    "d31f43fcfba1fae04313d384d7fba026bd52796550c57def6cf47b069c18043f";

/// Sign a ConquerD release manifest using an Ed25519 private key seed.
///
/// The input should be the unsigned manifest JSON.
/// Output is pretty-printed signed JSON with "signature" and "signer_pubkey".
///
/// Private key file can be:
/// - 32 raw bytes (the Ed25519 seed)
/// - file containing 64 hex characters (the seed, whitespace tolerant)
/// - PEM file from `openssl genpkey -algorithm Ed25519 -out release-signer-private.pem`
///   (PKCS#8 format is supported; the 32-byte seed is extracted from the DER)
///
/// The --pubkey must match RELEASE_SIGNER_PUBKEY_HEX in release_manifest.rs (and the committed
/// keys/release-signer-public.pem) at the time the signed manifest is produced.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value = "releases_manifest.json")]
    input: String,
    #[arg(short, long, default_value = "-")]
    output: String,
    #[arg(short, long)]
    private_key: Option<PathBuf>,
    /// The 64-char hex public key to embed in the manifest as signer_pubkey.
    /// Must match the verifier constant in the installer at build time.
    #[arg(
        long,
        default_value = "d31f43fcfba1fae04313d384d7fba026bd52796550c57def6cf47b069c18043f"
    )]
    pubkey: String,

    /// Run an internal roundtrip self-test (generates a throwaway Ed25519 key,
    /// signs a sample manifest, verifies the signature using the same canonical
    /// form as the installer). Does not require a private key file. Useful for
    /// CI and to prove the signer/verifier pipeline is consistent.
    #[arg(long)]
    self_test: bool,

    /// Generate an *unsigned* skeleton releases_manifest.json for the current
    /// version (read from Cargo.toml) with all supported platforms. Fill in the
    /// real build_hash (SHA-256 of the final .7z / dmg / AppImage) and build_id
    /// after the CI build or local `build_*.ps1` run. Then feed the file to the
    /// normal sign flow with your private key.
    ///
    /// Does not require --private-key. Writes to -o (default releases_manifest.json).
    #[arg(long)]
    generate_unsigned: bool,

    /// Verify a (signed) releases_manifest.json against the public key compiled
    /// into this binary (the one in release_manifest.rs). Exits non-zero if the
    /// signature is missing, malformed, or does not verify. Intended for CI
    /// preflight and approver sanity checks. Uses the exact same canonical form
    /// as the installer.
    #[arg(long)]
    verify: bool,

    /// Sign a P2P build-attestation claim (`build_id=...,version=...` with
    /// optional `,source_hash=...`) and print the base64 signature to stdout.
    /// Used by CI to bake CONQUERD_RELEASE_PROOF into official binaries.
    #[arg(long)]
    sign_build_claim: bool,
    #[arg(long)]
    build_id: Option<String>,
    /// App version for --sign-build-claim (not clap's built-in --version).
    #[arg(long = "claim-version")]
    claim_version: Option<String>,
    #[arg(long)]
    source_hash: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.self_test {
        return do_self_test();
    }

    if args.generate_unsigned {
        let skeleton = generate_unsigned_skeleton(&args.pubkey);
        if args.output == "-" {
            println!("{}", skeleton);
        } else {
            fs::write(&args.output, &skeleton)
                .with_context(|| format!("Failed to write unsigned skeleton: {}", args.output))?;
            eprintln!("Wrote unsigned skeleton to {}", args.output);
            eprintln!("Edit the three platform entries (build_hash from the .sha256 asset, build_id from baked/CI value).");
            eprintln!(
                "Then sign with: --private-key C:\\path\\to\\release-signer-private.pem -i {} -o releases_manifest.json",
                args.output
            );
            eprintln!("(PEM files from openssl genpkey are supported directly)");
        }
        return Ok(());
    }

    if args.verify {
        return do_verify(&args.input);
    }

    if args.sign_build_claim {
        let private_key = args
            .private_key
            .context("--private-key is required for --sign-build-claim")?;
        let build_id = args
            .build_id
            .context("--build-id is required for --sign-build-claim")?;
        let version = args
            .claim_version
            .context("--claim-version is required for --sign-build-claim")?;
        let seed = load_private_seed(&private_key)?;
        let sig_b64 = sign_build_claim(&seed, &build_id, &version, args.source_hash.as_deref())?;
        print!("{sig_b64}");
        return Ok(());
    }

    let private_key = args.private_key.context(
        "--private-key is required (or use --self-test / --generate-unsigned / --verify)",
    )?;
    let unsigned = if args.input == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        fs::read_to_string(&args.input)
            .with_context(|| format!("Failed to read input manifest: {}", args.input))?
    };
    let seed = load_private_seed(&private_key)?;
    let signed = sign_manifest(&unsigned, &seed, &args.pubkey)?;
    if args.output == "-" {
        println!("{}", signed);
    } else {
        fs::write(&args.output, &signed)
            .with_context(|| format!("Failed to write signed manifest: {}", args.output))?;
        eprintln!("Wrote signed manifest to {}", args.output);
    }
    Ok(())
}

fn load_private_seed(path: &PathBuf) -> Result<[u8; 32]> {
    let data = fs::read(path)
        .with_context(|| format!("Failed to read private key file: {}", path.display()))?;

    // 1. Exact 32-byte raw seed file
    if data.len() == 32 {
        return Ok(data.try_into().unwrap());
    }

    // 2. Hex-encoded seed (64 chars, tolerate whitespace/newlines)
    let hex_str: String = String::from_utf8_lossy(&data)
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();
    if hex_str.len() == 64 {
        let bytes = hex::decode(&hex_str).context("Failed to hex-decode private seed")?;
        return bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Seed must be 32 bytes"));
    }

    // 3. PEM-encoded Ed25519 private key (supports openssl genpkey output)
    let text = String::from_utf8_lossy(&data);
    if text.contains("-----BEGIN") {
        let pem = pem::parse(&data).context("Failed to parse PEM private key")?;
        let der = pem.contents();

        // Look for OCTET STRING (tag 0x04, len 0x20) containing the 32-byte seed.
        // Common in PKCS#8 Ed25519 keys from OpenSSL.
        for i in 0..=der.len().saturating_sub(34) {
            if der[i] == 0x04 && der[i + 1] == 0x20 {
                let seed: [u8; 32] = der[i + 2..i + 34]
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("Bad seed slice in PEM"))?;
                return Ok(seed);
            }
        }

        // Fallback: many Ed25519 private keys have the seed as the last 32 bytes.
        if der.len() >= 32 {
            let seed: [u8; 32] = der[der.len() - 32..]
                .try_into()
                .map_err(|_| anyhow::anyhow!("Bad trailing seed slice in PEM"))?;
            return Ok(seed);
        }

        anyhow::bail!("Could not locate 32-byte Ed25519 seed inside the PEM DER");
    }

    anyhow::bail!(
        "Private key must be 32 raw bytes, 64-hex seed, or PEM (BEGIN PRIVATE KEY). Got {} bytes.",
        data.len()
    )
}

/// Sign the canonical build-attestation claim baked into official binaries.
fn sign_build_claim(
    private_seed: &[u8; 32],
    build_id: &str,
    version: &str,
    source_hash: Option<&str>,
) -> Result<String> {
    let claim = match source_hash {
        Some(h) if !h.is_empty() => {
            format!("build_id={build_id},version={version},source_hash={h}")
        }
        _ => format!("build_id={build_id},version={version}"),
    };
    let signing_key = SigningKey::from_bytes(private_seed);
    let signature = signing_key.sign(claim.as_bytes());
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()))
}

fn sign_manifest(unsigned_json: &str, private_seed: &[u8; 32], pubkey_hex: &str) -> Result<String> {
    let mut manifest: Value =
        serde_json::from_str(unsigned_json).context("Failed to parse input as JSON")?;
    let map = manifest
        .as_object_mut()
        .context("Manifest must be a JSON object")?;
    map.remove("signature");
    map.insert(
        "signer_pubkey".to_string(),
        Value::String(pubkey_hex.to_string()),
    );
    let sorted: BTreeMap<String, Value> = map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let canonical =
        serde_json::to_vec(&sorted).context("Failed to serialize canonical manifest")?;
    let signing_key = SigningKey::from_bytes(private_seed);
    let signature = signing_key.sign(&canonical);
    let sig_bytes = signature.to_bytes();
    use base64::Engine;
    let sig_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig_bytes);
    map.insert("signature".to_string(), Value::String(sig_b64));
    if !map.contains_key("signed_at") {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        map.insert(
            "signed_at".to_string(),
            Value::Number(serde_json::Number::from_f64(now).unwrap()),
        );
    }
    let signed_json = serde_json::to_string_pretty(&manifest)?;
    Ok(signed_json)
}

/// Internal self-test: proves that sign_manifest produces a canonical form + signature
/// that the installer's verification logic (canonical_json + verify_signature) would accept.
/// Uses a throwaway key so it works in CI without any secrets.
fn do_self_test() -> Result<()> {
    use base64::Engine;
    use ed25519_dalek::{SigningKey, Verifier as _};

    eprintln!("sign-release-manifest self-test: generating throwaway Ed25519 keypair...");

    let mut seed = [0u8; 32];
    // Use a fixed test vector for deterministic output (not a real secret)
    for (i, b) in seed.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(17).wrapping_add(42);
    }
    let signing_key = SigningKey::from_bytes(&seed);
    let verifying_key = signing_key.verifying_key();
    let pubkey_hex = hex::encode(verifying_key.to_bytes());

    // Sample unsigned manifest (typical shape)
    let unsigned = r#"{
  "releases": [
    {"version": "1.0.0", "platform": "win64", "build_hash": "deadbeef0123456789abcdef0123456789abcdef0123456789abcdef01234567", "build_id": "release-1.0.0-abc1234", "published_at": 1710000000.0}
  ],
  "signed_at": 1710000000.0
}"#;

    let signed = sign_manifest(unsigned, &seed, &pubkey_hex)?;

    // Now replicate the *verifier* side using only the pieces also present in the bin:
    // 1. Re-parse, remove signature for canonical
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&signed).context("self-test: failed to reparse signed json")?;
    let provided_sig = map
        .remove("signature")
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .context("self-test: no signature in produced manifest")?;

    let sorted: std::collections::BTreeMap<_, _> = map.into_iter().collect();
    let canonical = serde_json::to_vec(&sorted).context("self-test: canonical serialize failed")?;

    // 2. Decode sig (exactly as release_manifest.rs base64_decode + pad_base64 does)
    let sig_bytes = {
        let s = &provided_sig;
        let rem = s.len() % 4;
        let padded = if rem == 0 {
            s.to_string()
        } else {
            format!("{}{}", s, "=".repeat(4 - rem))
        };
        base64::engine::general_purpose::URL_SAFE
            .decode(&padded)
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&padded))
            .context("self-test: signature base64 decode failed")?
    };
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("self-test: signature not 64 bytes"))?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

    // 3. Verify
    let vk = verifying_key;
    vk.verify(&canonical, &sig)
        .context("self-test: signature verification FAILED")?;

    // Also confirm the signer_pubkey field was written and matches
    let reparsed: serde_json::Value = serde_json::from_str(&signed)?;
    let embedded_pk = reparsed
        .get("signer_pubkey")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        embedded_pk, pubkey_hex,
        "self-test: signer_pubkey in output must match the one we signed for"
    );

    eprintln!(
        "self-test: canonical form + sign + verify roundtrip OK (pubkey={})",
        &pubkey_hex[..16]
    );
    eprintln!("self-test: produced {} byte signed manifest", signed.len());
    Ok(())
}

/// Generate a ready-to-edit unsigned skeleton for the current release.
/// Version is read from rust/conquerd-client/Cargo.toml so it stays in sync.
/// Platforms match the current release workflow artifacts (win64 .7z, macos-arm64 .dmg, linux x86_64 AppImage).
/// A realistic clean sample `build_id` (format produced by build.rs on tagged clean checkout,
/// or injected by CI as "release-{tag}-{sha12}") is embedded as an example.
/// The `comment` field is preserved after signing (useful for auditors; unknown fields are ignored by the verifier).
fn generate_unsigned_skeleton(pubkey_hex: &str) -> String {
    // Determine version dynamically (works from repo root or rust/ subdir).
    let version = {
        let candidates = [
            "rust/conquerd-client/Cargo.toml",
            "conquerd-client/Cargo.toml",
            "../conquerd-client/Cargo.toml",
        ];
        let mut ver = "1.0.0".to_string();
        for p in &candidates {
            if let Ok(content) = std::fs::read_to_string(p) {
                for line in content.lines() {
                    let t = line.trim_start();
                    if t.starts_with("version") {
                        if let Some(v) = t.split('"').nth(1) {
                            ver = v.to_string();
                            break;
                        }
                    }
                }
                if ver != "1.0.0" {
                    break;
                }
            }
        }
        ver
    };

    // Sample clean build_id (what a `git checkout v1.0.0 && cargo build` (no env override) or
    // the CI "release-${{ github.ref_name }}-${{ github.sha }}" (short) produces).
    // Peers will see/attest this exact string for official release binaries.
    let sample_build_id = format!("release-{}-18eae80", version);

    // published_at 0 in the template; signer will populate a real signed_at.
    let skeleton = format!(
        r#"{{
  "comment": "UNSIGNED skeleton for ConquerD {version}. Replace build_hash (full lowercase SHA-256 of the final published archive from the .sha256 asset) and build_id (exact CONQUERD_BUILD_ID baked at build time) for each platform. Example build_id: 'release-1.0.0-18eae80' or CI value. Then sign with your private key using the sign-release-manifest binary (or the .ps1 wrapper). The signed result (containing 'signature') is the file to commit and attach to the release.",
  "releases": [
    {{
      "version": "{version}",
      "platform": "win64",
      "build_hash": "REPLACE_WITH_SHA256_OF_ConquerD-{version}-win64.7z",
      "build_id": "{sample_build_id}",
      "published_at": 0.0
    }},
    {{
      "version": "{version}",
      "platform": "macos-arm64",
      "build_hash": "REPLACE_WITH_SHA256_OF_ConquerD-{version}-macos-arm64.dmg",
      "build_id": "{sample_build_id}",
      "published_at": 0.0
    }},
    {{
      "version": "{version}",
      "platform": "linux-x86_64",
      "build_hash": "REPLACE_WITH_SHA256_OF_ConquerD-{version}-x86_64.AppImage",
      "build_id": "{sample_build_id}",
      "published_at": 0.0
    }}
  ],
  "signed_at": 0.0,
  "signer_pubkey": "{pubkey_hex}"
}}"#,
        version = version,
        sample_build_id = sample_build_id,
        pubkey_hex = pubkey_hex
    );

    // Re-parse + pretty-print (consistent with sign path + validates JSON).
    match serde_json::from_str::<serde_json::Value>(&skeleton) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or(skeleton),
        Err(_) => skeleton,
    }
}

/// Verify a signed manifest file (or - for stdin) using the same canonicalization
/// and Ed25519 rules as ReleaseManifest::parse_and_verify in the installer.
/// Exits 0 only if the signature is present and validates against the compiled-in key.
fn do_verify(input: &str) -> Result<()> {
    let raw = if input == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else {
        fs::read_to_string(input)
            .with_context(|| format!("Failed to read manifest for verification: {}", input))?
    };

    use base64::Engine;
    use ed25519_dalek::Verifier;

    // Minimal reimplementation of the verifier (so the bin stays self-contained).
    // 1. Parse as the struct (to get the signature field).
    #[derive(serde::Deserialize)]
    struct MinimalManifest {
        #[serde(default)]
        signature: String,
        #[serde(flatten)]
        _rest: std::collections::BTreeMap<String, serde_json::Value>,
    }
    let m: MinimalManifest = serde_json::from_str(&raw)
        .context("Manifest is not valid JSON or is missing required shape")?;

    if m.signature.trim().is_empty() {
        anyhow::bail!(
            "Manifest has no 'signature' field (it is probably still the unsigned skeleton)"
        );
    }

    // 2. Build canonical (remove signature, BTree sort) — same as canonical_json in release_manifest.rs
    let mut map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&raw).context("Could not re-parse for canonicalization")?;
    map.remove("signature");
    let sorted: BTreeMap<_, _> = map.into_iter().collect();
    let canonical = serde_json::to_vec(&sorted)
        .context("Failed to produce canonical bytes for verification")?;

    // 3. Decode signature (url-safe no-pad preferred, then standard, with padding fix) — matches the installer
    let sig_bytes = {
        let s = m.signature.trim();
        let rem = s.len() % 4;
        let padded = if rem == 0 {
            s.to_string()
        } else {
            format!("{}{}", s, "=".repeat(4 - rem))
        };
        base64::engine::general_purpose::URL_SAFE
            .decode(&padded)
            .or_else(|_| base64::engine::general_purpose::STANDARD.decode(&padded))
            .context("Signature base64 is invalid")?
    };
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Signature must decode to exactly 64 bytes"))?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);

    // 4. Verify with the compiled-in key (must match the one in release_manifest.rs)
    if RELEASE_SIGNER_PUBKEY_HEX == "0".repeat(64) {
        anyhow::bail!("RELEASE_SIGNER_PUBKEY_HEX is still the placeholder — cannot verify");
    }
    let pubkey_bytes: [u8; 32] = hex::decode(RELEASE_SIGNER_PUBKEY_HEX)
        .context("Invalid RELEASE_SIGNER_PUBKEY_HEX in the signer binary")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("Public key must be 32 bytes"))?;
    let vk = VerifyingKey::from_bytes(&pubkey_bytes)
        .context("Compiled public key is not a valid Ed25519 key")?;

    vk.verify(&canonical, &sig).context(
        "Ed25519 signature verification FAILED — manifest is not authentic or was tampered with",
    )?;

    eprintln!("OK: releases_manifest.json signature verifies against the project release key.");
    eprintln!("    signer_pubkey: {}", RELEASE_SIGNER_PUBKEY_HEX);
    Ok(())
}
