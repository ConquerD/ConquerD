//! Sliding-window replay protection for signed signaling messages.
//!
//! The signaling layer already rejects any message whose `timestamp` is outside
//! a freshness window (see the client `verify_inbound_signature` and the
//! supernode WS handler, both using a 5-minute window). The freshness window
//! alone, however, still lets an attacker **re-deliver** a captured, validly
//! signed message any number of times *within* that window.
//!
//! [`ReplayGuard`] closes that gap. It keeps, per sender, the set of message
//! signatures it has already accepted, each tagged with the wall-clock instant
//! it was first seen. Because every distinct message carries a distinct
//! Ed25519 signature, and an attacker cannot forge signatures, a replayed
//! message is exactly a message whose signature was already recorded:
//!
//! * A replay **inside** the freshness window → caught here (signature already
//!   present in the per-sender set).
//! * A replay **outside** the freshness window → caught by the timestamp
//!   freshness check upstream.
//!
//! Entries are pruned once they age past the configured window, so memory stays
//! bounded by `peer_count × message_rate × window`. A hard per-sender cap
//! provides a final backstop against memory exhaustion from a misbehaving peer.
//!
//! The guard uses interior mutability ([`Mutex`]) so it can be shared across
//! tasks/connections behind an [`std::sync::Arc`], mirroring [`QuotaRegistry`].
//!
//! [`QuotaRegistry`]: crate::quota::QuotaRegistry

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Default replay window, matching the 5-minute signaling freshness window.
pub const DEFAULT_REPLAY_WINDOW_SECS: f64 = 300.0;

/// Hard cap on remembered signatures per sender. Sized well above any plausible
/// `rate × window` so legitimate traffic is never evicted, while still bounding
/// memory for a single misbehaving peer. When exceeded after pruning, the guard
/// fails closed (rejects the new message) rather than evicting still-valid
/// entries, since evicting would reopen a replay window.
const MAX_ENTRIES_PER_SENDER: usize = 16_384;

/// Compact key for a recorded signature.
///
/// Ed25519 signatures are 64 effectively-random bytes. Keying on the first 16
/// bytes (128 bits) makes accidental collisions astronomically unlikely, and a
/// *deliberate* collision is impossible without forging a valid signature
/// (which requires the sender's private key). Truncating keeps the per-entry
/// memory small.
type SigKey = [u8; 16];

fn sig_key(signature: &[u8]) -> SigKey {
    let mut k = [0u8; 16];
    let n = signature.len().min(16);
    k[..n].copy_from_slice(&signature[..n]);
    k
}

/// Per-sender record of recently accepted signatures.
struct SenderWindow {
    /// signature key → instant first seen.
    seen: HashMap<SigKey, Instant>,
}

impl SenderWindow {
    fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// Drop entries older than `window` relative to `now`.
    fn prune(&mut self, now: Instant, window: Duration) {
        self.seen
            .retain(|_, &mut first_seen| now.duration_since(first_seen) <= window);
    }
}

/// Sliding-window replay guard shared across all inbound signaling paths.
pub struct ReplayGuard {
    windows: Mutex<HashMap<String, SenderWindow>>,
    window: Duration,
}

impl ReplayGuard {
    /// Create a guard with the given window in seconds.
    pub fn new(window_secs: f64) -> Self {
        let secs = if window_secs.is_finite() && window_secs > 0.0 {
            window_secs
        } else {
            DEFAULT_REPLAY_WINDOW_SECS
        };
        Self {
            windows: Mutex::new(HashMap::new()),
            window: Duration::from_secs_f64(secs),
        }
    }

    /// Record an accepted message and report whether it is a replay.
    ///
    /// Returns `true` if the `(sender, signature)` pair is **new** (the message
    /// should be processed) and `false` if it is a **replay** of a signature
    /// already seen within the window (the message must be dropped).
    ///
    /// Call this only *after* the signature and freshness checks have passed —
    /// an empty signature is always treated as a replay (rejected), since an
    /// unsigned message has no business reaching this point.
    pub fn check_and_record(&self, sender: &str, signature: &[u8]) -> bool {
        self.check_and_record_at(sender, signature, Instant::now())
    }

    /// Testable variant of [`check_and_record`](Self::check_and_record) with an
    /// explicit clock.
    pub fn check_and_record_at(&self, sender: &str, signature: &[u8], now: Instant) -> bool {
        if signature.is_empty() {
            return false;
        }
        let key = sig_key(signature);
        let mut windows = self.windows.lock();
        let win = windows
            .entry(sender.to_owned())
            .or_insert_with(SenderWindow::new);
        win.prune(now, self.window);

        if win.seen.contains_key(&key) {
            // Already accepted this exact signature within the window → replay.
            return false;
        }
        if win.seen.len() >= MAX_ENTRIES_PER_SENDER {
            // Fail closed: refuse to grow unbounded. Evicting live entries would
            // reopen a replay window, so reject instead.
            return false;
        }
        win.seen.insert(key, now);
        true
    }

