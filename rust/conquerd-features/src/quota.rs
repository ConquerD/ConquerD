//! Per-feature, per-peer token-bucket quota enforcement.
//!
//! Each `(feature_id, peer_id)` pair gets an independent `QuotaState`
//! that tracks two independent token buckets:
//!
//! * **bytes** — refilled at `bytes_per_sec` bytes/sec; capped at a one-second
//!   burst (`bytes_per_sec`).
//! * **datagrams** — refilled at `datagrams_per_sec` units/sec; capped at
//!   a one-second burst (`datagrams_per_sec`).
//!
//! Rates are extracted from `descriptor.params["quota_bytes_per_sec"]` and
//! `descriptor.params["quota_datagrams_per_sec"]`. Unknown/missing values fall
//! back to [`DEFAULT_BYTES_PER_SEC`] and [`DEFAULT_DATAGRAMS_PER_SEC`].
//!
//! These defaults also apply to any feature in an unknown namespace (i.e.
//! not under `core.*`, `transport.*`, `room.*`, `web.*`, or `game.*`).
//!
//! ## Why the outbound rate can differ
//!
//! Inbound and outbound already use separate *buckets*; the optional
//! `quota_bytes_per_sec_outbound` / `quota_datagrams_per_sec_outbound` params
//! let them use separate *rates* as well. One number cannot serve both roles for
//! an SFU feature, because the two gates count different things:
//!
//! * **Inbound** is keyed on the *sender* and carries one peer's stream. Its job
//!   is to stop a single client flooding the relay, so it should sit just above
//!   what one legitimate client can emit.
//! * **Outbound** is keyed on the *recipient* and carries every other sender
//!   fanned to them at once. In an N-way room that is (N-1) streams through one
//!   bucket.
//!
//! Sizing the shared number for fan-out would hand every individual sender the
//! whole room's allowance inbound — losing the per-sender cap as a side effect
//! of making rooms work. Features that do not fan out simply omit the outbound
//! keys and both directions keep the same rate, exactly as before.

use std::collections::HashMap;
use std::time::Instant;

use parking_lot::Mutex;

/// Default inbound byte quota for features without explicit `quota_bytes_per_sec`
/// in their descriptor params (64 KiB/s).
pub const DEFAULT_BYTES_PER_SEC: f64 = 64.0 * 1024.0;
/// Default inbound datagram quota for features without explicit
/// `quota_datagrams_per_sec` in their descriptor params.
pub const DEFAULT_DATAGRAMS_PER_SEC: f64 = 256.0;

/// Rates extracted from a descriptor's `params` field.
#[derive(Debug, Clone, Copy)]
pub struct QuotaParams {
    pub bytes_per_sec: f64,
    pub datagrams_per_sec: f64,
}

impl Default for QuotaParams {
    fn default() -> Self {
        Self {
            bytes_per_sec: DEFAULT_BYTES_PER_SEC,
            datagrams_per_sec: DEFAULT_DATAGRAMS_PER_SEC,
        }
    }
}

impl QuotaParams {
    /// Extract quota rates from the `params` JSON object of a
    /// [`CapabilityDescriptor`].  Missing or non-numeric fields use the
    /// default values.
    ///
    /// A value of `0.0` is the **unbounded sentinel** — it disables that axis
    /// of the quota entirely.  Any other positive value is clamped to a minimum
    /// of `1.0` to avoid near-zero bucket sizes.
    pub fn from_params(params: &serde_json::Value) -> Self {
        Self::read(
            params,
            "quota_bytes_per_sec",
            "quota_datagrams_per_sec",
            DEFAULT_BYTES_PER_SEC,
            DEFAULT_DATAGRAMS_PER_SEC,
        )
    }

    /// Rates for the **outbound** direction.
    ///
    /// Reads `quota_bytes_per_sec_outbound` / `quota_datagrams_per_sec_outbound`
    /// and falls back to the inbound rate for each axis independently, so a
    /// descriptor that says nothing about outbound behaves exactly as it did
    /// before these keys existed. See the module docs for why a fan-out feature
    /// needs the two to differ.
    pub fn from_params_outbound(params: &serde_json::Value) -> Self {
        let inbound = Self::from_params(params);
        Self::read(
            params,
            "quota_bytes_per_sec_outbound",
            "quota_datagrams_per_sec_outbound",
            inbound.bytes_per_sec,
            inbound.datagrams_per_sec,
        )
    }

