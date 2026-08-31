//! Outbound routing: path pick, multi-supernode fan-out, EncryptedSignal wrap.

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

use super::unix_now_f64;

/// Whether a signed, peer-targeted message that missed direct QUIC should fan
/// out across every connected supernode WS session.
///
/// Multi-homed recipients may only be live on a subset of cluster members; a
/// first-only delivery can land on a node where the peer is not attached and
/// strand chat, call control, file chunks, or group-key envelopes. Fan-out is
/// safe: supernodes forward only if the peer is connected there, and inbound
/// paths are idempotent (signature/`message_id` / transfer state).
///
/// `target_is_supernode_session` is true when `target` resolves to one of our
/// live supernode session keys — those messages must stay single-homed.
pub fn should_fanout_peer_relay(has_target: bool, target_is_supernode_session: bool) -> bool {
    has_target && !target_is_supernode_session
}

impl ConnectionManager {
    /// Fixed first-party channel tag for a message type, if it rides a
    /// dedicated channel rather than the control/signaling channel.
    ///
    /// Channel tag for a message on the QUIC peer stream. Chat and file ride
    /// dedicated tags; everything else uses the control channel tag.
    pub(super) fn channel_tag_for(msg_type: MessageType) -> u8 {
        match msg_type {
            MessageType::ChatMessage | MessageType::ChatAck | MessageType::ChatTyping => {
                channel_frame::CHAT_TAG
            }
            MessageType::FileTransferOffer
            | MessageType::FileTransferAccept
            | MessageType::FileTransferReject
            | MessageType::FileTransferChunk
            | MessageType::FileTransferComplete
            | MessageType::FileTransferAck
            | MessageType::FileTransferError => channel_frame::FILE_TAG,
            _ => channel_frame::CONTROL_TAG,
        }
    }

    /// Supernode-targeted room broadcast messages that may ride the reliable
    /// QUIC relay signaling stream (`room.chat.v1` / `room.file.v1`) instead of
    /// the WebSocket signaling path. `SfuAudio` is excluded — it rides the
    /// unreliable relay datagram path.
    pub(super) fn is_relay_signaling_type(msg_type: &MessageType) -> bool {
        matches!(
            msg_type,
            MessageType::SfuChat
                | MessageType::SfuFileOffer
                | MessageType::SfuFileRequest
                | MessageType::SfuFileRevoke
                | MessageType::SfuFileChunk
                | MessageType::SfuFileComplete
        )
    }

    /// Chunks and COMPLETE must stay on one transport. Falling back from a
    /// full QUIC queue onto WebSocket lets the small COMPLETE overtake the
    /// payload, so the sender hits 100% while the receiver is still missing
    /// most of the file.
    pub(crate) fn is_ordered_file_payload(msg_type: &MessageType) -> bool {
        matches!(
            msg_type,
            MessageType::FileTransferChunk
                | MessageType::FileTransferComplete
                | MessageType::SfuFileChunk
                | MessageType::SfuFileComplete
        )
    }

