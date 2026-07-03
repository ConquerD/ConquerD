use super::internal::{host_from_url, is_loopback_or_wildcard, rewrite_loopback_wt_url};
use super::manager::{
    build_room_invite_url, parse_quic_lan_hint, parse_room_invite, peer_quic_endpoint,
    RoomInvitePayload, ROOM_INVITE_SCHEMA,
};
use super::ConnectionManager;

#[test]
fn parses_saved_quic_endpoints() {
    assert_eq!(
        parse_quic_lan_hint("quic://192.168.1.20:61046"),
        Some(("192.168.1.20".to_owned(), 61046))
    );
    assert_eq!(
        parse_quic_lan_hint("udp://[2001:db8::1]:61047"),
        Some(("2001:db8::1".to_owned(), 61047))
    );
    assert_eq!(parse_quic_lan_hint("quic://localhost:0"), None);
}

#[test]
fn peer_endpoint_prefers_persisted_hint() {
    let record = crate::peer_store::PeerRecord {
        relay_hints: vec!["quic://10.0.0.8:61048".to_owned()],
        quic_port: 61049,
        ..Default::default()
    };
    assert_eq!(
        peer_quic_endpoint(&record),
        Some(("10.0.0.8".to_owned(), 61048))
    );
}

#[test]
fn host_from_url_variants() {
    assert_eq!(
        host_from_url("ws://1.2.3.4:34935/sig").as_deref(),
        Some("1.2.3.4")
    );
    assert_eq!(
        host_from_url("wss://relay.example:443").as_deref(),
        Some("relay.example")
    );
    assert_eq!(
        host_from_url("https://localhost:8443").as_deref(),
        Some("localhost")
    );
    assert_eq!(
        host_from_url("relay.example:34935").as_deref(),
        Some("relay.example")
    );
    assert_eq!(
        host_from_url("ws://user@host:80/x").as_deref(),
        Some("host")
    );
    assert_eq!(host_from_url("https://[::1]:8443").as_deref(), Some("::1"));
    assert_eq!(
        host_from_url("https://[2001:db8::1]:8443").as_deref(),
        Some("2001:db8::1")
    );
    assert_eq!(host_from_url(""), None);
}

#[test]
fn room_invite_url_round_trips() {
    let url = build_room_invite_url(
        "supernode-identity-pub",
        "wss://relay.example:443/sig",
        "room-abc",
        "Team Standup",
        "private",
        "f4052efe6d931922582f2f4ef4cec47f",
        1_800_000_000,
    );
    assert!(url.starts_with("conquerd://room#"), "url = {url}");
    let encoded = url.strip_prefix("conquerd://room#").unwrap();
    assert_eq!(
        parse_room_invite(encoded).unwrap(),
        RoomInvitePayload {
            supernode_id: "supernode-identity-pub".into(),
            supernode_hint: "wss://relay.example:443/sig".into(),
            room_id: "room-abc".into(),
            room_name: "Team Standup".into(),
            room_type: "private".into(),
            invite_token: "f4052efe6d931922582f2f4ef4cec47f".into(),
            expires_at: 1_800_000_000,
        }
    );
}

/// Invites minted before `room_type` existed (and any with it blank) default to
/// private — the only kind that existed then — so the token path still runs.
#[test]
fn room_invite_defaults_room_type_to_private() {
    let bare = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        br#"{"v":1,"supernode_id":"s","room_id":"r"}"#,
    );
    assert_eq!(parse_room_invite(&bare).unwrap().room_type, "private");
}

/// Wire-format field stability guard for the room invite payload.
///
/// If you rename a field, keep the JSON key stable and update this list; a real
/// wire change must bump `ROOM_INVITE_SCHEMA` and add migration in
/// `parse_room_invite`.
#[test]
fn room_invite_wire_fields_are_stable() {
    let url = build_room_invite_url("sn", "wss://h:443", "r", "n", "private", "tok", 42);
    let encoded = url.strip_prefix("conquerd://room#").unwrap();
    let json_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, encoded).unwrap();
    let obj: serde_json::Value = serde_json::from_slice(&json_bytes).unwrap();
    for key in [
        "v",
        "supernode_id",
        "supernode_hint",
        "room_id",
        "room_name",
        "room_type",
        "invite_token",
        "expires_at",
    ] {
        assert!(
            obj.get(key).is_some(),
            "room invite wire field missing or renamed: `{key}`"
        );
    }
    assert_eq!(obj["v"].as_u64(), Some(ROOM_INVITE_SCHEMA as u64));
}

