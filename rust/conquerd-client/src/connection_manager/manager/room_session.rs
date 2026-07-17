//! SFU rooms, group-key lifecycle, and cluster failover.

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

use crate::connection_fallback::{DirectFallbackCoordinator, PendingFallback};

use super::invite::{RoomInviteEntry, ROOM_INVITE_SCHEMA};
use super::{
    unix_now_f64, PendingGroupKeyAck, GROUP_KEY_MAX_ATTEMPTS, GROUP_KEY_RETRY_INTERVAL_MS,
};

/// The room group-key "elected keyer" tie-break: `me` acts iff it is present
/// in `members` and no other member present sorts before it lexicographically.
/// Every member evaluates this against the same authoritative membership
/// snapshot, so at most one of them distributes for a given snapshot — no
/// fixed "creator" required (see [`ConnectionManager::sync_room_membership`]
/// and `backlog.md` "Crypto — group key reliability").
pub fn is_elected_keyer(members: &[String], me: &str) -> bool {
    members.iter().any(|m| m == me) && !members.iter().any(|m| m.as_str() < me)
}

/// Epoch acceptance once the sender is known to be the elected keyer.
///
/// * No real key yet → accept any epoch (first install / dual-join race heal).
/// * Otherwise only the current epoch (reseal after reconnect) or the next
///   rotation (`cur.wrapping_add(1)`) — rejects hostile epoch jumps.
pub fn accept_group_key_epoch(has_real_key: bool, current_epoch: u8, offered: u8) -> bool {
    if !has_real_key {
        return true;
    }
    offered == current_epoch || offered == current_epoch.wrapping_add(1)
}

/// Whether the elected keyer should mint the first real room key now.
///
/// Defers until at least one *other* member is visible so two solo joiners
/// cannot each mint a different epoch-0 key (dual-keyer bootstrap race).
pub fn should_mint_first_room_key(
    is_elected: bool,
    has_real_key: bool,
    other_member_count: usize,
) -> bool {
    is_elected && !has_real_key && other_member_count > 0
}

/// Fail-closed gate for outbound room E2E content (audio / chat / file chunks).
///
/// Real (distributed) key material is required so the supernode cannot derive
/// the content key from `room_id` alone. The deterministic epoch-0 fallback is
/// intentionally *not* used for outbound sends.
pub fn may_send_room_e2e_content(has_real_key: bool) -> bool {
    has_real_key
}

/// Composite key for pending materialize / private-join / room-store maps.
pub fn room_scope_key(supernode_id: &str, room_id: &str) -> String {
    format!("{supernode_id}:{room_id}")
}

/// Normalize client-supplied `room_type` for `SfuRoomCreate` (unknown → public).
pub fn normalize_room_type(room_type: &str) -> &'static str {
    match room_type.trim().to_ascii_lowercase().as_str() {
        "private" => "private",
        _ => "public",
    }
}

/// Track `pending_materialize` only when replaying a client-owned definition
/// with a known room id (reconnect / GC rematerialize).
pub fn should_track_pending_materialize(materialize_only: bool, room_id: Option<&str>) -> bool {
    materialize_only && room_id.is_some_and(|s| !s.is_empty())
}

/// After a successful `SfuRoomCreated` ack: auto-join + emit `RoomCreated` only
/// for user-initiated creates. Materialize-only, denied, and empty room_id
/// must never auto-join (reconnect must not steal the active voice room).
pub fn should_auto_join_on_room_created(
    denied: bool,
    room_id_empty: bool,
    materialize_only: bool,
) -> bool {
    !denied && !room_id_empty && !materialize_only
}

/// Whether `join_room` should take the private invite round-trip
/// (`JoinRoomWithInvite`) instead of a plain `SfuJoin`.
///
/// Creators and already-admitted peers skip the single-use token path.
/// Shared with the Qt bridge so UI and CM cannot diverge on this policy.
pub fn should_use_private_room_invite(
    already_admitted: bool,
    is_private: bool,
    is_creator: bool,
    has_invite_token: bool,
) -> bool {
    !already_admitted && is_private && !is_creator && has_invite_token
}

/// Cluster-wide union of a room's members across every supernode snapshot.
///
/// `snapshots` is keyed by `"{supernode_id}:{room_id}"` (per-node last-seen
/// member sets, excluding self). Under clustering the same logical room is
/// hosted on multiple members, each with its own snapshot; the union is the
/// authoritative "who is in this room anywhere" set used for keyer election and
/// join/leave diffing, so a peer present on any node is never mistaken for
/// having left. Supernode ids are base64url and room ids hex, so neither
/// contains `':'` — the single separator makes the `":{room_id}"` suffix exact.
pub fn union_members_for_room(
    snapshots: &HashMap<String, HashSet<String>>,
    room_id: &str,
) -> HashSet<String> {
    let suffix = format!(":{room_id}");
    let mut union = HashSet::new();
    for (key, members) in snapshots {
        if key.ends_with(&suffix) {
            union.extend(members.iter().cloned());
        }
    }
    union
}

/// Parameters for `send_room_create`, bundled into one struct to keep the
/// function under clippy's argument-count lint — every field maps 1:1 to a
/// `SfuRoomCreate` wire field or a client-only replay/materialize flag, so
/// there is no natural way to shrink the field count further.
pub(super) struct RoomCreateRequest<'a> {
    pub(super) supernode_id: &'a str,
    pub(super) room_name: &'a str,
    pub(super) room_type: &'a str,
    pub(super) room_id: Option<&'a str>,
    pub(super) creator_id: Option<&'a str>,
    pub(super) materialize_only: bool,
    pub(super) invite_policy: &'a str,
    /// Client-held invite credential to re-seed post-GC (empty on first create).
    pub(super) invite_token: &'a str,
}

