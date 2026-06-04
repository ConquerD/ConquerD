//! QUIC TLS helpers for ConquerD peer-to-peer transport.
//!
//! Mirrors the cert generation in `conquerd-quic/src/identity.rs`:
//! - Self-signed Ed25519 certificate, CN = hex(public_key_bytes).
//! - Both client and server use mutual TLS; cert verified by peer_id match.
//! - ALPN = b"conquerd/1".

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ED25519};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

/// ALPN protocol tag — must match the supernode and other peers.
pub const ALPN: &[u8] = b"conquerd/1";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Create a Quinn `Endpoint` bound on `0.0.0.0:<port>` (use port 0 for ephemeral).
///
/// The endpoint uses the caller's Ed25519 identity for its TLS certificate.
/// Both inbound and outbound connections use mutual TLS; certificate
/// verification is handled by `AcceptAnyCert` / `AcceptAnyClient` which
/// accept any valid cert — callers must verify the peer_id from the cert CN.
pub fn make_quic_endpoint(signing_key: &SigningKey, port: u16) -> anyhow::Result<quinn::Endpoint> {
    let (cert_chain, key_der) = generate_self_signed_cert(signing_key)?;

    let provider = Arc::new(rustls::crypto::ring::default_provider());

    // -- server config (accepts incoming connections) -----------------------
    let server_config = {
        let verifier = Arc::new(AcceptAnyClient);
        let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| anyhow::anyhow!("TLS1.3 not supported: {e}"))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(cert_chain.clone(), clone_key(&key_der))
            .map_err(|e| anyhow::anyhow!("Server cert: {e}"))?;
        cfg.alpn_protocols = vec![ALPN.to_vec()];
        cfg
    };

    // -- client config (for outgoing connections) ---------------------------
    let client_config = {
        let verifier = Arc::new(AcceptAnyCert);
        let mut cfg = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| anyhow::anyhow!("TLS1.3 not supported: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(cert_chain, key_der)
            .map_err(|e| anyhow::anyhow!("Client cert: {e}"))?;
        cfg.alpn_protocols = vec![ALPN.to_vec()];
        cfg
    };

    // -- transport timings --------------------------------------------------
    let transport = Arc::new({
        let mut t = quinn::TransportConfig::default();
        t.max_idle_timeout(Some(
            Duration::from_secs(120)
                .try_into()
                .map_err(|_| anyhow::anyhow!("idle timeout overflow"))?,
        ));
        t.keep_alive_interval(Some(Duration::from_secs(5)));
        t
    });

    let mut server_cfg = quinn::ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_config)
            .map_err(|e| anyhow::anyhow!("QuicServerConfig: {e}"))?,
    ));
    server_cfg.transport_config(Arc::clone(&transport));

    let mut client_cfg = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config)
            .map_err(|e| anyhow::anyhow!("QuicClientConfig: {e}"))?,
    ));
    client_cfg.transport_config(Arc::clone(&transport));

    // -- bind endpoint ------------------------------------------------------
    let bind_addr: std::net::SocketAddr = format!("0.0.0.0:{port}").parse()?;
    let mut endpoint = quinn::Endpoint::server(server_cfg, bind_addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

/// Derive `peer_id` from raw Ed25519 public key bytes: `hex(sha256(pub_bytes))`.
pub fn peer_id_from_pub_bytes(pub_bytes: &[u8]) -> String {
    let hash = Sha256::digest(pub_bytes);
    hex::encode(hash)
}

/// Extract the hex-encoded CN from a DER-encoded self-signed certificate.
pub fn cn_from_cert_der(cert_der: &CertificateDer<'_>) -> Option<String> {
    parse_cn_from_der(cert_der.as_ref())
}

// ---------------------------------------------------------------------------
// Cert generation
// ---------------------------------------------------------------------------

fn generate_self_signed_cert(
    signing_key: &SigningKey,
) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let pub_bytes = signing_key.verifying_key().as_bytes().to_vec();
    let cn = hex::encode(&pub_bytes);

    let pkcs8 = build_ed25519_pkcs8(signing_key);
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8));

    let key_pair = KeyPair::from_der_and_sign_algo(&key_der, &PKCS_ED25519)
        .map_err(|e| anyhow::anyhow!("KeyPair: {e}"))?;

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, &cn);
    params.distinguished_name = dn;
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2030, 1, 1);

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow::anyhow!("Self-sign: {e}"))?;
    let cert_der = CertificateDer::from(cert.der().to_vec());
    Ok((vec![cert_der], key_der))
}

