//! Network quality monitor.
//!
//! Runs a periodic stats poll on a tokio interval. QUIC (quinn)
//! exposes path stats directly via `quinn::Connection::stats()`.
//!
//! Usage
//! -----
//! ```ignore
//! let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
//! let (metrics_tx, metrics_rx) = tokio::sync::mpsc::channel(16);
//! tokio::spawn(network_monitor_task(connections, metrics_tx, stop_rx));
//! ```

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Connection quality classification
// ---------------------------------------------------------------------------

/// High-level quality bucket, mirrors the Python `ConnectionQuality` enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionQuality {
    Excellent,
    Good,
    Fair,
    Poor,
    Unknown,
}

impl ConnectionQuality {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Excellent => "excellent",
            Self::Good => "good",
            Self::Fair => "fair",
            Self::Poor => "poor",
            Self::Unknown => "unknown",
        }
    }
}

/// Path type derived from the remote IP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathType {
    LanDirect,
    WanDirect,
    Relay,
    Unknown,
}

impl PathType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::LanDirect => "lan_direct",
            Self::WanDirect => "wan_direct",
            Self::Relay => "relay",
            Self::Unknown => "unknown",
        }
    }
}

/// Classify a remote IP string into a [`PathType`].
pub fn classify_remote_host(remote_host: &str) -> PathType {
    let ip: IpAddr = match remote_host.parse() {
        Ok(a) => a,
        Err(_) => return PathType::Unknown,
    };
    if ip.is_loopback() {
        return PathType::LanDirect;
    }
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_private() || v4.is_link_local() {
                PathType::LanDirect
            } else {
                PathType::WanDirect
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                PathType::LanDirect
            } else {
                PathType::WanDirect
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-peer metrics snapshot
// ---------------------------------------------------------------------------

/// Snapshot of metrics for a single peer connection.
#[derive(Debug, Clone)]
pub struct PeerMetrics {
    pub peer_id: String,
    pub rtt_ms: f64,
    pub packet_loss_pct: f64,
    pub path_type: PathType,
    pub relay_used: bool,
    pub quality: ConnectionQuality,
}

impl PeerMetrics {
    pub fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            rtt_ms: 0.0,
            packet_loss_pct: 0.0,
            path_type: PathType::Unknown,
            relay_used: false,
            quality: ConnectionQuality::Unknown,
        }
    }

    /// Re-derive quality from current RTT and loss values.
    pub fn update_quality(&mut self) {
        self.quality = if self.rtt_ms == 0.0 {
            ConnectionQuality::Unknown
        } else if self.rtt_ms < 80.0 && self.packet_loss_pct < 2.0 {
            ConnectionQuality::Excellent
        } else if self.rtt_ms < 150.0 && self.packet_loss_pct < 5.0 {
            ConnectionQuality::Good
        } else if self.rtt_ms < 300.0 && self.packet_loss_pct < 10.0 {
            ConnectionQuality::Fair
        } else {
            ConnectionQuality::Poor
        };
    }
}

// ---------------------------------------------------------------------------
// Monitor event
// ---------------------------------------------------------------------------

/// Events emitted by the monitor task.
#[derive(Debug, Clone)]
pub enum MonitorEvent {
    /// Aggregated metrics snapshot for all active peers.
    MetricsUpdated(Vec<PeerMetrics>),
    /// Quality changed for a specific peer.
    PeerQualityChanged {
        peer_id: String,
        quality: ConnectionQuality,
    },
}

// ---------------------------------------------------------------------------
// Snapshot input — caller supplies this each poll tick
// ---------------------------------------------------------------------------

/// A caller-supplied snapshot of a single QUIC connection's stats.
/// The connection manager builds these and sends them into the monitor.
#[derive(Debug, Clone)]
pub struct ConnectionSnapshot {
    pub peer_id: String,
    pub remote_host: String,
    pub rtt_us: u64, // smoothed RTT in microseconds (from quinn)
    pub relay_used: bool,
}

// ---------------------------------------------------------------------------
// Monitor task
// ---------------------------------------------------------------------------

const DEFAULT_POLL_INTERVAL_MS: u64 = 2_000;

