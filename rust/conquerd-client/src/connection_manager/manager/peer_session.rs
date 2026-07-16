//! Direct QUIC peer sessions: connect, aliases, reconnect, audio datagrams.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use conquerd_features::{
    channel_frame::{self, FrameClass},
    wellknown, AuthTier, CapabilityDescriptor, FeatureRegistry, InvocationContext, ReplayGuard,
};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

use crate::avatar_config::AvatarConfig as PeerAvatarConfig;
use crate::feature_trust::{FeatureTrustGate, FeatureTrustStore, TrustDecision};
use crate::file_transfer::{FileTransferManager, TransferEvent};
use crate::group_key::{GroupKeySource, SenderKeysGroup};
use crate::identity::Identity;
use crate::peer_store::PeerStore;
use crate::protocol::{MessageType, SignalingMessage};
use crate::quic_relay_client::{QuicRelayClient, RelayGameInbound, RelaySignalingInbound};
use crate::quic_tls;
use crate::web_app_client::{self, WebAppResponse};

use super::super::events::{ConnectionCommand, ConnectionEvent};
use super::super::internal::{
    InternalEvent, PeerConnection, PeerConnectionState, PeerOutbound, PeerTransportStats,
    PendingInvite, SupernodePingTracker, SupernodeSession, INVITE_TTL,
};
use super::super::quic::run_quic_peer_session;
use super::super::ws::supernode_ws_task;
use super::ConnectionManager;

use super::{
    unix_now_f64, AUDIO_CHANNEL_TAG, DEFAULT_QUIC_LISTENER_PORT, PEER_RECONNECT_MAX_BACKOFF_S,
    QUIC_PORT_FILE, QUIC_PORT_SEARCH_LIMIT,
};

pub fn parse_quic_lan_hint(hint: &str) -> Option<(String, u16)> {
    let trimmed = hint.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed
        .strip_prefix("quic://")
        .or_else(|| trimmed.strip_prefix("udp://"))
        .unwrap_or(trimmed);
    let authority = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(without_scheme);
    if authority.is_empty() {
        return None;
    }
    if let Ok(addr) = authority.parse::<SocketAddr>() {
        return Some((addr.ip().to_string(), addr.port()));
    }
    let (host, port) = authority.rsplit_once(':')?;
    let host = host.trim_matches(['[', ']']);
    let port = port.parse::<u16>().ok()?;
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((host.to_owned(), port))
}

/// Exponential backoff for direct-QUIC peer reconnect: 1s, 2s, 4s, … capped.
///
/// `attempts` is the number of reconnects already scheduled (0 → first wait).
pub fn peer_reconnect_backoff(attempts: u32) -> Duration {
    let shift = attempts.min(6);
    let secs = (1u64 << shift).min(PEER_RECONNECT_MAX_BACKOFF_S);
    Duration::from_secs(secs)
}

/// Direct-QUIC reconnect candidate after a disconnect.
#[derive(Debug, Clone)]
pub(super) struct PendingPeerReconnect {
    pub(super) host: String,
    pub(super) port: u16,
    /// Number of reconnect attempts already scheduled (drives backoff).
    pub(super) attempts: u32,
    pub(super) next_at: Instant,
}

pub(super) fn saved_quic_port_path() -> std::path::PathBuf {
    Identity::default_key_dir().join(QUIC_PORT_FILE)
}

pub(super) fn load_saved_quic_port() -> Option<u16> {
    std::fs::read_to_string(saved_quic_port_path())
        .ok()?
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

pub(super) fn save_quic_port(port: u16) {
    let path = saved_quic_port_path();
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("Could not create QUIC port state directory: {e}");
            return;
        }
    }
    if let Err(e) = std::fs::write(&path, port.to_string()) {
        warn!(
            "Could not persist QUIC listener port to {}: {e}",
            path.display()
        );
    }
}

