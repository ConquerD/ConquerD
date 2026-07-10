use super::internal::{host_from_url, is_loopback_or_wildcard, rewrite_loopback_wt_url};
use super::manager::{
    build_room_invite_url, is_elected_keyer, parse_quic_lan_hint, parse_room_invite,
    peer_quic_endpoint, plan_cluster_failover, FailoverPlan, RoomInvitePayload, ROOM_INVITE_SCHEMA,
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
