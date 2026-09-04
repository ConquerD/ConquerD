//! Direct QUIC peer sessions: connect, aliases, reconnect, audio datagrams.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use serde_json::Value;
use tracing::{debug, error, info, warn};

use crate::identity::Identity;
use crate::protocol::{MessageType, SignalingMessage};
use crate::quic_tls;

use super::super::events::ConnectionEvent;
use super::super::internal::{InternalEvent, PeerConnection, PeerConnectionState, PeerOutbound};
use super::super::quic::run_quic_peer_session;
use super::ConnectionManager;

use super::{
    AUDIO_CHANNEL_TAG, CONTENT_AUDIO_CHANNEL_TAG, DEFAULT_QUIC_LISTENER_PORT,
    PEER_RECONNECT_MAX_BACKOFF_S, QUIC_PORT_FILE, QUIC_PORT_SEARCH_LIMIT, VIDEO_CHANNEL_TAG,
};

/// How long a hole-punch registration is assumed to still be live at the
/// supernode. Matches the 30s the supernode keeps an unpaired registration
/// before sweeping it, so a retry is only sent once the old one is gone.
const PUNCH_REGISTRATION_TTL: Duration = Duration::from_secs(30);

/// Ceiling on how long a `PUNCH_READY` may ask us to wait before dialing.
/// The supernode picks ~500ms; anything much larger is clock skew or a bad
/// value, and parking a dial on it would strand the connection attempt.
const PUNCH_MAX_DELAY_S: f64 = 5.0;

/// Parse a `host:port` endpoint from punch coordination into a dialable
/// address, refusing the ones that would make the client a probe.
///
/// Unspecified and loopback addresses are rejected because a punch toward
/// them is either meaningless or aimed back at this machine; multicast
/// because a NAT punch has exactly one intended recipient.
fn parse_punch_endpoint(raw: &str) -> Option<SocketAddr> {
    let addr: SocketAddr = raw.trim().parse().ok()?;
    if addr.port() == 0 || addr.ip().is_unspecified() || addr.ip().is_loopback() {
        return None;
    }
    if addr.ip().is_multicast() {
        return None;
    }
    Some(addr)
}

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

/// Own avatar configuration JSON from this profile's `settings.json`
/// (`avatar_config_json`). An empty string means "use defaults" — there is
/// nothing to send because the receiver already falls back to the identical
/// `AvatarConfig::default()`. Mirrors [`read_local_display_handle`] so the
/// avatar propagates on connect the same way the handle does.
pub(super) fn read_local_avatar_config() -> String {
    let path = Identity::default_key_dir().join("settings.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return String::new();
    };
    value
        .get("avatar_config_json")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned()
}

pub fn peer_quic_endpoint(record: &crate::peer_store::PeerRecord) -> Option<(String, u16)> {
    peer_quic_endpoints(record).into_iter().next()
}

/// Every address worth dialing for `record`, best first.
///
/// A peer may be reachable at more than one address and which one works is not
/// knowable in advance: the LAN hint wins whenever both peers are on the same
/// network (no NAT in the path at all), while a public hint — learned from a
/// UPnP mapping or from what a supernode observed — is the only thing that
/// works from anywhere else. Both are kept, in that order.
///
/// The loopback entry is last and deliberate: it is what makes two profiles on
/// one machine connect directly, and it is useless anywhere else, so it must
/// never crowd out a real address the way it did when it was the sole
/// fallback.
pub fn peer_quic_endpoints(record: &crate::peer_store::PeerRecord) -> Vec<(String, u16)> {
    let mut out: Vec<(String, u16)> = Vec::new();
    let mut push = |cand: (String, u16)| {
        if !out.contains(&cand) {
            out.push(cand);
        }
    };
    for hint in &record.relay_hints {
        if let Some(cand) = parse_quic_lan_hint(hint) {
            push(cand);
        }
    }
    if record.quic_port != 0 {
        push(("127.0.0.1".to_owned(), record.quic_port));
    }
    out
}

