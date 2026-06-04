//! Relay access management and connection fallback logic.

use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tracing::info;

// ---------------------------------------------------------------------------
// Relay Access Manager
// ---------------------------------------------------------------------------

/// Events emitted by the relay access manager.
#[derive(Debug, Clone)]
pub enum RelayAccessEvent {
    /// A supernode requires portal interaction before granting relay access.
    PortalNeeded {
        supernode_id: String,
        portal_url: String,
        session_token: String,
        expires_at: f64,
    },
    /// The supernode has granted relay access.
    AccessGranted { supernode_id: String },
    /// The supernode has denied relay access.
    AccessDenied {
        supernode_id: String,
        reason: String,
    },
}

struct PendingPortal {
    url: String,
    token: String,
    expires_at: f64,
}

/// Client-side state tracker for supernode relay access control.
///
/// Replaces the old `PaymentManager` — no payment amounts, no cryptocurrency.
/// Tracks which supernodes require portal interaction and emits events for
/// the UI.
pub struct RelayAccessManager {
    pending: HashMap<String, PendingPortal>,
    event_tx: mpsc::Sender<RelayAccessEvent>,
}

impl RelayAccessManager {
    pub fn new(event_tx: mpsc::Sender<RelayAccessEvent>) -> Self {
        Self {
            pending: HashMap::new(),
            event_tx,
        }
    }

    /// A `RELAY_PAYMENT_REQUIRED` message was received.
    pub fn on_payment_required(
        &mut self,
        supernode_id: String,
        portal_url: String,
        session_token: String,
        expires_at: f64,
    ) {
        info!(
            "[access] Portal required for supernode {} — url: {}",
            &supernode_id[..supernode_id.len().min(12)],
            portal_url
        );
        self.pending.insert(
            supernode_id.clone(),
            PendingPortal {
                url: portal_url.clone(),
                token: session_token.clone(),
                expires_at,
            },
        );
        let _ = self.event_tx.try_send(RelayAccessEvent::PortalNeeded {
            supernode_id,
            portal_url,
            session_token,
            expires_at,
        });
    }

    /// A `RELAY_ACCESS_GRANTED` message was received.
    pub fn on_access_granted(&mut self, supernode_id: String) {
        self.pending.remove(&supernode_id);
        info!(
            "[access] Access granted by supernode {}",
            &supernode_id[..supernode_id.len().min(12)]
        );
        let _ = self
            .event_tx
            .try_send(RelayAccessEvent::AccessGranted { supernode_id });
    }

    /// A `RELAY_ACCESS_DENIED` message was received.
    pub fn on_access_denied(&mut self, supernode_id: String, reason: String) {
        self.pending.remove(&supernode_id);
        info!(
            "[access] Access denied by supernode {}: {}",
            &supernode_id[..supernode_id.len().min(12)],
            reason
        );
        let _ = self.event_tx.try_send(RelayAccessEvent::AccessDenied {
            supernode_id,
            reason,
        });
    }

    /// Returns `true` if the given supernode has a pending portal.
    pub fn portal_pending(&self, supernode_id: &str) -> bool {
        self.pending.contains_key(supernode_id)
    }

    /// Returns all supernode IDs with active portal requirements.
    pub fn pending_supernodes(&self) -> Vec<&str> {
        self.pending.keys().map(String::as_str).collect()
    }
}

// ---------------------------------------------------------------------------
// Connection fallback candidate ordering
// ---------------------------------------------------------------------------