    /// Returns `true` when the frame was handed to a transport. `false` means
    /// it was not sent (quota, no path, serialize error) and a file pump must
    /// retry rather than skip ahead.
    pub(in crate::connection_manager) async fn dispatch_outbound(
        &mut self,
        mut msg: SignalingMessage,
    ) -> bool {
        let chat_attempt = if msg.msg_type == MessageType::ChatMessage {
            msg.target.clone().and_then(|peer_id| {
                msg.payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .map(|message_id| (peer_id, message_id.to_owned()))
            })
        } else {
            None
        };
        if let Some((peer_id, message_id)) = chat_attempt.clone() {
            let direct_connected = self
                .peers
                .get(&peer_id)
                .map(|peer| peer.state == PeerConnectionState::Connected)
                .unwrap_or(false);
            if !direct_connected {
                // No direct QUIC peer session. Fall back to supernode relay
                // when a supernode WS session is connected: the supernode
                // forwards peer-targeted messages to the destination if it is
                // also connected there (see signaling.rs "Relay to target").
                // The recipient still verifies signature + replay + blocked
                // sender. For paired peers the relayed payload is wrapped in an
                // `EncryptedSignal` envelope (see `maybe_wrap_for_relay`), so the
                // supernode sees only opaque ciphertext + routing metadata; it
                // can neither read nor forge the 1:1 content.
                let relay_available = self.supernodes.values().any(|sn| sn.connected);
                if !relay_available {
                    warn!(
                        "No direct session or supernode relay for chat {} to {}",
                        message_id,
                        &peer_id[..8.min(peer_id.len())]
                    );
                    self.emit_event(ConnectionEvent::ChatSendFailed {
                        peer_id,
                        message_id,
                        reason: "peer is offline".to_owned(),
                    });
                    return false;
                }
                debug!(
                    "No direct session for chat {}; relaying to {} via supernode",
                    message_id,
                    &peer_id[..8.min(peer_id.len())]
                );
                // Fall through: quota gating + signing happen below, then the
                // supernode WS fallback route delivers the signed message.
            }
        }

        // Gate outbound chat and file messages through their feature quota
        // before signing/sending.  This is symmetric with the inbound quota
        // enforcement in QuotaRegistry::try_consume that applies to inbound
        // messages dispatched via FeatureRegistry::dispatch_message.
        if let Some(ref target) = msg.target.clone() {
            let feature_gate = match msg.msg_type {
                // core.chat.v1 covers text chat and related control messages.
                MessageType::ChatMessage | MessageType::ChatAck | MessageType::ChatTyping => {
                    Some("core.chat.v1")
                }
                // core.file.v1 covers the full file-transfer handshake.
                MessageType::FileTransferOffer
                | MessageType::FileTransferAccept
                | MessageType::FileTransferReject
                | MessageType::FileTransferChunk
                | MessageType::FileTransferComplete
                | MessageType::FileTransferAck
                | MessageType::FileTransferError => Some("core.file.v1"),
                MessageType::SfuFileOffer
                | MessageType::SfuFileRequest
                | MessageType::SfuFileRevoke
                | MessageType::SfuFileChunk
                | MessageType::SfuFileComplete => Some("room.file.v1"),
                // room.chat.v1 covers SFU room text chat broadcast.
                MessageType::SfuChat => Some("room.chat.v1"),
                _ => None,
            };
            if let Some(fid) = feature_gate {
                // Estimate outbound byte cost from the payload values so we
                // don't have to re-serialize the whole message.  A floor of
                // 64 bytes ensures a non-trivially small message still
                // consumes tokens (prevents quota-bypass via empty messages).
                let byte_est: usize = msg
                    .payload
                    .values()
                    .filter_map(|v| v.as_str())
                    .map(str::len)
                    .sum::<usize>()
                    .max(64);
                if !self
                    .feature_registry
                    .gate_through_feature(fid, target, byte_est)
                {
                    warn!(
                        "[gate_through_feature] {} outbound quota exceeded for {}; dropping {:?}",
                        fid,
                        &target[..8.min(target.len())],
                        msg.msg_type,
                    );
                    if let Some((peer_id, message_id)) = chat_attempt {
                        self.emit_event(ConnectionEvent::ChatSendFailed {
                            peer_id,
                            message_id,
                            reason: "quota exceeded".to_owned(),
                        });
                    }
                    return false;
                }
            }
        }

        // Sign the message
        let canonical = match msg.canonical_bytes() {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to canonicalize message for signing: {}", e);
                return false;
            }
        };
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(&sig));

        let msg_type = msg.msg_type.clone();

        let json = match msg.to_json() {
            Ok(j) => j,
            Err(e) => {
                error!("Failed to serialize message: {}", e);
                if let Some((peer_id, message_id)) = chat_attempt {
                    self.emit_event(ConnectionEvent::ChatSendFailed {
                        peer_id,
                        message_id,
                        reason: "message serialization failed".to_owned(),
                    });
                }
                return false;
            }
        };

        // For the supernode-relay fallback, wrap peer-targeted messages in a
        // signed `EncryptedSignal` envelope so the relaying supernode cannot
        // read the payload. The direct-QUIC lane (below) is already private and
        // always uses the plaintext `json`; `relay_json` is used only on the
        // supernode-WS relay routes. Falls back to plaintext when no pairwise
        // key is derivable (supernode target, not-yet-paired peer, broadcast).
        let relay_json = self
            .maybe_wrap_for_relay(&msg, &json)
            .unwrap_or_else(|| json.clone());

        // Route: QUIC direct > relay WS > supernode WS fallback
        if let Some(target) = &msg.target.clone() {
            // Clone the sender so we don't hold a borrow of `self.peers`
            // while emitting a failure event on `self.event_tx` below.
            let quic_out_tx = self.peers.get(target).and_then(|peer| {
                if peer.state == PeerConnectionState::Connected {
                    peer.quic_out_tx.clone()
                } else {
                    None
                }
            });
            if let Some(out_tx) = quic_out_tx {
                // Every peer-stream frame is tagged: chat/file dedicated
                // tags, control for all other signaling.
                let tag = Self::channel_tag_for(msg_type.clone());
                let bytes = channel_frame::encode_frame(tag, json.as_bytes());
                if out_tx.try_send(PeerOutbound::Reliable(bytes)).is_ok() {
                    return true;
                }
                if Self::is_ordered_file_payload(&msg_type) {
                    // Backpressure the pump instead of splitting the transfer
                    // across QUIC and the supernode WS fallback.
                    return false;
                }
                // Full or closing QUIC channel: fall back to supernode relay
                // when available so chat / call / file are not stranded.
                if self.supernodes.values().any(|sn| sn.connected) {
                    debug!(
                        "QUIC signaling channel busy for {:?}; falling back to supernode relay",
                        msg_type
                    );
                } else {
                    if let Some((peer_id, message_id)) = chat_attempt {
                        warn!(
                            "QUIC signaling channel unavailable for chat {} to {}",
                            message_id,
                            &peer_id[..8.min(peer_id.len())]
                        );
                        self.emit_event(ConnectionEvent::ChatSendFailed {
                            peer_id,
                            message_id,
                            reason: "connection busy".to_owned(),
                        });
                    }
                    return false;
                }
            }
        }

        // Route supernode-targeted signaling to that supernode's WS session.
        // Without this, multi-supernode clients always hit the first connected
        // session — room creates/lists from SN-B would land on SN-A instead.
        if let Some(target) = msg.target.clone() {
            if let Some(sn_id) = self.resolve_supernode_ws_target(&target) {
                // Reliable room broadcasts (room.chat.v1 / room.file.v1) prefer
                // the QUIC relay signaling stream when a live relay session
                // exists — no TCP head-of-line blocking. Falls through to the
                // WebSocket route below if the stream is unavailable/backed up.
                if Self::is_relay_signaling_type(&msg_type) {
                    if let Some(relay) = self.quic_relays.get(&sn_id).filter(|r| r.is_alive()) {
                        if relay.send_signaling(json.as_bytes()) {
                            return true;
                        }
                        if relay.signaling_usable() && Self::is_ordered_file_payload(&msg_type) {
                            // Stream is up but the 512-frame queue is full.
                            // Retry here. Do not send COMPLETE over WS while
                            // chunks are still queued on QUIC.
                            return false;
                        }
                        // Portal-only, dead signaling stream, or back-pressure:
                        // fall through to WebSocket so room chat/file is never
                        // silently lost on a guest or half-dead relay.
                        debug!(
                            "[room] relay signaling unavailable for {:?} via {}…; using WS",
                            msg_type,
                            &sn_id[..12.min(sn_id.len())]
                        );
                    }
                }
                match self.supernodes.get(&sn_id) {
                    Some(sn) if sn.connected => {
                        if sn.send_tx.try_send(WsMessage::Text(json.clone())).is_err() {
                            self.note_ws_outbound_drop("supernode-targeted signaling");
                            if Self::is_relay_signaling_type(&msg_type) {
                                warn!(
                                    "[room] WS send failed for {:?} via {}… — frame dropped",
                                    msg_type,
                                    &sn_id[..12.min(sn_id.len())]
                                );
                            }
                            return false;
                        }
                        return true;
                    }
                    _ => {
                        warn!(
                            "Supernode {} not connected; dropping {:?}",
                            &sn_id[..8.min(sn_id.len())],
                            msg_type
                        );
                        if let Some((peer_id, message_id)) = chat_attempt {
                            self.emit_event(ConnectionEvent::ChatSendFailed {
                                peer_id,
                                message_id,
                                reason: "supernode is offline".to_owned(),
                            });
                        }
                        return false;
                    }
                }
            }
        }

        // Fall back: deliver via supernode WebSocket relay (path for untargeted
        // or peer-targeted messages that missed QUIC).
        //
        // Peer-targeted traffic fans out to every connected supernode: we do
        // not know which node the recipient is attached to, and multi-homed
        // peers may only be live on a subset of the cluster. Each supernode
        // forwards only if the peer is connected there; the rest drop it.
        // Inbound paths are idempotent (chat `message_id`, transfer state,
        // signature/`ReplayGuard`), so duplicate delivery is safe.
        //
        // Untargeted / broadcast messages still use first-successful delivery.
        let target_is_supernode = msg
            .target
            .as_ref()
            .and_then(|t| self.resolve_supernode_ws_target(t))
            .is_some();
        let peer_relay_fanout = should_fanout_peer_relay(msg.target.is_some(), target_is_supernode);
        if peer_relay_fanout {
            let mut delivered_any = false;
            let mut fanout_drops = 0u32;
            for sn in self.supernodes.values() {
                if sn.connected {
                    if sn
                        .send_tx
                        .try_send(WsMessage::Text(relay_json.clone()))
                        .is_ok()
                    {
                        delivered_any = true;
                    } else {
                        fanout_drops += 1;
                    }
                }
            }
            for _ in 0..fanout_drops {
                self.note_ws_outbound_drop("peer relay fan-out");
            }
            if delivered_any {
                return true;
            }
            warn!("No connected supernode accepted relay {:?}", msg_type);
            if let Some((peer_id, message_id)) = chat_attempt {
                self.emit_event(ConnectionEvent::ChatSendFailed {
                    peer_id,
                    message_id,
                    reason: "peer is offline".to_owned(),
                });
            }
            return false;
        }

        // Untargeted broadcasts: first connected supernode that accepts.
        for sn in self.supernodes.values() {
            if sn.connected {
                // If this supernode's outbound queue is full or closed, try
                // the next connected supernode rather than dropping silently.
                if sn
                    .send_tx
                    .try_send(WsMessage::Text(relay_json.clone()))
                    .is_ok()
                {
                    return true;
                }
            }
        }
        warn!("No connected path to deliver message {:?}", msg_type);
        false
    }

    /// Wrap a signed, peer-targeted `inner` message in a signed
    /// `EncryptedSignal` envelope for the supernode-relay fallback, so the
    /// relaying supernode routes by envelope `target` only and never sees the
    /// payload. `inner_json` is the already-serialized plaintext form (reused to
    /// avoid re-serializing).
    ///
    /// Returns `None` (caller falls back to plaintext) when there is no pairwise
    /// key to use: the message is untargeted, the target is a supernode (the
    /// intended recipient), the peer is not yet in the local store, or `inner`
    /// is itself an envelope.
    pub(super) fn maybe_wrap_for_relay(
        &self,
        inner: &SignalingMessage,
        inner_json: &str,
    ) -> Option<String> {
        if inner.msg_type == MessageType::EncryptedSignal {
            return None;
        }
        let target = inner.target.as_ref()?;
        let peer_identity_pub = {
            let store = self.peer_store.read();
            // Never encrypt toward a supernode — it is the recipient, not a relay.
            if store.is_supernode_id(target) {
                return None;
            }
            store
                .get(target)
                .or_else(|| store.get_by_identity(target))
                .map(|r| r.identity_pub.clone())?
        };
        if peer_identity_pub.is_empty() {
            return None;
        }
        let key = self
            .identity
            .derive_pairwise_relay_key(&peer_identity_pub)
            .ok()?;
        let ciphertext = crate::crypto::encrypt_blob(&key, inner_json.as_bytes()).ok()?;
        let ciphertext_b64 = crate::crypto::b64url_encode(&ciphertext);

        let mut env =
            SignalingMessage::new(MessageType::EncryptedSignal, self.identity.public_id());
        // Route by the same target the plaintext message would have used.
        env.target = inner.target.clone();
        env.payload
            .insert("ciphertext".to_owned(), Value::String(ciphertext_b64));
        let canonical = env.canonical_bytes().ok()?;
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        env.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(&sig));
        env.to_json().ok()
    }

    /// Sign `msg` in place (Ed25519 over its canonical bytes) and return the
    /// serialized JSON, mirroring the signing step in [`Self::dispatch_outbound`].
    /// Returns `None` if canonicalization or serialization fails.
    pub(super) fn sign_message_json(&self, msg: &mut SignalingMessage) -> Option<String> {
        let canonical = msg.canonical_bytes().ok()?;
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        msg.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        msg.to_json().ok()
    }

    /// Resolve a signaling `target` to a **live** `supernodes` session key.
    ///
    /// When the invite / requested member is offline but a verified cluster
    /// sibling still has a connected session (eager multi-home), rewrite the
    /// target so room join/list/chat keep working after the original host dies.
    /// Without this, UI room ops keep targeting the invite supernode and are
    /// silently dropped once it goes down — voice counts freeze even though
    /// the client is multi-homed to a live sibling.
    pub(super) fn resolve_supernode_ws_target(&self, target: &str) -> Option<String> {
        let preferred = self.resolve_supernode_session_key(target)?;
        if self
            .supernodes
            .get(&preferred)
            .is_some_and(|sn| sn.connected)
        {
            return Some(preferred);
        }
        if let Some(live) = self.live_cluster_sibling_session(&preferred) {
            info!(
                "cluster: rewriting offline supernode target {} → live sibling {}",
                &preferred[..12.min(preferred.len())],
                &live[..12.min(live.len())]
            );
            return Some(live);
        }
        // No live sibling — keep the preferred key so the caller logs a clear
        // "not connected" drop for the intended host.
        Some(preferred)
    }

    /// Map `target` to a key in `self.supernodes` without requiring the session
    /// to be connected (session may exist but be down mid-reconnect).
    fn resolve_supernode_session_key(&self, target: &str) -> Option<String> {
        if self.supernodes.contains_key(target) {
            return Some(target.to_owned());
        }
        let bare = target.trim_end_matches('=');
        if bare != target {
            if let Some((k, _)) = self
                .supernodes
                .iter()
                .find(|(k, _)| k.trim_end_matches('=') == bare)
            {
                return Some(k.clone());
            }
        }
        let canon = self
            .peer_store
            .read()
            .resolve_supernode_identity_pub(target)?;
        if self.supernodes.contains_key(&canon) {
            return Some(canon);
        }
        let canon_bare = canon.trim_end_matches('=');
        self.supernodes
            .keys()
            .find(|k| k.trim_end_matches('=') == canon_bare)
            .cloned()
    }

    /// A connected session for a verified cluster sibling of `offline_id`, if any.
    fn live_cluster_sibling_session(&self, offline_id: &str) -> Option<String> {
        let offline_bare = offline_id.trim_end_matches('=');
        let mut member_ids: Vec<String> = Vec::new();
        for (key, members) in &self.cluster_members {
            let key_bare = key.trim_end_matches('=');
            let related = key_bare == offline_bare
                || members
                    .iter()
                    .any(|m| m.identity_pub.trim_end_matches('=') == offline_bare);
            if !related {
                continue;
            }
            member_ids.push(key.clone());
            for m in members {
                member_ids.push(m.identity_pub.clone());
            }
        }
        if member_ids.is_empty() {
            return None;
        }
        for candidate in member_ids {
            let cand_bare = candidate.trim_end_matches('=');
            if cand_bare == offline_bare {
                continue;
            }
            for (sid, sn) in &self.supernodes {
                if !sn.connected {
                    continue;
                }
                if sid.trim_end_matches('=') == cand_bare {
                    return Some(sid.clone());
                }
            }
        }
        None
    }
}