/// What to do with a room whose hosting supernode was just lost, given the
/// verified cluster siblings we could move it to.
#[derive(Debug, PartialEq, Eq)]
pub enum FailoverPlan {
    /// No verified sibling advertises a signaling address — nothing to do.
    None,
    /// Resume the room across the cluster. We cannot know which sibling still
    /// has the room materialized (a denied join returns *no* response, so we
    /// can't probe one-at-a-time), so we attempt the rejoin on **every** live
    /// sibling at once — whichever holds the room answers with `SfuMembers`,
    /// the rest silently deny — and arm the not-yet-connected ones for a replay
    /// when they come back.
    Fanout {
        /// Sibling ids with a live session; attempt the rejoin on all now.
        live: Vec<String>,
        /// `(identity_pub, ws_url)` for siblings without a live session; arm
        /// each for a rejoin on its next connect and dial the ones we have no
        /// session task for yet.
        cold: Vec<(String, String)>,
    },
}

/// Plan how a lost room should resume, given `targets` in roster order and a
/// `session` lookup returning `Some(connected)` when we hold a session with
/// that sibling (`None` when we have never dialed it).
///
/// Eager multi-homing keeps a session open to every sibling, so the common case
/// is a non-empty `live` set and an immediate rejoin with no dial and no wait.
pub fn plan_cluster_failover(
    targets: &[(String, String)],
    session: impl Fn(&str) -> Option<bool>,
) -> FailoverPlan {
    if targets.is_empty() {
        return FailoverPlan::None;
    }
    let mut live = Vec::new();
    let mut cold = Vec::new();
    for (id, url) in targets {
        if session(id) == Some(true) {
            live.push(id.clone());
        } else {
            cold.push((id.clone(), url.clone()));
        }
    }
    FailoverPlan::Fanout { live, cold }
}

impl ConnectionManager {
    /// Store the verified cluster siblings of `supernode_id` for failover. The
    /// roster has already been signature-checked against `supernode_id`.
    pub(super) fn record_cluster_members(
        &mut self,
        supernode_id: &str,
        members: &[crate::cluster::ClusterMember],
    ) {
        if members.is_empty() {
            self.cluster_members.remove(supernode_id);
            return;
        }
        self.cluster_members
            .insert(supernode_id.to_owned(), members.to_vec());
        // Log the resolved failover attach points, reusing the same ws scheme as
        // the supernode we're connected to.
        let scheme = self
            .supernodes
            .get(supernode_id)
            .and_then(|sn| sn.ws_url.split("://").next())
            .unwrap_or("ws")
            .to_owned();
        let urls = self.cluster_failover_ws_urls(supernode_id, &scheme);
        debug!(
            "cluster: {} failover attach point(s) for supernode {}: {:?}",
            urls.len(),
            &supernode_id[..12.min(supernode_id.len())],
            urls
        );
        // Surface the verified roster to the UI so it can replay client-owned
        // rooms saved under a sibling's identity onto this supernode too (a
        // cluster presents as one logical supernode to peers).
        let member_ids: Vec<String> = members
            .iter()
            .map(|m| m.identity_pub.trim_end_matches('=').to_owned())
            .collect();
        self.emit_event(ConnectionEvent::ClusterMembersUpdated {
            supernode_id: supernode_id.to_owned(),
            members: member_ids,
        });
    }

    /// Proactively open sessions to any verified cluster siblings of
    /// `supernode_id` we don't already have one with. Without this, a peer
    /// that only ever opened one session to the cluster becomes unreachable
    /// the instant that single node goes down — other members can't relay
    /// `EncryptedSignal`/room traffic to it because they have no live session
    /// to route through (`"Relay target ... not connected"` on the supernode).
    /// Reuses the same signature-verified roster `maybe_failover_to_cluster`
    /// uses reactively; this just does it eagerly instead of waiting for our
    /// own room's host to die. Purely a runtime session — does not touch
    /// `PeerStore` trust (same as the existing reactive failover path).
    pub(super) async fn connect_cluster_siblings(&mut self, supernode_id: &str) {
        let scheme = self
            .supernodes
            .get(supernode_id)
            .and_then(|sn| sn.ws_url.split("://").next())
            .unwrap_or("ws")
            .to_owned();
        let targets: Vec<(String, String)> = self
            .cluster_sibling_targets(supernode_id, &scheme)
            .into_iter()
            .filter(|(id, _)| !self.supernodes.contains_key(id))
            .collect();
        for (sibling_id, ws_url) in targets {
            info!(
                "cluster: multi-homing to sibling {} at {} (reachability for cluster failover)",
                &sibling_id[..12.min(sibling_id.len())],
                ws_url
            );
            self.connect_supernode_ws(sibling_id, vec![ws_url]).await;
        }
    }

    /// Ordered WebSocket attach-point URLs for `supernode_id`'s verified
    /// siblings. Pure read of the stored, verified roster.
    pub(super) fn cluster_failover_ws_urls(&self, supernode_id: &str, scheme: &str) -> Vec<String> {
        self.cluster_sibling_targets(supernode_id, scheme)
            .into_iter()
            .map(|(_, url)| url)
            .collect()
    }

