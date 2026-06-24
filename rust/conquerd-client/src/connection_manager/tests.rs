use super::internal::{host_from_url, is_loopback_or_wildcard, rewrite_loopback_wt_url};
use super::manager::{parse_quic_lan_hint, peer_quic_endpoint};
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
