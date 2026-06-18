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

pub(super) fn rewrite_loopback_wt_url(wt_url: &str, signaling_url: &str) -> Option<String> {
    let wt_host = host_from_url(wt_url)?;
    if !is_loopback_or_wildcard(&wt_host) {
        return None;
    }
    let real_host = host_from_url(signaling_url)?;
    if is_loopback_or_wildcard(&real_host) || real_host == wt_host {
        return None;
    }
    Some(wt_url.replacen(&wt_host, &real_host, 1))
}

// ---------------------------------------------------------------------------
// PendingInvite
// ---------------------------------------------------------------------------

pub(super) struct PendingInvite {
    pub(super) inviter_peer_id: String,
    pub(super) inviter_identity_pub: String,
    pub(super) invite_id: String,
    pub(super) relay_hint: String,
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