/// Build an ordered, de-duplicated list of WebSocket connect candidates.
///
/// Strategy:
/// 1. Connected supernode relay endpoints first (fastest signaling path when
///    both peers are behind NAT and a trusted supernode is already connected).
/// 2. Direct invite/store endpoint next (best-case direct path).
/// 3. LAN hint, then any additional `relay_hints` from the peer record.
///
/// Falsy entries (empty strings) are skipped.
pub fn build_ws_candidates(
    endpoint_url: Option<&str>,
    lan_hint: Option<&str>,
    supernode_relay_hints: &[String],
    connected_supernode_endpoints: &[String],
    peer_relay_hints: &[String],
) -> Vec<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut candidates: Vec<String> = Vec::new();

    let mut push = |url: &str| {
        if !url.is_empty() && seen.insert(url.to_string()) {
            candidates.push(url.to_string());
        }
    };

    // 1. Connected supernode relay endpoints
    for h in connected_supernode_endpoints {
        push(h);
    }
    // Supernode relay hints from caller
    for h in supernode_relay_hints {
        push(h);
    }
    // 2. Direct endpoint
    if let Some(u) = endpoint_url {
        push(u);
    }
    // 3. LAN hint
    if let Some(l) = lan_hint {
        push(l);
    }
    // Additional peer relay hints
    for h in peer_relay_hints {
        push(h);
    }

    candidates
}

// ---------------------------------------------------------------------------
// Direct fallback helpers
// ---------------------------------------------------------------------------

/// Candidate priority for a direct (non-relay) connection attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectCandidate {
    /// Stored direct endpoint URL.
    Endpoint(String),
    /// LAN address hint.
    Lan(String),
    /// ICE/STUN derived candidate.
    Ice(String),
}

impl DirectCandidate {
    pub fn url(&self) -> &str {
        match self {
            Self::Endpoint(u) | Self::Lan(u) | Self::Ice(u) => u,
        }
    }
}

/// Build an ordered list of direct connection candidates for a peer.
pub fn build_direct_candidates(
    endpoint_url: Option<&str>,
    lan_hint: Option<&str>,
    ice_candidates: &[String],
) -> Vec<DirectCandidate> {
    let mut candidates = Vec::new();
    if let Some(u) = endpoint_url {
        if !u.is_empty() {
            candidates.push(DirectCandidate::Endpoint(u.to_string()));
        }
    }
    if let Some(l) = lan_hint {
        if !l.is_empty() {
            candidates.push(DirectCandidate::Lan(l.to_string()));
        }
    }
    for ice in ice_candidates {
        if !ice.is_empty() {
            candidates.push(DirectCandidate::Ice(ice.clone()));
        }
    }
    candidates
}

// ---------------------------------------------------------------------------
// Relay ticket auto-renew tracker
// ---------------------------------------------------------------------------

/// Relay ticket lifetime constants.
pub const RELAY_TICKET_TTL_SECS: f64 = 3_600.0; // 1 hour
pub const RELAY_TICKET_RENEW_SECS: f64 = 600.0; // renew within 10 min of expiry

/// A relay access ticket for one supernode.
#[derive(Debug, Clone)]
pub struct RelayTicket {
    pub supernode_id: String,
    /// Opaque token returned by the supernode.
    pub token: String,
    /// Unix timestamp (seconds) when the ticket expires.
    pub expires_at: f64,
}

impl RelayTicket {
    /// Create a ticket that expires `TTL` seconds from now.
    pub fn new(supernode_id: String, token: String) -> Self {
        let now = unix_now();
        Self {
            supernode_id,
            token,
            expires_at: now + RELAY_TICKET_TTL_SECS,
        }
    }

    /// Create a ticket with an explicit expiry timestamp.
    pub fn with_expiry(supernode_id: String, token: String, expires_at: f64) -> Self {
        Self {
            supernode_id,
            token,
            expires_at,
        }
    }

    /// Returns `true` if the ticket has expired.
    pub fn is_expired(&self) -> bool {
        unix_now() >= self.expires_at
    }

    /// Returns `true` if the ticket needs renewal (within the renewal window).
    pub fn needs_renew(&self) -> bool {
        unix_now() >= self.expires_at - RELAY_TICKET_RENEW_SECS
    }

    /// Seconds remaining until expiry (0 if already expired).
    pub fn remaining_secs(&self) -> f64 {
        (self.expires_at - unix_now()).max(0.0)
    }
}