#[test]
fn room_invite_rejects_missing_required_fields() {
    // Missing supernode_id.
    let bad = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        br#"{"v":1,"room_id":"r"}"#,
    );
    assert!(parse_room_invite(&bad).is_err());
    // Unknown future schema version is refused.
    let future = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        br#"{"v":999,"supernode_id":"s","room_id":"r"}"#,
    );
    assert!(parse_room_invite(&future).is_err());
}

#[test]
fn loopback_detection() {
    for h in ["localhost", "127.0.0.1", "0.0.0.0", "::1", "::"] {
        assert!(
            is_loopback_or_wildcard(h),
            "{h} should be loopback/wildcard"
        );
    }
    for h in ["1.2.3.4", "relay.example", "example.com"] {
        assert!(!is_loopback_or_wildcard(h), "{h} should be routable");
    }
}

#[test]
fn rewrites_loopback_host_using_signaling_url() {
    let fixed = rewrite_loopback_wt_url("https://localhost:8443", "ws://203.0.113.7:34935/sig");
    assert_eq!(fixed.as_deref(), Some("https://203.0.113.7:8443"));
}

#[test]
fn rewrites_wildcard_host_and_preserves_port() {
    let fixed = rewrite_loopback_wt_url("https://0.0.0.0:9000", "wss://relay.example:443");
    assert_eq!(fixed.as_deref(), Some("https://relay.example:9000"));
}

#[test]
fn no_rewrite_when_wt_host_already_routable() {
    assert!(
        rewrite_loopback_wt_url("https://relay.example:8443", "ws://203.0.113.7:34935",).is_none()
    );
}

#[test]
fn no_rewrite_when_signaling_host_is_also_loopback() {
    assert!(rewrite_loopback_wt_url("https://localhost:8443", "ws://127.0.0.1:34935",).is_none());
}

#[test]
fn trusted_sender_gate_resolves_and_excludes() {
    use crate::identity::Identity;
    use crate::peer_store::{PeerRecord, PeerStore};
    use parking_lot::RwLock;
    use std::sync::Arc;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let id = Identity::generate();
    let mut store = PeerStore::open(&id, Some(&dir.path().join("peers.dat"))).unwrap();

    store.upsert(PeerRecord {
        peer_id: "hexpeerid".to_owned(),
        identity_pub: "base64identity".to_owned(),
        handle: "Trusted".to_owned(),
        ..Default::default()
    });
    store.upsert(PeerRecord {
        peer_id: "hexblocked".to_owned(),
        identity_pub: "base64blocked".to_owned(),
        blocked: true,
        ..Default::default()
    });
    store.upsert(PeerRecord {
        peer_id: "hexrevoked".to_owned(),
        identity_pub: "base64revoked".to_owned(),
        revoked: true,
        ..Default::default()
    });

    let store = Arc::new(RwLock::new(store));

    assert!(ConnectionManager::is_trusted_sender(
        &store,
        "base64identity"
    ));
    assert!(ConnectionManager::is_trusted_sender(&store, "hexpeerid"));
    assert!(!ConnectionManager::is_trusted_sender(&store, "stranger"));
    assert!(!ConnectionManager::is_trusted_sender(
        &store,
        "base64blocked"
    ));
    assert!(!ConnectionManager::is_trusted_sender(
        &store,
        "base64revoked"
    ));
}

