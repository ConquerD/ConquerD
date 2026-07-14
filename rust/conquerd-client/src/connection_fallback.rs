//! Connection fallback logic — pure-data, no I/O.
//!
//! ## Candidate ordering ([`build_ws_candidates`])
//!
//! Returns an ordered, de-duplicated list of WebSocket connect candidates:
//! 1. Connected / known supernode relay endpoints (fastest when both peers
//!    are behind NAT and a trusted supernode is already reachable).
//! 2. Direct invite/store endpoint.
//! 3. LAN hint advertised by the peer.
//! 4. Any further relay hints from the peer record.
//!
//! ## Direct-call fallback ([`DirectFallbackCoordinator`])
//!
//! When a direct QUIC call cannot be established the client falls back to a
//! temporary private SFU room on a trusted supernode. The connection manager
//! owns the I/O; this module holds the pure state machine and helpers.

use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Candidate ordering
// ---------------------------------------------------------------------------

/// Prefix used for temporary direct-fallback rooms.
pub const TEMP_ROOM_PREFIX: &str = "direct-";

/// Build an ordered, de-duplicated list of WebSocket connect candidates.
///
/// # Arguments
///
/// * `endpoint_url`                  – peer's primary direct endpoint.
/// * `lan_hint`                      – best-effort LAN address from the peer.
/// * `supernode_relay_hints`         – caller-supplied relay hints.
/// * `connected_supernode_endpoints` – WS URLs of currently-connected trusted supernodes.
/// * `peer_relay_hints`              – further `relay_hints` from the peer record.
pub fn build_ws_candidates<'a>(
    endpoint_url: Option<&'a str>,
    lan_hint: Option<&'a str>,
    supernode_relay_hints: impl IntoIterator<Item = &'a str>,
    connected_supernode_endpoints: impl IntoIterator<Item = &'a str>,
    peer_relay_hints: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates: Vec<String> = Vec::new();

    let mut push = |url: &str| {
        if !url.is_empty() && seen.insert(url.to_owned()) {
            candidates.push(url.to_owned());
        }
    };

    for h in connected_supernode_endpoints {
        push(h);
    }
    for h in supernode_relay_hints {
        push(h);
    }
    if let Some(ep) = endpoint_url {
        push(ep);
    }
    if let Some(lh) = lan_hint {
        push(lh);
    }
    for h in peer_relay_hints {
        push(h);
    }

    candidates
}

/// Convenience: ordered, de-duplicated list from a peer/supernode's stored
/// `relay_hints` (and optional primary endpoint).
pub fn build_ws_candidates_from_hints(
    endpoint_url: Option<&str>,
    relay_hints: &[String],
) -> Vec<String> {
    build_ws_candidates(
        endpoint_url,
        None,
        std::iter::empty(),
        std::iter::empty(),
        relay_hints.iter().map(String::as_str),
    )
}

// ---------------------------------------------------------------------------
// DirectFallbackCoordinator
// ---------------------------------------------------------------------------

/// State for a pending direct-call → private-room fallback.
#[derive(Debug, Clone)]
pub struct PendingFallback {
    /// The original target peer.
    pub peer_id: String,
    /// The temporary room id that was created.
    pub room_id: String,
    /// The supernode that is hosting the room.
    pub supernode_id: String,
}

/// State machine for direct-call → private-room fallback.
///
/// Holds at most one pending fallback at a time.
#[derive(Debug, Default)]
pub struct DirectFallbackCoordinator {
    pending: Option<PendingFallback>,
    /// Monotonic counter embedded in room ids so concurrent profiles don't
    /// collide when identity prefixes match.
    counter: u32,
}