    /// Every verified `(sibling_identity_pub, ws_url)` attach point for
    /// `supernode_id`, in roster order. Siblings we already hold a session with
    /// are **included** — failover needs to see them precisely because a live
    /// session is the cheapest place to resume the room.
    pub(super) fn cluster_sibling_targets(
        &self,
        supernode_id: &str,
        scheme: &str,
    ) -> Vec<(String, String)> {
        let Some(members) = self.cluster_members.get(supernode_id) else {
            return Vec::new();
        };
        members
            .iter()
            .map(|m| (m.identity_pub.trim_end_matches('=').to_owned(), m))
            .filter_map(|(id, m)| m.ws_url(scheme).map(|url| (id, url)))
            .collect()
    }

    /// When the supernode hosting our current room is lost, move the room to a
    /// verified cluster sibling. Guarded so the per-retry disconnect storm
    /// triggers this once.
    ///
    /// A denied `SfuJoin` (e.g. the room isn't materialized on that member)
    /// returns *no* response, so we can't probe siblings one at a time. Instead
    /// we fan the rejoin out to **every** live sibling at once: the member that
    /// still holds the room answers with `SfuMembers` (which promotes it to
    /// `current_supernode_id`), and the rest silently deny. Siblings that aren't
    /// connected yet — plus the node we just lost — are armed to replay the join
    /// when they return, so the room only truly dies once no member is reachable.
    pub(super) async fn maybe_failover_to_cluster(&mut self, lost_supernode: &str, room_id: &str) {
        if room_id.is_empty() || self.failover_in_progress.contains(lost_supernode) {
            return;
        }
        let scheme = self
            .supernodes
            .get(lost_supernode)
            .and_then(|sn| sn.ws_url.split("://").next())
            .unwrap_or("ws")
            .to_owned();
        let targets = self.cluster_sibling_targets(lost_supernode, &scheme);
        let plan = plan_cluster_failover(&targets, |id| {
            self.supernodes.get(id).map(|sn| sn.connected)
        });

        let FailoverPlan::Fanout { live, cold } = plan else {
            return; // FailoverPlan::None — no verified sibling to fail over to
        };
        self.failover_in_progress.insert(lost_supernode.to_owned());
        self.current_room_id = room_id.to_owned();

        // Attempt the rejoin on every live sibling now. Whichever still has the
        // room replies with `SfuMembers`; until one does, point outbound room
        // ops at the first live sibling so they have somewhere to go. The
        // `SfuMembers` handler reassigns `current_supernode_id` to the actual
        // responder (which may differ from this optimistic guess).
        if let Some(first) = live.first() {
            self.current_supernode_id = first.clone();
            self.failover_pending_room = Some(room_id.to_owned());
            info!(
                "Cluster failover: supernode {} lost — attempting rejoin on {} live sibling(s)",
                &lost_supernode[..12.min(lost_supernode.len())],
                live.len()
            );
            for sibling_id in &live {
                self.send_room_join(sibling_id, room_id).await;
                self.ensure_room_relay(sibling_id).await;
            }
        }

        // Arm every not-yet-live sibling to replay the join when it (re)connects,
        // dialing the ones we have no session task for. The first to come back
        // and accept the room wins.
        //
        // Only arm the node we just lost when there is *no* live sibling to fan
        // out to — its return is a valid resume path then. When a fan-out is in
        // flight, arming it would let the fresh, roomless node (post-restart,
        // before roster gossip refills it) hijack a working failover and strand
        // us on `room_absent`. A successful fan-out also disarms these on
        // confirmation, but not arming it here removes the race entirely.
        if live.is_empty() {
            self.pending_failover_rejoin
                .insert(lost_supernode.to_owned(), room_id.to_owned());
        }
        for (sibling_id, ws_url) in cold {
            self.pending_failover_rejoin
                .insert(sibling_id.clone(), room_id.to_owned());
            // A sibling with an existing session has a task already retrying on
            // its own backoff; only one we have never dialed needs one spawned.
            if !self.supernodes.contains_key(&sibling_id) {
                self.connect_supernode_ws(sibling_id, vec![ws_url]).await;
            }
        }
    }

    /// Seal `inner` into an `EncryptedSignal` envelope addressed to `member_pub`
    /// (a room member's public_id, which *is* their Ed25519 identity key), using
    /// the deterministic pairwise key. Unlike [`Self::maybe_wrap_for_relay`] this
    /// does not consult the peer store, so it works for room members we have no
    /// prior relationship with. The supernode routes the envelope by `target` and
    /// never sees the sealed group key. Returns the signed envelope to dispatch.
    pub(super) fn seal_signal_to_member(
        &self,
        inner: &SignalingMessage,
        member_pub: &str,
    ) -> Option<SignalingMessage> {
        let inner_json = inner.to_json().ok()?;
        let key = self.identity.derive_pairwise_relay_key(member_pub).ok()?;
        let ciphertext = crate::crypto::encrypt_blob(&key, inner_json.as_bytes()).ok()?;
        let ciphertext_b64 = crate::crypto::b64url_encode(&ciphertext);
        let mut env =
            SignalingMessage::new(MessageType::EncryptedSignal, self.identity.public_id());
        env.target = Some(member_pub.to_owned());
        env.payload
            .insert("ciphertext".to_owned(), Value::String(ciphertext_b64));
        Some(env)
    }