/// Tokio task that periodically receives connection snapshots and emits
/// aggregated metrics.  The caller pushes snapshots via `snapshot_tx` and
/// reads events from `event_rx`.
///
/// Send `()` on `stop_rx` to shut down the task cleanly.
pub async fn network_monitor_task(
    mut snapshot_rx: mpsc::Receiver<Vec<ConnectionSnapshot>>,
    event_tx: mpsc::Sender<MonitorEvent>,
    mut stop_rx: oneshot::Receiver<()>,
    poll_interval_ms: Option<u64>,
) {
    let interval_ms = poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS);
    let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
    info!("Network monitor started (interval={}ms)", interval_ms);

    let mut last_snapshots: Vec<ConnectionSnapshot> = Vec::new();
    let mut previous_quality: HashMap<String, ConnectionQuality> = HashMap::new();

    loop {
        tokio::select! {
            // Receive latest snapshots from the connection manager
            Some(snaps) = snapshot_rx.recv() => {
                last_snapshots = snaps;
            }
            // Periodic poll
            _ = ticker.tick() => {
                let mut metrics_vec: Vec<PeerMetrics> = Vec::new();
                let mut quality_changes: Vec<(String, ConnectionQuality)> = Vec::new();

                for snap in &last_snapshots {
                    let mut m = PeerMetrics::new(snap.peer_id.clone());
                    m.rtt_ms    = snap.rtt_us as f64 / 1_000.0;
                    m.relay_used = snap.relay_used;
                    m.path_type = if snap.relay_used {
                        PathType::Relay
                    } else {
                        classify_remote_host(&snap.remote_host)
                    };
                    m.update_quality();

                    let prev = previous_quality
                        .get(&snap.peer_id)
                        .cloned()
                        .unwrap_or(ConnectionQuality::Unknown);
                    if m.quality != prev {
                        quality_changes.push((snap.peer_id.clone(), m.quality.clone()));
                        previous_quality.insert(snap.peer_id.clone(), m.quality.clone());
                    }
                    metrics_vec.push(m);
                }

                if !metrics_vec.is_empty() {
                    let _ = event_tx.try_send(MonitorEvent::MetricsUpdated(metrics_vec));
                }
                for (peer_id, quality) in quality_changes {
                    debug!(
                        "Peer {} quality -> {}",
                        &peer_id[..8.min(peer_id.len())], quality.as_str()
                    );
                    let _ = event_tx.try_send(MonitorEvent::PeerQualityChanged { peer_id, quality });
                }
            }
            // Stop signal
            _ = &mut stop_rx => {
                info!("Network monitor stopped");
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_loopback() {
        assert_eq!(classify_remote_host("127.0.0.1"), PathType::LanDirect);
        assert_eq!(classify_remote_host("::1"), PathType::LanDirect);
    }

    #[test]
    fn classify_private_ipv4() {
        assert_eq!(classify_remote_host("192.168.1.50"), PathType::LanDirect);
        assert_eq!(classify_remote_host("10.0.0.1"), PathType::LanDirect);
        assert_eq!(classify_remote_host("172.16.5.1"), PathType::LanDirect);
    }

    #[test]
    fn classify_public_ipv4() {
        assert_eq!(classify_remote_host("8.8.8.8"), PathType::WanDirect);
        assert_eq!(classify_remote_host("203.0.113.42"), PathType::WanDirect);
    }

    #[test]
    fn classify_invalid_string() {
        assert_eq!(classify_remote_host("not-an-ip"), PathType::Unknown);
        assert_eq!(classify_remote_host(""), PathType::Unknown);
    }

    #[test]
    fn quality_excellent_low_rtt() {
        let mut m = PeerMetrics::new("peer");
        m.rtt_ms = 30.0;
        m.packet_loss_pct = 0.5;
        m.update_quality();
        assert_eq!(m.quality, ConnectionQuality::Excellent);
    }

    #[test]
    fn quality_poor_high_rtt() {
        let mut m = PeerMetrics::new("peer");
        m.rtt_ms = 500.0;
        m.packet_loss_pct = 15.0;
        m.update_quality();
        assert_eq!(m.quality, ConnectionQuality::Poor);
    }

    #[test]
    fn quality_unknown_zero_rtt() {
        let mut m = PeerMetrics::new("peer");
        // rtt_ms = 0, packet_loss = 0
        m.update_quality();
        assert_eq!(m.quality, ConnectionQuality::Unknown);
    }

    #[tokio::test]
    async fn monitor_task_stops_on_signal() {
        let (_snap_tx, snap_rx) = tokio::sync::mpsc::channel(8);
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(network_monitor_task(
            snap_rx,
            event_tx,
            stop_rx,
            Some(50), // fast interval for test
        ));
        let _ = stop_tx.send(());
        // Task should finish cleanly
        tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("monitor task did not stop within 2s")
            .expect("task panicked");
    }

    #[tokio::test]
    async fn monitor_emits_metrics_for_snapshot() {
        let (snap_tx, snap_rx) = tokio::sync::mpsc::channel(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(16);
        let (_stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(network_monitor_task(
            snap_rx,
            event_tx,
            stop_rx,
            Some(20), // very fast tick
        ));

        snap_tx
            .send(vec![ConnectionSnapshot {
                peer_id: "aaaa".into(),
                remote_host: "8.8.8.8".into(),
                rtt_us: 25_000, // 25 ms
                relay_used: false,
            }])
            .await
            .unwrap();

        // Wait for a MetricsUpdated event
        let ev = tokio::time::timeout(std::time::Duration::from_millis(200), event_rx.recv())
            .await
            .expect("no event within 200ms")
            .unwrap();

        assert!(matches!(ev, MonitorEvent::MetricsUpdated(_)));
    }
}