    fn read(
        params: &serde_json::Value,
        bytes_key: &str,
        datagrams_key: &str,
        default_bytes: f64,
        default_datagrams: f64,
    ) -> Self {
        let raw_bytes = params
            .get(bytes_key)
            .and_then(|v| v.as_f64())
            .unwrap_or(default_bytes);
        let raw_dg = params
            .get(datagrams_key)
            .and_then(|v| v.as_f64())
            .unwrap_or(default_datagrams);
        // 0.0 → unbounded; any other value → at least 1.0.
        let bytes_per_sec = if raw_bytes == 0.0 {
            0.0
        } else {
            raw_bytes.max(1.0)
        };
        let datagrams_per_sec = if raw_dg == 0.0 { 0.0 } else { raw_dg.max(1.0) };
        Self {
            bytes_per_sec,
            datagrams_per_sec,
        }
    }

    /// Returns `true` when both axes are the unbounded sentinel (0.0).
    pub fn is_unbounded(&self) -> bool {
        self.bytes_per_sec == 0.0 && self.datagrams_per_sec == 0.0
    }
}

/// Token-bucket state for a single `(feature, peer)` pair.
struct QuotaState {
    bytes_tokens: f64,
    datagram_tokens: f64,
    last_refill: Instant,
    params: QuotaParams,
}

impl QuotaState {
    fn new(params: QuotaParams) -> Self {
        // Start with a full bucket so the first burst is not penalised.
        Self {
            bytes_tokens: params.bytes_per_sec,
            datagram_tokens: params.datagrams_per_sec,
            last_refill: Instant::now(),
            params,
        }
    }

    /// Refill tokens proportional to elapsed wall time, then try to consume
    /// *byte_count* bytes **and** one datagram token.
    ///
    /// Returns `true` if both buckets have enough tokens; `false` (quota
    /// exceeded) otherwise.  The buckets are NOT consumed on `false`.
    /// When the quota is unbounded (both axes == 0.0) this always returns
    /// `true` without touching the buckets.
    fn try_consume(&mut self, byte_count: usize) -> bool {
        if self.params.is_unbounded() {
            return true;
        }
        self.refill();
        let bytes_ok = self.params.bytes_per_sec == 0.0 || self.bytes_tokens >= byte_count as f64;
        let dg_ok = self.params.datagrams_per_sec == 0.0 || self.datagram_tokens >= 1.0;
        if bytes_ok && dg_ok {
            if self.params.bytes_per_sec > 0.0 {
                self.bytes_tokens -= byte_count as f64;
            }
            if self.params.datagrams_per_sec > 0.0 {
                self.datagram_tokens -= 1.0;
            }
            true
        } else {
            false
        }
    }

    /// Refill tokens proportional to elapsed wall time, then try to consume
    /// one datagram token only (no byte accounting).
    ///
    /// Used by `dispatch_invoke_datagram` where we haven't yet seen the
    /// payload size.
    fn try_consume_datagram(&mut self) -> bool {
        if self.params.datagrams_per_sec == 0.0 {
            return true;
        }
        self.refill();
        if self.datagram_tokens >= 1.0 {
            self.datagram_tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = (now - self.last_refill).as_secs_f64();
        self.last_refill = now;

        if self.params.bytes_per_sec > 0.0 {
            self.bytes_tokens = (self.bytes_tokens + self.params.bytes_per_sec * elapsed)
                .min(self.params.bytes_per_sec);
        }
        if self.params.datagrams_per_sec > 0.0 {
            self.datagram_tokens = (self.datagram_tokens + self.params.datagrams_per_sec * elapsed)
                .min(self.params.datagrams_per_sec);
        }
    }
}

/// Registry of quota states, keyed by `(feature_id, peer_id)`.
///
/// A single `QuotaRegistry` is embedded in `FeatureRegistry`.  Thread-safety
/// is provided by an outer `Mutex` (parking_lot, so unwind-safe).
#[derive(Default)]
pub struct QuotaRegistry {
    states: Mutex<HashMap<(String, String), QuotaState>>,
}

impl QuotaRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to consume *byte_count* bytes + 1 datagram from the quota for
    /// (*feature_id*, *peer_id*).  Creates a new bucket if this is the first
    /// message from this peer for this feature.
    ///
    /// * `params` — quota rates to use if a new bucket is created.
    /// * Returns `true` (allowed) or `false` (quota exceeded).
    pub fn try_consume(
        &self,
        feature_id: &str,
        peer_id: &str,
        byte_count: usize,
        params: QuotaParams,
    ) -> bool {
        let mut g = self.states.lock();
        let key = (feature_id.to_owned(), peer_id.to_owned());
        let state = g.entry(key).or_insert_with(|| QuotaState::new(params));
        state.try_consume(byte_count)
    }

