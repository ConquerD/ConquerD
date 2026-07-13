use super::internal::{host_from_url, is_loopback_or_wildcard};
use super::manager::{
    accept_group_key_epoch, build_room_invite_url, is_elected_keyer, may_send_room_e2e_content,
    normalize_room_type, parse_quic_lan_hint, parse_room_invite, peer_quic_endpoint,
    plan_cluster_failover, room_scope_key, should_auto_join_on_room_created,
    should_mint_first_room_key, should_track_pending_materialize, should_use_private_room_invite,
    union_members_for_room, FailoverPlan, RoomInvitePayload, ROOM_INVITE_SCHEMA,
};
use super::ConnectionManager;
use std::collections::{HashMap, HashSet};

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
        "",
        "",
        "",
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
            space_root: String::new(),
            space_proof: String::new(),
            space_grant: String::new(),
        }
    );
}

/// Space proof-based admission fields survive the invite round-trip as nested
/// JSON objects, so a joiner can forward them to the supernode for verification.
#[test]
fn room_invite_carries_space_fields() {
    let root = r#"{"schema":1,"space_id":"srv0","epoch":3,"root_hash":"ab","node_count":2,"issued_at":9,"signer":"OWNER","signature":"SIG"}"#;
    let proof = r#"{"schema":1,"node":{"node_id":"r","parent_id":"srv0","kind":"room","name":"R","node_type":"public","owner_pub":"OWNER","invite_policy":"","inherit":false,"key_commit":""},"leaf_index":0,"path":[],"epoch":3}"#;
    let grant = r#"{"schema":1,"node_id":"r","epoch":3,"grantee_pub":"BEE","expires_at":0,"signature":"GSIG"}"#;
    let url = build_room_invite_url(
        "sn",
        "wss://h:443",
        "r",
        "R",
        "public",
        "",
        0,
        root,
        proof,
        grant,
    );
    let encoded = url.strip_prefix("conquerd://room#").unwrap();
    let got = parse_room_invite(encoded).unwrap();
    // Re-parse the extracted JSON text and compare structurally (key order may
    // differ after the round-trip, but the fields — and thus signatures — match).
    let as_val = |s: &str| serde_json::from_str::<serde_json::Value>(s).unwrap();
    assert_eq!(as_val(&got.space_root), as_val(root));
    assert_eq!(as_val(&got.space_proof), as_val(proof));
    assert_eq!(as_val(&got.space_grant), as_val(grant));

    // An invite without space fields yields empty strings (not "null").
    let plain = build_room_invite_url(
        "sn",
        "wss://h:443",
        "r",
        "R",
        "public",
        "tok",
        0,
        "",
        "",
        "",
    );
    let plain_got = parse_room_invite(plain.strip_prefix("conquerd://room#").unwrap()).unwrap();
    assert!(plain_got.space_root.is_empty() && plain_got.space_proof.is_empty());
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
    let url = build_room_invite_url(
        "sn",
        "wss://h:443",
        "r",
        "n",
        "private",
        "tok",
        42,
        "",
        "",
        "",
    );
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

/// Regression: room group-key distribution must **sign** the inner
/// `SfuGroupKey` before sealing it in `EncryptedSignal`. The receiver unwraps
/// the envelope and re-dispatches the inner through the full inbound pipeline
/// (signature + freshness + replay). An unsigned inner is dropped as
/// "signature missing", so the peer never installs the epoch key, stays on the
/// deterministic fallback, and E2E room audio is silenced for both sides
/// (keyer seals under the real key; peer cannot open). Mirrors
/// `distribute_group_key` + the inbound `EncryptedSignal` → `SfuGroupKey` path.
#[test]
fn sfu_group_key_inner_must_be_signed_to_install() {
    use crate::crypto::{b64url_decode, b64url_encode, decrypt_blob, encrypt_blob};
    use crate::group_key::{open_voice_frame, seal_voice_frame, SenderKeysGroup};
    use crate::identity::Identity;
    use crate::protocol::{MessageType, SignalingMessage};
    use base64::Engine;
    use serde_json::Value;

    let alice = Identity::generate();
    let bob = Identity::generate();
    let room_id = "default";
    let epoch_key = [0x42u8; 32];

    // --- Bug path: unsigned inner is rejected by the inbound pipeline ---
    let mut unsigned = SignalingMessage::new(MessageType::SfuGroupKey, alice.public_id());
    unsigned
        .payload
        .insert("room_id".into(), Value::String(room_id.into()));
    unsigned
        .payload
        .insert("epoch".into(), Value::Number(0u64.into()));
    unsigned
        .payload
        .insert("key".into(), Value::String(b64url_encode(&epoch_key)));
    assert!(
        !ConnectionManager::verify_inbound_signature_for_test(&unsigned),
        "unsigned SfuGroupKey must fail verify_inbound_signature (the silent-room bug)"
    );

    // --- Fixed path: sign inner, seal, unwrap, verify, install, open audio ---
    let mut inner = unsigned.clone();
    let canonical = inner.canonical_bytes().unwrap();
    inner.signature =
        Some(base64::engine::general_purpose::URL_SAFE.encode(alice.sign(&canonical)));
    assert!(
        ConnectionManager::verify_inbound_signature_for_test(&inner),
        "signed SfuGroupKey must pass the inbound signature + freshness checks"
    );

    let key_a = alice.derive_pairwise_relay_key(&bob.public_id()).unwrap();
    let ct = encrypt_blob(&key_a, inner.to_json().unwrap().as_bytes()).unwrap();
    let mut env = SignalingMessage::new(MessageType::EncryptedSignal, alice.public_id());
    env.target = Some(bob.public_id());
    env.payload
        .insert("ciphertext".into(), Value::String(b64url_encode(&ct)));
    let env_canon = env.canonical_bytes().unwrap();
    env.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(alice.sign(&env_canon)));
    assert!(ConnectionManager::verify_inbound_signature_for_test(&env));

    // Bob unwraps the envelope (same steps as the EncryptedSignal arm).
    let key_b = bob.derive_pairwise_relay_key(&alice.public_id()).unwrap();
    assert_eq!(key_a, key_b);
    let ct_b = b64url_decode(env.payload.get("ciphertext").unwrap().as_str().unwrap()).unwrap();
    let recovered = decrypt_blob(&key_b, &ct_b).unwrap();
    let inner2 = SignalingMessage::from_json(std::str::from_utf8(&recovered).unwrap()).unwrap();
    assert_eq!(inner2.msg_type, MessageType::SfuGroupKey);
    assert!(
        ConnectionManager::verify_inbound_signature_for_test(&inner2),
        "unwrapped SfuGroupKey must still verify after EncryptedSignal round-trip"
    );

    // Install on both sides and prove voice E2E opens both ways.
    let mut alice_keys = SenderKeysGroup::new();
    let mut bob_keys = SenderKeysGroup::new();
    alice_keys.install(room_id, 0, epoch_key);
    let key_bytes = b64url_decode(inner2.payload.get("key").unwrap().as_str().unwrap()).unwrap();
    let mut installed = [0u8; 32];
    installed.copy_from_slice(&key_bytes);
    bob_keys.install(room_id, 0, installed);

    let opus = b"fake-opus-frame";
    let sealed = seal_voice_frame(&alice_keys, room_id, &alice.public_id(), 1, opus).unwrap();
    let opened = open_voice_frame(&bob_keys, room_id, &alice.public_id(), 1, &sealed).unwrap();
    assert_eq!(opened, opus);

    // Without install, Bob still on deterministic fallback cannot open Alice's real-key frame.
    let bob_fallback = SenderKeysGroup::new();
    assert!(
        open_voice_frame(&bob_fallback, room_id, &alice.public_id(), 1, &sealed).is_none(),
        "uninstalled peer must not open real-key E2E audio (would hear silence in production)"
    );
}

