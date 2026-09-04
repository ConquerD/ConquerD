//! Inbound signaling verification and message dispatch.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use conquerd_features::{AuthTier, CapabilityDescriptor, InvocationContext};
use parking_lot::RwLock;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::avatar_config::AvatarConfig as PeerAvatarConfig;
use crate::feature_trust::{FeatureTrustGate, TrustDecision};
use crate::file_transfer::{TransferEvent, ROOM_FILE_CHUNK_BUDGET};
use crate::peer_store::PeerStore;
use crate::protocol::{MessageType, SignalingMessage};

use super::super::events::ConnectionEvent;
use super::super::internal::PeerConnectionState;
use super::ConnectionManager;

use super::{
    may_send_room_e2e_content, parse_quic_lan_hint, room_scope_key,
    should_auto_join_on_room_created, unix_now_f64, PendingRoomJoinRetry,
};

/// Pad-tolerant supernode id compare (URL-safe base64 with/without `=`).
fn same_supernode_pad(a: &str, b: &str) -> bool {
    a == b || a.trim_end_matches('=') == b.trim_end_matches('=')
}

impl ConnectionManager {
    /// Clear any pending private-join keys for this room (exact host or any
    /// cluster sibling). Returns whether a pending entry existed.
    fn take_pending_private_room_join(&mut self, answering_host: &str, room_id: &str) -> bool {
        Self::take_pending_scoped_key(
            &mut self.pending_private_room_joins,
            answering_host,
            room_id,
        )
    }

    /// Consume one pending materialize-only create for this room (exact host,
    /// a pad variant, or any cluster sibling). Without room-id matching, a
    /// CreateRoom sent to B/C after rewrite misses `pending_materialize` and
    /// fires `RoomCreated` + tray "Private room created" spam on every
    /// rematerialize. Decrements by one rather than clearing the whole entry —
    /// rematerialize can legitimately fire twice for the same key (connect +
    /// a racing cluster-roster update) before either reply lands, and each
    /// `SfuRoomCreated` must independently see "yes, this was materialize-only".
    pub(super) fn take_pending_materialize(&mut self, answering_host: &str, room_id: &str) -> bool {
        let exact = format!("{answering_host}:{room_id}");
        if Self::decrement_pending(&mut self.pending_materialize, &exact) {
            return true;
        }
        let bare = answering_host.trim_end_matches('=');
        if bare != answering_host {
            let bare_key = format!("{bare}:{room_id}");
            if Self::decrement_pending(&mut self.pending_materialize, &bare_key) {
                return true;
            }
        }
        let suffix = format!(":{room_id}");
        let Some(key) = self
            .pending_materialize
            .keys()
            .find(|k| k.ends_with(&suffix))
            .cloned()
        else {
            return false;
        };
        Self::decrement_pending(&mut self.pending_materialize, &key)
    }

    /// Decrement `key`'s pending count by one, removing the entry once it
    /// reaches zero. Returns whether `key` had a positive count to consume.
    fn decrement_pending(map: &mut HashMap<String, u32>, key: &str) -> bool {
        match map.get_mut(key) {
            Some(count) if *count > 1 => {
                *count -= 1;
                true
            }
            Some(_) => {
                map.remove(key);
                true
            }
            None => false,
        }
    }

    fn take_pending_scoped_key(
        set: &mut HashSet<String>,
        answering_host: &str,
        room_id: &str,
    ) -> bool {
        let exact = format!("{answering_host}:{room_id}");
        let mut hit = set.remove(&exact);
        let bare = answering_host.trim_end_matches('=');
        if bare != answering_host {
            hit |= set.remove(&format!("{bare}:{room_id}"));
        }
        let suffix = format!(":{room_id}");
        let extras: Vec<String> = set
            .iter()
            .filter(|k| k.ends_with(&suffix))
            .cloned()
            .collect();
        if !extras.is_empty() {
            hit = true;
            for k in extras {
                set.remove(&k);
            }
        }
        hit
    }

    pub(super) async fn handle_inbound_from_quic(
        &mut self,
        transport_peer_id: String,
        msg: SignalingMessage,
    ) {
        self.handle_inbound_inner(msg, Some(transport_peer_id), None)
            .await;
    }

    #[cfg(test)]
    pub(in crate::connection_manager) async fn handle_inbound(&mut self, msg: SignalingMessage) {
        self.handle_inbound_inner(msg, None, None).await;
    }

    pub(in crate::connection_manager) async fn handle_inbound_from_supernode(
        &mut self,
        supernode_id: String,
        msg: SignalingMessage,
    ) {
        self.handle_inbound_inner(msg, None, Some(supernode_id))
            .await;
    }