fn clone_key(key: &PrivateKeyDer<'static>) -> PrivateKeyDer<'static> {
    match key {
        PrivateKeyDer::Pkcs8(k) => {
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(k.secret_pkcs8_der().to_vec()))
        }
        PrivateKeyDer::Pkcs1(k) => PrivateKeyDer::Pkcs1(
            rustls::pki_types::PrivatePkcs1KeyDer::from(k.secret_pkcs1_der().to_vec()),
        ),
        PrivateKeyDer::Sec1(k) => PrivateKeyDer::Sec1(rustls::pki_types::PrivateSec1KeyDer::from(
            k.secret_sec1_der().to_vec(),
        )),
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// Custom verifiers — accept any cert; peer_id verified by caller
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct AcceptAnyCert;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct AcceptAnyClient;

impl rustls::server::danger::ClientCertVerifier for AcceptAnyClient {
    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        Ok(rustls::server::danger::ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn client_auth_mandatory(&self) -> bool {
        false // optional client cert — peer connects with or without identity
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
}

// ---------------------------------------------------------------------------
// DER CN parser
// ---------------------------------------------------------------------------

fn parse_cn_from_der(der: &[u8]) -> Option<String> {
    // CN OID 2.5.4.3 → [0x55, 0x04, 0x03]
    let cn_oid = [0x55u8, 0x04, 0x03];
    let pos = der.windows(cn_oid.len()).position(|w| w == cn_oid)?;
    let after_oid = pos + cn_oid.len();
    if after_oid >= der.len() {
        return None;
    }
    let tag = der[after_oid];
    if tag != 0x0C && tag != 0x13 {
        return None;
    }
    let len_pos = after_oid + 1;
    if len_pos >= der.len() {
        return None;
    }
    let (value_len, consumed) = parse_der_length(&der[len_pos..])?;
    let value_start = len_pos + consumed;
    let value_end = value_start + value_len;
    if value_end > der.len() {
        return None;
    }
    std::str::from_utf8(&der[value_start..value_end])
        .ok()
        .map(|s| s.to_owned())
}

fn parse_der_length(data: &[u8]) -> Option<(usize, usize)> {
    if data.is_empty() {
        return None;
    }
    let first = data[0];
    if first < 0x80 {
        Some((first as usize, 1))
    } else {
        let num_bytes = (first & 0x7F) as usize;
        if num_bytes == 0 || data.len() < 1 + num_bytes {
            return None;
        }
        let mut len = 0usize;
        for &b in &data[1..1 + num_bytes] {
            len = (len << 8) | b as usize;
        }
        Some((len, 1 + num_bytes))
    }
}

// ---------------------------------------------------------------------------
// PKCS#8 encoding for Ed25519
// ---------------------------------------------------------------------------

fn build_ed25519_pkcs8(key: &SigningKey) -> Vec<u8> {
    let raw = key.to_bytes();
    let inner_octet = asn1_octet_string(&raw);
    let outer_octet = asn1_octet_string(&inner_octet);
    let oid_bytes = [0x06u8, 0x03, 0x2b, 0x65, 0x70]; // OID 1.3.101.112
    let algo_seq = asn1_sequence(&oid_bytes);
    let version = [0x02u8, 0x01, 0x00];
    let mut inner = Vec::new();
    inner.extend_from_slice(&version);
    inner.extend_from_slice(&algo_seq);
    inner.extend_from_slice(&outer_octet);
    asn1_sequence(&inner)
}

fn asn1_sequence(data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0x30u8];
    encode_asn1_length(data.len(), &mut buf);
    buf.extend_from_slice(data);
    buf
}

fn asn1_octet_string(data: &[u8]) -> Vec<u8> {
    let mut buf = vec![0x04u8];
    encode_asn1_length(data.len(), &mut buf);
    buf.extend_from_slice(data);
    buf
}

fn encode_asn1_length(len: usize, buf: &mut Vec<u8>) {
    if len < 0x80 {
        buf.push(len as u8);
    } else if len < 0x100 {
        buf.extend_from_slice(&[0x81, len as u8]);
    } else {
        buf.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
}