/// Tracks relay tickets for all connected supernodes and signals when renewal
/// is due.
///
/// Call `poll_renewals()` from `ConnectionManager::run_inner`'s periodic tick.
/// It returns the IDs of supernodes whose tickets need renewal; the caller
/// sends a `RELAY_TICKET_RENEW` message to each.
#[derive(Debug, Default)]
pub struct RelayTicketTracker {
    tickets: HashMap<String, RelayTicket>,
}

impl RelayTicketTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store (or replace) a ticket for the given supernode.
    pub fn upsert(&mut self, ticket: RelayTicket) {
        info!(
            "[ticket] Stored relay ticket for {} (expires in {:.0}s)",
            &ticket.supernode_id[..ticket.supernode_id.len().min(12)],
            ticket.remaining_secs(),
        );
        self.tickets.insert(ticket.supernode_id.clone(), ticket);
    }

    /// Remove the ticket for the given supernode (e.g. on disconnect).
    pub fn remove(&mut self, supernode_id: &str) {
        self.tickets.remove(supernode_id);
    }

    /// Returns supernode IDs whose tickets need renewal right now.
    ///
    /// Should be called once per connection-manager tick (e.g. every 30 s).
    pub fn poll_renewals(&mut self) -> Vec<String> {
        let due: Vec<String> = self
            .tickets
            .iter()
            .filter(|(_, t)| t.needs_renew())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &due {
            info!(
                "[ticket] Renewal due for supernode {}",
                &id[..id.len().min(12)]
            );
        }
        due
    }

    /// Returns `true` if a valid (non-expired) ticket exists for the supernode.
    pub fn has_valid(&self, supernode_id: &str) -> bool {
        self.tickets
            .get(supernode_id)
            .map(|t| !t.is_expired())
            .unwrap_or(false)
    }

    pub fn get(&self, supernode_id: &str) -> Option<&RelayTicket> {
        self.tickets.get(supernode_id)
    }
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_candidates_ordering() {
        let candidates = build_ws_candidates(
            Some("ws://direct:4000"),
            Some("ws://lan:4000"),
            &["ws://relay-hint:4001".to_string()],
            &["ws://supernode:4002".to_string()],
            &["ws://peer-relay:4003".to_string()],
        );
        // Supernode endpoints come first
        assert_eq!(candidates[0], "ws://supernode:4002");
        // Then supernode relay hints
        assert_eq!(candidates[1], "ws://relay-hint:4001");
        // Then direct endpoint
        assert_eq!(candidates[2], "ws://direct:4000");
        // LAN
        assert_eq!(candidates[3], "ws://lan:4000");
        // Peer relay hints last
        assert_eq!(candidates[4], "ws://peer-relay:4003");
    }

    #[test]
    fn ws_candidates_dedup() {
        let url = "ws://same:4000".to_string();
        let candidates = build_ws_candidates(
            Some("ws://same:4000"),
            None,
            &[],
            &[url.clone()],
            &[url.clone()],
        );
        // Should only appear once
        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn ws_candidates_skips_empty() {
        let candidates = build_ws_candidates(Some(""), None, &[], &[], &[]);
        assert!(candidates.is_empty());
    }

    #[test]
    fn direct_candidates_ordering() {
        let cands = build_direct_candidates(
            Some("ws://ep:4000"),
            Some("ws://lan:4001"),
            &["ws://ice:4002".to_string()],
        );
        assert!(matches!(cands[0], DirectCandidate::Endpoint(_)));
        assert!(matches!(cands[1], DirectCandidate::Lan(_)));
        assert!(matches!(cands[2], DirectCandidate::Ice(_)));
    }

    #[tokio::test]
    async fn relay_access_manager_events() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut mgr = RelayAccessManager::new(tx);

        mgr.on_payment_required(
            "sn-001".to_string(),
            "https://node/portal".to_string(),
            "tok".to_string(),
            9999.0,
        );
        assert!(mgr.portal_pending("sn-001"));
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, RelayAccessEvent::PortalNeeded { .. }));

        mgr.on_access_granted("sn-001".to_string());
        assert!(!mgr.portal_pending("sn-001"));
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, RelayAccessEvent::AccessGranted { .. }));
    }
}
