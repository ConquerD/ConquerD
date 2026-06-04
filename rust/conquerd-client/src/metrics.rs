//! Connection metrics — per-peer call quality tracking.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateType {
    Host,
    Srflx,
    Prflx,
    Relay,
    Unknown,
}

impl CandidateType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Srflx => "srflx",
            Self::Prflx => "prflx",
            Self::Relay => "relay",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionQuality {
    Excellent,
    Good,
    Fair,
    Poor,
    Bad,
    Unknown,
}

impl ConnectionQuality {
    /// Map RTT + loss to a quality tier.
    pub fn from_stats(rtt_ms: f64, loss_pct: f64) -> Self {
        if loss_pct >= 10.0 || rtt_ms >= 400.0 {
            Self::Bad
        } else if loss_pct >= 3.0 || rtt_ms >= 200.0 {
            Self::Poor
        } else if loss_pct >= 1.0 || rtt_ms >= 100.0 {
            Self::Fair
        } else if rtt_ms >= 50.0 {
            Self::Good
        } else {
            Self::Excellent
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Excellent => "excellent",
            Self::Good => "good",
            Self::Fair => "fair",
            Self::Poor => "poor",
            Self::Bad => "bad",
            Self::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// PeerMetrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerMetrics {
    pub peer_id: String,

    // Call setup
    pub setup_start: f64,
    pub setup_connected: f64,
    pub setup_duration_ms: f64,

    // ICE
    pub local_candidate_type: CandidateType,
    pub remote_candidate_type: CandidateType,
    pub local_candidates_gathered: u32,
    pub relay_used: bool,

    // Ongoing quality
    pub rtt_ms: f64,
    pub jitter_ms: f64,
    pub packet_loss_pct: f64,
    pub packets_sent: u64,
    pub packets_recv: u64,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
    pub audio_energy_local: f64,
    pub audio_energy_remote: f64,

    pub quality: ConnectionQuality,
}

impl PeerMetrics {
    pub fn new(peer_id: impl Into<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Self {
            peer_id: peer_id.into(),
            setup_start: now,
            setup_connected: 0.0,
            setup_duration_ms: 0.0,
            local_candidate_type: CandidateType::Unknown,
            remote_candidate_type: CandidateType::Unknown,
            local_candidates_gathered: 0,
            relay_used: false,
            rtt_ms: 0.0,
            jitter_ms: 0.0,
            packet_loss_pct: 0.0,
            packets_sent: 0,
            packets_recv: 0,
            bytes_sent: 0,
            bytes_recv: 0,
            audio_energy_local: 0.0,
            audio_energy_remote: 0.0,
            quality: ConnectionQuality::Unknown,
        }
    }

    /// Record ICE connection established.
    pub fn mark_connected(&mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        self.setup_connected = now;
        if self.setup_start > 0.0 {
            self.setup_duration_ms = (now - self.setup_start) * 1000.0;
        }
    }

    /// Update rolling quality stats and recompute the quality tier.
    pub fn update_stats(&mut self, rtt_ms: f64, jitter_ms: f64, loss_pct: f64) {
        self.rtt_ms = rtt_ms;
        self.jitter_ms = jitter_ms;
        self.packet_loss_pct = loss_pct;
        self.quality = ConnectionQuality::from_stats(rtt_ms, loss_pct);
    }

    /// Estimated MOS-like score 1.0–5.0 (higher is better).
    pub fn mos_score(&self) -> f64 {
        match self.quality {
            ConnectionQuality::Excellent => 4.5,
            ConnectionQuality::Good => 4.0,
            ConnectionQuality::Fair => 3.5,
            ConnectionQuality::Poor => 2.5,
            ConnectionQuality::Bad => 1.5,
            ConnectionQuality::Unknown => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// MetricsCollector
// ---------------------------------------------------------------------------

/// Aggregates metrics for all active peer connections in a call.
#[derive(Default)]
pub struct MetricsCollector {
    peers: HashMap<String, PeerMetrics>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn init_peer(&mut self, peer_id: impl Into<String>) -> &mut PeerMetrics {
        let id: String = peer_id.into();
        self.peers
            .entry(id.clone())
            .or_insert_with(|| PeerMetrics::new(id))
    }

    pub fn get(&self, peer_id: &str) -> Option<&PeerMetrics> {
        self.peers.get(peer_id)
    }

    pub fn get_mut(&mut self, peer_id: &str) -> Option<&mut PeerMetrics> {
        self.peers.get_mut(peer_id)
    }

    pub fn remove(&mut self, peer_id: &str) {
        self.peers.remove(peer_id);
    }

    pub fn clear(&mut self) {
        self.peers.clear();
    }

    /// Average RTT across all peers with known RTT.
    pub fn avg_rtt_ms(&self) -> f64 {
        let peers: Vec<f64> = self
            .peers
            .values()
            .filter(|m| m.rtt_ms > 0.0)
            .map(|m| m.rtt_ms)
            .collect();
        if peers.is_empty() {
            0.0
        } else {
            peers.iter().sum::<f64>() / peers.len() as f64
        }
    }

    /// Worst quality tier across all active peers.
    pub fn worst_quality(&self) -> ConnectionQuality {
        let mut worst = ConnectionQuality::Excellent;
        for m in self.peers.values() {
            let order = |q: &ConnectionQuality| match q {
                ConnectionQuality::Unknown => 0,
                ConnectionQuality::Excellent => 1,
                ConnectionQuality::Good => 2,
                ConnectionQuality::Fair => 3,
                ConnectionQuality::Poor => 4,
                ConnectionQuality::Bad => 5,
            };
            if order(&m.quality) > order(&worst) {
                worst = m.quality.clone();
            }
        }
        worst
    }

    /// Snapshot as a serde JSON value for UI consumption.
    pub fn summary_json(&self) -> serde_json::Value {
        let peers: Vec<serde_json::Value> = self
            .peers
            .values()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .collect();
        serde_json::json!({
            "peers": peers,
            "avg_rtt_ms": self.avg_rtt_ms(),
            "worst_quality": self.worst_quality().as_str(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_tiers() {
        assert_eq!(
            ConnectionQuality::from_stats(30.0, 0.0),
            ConnectionQuality::Excellent
        );
        assert_eq!(
            ConnectionQuality::from_stats(80.0, 0.5),
            ConnectionQuality::Good
        );
        assert_eq!(
            ConnectionQuality::from_stats(150.0, 2.0),
            ConnectionQuality::Fair
        );
        assert_eq!(
            ConnectionQuality::from_stats(300.0, 5.0),
            ConnectionQuality::Poor
        );
        assert_eq!(
            ConnectionQuality::from_stats(500.0, 15.0),
            ConnectionQuality::Bad
        );
    }

    #[test]
    fn collector_avg_rtt() {
        let mut c = MetricsCollector::new();
        c.init_peer("p1").update_stats(100.0, 5.0, 0.0);
        c.init_peer("p2").update_stats(200.0, 10.0, 0.0);
        let avg = c.avg_rtt_ms();
        assert!((avg - 150.0).abs() < 0.1, "avg={}", avg);
    }

    #[test]
    fn collector_worst_quality() {
        let mut c = MetricsCollector::new();
        c.init_peer("p1").update_stats(30.0, 0.0, 0.0); // Excellent
        c.init_peer("p2").update_stats(300.0, 5.0, 0.0); // Poor
        assert_eq!(c.worst_quality(), ConnectionQuality::Poor);
    }
}
