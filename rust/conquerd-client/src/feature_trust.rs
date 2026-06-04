//! Feature-invoke trust gating for non-first-party namespaces.
//!
//! * First-party namespaces (`core.*`, `transport.*`, `room.*`, `web.*`,
//!   `game.*`) bypass the prompt and are always allowed.
//! * Non-first-party namespaces are checked against the persistent
//!   [`FeatureTrustStore`]; if no decision exists yet, the gate emits
//!   a [`TrustDecision::Pending`] so the UI layer can prompt the user.
//!
//! Persistence and Qt prompting are the responsibility of the UI layer;
//! this module only provides the in-memory state machine.

use std::collections::HashMap;

/// Reverse-DNS namespaces bundled and audited as part of the client release.
const FIRST_PARTY_NAMESPACES: &[&str] = &["core", "transport", "room", "web", "game"];

/// Returns `true` iff *feature_id* lives in a reserved first-party namespace.
pub fn is_first_party(feature_id: &str) -> bool {
    let ns = feature_id.split('.').next().unwrap_or("");
    FIRST_PARTY_NAMESPACES.contains(&ns)
}

/// In-memory cache of `(feature_id, peer_id) -> allow` decisions.
///
/// Persistence (encrypted on disk via `peer_store`) is the caller's
/// responsibility — `to_pairs` / `from_pairs` provide the round-trip.
#[derive(Debug, Default, Clone)]
pub struct FeatureTrustStore {
    decisions: HashMap<(String, String), bool>,
}

impl FeatureTrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up a stored decision for `(feature_id, peer_id)`.
    pub fn get(&self, feature_id: &str, peer_id: &str) -> Option<bool> {
        self.decisions
            .get(&(feature_id.to_owned(), peer_id.to_owned()))
            .copied()
    }

    /// Record a decision for `(feature_id, peer_id)`.
    pub fn set(&mut self, feature_id: &str, peer_id: &str, allow: bool) {
        self.decisions
            .insert((feature_id.to_owned(), peer_id.to_owned()), allow);
    }

    /// Forget a decision (the next invocation will re-prompt).
    pub fn forget(&mut self, feature_id: &str, peer_id: &str) {
        self.decisions
            .remove(&(feature_id.to_owned(), peer_id.to_owned()));
    }

    /// Number of cached decisions.
    pub fn len(&self) -> usize {
        self.decisions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.decisions.is_empty()
    }

    /// Serialise to a flat list of `(feature_id, peer_id, allow)` tuples
    /// for persistence.
    pub fn to_pairs(&self) -> Vec<(String, String, bool)> {
        self.decisions
            .iter()
            .map(|((f, p), &v)| (f.clone(), p.clone(), v))
            .collect()
    }

    /// Restore from a flat list produced by [`Self::to_pairs`].
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String, bool)>) -> Self {
        let mut s = Self::new();
        for (f, p, v) in pairs {
            s.decisions.insert((f, p), v);
        }
        s
    }
}

/// Outcome of [`FeatureTrustGate::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// Invoke is allowed (first-party namespace or stored `true`).
    Allow,
    /// Invoke is denied (stored `false`).
    Deny,
    /// No stored decision — UI must prompt the user, then call
    /// [`FeatureTrustStore::set`] and re-dispatch.
    Pending,
}

/// Stateless gate: combines [`is_first_party`] with the store lookup.
pub struct FeatureTrustGate;

impl FeatureTrustGate {
    /// Decide whether *feature_id* invoked by *peer_id* should be
    /// dispatched. The *store* is consulted only for non-first-party ids.
    pub fn check(feature_id: &str, peer_id: &str, store: &FeatureTrustStore) -> TrustDecision {
        if is_first_party(feature_id) {
            return TrustDecision::Allow;
        }
        match store.get(feature_id, peer_id) {
            Some(true) => TrustDecision::Allow,
            Some(false) => TrustDecision::Deny,
            None => TrustDecision::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_party_namespaces_recognised() {
        assert!(is_first_party("core.chat.v1"));
        assert!(is_first_party("transport.quic.audio.v1"));
        assert!(is_first_party("room.audio.sfu"));
        assert!(is_first_party("web.host.h3.v1"));
        assert!(is_first_party("game.demo.v1"));
        assert!(!is_first_party("x.vendor.thing"));
        assert!(!is_first_party("custom.thing"));
        assert!(!is_first_party(""));
    }

    #[test]
    fn first_party_always_allowed() {
        let store = FeatureTrustStore::new();
        assert_eq!(
            FeatureTrustGate::check("core.chat.v1", "peer-a", &store),
            TrustDecision::Allow
        );
    }

    #[test]
    fn bespoke_pending_then_allow() {
        let mut store = FeatureTrustStore::new();
        assert_eq!(
            FeatureTrustGate::check("x.vendor.thing", "peer-a", &store),
            TrustDecision::Pending
        );
        store.set("x.vendor.thing", "peer-a", true);
        assert_eq!(
            FeatureTrustGate::check("x.vendor.thing", "peer-a", &store),
            TrustDecision::Allow
        );
    }

    #[test]
    fn bespoke_deny_persists() {
        let mut store = FeatureTrustStore::new();
        store.set("x.vendor.thing", "peer-a", false);
        assert_eq!(
            FeatureTrustGate::check("x.vendor.thing", "peer-a", &store),
            TrustDecision::Deny
        );
    }

    #[test]
    fn pairs_round_trip() {
        let mut store = FeatureTrustStore::new();
        store.set("x.a.v1", "peer-1", true);
        store.set("x.b.v1", "peer-2", false);
        let pairs = store.to_pairs();
        let restored = FeatureTrustStore::from_pairs(pairs);
        assert_eq!(restored.get("x.a.v1", "peer-1"), Some(true));
        assert_eq!(restored.get("x.b.v1", "peer-2"), Some(false));
        assert_eq!(restored.len(), 2);
    }

    #[test]
    fn forget_removes_decision() {
        let mut store = FeatureTrustStore::new();
        store.set("x.a.v1", "peer-1", true);
        store.forget("x.a.v1", "peer-1");
        assert_eq!(store.get("x.a.v1", "peer-1"), None);
    }
}