    /// Owner: seal the group key for `(room_id, epoch)` to each member and send
    /// it (inside an `EncryptedSignal` envelope) so the supernode forwards it
    /// blind. `members` must already exclude ourselves.
    ///
    /// The **inner** `SfuGroupKey` is Ed25519-signed before sealing. The
    /// receiver unwraps the envelope and re-dispatches the inner message through
    /// the full inbound pipeline (signature + freshness + replay). An unsigned
    /// inner is dropped as "signature missing", so the peer never installs the
    /// epoch key, stays on the deterministic fallback, and E2E room audio is
    /// silenced for both sides (keyer seals under the real key; peer cannot open).
    ///
    /// Each successful send is tracked in [`Self::pending_group_key_acks`] until
    /// the member returns a sealed `SfuGroupKeyAck` (or we give up / they leave).
    /// Lost envelopes are re-sealed on a short timer — see
    /// [`Self::retry_pending_group_keys`].
    pub(super) async fn distribute_group_key(
        &mut self,
        room_id: &str,
        epoch: u8,
        key: &[u8; 32],
        members: &[String],
    ) {
        let sender = self.identity.public_id();
        let key_b64 = crate::crypto::b64url_encode(key);
        for member in members {
            let mut inner = SignalingMessage::new(MessageType::SfuGroupKey, sender.clone());
            inner
                .payload
                .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
            inner
                .payload
                .insert("epoch".to_owned(), Value::Number((epoch as u64).into()));
            inner
                .payload
                .insert("key".to_owned(), Value::String(key_b64.clone()));
            // Sign inner before seal — see doc comment above.
            let Ok(canonical) = inner.canonical_bytes() else {
                warn!(
                    "[group-key] could not canonicalize SfuGroupKey for {}",
                    &member[..8.min(member.len())]
                );
                continue;
            };
            let sig = self.identity.sign(&canonical);
            use base64::Engine;
            inner.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
            if let Some(env) = self.seal_signal_to_member(&inner, member) {
                self.dispatch_outbound(env).await;
                let now = std::time::Instant::now();
                self.pending_group_key_acks
                    .entry((room_id.to_owned(), member.clone()))
                    .and_modify(|p| {
                        p.epoch = epoch;
                        p.last_sent = now;
                        p.attempts = p.attempts.saturating_add(1);
                    })
                    .or_insert(PendingGroupKeyAck {
                        epoch,
                        last_sent: now,
                        attempts: 1,
                    });
            } else {
                warn!(
                    "[group-key] could not seal group key to {}",
                    &member[..8.min(member.len())]
                );
            }
        }
    }

    /// Whether to install a sealed `SfuGroupKey` from `sender` for `room_id` at
    /// `epoch`. Requires the sender to be the elected keyer for the current
    /// membership view (including the sender if our snapshot is still empty —
    /// join race) and the epoch to be the current one, the next rotation, or
    /// any epoch when we have no real key yet.
    pub(super) fn accept_group_key_from(&self, sender: &str, room_id: &str, epoch: u8) -> bool {
        let me = self.identity.public_id();
        let union = union_members_for_room(&self.room_group_members, room_id);
        let mut present: Vec<String> = union.iter().cloned().collect();
        present.push(me);
        if !present.iter().any(|m| m == sender) {
            present.push(sender.to_owned());
        }
        if !is_elected_keyer(&present, sender) {
            return false;
        }
        let has_real = self.group_keys.has_real_key(room_id);
        let cur = self.group_keys.current_epoch(room_id);
        accept_group_key_epoch(has_real, cur, epoch)
    }

    /// Member → keyer: confirm we installed `(room_id, epoch)`. Sealed the same
    /// way as `SfuGroupKey` so the supernode never sees the ack in the clear.
    pub(super) async fn send_group_key_ack(&mut self, room_id: &str, epoch: u8, keyer: &str) {
        let mut inner =
            SignalingMessage::new(MessageType::SfuGroupKeyAck, self.identity.public_id());
        inner
            .payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        inner
            .payload
            .insert("epoch".to_owned(), Value::Number((epoch as u64).into()));
        let Ok(canonical) = inner.canonical_bytes() else {
            warn!(
                "[group-key] could not canonicalize SfuGroupKeyAck for {}",
                &keyer[..8.min(keyer.len())]
            );
            return;
        };
        let sig = self.identity.sign(&canonical);
        use base64::Engine;
        inner.signature = Some(base64::engine::general_purpose::URL_SAFE.encode(sig));
        if let Some(env) = self.seal_signal_to_member(&inner, keyer) {
            self.dispatch_outbound(env).await;
        } else {
            warn!(
                "[group-key] could not seal SfuGroupKeyAck to {}",
                &keyer[..8.min(keyer.len())]
            );
        }
    }

    /// Reseal any un-acked group keys (lost EncryptedSignal / offline peer).
    /// Called on a short timer from the connection manager run loop.
    pub(super) async fn retry_pending_group_keys(&mut self) {
        if self.pending_group_key_acks.is_empty() {
            return;
        }
        let now = std::time::Instant::now();
        let interval = Duration::from_millis(GROUP_KEY_RETRY_INTERVAL_MS);
        let me = self.identity.public_id();

        let mut drop_keys: Vec<(String, String)> = Vec::new();
        let mut retries: Vec<(String, String, u8)> = Vec::new();

        for ((room_id, member), pending) in &self.pending_group_key_acks {
            let union = union_members_for_room(&self.room_group_members, room_id);
            if !union.contains(member) {
                drop_keys.push((room_id.clone(), member.clone()));
                continue;
            }
            let mut present: Vec<String> = union.iter().cloned().collect();
            present.push(me.clone());
            if !is_elected_keyer(&present, &me) {
                // Another peer is now keyer — they will distribute.
                drop_keys.push((room_id.clone(), member.clone()));
                continue;
            }
            if pending.attempts >= GROUP_KEY_MAX_ATTEMPTS {
                warn!(
                    "[group-key] giving up waiting for ack from {} room {} epoch {} after {} attempts",
                    &member[..8.min(member.len())],
                    &room_id[..8.min(room_id.len())],
                    pending.epoch,
                    pending.attempts
                );
                drop_keys.push((room_id.clone(), member.clone()));
                continue;
            }
            if now.duration_since(pending.last_sent) >= interval {
                retries.push((room_id.clone(), member.clone(), pending.epoch));
            }
        }

        for k in drop_keys {
            self.pending_group_key_acks.remove(&k);
        }

        for (room_id, member, epoch) in retries {
            let Some(key) = self.group_keys.epoch_key(&room_id, epoch) else {
                // Key material gone (forgot / rotated out of retention) — stop.
                self.pending_group_key_acks.remove(&(room_id, member));
                continue;
            };
            debug!(
                "[group-key] resealing epoch {} to {} for room {} (awaiting ack)",
                epoch,
                &member[..8.min(member.len())],
                &room_id[..8.min(room_id.len())]
            );
            self.distribute_group_key(&room_id, epoch, &key, &[member])
                .await;
        }
    }

