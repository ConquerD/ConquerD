//! Internal connection-manager state and helpers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::protocol::SignalingMessage;
use crate::quic_relay_client::QuicRelayClient;

// ---------------------------------------------------------------------------
// Peer connection tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PeerConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug)]
pub(super) struct PeerConnection {
    pub(super) peer_id: String,
    pub(super) state: PeerConnectionState,
    pub(super) quic_sig_tx: Option<mpsc::Sender<Vec<u8>>>,
    pub(super) connected_at: Option<Instant>,
}

impl PeerConnection {
    pub(super) fn new(peer_id: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            state: PeerConnectionState::Disconnected,
            quic_sig_tx: None,
            connected_at: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal QUIC / WebSocket events (task → connection manager)
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub(super) enum InternalEvent {
    QuicConnected {
        peer_id: String,
        sig_tx: mpsc::Sender<Vec<u8>>,
    },
    QuicDisconnected {
        peer_id: String,
    },
    QuicStats {
        peer_id: String,
        rtt_ms: f64,
        packet_loss_pct: f64,
        jitter_ms: f64,
        bandwidth_kbps: f64,
    },
    QuicSignalingData {
        peer_id: String,
        data: Vec<u8>,
    },
    WsConnected {
        peer_id: String,
    },
    WsDisconnected {
        peer_id: String,
    },
    WsSignalingMessage {
        /// Identity pubkey of the supernode that delivered this frame.
        supernode_id: String,
        msg: SignalingMessage,
    },

    RelayClientReady {
        supernode_id: String,
        client: Option<Arc<QuicRelayClient>>,
    },
}

// ---------------------------------------------------------------------------
// SupernodeSession
// ---------------------------------------------------------------------------

pub(super) struct SupernodeSession {
    pub(super) peer_id: String,
    pub(super) ws_url: String,
    pub(super) send_tx: mpsc::Sender<WsMessage>,
    pub(super) connected: bool,
    pub(super) ws_task: tokio::task::JoinHandle<()>,
}

pub(super) fn host_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split("://").last().unwrap_or(url);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    if authority.is_empty() {
        return None;
    }
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let host = if let Some(rest) = host_port.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host_port.split(':').next().unwrap_or(host_port)
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

pub(super) fn is_loopback_or_wildcard(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1" | "::")
}

// ---------------------------------------------------------------------------
// PendingInvite
// ---------------------------------------------------------------------------

pub(super) struct PendingInvite {
    pub(super) inviter_peer_id: String,
    pub(super) inviter_identity_pub: String,
    pub(super) invite_id: String,
    pub(super) relay_hint: String,
    pub(super) lan_hint: String,
    pub(super) is_supernode: bool,
    pub(super) created_at: Instant,
}

pub(super) const INVITE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Default)]
pub(super) struct PeerTransportStats {
    pub(super) rtt_ms: f64,
    pub(super) packet_loss_pct: f64,
    pub(super) jitter_ms: f64,
    pub(super) bandwidth_kbps: f64,
}

/// Application-level Ping/Pong RTT tracking for supernode WebSocket sessions.
#[derive(Debug, Clone, Default)]
pub(super) struct SupernodePingTracker {
    pending_since: Option<Instant>,
    pings_sent: u32,
    pongs_recv: u32,
    last_rtt_ms: f64,
    prev_rtt_ms: Option<f64>,
}

impl SupernodePingTracker {
    pub(super) fn note_ping_sent(&mut self) {
        self.pings_sent = self.pings_sent.saturating_add(1);
        self.pending_since = Some(Instant::now());
    }

    pub(super) fn note_pong(&mut self) -> Option<PeerTransportStats> {
        let sent_at = self.pending_since.take()?;
        self.pongs_recv = self.pongs_recv.saturating_add(1);
        let rtt_ms = sent_at.elapsed().as_secs_f64() * 1000.0;
        let jitter_ms = self
            .prev_rtt_ms
            .or_else(|| (self.last_rtt_ms > 0.0).then_some(self.last_rtt_ms))
            .map(|prev| (rtt_ms - prev).abs())
            .unwrap_or(0.0);
        self.prev_rtt_ms = Some(self.last_rtt_ms);
        self.last_rtt_ms = rtt_ms;
        let packet_loss_pct = if self.pings_sent > 0 {
            ((self.pings_sent.saturating_sub(self.pongs_recv)) as f64 / self.pings_sent as f64)
                * 100.0
        } else {
            0.0
        };
        if self.pings_sent > 20 {
            self.pings_sent = 10;
            self.pongs_recv = (self.pongs_recv / 2).max(1);
        }
        Some(PeerTransportStats {
            rtt_ms,
            packet_loss_pct,
            jitter_ms,
            bandwidth_kbps: 0.0,
        })
    }
}

#[cfg(test)]
mod ping_tracker_tests {
    use super::*;

    #[test]
    fn supernode_ping_tracker_rtt_and_loss() {
        let mut t = SupernodePingTracker::default();
        t.note_ping_sent();
        std::thread::sleep(Duration::from_millis(5));
        let stats = t.note_pong().expect("pong");
        assert!(stats.rtt_ms >= 4.0);
        assert_eq!(stats.packet_loss_pct, 0.0);

        t.note_ping_sent();
        let stats2 = t.note_pong().expect("second pong");
        assert!(stats2.jitter_ms >= 0.0);

        t.note_ping_sent();
        t.note_ping_sent();
        let stats3 = t.note_pong().expect("third pong");
        assert!(stats3.packet_loss_pct >= 0.0 && stats3.packet_loss_pct <= 100.0);
    }
}