    pub(super) async fn handle_inbound_inner(
        &mut self,
        msg: SignalingMessage,
        quic_peer_id: Option<String>,
        inbound_supernode_id: Option<String>,
    ) {
        // Enforce signed-transcript model: every inbound signaling message
        // MUST carry a valid Ed25519 signature over its canonical bytes,
        // signed by the key whose public_id is `msg.sender`. Drop silently
        // (with a warning) on any failure — never dispatch unverified data.
        if !Self::verify_inbound_signature(&msg) {
            warn!(
                "[signaling] dropping {:?} from {} — signature missing or invalid",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        // Sliding-window replay guard: reject re-delivery of an already-seen
        // signed message within the freshness window. Runs only after the
        // signature + freshness checks above have passed.
        //
        // Exempt from signature dedup *only* — signature verification,
        // freshness and the per-feature byte quotas all still apply:
        //
        //  * Real-time audio (SfuAudio, ~50 Hz): ephemeral, already covered by
        //    the freshness window and the jitter buffer.
        //  * Bulk file payload: idempotent at the receiver (a duplicate chunk
        //    index is discarded, and COMPLETE only acts on a transfer still
        //    `Transferring`), and high-rate enough to fill the per-sender
        //    window — which `ReplayGuard` fails closed on, taking this peer's
        //    chat and call control down with the transfer.
        let dedup_exempt =
            msg.msg_type == MessageType::SfuAudio || Self::is_ordered_file_payload(&msg.msg_type);
        if !dedup_exempt && !self.check_replay(&msg) {
            warn!(
                "[signaling] dropping {:?} from {} — replayed message",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        // Positive mutual-trust gate for chat/call/file-class signaling. These
        // message types are only honoured from peers we already trust (present
        // in the local store, not revoked/blocked). This both closes the
        // blocked-peer hole and — crucially — bounds the supernode relay
        // fallback: a supernode forwards anything peer-targeted, so without a
        // receiver-side trust check an untrusted peer sharing the same
        // supernode could inject chat/call/file messages. With it, relay assist
        // works *only* between two mutually-trusted peers.
        if matches!(
            msg.msg_type,
            MessageType::ChatMessage
                | MessageType::ChatAck
                | MessageType::ChatTyping
                | MessageType::CallRequest
                | MessageType::CallAccept
                | MessageType::CallReject
                | MessageType::CallEnd
                | MessageType::FileTransferOffer
                | MessageType::FileTransferAccept
                | MessageType::FileTransferReject
                | MessageType::FileTransferChunk
                | MessageType::FileTransferComplete
                | MessageType::FileTransferAck
                | MessageType::FileTransferError
        ) && !Self::is_trusted_sender(&self.peer_store, &msg.sender)
        {
            warn!(
                "[signaling] dropping {:?} from untrusted or blocked peer {}",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return;
        }
        match msg.msg_type {
            // Supernode-relay E2E envelope: decrypt with the pairwise key derived
            // from our identity + the envelope sender's identity (`msg.sender`),
            // then re-dispatch the inner message through the full pipeline (its
            // own signature, freshness, replay, trust, and quota checks all run
            // again). Only the two paired peers can decrypt; a forged or foreign
            // envelope fails decryption and is dropped. The outer envelope has
            // already passed signature/freshness/replay above.
            MessageType::EncryptedSignal => {
                let Some(ciphertext_b64) = msg.payload.get("ciphertext").and_then(Value::as_str)
                else {
                    warn!(
                        "[signaling] EncryptedSignal from {} missing ciphertext — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                let key = match self.identity.derive_pairwise_relay_key(&msg.sender) {
                    Ok(k) => k,
                    Err(e) => {
                        warn!(
                            "[signaling] EncryptedSignal from {} — key derivation failed: {e}",
                            &msg.sender[..8.min(msg.sender.len())],
                        );
                        return;
                    }
                };
                let Ok(ciphertext) = crate::crypto::b64url_decode(ciphertext_b64) else {
                    warn!(
                        "[signaling] EncryptedSignal from {} — malformed ciphertext — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                let Ok(inner_bytes) = crate::crypto::decrypt_blob(&key, &ciphertext) else {
                    warn!(
                        "[signaling] EncryptedSignal from {} — could not decrypt (not a paired peer?) — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                let Some(inner) = std::str::from_utf8(&inner_bytes)
                    .ok()
                    .and_then(|s| SignalingMessage::from_json(s).ok())
                else {
                    warn!(
                        "[signaling] EncryptedSignal from {} — inner payload not a valid message — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                };
                // Depth guard: a single layer only — never unwrap a nested envelope.
                if inner.msg_type == MessageType::EncryptedSignal {
                    warn!(
                        "[signaling] nested EncryptedSignal from {} — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                }
                // The envelope author must be the inner message's author; this
                // stops a paired peer from relaying a third party's signed
                // message wrapped under their own envelope.
                if inner.sender != msg.sender {
                    warn!(
                        "[signaling] EncryptedSignal inner/outer sender mismatch from {} — dropped",
                        &msg.sender[..8.min(msg.sender.len())],
                    );
                    return;
                }
                Box::pin(self.handle_inbound_inner(inner, quic_peer_id, inbound_supernode_id))
                    .await;
            }
            MessageType::Pong => {
                debug!("Pong from {}", msg.sender);
                self.record_supernode_pong(&msg.sender);
            }
            MessageType::PunchReady => {
                self.handle_punch_ready(&msg);
            }
            MessageType::ChatMessage => {
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                // Approximate payload size for the chat-feature quota.
                let payload_size = msg
                    .payload
                    .get("body")
                    .and_then(Value::as_str)
                    .map(str::len)
                    .unwrap_or(0);
                let probe = vec![0u8; payload_size];
                if !self.gate_through_feature("core.chat.v1", &msg.sender, &probe) {
                    return;
                }
                let body = msg
                    .payload
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let msg_id = msg
                    .payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let handle = msg
                    .payload
                    .get("sender_handle")
                    .or_else(|| msg.payload.get("handle"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !msg_id.is_empty() {
                    let mut ack =
                        SignalingMessage::new(MessageType::ChatAck, self.identity.public_id());
                    ack.target = Some(sender_peer_id.clone());
                    ack.payload
                        .insert("message_id".to_string(), Value::String(msg_id.clone()));
                    self.dispatch_outbound(ack).await;
                }
                self.emit_event(ConnectionEvent::ChatMessage {
                    peer_id: sender_peer_id,
                    message_id: msg_id,
                    body,
                    timestamp: msg.timestamp,
                    sender_handle: handle,
                });
            }
            MessageType::ChatAck => {
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                // Tiny payload — use a minimal probe so chat-ack stays under
                // the same quota umbrella as chat messages.
                if !self.gate_through_feature("core.chat.v1", &msg.sender, &[]) {
                    return;
                }
                let msg_id = msg
                    .payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                self.emit_event(ConnectionEvent::ChatAck {
                    peer_id: sender_peer_id,
                    message_id: msg_id,
                });
            }
            MessageType::CallRequest => {
                // A caller that could not open direct QUIC includes temp
                // private-room coordinates (`fallback_*`); the callee joins
                // that room on accept instead of waiting for a P2P path.
                let field = |key: &str| {
                    msg.payload
                        .get(key)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned()
                };
                self.emit_event(ConnectionEvent::CallRequest {
                    peer_id: self.canonical_peer_id_for_sender(&msg.sender),
                    fallback_supernode_id: field("fallback_supernode_id"),
                    fallback_room_id: field("fallback_room_id"),
                    fallback_invite_token: field("fallback_invite_token"),
                });
            }
            MessageType::CallAccept => {
                let peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                // Caller side: the callee accepted, but real-time audio needs a
                // direct QUIC session. Arm a grace-period check — if none forms
                // in time, fall back to a temporary private SFU room.
                let direct_connected = self
                    .peers
                    .get(&peer_id)
                    .map(|p| p.state == PeerConnectionState::Connected)
                    .unwrap_or(false);
                if !direct_connected && !self.direct_fallback.is_pending_for(&peer_id) {
                    self.pending_call_fallback_checks.insert(
                        peer_id.clone(),
                        std::time::Instant::now()
                            + std::time::Duration::from_secs(super::DIRECT_CALL_FALLBACK_GRACE_S),
                    );
                }
                self.emit_event(ConnectionEvent::CallAccepted { peer_id });
            }
            MessageType::CallEnd | MessageType::CallReject => {
                let peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                // Call over — drop any in-flight direct-call fallback for them.
                if self.direct_fallback.is_pending_for(&peer_id) {
                    self.direct_fallback.cancel();
                }
                self.pending_call_fallback_checks.remove(&peer_id);
                self.emit_event(ConnectionEvent::CallEnded { peer_id });
            }
            MessageType::ChatTyping => {
                if !self.gate_through_feature("core.chat.v1", &msg.sender, &[]) {
                    return;
                }
                let is_typing = msg
                    .payload
                    .get("typing")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.emit_event(ConnectionEvent::TypingIndicator {
                    peer_id: self.canonical_peer_id_for_sender(&msg.sender),
                    is_typing,
                });
            }
            MessageType::HandleUpdate => {
                let handle = msg
                    .payload
                    .get("handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                if !handle.is_empty() {
                    // Persist updated handle in peer store (peer_id or identity_pub key).
                    let mut store = self.peer_store.write();
                    let key = if store.get(&sender_peer_id).is_some() {
                        Some(sender_peer_id.clone())
                    } else {
                        store
                            .get_by_identity(&msg.sender)
                            .map(|r| r.peer_id.clone())
                    };
                    if let Some(pid) = key {
                        if let Some(rec) = store.get_mut(&pid) {
                            rec.handle = handle.clone();
                        }
                        let _ = store.save();
                    }
                    drop(store);
                }
                self.emit_event(ConnectionEvent::HandleUpdated {
                    peer_id: sender_peer_id,
                    handle,
                });
            }
            MessageType::AvatarConfig => {
                if !Self::is_trusted_sender(&self.peer_store, &msg.sender) {
                    return;
                }
                let sender_peer_id = self.canonical_peer_id_for_sender(&msg.sender);
                // Deserialize from the "config" sub-object in the payload.
                if let Some(cfg_val) = msg.payload.get("config") {
                    if let Ok(cfg) = serde_json::from_value::<PeerAvatarConfig>(cfg_val.clone()) {
                        let mut store = self.peer_store.write();
                        if let Some(rec) = store.get_mut(&sender_peer_id) {
                            rec.avatar_config = Some(cfg);
                        }
                        let _ = store.save();
                        drop(store);
                        self.emit_event(ConnectionEvent::AvatarConfigUpdated {
                            peer_id: sender_peer_id,
                        });
                    }
                }
            }
            MessageType::SfuGroupKey => {
                // A room group key sealed to us by the elected keyer. Outer
                // EncryptedSignal already proved pairwise possession of
                // msg.sender's identity; we additionally require that sender
                // is the elected keyer for this room's membership snapshot
                // (or can be, once we include them) and that the epoch is
                // plausible — rejecting random room peers who would otherwise
                // pin us on a hostile key and silence decrypt (DoS).
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let epoch = msg.payload.get("epoch").and_then(Value::as_u64);
                let key_b64 = msg.payload.get("key").and_then(Value::as_str);
                if let (false, Some(epoch), Some(key_b64)) = (room_id.is_empty(), epoch, key_b64) {
                    let epoch_u8 = epoch as u8;
                    if !self.accept_group_key_from(&msg.sender, room_id, epoch_u8) {
                        warn!(
                            "[group-key] rejecting key epoch {} for room {} from {} (not elected keyer or bad epoch)",
                            epoch_u8,
                            &room_id[..8.min(room_id.len())],
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                    match crate::crypto::b64url_decode(key_b64) {
                        Ok(bytes) if bytes.len() == 32 => {
                            let mut key = [0u8; 32];
                            key.copy_from_slice(&bytes);
                            self.group_keys.install(room_id, epoch_u8, key);
                            info!(
                                "[group-key] installed epoch {} for room {} from {}",
                                epoch_u8,
                                &room_id[..8.min(room_id.len())],
                                &msg.sender[..8.min(msg.sender.len())]
                            );
                            // Tell the keyer we have the material so they stop resealing.
                            self.send_group_key_ack(room_id, epoch_u8, &msg.sender)
                                .await;
                        }
                        _ => warn!(
                            "[group-key] malformed key from {}",
                            &msg.sender[..8.min(msg.sender.len())]
                        ),
                    }
                }
            }
            MessageType::SfuGroupKeyAck => {
                // Keyer side: member confirmed install of `(room_id, epoch)`.
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let Some(epoch) = msg
                    .payload
                    .get("epoch")
                    .and_then(Value::as_u64)
                    .map(|e| e as u8)
                else {
                    return;
                };
                if room_id.is_empty() {
                    return;
                }
                let key = (room_id.clone(), msg.sender.clone());
                match self.pending_group_key_acks.get(&key) {
                    Some(p) if p.epoch == epoch => {
                        self.pending_group_key_acks.remove(&key);
                        info!(
                            "[group-key] ack epoch {} for room {} from {}",
                            epoch,
                            &room_id[..8.min(room_id.len())],
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                    }
                    Some(p) => {
                        debug!(
                            "[group-key] stale ack epoch {} (pending {}) from {} room {}",
                            epoch,
                            p.epoch,
                            &msg.sender[..8.min(msg.sender.len())],
                            &room_id[..8.min(room_id.len())]
                        );
                    }
                    None => {
                        // Duplicate ack or we already gave up — fine.
                        debug!(
                            "[group-key] unexpected ack epoch {} from {} room {}",
                            epoch,
                            &msg.sender[..8.min(msg.sender.len())],
                            &room_id[..8.min(room_id.len())]
                        );
                    }
                }
            }
            MessageType::SfuMembers => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let members: Vec<String> = msg
                    .payload
                    .get("members")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default();
                // `chat_members` (participants + text-chat subscribers) is the key
                // group: it drives group-key election/sealing so text-only
                // subscribers get keyed and can send/read room chat without a
                // voice join. Falls back to `members` for older supernodes that
                // don't send it (legacy behavior: subscribers stay unkeyed).
                let chat_members: Vec<String> = msg
                    .payload
                    .get("chat_members")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_else(|| members.clone());
                // Confirmed on *some* member — any `room_absent` retries still
                // in flight for this room (this node or a failover sibling) are
                // moot now; drop them so the retry timer doesn't keep sending
                // redundant joins (and eventually a bogus give-up rejection)
                // after we're already in.
                self.pending_room_join_retries
                    .retain(|(_, r), _| r != &room_id);
                // If a cluster failover fan-out is awaiting confirmation for this
                // room, the sibling that answered is the one that still holds it
                // — promote it to the current supernode (correcting the
                // optimistic guess made when the joins were sent). Only the first
                // responder wins; later acks fall through as normal updates.
                if self.failover_pending_room.as_deref() == Some(room_id.as_str()) {
                    self.failover_pending_room = None;
                    self.current_supernode_id = msg.sender.clone();
                    self.current_room_id = room_id.clone();
                    // The room is confirmed resumed here, so disarm every pending
                    // rejoin for it — including the one for the node we failed
                    // over *from*. Otherwise, when that node restarts (fresh and
                    // roomless until roster gossip refills it), the reconnect
                    // handler would blindly rejoin the room there and adopt it as
                    // current; the fresh node silently denies (`room_absent`),
                    // stranding a client that was already working on this sibling.
                    self.pending_failover_rejoin.retain(|_, r| *r != room_id);
                    info!(
                        "Cluster failover: room resumed on sibling {}",
                        &msg.sender[..12.min(msg.sender.len())]
                    );
                    self.ensure_room_relay(&msg.sender).await;
                    // Tell the UI the room moved to this sibling so it follows the
                    // failover instead of showing offline / no room.
                    self.emit_event(ConnectionEvent::RoomFailedOver {
                        supernode_id: msg.sender.clone(),
                        room_id: room_id.clone(),
                    });
                    // Our camera-on went to the node that was lost, so the room
                    // as it exists on this sibling has never heard it. Nobody
                    // else will prompt us either — the members already here see
                    // no join from us worth replaying to.
                    self.reannounce_video_state(&room_id).await;
                    // Same for our subscriptions: the sibling defaults to
                    // forwarding every sender, so failing over silently undoes
                    // the saving until the UI next happens to change a tile.
                    self.resend_video_subscriptions().await;
                }
                // Reconcile the room group key against the authoritative key
                // group (participants + subscribers) so text-only members are
                // keyed too. Voice rail uses `members`; text members panel uses
                // `chat_members`.
                self.sync_room_membership(&msg.sender, &room_id, &chat_members)
                    .await;
                self.emit_event(ConnectionEvent::RoomMembersChanged {
                    supernode_id: msg.sender.clone(),
                    room_id,
                    members,
                    chat_members,
                });
            }
            MessageType::SfuPeerJoined => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let peer_id = msg
                    .payload
                    .get("peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                // Reseal the current epoch key to the newcomer if we're the
                // elected keyer (see `sync_room_membership`).
                let room_key = format!("{}:{}", msg.sender, room_id);
                let me = self.identity.public_id();
                let mut set = self
                    .room_group_members
                    .get(&room_key)
                    .cloned()
                    .unwrap_or_default();
                set.insert(me);
                set.insert(peer_id.clone());
                let members: Vec<String> = set.into_iter().collect();
                self.sync_room_membership(&msg.sender, &room_id, &members)
                    .await;
                // The newcomer never saw our camera-on edge; replay it so their
                // indicator (and ours on their side) reflects who is streaming.
                self.reannounce_video_state(&room_id).await;
                self.emit_event(ConnectionEvent::RoomPeerJoined {
                    supernode_id: msg.sender.clone(),
                    room_id,
                    peer_id,
                });
            }
            MessageType::SfuPeerLeft => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let peer_id = msg
                    .payload
                    .get("peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                // A departure → rotate the epoch key and reseal to those who
                // remain (forward secrecy + post-compromise security) if we're
                // the elected keyer (see `sync_room_membership`).
                let room_key = format!("{}:{}", msg.sender, room_id);
                let me = self.identity.public_id();
                let mut set = self
                    .room_group_members
                    .get(&room_key)
                    .cloned()
                    .unwrap_or_default();
                set.insert(me);
                set.remove(&peer_id);
                let members: Vec<String> = set.into_iter().collect();
                self.sync_room_membership(&msg.sender, &room_id, &members)
                    .await;
                self.emit_event(ConnectionEvent::RoomPeerLeft {
                    supernode_id: msg.sender.clone(),
                    room_id,
                    peer_id,
                });
            }
            MessageType::SfuChat => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("default")
                    .to_owned();
                let raw_body = msg
                    .payload
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let sender_handle = msg
                    .payload
                    .get("sender_handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let message_id = msg
                    .payload
                    .get("message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // Room chat is E2E-only: `body` is `b64(nonce ‖ aesgcm(body))`
                // under the room group key (AAD = room_id ‖ sender ‖
                // message_id). Cleartext (missing `e2e`) is rejected.
                if !msg
                    .payload
                    .get("e2e")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    warn!(
                        "[room.chat.v1] cleartext body rejected from {}; dropping",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let epoch = msg
                    .payload
                    .get("epoch")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                let sealed = match crate::crypto::b64url_decode(&raw_body) {
                    Ok(b) => b,
                    Err(e) => {
                        warn!(
                            "[room.chat.v1] body b64 decode failed from {}; dropping: {e}",
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let epoch_u8: Option<u8> = epoch.try_into().ok();
                if epoch_u8.is_none() {
                    warn!(
                        "[room.chat.v1] invalid epoch {epoch} from {}; dropping",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let plaintext = epoch_u8.and_then(|e| {
                    crate::group_key::open_chat_body(
                        &self.group_keys,
                        &room_id,
                        &msg.sender,
                        &message_id,
                        e,
                        &sealed,
                    )
                });
                let body = match plaintext.and_then(|p| String::from_utf8(p).ok()) {
                    Some(s) => s,
                    None => {
                        warn!(
                            "[room.chat.v1] failed to open E2E body from {} room={} epoch={epoch}; dropping \
                             (missing group key or wrong epoch)",
                            &msg.sender[..8.min(msg.sender.len())],
                            &room_id[..12.min(room_id.len())]
                        );
                        return;
                    }
                };
                if !body.is_empty() {
                    // Enforce the room.chat.v1 per-sender inbound quota,
                    // symmetric with the outbound gate in dispatch_outbound
                    // and with room.audio.sfu / room.file.v1.
                    if !self.check_inbound_feature_quota(
                        "room.chat.v1",
                        &msg.sender,
                        body.len().max(64),
                    ) {
                        debug!(
                            "[room.chat.v1] inbound quota exceeded for {}; dropping message",
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                    let supernode_id = inbound_supernode_id
                        .or_else(|| msg.target.clone())
                        .unwrap_or_default();
                    self.emit_event(ConnectionEvent::RoomChatMessage {
                        supernode_id,
                        room_id,
                        sender_id: msg.sender.clone(),
                        sender_handle,
                        body,
                        timestamp: msg.timestamp,
                        message_id,
                    });
                }
            }
            MessageType::SfuFileOffer => {
                let sn = inbound_supernode_id.clone().unwrap_or_default();
                self.handle_sfu_file_offer(&msg, &sn).await;
            }
            MessageType::SfuFileRequest => {
                let sn = inbound_supernode_id.clone().unwrap_or_default();
                self.handle_sfu_file_request(&msg, &sn).await;
            }
            MessageType::SfuFileRevoke => {
                self.handle_sfu_file_revoke(&msg).await;
            }
            MessageType::SfuFileChunk => {
                self.handle_sfu_file_chunk(&msg).await;
            }
            MessageType::SfuFileComplete => {
                self.handle_sfu_file_complete(&msg).await;
            }
            MessageType::SfuVideoState => {
                // A room member's camera turned on or off. Signed and routed
                // like any signaling message, so the sender is authenticated by
                // the time we get here.
                let active = msg
                    .payload
                    .get("active")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.emit_event(ConnectionEvent::PeerVideoStateChanged {
                    peer_id: msg.sender.clone(),
                    active,
                });
            }
            MessageType::SfuVideoKeyframeRequest => {
                // A receiver cannot decode and wants a fresh keyframe.
                self.emit_event(ConnectionEvent::VideoKeyframeRequested {
                    peer_id: msg.sender.clone(),
                });
            }
            MessageType::SfuAudio => {
                // Inbound room audio relayed by the supernode.  The `sender`
                // field is the originating peer (preserved by the supernode
                // broadcast).  Decode the base64 Opus payload, enforce the
                // room.audio.sfu per-sender inbound quota, then forward to
                // the call controller via a `SfuAudioReceived` event.
                use base64::Engine;
                let audio_b64 = msg
                    .payload
                    .get("audio")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let Ok(raw) = base64::engine::general_purpose::URL_SAFE.decode(audio_b64) else {
                    return;
                };
                if raw.is_empty() {
                    return;
                }
                // Room audio is E2E-only: `raw` is sealed
                // `[epoch][nonce][aesgcm(opus)]` under the room group key
                // (AAD = room_id ‖ sender ‖ seq from the signature-
                // authenticated envelope). Cleartext (missing `e2e`) is
                // rejected.
                if !msg
                    .payload
                    .get("e2e")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    warn!(
                        "[room.audio.sfu] cleartext frame rejected from {}; dropping",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let seq = msg
                    .payload
                    .get("seq")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::MAX);
                let opus_data = match crate::group_key::open_voice_frame(
                    &self.group_keys,
                    room_id,
                    &msg.sender,
                    seq,
                    &raw,
                ) {
                    Some(opus) => opus,
                    None => {
                        // Persistent open failure means group-key desync
                        // (missed/unsigned SfuGroupKey install) — audio is
                        // silent until keys reconverge.
                        warn!(
                            "[room.audio.sfu] failed to open E2E frame from {}; dropping",
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                if opus_data.is_empty() {
                    return;
                }
                if !self.check_inbound_feature_quota("room.audio.sfu", &msg.sender, opus_data.len())
                {
                    debug!(
                        "[room.audio.sfu] inbound quota exceeded for {}; dropping frame",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                self.emit_event(ConnectionEvent::SfuAudioReceived {
                    peer_id: msg.sender.clone(),
                    opus_data,
                });
            }
            MessageType::RelayGranted => {
                let ticket = msg
                    .payload
                    .get("ticket")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let host = msg
                    .payload
                    .get("relay_host")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let port = msg
                    .payload
                    .get("relay_port")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u16;
                // Portal-only grant: we're a guest that must pass the access
                // gate before full relay access. Absent on older supernodes.
                let portal_only = msg
                    .payload
                    .get("portal_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.emit_event(ConnectionEvent::RelayGranted {
                    supernode_id: msg.sender.clone(),
                    ticket,
                    relay_host: host.clone(),
                    relay_port: port,
                    portal_only,
                });
                // Open the QUIC relay connection so subsequent
                // `web.host.app.v1` fetches (and future native SFU paths)
                // have a live `quinn::Connection` to multiplex over.
                // Pass `portal_only` so we do not open a room-chat signaling
                // stream that the supernode would silently drop.
                self.spawn_relay_client_connect(msg.sender.clone(), host, port, portal_only);
            }
            MessageType::CapabilityAnnounce => {
                let raw = msg
                    .payload
                    .get("capabilities")
                    .cloned()
                    .unwrap_or(Value::Null);
                let caps_json = raw.to_string();
                // Parse and cache for the intersection check on inbound
                // CAPABILITY_INVOKE. Unknown / malformed entries are
                // silently ignored — we keep what successfully parsed.
                let parsed: Vec<CapabilityDescriptor> = match raw {
                    Value::Array(arr) => arr
                        .into_iter()
                        .filter_map(|v| serde_json::from_value::<CapabilityDescriptor>(v).ok())
                        .collect(),
                    _ => Vec::new(),
                };
                debug!(
                    "CAPABILITY_ANNOUNCE from {}: {} cap(s)",
                    &msg.sender[..8.min(msg.sender.len())],
                    parsed.len()
                );
                self.peer_capabilities.insert(msg.sender.clone(), parsed);
                self.emit_event(ConnectionEvent::CapabilityAnnounced {
                    peer_id: msg.sender.clone(),
                    caps_json,
                });
            }
            MessageType::CapabilityInvoke => {
                self.handle_capability_invoke(&msg);
            }
            MessageType::EndpointUpdate => {
                let endpoints: Vec<String> = msg
                    .payload
                    .get("endpoints")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                self.emit_event(ConnectionEvent::EndpointUpdated {
                    peer_id: msg.sender.clone(),
                    endpoints,
                });
            }
            MessageType::SupernodeInfo => {
                let homepage_url = msg
                    .payload
                    .get("homepage_url")
                    .or_else(|| msg.payload.get("app_url"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let title = msg
                    .payload
                    .get("title")
                    .or_else(|| msg.payload.get("node_title"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let sfu_enabled = msg
                    .payload
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .map(|caps| {
                        caps.iter().any(|c| {
                            c.get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id == "room.audio.sfu")
                        })
                    })
                    .unwrap_or(false);
                let public_rooms_enabled = msg
                    .payload
                    .get("capabilities")
                    .and_then(Value::as_array)
                    .and_then(|caps| {
                        caps.iter().find(|c| {
                            c.get("id")
                                .and_then(Value::as_str)
                                .is_some_and(|id| id == "room.audio.sfu")
                        })
                    })
                    .and_then(|cap| cap.get("params"))
                    .and_then(|p| p.get("allow_public_rooms"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.emit_event(ConnectionEvent::SupernodeInfoReceived {
                    supernode_id: msg.sender.clone(),
                    homepage_url,
                    title,
                    sfu_enabled,
                    public_rooms_enabled,
                });

                // Clustered supernode: parse + verify the signed sibling roster.
                // The signature must bind the roster to this supernode (which we
                // already trust), so a relay cannot inject bogus failover targets.
                if let Some(desc_val) = msg.payload.get("cluster") {
                    match serde_json::from_value::<crate::cluster::SignedClusterDescriptor>(
                        desc_val.clone(),
                    ) {
                        Ok(desc) => match desc.verified_members(&msg.sender) {
                            Some(members) => {
                                info!(
                                    "Supernode {} is in cluster '{}' with {} sibling member(s)",
                                    &msg.sender[..12.min(msg.sender.len())],
                                    desc.cluster_id,
                                    members.len()
                                );
                                self.record_cluster_members(&msg.sender, &members);
                                self.connect_cluster_siblings(&msg.sender).await;
                            }
                            None => warn!(
                                "Ignoring cluster roster from {} — signature/signer check failed",
                                &msg.sender[..12.min(msg.sender.len())]
                            ),
                        },
                        Err(e) => debug!("Malformed cluster descriptor in SUPERNODE_INFO: {e}"),
                    }
                }
            }
            MessageType::RelayPaymentRequired => {
                let portal_url = msg
                    .payload
                    .get("portal_url")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !portal_url.is_empty() {
                    info!(
                        "[relay] Portal required from {}: {}",
                        &msg.sender[..8.min(msg.sender.len())],
                        portal_url
                    );
                    self.emit_event(ConnectionEvent::RelayPaymentRequired {
                        supernode_id: msg.sender.clone(),
                        portal_url,
                    });
                }
            }
            MessageType::SfuRoomList => {
                let rooms_json = msg
                    .payload
                    .get("rooms")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "[]".to_owned());
                self.emit_event(ConnectionEvent::RoomListReceived {
                    supernode_id: msg.sender.clone(),
                    rooms_json,
                });
            }
            MessageType::SfuRoomCreated => {
                let denied = msg
                    .payload
                    .get("denied")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if denied {
                    let reason = msg
                        .payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("room_creation_denied");
                    let room_name = msg
                        .payload
                        .get("room_name")
                        .and_then(Value::as_str)
                        .unwrap_or("Room");
                    warn!(
                        "SFU room create denied by {} for '{room_name}': {reason}",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let room_name = msg
                    .payload
                    .get("room_name")
                    .and_then(Value::as_str)
                    .unwrap_or("Room")
                    .to_owned();
                let room_type = msg
                    .payload
                    .get("room_type")
                    .and_then(Value::as_str)
                    .unwrap_or("public")
                    .to_owned();
                let invite_token = msg
                    .payload
                    .get("invite_token")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if room_id.is_empty() {
                    warn!(
                        "SfuRoomCreated missing room_id from {}",
                        &msg.sender[..8.min(msg.sender.len())]
                    );
                    return;
                }
                let supernode_id = msg.sender.clone();
                let materialize_key = room_scope_key(&supernode_id, &room_id);
                // Match pending by live host *or* any host:room_id (cluster rewrite).
                let materialize_only = self.take_pending_materialize(&supernode_id, &room_id);
                if !should_auto_join_on_room_created(denied, room_id.is_empty(), materialize_only) {
                    // Materialize-only replay: refresh sidebar counts, do not join,
                    // and do **not** emit RoomCreated (that drives tray spam).
                    self.send_room_list_request(&supernode_id).await;
                    // Private rooms stay text-active while connected: subscribe
                    // *after* the room exists on the supernode so SfuChat is
                    // delivered even when the user is in another voice room.
                    if room_type == "private" {
                        self.chat_active_rooms.insert(materialize_key);
                        self.send_room_subscribe(&supernode_id, &room_id).await;
                    }
                } else {
                    self.current_supernode_id = supernode_id.clone();
                    self.current_room_id = room_id.clone();
                    // We created this room, so we're trivially its only (and thus
                    // smallest-id) member — bootstrapping happens naturally via
                    // `sync_room_membership` once the SfuMembers ack arrives.
                    // Reset any stale local state in case this room_id was reused
                    // (room ids are derived from creator+name, so recreating with
                    // the same name can repeat one).
                    let room_key = room_scope_key(&supernode_id, &room_id);
                    self.room_group_members.remove(&room_key);
                    self.group_keys.forget(&room_id);
                    self.pending_group_key_acks
                        .retain(|(r, _), _| r != &room_id);
                    self.send_room_join(&supernode_id, &room_id).await;
                    if self
                        .direct_fallback
                        .is_pending_room(&supernode_id, &room_id)
                    {
                        // Temp direct-call fallback room: invite the call target
                        // and switch local audio to room mode. Deliberately no
                        // RoomCreated event — the temp room must not enter the
                        // sidebar / room store / Space tree.
                        self.complete_direct_call_fallback(&supernode_id, &room_id, &invite_token)
                            .await;
                        return;
                    }
                    // Join ack (SfuMembers) + supernode broadcast_room_list carry
                    // authoritative counts; an immediate list request can race and
                    // publish a pre-join participant_count to the sidebar bubble.
                    self.emit_event(ConnectionEvent::RoomCreated {
                        supernode_id,
                        room_id,
                        room_name,
                        room_type,
                        invite_token,
                    });
                }
            }
            MessageType::SfuRoomInviteResult => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let accepted = msg
                    .payload
                    .get("accepted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if room_id.is_empty() {
                    return;
                }
                let supernode_id = msg.sender.clone();
                // Cluster: invite was sent to a live sibling while pending was
                // keyed under the invite host (A). Match by exact key *or* any
                // pending entry for this room_id, else we never SfuJoin after a
                // successful B/C invite (Greens Place stuck after valid=true).
                let was_pending = self.take_pending_private_room_join(&supernode_id, &room_id);
                if accepted {
                    if was_pending || self.current_room_id == room_id {
                        self.current_supernode_id = supernode_id.clone();
                        self.current_room_id = room_id.clone();
                        self.send_room_join(&supernode_id, &room_id).await;
                        // Counts follow from SfuMembers + post-join broadcast; a
                        // list request here often lands before SfuJoin completes.
                    } else {
                        debug!(
                            "SfuRoomInviteResult accepted for {} from {} but no pending join — ignoring",
                            &room_id[..8.min(room_id.len())],
                            &supernode_id[..8.min(supernode_id.len())]
                        );
                    }
                } else {
                    let reason = msg
                        .payload
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("invalid_token");
                    warn!(
                        "Private room invite rejected by {} for room {}: {}",
                        &supernode_id[..8.min(supernode_id.len())],
                        room_id,
                        reason
                    );
                    // Invite failed before SfuJoin — same UI rollback as a join deny
                    // so optimistic selection doesn't stick on a private room we
                    // never entered.
                    if self.current_room_id == room_id
                        && (self.current_supernode_id == supernode_id
                            || self.current_supernode_id.is_empty()
                            || same_supernode_pad(&self.current_supernode_id, &supernode_id))
                    {
                        self.current_room_id.clear();
                        self.current_supernode_id.clear();
                    }
                    self.emit_event(ConnectionEvent::RoomJoinRejected {
                        supernode_id,
                        room_id,
                        reason: reason.to_owned(),
                    });
                }
            }
            MessageType::SfuJoinResult => {
                let room_id = msg
                    .payload
                    .get("room_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let accepted = msg
                    .payload
                    .get("accepted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if room_id.is_empty() || accepted {
                    // Accept path is SfuMembers; ignore accepted=true if ever sent.
                    return;
                }
                let supernode_id = msg.sender.clone();
                let reason = msg
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("join_failed")
                    .to_owned();
                warn!(
                    "SFU join rejected by {} for room {}: {}",
                    &supernode_id[..8.min(supernode_id.len())],
                    room_id,
                    reason
                );
                // Drop optimistic current-room pointer so send_room_audio stops.
                if self.current_room_id == room_id
                    && (self.current_supernode_id == supernode_id
                        || self.current_supernode_id.is_empty())
                {
                    self.current_room_id.clear();
                    self.current_supernode_id.clear();
                }
                let _ = self.take_pending_private_room_join(&supernode_id, &room_id);
                // Failover fan-out may leave cold rejoins armed; clear any
                // pending rejoin for this room so we don't thrash denied nodes.
                self.pending_failover_rejoin.retain(|_, r| *r != room_id);
                if self.failover_pending_room.as_deref() == Some(room_id.as_str()) {
                    self.failover_pending_room = None;
                }
                // `room_absent` usually just means this member hasn't received
                // the room via cluster `RoomRoster` gossip yet (fresh restart,
                // cluster_link still reconnecting) — retry the same join with
                // backoff instead of surfacing a hard failure immediately. Only
                // arm a fresh retry if one isn't already in flight so we don't
                // reset its attempt counter/backoff on every denied retry.
                if reason == "room_absent" {
                    self.pending_room_join_retries
                        .entry((supernode_id, room_id))
                        .or_insert_with(|| PendingRoomJoinRetry {
                            last_sent: std::time::Instant::now(),
                            attempts: 0,
                        });
                    return;
                }
                self.emit_event(ConnectionEvent::RoomJoinRejected {
                    supernode_id,
                    room_id,
                    reason,
                });
            }
            MessageType::PresenceUpdate => {
                let status = msg
                    .payload
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("online")
                    .to_owned();
                self.emit_event(ConnectionEvent::PresenceUpdated {
                    peer_id: msg.sender.clone(),
                    status,
                });
            }
            // ── Invite handshake (inviter side: we receive INIT from the joiner) ──
            MessageType::InviteHandshakeInit => {
                let invite_id = msg
                    .payload
                    .get("invite_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let joiner_identity_pub = msg.sender.clone();
                let joiner_peer_id = msg
                    .payload
                    .get("joiner_peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&joiner_identity_pub)
                    .to_owned();
                let joiner_handle = msg
                    .payload
                    .get("joiner_handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let joiner_quic_port = msg
                    .payload
                    .get("joiner_quic_port")
                    .and_then(Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok())
                    .unwrap_or(0);
                let joiner_lan_hint = msg
                    .payload
                    .get("joiner_lan_hint")
                    .and_then(Value::as_str)
                    .filter(|hint| parse_quic_lan_hint(hint).is_some())
                    .unwrap_or("")
                    .to_owned();
                // The joiner's address as reachable from outside their NAT,
                // when they know one. Stored next to the LAN hint because
                // which of the two works depends on where we are dialing
                // from, and that is decided at dial time, not here.
                let joiner_public_hint = msg
                    .payload
                    .get("joiner_public_hint")
                    .and_then(Value::as_str)
                    .filter(|hint| parse_quic_lan_hint(hint).is_some())
                    .unwrap_or("")
                    .to_owned();
                if let Some(ref transport_peer_id) = quic_peer_id {
                    self.relabel_quic_peer_session(transport_peer_id, &joiner_peer_id);
                }
                info!(
                    "InviteHandshakeInit from {} (id={})",
                    &joiner_identity_pub[..8.min(joiner_identity_pub.len())],
                    &invite_id[..8.min(invite_id.len())]
                );

                // Add joiner to peer store
                {
                    let mut store = self.peer_store.write();
                    if let Some(record) = store.get_mut(&joiner_peer_id) {
                        record.last_seen_at = unix_now_f64();
                        record.auto_connect = true;
                        // Refresh display name when the joiner announces one
                        // (earlier empty-handle trust rows stay blank otherwise).
                        if !joiner_handle.is_empty() {
                            record.handle = joiner_handle.clone();
                        }
                        if joiner_quic_port != 0 {
                            record.quic_port = joiner_quic_port;
                        }
                        if !joiner_lan_hint.is_empty()
                            && !record.relay_hints.contains(&joiner_lan_hint)
                        {
                            record.relay_hints.push(joiner_lan_hint.clone());
                        }
                        if !joiner_public_hint.is_empty()
                            && !record.relay_hints.contains(&joiner_public_hint)
                        {
                            record.relay_hints.push(joiner_public_hint.clone());
                        }
                    } else {
                        store.upsert(crate::peer_store::PeerRecord {
                            peer_id: joiner_peer_id.clone(),
                            identity_pub: joiner_identity_pub.clone(),
                            handle: joiner_handle.clone(),
                            relay_hints: [&joiner_lan_hint, &joiner_public_hint]
                                .into_iter()
                                .filter(|h| !h.is_empty())
                                .cloned()
                                .collect(),
                            auto_connect: true,
                            quic_port: joiner_quic_port,
                            created_at: unix_now_f64(),
                            last_seen_at: unix_now_f64(),
                            ..Default::default()
                        });
                    }
                    let _ = store.save();
                }

                // Send INVITE_HANDSHAKE_ACCEPT back
                let sender = self.identity.public_id();
                let peer_id_str = self.identity.peer_id();
                let inviter_handle = super::peer_session::read_local_display_handle();
                let mut reply =
                    SignalingMessage::new(MessageType::InviteHandshakeAccept, sender.clone());
                let direct_joiner_connected = self
                    .peers
                    .get(&joiner_peer_id)
                    .map(|peer| peer.state == PeerConnectionState::Connected)
                    .unwrap_or(false);
                reply.target = Some(if direct_joiner_connected {
                    joiner_peer_id.clone()
                } else {
                    joiner_identity_pub.clone()
                });
                reply
                    .payload
                    .insert("invite_id".into(), Value::String(invite_id));
                reply
                    .payload
                    .insert("inviter_peer_id".into(), Value::String(peer_id_str));
                reply
                    .payload
                    .insert("inviter_identity_pub".into(), Value::String(sender));
                if !inviter_handle.is_empty() {
                    reply
                        .payload
                        .insert("inviter_handle".into(), Value::String(inviter_handle));
                }
                self.dispatch_outbound(reply).await;
                // Redundant with inviter_handle on ACCEPT, but also covers reconnect
                // paths that only re-run INIT/ACCEPT without a fresh invite URL.
                // Prefer the direct QUIC peer_id when connected; otherwise target
                // the joiner's identity public_id so a shared supernode can relay.
                let handle_target = if direct_joiner_connected {
                    joiner_peer_id.as_str()
                } else {
                    joiner_identity_pub.as_str()
                };
                self.send_handle_update(handle_target).await;
                self.send_local_avatar_config(handle_target).await;

                self.emit_event(ConnectionEvent::PeerConnected(joiner_peer_id.clone()));
                self.emit_event(ConnectionEvent::InviteAccepted {
                    peer_id: joiner_peer_id,
                    handle: joiner_handle,
                });
            }
            // ── Invite handshake (joiner side: we receive ACCEPT from the inviter) ──
            MessageType::InviteHandshakeAccept => {
                let invite_id = msg
                    .payload
                    .get("invite_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let inviter_identity_pub = msg
                    .payload
                    .get("inviter_identity_pub")
                    .and_then(Value::as_str)
                    .unwrap_or(&msg.sender)
                    .to_owned();
                let inviter_peer_id = msg
                    .payload
                    .get("inviter_peer_id")
                    .and_then(Value::as_str)
                    .unwrap_or(&inviter_identity_pub)
                    .to_owned();
                let inviter_handle = msg
                    .payload
                    .get("inviter_handle")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();

                if let Some(pending) = self.pending_invites.remove(&invite_id) {
                    if inviter_identity_pub != pending.inviter_identity_pub
                        || inviter_peer_id != pending.inviter_peer_id
                    {
                        warn!(
                            "InviteHandshakeAccept identity mismatch for invite_id={invite_id}: expected {}/{}, got {}/{}",
                            &pending.inviter_identity_pub
                                [..8.min(pending.inviter_identity_pub.len())],
                            &pending.inviter_peer_id[..8.min(pending.inviter_peer_id.len())],
                            &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
                            &inviter_peer_id[..8.min(inviter_peer_id.len())],
                        );
                        return;
                    }
                    if let Some(ref transport_peer_id) = quic_peer_id {
                        self.relabel_quic_peer_session(transport_peer_id, &inviter_peer_id);
                    }
                    info!(
                        "InviteHandshakeAccept from {} (id={})",
                        &inviter_identity_pub[..8.min(inviter_identity_pub.len())],
                        &invite_id[..8.min(invite_id.len())]
                    );
                    // Add inviter to peer store
                    {
                        let mut store = self.peer_store.write();
                        let relay_hints = if pending.relay_hint.is_empty() {
                            vec![]
                        } else {
                            vec![pending.relay_hint.clone()]
                        };
                        // Prefer ACCEPT handle; fall back to any previously
                        // known handle so a blank ACCEPT never blanks a name.
                        let prior_handle = store
                            .get(&inviter_peer_id)
                            .or_else(|| store.get_by_identity(&inviter_identity_pub))
                            .map(|r| r.handle.clone())
                            .unwrap_or_default();
                        let handle = if !inviter_handle.is_empty() {
                            inviter_handle.clone()
                        } else {
                            prior_handle
                        };
                        store.upsert_from_invite(crate::peer_store::PeerRecord {
                            peer_id: inviter_peer_id.clone(),
                            identity_pub: inviter_identity_pub.clone(),
                            handle,
                            relay_hints,
                            auto_connect: !pending.is_supernode,
                            quic_port: parse_quic_lan_hint(&pending.lan_hint)
                                .map(|(_, port)| port)
                                .unwrap_or(0),
                            is_supernode: pending.is_supernode,
                            supernode_from_invite: pending.is_supernode,
                            created_at: unix_now_f64(),
                            last_seen_at: unix_now_f64(),
                            ..Default::default()
                        });
                        let _ = store.save();
                    }
                    // Tell the inviter our display name (INIT may have been empty
                    // if settings loaded after handshake started). Prefer direct
                    // peer_id; fall back to identity_pub for supernode-relayed trust.
                    let direct_inviter = self
                        .peers
                        .get(&inviter_peer_id)
                        .map(|p| p.state == PeerConnectionState::Connected)
                        .unwrap_or(false);
                    let handle_target = if direct_inviter {
                        inviter_peer_id.as_str()
                    } else {
                        inviter_identity_pub.as_str()
                    };
                    self.send_handle_update(handle_target).await;
                    self.send_local_avatar_config(handle_target).await;
                    self.emit_event(ConnectionEvent::PeerConnected(inviter_peer_id.clone()));
                    self.emit_event(ConnectionEvent::InviteAccepted {
                        peer_id: inviter_peer_id,
                        handle: inviter_handle,
                    });
                } else {
                    warn!("InviteHandshakeAccept for unknown invite_id={invite_id}");
                }
            }
            MessageType::InviteHandshakeReject => {
                warn!(
                    "Invite rejected by {}",
                    &msg.sender[..8.min(msg.sender.len())]
                );
            }
            // ── File transfer ─────────────────────────────────────────────────────────────
            MessageType::FileTransferOffer => {
                if !self.gate_through_feature("core.file.v1", &msg.sender, &[]) {
                    return;
                }
                // Required fields. A peer that omits `transfer_id`, `sha256`,
                // or `size` cannot be honoured — silently coercing those to
                // empty/zero used to create ghost inbound transfers that
                // could never complete and never time out, leaking state.
                let tid = match msg.payload.get("transfer_id").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() => s.to_owned(),
                    _ => {
                        warn!(
                            "FILE_OFFER from {} missing transfer_id — dropped",
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let sha = match msg.payload.get("sha256").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() => s.to_owned(),
                    _ => {
                        warn!(
                            "FILE_OFFER {} from {} missing sha256 — dropped",
                            tid,
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let size = match msg.payload.get("size").and_then(Value::as_u64) {
                    Some(n) => n as usize,
                    None => {
                        warn!(
                            "FILE_OFFER {} from {} missing/non-numeric size — dropped",
                            tid,
                            &msg.sender[..8.min(msg.sender.len())]
                        );
                        return;
                    }
                };
                let rel = msg
                    .payload
                    .get("rel_path")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let tot = msg
                    .payload
                    .get("total_chunks")
                    .and_then(Value::as_u64)
                    .unwrap_or(1) as usize;
                let purp = msg
                    .payload
                    .get("purpose")
                    .and_then(Value::as_str)
                    .unwrap_or("file")
                    .to_owned();
                let comp = msg
                    .payload
                    .get("compressed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let delt = msg
                    .payload
                    .get("is_delta")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let bsha = msg
                    .payload
                    .get("base_sha256")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_offer_received(
                    &msg.sender,
                    &tid,
                    &rel,
                    &sha,
                    size,
                    tot,
                    &purp,
                    comp,
                    delt,
                    &bsha,
                );
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferAccept => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // The offer may be gone — revoked when we deleted the message.
                // Say so, or the peer waits forever for chunks that will never
                // come.
                if !self.file_mgr.has_outbound(&tid) {
                    info!(
                        "[core.file.v1] {} accepted revoked transfer {}; refusing",
                        &msg.sender[..8.min(msg.sender.len())],
                        &tid[..8.min(tid.len())]
                    );
                    let sender = self.identity.public_id();
                    let mut err = SignalingMessage::new(MessageType::FileTransferError, sender);
                    err.target = Some(msg.sender.clone());
                    err.payload
                        .insert("transfer_id".to_owned(), Value::String(tid));
                    err.payload.insert(
                        "reason".to_owned(),
                        Value::String("no longer shared".to_owned()),
                    );
                    self.dispatch_outbound(err).await;
                    return;
                }
                let evs = self.file_mgr.on_transfer_accepted(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferReject => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_rejected(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferChunk => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let idx = msg
                    .payload
                    .get("chunk_index")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                let data = msg
                    .payload
                    .get("data")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                // Chunk size dominates the file feature's byte quota —
                // base64 expands by ~4/3 so the wire size approximates
                // `data.len()`.
                // In-flight transfers must not punch holes: quota-dropping a
                // reliable chunk leaves the receiver unable to complete. New
                // unknown transfers still pay the gate.
                if !self.file_mgr.has_inbound(&tid) {
                    let probe = vec![0u8; data.len()];
                    if !self.gate_through_feature("core.file.v1", &msg.sender, &probe) {
                        return;
                    }
                }
                let evs = self.file_mgr.on_chunk_received(&tid, idx, data);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferComplete => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_complete_received(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferAck => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_ack(&tid);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::FileTransferError => {
                let tid = msg
                    .payload
                    .get("transfer_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let reason = msg
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let evs = self.file_mgr.on_transfer_error(&tid, &reason);
                self.dispatch_transfer_events(evs).await;
            }
            MessageType::BuildAttestation | MessageType::AttestationResponse => {
                // Store the peer's reported build info for reproducible-build / trusted-build attestation.
                // The message is already signature + replay verified by the caller.
                let build_id = msg
                    .payload
                    .get("build_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let version = msg
                    .payload
                    .get("version")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let source_hash = msg
                    .payload
                    .get("source_hash")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let release_sig = msg.payload.get("release_sig").and_then(Value::as_str);

                if !build_id.is_empty() {
                    let is_official = crate::crypto::verify_official_release_build(
                        &build_id,
                        &version,
                        &source_hash,
                        release_sig,
                    );

                    let mut store = self.peer_store.write();
                    if let Some(rec) = store.get_mut(&msg.sender) {
                        rec.peer_build_hash = build_id.clone();
                        rec.peer_source_hash = source_hash.clone();
                        if !version.is_empty() {
                            rec.peer_version = version.clone();
                        }
                        rec.last_attestation_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs_f64())
                            .unwrap_or(0.0);
                        rec.attestation_status = if is_official {
                            "official".to_string()
                        } else {
                            "claimed".to_string()
                        };
                    }
                    let _ = store.save();
                    drop(store);

                    debug!(
                        "Build attestation from {}: build_id={}, source_hash={}, version={}, official={}",
                        &msg.sender[..8.min(msg.sender.len())],
                        build_id,
                        if source_hash.is_empty() { "n/a" } else { &source_hash },
                        if version.is_empty() { "n/a" } else { &version },
                        is_official
                    );

                    // Also forward so the UI layer (bridge, models) can react if desired
                    // (e.g. update peer list with build info, enforce policy).
                    self.emit_event(ConnectionEvent::SignalingMessage(msg));
                }
            }
            _ => {
                // Forward unhandled messages to the app layer
                self.emit_event(ConnectionEvent::SignalingMessage(msg));
            }
        }
    }

    /// Apply the three framework gates (intersection, auth tier, trust) and
    /// dispatch to the local module if all pass.
    pub(super) fn handle_capability_invoke(&mut self, msg: &SignalingMessage) {
        let feature_id = match msg.payload.get("id").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                warn!(
                    "[capabilities] CAPABILITY_INVOKE from {} missing 'id' — dropped",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        // Validate `params` shape — modules expect either nothing or a JSON
        // object. Accepting arbitrary scalars (string, number, bool, array)
        // is a foot-gun for module authors and surfaces as a panic deep
        // inside whichever module handler runs.
        let params = match msg.payload.get("params") {
            None => Value::Null,
            Some(Value::Null) => Value::Null,
            Some(v) if v.is_object() => v.clone(),
            Some(_) => {
                warn!(
                    "[capabilities] CAPABILITY_INVOKE '{}' from {} has non-object params — dropped",
                    feature_id,
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };

        // Gate 1 — intersection: peer must have announced the feature.
        let peer_supports = self
            .peer_capabilities
            .get(&msg.sender)
            .map(|caps| caps.iter().any(|c| c.id == feature_id))
            .unwrap_or(false);
        if !peer_supports {
            info!(
                "[capabilities] peer {} invoked '{}' but did not announce it — dropped",
                &msg.sender[..8.min(msg.sender.len())],
                feature_id
            );
            return;
        }

        // Gate 2 — auth tier: only enforced when we have a local descriptor.
        if let Some(desc) = self.feature_registry.get(&feature_id) {
            match desc.auth {
                AuthTier::TrustedPeer => {
                    let trusted = self
                        .peer_store
                        .read()
                        .get_by_identity(&msg.sender)
                        .is_some();
                    if !trusted {
                        warn!(
                            "[capabilities] peer {} invoked '{}' (auth=trusted-peer) but is not trusted — dropped",
                            &msg.sender[..8.min(msg.sender.len())],
                            feature_id
                        );
                        return;
                    }
                }
                AuthTier::RoomMember => {
                    if !self.room_members.contains(&msg.sender) {
                        warn!(
                            "[capabilities] peer {} invoked '{}' (auth=room-member) but is not in room — dropped",
                            &msg.sender[..8.min(msg.sender.len())],
                            feature_id
                        );
                        return;
                    }
                }
                AuthTier::Public => {}
            }
        }

        // Gate 3 — feature trust: bespoke namespaces require user consent.
        match FeatureTrustGate::check(&feature_id, &msg.sender, &self.feature_trust) {
            TrustDecision::Allow => {}
            TrustDecision::Deny => {
                info!(
                    "[feature_trust] invoke of '{}' from {} denied (stored decision)",
                    feature_id,
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
            TrustDecision::Pending => {
                info!(
                    "[feature_trust] invoke of '{}' from {} pending user decision",
                    feature_id,
                    &msg.sender[..8.min(msg.sender.len())]
                );
                self.emit_event(ConnectionEvent::CapabilityInvokePending {
                    peer_id: msg.sender.clone(),
                    feature_id,
                    params,
                });
                return;
            }
        }

        // All gates passed — dispatch to the local module if one is bound.
        let ctx = InvocationContext {
            peer: msg.sender.clone(),
            params: params.clone(),
            channel_tag: None,
        };
        match self.feature_registry.dispatch_invoke(&feature_id, ctx) {
            Ok(()) => debug!(
                "[capabilities] invoked '{}' from {}",
                feature_id,
                &msg.sender[..8.min(msg.sender.len())]
            ),
            Err(e) => debug!(
                "[capabilities] no module bound for '{}' from {} ({e})",
                feature_id,
                &msg.sender[..8.min(msg.sender.len())]
            ),
        }
        self.emit_event(ConnectionEvent::CapabilityInvoked {
            peer_id: msg.sender.clone(),
            feature_id,
            params,
        });
    }

    pub(super) async fn handle_sfu_file_offer(
        &mut self,
        msg: &SignalingMessage,
        supernode_id: &str,
    ) {
        if !self.gate_through_feature("room.file.v1", &msg.sender, &[]) {
            return;
        }
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned();
        let tid = match msg.payload.get("transfer_id").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                warn!(
                    "SFU_FILE_OFFER from {} missing transfer_id",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        let sha = match msg.payload.get("sha256").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_owned(),
            _ => {
                warn!(
                    "SFU_FILE_OFFER {tid} from {} missing sha256",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        let size = match msg.payload.get("size").and_then(Value::as_u64) {
            Some(n) => n as usize,
            None => {
                warn!(
                    "SFU_FILE_OFFER {tid} from {} missing/non-numeric size",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
        };
        let rel = msg
            .payload
            .get("rel_path")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_owned();
        let total_chunks = msg
            .payload
            .get("total_chunks")
            .and_then(Value::as_u64)
            .unwrap_or(1) as usize;
        let purpose = msg
            .payload
            .get("purpose")
            .and_then(Value::as_str)
            .unwrap_or("room_file")
            .to_owned();
        let compressed = msg
            .payload
            .get("compressed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let is_delta = msg
            .payload
            .get("is_delta")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let base_sha = msg
            .payload
            .get("base_sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        // Prefer the stamped originator; `msg.sender` is the fallback when an
        // older peer omitted `origin_id`. Never use the room id as the author.
        let origin = msg
            .payload
            .get("origin_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && *s != room_id.as_str())
            .unwrap_or(msg.sender.as_str())
            .to_owned();
        let evs = self.room_file_mgr.on_offer_received_with_room(
            &origin,
            &room_id,
            supernode_id,
            &tid,
            &rel,
            &sha,
            size,
            total_chunks,
            &purpose,
            compressed,
            is_delta,
            &base_sha,
        );
        // Do NOT auto-accept. Room files are advertised and pulled: the user
        // decides, and only then does `accept_room_file` send an
        // SfuFileRequest. Auto-accepting here meant every member downloaded
        // every file, which does not scale to a 250 MB video.
        self.dispatch_room_transfer_events(evs, supernode_id, &room_id)
            .await;
    }

    /// Accept whichever inbound transfer `transfer_id` belongs to.
    ///
    /// ChatPanel always calls `AcceptFile` and RoomPanel `AcceptRoomFile`. A
    /// room offer that was mis-keyed under the room id used to land in the
    /// 1:1 panel; either command must still start the right stream.
    pub(super) async fn accept_inbound_file(&mut self, transfer_id: &str) {
        if self.file_mgr.has_inbound(transfer_id) {
            let evs = self.file_mgr.accept_transfer(transfer_id);
            self.dispatch_transfer_events(evs).await;
            return;
        }
        match self.room_file_mgr.inbound_route(transfer_id) {
            Some((origin, room_id, sn)) => {
                let sn = if sn.is_empty() {
                    self.current_supernode_id.clone()
                } else {
                    sn
                };
                self.accept_room_file(&sn, &room_id, transfer_id, &origin)
                    .await;
            }
            None => warn!("AcceptFile: unknown transfer {transfer_id}"),
        }
    }

    /// The user accepted a room file offer → ask the originator to stream it.
    pub(super) async fn accept_room_file(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        transfer_id: &str,
        origin_peer: &str,
    ) {
        let evs = self.room_file_mgr.accept_transfer_locally(transfer_id);
        self.dispatch_room_transfer_events(evs, supernode_id, room_id)
            .await;

        let route = self.live_room_route(supernode_id);
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuFileRequest, sender);
        msg.target = Some(route);
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload.insert(
            "transfer_id".to_owned(),
            Value::String(transfer_id.to_owned()),
        );
        // `to` names the file's originator; the supernode relays to them alone.
        msg.payload
            .insert("to".to_owned(), Value::String(origin_peer.to_owned()));
        info!(
            "[room.file.v1] requesting transfer {} from {}",
            &transfer_id[..8.min(transfer_id.len())],
            &origin_peer[..8.min(origin_peer.len())]
        );
        self.dispatch_outbound(msg).await;
    }

    /// A room member accepted our offer → start streaming to them.
    pub(super) async fn handle_sfu_file_request(
        &mut self,
        msg: &SignalingMessage,
        supernode_id: &str,
    ) {
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(Value::as_str)
            .unwrap_or("default")
            .to_owned();
        let tid = msg
            .payload
            .get("transfer_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if tid.is_empty() {
            return;
        }
        // Only serve a request that names us; the supernode routes by `to`, but
        // a stray copy must not make us stream to the wrong room.
        // Pad-tolerant: relay-sourced ids are unpadded, handshake ids are not.
        let to = msg.payload.get("to").and_then(Value::as_str).unwrap_or("");
        let me = self.identity.public_id();
        if !to.is_empty() && !same_supernode_pad(to, &me) {
            return;
        }
        // The offer may be gone: revoked when the sender deleted the message,
        // or aged out past OFFER_TTL_SECS. Tell the requester so their chip
        // fails instead of waiting forever for chunks that will never come.
        if !self.room_file_mgr.has_outbound(&tid) {
            info!(
                "[room.file.v1] {} requested revoked/expired transfer {}; refusing",
                &msg.sender[..8.min(msg.sender.len())],
                &tid[..8.min(tid.len())]
            );
            self.send_file_revoke(supernode_id, &room_id, &tid, Some(&msg.sender))
                .await;
            return;
        }
        info!(
            "[room.file.v1] {} requested transfer {}; streaming",
            &msg.sender[..8.min(msg.sender.len())],
            &tid[..8.min(tid.len())]
        );
        let evs = self
            .room_file_mgr
            .start_stream_for(&tid, &msg.sender, ROOM_FILE_CHUNK_BUDGET);
        self.dispatch_room_transfer_events(evs, supernode_id, &room_id)
            .await;
    }

    /// Announce that a room file offer is withdrawn.
    ///
    /// `to = None` broadcasts to the room (the sender deleted the message);
    /// `to = Some(peer)` answers one requester whose offer had already gone.
    pub(super) async fn send_file_revoke(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        transfer_id: &str,
        to: Option<&str>,
    ) {
        let route = self.live_room_route(supernode_id);
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuFileRevoke, sender);
        msg.target = Some(route);
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload.insert(
            "transfer_id".to_owned(),
            Value::String(transfer_id.to_owned()),
        );
        if let Some(peer) = to {
            msg.payload
                .insert("to".to_owned(), Value::String(peer.to_owned()));
        }
        self.dispatch_outbound(msg).await;
    }

    /// The originator withdrew a room file offer.
    ///
    /// Only the peer who advertised it may revoke it — otherwise any room
    /// member could cancel someone else's transfer.
    pub(super) async fn handle_sfu_file_revoke(&mut self, msg: &SignalingMessage) {
        let tid = msg
            .payload
            .get("transfer_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if tid.is_empty() {
            return;
        }
        match self.room_file_mgr.inbound_route(&tid) {
            Some((origin, _, _)) if origin == msg.sender => {}
            Some(_) => {
                warn!(
                    "[room.file.v1] {} tried to revoke a transfer it did not offer; ignoring",
                    &msg.sender[..8.min(msg.sender.len())]
                );
                return;
            }
            // Unknown transfer: nothing pending, nothing to do.
            None => return,
        }
        info!(
            "[room.file.v1] transfer {} revoked by sender",
            &tid[..8.min(tid.len())]
        );
        self.room_file_mgr.discard_inbound(&tid);
        self.emit_event(ConnectionEvent::FileFailed {
            transfer_id: tid,
            reason: "no longer shared".to_owned(),
        });
    }

    pub(super) async fn handle_sfu_file_chunk(&mut self, msg: &SignalingMessage) {
        let tid = msg
            .payload
            .get("transfer_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let idx = msg
            .payload
            .get("chunk_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let raw_data = msg
            .payload
            .get("data")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !self.room_file_mgr.has_inbound(&tid) {
            let probe = vec![0u8; raw_data.len()];
            if !self.gate_through_feature("room.file.v1", &msg.sender, &probe) {
                return;
            }
        }
        // Room file chunks are E2E-only: `data` is
        // `base64(nonce ‖ aesgcm(data))` under the room group key
        // (AAD = room_id ‖ sender ‖ transfer_id ‖ chunk_index). Decrypt and
        // re-encode as plain base64 for `FileTransferManager::on_chunk_received`.
        // Cleartext (missing `e2e`) is rejected.
        if !msg
            .payload
            .get("e2e")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            warn!(
                "[room.file.v1] cleartext chunk rejected from {}; dropping",
                &msg.sender[..8.min(msg.sender.len())]
            );
            return;
        }
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD;
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let epoch = msg
            .payload
            .get("epoch")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX);
        let Ok(sealed) = b64.decode(raw_data) else {
            return;
        };
        let plaintext = epoch.try_into().ok().and_then(|e: u8| {
            crate::group_key::open_file_chunk(
                &self.group_keys,
                room_id,
                &msg.sender,
                &tid,
                idx as u64,
                e,
                &sealed,
            )
        });
        let Some(plain) = plaintext else {
            warn!(
                "[room.file.v1] failed to open E2E chunk from {}; dropping",
                &msg.sender[..8.min(msg.sender.len())]
            );
            return;
        };
        // Hand the manager raw bytes. This used to re-encode the decrypted
        // chunk to base64 purely so `on_chunk_received` could decode it again —
        // two allocations and two codec passes per chunk, ~4 000 of them for a
        // 250 MB file.
        let evs = self.room_file_mgr.on_chunk_bytes_received(&tid, idx, plain);
        self.dispatch_room_transfer_events(evs, "", "").await;
    }

    pub(super) async fn handle_sfu_file_complete(&mut self, msg: &SignalingMessage) {
        let tid = msg
            .payload
            .get("transfer_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let evs = self.room_file_mgr.on_complete_received(&tid);
        self.dispatch_room_transfer_events(evs, "", "").await;
    }

    /// Dispatch a batch of [`TransferEvent`]s, routing outbound messages and
    /// emitting the appropriate [`ConnectionEvent`]s upward.
    pub(super) async fn dispatch_transfer_events(&mut self, events: Vec<TransferEvent>) {
        // Quota can refuse a chunk after `next_chunk_events` already counted
        // it. Track unsent file-data frames so we rewind and retry next pump
        // instead of leaving holes the receiver can never fill.
        let mut hold_file_data = false;
        let mut unsent_chunks: HashMap<String, usize> = HashMap::new();
        // Streams that reached no transport this turn get backed off, so a
        // lost route costs one retry every few seconds instead of 50 a second.
        let mut stalled: HashSet<String> = HashSet::new();
        let mut progressed: HashSet<String> = HashSet::new();
        for ev in events {
            match ev {
                TransferEvent::SendMessage {
                    peer_id,
                    message_type,
                    payload,
                } => {
                    let tid = payload
                        .get("transfer_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let is_chunk = message_type == MessageType::FileTransferChunk;
                    let is_complete = message_type == MessageType::FileTransferComplete;
                    if hold_file_data && (is_chunk || is_complete) {
                        if !tid.is_empty() {
                            stalled.insert(tid.clone());
                            if is_chunk {
                                *unsent_chunks.entry(tid).or_insert(0) += 1;
                            }
                        }
                        continue;
                    }
                    let sender = self.identity.public_id();
                    let mut msg = SignalingMessage::new(message_type, sender);
                    msg.target = Some(peer_id);
                    msg.payload = payload.into_iter().collect();
                    let sent = self.dispatch_outbound(msg).await;
                    if is_chunk {
                        if !sent {
                            hold_file_data = true;
                            if !tid.is_empty() {
                                *unsent_chunks.entry(tid.clone()).or_insert(0) += 1;
                                stalled.insert(tid);
                            }
                        } else if !tid.is_empty() {
                            progressed.insert(tid);
                        }
                    } else if is_complete {
                        if sent {
                            self.file_mgr.mark_outbound_complete(&tid);
                        } else if !tid.is_empty() {
                            // COMPLETE is the whole batch once every chunk is
                            // out, so a failure here is the case that would
                            // otherwise re-sign it 50 times a second forever.
                            stalled.insert(tid);
                        }
                    }
                }
                TransferEvent::Offered {
                    transfer_id,
                    peer_id,
                    rel_path,
                    size,
                    purpose,
                } => {
                    self.emit_event(ConnectionEvent::FileOffered {
                        transfer_id,
                        peer_id: peer_id.clone(),
                        rel_path,
                        size,
                        purpose,
                        is_self: false,
                        origin_id: peer_id,
                        supernode_id: String::new(),
                    });
                }
                TransferEvent::Progress {
                    transfer_id,
                    progress,
                } => {
                    self.emit_event(ConnectionEvent::FileProgress {
                        transfer_id,
                        progress,
                    });
                }
                TransferEvent::Complete {
                    transfer_id,
                    peer_id,
                    room_id,
                    supernode_id,
                    purpose,
                    payload,
                    rel_path,
                } => {
                    self.emit_event(ConnectionEvent::FileComplete {
                        transfer_id,
                        peer_id,
                        room_id,
                        supernode_id,
                        purpose,
                        payload,
                        rel_path,
                    });
                }
                TransferEvent::Failed {
                    transfer_id,
                    reason,
                } => {
                    self.emit_event(ConnectionEvent::FileFailed {
                        transfer_id,
                        reason,
                    });
                }
                TransferEvent::StateChanged { .. } => {
                    // Granular state changes; no top-level event for now.
                }
            }
        }
        for (tid, n) in unsent_chunks {
            if n > 0 {
                self.file_mgr.unsend_chunks(&tid, n);
            }
        }
        for tid in progressed.difference(&stalled) {
            self.file_mgr.note_send_progress(tid);
        }
        for tid in stalled {
            if let Some(reason) = self.file_mgr.note_send_stalled(&tid) {
                self.emit_event(ConnectionEvent::FileFailed {
                    transfer_id: tid,
                    reason,
                });
            }
        }
    }

    pub(super) async fn dispatch_room_transfer_events(
        &mut self,
        events: Vec<TransferEvent>,
        supernode_id: &str,
        room_id: &str,
    ) {
        let mut hold_file_data = false;
        let mut unsent_chunks: HashMap<String, usize> = HashMap::new();
        let mut stalled: HashSet<String> = HashSet::new();
        let mut progressed: HashSet<String> = HashSet::new();
        for ev in events {
            match ev {
                TransferEvent::SendMessage {
                    message_type,
                    mut payload,
                    ..
                } => {
                    let room_msg_type = match message_type {
                        MessageType::FileTransferOffer => MessageType::SfuFileOffer,
                        MessageType::FileTransferChunk => MessageType::SfuFileChunk,
                        MessageType::FileTransferComplete => MessageType::SfuFileComplete,
                        _ => continue,
                    };
                    let tid = payload
                        .get("transfer_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let is_chunk = room_msg_type == MessageType::SfuFileChunk;
                    let is_complete = room_msg_type == MessageType::SfuFileComplete;
                    if hold_file_data && (is_chunk || is_complete) {
                        if !tid.is_empty() {
                            stalled.insert(tid.clone());
                            if is_chunk {
                                *unsent_chunks.entry(tid).or_insert(0) += 1;
                            }
                        }
                        continue;
                    }
                    payload.insert("room_id".into(), Value::String(room_id.to_owned()));
                    let sender = self.identity.public_id();
                    // The signed `sender` is the originator, but room UI used
                    // to key offers by room id. Stamp the person on the offer
                    // so receivers never render the room or supernode as author.
                    if room_msg_type == MessageType::SfuFileOffer {
                        payload.insert("origin_id".into(), Value::String(sender.clone()));
                    }
                    // E2E-seal chunk data under real group-key material only
                    // (deterministic fallback is not supernode-opaque). Drop
                    // the chunk until keyed — receiver never gets cleartext.
                    if room_msg_type == MessageType::SfuFileChunk {
                        use base64::Engine;
                        let b64 = base64::engine::general_purpose::STANDARD;
                        if !may_send_room_e2e_content(self.group_keys.has_real_key(room_id)) {
                            warn!("[room.file.v1] no real group key yet; dropping outbound chunk");
                            hold_file_data = true;
                            if !tid.is_empty() {
                                *unsent_chunks.entry(tid.clone()).or_insert(0) += 1;
                                stalled.insert(tid);
                            }
                            continue;
                        }
                        let transfer_id = payload
                            .get("transfer_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let chunk_index = payload
                            .get("chunk_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        if let Some(raw) = payload
                            .get("data")
                            .and_then(Value::as_str)
                            .and_then(|s| b64.decode(s).ok())
                        {
                            match crate::group_key::seal_file_chunk(
                                &self.group_keys,
                                room_id,
                                &sender,
                                &transfer_id,
                                chunk_index,
                                &raw,
                            ) {
                                Some((epoch, sealed)) => {
                                    payload.insert(
                                        "data".to_owned(),
                                        Value::String(b64.encode(&sealed)),
                                    );
                                    payload.insert("e2e".to_owned(), Value::Bool(true));
                                    payload.insert(
                                        "epoch".to_owned(),
                                        Value::Number((epoch as u64).into()),
                                    );
                                }
                                None => {
                                    warn!("[room.file.v1] seal failed; dropping outbound chunk");
                                    hold_file_data = true;
                                    if !tid.is_empty() {
                                        *unsent_chunks.entry(tid.clone()).or_insert(0) += 1;
                                        stalled.insert(tid);
                                    }
                                    continue;
                                }
                            }
                        }
                    }
                    let mut msg = SignalingMessage::new(room_msg_type, sender);
                    msg.target = Some(supernode_id.to_owned());
                    msg.payload = payload.into_iter().collect();
                    let sent = self.dispatch_outbound(msg).await;
                    if is_chunk {
                        if !sent {
                            hold_file_data = true;
                            if !tid.is_empty() {
                                *unsent_chunks.entry(tid.clone()).or_insert(0) += 1;
                                stalled.insert(tid);
                            }
                        } else if !tid.is_empty() {
                            progressed.insert(tid);
                        }
                    } else if is_complete {
                        if sent {
                            self.room_file_mgr.mark_outbound_complete(&tid);
                        } else if !tid.is_empty() {
                            stalled.insert(tid);
                        }
                    }
                }
                TransferEvent::Offered {
                    transfer_id,
                    peer_id: offered_peer,
                    rel_path,
                    size,
                    purpose,
                } => {
                    // Prefer explicit room_id from the dispatch context; fall
                    // back to the event peer (already remapped for room offers).
                    let ui_peer = if room_id.is_empty() {
                        offered_peer.clone()
                    } else {
                        room_id.to_owned()
                    };
                    let me = self.identity.public_id();
                    let is_self = same_supernode_pad(&offered_peer, &me);
                    self.emit_event(ConnectionEvent::FileOffered {
                        transfer_id,
                        peer_id: ui_peer,
                        rel_path,
                        size,
                        purpose,
                        is_self,
                        origin_id: offered_peer,
                        supernode_id: supernode_id.to_owned(),
                    });
                }
                TransferEvent::Progress {
                    transfer_id,
                    progress,
                } => {
                    self.emit_event(ConnectionEvent::FileProgress {
                        transfer_id,
                        progress,
                    });
                }
                TransferEvent::Complete {
                    transfer_id,
                    peer_id,
                    room_id: xfer_room,
                    supernode_id: xfer_sn,
                    purpose,
                    payload,
                    rel_path,
                } => {
                    let rid = if xfer_room.is_empty() {
                        room_id.to_owned()
                    } else {
                        xfer_room
                    };
                    let sn = if xfer_sn.is_empty() {
                        supernode_id.to_owned()
                    } else {
                        xfer_sn
                    };
                    self.emit_event(ConnectionEvent::FileComplete {
                        transfer_id,
                        peer_id,
                        room_id: rid,
                        supernode_id: sn,
                        purpose,
                        payload,
                        rel_path,
                    });
                }
                TransferEvent::Failed {
                    transfer_id,
                    reason,
                } => {
                    self.emit_event(ConnectionEvent::FileFailed {
                        transfer_id,
                        reason,
                    });
                }
                TransferEvent::StateChanged { .. } => {}
            }
        }
        for (tid, n) in unsent_chunks {
            if n > 0 {
                self.room_file_mgr.unsend_chunks(&tid, n);
            }
        }
        for tid in progressed.difference(&stalled) {
            self.room_file_mgr.note_send_progress(tid);
        }
        for tid in stalled {
            if let Some(reason) = self.room_file_mgr.note_send_stalled(&tid) {
                self.emit_event(ConnectionEvent::FileFailed {
                    transfer_id: tid,
                    reason,
                });
            }
        }
    }

    #[inline]
    pub(super) fn check_inbound_feature_quota(
        &self,
        feature_id: &str,
        sender: &str,
        byte_count: usize,
    ) -> bool {
        self.feature_registry
            .gate_inbound_through_feature(feature_id, sender, byte_count)
    }

    /// Inbound gate for relayed room media, charged against the **supernode**.
    ///
    /// Split from [`check_inbound_feature_quota`](Self::check_inbound_feature_quota)
    /// because that bucket then holds every sender in the room rather than one
    /// peer, so the per-sender rate is the wrong meter for it — see
    /// [`FeatureRegistry::gate_inbound_fanout_through_feature`].
    pub(super) fn check_inbound_fanout_quota(
        &self,
        feature_id: &str,
        supernode_id: &str,
        byte_count: usize,
    ) -> bool {
        self.feature_registry.gate_inbound_fanout_through_feature(
            feature_id,
            supernode_id,
            byte_count,
        )
    }

    /// Gate an inbound payload through the framework's per-feature quota.
    ///
    /// Legacy inbound gate used only by the old direct-emit paths for
    /// chat and file transfer.
    ///
    /// IMPORTANT (Audio Dispatch Decision - Option A):
    /// This wrapper is **not** used for `core.audio.opus`.
    /// Audio always calls `FeatureRegistry::gate_through_feature` directly
    /// on both the outbound send paths and (via dispatch) on inbound.
    ///
    /// The special case below (returning true when no module is bound) exists
    /// only so that advertisement-only first-party features like audio can
    /// continue using the legacy inbound chat/file dispatch paths without
    /// being dropped. Audio itself never goes through this wrapper.
    pub(super) fn gate_through_feature(
        &self,
        feature_id: &str,
        sender: &str,
        payload: &[u8],
    ) -> bool {
        if self.feature_registry.module(feature_id).is_none() {
            return true;
        }
        if self
            .feature_registry
            .dispatch_message(feature_id, sender.to_owned(), payload)
        {
            true
        } else {
            warn!(
                "[capabilities] '{}' from {} dropped — quota exhausted",
                feature_id,
                &sender[..8.min(sender.len())]
            );
            false
        }
    }

    pub(super) fn verify_inbound_signature(msg: &SignalingMessage) -> bool {
        let Some(sig_b64) = msg.signature.as_deref() else {
            return false;
        };
        let Ok(sig_bytes) = crate::crypto::b64url_decode(sig_b64) else {
            return false;
        };
        let Ok(pub_bytes) = crate::crypto::b64url_decode(&msg.sender) else {
            return false;
        };
        if pub_bytes.len() != 32 {
            return false;
        }
        let canonical = match msg.canonical_bytes() {
            Ok(b) => b,
            Err(_) => return false,
        };
        if !crate::crypto::ed25519_verify(&pub_bytes, &sig_bytes, &canonical) {
            return false;
        }

        if !msg.is_fresh(Self::MAX_MESSAGE_AGE_SECS) {
            warn!(
                "[signaling] dropping {:?} from {} — stale or future timestamp",
                msg.msg_type,
                &msg.sender[..8.min(msg.sender.len())],
            );
            return false;
        }

        true
    }

    /// Test hook for signature + freshness verification on the client path.
    #[cfg(test)]
    pub(crate) fn verify_inbound_signature_for_test(msg: &SignalingMessage) -> bool {
        Self::verify_inbound_signature(msg)
    }

    /// Sliding-window replay check, keyed on the message's Ed25519 signature.
    ///
    /// Returns `true` if the message is new (process it) and `false` if it is a
    /// replay of a signature already accepted from this sender within the
    /// freshness window. Must be called only after
    /// [`verify_inbound_signature`](Self::verify_inbound_signature) succeeds, so
    /// the signature is present and valid.
    pub(super) fn check_replay(&self, msg: &SignalingMessage) -> bool {
        let Some(sig_b64) = msg.signature.as_deref() else {
            return false;
        };
        let Ok(sig_bytes) = crate::crypto::b64url_decode(sig_b64) else {
            return false;
        };
        self.replay_guard.check_and_record(&msg.sender, &sig_bytes)
    }

    /// Returns `true` when `sender` (a base64url `public_id`, as it appears in
    /// `msg.sender`) resolves to a peer we mutually trust: present in the local
    /// trust store and neither `revoked` nor `blocked`.
    ///
    /// Peer records may be keyed by the hex `peer_id` *or* by the base64url
    /// `identity_pub` depending on whether the originating invite carried an
    /// explicit `inviter_peer_id`, so we probe by both `get` (hex key) and
    /// `get_by_identity` (base64url field) to resolve reliably.
    ///
    /// This is what makes supernode-assisted chat safe: a supernode will relay
    /// any peer-targeted message, but a receiver only honours chat/call
    /// signaling from peers it already trusts. Two mutually-trusted peers can
    /// therefore fall back to relay when no direct P2P path exists, while an
    /// untrusted peer that merely shares a supernode cannot inject signaling.
    pub(crate) fn is_trusted_sender(peer_store: &Arc<RwLock<PeerStore>>, sender: &str) -> bool {
        let store = peer_store.read();
        store
            .get(sender)
            .or_else(|| store.get_by_identity(sender))
            .is_some_and(|rec| !rec.blocked && !rec.revoked)
    }

    pub(super) fn canonical_peer_id_for_sender(&self, sender: &str) -> String {
        let store = self.peer_store.read();
        store
            .get(sender)
            .or_else(|| store.get_by_identity(sender))
            .map(|rec| rec.peer_id.clone())
            .unwrap_or_else(|| sender.to_owned())
    }
}