    /// Reconcile a room's group key against the current, authoritative member
    /// set. Any member holding real (distributed) key material for `room_id`
    /// — not just whoever created it — can act as its "keyer": it bootstraps
    /// the first epoch, rotates on departure (forward secrecy / PCS), or seals
    /// the current epoch to newcomers. Exactly one member acts via a
    /// deterministic tie-break (the lexicographically smallest `public_id`
    /// currently present) — every member computes the same winner from the same
    /// authoritative set, so this needs no fixed "creator" at all, which is what
    /// lets it also cover the built-in `default` room (no client-side creator).
    ///
    /// The membership set is the **cluster-wide union** across every supernode
    /// we're multi-homed to, not the single `supernode_id` snapshot that carried
    /// this update. That matters under clustering: the same logical room is
    /// hosted on several members, and each sends its own `SfuMembers`. If we
    /// diffed a single node's snapshot, a peer that had merely not yet joined on
    /// *this* node (but is present on a sibling) would look like a departure and
    /// trigger a **spurious key rotation**, advancing the epoch and stranding
    /// that peer on stale key material — silencing E2E audio. Diffing the union
    /// means a member counts as present while on any node, and a rotation fires
    /// only on a true, cluster-wide leave.
    pub(super) async fn sync_room_membership(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        members: &[String],
    ) {
        let room_key = format!("{supernode_id}:{room_id}");
        let me = self.identity.public_id();

        // Cluster-wide union of this room's members BEFORE applying this node's
        // snapshot, then apply the snapshot and recompute. Snapshots exclude us,
        // so the union does too.
        let union_old = union_members_for_room(&self.room_group_members, room_id);
        let node_new: HashSet<String> = members.iter().filter(|m| **m != me).cloned().collect();
        self.room_group_members.insert(room_key, node_new);
        let union_new = union_members_for_room(&self.room_group_members, room_id);

        // Keyer election over the union (plus us): only the deterministic winner
        // across the whole cluster distributes, so members never disagree on who
        // keys or race competing epochs once they share a view.
        let mut present: Vec<String> = union_new.iter().cloned().collect();
        present.push(me.clone());
        if !is_elected_keyer(&present, &me) {
            // Not the keyer: drop any pending seals we queued while we briefly
            // thought we were (solo bootstrap race). Keep installed key material
            // until a legitimate keyer's SfuGroupKey overwrites it.
            self.pending_group_key_acks.retain(|(r, _), _| r != room_id);
            return;
        }

        let removed = union_old.difference(&union_new).count() > 0;
        let added: Vec<String> = union_new.difference(&union_old).cloned().collect();

        // Drop pending acks for members who left this room entirely.
        self.pending_group_key_acks
            .retain(|(r, m), _| r != room_id || union_new.contains(m));

        // Defer first real-key generation until at least one *other* member is
        // present. Solo minting was the dual-keyer bootstrap race: both peers
        // join alone, each mint a different epoch-0 key, then cannot open each
        // other's audio until a later reseal. Waiting for a non-empty union
        // means only the elected keyer mints once both are visible.
        let has_real = self.group_keys.has_real_key(room_id);
        if should_mint_first_room_key(true, has_real, union_new.len()) {
            // First keying: generate epoch 0 and seal to everyone present.
            // (Caller already established we are elected keyer.)
            let (epoch, key) = self.group_keys.new_owner_epoch(room_id);
            let all: Vec<String> = union_new.iter().cloned().collect();
            self.distribute_group_key(room_id, epoch, &key, &all).await;
        } else if has_real && removed {
            // A member left the cluster entirely → rotate for forward secrecy
            // and reseal to the rest.
            let (epoch, key) = self.group_keys.rotate(room_id);
            let all: Vec<String> = union_new.iter().cloned().collect();
            // Stale-epoch pendings for this room are obsolete after rotate.
            self.pending_group_key_acks.retain(|(r, _), _| r != room_id);
            self.distribute_group_key(room_id, epoch, &key, &all).await;
        } else if has_real && !added.is_empty() {
            // Pure join(s) → seal the current epoch to the newcomers only.
            let epoch = self.group_keys.current_epoch(room_id);
            if let Some(key) = self.group_keys.epoch_key(room_id, epoch) {
                self.distribute_group_key(room_id, epoch, &key, &added)
                    .await;
            }
        }
        // else: elected but alone (or still no real key and no others) — wait.
    }

    /// Request a relay grant for `supernode_id` so room audio can ride QUIC
    /// datagrams. No-op when a live relay session already exists. The grant
    /// flow (`RelayGranted` → background connect) is best-effort; room audio
    /// transparently falls back to the WebSocket SFU path if it never lands.
    pub(super) async fn ensure_room_relay(&mut self, supernode_id: &str) {
        if self
            .quic_relays
            .get(supernode_id)
            .is_some_and(|r| r.is_alive())
        {
            return;
        }
        self.request_relay(supernode_id).await;
    }