/// Whether `host` is only meaningful on this machine.
///
/// Used to decide that a peer has nothing dialable from here, which is the
/// signal that a hole punch is worth the round trip.
pub fn is_local_only_host(host: &str) -> bool {
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback() || ip.is_unspecified())
        .unwrap_or(false)
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
                    // Ask the router to forward this exact port. It is the
                    // listener peers dial, which is the only mapping that
                    // buys a direct session — mapping an outbound relay port
                    // achieves nothing, because that connection is already
                    // established from this side.
                    //
                    // Same port inside and out so the advertised hint stays
                    // true if the mapping is later dropped and the peer falls
                    // back to a punched address.
                    if let Some(tx) = &self.upnp_cmd {
                        let _ = tx.try_send(crate::upnp::UpnpCommand::AddMapping {
                            internal_port: candidate,
                            external_port: candidate,
                            protocol: crate::upnp::Protocol::Udp,
                            description: "ConquerD QUIC".to_string(),
                        });
                    }
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

        // Peers with nothing dialable never enter the reconnect schedule at
        // all — there is no address to retry — so without this they would get
        // one hole-punch attempt at startup and never another. A peer that
        // came online later, or moved networks, would stay unreachable for the
        // life of the process.
        //
        // Registering is cheap and self-limiting: `request_hole_punch` refuses
        // to re-register inside the supernode's own 30s rendezvous window, so
        // this tick contributes at most one message per peer per window.
        let stranded: Vec<String> = {
            let store = self.peer_store.read();
            store
                .auto_connect_peers()
                .into_iter()
                .filter(|record| !record.is_supernode)
                .filter(|record| {
                    !peer_quic_endpoints(record)
                        .iter()
                        .any(|(host, _)| !is_local_only_host(host))
                })
                .map(|record| record.peer_id.clone())
                .collect()
        };
        for peer_id in stranded {
            let connected = self
                .peers
                .get(&peer_id)
                .map(|p| p.state != PeerConnectionState::Disconnected)
                .unwrap_or(false);
            if !connected {
                self.request_hole_punch(&peer_id).await;
            }
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

    #[inline]
    pub(super) fn check_video_quota(&self, target: &str, byte_count: usize) -> bool {
        self.feature_registry
            .gate_through_feature("core.video.v1", target, byte_count)
    }

    #[inline]
    pub(super) fn check_content_audio_quota(&self, target: &str, byte_count: usize) -> bool {
        self.feature_registry
            .gate_through_feature("core.audio.content.v1", target, byte_count)
    }

    /// Send one content-audio frame to a directly-connected peer.
    ///
    /// Same documented low-tag bypass as
    /// [`send_video_datagram`](Self::send_video_datagram) / voice: real-time
    /// datagram under `CONTENT_AUDIO_TAG`, capability + outbound quota still
    /// honoured. **Not app-layer sealed** — direct sessions already have QUIC
    /// mTLS confidentiality and no untrusted relay on path (room content audio
    /// is sealed; see `send_room_content_audio`).
    ///
    /// The per-frame signature still runs so the receiver has one verification
    /// path for both transports, and so a `pts_us` shift cannot pass as a
    /// legitimate frame.
    pub(super) async fn send_content_audio_datagram(
        &mut self,
        peer_id: &str,
        opus: Vec<u8>,
        pts_us: u64,
    ) {
        let Some(qtx) = self.peers.get(peer_id).and_then(|c| c.quic_out_tx.clone()) else {
            return; // No QUIC (or no peer): drop. Content audio never falls back to WS.
        };

        let sender = self.identity.public_id();
        let seq = self
            .direct_content_audio_seq
            .get(peer_id)
            .copied()
            .unwrap_or(0);
        let conv_id = crate::video::direct_conv_id(&sender, peer_id);
        let signing_bytes = crate::content_audio::content_audio_signing_bytes(
            &conv_id, &sender, seq, pts_us, &opus,
        );
        let sig_vec = self.identity.sign(&signing_bytes);
        let Ok(signature) = <[u8; crate::content_audio::SIGNATURE_LEN]>::try_from(&sig_vec[..])
        else {
            return;
        };

        let Some(frame) =
            crate::content_audio::encode_frame(&sender, seq, pts_us, &signature, &opus)
        else {
            warn!("[core.audio.content.v1] could not encode frame; dropping");
            return;
        };

        // +1 for the channel tag that rides ahead of the frame.
        if !self.check_content_audio_quota(peer_id, frame.len() + 1) {
            debug!(
                "[core.audio.content.v1] outbound quota exceeded for {}; dropping frame",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        }

        self.direct_content_audio_seq
            .insert(peer_id.to_owned(), seq.wrapping_add(1));

        let mut framed = Vec::with_capacity(1 + frame.len());
        framed.push(CONTENT_AUDIO_CHANNEL_TAG);
        framed.extend_from_slice(&frame);
        if qtx.try_send(PeerOutbound::Datagram(framed)).is_err() {
            use super::super::internal::drop_metrics;
            drop_metrics::note(&drop_metrics::PEER_OUTBOUND);
        }
    }

    /// Send one encoded video frame to a directly-connected peer.
    ///
    /// Extends the same documented bypass as
    /// [`send_audio_datagram`](Self::send_audio_datagram) — a dedicated low tag
    /// rather than the generic feature-datagram multiplexer — for the same
    /// real-time reasons, with capability negotiation (`CoreVideoVp8Module`)
    /// and outbound quota still honoured.
    ///
    /// Two differences from the audio path:
    ///
    /// * One frame becomes several datagrams, so it is fragmented first.
    /// * **The frame is not app-layer encrypted**, matching direct audio: on a
    ///   direct session confidentiality comes from the QUIC mTLS channel, and
    ///   there is no untrusted relay in the middle to hide content from. Room
    ///   video, which does traverse a relay, is sealed — see `send_room_video`.
    ///
    /// The per-frame signature is still attached even though mTLS already
    /// authenticates the peer. It is cheap at frame rates (tens of signatures a
    /// second) and it means the receiver runs exactly one verification path
    /// instead of branching on how the frame arrived.
    pub(super) async fn send_video_datagram(
        &mut self,
        peer_id: &str,
        encoded: Vec<u8>,
        keyframe: bool,
        codec: conquerd_features::video_codec::VideoCodec,
        pts_us: u64,
    ) {
        // Clone the sender handle so the `self.peers` borrow ends before the
        // per-peer sequence counter is advanced below.
        let Some(qtx) = self.peers.get(peer_id).and_then(|c| c.quic_out_tx.clone()) else {
            return; // No QUIC (or no peer): drop. Video never falls back to WS.
        };

        let sender = self.identity.public_id();
        let seq = self.direct_video_seq.get(peer_id).copied().unwrap_or(0);
        let conv_id = crate::video::direct_conv_id(&sender, peer_id);
        let signing_bytes = crate::video::video_frame_signing_bytes(
            &conv_id, &sender, seq, codec, pts_us, &encoded,
        );
        let sig_vec = self.identity.sign(&signing_bytes);
        let Ok(signature) = <[u8; crate::video::fragment::SIGNATURE_LEN]>::try_from(&sig_vec[..])
        else {
            return;
        };

        // One byte of channel tag rides ahead of each fragment.
        let budget = crate::video::DEFAULT_MAX_DATAGRAM.saturating_sub(1);
        let Some(fragments) = crate::video::fragment::fragment_frame(
            &sender, seq, keyframe, codec, pts_us, &signature, &encoded, budget,
        ) else {
            warn!(
                "[core.video.v1] frame of {}B does not fit the fragment budget; dropping",
                encoded.len()
            );
            return;
        };

        // Gate the whole frame: a half-sent frame is bandwidth spent on
        // something the receiver is guaranteed to discard.
        let wire_bytes: usize = fragments.iter().map(|f| f.len() + 1).sum();
        if !self.check_video_quota(peer_id, wire_bytes) {
            debug!(
                "[core.video.v1] outbound quota exceeded for {}; dropping frame",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        }

        self.direct_video_seq
            .insert(peer_id.to_owned(), seq.wrapping_add(1));

        for fragment in fragments {
            let mut framed = Vec::with_capacity(1 + fragment.len());
            framed.push(VIDEO_CHANNEL_TAG);
            framed.extend_from_slice(&fragment);
            if qtx.try_send(PeerOutbound::Datagram(framed)).is_err() {
                use super::super::internal::drop_metrics;
                drop_metrics::note(&drop_metrics::PEER_OUTBOUND);
            }
        }
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
    // -----------------------------------------------------------------------
    // NAT hole punching
    // -----------------------------------------------------------------------

    /// Ask a trusted supernode to coordinate a hole punch with `peer_id`.
    ///
    /// This is what makes a direct session possible between two peers who are
    /// both behind NAT and neither of whom has forwarded a port — the common
    /// case, and one that no amount of address advertising fixes on its own,
    /// because neither side has a mapping the other can reach until both send
    /// outward at roughly the same time.
    ///
    /// Registering is a rendezvous rather than a request: the supernode holds
    /// it for 30s waiting for the other side, then hands both peers each
    /// other's observed address and a shared `punch_at`. So it is worth doing
    /// early and exactly once per attempt, which `punch_registered` enforces.
    ///
    /// The endpoint offered here is a best guess. The supernode is expected to
    /// prefer the address it actually observes for us, which is the only one
    /// that is correct behind carrier-grade NAT — there, the router's own
    /// "external" address is itself private. Offering the LAN hint when
    /// nothing better is known still helps two peers on one network.
    pub(in crate::connection_manager) async fn request_hole_punch(&mut self, peer_id: &str) {
        if peer_id.is_empty() || peer_id == self.identity.public_id() {
            return;
        }
        // A registration already in flight has not expired at the supernode
        // yet, so re-sending would only churn its pair table.
        if let Some(at) = self.punch_registered.get(peer_id) {
            if at.elapsed() < PUNCH_REGISTRATION_TTL {
                return;
            }
        }

        let connected: std::collections::HashSet<String> = self
            .supernodes
            .iter()
            .filter(|(_, sn)| sn.connected)
            .map(|(id, _)| id.clone())
            .collect();
        let trusted: Vec<String> = {
            let store = self.peer_store.read();
            store
                .supernodes()
                .iter()
                .map(|r| r.identity_pub.clone())
                .collect()
        };
        let supernode_id = crate::connection_fallback::DirectFallbackCoordinator::pick_supernode(
            trusted.iter().map(String::as_str),
            &connected,
        );
        if supernode_id.is_empty() {
            debug!(
                "[punch] no trusted supernode to coordinate with for {}",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        }

        // Prefer whatever the outside world has already told us about us.
        let sender_endpoint = self
            .public_quic_hint
            .clone()
            .or_else(|| self.local_quic_hint())
            .and_then(|hint| parse_quic_lan_hint(&hint).map(|(h, p)| format!("{h}:{p}")));
        let Some(sender_endpoint) = sender_endpoint else {
            debug!("[punch] no local endpoint to offer yet");
            return;
        };

        let mut msg = SignalingMessage::new(MessageType::PunchRegister, self.identity.public_id());
        msg.target = Some(supernode_id.clone());
        msg.payload
            .insert("target_peer".into(), Value::String(peer_id.to_owned()));
        msg.payload
            .insert("sender_endpoint".into(), Value::String(sender_endpoint));

        info!(
            "[punch] registering with {} to reach {}",
            &supernode_id[..12.min(supernode_id.len())],
            &peer_id[..8.min(peer_id.len())]
        );
        self.punch_registered
            .insert(peer_id.to_owned(), Instant::now());
        self.dispatch_outbound(msg).await;
    }

    /// Both peers registered, so the supernode has handed us the other side's
    /// observed address and a moment to dial it.
    ///
    /// Nothing is dialed here. The entire value of the exchange is that both
    /// peers transmit at nearly the same time, so the dial is deferred to
    /// `punch_at` through [`InternalEvent::PunchNow`].
    pub(in crate::connection_manager) fn handle_punch_ready(&mut self, msg: &SignalingMessage) {
        // Only a supernode we hold a live session with may steer our UDP
        // traffic. Without this, one signed message would be enough to point
        // the client at an arbitrary host and use it as a probe.
        if !self
            .supernodes
            .get(&msg.sender)
            .map(|sn| sn.connected)
            .unwrap_or(false)
        {
            warn!(
                "[punch] PUNCH_READY from non-session sender {} — ignored",
                &msg.sender[..12.min(msg.sender.len())]
            );
            return;
        }

        let peer_id = msg
            .payload
            .get("peer_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if peer_id.is_empty() {
            return;
        }
        // Punching toward someone we have never heard of is not a connection
        // we were trying to make.
        if !self.peer_store.read().contains(&peer_id) {
            warn!(
                "[punch] PUNCH_READY names unknown peer {} — ignored",
                &peer_id[..8.min(peer_id.len())]
            );
            return;
        }

        // Our own mapping as the supernode observes it. This is the only
        // address that is correct behind carrier-grade NAT, so it outranks
        // anything a UPnP gateway reported about itself.
        if let Some(addr) = msg
            .payload
            .get("your_endpoint")
            .and_then(Value::as_str)
            .and_then(parse_punch_endpoint)
        {
            let hint = format!("quic://{}:{}", addr.ip(), addr.port());
            if self.public_quic_hint.as_deref() != Some(hint.as_str()) {
                info!("[punch] observed public address {hint}");
                self.public_quic_hint = Some(hint);
            }
        }

        let Some(target) = msg
            .payload
            .get("peer_endpoint")
            .and_then(Value::as_str)
            .and_then(parse_punch_endpoint)
        else {
            warn!("[punch] PUNCH_READY carried no dialable peer endpoint");
            return;
        };

        // `punch_at` is absolute unix seconds decided by the supernode.
        // Trusting it blindly would let clock skew — or a hostile value — park
        // a dial far in the future, so the wait is clamped.
        let now = super::unix_now_f64();
        let punch_at = msg
            .payload
            .get("punch_at")
            .and_then(Value::as_f64)
            .unwrap_or(now);
        let delay = Duration::from_secs_f64((punch_at - now).clamp(0.0, PUNCH_MAX_DELAY_S));

        let internal_tx = self.internal_tx.clone();
        let host = target.ip().to_string();
        let port = target.port();
        let peer_for_task = peer_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = internal_tx
                .send(InternalEvent::PunchNow {
                    peer_id: peer_for_task,
                    host,
                    port,
                })
                .await;
        });
        debug!(
            "[punch] {} scheduled in {:.0}ms",
            &peer_id[..8.min(peer_id.len())],
            delay.as_secs_f64() * 1000.0
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{is_local_only_host, parse_punch_endpoint, peer_quic_endpoints};
    use crate::peer_store::PeerRecord;

    fn record(hints: &[&str], quic_port: u16) -> PeerRecord {
        PeerRecord {
            peer_id: "peer".to_owned(),
            relay_hints: hints.iter().map(|h| (*h).to_owned()).collect(),
            quic_port,
            ..Default::default()
        }
    }

    /// The regression this ordering exists for: loopback used to be the sole
    /// fallback, so a peer with a real address but no parseable hint was
    /// dialed at 127.0.0.1 forever.
    #[test]
    fn a_real_address_outranks_loopback() {
        let rec = record(&["quic://203.0.113.7:9000"], 9000);
        let got = peer_quic_endpoints(&rec);
        assert_eq!(got.first(), Some(&("203.0.113.7".to_owned(), 9000)));
        assert_eq!(
            got.last(),
            Some(&("127.0.0.1".to_owned(), 9000)),
            "loopback stays available for two profiles on one machine"
        );
    }

    /// Both hints are kept: which one works depends on where the dial is made
    /// from, and that is not knowable here.
    #[test]
    fn lan_and_public_hints_are_both_offered_lan_first() {
        let rec = record(
            &["quic://192.168.1.5:9000", "quic://203.0.113.7:9000"],
            9000,
        );
        let got = peer_quic_endpoints(&rec);
        assert_eq!(
            got[..2],
            [
                ("192.168.1.5".to_owned(), 9000),
                ("203.0.113.7".to_owned(), 9000)
            ]
        );
    }

    #[test]
    fn duplicate_hints_are_collapsed() {
        let rec = record(&["quic://192.168.1.5:9000", "quic://192.168.1.5:9000"], 0);
        assert_eq!(peer_quic_endpoints(&rec).len(), 1);
    }

    #[test]
    fn a_peer_with_nothing_known_offers_nothing() {
        assert!(peer_quic_endpoints(&record(&[], 0)).is_empty());
    }

    /// Drives the "should we spend a hole punch?" decision, so a wrong answer
    /// here either wastes a round trip or strands the peer forever.
    #[test]
    fn only_loopback_and_wildcard_count_as_local_only() {
        assert!(is_local_only_host("127.0.0.1"));
        assert!(is_local_only_host("0.0.0.0"));
        assert!(is_local_only_host("::1"));
        assert!(!is_local_only_host("192.168.1.5"));
        assert!(!is_local_only_host("203.0.113.7"));
        // Not an IP at all — a hostname is something we can still dial.
        assert!(!is_local_only_host("peer.example"));
    }

    #[test]
    fn punch_endpoints_that_would_make_us_a_probe_are_refused() {
        assert!(parse_punch_endpoint("127.0.0.1:9000").is_none());
        assert!(parse_punch_endpoint("0.0.0.0:9000").is_none());
        assert!(parse_punch_endpoint("203.0.113.7:0").is_none());
        assert!(parse_punch_endpoint("239.1.1.1:9000").is_none());
        assert!(parse_punch_endpoint("not-an-address").is_none());
        assert!(parse_punch_endpoint("").is_none());
    }

    #[test]
    fn an_ordinary_punch_endpoint_parses() {
        let got = parse_punch_endpoint(" 203.0.113.7:9000 ").expect("valid");
        assert_eq!(got.ip().to_string(), "203.0.113.7");
        assert_eq!(got.port(), 9000);
    }
}
