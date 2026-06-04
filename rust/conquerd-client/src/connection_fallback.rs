//! Connection fallback logic — pure-data, no I/O.
//!
//! ## Candidate ordering (`build_ws_candidates`)
//!
//! Returns an ordered, de-duplicated list of WebSocket connect candidates:
//! 1. Connected / known supernode relay endpoints (fastest when both peers
//!    are behind NAT and a trusted supernode is already reachable).
//! 2. Direct invite/store endpoint.
//! 3. LAN hint advertised by the peer.
//! 4. Any further relay hints from the peer record.
//!
//! ## Direct-call fallback (`DirectFallbackCoordinator`)
//!
//! When a direct QUIC call to a peer fails the client falls back to a
//! temporary private SFU room:
//! 1. Pick a trusted supernode (prefer already-connected).
//! 2. Build a deterministic `direct-…` room id.
//! 3. On `SFU_ROOM_CREATED` ack, send a peer-room invite and join.
//!
//! This module owns only the state machine and pure helpers.  All I/O lives
//! in the calling layer.

// ---------------------------------------------------------------------------
// Candidate ordering
// ---------------------------------------------------------------------------

/// Prefix used for temporary direct-fallback rooms.
pub const TEMP_ROOM_PREFIX: &str = "direct-";

/// Build an ordered, de-duplicated list of WebSocket connect candidates.
///
/// # Arguments
///
/// * `endpoint_url`                – peer's primary direct endpoint.
/// * `lan_hint`                    – best-effort LAN address from the peer.
/// * `supernode_relay_hints`       – caller-supplied relay hints.
/// * `connected_supernode_endpoints` – WS URLs of currently-connected trusted supernodes.
/// * `peer_relay_hints` – further `relay_hints` from the peer record.
pub fn build_ws_candidates<'a>(
    endpoint_url: Option<&'a str>,
    lan_hint: Option<&'a str>,
    supernode_relay_hints: impl IntoIterator<Item = &'a str>,
    connected_supernode_endpoints: impl IntoIterator<Item = &'a str>,
    peer_relay_hints: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    let mut supernode_candidates: Vec<String> = Vec::new();

    let extend = |items: &mut Vec<String>, src: &mut dyn Iterator<Item = &str>| {
        for hint in src {
            if !hint.is_empty() && !items.contains(&hint.to_owned()) {
                items.push(hint.to_owned());
            }
        }
    };

    extend(
        &mut supernode_candidates,
        &mut connected_supernode_endpoints.into_iter(),
    );
    extend(
        &mut supernode_candidates,
        &mut supernode_relay_hints.into_iter(),
    );

    // Supernode candidates go first
    for sc in &supernode_candidates {
        if !candidates.contains(sc) {
            candidates.push(sc.clone());
        }
    }

    if let Some(ep) = endpoint_url {
        if !ep.is_empty() && !candidates.contains(&ep.to_owned()) {
            candidates.push(ep.to_owned());
        }
    }
    if let Some(lh) = lan_hint {
        if !lh.is_empty() && !candidates.contains(&lh.to_owned()) {
            candidates.push(lh.to_owned());
        }
    }
    extend(&mut candidates, &mut peer_relay_hints.into_iter());

    candidates
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
}

impl DirectFallbackCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    /// The currently pending fallback, if any.
    pub fn pending(&self) -> Option<&PendingFallback> {
        self.pending.as_ref()
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

    // -- Pure helpers -------------------------------------------------------

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
    /// 1. A trusted (not blocked, not revoked) supernode that is currently
    ///    connected via WebSocket.
    /// 2. Any trusted supernode in the provided list.
    /// 3. Empty string when none qualify.
    ///
    /// `supernode_ids` is the full list of trusted supernode peer-ids.
    /// `connected_ids` is the subset that currently has an active WS session.
    pub fn pick_supernode<'a>(
        supernode_ids: impl IntoIterator<Item = &'a str>,
        connected_ids: &std::collections::HashSet<String>,
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
            ["ws://supernode:443"].into_iter(),
            std::iter::empty(),
        );
        assert_eq!(candidates[0], "ws://supernode:443");
        assert_eq!(candidates[1], "ws://direct:8080");
        assert_eq!(candidates[2], "ws://192.168.1.2:8080");
    }

    #[test]
    fn candidate_dedup() {
        let candidates = build_ws_candidates(
            Some("ws://node:9000"),
            None,
            ["ws://node:9000"].into_iter(),
            ["ws://node:9000"].into_iter(),
            std::iter::empty(),
        );
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn build_room_id_format() {
        let id =
            DirectFallbackCoordinator::build_room_id("aabbccddeeff0011", "112233445566ffee", 7);
        assert!(id.starts_with("direct-"));
        assert!(id.contains('-'));
    }

    #[test]
    fn pick_supernode_prefers_connected() {
        let mut connected = std::collections::HashSet::new();
        connected.insert("sn2".to_owned());
        let picked = DirectFallbackCoordinator::pick_supernode(
            ["sn1", "sn2", "sn3"].into_iter(),
            &connected,
        );
        assert_eq!(picked, "sn2");
    }

    #[test]
    fn pick_supernode_falls_back_to_first() {
        let connected = std::collections::HashSet::new();
        let picked =
            DirectFallbackCoordinator::pick_supernode(["sn1", "sn2"].into_iter(), &connected);
        assert_eq!(picked, "sn1");
    }
}