    /// Try to consume 1 datagram from the quota for (*feature_id*, *peer_id*).
    /// No byte accounting.
    pub fn try_consume_datagram(
        &self,
        feature_id: &str,
        peer_id: &str,
        params: QuotaParams,
    ) -> bool {
        let mut g = self.states.lock();
        let key = (feature_id.to_owned(), peer_id.to_owned());
        let state = g.entry(key).or_insert_with(|| QuotaState::new(params));
        state.try_consume_datagram()
    }

    /// Remove all quota state for *peer_id* (call on peer disconnect).
    pub fn clear_peer(&self, peer_id: &str) {
        let mut g = self.states.lock();
        g.retain(|(_, p), _| p != peer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(bps: f64, dps: f64) -> QuotaParams {
        QuotaParams {
            bytes_per_sec: bps,
            datagrams_per_sec: dps,
        }
    }

    #[test]
    fn first_message_within_quota_is_allowed() {
        let qr = QuotaRegistry::new();
        assert!(qr.try_consume("core.chat.v1", "peer-a", 100, params(1024.0, 10.0)));
    }

    #[test]
    fn quota_exceeded_when_bucket_empty() {
        let qr = QuotaRegistry::new();
        let p = params(200.0, 100.0);
        // Consume the entire byte bucket.
        assert!(qr.try_consume("core.chat.v1", "peer-a", 200, p));
        // Next message should be denied (bucket empty).
        assert!(!qr.try_consume("core.chat.v1", "peer-a", 1, p));
    }

    #[test]
    fn datagram_quota_is_independent_per_peer() {
        let qr = QuotaRegistry::new();
        let p = params(DEFAULT_BYTES_PER_SEC, 2.0);
        assert!(qr.try_consume_datagram("core.chat.v1", "peer-a", p));
        assert!(qr.try_consume_datagram("core.chat.v1", "peer-a", p));
        // Bucket is now empty for peer-a.
        assert!(!qr.try_consume_datagram("core.chat.v1", "peer-a", p));
        // peer-b is unaffected.
        assert!(qr.try_consume_datagram("core.chat.v1", "peer-b", p));
    }

    #[test]
    fn clear_peer_removes_state() {
        let qr = QuotaRegistry::new();
        let p = params(1.0, 1.0);
        assert!(qr.try_consume("x.test.v1", "peer-a", 1, p));
        assert!(!qr.try_consume("x.test.v1", "peer-a", 1, p));
        qr.clear_peer("peer-a");
        // After clear the bucket is fresh again.
        assert!(qr.try_consume("x.test.v1", "peer-a", 1, p));
    }

    #[test]
    fn from_params_uses_defaults_for_missing_fields() {
        let p = QuotaParams::from_params(&serde_json::Value::Null);
        assert_eq!(p.bytes_per_sec, DEFAULT_BYTES_PER_SEC);
        assert_eq!(p.datagrams_per_sec, DEFAULT_DATAGRAMS_PER_SEC);
    }

    #[test]
    fn from_params_reads_explicit_fields() {
        let v = serde_json::json!({
            "quota_bytes_per_sec": 8192.0,
            "quota_datagrams_per_sec": 50.0,
        });
        let p = QuotaParams::from_params(&v);
        assert_eq!(p.bytes_per_sec, 8192.0);
        assert_eq!(p.datagrams_per_sec, 50.0);
    }
}