    pub(super) async fn request_relay(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::RelayRequest, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("requester".to_owned(), Value::String(sender));
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_room_join(&mut self, supernode_id: &str, room_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuJoin, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload
            .insert("peer_id".to_owned(), Value::String(sender));
        // Attach Space proof-based admission creds carried by the invite we used
        // to reach this room (single-use), so the supernode can admit + materialize
        // it by proof on any cluster member. Absent → legacy ACL applies.
        if let Some((root, proof, grant)) = self.pending_join_space_creds.remove(room_id) {
            for (key, text) in [
                ("space_root", root),
                ("space_proof", proof),
                ("space_grant", grant),
            ] {
                if !text.is_empty() {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        msg.payload.insert(key.to_owned(), v);
                    }
                }
            }
        }
        self.dispatch_outbound(msg).await;
    }

    /// Announce a signed Space root to `supernode_id` (authenticated room-set
    /// sync). `root_json` is a serialized `SignedSpaceRoot`; the supernode
    /// verifies + stores + cluster-gossips it.
    pub(super) async fn send_space_root_announce(&mut self, supernode_id: &str, root_json: &str) {
        let Ok(root) = serde_json::from_str::<Value>(root_json) else {
            return;
        };
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SpaceRootAnnounce, sender);
        msg.target = Some(supernode_id.to_owned());
        msg.payload.insert("root".to_owned(), root);
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_room_leave(&mut self, supernode_id: &str, room_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuLeave, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("peer_id".to_owned(), Value::String(sender));
        let rid = if room_id.is_empty() {
            "default".to_owned()
        } else {
            room_id.to_owned()
        };
        msg.payload.insert("room_id".to_owned(), Value::String(rid));
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_room_subscribe(&mut self, supernode_id: &str, room_id: &str) {
        // Establish a QUIC relay session (if not already up) so room chat/file
        // ride the reliable signaling stream rather than the WebSocket path —
        // even for chat-only rooms with no active voice. No-op if a live relay
        // already exists; room messaging still works over WS if the grant
        // never lands.
        self.ensure_room_relay(supernode_id).await;
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuSubscribe, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_room_unsubscribe(&mut self, supernode_id: &str, room_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuUnsubscribe, sender);
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_room_invite(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        invite_token: &str,
    ) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuRoomInvite, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        msg.payload.insert(
            "invite_token".to_owned(),
            Value::String(invite_token.to_owned()),
        );
        // Attach the Space proof-based admission creds carried by the invite link
        // (mirrors `send_room_join`) so the supernode can verify the proof,
        // materialize the room, and adopt its proven owner on any cluster member
        // — without this the invite is token-only and the proof path never runs.
        // Peek (not consume): the follow-up `SfuJoin` removes them after accept.
        if let Some((root, proof, grant)) = self.pending_join_space_creds.get(room_id).cloned() {
            for (key, text) in [
                ("space_root", root),
                ("space_proof", proof),
                ("space_grant", grant),
            ] {
                if !text.is_empty() {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        msg.payload.insert(key.to_owned(), v);
                    }
                }
            }
        }
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_room_create(&mut self, req: RoomCreateRequest<'_>) {
        let RoomCreateRequest {
            supernode_id,
            room_name,
            room_type,
            room_id,
            creator_id,
            materialize_only,
            invite_policy,
            invite_token,
        } = req;
        let normalized = normalize_room_type(room_type);
        if should_track_pending_materialize(materialize_only, room_id) {
            if let Some(rid) = room_id.filter(|s| !s.is_empty()) {
                self.pending_materialize
                    .insert(room_scope_key(supernode_id, rid));
            }
        }
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuRoomCreate, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_name".to_owned(), Value::String(room_name.to_owned()));
        msg.payload
            .insert("room_type".to_owned(), Value::String(normalized.to_owned()));
        if let Some(rid) = room_id.filter(|s| !s.is_empty()) {
            msg.payload
                .insert("room_id".to_owned(), Value::String(rid.to_owned()));
        }
        if let Some(cid) = creator_id.filter(|s| !s.is_empty()) {
            msg.payload
                .insert("creator_id".to_owned(), Value::String(cid.to_owned()));
        }
        if !invite_policy.is_empty() {
            msg.payload.insert(
                "invite_policy".to_owned(),
                Value::String(invite_policy.to_owned()),
            );
        }
        // Re-seed the durable invite credential after idle GC so the supernode
        // can re-admit this peer (and validate SfuRoomInvite with the same
        // token). Empty on first create — the supernode mints a fresh token.
        if !invite_token.is_empty() {
            msg.payload.insert(
                "invite_token".to_owned(),
                Value::String(invite_token.to_owned()),
            );
        }
        info!(
            "[cm] SfuRoomCreate: supernode={} name={room_name} type={normalized} materialize_only={materialize_only} has_token={}",
            &supernode_id[..8.min(supernode_id.len())],
            !invite_token.is_empty()
        );
        self.dispatch_outbound(msg).await;
    }

    #[inline]
    pub(super) fn check_room_audio_outbound_quota(&self, target: &str, byte_count: usize) -> bool {
        self.feature_registry
            .gate_through_feature("room.audio.sfu", target, byte_count)
    }

    /// Send a room audio frame to the supernode for SFU fan-out.
    ///
    /// Prefers an unreliable QUIC **relay datagram** when a live relay session
    /// to the room's supernode exists: datagrams avoid the TCP head-of-line
    /// blocking that dominates room-audio latency on the WebSocket path. The
    /// frame is the *same signed `SfuAudio` JSON* either way, so the receiver
    /// verifies the sender's Ed25519 signature identically and the supernode
    /// stays a dumb forwarder. Falls back to the WebSocket SFU path when no
    /// relay session is available or the datagram could not be sent.
    ///
    /// Outbound quota uses `room.audio.sfu` (gated against the supernode peer id).
    /// See `send_audio_datagram` for the direct P2P `core.audio.opus` path.
    ///
    /// Quota is charged on the **signed wire size** (`1 + json.len()` for the
    /// relay tag + envelope), matching the supernode inbound gate — not raw
    /// Opus length (which under-counted by ~3–5× and let the client flood past
    /// the supernode's 32 KiB/s cap before that was raised).
    pub(super) async fn send_room_audio(&mut self, opus_data: Vec<u8>) {
        if self.current_room_id.is_empty() || self.current_supernode_id.is_empty() {
            return; // Not in a room
        }
        let sender = self.identity.public_id();
        let room_id = self.current_room_id.clone();
        let supernode_id = self.current_supernode_id.clone();
        use base64::Engine;

        // E2E-seal under real group-key material only. The deterministic
        // fallback is not supernode-opaque (key = f(room_id)); drop until the
        // elected keyer's SfuGroupKey is installed. A few 20 ms frames of
        // silence at join is preferable to content the relay can derive.
        if !may_send_room_e2e_content(self.group_keys.has_real_key(&room_id)) {
            warn!("[room.audio.sfu] no real group key yet; dropping frame");
            return;
        }
        let seq = self.room_audio_seq;
        let Some(sealed) = crate::group_key::seal_voice_frame(
            &self.group_keys,
            &room_id,
            &sender,
            seq,
            &opus_data,
        ) else {
            warn!("[room.audio.sfu] seal failed; dropping frame");
            return;
        };
        self.room_audio_seq = self.room_audio_seq.wrapping_add(1);
        let audio_b64 = base64::engine::general_purpose::URL_SAFE.encode(&sealed);
        let mut msg = SignalingMessage::new(MessageType::SfuAudio, sender);
        msg.target = Some(supernode_id.clone());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id));
        msg.payload
            .insert("audio".to_owned(), Value::String(audio_b64));
        msg.payload.insert("e2e".to_owned(), Value::Bool(true));
        msg.payload
            .insert("seq".to_owned(), Value::Number(seq.into()));