    /// Forget all state for a sender (call on disconnect to reclaim memory).
    pub fn forget_peer(&self, sender: &str) {
        self.windows.lock().remove(sender);
    }

    /// Number of senders currently tracked (for tests / diagnostics).
    pub fn tracked_senders(&self) -> usize {
        self.windows.lock().len()
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new(DEFAULT_REPLAY_WINDOW_SECS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(n: u8) -> Vec<u8> {
        // 64-byte signature, distinguished by its first byte.
        let mut v = vec![0u8; 64];
        v[0] = n;
        v
    }

    #[test]
    fn first_sight_accepted_replay_rejected() {
        let g = ReplayGuard::new(300.0);
        let s = sig(1);
        assert!(g.check_and_record("alice", &s), "first delivery accepted");
        assert!(
            !g.check_and_record("alice", &s),
            "identical signature is a replay"
        );
        assert!(
            !g.check_and_record("alice", &s),
            "still rejected on further replays"
        );
    }

    #[test]
    fn distinct_signatures_all_accepted() {
        let g = ReplayGuard::new(300.0);
        for n in 1..=50u8 {
            assert!(
                g.check_and_record("alice", &sig(n)),
                "distinct signature {n} accepted"
            );
        }
    }

    #[test]
    fn per_sender_isolation() {
        let g = ReplayGuard::new(300.0);
        let s = sig(7);
        assert!(g.check_and_record("alice", &s));
        // Same signature bytes but a different sender is independent state.
        assert!(
            g.check_and_record("bob", &s),
            "different sender has its own window"
        );
        assert!(
            !g.check_and_record("alice", &s),
            "alice replay still caught"
        );
    }

    #[test]
    fn entry_expires_after_window() {
        let g = ReplayGuard::new(10.0);
        let t0 = Instant::now();
        let s = sig(3);
        assert!(g.check_and_record_at("alice", &s, t0));
        // Just inside the window → still a replay.
        let t_inside = t0 + Duration::from_secs(9);
        assert!(!g.check_and_record_at("alice", &s, t_inside));
        // Past the window → the old entry is pruned, so a fresh-timestamp
        // message with the same signature key is accepted again. (In practice
        // the upstream freshness check rejects such an old timestamp; this only
        // verifies the guard's own pruning frees memory.)
        let t_after = t0 + Duration::from_secs(11);
        assert!(g.check_and_record_at("alice", &s, t_after));
    }

    #[test]
    fn empty_signature_rejected() {
        let g = ReplayGuard::new(300.0);
        assert!(
            !g.check_and_record("alice", &[]),
            "empty signature rejected"
        );
    }

    #[test]
    fn forget_peer_clears_state() {
        let g = ReplayGuard::new(300.0);
        let s = sig(9);
        assert!(g.check_and_record("alice", &s));
        assert_eq!(g.tracked_senders(), 1);
        g.forget_peer("alice");
        assert_eq!(g.tracked_senders(), 0);
        // After forgetting, the same signature is treated as new again.
        assert!(g.check_and_record("alice", &s));
    }

    #[test]
    fn truncated_key_distinguishes_realistic_signatures() {
        // Two signatures that differ within the first 16 bytes are distinct.
        let g = ReplayGuard::new(300.0);
        let mut a = vec![0u8; 64];
        let mut b = vec![0u8; 64];
        a[15] = 1;
        b[15] = 2;
        assert!(g.check_and_record("p", &a));
        assert!(g.check_and_record("p", &b), "differ in byte 15 → distinct");
    }

    #[test]
    fn pruning_keeps_recent_entries() {
        let g = ReplayGuard::new(100.0);
        let t0 = Instant::now();
        // Record at staggered times.
        assert!(g.check_and_record_at("a", &sig(1), t0));
        assert!(g.check_and_record_at("a", &sig(2), t0 + Duration::from_secs(50)));
        // At t0+90, sig(1) (age 90) is still inside the 100s window → replay.
        assert!(!g.check_and_record_at("a", &sig(1), t0 + Duration::from_secs(90)));
        // At t0+160, sig(1) (age 160) is pruned but sig(2) (age 110) also pruned.
        assert!(g.check_and_record_at("a", &sig(1), t0 + Duration::from_secs(160)));
    }
}