#[test]
fn verify_inbound_signature_rejects_stale_and_future_timestamps() {
    use crate::identity::Identity;
    use crate::protocol::{MessageType, SignalingMessage};
    use base64::Engine;

    const MAX_AGE: f64 = 300.0;
    let id = Identity::generate();

    let signed = |timestamp: f64| -> SignalingMessage {
        let mut msg = SignalingMessage::new(MessageType::ChatMessage, id.public_id());
        msg.timestamp = timestamp;
        msg.target = Some("peer-target".to_owned());
        let canonical = msg.canonical_bytes().expect("canonical");
        let sig = id.sign(&canonical);
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        msg
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);

    assert!(ConnectionManager::verify_inbound_signature_for_test(
        &signed(now)
    ));
    assert!(!ConnectionManager::verify_inbound_signature_for_test(
        &signed(now - MAX_AGE - 1.0)
    ));
    assert!(!ConnectionManager::verify_inbound_signature_for_test(
        &signed(now + MAX_AGE + 1.0)
    ));
}

/// End-to-end guard for the supernode-relay `EncryptedSignal` envelope: two
/// paired peers derive the same pairwise key, the wrapped wire form leaks no
/// plaintext, and the inner message survives the round-trip with its own
/// signature intact. Mirrors the format produced by `maybe_wrap_for_relay`
/// and consumed by the inbound `EncryptedSignal` arm.
#[test]
fn encrypted_signal_envelope_round_trips_and_hides_plaintext() {
    use crate::crypto::{b64url_decode, b64url_encode, decrypt_blob, encrypt_blob};
    use crate::identity::Identity;
    use crate::protocol::{MessageType, SignalingMessage};
    use base64::Engine;
    use serde_json::Value;

    let alice = Identity::generate();
    let bob = Identity::generate();

    // Alice builds + signs an inner ChatMessage targeted at Bob.
    let mut inner = SignalingMessage::new(MessageType::ChatMessage, alice.public_id());
    inner.target = Some(bob.public_id());
    inner
        .payload
        .insert("body".into(), Value::String("secret hi".into()));
    inner
        .payload
        .insert("message_id".into(), Value::String("m1".into()));
    let canonical = inner.canonical_bytes().unwrap();
    inner.signature =
        Some(base64::engine::general_purpose::URL_SAFE.encode(alice.sign(&canonical)));
    let inner_json = inner.to_json().unwrap();

    // Alice wraps it for the relay (same steps as `maybe_wrap_for_relay`).
    let key_a = alice.derive_pairwise_relay_key(&bob.public_id()).unwrap();
    let ct = encrypt_blob(&key_a, inner_json.as_bytes()).unwrap();
    let mut env = SignalingMessage::new(MessageType::EncryptedSignal, alice.public_id());
    env.target = Some(bob.public_id());
    env.payload
        .insert("ciphertext".into(), Value::String(b64url_encode(&ct)));
    let env_canon = env.canonical_bytes().unwrap();
    env.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(alice.sign(&env_canon)));

    // The relayed wire form exposes neither the inner type nor its content.
    assert_eq!(env.msg_type, MessageType::EncryptedSignal);
    assert!(!env.payload.contains_key("body"));
    let env_wire = env.to_json().unwrap();
    assert!(!env_wire.contains("secret hi"));
    assert!(!env_wire.contains("chat_message"));
    // The envelope itself is signature-valid + fresh (supernode relays it as-is).
    assert!(ConnectionManager::verify_inbound_signature_for_test(&env));

    // Bob derives the identical key, decrypts, and recovers the inner message.
    let key_b = bob.derive_pairwise_relay_key(&alice.public_id()).unwrap();
    assert_eq!(key_a, key_b);
    let ct_b = b64url_decode(env.payload.get("ciphertext").unwrap().as_str().unwrap()).unwrap();
    let recovered = decrypt_blob(&key_b, &ct_b).unwrap();
    let inner2 = SignalingMessage::from_json(std::str::from_utf8(&recovered).unwrap()).unwrap();
    assert_eq!(inner2.msg_type, MessageType::ChatMessage);
    assert_eq!(
        inner2.payload.get("body").unwrap().as_str().unwrap(),
        "secret hi"
    );
    assert_eq!(inner2.sender, alice.public_id());
    // Inner signature still verifies after the round-trip (defense in depth).
    assert!(ConnectionManager::verify_inbound_signature_for_test(
        &inner2
    ));

    // A third party who is not the paired peer cannot decrypt.
    let eve = Identity::generate();
    let eve_key = eve.derive_pairwise_relay_key(&alice.public_id()).unwrap();
    assert!(decrypt_blob(&eve_key, &ct_b).is_err());
}
