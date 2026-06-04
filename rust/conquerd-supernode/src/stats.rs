// ConquerD Supernode — stats.rs
// Stats collection aggregating relay, SFU, and connection data.

use std::time::Instant;

use serde_json::json;

use crate::relay::QUICRelayServer;
use crate::sfu::SFURoomManager;

/// Collect a stats snapshot.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_stats(
    version: &str,
    start_time: Instant,
    access_mode: &str,
    features: &[&str],
    trusted_peers_total: usize,
    connected_peers: usize,
    relay: Option<&QUICRelayServer>,
    sfu: Option<&parking_lot::RwLock<SFURoomManager>>,
) -> serde_json::Value {
    let uptime = start_time.elapsed().as_secs();

    let relay_stats = relay.map(|r| {
        let s = r.stats();
        json!({
            "peers_connected": s.peers_connected,
            "bytes_relayed_total": s.bytes_relayed_total,
            "active_rooms": s.active_rooms,
            "active_tickets": s.active_tickets,
            "rooms": s.rooms,
        })
    });

    let sfu_stats = sfu.map(|s| {
        let s = s.read();
        let stats = s.stats();
        json!({
            "rooms_total": stats.rooms_total,
            "participants_total": stats.participants_total,
            "rooms": stats.rooms,
        })
    });

    json!({
        "version": version,
        "uptime_seconds": uptime,
        "access_mode": access_mode,
        "features": features,
        "trusted_peers_total": trusted_peers_total,
        "connected_peers": connected_peers,
        "relay": relay_stats.unwrap_or(json!(null)),
        "sfu": sfu_stats.unwrap_or(json!(null)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_stats_no_relay_no_sfu() {
        let start = Instant::now();
        let v = collect_stats("1.0.0", start, "open", &["chat", "files"], 3, 2, None, None);

        assert_eq!(v["version"], "1.0.0");
        assert_eq!(v["access_mode"], "open");
        assert_eq!(v["trusted_peers_total"], 3);
        assert_eq!(v["connected_peers"], 2);
        assert!(v["relay"].is_null());
        assert!(v["sfu"].is_null());

        let features = v["features"].as_array().unwrap();
        assert_eq!(features.len(), 2);
        assert_eq!(features[0], "chat");
        assert_eq!(features[1], "files");
    }

    #[test]
    fn collect_stats_uptime_is_non_negative() {
        let start = Instant::now();
        let v = collect_stats("0.9", start, "tos", &[], 0, 0, None, None);
        assert!(
            v["uptime_seconds"].as_u64().unwrap_or(u64::MAX) < 60,
            "uptime should be near-zero in a test"
        );
    }

    #[test]
    fn collect_stats_empty_features() {
        let start = Instant::now();
        let v = collect_stats("2.0", start, "ad", &[], 0, 0, None, None);
        assert_eq!(v["features"].as_array().unwrap().len(), 0);
    }
}