/// Elected-keyer gate rejects group keys from non-keyer room members.
#[test]
fn accept_group_key_requires_elected_keyer() {
    // is_elected_keyer is the sole membership check used by accept_group_key_from
    // for the "who may install" question — cover the predicate here; epoch
    // policy is unit-tested separately via `accept_group_key_epoch`.
    let members = vec!["alice".to_owned(), "bob".to_owned(), "carol".to_owned()];
    assert!(is_elected_keyer(&members, "alice"));
    assert!(!is_elected_keyer(&members, "bob"));
    assert!(!is_elected_keyer(&members, "carol"));
    // Hostile "bob" must not be able to claim keyer status.
    assert!(!is_elected_keyer(&members, "bob"));
}

/// Epoch policy for installing a sealed SfuGroupKey (security: no hostile jumps).
#[test]
fn accept_group_key_epoch_allows_bootstrap_and_adjacent_only() {
    // No real key yet → first install accepts any offered epoch.
    assert!(accept_group_key_epoch(false, 0, 0));
    assert!(accept_group_key_epoch(false, 0, 7));
    assert!(accept_group_key_epoch(false, 0, 255));

    // With real key at epoch 3: same epoch (reseal) and +1 (rotation) only.
    assert!(accept_group_key_epoch(true, 3, 3));
    assert!(accept_group_key_epoch(true, 3, 4));
    assert!(!accept_group_key_epoch(true, 3, 5));
    assert!(!accept_group_key_epoch(true, 3, 2));
    assert!(!accept_group_key_epoch(true, 3, 0));

    // u8 wrap: current 255, next rotation is 0.
    assert!(accept_group_key_epoch(true, 255, 255));
    assert!(accept_group_key_epoch(true, 255, 0));
    assert!(!accept_group_key_epoch(true, 255, 1));
}