        // Sign once so both the relay path and the WS fallback share the same
        // wire bytes — and so outbound quota can charge the real envelope size
        // (ROOM_AUDIO_TAG + signed JSON), matching supernode inbound accounting.
        let Some(json) = self.sign_message_json(&mut msg) else {
            return;
        };
        // +1 for ROOM_AUDIO_TAG on the relay datagram path (WS is comparable).
        let wire_bytes = json.len().saturating_add(1);
        if !self.check_room_audio_outbound_quota(&supernode_id, wire_bytes) {
            debug!(
                "[room.audio.sfu] outbound quota exceeded for {}; dropping frame",
                &supernode_id[..8.min(supernode_id.len())]
            );
            return;
        }

        // Fast path: relay datagram (no TCP head-of-line blocking), unless we're
        // in a WS cooldown after repeated relay failures (anti-thrash). The Arc
        // clone drops the `self.quic_relays` borrow before we send / fall back.
        let try_relay = self.room_relay_cooldown_frames == 0;
        if self.room_relay_cooldown_frames > 0 {
            self.room_relay_cooldown_frames -= 1;
        }
        let relay = if try_relay {
            self.quic_relays
                .get(&supernode_id)
                .filter(|r| r.is_alive())
                .cloned()
        } else {
            None
        };
        if let Some(relay) = relay {
            if relay.send_room_audio(json.as_bytes()) {
                self.room_relay_fail_streak = 0;
                return;
            }
            // Relay path is unhealthy; after a short streak, prefer WS for a
            // ~3 s cooldown (≈150 frames at 50 fps) rather than retrying — and
            // probably failing — on every frame.
            self.room_relay_fail_streak += 1;
            if self.room_relay_fail_streak >= 5 {
                self.room_relay_cooldown_frames = 150;
                self.room_relay_fail_streak = 0;
                debug!("[room.audio.sfu] relay datagram unhealthy; using WS for ~3 s");
            }
        }
        // Fallback: WebSocket SFU relay. Message is already signed.
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_sfu_chat(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        body: &str,
        sender_handle: &str,
        message_id: &str,
    ) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuChat, sender.clone());
        msg.target = Some(supernode_id.to_owned());
        msg.payload
            .insert("room_id".to_owned(), Value::String(room_id.to_owned()));
        // E2E-seal the body under the room group key (`AAD = room_id ‖ sender ‖
        // message_id`). Fail closed until real (distributed) key material is
        // installed — the deterministic fallback is not confidential vs. the
        // supernode (it knows `room_id`), and cleartext is worse. Keying is
        // near-instant at join with ACK/reseal.
        if !may_send_room_e2e_content(self.group_keys.has_real_key(room_id)) {
            warn!("[room.chat] no real group key for room yet; dropping outbound message");
            return;
        }
        let Some((epoch, sealed)) = crate::group_key::seal_chat_body(
            &self.group_keys,
            room_id,
            &sender,
            message_id,
            body.as_bytes(),
        ) else {
            warn!("[room.chat] seal failed; dropping outbound message");
            return;
        };
        msg.payload.insert(
            "body".to_owned(),
            Value::String(crate::crypto::b64url_encode(&sealed)),
        );
        msg.payload.insert("e2e".to_owned(), Value::Bool(true));
        msg.payload
            .insert("epoch".to_owned(), Value::Number((epoch as u64).into()));
        msg.payload.insert(
            "sender_handle".to_owned(),
            Value::String(sender_handle.to_owned()),
        );
        if !message_id.is_empty() {
            msg.payload.insert(
                "message_id".to_owned(),
                Value::String(message_id.to_owned()),
            );
        }
        self.dispatch_outbound(msg).await;
    }

    /// Direct QUIC to `peer_id` is unavailable — fall back to a temporary
    /// private SFU room on a trusted supernode.
    ///
    /// Flow: pick a supernode (connected preferred) → create a `direct-…`
    /// private room → on its `SfuRoomCreated` ack we auto-join and send the
    /// peer a `CallRequest` carrying the room coordinates + invite token (see
    /// [`Self::complete_direct_call_fallback`]). The callee joins the room on
    /// accept instead of waiting for a P2P path. Cancelled if direct QUIC
    /// recovers first (`QuicConnected`) or the call ends.
    pub(in crate::connection_manager) async fn start_direct_call_fallback(
        &mut self,
        peer_id: &str,
    ) {
        if self.direct_fallback.is_pending_for(peer_id) {
            return; // already in flight for this peer
        }
        let connected: HashSet<String> = self
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
        let supernode_id = DirectFallbackCoordinator::pick_supernode(
            trusted.iter().map(String::as_str),
            &connected,
        );
        if supernode_id.is_empty() {
            warn!(
                "Direct-call fallback for {}: no trusted supernode available",
                &peer_id[..8.min(peer_id.len())]
            );
            self.emit_event(ConnectionEvent::CallEnded {
                peer_id: peer_id.to_owned(),
            });
            return;
        }
        let counter = self.direct_fallback.next_counter();
        let room_id =
            DirectFallbackCoordinator::build_room_id(&self.identity.public_id(), peer_id, counter);
        info!(
            "Direct-call fallback for {}: creating temp private room {} on {}",
            &peer_id[..8.min(peer_id.len())],
            room_id,
            &supernode_id[..8.min(supernode_id.len())]
        );
        self.direct_fallback.set_pending(PendingFallback {
            peer_id: peer_id.to_owned(),
            room_id: room_id.clone(),
            supernode_id: supernode_id.clone(),
        });
        self.send_room_create(RoomCreateRequest {
            supernode_id: &supernode_id,
            room_name: "Direct call",
            room_type: "private",
            room_id: Some(&room_id),
            creator_id: None,
            materialize_only: false,
            invite_policy: "owner",
            invite_token: "",
        })
        .await;
    }

    /// Fire due direct-call fallback checks (armed when a callee accepted but
    /// no direct QUIC session existed). If the peer still isn't connected over
    /// QUIC when the grace deadline passes, start the private-room fallback.
    /// Runs on the 1 s reconnect tick.
    pub(super) async fn tick_call_fallback_checks(&mut self) {
        if self.pending_call_fallback_checks.is_empty() {
            return;
        }
        let now = Instant::now();
        let due: Vec<String> = self
            .pending_call_fallback_checks
            .iter()
            .filter(|(_, deadline)| **deadline <= now)
            .map(|(peer, _)| peer.clone())
            .collect();
        for peer_id in due {
            self.pending_call_fallback_checks.remove(&peer_id);
            let direct_connected = self
                .peers
                .get(&peer_id)
                .map(|p| p.state == PeerConnectionState::Connected)
                .unwrap_or(false);
            if !direct_connected {
                info!(
                    "No direct QUIC to {} within fallback grace — starting private-room fallback",
                    &peer_id[..8.min(peer_id.len())]
                );
                self.start_direct_call_fallback(&peer_id).await;
            }
        }
    }

    /// The pending fallback room was created (its `SfuRoomCreated` ack matched
    /// [`DirectFallbackCoordinator::is_pending_room`]) and we auto-joined it.
    /// Invite the original call target: send a `CallRequest` carrying the room
    /// coordinates + single-use invite token, and tell the local UI to switch
    /// audio to room mode via [`ConnectionEvent::CallFallbackRoomReady`].
    pub(super) async fn complete_direct_call_fallback(
        &mut self,
        supernode_id: &str,
        room_id: &str,
        invite_token: &str,
    ) {
        let Some(peer_id) = self.direct_fallback.pending().map(|p| p.peer_id.clone()) else {
            return;
        };
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::CallRequest, sender);
        msg.target = Some(peer_id.clone());
        msg.payload.insert(
            "fallback_supernode_id".to_owned(),
            Value::String(supernode_id.to_owned()),
        );
        msg.payload.insert(
            "fallback_room_id".to_owned(),
            Value::String(room_id.to_owned()),
        );
        msg.payload.insert(
            "fallback_invite_token".to_owned(),
            Value::String(invite_token.to_owned()),
        );
        self.dispatch_outbound(msg).await;
        self.emit_event(ConnectionEvent::CallFallbackRoomReady {
            peer_id,
            supernode_id: supernode_id.to_owned(),
            room_id: room_id.to_owned(),
        });
    }

    pub(super) async fn send_room_list_request(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SfuRoomList, sender);
        msg.target = Some(supernode_id.to_owned());
        self.dispatch_outbound(msg).await;
    }

    pub(super) async fn send_supernode_info_request(&mut self, supernode_id: &str) {
        let sender = self.identity.public_id();
        let mut msg = SignalingMessage::new(MessageType::SupernodeInfoRequest, sender);
        msg.target = Some(supernode_id.to_owned());
        self.dispatch_outbound(msg).await;
    }
}