pub(super) fn load_direct_p2p_settings() -> (bool, u16) {
    let path = Identity::default_key_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (
            true,
            load_saved_quic_port().unwrap_or(DEFAULT_QUIC_LISTENER_PORT),
        );
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return (
            true,
            load_saved_quic_port().unwrap_or(DEFAULT_QUIC_LISTENER_PORT),
        );
    };
    let enabled = value
        .get("direct_p2p_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let port = load_saved_quic_port()
        .or_else(|| {
            value
                .get("direct_p2p_port")
                .and_then(Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
        })
        .unwrap_or(DEFAULT_QUIC_LISTENER_PORT);
    (enabled, port)
}

/// Display name from this profile's `settings.json` (`local_handle`).
/// Used in invite URLs, handshake INIT/ACCEPT, and `HandleUpdate` broadcasts.
pub(super) fn read_local_display_handle() -> String {
    let path = Identity::default_key_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return String::new();
    };
    value
        .get("local_handle")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

pub fn peer_quic_endpoint(record: &crate::peer_store::PeerRecord) -> Option<(String, u16)> {
    record
        .relay_hints
        .iter()
        .find_map(|hint| parse_quic_lan_hint(hint))
        .or_else(|| (record.quic_port != 0).then(|| ("127.0.0.1".to_owned(), record.quic_port)))
}

impl ConnectionManager {
    /// Lazily create the QUIC endpoint. Port 0 means the saved profile port,
    /// then the default. If that port is occupied, try consecutive ports.
    pub(super) fn ensure_quic_endpoint(&mut self, port: u16) -> bool {
        if self.quic_endpoint.is_some() {
            return true;
        }
        let preferred = if port == 0 {
            load_saved_quic_port().unwrap_or(DEFAULT_QUIC_LISTENER_PORT)
        } else {
            port
        };
        let mut last_error = None;
        for offset in 0..QUIC_PORT_SEARCH_LIMIT {
            let Some(candidate) = preferred.checked_add(offset) else {
                break;
            };
            match quic_tls::make_quic_endpoint(self.identity.signing_key(), candidate) {
                Ok(ep) => {
                    info!(
                        "QUIC endpoint bound on {}",
                        ep.local_addr().map(|a| a.to_string()).unwrap_or_default()
                    );
                    save_quic_port(candidate);
                    self.quic_endpoint = Some(ep);
                    return true;
                }
                Err(e) => {
                    debug!("QUIC port {candidate} unavailable: {e}");
                    last_error = Some(e);
                }
            }
        }
        error!(
            "Failed to create QUIC endpoint starting at port {preferred}: {}",
            last_error
                .map(|e| e.to_string())
                .unwrap_or_else(|| "no usable port in range".to_owned())
        );
        false
    }

    pub(super) fn local_quic_hint(&self) -> Option<String> {
        let port = self.quic_endpoint.as_ref()?.local_addr().ok()?.port();
        let host = crate::platform::local_ip().unwrap_or_else(|| "127.0.0.1".to_owned());
        Some(format!("quic://{host}:{port}"))
    }

    pub(super) async fn connect_direct_quic(&mut self, peer_id: &str, host: &str, port: u16) {
        // Avoid stacking parallel dials for the same peer.
        if let Some(peer) = self.peers.get(peer_id) {
            if peer.state == PeerConnectionState::Connecting
                || peer.state == PeerConnectionState::Connected
            {
                return;
            }
        }

        info!(
            "QUIC outbound to {}:{} (peer {})",
            host,
            port,
            &peer_id[..8.min(peer_id.len())]
        );

        // Ensure we have a QUIC endpoint (ephemeral port).
        if !self.ensure_quic_endpoint(0) {
            return;
        }
        let Some(endpoint) = self.quic_endpoint.as_ref() else {
            error!("QUIC endpoint missing after ensure_quic_endpoint");
            return;
        };
        let endpoint = endpoint.clone();

        let addr: SocketAddr = match format!("{host}:{port}").parse() {
            Ok(a) => a,
            Err(e) => {
                error!("Invalid peer address {host}:{port}: {e}");
                return;
            }
        };

        let peer_id_owned = peer_id.to_owned();
        let internal_tx = self.internal_tx.clone();

        tokio::spawn(async move {
            // Quinn server_name is arbitrary for self-signed certs; we verify by peer_id.
            let connecting = match endpoint.connect(addr, "conquerd") {
                Ok(c) => c,
                Err(e) => {
                    warn!("QUIC connect init to {addr} failed: {e}");
                    let _ = internal_tx
                        .send(InternalEvent::QuicDisconnected {
                            peer_id: peer_id_owned,
                        })
                        .await;
                    return;
                }
            };

            let connection = match connecting.await {
                Ok(c) => c,
                Err(e) => {
                    warn!("QUIC handshake with {addr} failed: {e}");
                    let _ = internal_tx
                        .send(InternalEvent::QuicDisconnected {
                            peer_id: peer_id_owned,
                        })
                        .await;
                    return;
                }
            };
            let actual_peer_id = connection
                .peer_identity()
                .and_then(|id| id.downcast::<Vec<rustls::pki_types::CertificateDer>>().ok())
                .and_then(|certs| certs.first().cloned())
                .and_then(|cert| quic_tls::cn_from_cert_der(&cert))
                .and_then(|cn| {
                    let pub_bytes = hex::decode(&cn).ok()?;
                    Some(quic_tls::peer_id_from_pub_bytes(&pub_bytes))
                });
            if actual_peer_id.as_deref() != Some(peer_id_owned.as_str()) {
                warn!(
                    "QUIC peer id mismatch for {addr}: expected {}, got {}; waiting for signed invite handshake",
                    &peer_id_owned[..8.min(peer_id_owned.len())],
                    actual_peer_id
                        .as_deref()
                        .map(|id| &id[..8.min(id.len())])
                        .unwrap_or("<none>")
                );
            }

            info!(
                "QUIC connected to {addr} (peer {})",
                &peer_id_owned[..8.min(peer_id_owned.len())]
            );
            run_quic_peer_session(connection, peer_id_owned, internal_tx).await;
        });

        // Register as connecting
        self.peers
            .entry(peer_id.to_owned())
            .or_insert_with(|| PeerConnection::new(peer_id))
            .state = PeerConnectionState::Connecting;
    }

    /// Schedule a jitter-free exponential-backoff re-dial for a trusted peer
    /// that still has a stored QUIC endpoint. No-op when direct P2P is off,
    /// the peer is blocked/supernode, or no endpoint is known.
    pub(super) fn schedule_peer_reconnect(&mut self, peer_id: &str) {
        let (direct_enabled, _) = load_direct_p2p_settings();
        if !direct_enabled {
            return;
        }
        let endpoint = {
            let store = self.peer_store.read();
            let Some(record) = store
                .get(peer_id)
                .or_else(|| store.get_by_identity(peer_id))
            else {
                return;
            };
            if record.blocked || record.revoked || record.is_supernode {
                return;
            }
            peer_quic_endpoint(record)
        };
        let Some((host, port)) = endpoint else {
            return;
        };
        let attempts = self
            .pending_peer_reconnects
            .get(peer_id)
            .map(|p| p.attempts)
            .unwrap_or(0);
        let delay = peer_reconnect_backoff(attempts);
        info!(
            "Scheduling QUIC reconnect for {} to {}:{} in {:?} (attempt {})",
            &peer_id[..8.min(peer_id.len())],
            host,
            port,
            delay,
            attempts.saturating_add(1)
        );
        self.pending_peer_reconnects.insert(
            peer_id.to_owned(),
            PendingPeerReconnect {
                host,
                port,
                attempts: attempts.saturating_add(1),
                next_at: Instant::now() + delay,
            },
        );
    }

    pub(super) fn cancel_peer_reconnect(&mut self, peer_id: &str) {
        self.pending_peer_reconnects.remove(peer_id);
    }

    pub(super) async fn tick_peer_reconnects(&mut self) {
        let now = Instant::now();
        let due: Vec<(String, String, u16)> = self
            .pending_peer_reconnects
            .iter()
            .filter(|(_, p)| p.next_at <= now)
            .filter(|(id, _)| {
                self.peers
                    .get(id.as_str())
                    .map(|p| p.state == PeerConnectionState::Disconnected)
                    .unwrap_or(true)
            })
            .map(|(id, p)| (id.clone(), p.host.clone(), p.port))
            .collect();
        for (peer_id, host, port) in due {
            // Push next_at forward so a failed dial does not hot-loop this tick
            // interval; schedule_peer_reconnect on the resulting disconnect
            // will advance attempts further.
            if let Some(pending) = self.pending_peer_reconnects.get_mut(&peer_id) {
                pending.next_at = Instant::now() + peer_reconnect_backoff(pending.attempts);
            }
            self.connect_direct_quic(&peer_id, &host, port).await;
        }
    }

    pub(super) async fn handle_incoming_quic(&mut self, incoming: quinn::Incoming) {
        let internal_tx = self.internal_tx.clone();

        tokio::spawn(async move {
            let connection = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    warn!("Inbound QUIC handshake failed: {e}");
                    return;
                }
            };
            // Derive peer_id from the peer's certificate CN. Fail closed —
            // never key a session by raw remote address alone.
            let Some(peer_id) = connection
                .peer_identity()
                .and_then(|id| id.downcast::<Vec<rustls::pki_types::CertificateDer>>().ok())
                .and_then(|certs| certs.first().cloned())
                .and_then(|cert| quic_tls::cn_from_cert_der(&cert))
                .and_then(|cn| {
                    // CN = hex(pub_bytes), peer_id = hex(sha256(pub_bytes))
                    let pub_bytes = hex::decode(&cn).ok()?;
                    Some(quic_tls::peer_id_from_pub_bytes(&pub_bytes))
                })
            else {
                warn!(
                    "Inbound QUIC from {} missing cert-derived peer id — dropping",
                    connection.remote_address()
                );
                connection.close(0u32.into(), b"no peer id");
                return;
            };

            info!(
                "Inbound QUIC from {} (peer {})",
                connection.remote_address(),
                &peer_id[..8.min(peer_id.len())]
            );
            run_quic_peer_session(connection, peer_id, internal_tx).await;
        });
    }

    pub(super) fn resolve_quic_peer_alias(&self, peer_id: &str) -> String {
        self.quic_peer_aliases
            .get(peer_id)
            .cloned()
            .unwrap_or_else(|| peer_id.to_owned())
    }

    pub(super) fn relabel_quic_peer_session(
        &mut self,
        current_peer_id: &str,
        canonical_peer_id: &str,
    ) {
        if current_peer_id == canonical_peer_id || canonical_peer_id.is_empty() {
            return;
        }
        if canonical_peer_id == self.identity.peer_id() {
            warn!(
                "Ignoring QUIC relabel from {} to local peer id {}",
                &current_peer_id[..8.min(current_peer_id.len())],
                &canonical_peer_id[..8.min(canonical_peer_id.len())]
            );
            return;
        }

        let Some(mut provisional) = self.peers.remove(current_peer_id) else {
            return;
        };
        provisional.peer_id = canonical_peer_id.to_owned();
        let entry = self
            .peers
            .entry(canonical_peer_id.to_owned())
            .or_insert_with(|| PeerConnection::new(canonical_peer_id));
        entry.state = provisional.state;
        entry.quic_out_tx = provisional.quic_out_tx.take();
        entry.connected_at = provisional.connected_at;

        if let Some(stats) = self.transport_stats.remove(current_peer_id) {
            self.transport_stats
                .insert(canonical_peer_id.to_owned(), stats);
        }
        self.quic_peer_aliases
            .insert(current_peer_id.to_owned(), canonical_peer_id.to_owned());
        // Live session under the canonical id — drop any reconnect timers.
        self.pending_peer_reconnects.remove(current_peer_id);
        self.cancel_peer_reconnect(canonical_peer_id);

        info!(
            "QUIC peer relabeled {} -> {} after signed invite handshake",
            &current_peer_id[..8.min(current_peer_id.len())],
            &canonical_peer_id[..8.min(canonical_peer_id.len())]
        );
    }

    /// Send a real-time Opus audio frame to a directly-connected peer.
    ///
    /// ## Audio Dispatch Decision (P2 #9 - Option A)
    ///
    /// `core.audio.opus` deliberately uses a **dedicated low-tag datagram path**
    /// (AUDIO_CHANNEL_TAG in the reserved 0x01–0x0F range) instead of going
    /// through the generic feature datagram multiplexer (`transport.quic.feature_datagram.v1`
    /// + dynamic 0x10–0xEF tags) and a full `FeatureModule` implementation.
    ///
    /// Reasons (documented trade-off):
    /// - Real-time audio has extremely strict latency/jitter requirements
    ///   (target < 20-40 ms one-way for natural conversation).
    /// - The generic path (tag allocation, registry dispatch, module trait call)
    ///   adds unacceptable overhead and potential contention.
    /// - agents.md explicitly requires keeping "DSP and per-tick feature loops
    ///   within real-time budget" while also saying "don't shortcut around the
    ///   capability layer for convenience."
    ///
    /// What **is** honored (mandatory per guardrails):
    /// - Capability advertisement & negotiation (via `CoreAudioOpusModule`)
    /// - Per-feature outbound quota via `check_audio_quota` (see above)
    /// - Symmetric inbound quota enforcement on the receiver
    ///
    /// This is a **narrow, justified, permanent pragmatic bypass** for the
    /// real-time audio hot path only. Non-real-time audio use cases should
    /// use the normal feature path.
    ///
    /// If the peer has no QUIC connection the frame is silently dropped (UDP
    /// semantics — losing a frame is acceptable for real-time audio).
    pub(super) async fn send_audio_datagram(&self, peer_id: &str, opus_data: Vec<u8>) {
        let conn = match self.peers.get(peer_id) {
            Some(c) => c,
            None => return,
        };
        // Outbound quota gate (via dedicated helper to make the invariant obvious).
        if !self.check_audio_quota(peer_id, opus_data.len()) {
            debug!(
                "[core.audio.opus] outbound quota exceeded for {}; dropping frame",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        }
        // Real QUIC datagrams (not a per-frame uni stream). Same wire layout as
        // before: `[AUDIO_TAG][id_len][peer_id][opus]`.
        if let Some(ref qtx) = conn.quic_out_tx {
            let id_bytes = peer_id.as_bytes();
            let mut frame = Vec::with_capacity(2 + id_bytes.len() + opus_data.len());
            frame.push(AUDIO_CHANNEL_TAG);
            frame.push(id_bytes.len() as u8);
            frame.extend_from_slice(id_bytes);
            frame.extend_from_slice(&opus_data);
            if qtx.try_send(PeerOutbound::Datagram(frame)).is_err() {
                // Best-effort real-time audio: count for the drop metrics but
                // never log per frame.
                use super::super::internal::drop_metrics;
                drop_metrics::note(&drop_metrics::PEER_OUTBOUND);
            }
        }
        // If no QUIC, drop silently — audio is real-time; WS relay is too slow.
    }

    /// Send a raw Opus frame to a peer via QUIC datagram.
    ///
    /// Send a real-time Opus audio frame to a directly-connected peer.
    ///
    /// ## Audio Dispatch Decision (P2 #9 - Option A)
    ///
    /// `core.audio.opus` deliberately uses a **dedicated low-tag datagram path**
    /// (AUDIO_CHANNEL_TAG in the reserved 0x01–0x0F range) instead of going
    /// through the generic feature datagram multiplexer.
    ///
    /// See the long rationale comment below for why we chose the permanent
    /// documented bypass (Option A) over routing through a normal FeatureModule.
    ///
    /// Frame format: `[AUDIO_CHANNEL_TAG, peer_id_len(1 byte), ...peer_id_utf8, ...opus_data]`.
    /// Private helper that **must** be called before every audio frame send.
    /// This makes the "always gate audio" invariant obvious and hard to violate.
    #[inline]
    pub(super) fn check_audio_quota(&self, target: &str, byte_count: usize) -> bool {
        self.feature_registry
            .gate_through_feature("core.audio.opus", target, byte_count)
    }

    /// Emit a consolidated [`ConnectionEvent::SessionStateUpdate`] for `peer_id`
    /// derived from live transport facts (direct QUIC state, supernode relay
    /// availability, and the latest RTT/loss/jitter). The UI drives its
    /// connection banner / `connection_mode` from this instead of inferring the
    /// path from individual connect/disconnect events.
    pub(super) fn emit_peer_session_state(&self, peer_id: &str) {
        let (direct_connected, direct_connecting) = match self.peers.get(peer_id) {
            Some(p) => (
                p.state == PeerConnectionState::Connected,
                p.state == PeerConnectionState::Connecting,
            ),
            None => (false, false),
        };
        let relay_available = self.supernodes.values().any(|sn| sn.connected);
        let stats = self.transport_stats.get(peer_id);
        let rtt_ms = stats.map(|s| s.rtt_ms).filter(|rtt| *rtt > 0.0);
        let packet_loss = stats.map(|s| s.packet_loss_pct).unwrap_or(0.0);
        let jitter_ms = stats.map(|s| s.jitter_ms).unwrap_or(0.0);
        let state = crate::session_state::PeerSessionState::from_transport(
            peer_id,
            direct_connected,
            direct_connecting,
            relay_available,
            rtt_ms,
            packet_loss,
            jitter_ms,
            crate::session_state::VoiceMode::None,
        );
        self.emit_event(ConnectionEvent::SessionStateUpdate(state));
    }
}