/// Solo key defer closes the dual-keyer bootstrap race (architecture + opacity).
#[test]
fn should_mint_first_room_key_defers_when_solo_or_not_elected() {
    // Elected + no real key + another member present → mint.
    assert!(should_mint_first_room_key(true, false, 1));
    assert!(should_mint_first_room_key(true, false, 3));

    // Alone (union empty of others) → wait (fail-closed until peer arrives).
    assert!(!should_mint_first_room_key(true, false, 0));

    // Already have real key → not a "first mint".
    assert!(!should_mint_first_room_key(true, true, 1));

    // Non-elected never mints.
    assert!(!should_mint_first_room_key(false, false, 2));
    assert!(!should_mint_first_room_key(false, false, 0));
}

/// Outbound room audio/chat/file must not ship under the deterministic fallback.
#[test]
fn may_send_room_e2e_content_requires_real_key() {
    assert!(!may_send_room_e2e_content(false));
    assert!(may_send_room_e2e_content(true));
}

/// Wire reason strings the client rolls back on (`SfuJoinResult` / create deny)
/// stay stable — renames would break UX without a protocol bump.
#[test]
fn sfu_deny_and_join_result_wire_strings_are_stable() {
    use crate::protocol::MessageType;
    assert_eq!(MessageType::SfuJoinResult.as_wire_str(), "sfu_join_result");
    assert_eq!(
        MessageType::SfuRoomCreated.as_wire_str(),
        "sfu_room_created"
    );
    // Reason tokens (supernode → client) used by RoomJoinRejected / create deny.
    for reason in [
        "room_absent",
        "not_allowed",
        "room_full",
        "join_failed",
        "public_rooms_disabled",
        "private_rooms_disabled",
    ] {
        assert!(!reason.is_empty());
        assert!(reason.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
    }
}

/// Deterministic seal still works without real key — which is exactly why
/// `may_send_room_e2e_content` must gate outbound paths before seal.
#[test]
fn deterministic_seal_without_real_key_is_why_outbound_gate_exists() {
    use crate::group_key::{seal_voice_frame, SenderKeysGroup};
    let keys = SenderKeysGroup::new();
    assert!(!keys.has_real_key("room-x"));
    // Seal would succeed under the non-opaque deterministic key if we allowed it.
    let sealed = seal_voice_frame(&keys, "room-x", "sender", 1, b"opus");
    assert!(
        sealed.is_some(),
        "deterministic fallback still seals — outbound must check has_real_key first"
    );
    assert!(
        !may_send_room_e2e_content(keys.has_real_key("room-x")),
        "gate must block before that seal is ever transmitted"
    );
}

// ── Materialize / auto-join / private-invite policy ─────────────────────────
//
// Regression surface for: reconnect rematerialize must not auto-join voice;
// user create must auto-join; denied create must no-op; invite path only for
// non-creator private rooms with a token and not yet admitted.

#[test]
fn room_scope_key_is_stable_composite() {
    assert_eq!(room_scope_key("sn-A", "room-1"), "sn-A:room-1");
}

#[test]
fn normalize_room_type_private_or_public() {
    assert_eq!(normalize_room_type("private"), "private");
    assert_eq!(normalize_room_type("PRIVATE"), "private");
    assert_eq!(normalize_room_type(" private "), "private");
    assert_eq!(normalize_room_type("public"), "public");
    assert_eq!(normalize_room_type(""), "public");
    assert_eq!(normalize_room_type("garbage"), "public");
}

#[test]
fn pending_materialize_tracking_requires_id_and_flag() {
    assert!(should_track_pending_materialize(true, Some("abc")));
    assert!(!should_track_pending_materialize(true, Some("")));
    assert!(!should_track_pending_materialize(true, None));
    assert!(!should_track_pending_materialize(false, Some("abc")));
}

#[test]
fn auto_join_on_room_created_decision_table() {
    // User-initiated create → auto-join.
    assert!(should_auto_join_on_room_created(false, false, false));
    // Materialize-only reconnect → list only, no join.
    assert!(!should_auto_join_on_room_created(false, false, true));
    // Denied create → never join.
    assert!(!should_auto_join_on_room_created(true, false, false));
    assert!(!should_auto_join_on_room_created(true, false, true));
    // Empty room_id → never join.
    assert!(!should_auto_join_on_room_created(false, true, false));
    assert!(!should_auto_join_on_room_created(false, true, true));
}

#[test]
fn private_room_invite_path_decision_table() {
    // Non-creator private with token, not yet admitted → invite path.
    assert!(should_use_private_room_invite(false, true, false, true));
    // Already admitted this session → plain join (token spent).
    assert!(!should_use_private_room_invite(true, true, false, true));
    // Creator → plain join (self-admit).
    assert!(!should_use_private_room_invite(false, true, true, true));
    // Public room → plain join.
    assert!(!should_use_private_room_invite(false, false, false, true));
    // Private non-creator but no token → plain join (may be denied server-side).
    assert!(!should_use_private_room_invite(false, true, false, false));
}

/// Wire type + signed-ack envelope for group-key install confirmation.
/// Mirrors the SfuGroupKeyAck path: member signs an ack, seals it to the
/// keyer under the pairwise key (same as SfuGroupKey distribution).
#[test]
fn sfu_group_key_ack_round_trips_sealed() {
    use crate::crypto::{b64url_decode, b64url_encode, decrypt_blob, encrypt_blob};
    use crate::identity::Identity;
    use crate::protocol::{MessageType, SignalingMessage};
    use base64::Engine;
    use serde_json::Value;

    assert_eq!(
        MessageType::SfuGroupKeyAck.as_wire_str(),
        "sfu_group_key_ack"
    );

    let keyer = Identity::generate();
    let member = Identity::generate();

    let mut ack = SignalingMessage::new(MessageType::SfuGroupKeyAck, member.public_id());
    ack.payload
        .insert("room_id".into(), Value::String("default".into()));
    ack.payload
        .insert("epoch".into(), Value::Number(0u64.into()));
    let canon = ack.canonical_bytes().unwrap();
    ack.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(member.sign(&canon)));
    assert!(ConnectionManager::verify_inbound_signature_for_test(&ack));

    let key = member
        .derive_pairwise_relay_key(&keyer.public_id())
        .unwrap();
    let ct = encrypt_blob(&key, ack.to_json().unwrap().as_bytes()).unwrap();
    let mut env = SignalingMessage::new(MessageType::EncryptedSignal, member.public_id());
    env.target = Some(keyer.public_id());
    env.payload
        .insert("ciphertext".into(), Value::String(b64url_encode(&ct)));
    let env_canon = env.canonical_bytes().unwrap();
    env.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(member.sign(&env_canon)));

    let key_k = keyer
        .derive_pairwise_relay_key(&member.public_id())
        .unwrap();
    assert_eq!(key, key_k);
    let recovered = decrypt_blob(
        &key_k,
        &b64url_decode(env.payload.get("ciphertext").unwrap().as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let inner = SignalingMessage::from_json(std::str::from_utf8(&recovered).unwrap()).unwrap();
    assert_eq!(inner.msg_type, MessageType::SfuGroupKeyAck);
    assert_eq!(
        inner.payload.get("room_id").unwrap().as_str().unwrap(),
        "default"
    );
    assert_eq!(inner.payload.get("epoch").unwrap().as_u64().unwrap(), 0);
    assert!(ConnectionManager::verify_inbound_signature_for_test(&inner));
}

/// Room group-key "elected keyer" tie-break (`is_elected_keyer`), the fix for
/// the reliability gap in `backlog.md` "Crypto — group key reliability": any
/// member holding real key material can distribute it, chosen deterministically
/// (lexicographically smallest `public_id` present) so every member agrees on
/// a single actor without a fixed room "creator" — the property that lets the
/// built-in ownerless `default` room get keyed at all.
#[test]
fn elected_keyer_is_the_lexicographically_smallest_member() {
    let members = vec!["bob".to_owned(), "alice".to_owned(), "carol".to_owned()];
    assert!(is_elected_keyer(&members, "alice"));
    assert!(!is_elected_keyer(&members, "bob"));
    assert!(!is_elected_keyer(&members, "carol"));
}

#[test]
fn elected_keyer_requires_membership() {
    let members = vec!["bob".to_owned(), "carol".to_owned()];
    // Not present in the room at all → never the keyer, even if our id would
    // otherwise sort first.
    assert!(!is_elected_keyer(&members, "alice"));
}

#[test]
fn elected_keyer_is_unique_for_a_given_snapshot() {
    // Every member of the same snapshot must agree on exactly one keyer.
    let members = vec!["zeta".to_owned(), "mid".to_owned(), "aaa".to_owned()];
    let winners: Vec<&str> = members
        .iter()
        .filter(|m| is_elected_keyer(&members, m))
        .map(String::as_str)
        .collect();
    assert_eq!(winners, vec!["aaa"]);
}

#[test]
fn elected_keyer_recomputes_when_the_smallest_member_leaves() {
    // Simulates the keyer departing: the next-smallest remaining member takes
    // over automatically on the next membership snapshot (default-room
    // continuity + reconnect-after-drop, without a fixed owner).
    let before = vec!["aaa".to_owned(), "mid".to_owned(), "zeta".to_owned()];
    assert!(is_elected_keyer(&before, "aaa"));

    let after_aaa_left = vec!["mid".to_owned(), "zeta".to_owned()];
    assert!(!is_elected_keyer(&after_aaa_left, "aaa"));
    assert!(is_elected_keyer(&after_aaa_left, "mid"));
}

#[test]
fn single_member_room_is_its_own_keyer() {
    // The very first member of a room (e.g. the built-in `default` room,
    // which has no client-side creator) bootstraps its own real key.
    let members = vec!["solo".to_owned()];
    assert!(is_elected_keyer(&members, "solo"));
}

// ── Cluster-wide room membership union (`union_members_for_room`) ────────────
//
// The regression these guard against: with cluster multi-homing the same room
// has one membership snapshot per supernode. Diffing a single node's snapshot
// made a peer that simply hadn't joined on THIS node yet look like it had left,
// firing a spurious group-key rotation that stranded that peer on a stale epoch
// and silenced E2E audio. Keyer decisions must diff the union across nodes.

fn snap(pairs: &[(&str, &[&str])]) -> HashMap<String, HashSet<String>> {
    pairs
        .iter()
        .map(|(k, members)| {
            (
                (*k).to_owned(),
                members.iter().map(|m| (*m).to_owned()).collect(),
            )
        })
        .collect()
}

#[test]
fn room_union_merges_members_across_supernode_snapshots() {
    // peer2 is joined on A and C but not yet on B — the union still has it, so
    // no node's lagging snapshot can make it look departed.
    let snaps = snap(&[
        ("A:default", &["peer2"]),
        ("B:default", &[]),
        ("C:default", &["peer2", "peer3"]),
    ]);
    let mut got: Vec<String> = union_members_for_room(&snaps, "default")
        .into_iter()
        .collect();
    got.sort();
    assert_eq!(got, vec!["peer2".to_owned(), "peer3".to_owned()]);
}

#[test]
fn room_union_does_not_leak_other_rooms() {
    // A room-id suffix match must not pull members from a different room on the
    // same supernode.
    let snaps = snap(&[("A:room-aaa", &["peerX"]), ("A:room-bbb", &["peerY"])]);
    let got = union_members_for_room(&snaps, "room-aaa");
    assert_eq!(got, HashSet::from(["peerX".to_owned()]));
}

#[test]
fn room_union_is_empty_when_room_absent() {
    let snaps = snap(&[("A:default", &["peer2"])]);
    assert!(union_members_for_room(&snaps, "unheard-of").is_empty());
}

// ── Cluster failover selection (`plan_cluster_failover`) ────────────────────
//
// The regression these guard against: eager multi-homing opens a session to
// every sibling up front, so a failover selector that *excludes* siblings we
// already have a session with finds nothing and the room is silently dropped
// when its host dies. Failover must instead prefer exactly those live sessions.

fn targets(ids: &[&str]) -> Vec<(String, String)> {
    ids.iter()
        .map(|id| ((*id).to_owned(), format!("ws://{id}.example:34935")))
        .collect()
}

#[test]
fn failover_fans_out_to_every_live_sibling() {
    // Both siblings have live sessions (eager multi-homing). Because a denied
    // join is silent, we can't know which one still holds the room, so both are
    // attempted at once and none are left cold.
    let t = targets(&["B", "C"]);
    let plan = plan_cluster_failover(&t, |_| Some(true));
    assert_eq!(
        plan,
        FailoverPlan::Fanout {
            live: vec!["B".to_owned(), "C".to_owned()],
            cold: vec![],
        }
    );
}

#[test]
fn failover_regression_live_sibling_is_never_excluded() {
    // The original bug: the selector excluded siblings we already had a session
    // with, and eager multi-homing meant that was *all* of them — so failover
    // found nothing and the room was silently dropped. A live sibling must now
    // always appear in `live`, never be filtered away.
    let t = targets(&["B", "C"]);
    let plan = plan_cluster_failover(&t, |id| match id {
        "B" => Some(true), // already have a live session — must still be a target
        _ => None,
    });
    let FailoverPlan::Fanout { live, cold } = plan else {
        panic!("expected a fan-out plan, got {plan:?}");
    };
    assert_eq!(live, vec!["B".to_owned()]);
    assert_eq!(
        cold,
        vec![("C".to_owned(), "ws://C.example:34935".to_owned())]
    );
}

#[test]
fn failover_treats_a_down_session_as_cold() {
    // A sibling whose session exists but is disconnected can't accept a join
    // now, so it is armed as cold (a live one, C, is attempted immediately).
    let t = targets(&["B", "C"]);
    let plan = plan_cluster_failover(&t, |id| match id {
        "B" => Some(false), // session exists but down
        "C" => Some(true),
        _ => None,
    });
    assert_eq!(
        plan,
        FailoverPlan::Fanout {
            live: vec!["C".to_owned()],
            cold: vec![("B".to_owned(), "ws://B.example:34935".to_owned())],
        }
    );
}

#[test]
fn failover_all_cold_when_none_are_live() {
    // Whole cluster momentarily unreachable: no live attempt, every sibling is
    // armed cold so the first to reconnect resumes the room.
    let t = targets(&["B", "C"]);
    let plan = plan_cluster_failover(&t, |id| match id {
        "B" => Some(false), // session exists but down
        _ => None,          // never dialed
    });
    assert_eq!(
        plan,
        FailoverPlan::Fanout {
            live: vec![],
            cold: t,
        }
    );
}

#[test]
fn failover_without_a_roster_is_a_noop() {
    // A standalone (non-clustered) supernode has no siblings to move to.
    let plan = plan_cluster_failover(&[], |_| None);
    assert_eq!(plan, FailoverPlan::None);
}

#[test]
fn failover_preserves_roster_order_within_live_and_cold() {
    // Determinism: siblings keep roster order within each bucket, so all clients
    // sharing the roster attempt the same members in the same order.
    let t = targets(&["B", "C", "D"]);
    let plan = plan_cluster_failover(&t, |id| match id {
        "B" => Some(true),
        "D" => Some(true),
        _ => None, // C is cold
    });
    assert_eq!(
        plan,
        FailoverPlan::Fanout {
            live: vec!["B".to_owned(), "D".to_owned()],
            cold: vec![("C".to_owned(), "ws://C.example:34935".to_owned())],
        }
    );
}