impl DirectFallbackCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// The currently pending fallback, if any.
    pub fn pending(&self) -> Option<&PendingFallback> {
        self.pending.as_ref()
    }

    /// Take the next room-id counter value.
    pub fn next_counter(&mut self) -> u32 {
        self.counter = self.counter.wrapping_add(1);
        self.counter
    }

    /// Schedule a fallback for the given peer.
    pub fn set_pending(&mut self, pending: PendingFallback) {
        self.pending = Some(pending);
    }

    /// Drop any pending fallback (direct call recovered or session stopped).
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Returns `true` if a fallback is pending for `peer_id`.
    pub fn is_pending_for(&self, peer_id: &str) -> bool {
        self.pending
            .as_ref()
            .map(|p| p.peer_id == peer_id)
            .unwrap_or(false)
    }

    /// Returns `true` if the given room id is the currently pending fallback room.
    pub fn is_pending_room(&self, supernode_id: &str, room_id: &str) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.supernode_id == supernode_id && p.room_id == room_id)
    }

    /// Return `true` for room IDs minted by the fallback path.
    pub fn is_temp_direct_room(room_id: &str) -> bool {
        room_id.starts_with(TEMP_ROOM_PREFIX)
    }

    /// Build a deterministic temporary room id for a direct-call fallback.
    ///
    /// Embeds short prefixes of both identity public keys plus a counter so
    /// concurrent fallbacks across profiles don't collide.
    pub fn build_room_id(self_pub: &str, peer_id: &str, counter: u32) -> String {
        let sp = &self_pub[..12.min(self_pub.len())];
        let pp = &peer_id[..12.min(peer_id.len())];
        format!("{TEMP_ROOM_PREFIX}{sp}-{pp}-{counter}")
    }

    /// Pick a trusted supernode id for fallback.
    ///
    /// Preference order:
    /// 1. A trusted supernode that is currently connected via WebSocket.
    /// 2. Any trusted supernode in the provided list.
    /// 3. Empty string when none qualify.
    pub fn pick_supernode<'a>(
        supernode_ids: impl IntoIterator<Item = &'a str>,
        connected_ids: &HashSet<String>,
    ) -> String {
        let mut fallback: Option<String> = None;
        for id in supernode_ids {
            if connected_ids.contains(id) {
                return id.to_owned();
            }
            if fallback.is_none() {
                fallback = Some(id.to_owned());
            }
        }
        fallback.unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_order_supernode_first() {
        let candidates = build_ws_candidates(
            Some("ws://direct:8080"),
            Some("ws://192.168.1.2:8080"),
            std::iter::empty(),
            ["ws://supernode:443"],
            std::iter::empty(),
        );
        assert_eq!(candidates[0], "ws://supernode:443");
        assert_eq!(candidates[1], "ws://direct:8080");
        assert_eq!(candidates[2], "ws://192.168.1.2:8080");
    }

    #[test]
    fn candidate_order_all_tiers() {
        let candidates = build_ws_candidates(
            Some("ws://direct:4000"),
            Some("ws://lan:4000"),
            ["ws://relay-hint:4001"],
            ["ws://supernode:4002"],
            ["ws://peer-relay:4003"],
        );
        assert_eq!(
            candidates,
            vec![
                "ws://supernode:4002".to_owned(),  // connected supernodes first
                "ws://relay-hint:4001".to_owned(), // then supernode relay hints
                "ws://direct:4000".to_owned(),     // then direct endpoint
                "ws://lan:4000".to_owned(),        // then LAN hint
                "ws://peer-relay:4003".to_owned(), // peer relay hints last
            ]
        );
    }

    #[test]
    fn candidates_skip_empty() {
        let candidates =
            build_ws_candidates(Some(""), None, std::iter::empty(), [""], std::iter::empty());
        assert!(candidates.is_empty());
    }

    #[test]
    fn candidate_dedup() {
        let candidates = build_ws_candidates(
            Some("ws://node:9000"),
            None,
            ["ws://node:9000"],
            ["ws://node:9000"],
            std::iter::empty(),
        );
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn from_hints_preserves_order_and_dedups() {
        let hints = vec![
            "ws://a:1".to_owned(),
            "ws://b:2".to_owned(),
            "ws://a:1".to_owned(),
        ];
        let c = build_ws_candidates_from_hints(Some("ws://primary:9"), &hints);
        assert_eq!(
            c,
            vec![
                "ws://primary:9".to_owned(),
                "ws://a:1".to_owned(),
                "ws://b:2".to_owned(),
            ]
        );
    }

    #[test]
    fn build_room_id_format() {
        let id =
            DirectFallbackCoordinator::build_room_id("aabbccddeeff0011", "112233445566ffee", 7);
        assert!(id.starts_with("direct-"));
        assert!(DirectFallbackCoordinator::is_temp_direct_room(&id));
    }

    #[test]
    fn pick_supernode_prefers_connected() {
        let mut connected = HashSet::new();
        connected.insert("sn2".to_owned());
        let picked = DirectFallbackCoordinator::pick_supernode(["sn1", "sn2", "sn3"], &connected);
        assert_eq!(picked, "sn2");
    }

    #[test]
    fn pick_supernode_falls_back_to_first() {
        let connected = HashSet::new();
        let picked = DirectFallbackCoordinator::pick_supernode(["sn1", "sn2"], &connected);
        assert_eq!(picked, "sn1");
    }

    #[test]
    fn pending_room_match() {
        let mut c = DirectFallbackCoordinator::new();
        c.set_pending(PendingFallback {
            peer_id: "p".into(),
            room_id: "direct-x".into(),
            supernode_id: "sn".into(),
        });
        assert!(c.is_pending_room("sn", "direct-x"));
        assert!(!c.is_pending_room("sn", "other"));
        c.cancel();
        assert!(c.pending().is_none());
    }
}
