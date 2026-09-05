//! Intra-cluster transport — the supernode↔supernode replication fabric.
//!
//! Each cluster member runs one [`ClusterLink`]: a dedicated QUIC endpoint
//! (separate from the client-facing relay) that
//!
//! * **accepts** links from other members, authenticating the dialer's cert CN
//!   against the cluster roster ([`ClusterMembership::is_peer_member`]);
//! * **dials** every other member's `cluster_addr`, reconnecting with backoff;
//! * exchanges **Ed25519-signed** cluster messages, so a spoofed cert CN can
//!   never forge a subscription update or replicated frame;
//! * maintains a **remote subscription table** (`room_id → members that have
//!   local subscribers`) used to route replication; and
//! * forwards inbound [`ClusterMsgKind::Replicate`] frames to a callback (the
//!   SFU broadcast path consumes them in B.3).
//!
//! Wire framing on each cluster stream is one length-prefixed JSON
//! [`ClusterMsg`] per `[u32 BE len][json]`, matching the supernode's other
//! reliable stream encodings.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use quinn::Endpoint;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::cluster::ClusterMembership;
use crate::identity::Identity;
use crate::relay::{build_quinn_client_config, build_quinn_server_config, extract_peer_id};
use crate::space::SignedSpaceRoot;

/// Reconnect backoff bounds for dialing a peer member.
const DIAL_BACKOFF_START: Duration = Duration::from_secs(1);
const DIAL_BACKOFF_MAX: Duration = Duration::from_secs(30);
/// Bound a single dial attempt so a stalled handshake fails fast and the
/// `DIAL_BACKOFF_*` cadence actually governs retry timing, instead of QUIC's
/// own (much longer) handshake timeout silently stretching every failed
/// attempt. A cold member's `RoomRoster` only reaches its peers once this
/// link comes up, so a slow dial here directly widens the `room_absent`
/// window clients hit right after that member restarts.
const DIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a member re-advertises its local room subscriptions to peers.
const SUBSCRIPTION_REFRESH: Duration = Duration::from_secs(15);
/// Max accepted cluster frame size (defensive bound).
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// A signed message on a cluster link.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterMsg {
    pub kind: ClusterMsgKind,
    /// base64url Ed25519 identity of the sending member.
    pub sender: String,
    /// base64url Ed25519 signature over [`ClusterMsg::signing_bytes`].
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClusterMsgKind {
    /// First frame on a link: announce the cluster id we believe we're in.
    Hello { cluster_id: String },
    /// The full set of `room_id`s the sender currently has local subscribers
    /// for. Sent periodically and on change; replaces the sender's prior set.
    Subscriptions { rooms: Vec<String> },
    /// A room broadcast to replicate. `raw` is the already-signed, opaque
    /// client message (e.g. an `SfuChat` envelope) verbatim.
    Replicate {
        room_id: String,
        message_id: String,
        raw: String,
    },
    /// The full set of durable (client-owned) rooms the sender currently hosts.
    /// Sent on link-up and periodically so every member can materialize the same
    /// rooms and accept a failed-over join. Existence + creator only —
    /// per-peer private-room ACLs are *not* cluster-replicated; cold members
    /// admit via Space proof (or local invite-token rematerialize / creator
    /// self-admit).
    RoomRoster { rooms: Vec<RoomDescriptor> },
    /// A client-authorization grant: trust `identity_pub` (add to the peer store,
    /// relay allow-list, and access grants) so any member accepts the client's
    /// signaling/relay after it fails over. Idempotent.
    PeerAuth {
        identity_pub: String,
        handle: String,
        /// True when the origin node admitted this peer via a **direct supernode
        /// invite handshake** (non-empty transcript). Room-invite guests leave
        /// this false so open-mode TOS still applies after failover.
        #[serde(default)]
        direct_invite: bool,
    },
    /// Full set of client-authorization grants this member currently holds.
    /// Sent on link-up and periodically so cold / restarted members converge
    /// on the same trusted-client set without re-inviting each peer. Receivers
    /// apply each entry via the same path as a single [`ClusterMsgKind::PeerAuth`].
    PeerAuthRoster { peers: Vec<PeerAuthDescriptor> },
    /// Sent once after `Hello` to advertise this node's software version.
    VersionInfo {
        version: String,
        build_id: String,
        source_hash: String,
    },
    /// A verified signed Space root to reconcile (authenticated room-set sync).
    /// Sent on change; receivers keep the highest epoch per `(space_id, signer)`
    /// after re-verifying the owner signature.
    SpaceRoot { root: SignedSpaceRoot },
}

/// One client-authorization entry as advertised in a [`ClusterMsgKind::PeerAuthRoster`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerAuthDescriptor {
    pub identity_pub: String,
    #[serde(default)]
    pub handle: String,
    /// Origin admitted via direct supernode invite (handshake transcript).
    #[serde(default)]
    pub direct_invite: bool,
}

/// One durable room as advertised in a [`ClusterMsgKind::RoomRoster`]. Carries
/// exactly what a receiver needs to materialize the room via `create_room`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomDescriptor {
    pub room_id: String,
    pub room_name: String,
    /// "public" | "private".
    pub room_type: String,
    /// Room owner's identity_pub (may be empty for legacy anonymous rooms, which
    /// are not advertised). Preserved so the owner can rejoin on any member.
    pub creator_id: String,
    pub invite_policy: String,
}

impl ClusterMsg {
    /// Deterministic bytes covered by the signature (everything but the sig).
    fn signing_bytes(kind: &ClusterMsgKind, sender: &str) -> Vec<u8> {
        // serde_json with sorted struct field order is deterministic for these
        // shapes; prefix with sender to bind the signature to the claimed id.
        let mut out = Vec::new();
        out.extend_from_slice(b"conquerd-cluster-msg-v1|");
        out.extend_from_slice(sender.as_bytes());
        out.push(b'|');
        out.extend_from_slice(serde_json::to_vec(kind).unwrap_or_default().as_slice());
        out
    }

    /// Build a signed message from this node's identity.
    pub fn signed(kind: ClusterMsgKind, identity: &Identity) -> Self {
        let sender = identity.public_id();
        let sig = identity.sign(&Self::signing_bytes(&kind, &sender));
        Self {
            kind,
            sender,
            signature: crate::crypto::b64url_encode(&sig),
        }
    }

    /// Verify the signature binds `kind` to `sender`.
    pub fn verify(&self) -> bool {
        let (Ok(pk), Ok(sig)) = (
            crate::crypto::b64url_decode(&self.sender),
            crate::crypto::b64url_decode(&self.signature),
        ) else {
            return false;
        };
        Identity::verify_with_pub(&pk, &sig, &Self::signing_bytes(&self.kind, &self.sender))
    }

    fn encode_frame(&self) -> Vec<u8> {
        let json = serde_json::to_vec(self).unwrap_or_default();
        let mut out = Vec::with_capacity(4 + json.len());
        out.extend_from_slice(&(json.len() as u32).to_be_bytes());
        out.extend_from_slice(&json);
        out
    }
}

/// Bounded set of recently-seen ids, for replication dedup (loop/duplicate
/// guard). Insertion returns whether the id was newly seen.
pub struct SeenCache {
    set: HashSet<String>,
    order: std::collections::VecDeque<String>,
    cap: usize,
}

impl SeenCache {
    pub fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: std::collections::VecDeque::new(),
            cap,
        }
    }

    /// Record `id`; returns `true` if it had not been seen before.
    pub fn insert_new(&mut self, id: &str) -> bool {
        if id.is_empty() {
            return true; // no id to dedup on — always deliver
        }
        if self.set.contains(id) {
            return false;
        }
        self.set.insert(id.to_string());
        self.order.push_back(id.to_string());
        if self.order.len() > self.cap {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// A replicated room message handed to the local delivery path (B.3).
#[derive(Debug, Clone)]
pub struct ReplicatedMsg {
    pub room_id: String,
    pub message_id: String,
    pub raw: String,
}

/// A client-authorization grant replicated from another member (B.4b).
#[derive(Debug, Clone)]
pub struct PeerAuthGrant {
    pub identity_pub: String,
    pub handle: String,
    /// Origin admitted via direct supernode invite (handshake transcript).
    pub direct_invite: bool,
}

/// Fetches the `room_id`s this node currently has local subscribers for.
pub type LocalRoomsFn = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
/// Fetches the currently-held signed Space roots (one per `space_id`) so they
/// can be periodically re-gossiped (§8 periodic re-broadcast) for members that
/// missed the on-change gossip or joined the cluster later.
pub type LocalSpaceRootsFn = Arc<dyn Fn() -> Vec<SignedSpaceRoot> + Send + Sync>;
/// Consumes a replicated room message (delivers to local subscribers).
pub type OnReplicateFn = Arc<dyn Fn(ReplicatedMsg) + Send + Sync>;
/// Fetches this node's durable room descriptors to advertise to peers.
pub type LocalRoomRosterFn = Arc<dyn Fn() -> Vec<RoomDescriptor> + Send + Sync>;
/// Materializes a room advertised by a peer's roster (idempotent).
pub type OnRoomRosterFn = Arc<dyn Fn(RoomDescriptor) + Send + Sync>;
/// Applies a replicated client-authorization grant (trust the peer).
pub type OnPeerAuthFn = Arc<dyn Fn(PeerAuthGrant) + Send + Sync>;
/// Fetches this node's trusted-client descriptors to advertise to peers.
pub type LocalPeerAuthRosterFn = Arc<dyn Fn() -> Vec<PeerAuthDescriptor> + Send + Sync>;
/// Accepts a gossiped signed Space root (store highest epoch per space).
pub type OnSpaceRootFn = Arc<dyn Fn(SignedSpaceRoot) + Send + Sync>;

/// Per-member outbound channel + version info.
struct PeerLink {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    version: Option<String>,
    source_hash: Option<String>,
}

/// Shared mutable cluster-link state.
struct LinkState {
    /// member_id → outbound sender (present only while a link is up).
    peers: HashMap<String, PeerLink>,
    /// member_id → set of room_ids that member has local subscribers for.
    remote_subs: HashMap<String, HashSet<String>>,
}

impl LinkState {
    fn new() -> Self {
        Self {
            peers: HashMap::new(),
            remote_subs: HashMap::new(),
        }
    }

    /// Members (other than us) that have local subscribers for `room_id`.
    fn members_for_room(&self, room_id: &str) -> Vec<String> {
        self.remote_subs
            .iter()
            .filter(|(_, rooms)| rooms.contains(room_id))
            .map(|(m, _)| m.clone())
            .collect()
    }
}

/// The cluster transport for this node.
pub struct ClusterLink {
    identity: Identity,
    membership: ClusterMembership,
    state: Arc<RwLock<LinkState>>,
    on_replicate: OnReplicateFn,
    on_room_roster: OnRoomRosterFn,
    on_peer_auth: OnPeerAuthFn,
    on_space_root: OnSpaceRootFn,
    /// Set in [`ClusterLink::start`]; lets a freshly-established link advertise
    /// this node's current subscriptions immediately instead of waiting for the
    /// periodic refresh.
    local_rooms: RwLock<Option<LocalRoomsFn>>,
    /// Set in [`ClusterLink::start`]; source of durable room descriptors for the
    /// periodic roster gossip and the initial advertise on a new link.
    local_room_roster: RwLock<Option<LocalRoomRosterFn>>,
    /// Set in [`ClusterLink::start`]; source of trusted-client grants for the
    /// periodic PeerAuth roster gossip and the initial advertise on a new link.
    local_peer_auth_roster: RwLock<Option<LocalPeerAuthRosterFn>>,
    /// Set in [`ClusterLink::start`]; source of truth for the periodic Space-root
    /// re-gossip (and the initial advertise on a freshly-established link).
    local_space_roots: RwLock<Option<LocalSpaceRootsFn>>,
    /// Fired by [`ClusterLink::shutdown`] to stop all background loops.
    shutdown: Arc<Notify>,
    /// The accept-side endpoint, retained so shutdown can close it.
    server_endpoint: RwLock<Option<Endpoint>>,
}

impl ClusterLink {
    pub fn new(
        identity: Identity,
        membership: ClusterMembership,
        on_replicate: OnReplicateFn,
        on_room_roster: OnRoomRosterFn,
        on_peer_auth: OnPeerAuthFn,
        on_space_root: OnSpaceRootFn,
    ) -> Arc<Self> {
        Arc::new(Self {
            identity,
            membership,
            state: Arc::new(RwLock::new(LinkState::new())),
            on_replicate,
            on_room_roster,
            on_peer_auth,
            on_space_root,
            local_rooms: RwLock::new(None),
            local_room_roster: RwLock::new(None),
            local_peer_auth_roster: RwLock::new(None),
            local_space_roots: RwLock::new(None),
            shutdown: Arc::new(Notify::new()),
            server_endpoint: RwLock::new(None),
        })
    }

    /// Stop all background loops and close the cluster endpoint. Idempotent.
    #[cfg_attr(not(test), expect(dead_code, reason = "exercised by unit tests only"))]
    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
        if let Some(ep) = self.server_endpoint.read().clone() {
            ep.close(0u32.into(), b"shutdown");
        }
    }

    /// Replicate a room message to every peer member that has local subscribers
    /// for `room_id`. Loop-safe: replicated frames are delivered locally by the
    /// receiver and never re-replicated.
    pub fn replicate(&self, room_id: &str, message_id: &str, raw: &str) {
        let targets = self.state.read().members_for_room(room_id);
        if targets.is_empty() {
            return;
        }
        let msg = ClusterMsg::signed(
            ClusterMsgKind::Replicate {
                room_id: room_id.to_string(),
                message_id: message_id.to_string(),
                raw: raw.to_string(),
            },
            &self.identity,
        );
        let frame = msg.encode_frame();
        let st = self.state.read();
        for member in targets {
            if let Some(link) = st.peers.get(&member) {
                let _ = link.tx.send(frame.clone());
            }
        }
    }

    /// Broadcast a client-authorization grant to all peer members so any member
    /// accepts this client's signaling/relay after it fails over.
    pub fn replicate_peer_auth(&self, identity_pub: &str, handle: &str, direct_invite: bool) {
        let msg = ClusterMsg::signed(
            ClusterMsgKind::PeerAuth {
                identity_pub: identity_pub.to_string(),
                handle: handle.to_string(),
                direct_invite,
            },
            &self.identity,
        );
        self.send_to_all_peers(&msg.encode_frame());
    }

    /// Broadcast a verified signed Space root to all peer members (authenticated
    /// room-set sync). Every member re-verifies and keeps the highest epoch.
    pub fn replicate_space_root(&self, root: &SignedSpaceRoot) {
        let msg = ClusterMsg::signed(
            ClusterMsgKind::SpaceRoot { root: root.clone() },
            &self.identity,
        );
        self.send_to_all_peers(&msg.encode_frame());
    }

    /// Send a pre-encoded frame to every connected peer member.
    fn send_to_all_peers(&self, frame: &[u8]) {
        let st = self.state.read();
        for link in st.peers.values() {
            let _ = link.tx.send(frame.to_vec());
        }
    }

    /// IDs of peer members with a live link, used by the portal cluster stats.
    pub fn connected_peer_ids(&self) -> Vec<String> {
        self.state.read().peers.keys().cloned().collect()
    }

    /// Version info for each connected peer, keyed by peer id.
    pub fn peer_versions(&self) -> HashMap<String, (Option<String>, Option<String>)> {
        self.state
            .read()
            .peers
            .iter()
            .map(|(id, p)| (id.clone(), (p.version.clone(), p.source_hash.clone())))
            .collect()
    }

    /// Start the cluster endpoint: bind the local `cluster_addr`, accept inbound
    /// links, dial peer members, and periodically advertise local subscriptions
    /// and currently-held Space roots.
    pub async fn start(
        self: &Arc<Self>,
        local_rooms: LocalRoomsFn,
        local_room_roster: LocalRoomRosterFn,
        local_space_roots: LocalSpaceRootsFn,
        local_peer_auth_roster: LocalPeerAuthRosterFn,
    ) -> anyhow::Result<u16> {
        *self.local_space_roots.write() = Some(Arc::clone(&local_space_roots));
        let bind: SocketAddr = self
            .my_cluster_addr()
            .ok_or_else(|| anyhow::anyhow!("this node has no cluster_addr configured"))?;

        *self.local_rooms.write() = Some(Arc::clone(&local_rooms));
        *self.local_room_roster.write() = Some(Arc::clone(&local_room_roster));
        *self.local_peer_auth_roster.write() = Some(Arc::clone(&local_peer_auth_roster));

        let (server_config, _cert) = build_quinn_server_config(&self.identity.public_id())?;
        let endpoint = Endpoint::server(server_config, bind)?;
        let port = endpoint.local_addr()?.port();
        info!("Cluster link listening on {}", endpoint.local_addr()?);
        *self.server_endpoint.write() = Some(endpoint.clone());

        // Accept loop.
        {
            let this = Arc::clone(self);
            let endpoint = endpoint.clone();
            let shutdown = Arc::clone(&self.shutdown);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        incoming = endpoint.accept() => {
                            let Some(incoming) = incoming else { break };
                            let this = Arc::clone(&this);
                            tokio::spawn(async move {
                                if let Err(e) = this.handle_incoming(incoming).await {
                                    debug!("cluster inbound link error: {e}");
                                }
                            });
                        }
                        _ = shutdown.notified() => break,
                    }
                }
            });
        }

        // Dial each peer member that advertises a cluster_addr. To avoid two
        // links between the same pair (both sides dialing), only the member with
        // the lexicographically smaller id dials; the larger one accepts.
        let self_id = self.identity.public_id().trim_end_matches('=').to_string();
        for member in self.membership.peers() {
            let Some(addr) = member.cluster_addr.clone() else {
                continue;
            };
            let member_id = member.identity_pub.trim_end_matches('=').to_string();
            if self_id >= member_id {
                continue; // the peer dials us
            }
            let this = Arc::clone(self);
            tokio::spawn(async move {
                this.dial_loop(member_id, addr).await;
            });
        }

        // Periodic subscription + room / peer-auth roster + Space-root
        // re-advertisement. PeerAuth is otherwise only fire-and-forget on new
        // trust, so a member that missed that gossip (or joined later) would
        // never learn historical clients without this bulk roster.
        {
            let this = Arc::clone(self);
            let shutdown = Arc::clone(&self.shutdown);
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(SUBSCRIPTION_REFRESH);
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                            this.broadcast_subscriptions(&local_rooms());
                            this.broadcast_room_roster(&local_room_roster());
                            this.broadcast_peer_auth_roster(&local_peer_auth_roster());
                            for root in local_space_roots() {
                                this.replicate_space_root(&root);
                            }
                        }
                        _ = shutdown.notified() => break,
                    }
                }
            });
        }

        Ok(port)
    }

    fn my_cluster_addr(&self) -> Option<SocketAddr> {
        self.membership
            .self_member()
            .and_then(|m| m.cluster_addr.as_ref())
            .and_then(|a| a.parse().ok())
    }

    /// Send this node's current local room subscriptions to all connected peers.
    fn broadcast_subscriptions(&self, rooms: &[String]) {
        let msg = ClusterMsg::signed(
            ClusterMsgKind::Subscriptions {
                rooms: rooms.to_vec(),
            },
            &self.identity,
        );
        self.send_to_all_peers(&msg.encode_frame());
    }

    /// Advertise this node's durable rooms to all peers so they can materialize
    /// them and accept a failed-over join. No-op when this node hosts none.
    fn broadcast_room_roster(&self, rooms: &[RoomDescriptor]) {
        if rooms.is_empty() {
            return;
        }
        let msg = ClusterMsg::signed(
            ClusterMsgKind::RoomRoster {
                rooms: rooms.to_vec(),
            },
            &self.identity,
        );
        self.send_to_all_peers(&msg.encode_frame());
    }

    /// Advertise this node's trusted-client set so cold members accept
    /// multi-homed / failed-over clients without a re-invite. No-op when empty.
    fn broadcast_peer_auth_roster(&self, peers: &[PeerAuthDescriptor]) {
        if peers.is_empty() {
            return;
        }
        let msg = ClusterMsg::signed(
            ClusterMsgKind::PeerAuthRoster {
                peers: peers.to_vec(),
            },
            &self.identity,
        );
        self.send_to_all_peers(&msg.encode_frame());
    }

    /// Dial a peer with exponential backoff, holding the link open until it drops.
    async fn dial_loop(self: Arc<Self>, member_id: String, addr: String) {
        let mut backoff = DIAL_BACKOFF_START;
        loop {
            tokio::select! {
                r = self.dial_once(&member_id, &addr) => match r {
                    Ok(()) => backoff = DIAL_BACKOFF_START, // closed cleanly; retry
                    Err(e) => debug!("cluster dial to {} ({}) failed: {e}", short(&member_id), addr),
                },
                _ = self.shutdown.notified() => break,
            }
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = self.shutdown.notified() => break,
            }
            backoff = (backoff * 2).min(DIAL_BACKOFF_MAX);
        }
    }

    async fn dial_once(&self, member_id: &str, addr: &str) -> anyhow::Result<()> {
        let remote: SocketAddr = addr
            .parse()
            .map_err(|e| anyhow::anyhow!("bad cluster_addr {addr}: {e}"))?;
        let client_cfg = build_quinn_client_config(self.identity.public_key_bytes())?;
        let mut endpoint = Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(client_cfg);

        let connecting = endpoint.connect(remote, "conquerd")?;
        let conn = timeout(DIAL_CONNECT_TIMEOUT, connecting)
            .await
            .map_err(|_| {
                anyhow::anyhow!("connect to {addr} timed out after {DIAL_CONNECT_TIMEOUT:?}")
            })??;
        // The server cert is self-signed with a non-hex CN, so we don't bind the
        // peer identity here. Authenticity is enforced per-message instead:
        // `run_link`/`read_loop` require every frame to be Ed25519-signed with
        // `sender == member_id` (the configured target), which an imposter at
        // this address cannot forge.
        let (send, recv) = conn.open_bi().await?;
        info!("Cluster link up (dialed) to {}", short(member_id));
        self.run_link(member_id.to_string(), send, recv).await
    }

    async fn handle_incoming(self: &Arc<Self>, incoming: quinn::Incoming) -> anyhow::Result<()> {
        let conn = incoming.await?;
        let peer_id = extract_peer_id(&conn)?;
        let peer_id = peer_id.trim_end_matches('=').to_string();
        if !self.membership.is_peer_member(&peer_id) {
            warn!("rejecting cluster link from non-member {}", short(&peer_id));
            conn.close(0u32.into(), b"not_a_member");
            return Ok(());
        }
        let (send, recv) = conn.accept_bi().await?;
        info!("Cluster link up (accepted) from {}", short(&peer_id));
        self.run_link(peer_id, send, recv).await
    }

    /// Drive a single established link: register the outbound channel, send an
    /// initial Hello + subscriptions, then pump frames until either side ends.
    async fn run_link(
        &self,
        member_id: String,
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
    ) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        self.state.write().peers.insert(
            member_id.clone(),
            PeerLink {
                tx,
                version: None,
                source_hash: None,
            },
        );

        // Greet with our cluster id so a misconfigured peer is detected early.
        let hello = ClusterMsg::signed(
            ClusterMsgKind::Hello {
                cluster_id: self.membership.cluster_id().to_string(),
            },
            &self.identity,
        );
        let _ = send.write_all(&hello.encode_frame()).await;

        // Advertise our software version so peers can display it in the portal.
        let version_info = ClusterMsg::signed(
            ClusterMsgKind::VersionInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                build_id: env!("CONQUERD_BUILD_ID").to_string(),
                source_hash: env!("CONQUERD_SOURCE_HASH").to_string(),
            },
            &self.identity,
        );
        let _ = send.write_all(&version_info.encode_frame()).await;

        // Advertise our current subscriptions immediately so the peer can route
        // replication to us without waiting for the periodic refresh. Clone the
        // fn out of the guard first so no lock is held across the await.
        let local_rooms = self.local_rooms.read().clone();
        if let Some(local_rooms) = local_rooms {
            let subs = ClusterMsg::signed(
                ClusterMsgKind::Subscriptions {
                    rooms: local_rooms(),
                },
                &self.identity,
            );
            let _ = send.write_all(&subs.encode_frame()).await;
        }

        // Advertise our durable rooms immediately so a member that just came up
        // (or reconnected) materializes them without waiting for the periodic
        // refresh — existence self-heal for cold members (no per-peer ACL push).
        let local_room_roster = self.local_room_roster.read().clone();
        if let Some(local_room_roster) = local_room_roster {
            let rooms = local_room_roster();
            if !rooms.is_empty() {
                let roster =
                    ClusterMsg::signed(ClusterMsgKind::RoomRoster { rooms }, &self.identity);
                let _ = send.write_all(&roster.encode_frame()).await;
            }
        }

        // Advertise our currently-held Space roots immediately too, so a member
        // that just joined the cluster converges without waiting up to
        // `SUBSCRIPTION_REFRESH` for the periodic re-gossip.
        let local_space_roots = self.local_space_roots.read().clone();
        if let Some(local_space_roots) = local_space_roots {
            for root in local_space_roots() {
                let msg = ClusterMsg::signed(ClusterMsgKind::SpaceRoot { root }, &self.identity);
                if send.write_all(&msg.encode_frame()).await.is_err() {
                    break;
                }
            }
        }

        // Advertise trusted clients immediately so a cold member accepts
        // multi-homed clients without waiting for the next periodic tick (or a
        // re-invite). Mirrors RoomRoster self-heal for the peer trust set.
        let local_peer_auth_roster = self.local_peer_auth_roster.read().clone();
        if let Some(local_peer_auth_roster) = local_peer_auth_roster {
            let peers = local_peer_auth_roster();
            if !peers.is_empty() {
                let roster =
                    ClusterMsg::signed(ClusterMsgKind::PeerAuthRoster { peers }, &self.identity);
                let _ = send.write_all(&roster.encode_frame()).await;
            }
        }

        // Writer task: drain the outbound channel onto the stream.
        let writer = tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if send.write_all(&frame).await.is_err() {
                    break;
                }
            }
        });

        // Reader loop: length-prefixed JSON frames. Cancel on shutdown.
        let read_result = tokio::select! {
            r = self.read_loop(&member_id, &mut recv) => r,
            _ = self.shutdown.notified() => Ok(()),
        };

        // Teardown.
        writer.abort();
        let mut st = self.state.write();
        st.peers.remove(&member_id);
        st.remote_subs.remove(&member_id);
        debug!("Cluster link to {} closed", short(&member_id));
        read_result
    }

    async fn read_loop(&self, member_id: &str, recv: &mut quinn::RecvStream) -> anyhow::Result<()> {
        loop {
            let mut len_buf = [0u8; 4];
            match recv.read_exact(&mut len_buf).await {
                Ok(()) => {}
                Err(_) => return Ok(()), // peer closed
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len == 0 || len > MAX_FRAME_BYTES {
                anyhow::bail!("cluster frame length {len} out of bounds");
            }
            let mut buf = vec![0u8; len];
            recv.read_exact(&mut buf).await?;
            let Ok(msg) = serde_json::from_slice::<ClusterMsg>(&buf) else {
                debug!("malformed cluster frame from {}", short(member_id));
                continue;
            };
            // Authenticate: signature valid, sender is the link's member, and a
            // declared cluster peer. This is what makes a spoofed cert CN inert.
            if !msg.verify()
                || msg.sender.trim_end_matches('=') != member_id
                || !self.membership.is_peer_member(member_id)
            {
                warn!(
                    "dropping unauthenticated cluster frame from {}",
                    short(member_id)
                );
                continue;
            }
            self.handle_msg(member_id, msg.kind);
        }
    }

    fn handle_msg(&self, member_id: &str, kind: ClusterMsgKind) {
        match kind {
            ClusterMsgKind::Hello { cluster_id } => {
                if cluster_id != self.membership.cluster_id() {
                    warn!(
                        "cluster id mismatch from {}: {cluster_id}",
                        short(member_id)
                    );
                }
            }
            ClusterMsgKind::Subscriptions { rooms } => {
                self.state
                    .write()
                    .remote_subs
                    .insert(member_id.to_string(), rooms.into_iter().collect());
            }
            ClusterMsgKind::Replicate {
                room_id,
                message_id,
                raw,
            } => {
                (self.on_replicate)(ReplicatedMsg {
                    room_id,
                    message_id,
                    raw,
                });
            }
            ClusterMsgKind::RoomRoster { rooms } => {
                for room in rooms {
                    (self.on_room_roster)(room);
                }
            }
            ClusterMsgKind::PeerAuth {
                identity_pub,
                handle,
                direct_invite,
            } => {
                (self.on_peer_auth)(PeerAuthGrant {
                    identity_pub,
                    handle,
                    direct_invite,
                });
            }
            ClusterMsgKind::PeerAuthRoster { peers } => {
                for p in peers {
                    (self.on_peer_auth)(PeerAuthGrant {
                        identity_pub: p.identity_pub,
                        handle: p.handle,
                        direct_invite: p.direct_invite,
                    });
                }
            }
            ClusterMsgKind::VersionInfo {
                version,
                source_hash,
                ..
            } => {
                if let Some(peer) = self.state.write().peers.get_mut(member_id) {
                    peer.version = Some(version);
                    peer.source_hash = Some(source_hash);
                }
            }
            ClusterMsgKind::SpaceRoot { root } => {
                (self.on_space_root)(root);
            }
        }
    }
}

fn short(id: &str) -> &str {
    &id[..12.min(id.len())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg_for(kind: ClusterMsgKind, id: &Identity) -> ClusterMsg {
        ClusterMsg::signed(kind, id)
    }

    #[test]
    fn signed_message_round_trips() {
        let id = Identity::generate();
        let msg = msg_for(
            ClusterMsgKind::Subscriptions {
                rooms: vec!["r1".into(), "r2".into()],
            },
            &id,
        );
        assert_eq!(msg.sender, id.public_id());
        assert!(msg.verify());
    }

    #[test]
    fn tampered_message_fails_verify() {
        let id = Identity::generate();
        let mut msg = msg_for(
            ClusterMsgKind::Replicate {
                room_id: "r".into(),
                message_id: "m1".into(),
                raw: "ciphertext".into(),
            },
            &id,
        );
        // Mutate the payload after signing.
        msg.kind = ClusterMsgKind::Replicate {
            room_id: "r".into(),
            message_id: "m1".into(),
            raw: "TAMPERED".into(),
        };
        assert!(!msg.verify());
    }

    #[test]
    fn message_signed_by_other_key_does_not_match_claimed_sender() {
        let a = Identity::generate();
        let b = Identity::generate();
        let mut msg = msg_for(
            ClusterMsgKind::Hello {
                cluster_id: "c".into(),
            },
            &a,
        );
        // Claim to be b while signed by a.
        msg.sender = b.public_id();
        assert!(!msg.verify());
    }

    #[test]
    fn frame_encode_is_length_prefixed() {
        let id = Identity::generate();
        let msg = msg_for(
            ClusterMsgKind::Hello {
                cluster_id: "c".into(),
            },
            &id,
        );
        let frame = msg.encode_frame();
        let len = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(len, frame.len() - 4);
        let decoded: ClusterMsg = serde_json::from_slice(&frame[4..]).unwrap();
        assert!(decoded.verify());
    }

    /// Reserve two *distinct* ephemeral ports. Binding one socket at a time and
    /// dropping it before the next bind can hand back the same port twice (the OS
    /// reassigns the just-freed port), so the second link fails to bind ("address
    /// already in use", os error 10048). Binding both sockets at once guarantees
    /// the kernel picks two different ports.
    fn reserve_two_addrs() -> (String, String) {
        let s1 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let s2 = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let a = format!("127.0.0.1:{}", s1.local_addr().unwrap().port());
        let b = format!("127.0.0.1:{}", s2.local_addr().unwrap().port());
        (a, b)
    }

    /// Build the two-member `ClusterConfig`/`ClusterMembership` pair for a
    /// given address pair. Shared by `start_two_node_link` below.
    fn membership_pair(
        a_pub: &str,
        b_pub: &str,
        a_addr: &str,
        b_addr: &str,
    ) -> (ClusterMembership, ClusterMembership) {
        let mk = |id: &str, addr: &str| crate::cluster::ClusterMember {
            identity_pub: id.to_string(),
            relay_addr: addr.to_string(),
            cluster_addr: Some(addr.to_string()),
            ws_addr: None,
        };
        let cfg = crate::cluster::ClusterConfig {
            cluster_id: "test".into(),
            members: vec![mk(a_pub, a_addr), mk(b_pub, b_addr)],
        };
        (
            ClusterMembership::new(cfg.clone(), a_pub),
            ClusterMembership::new(cfg, b_pub),
        )
    }

    /// Construct and start a two-node cluster link, retrying with a fresh
    /// address pair a bounded number of times.
    ///
    /// `reserve_two_addrs()` reserves free ports by binding-then-dropping two
    /// sockets, but the real bind happens inside `ClusterLink::start()` only
    /// after building the QUIC server config (cert generation), which takes
    /// long enough that another concurrently-running test in this file can
    /// grab the just-freed port first (Windows: "Only one usage of each
    /// socket address...", os error 10048). Retrying with a fresh pair
    /// absorbs that race instead of failing the whole test run.
    /// Empty durable-room roster source, for tests that don't exercise roster
    /// replication.
    fn no_roster() -> LocalRoomRosterFn {
        Arc::new(Vec::new)
    }

    /// Empty peer-auth roster source, for tests that don't exercise trust sync.
    fn no_peer_auth_roster() -> LocalPeerAuthRosterFn {
        Arc::new(Vec::new)
    }

    async fn start_two_node_link(
        a_pub: &str,
        b_pub: &str,
        new_link_a: impl Fn(ClusterMembership) -> Arc<ClusterLink>,
        new_link_b: impl Fn(ClusterMembership) -> Arc<ClusterLink>,
        rooms_a: LocalRoomsFn,
        roster_a: LocalRoomRosterFn,
        roots_a: LocalSpaceRootsFn,
        rooms_b: LocalRoomsFn,
        roster_b: LocalRoomRosterFn,
        roots_b: LocalSpaceRootsFn,
    ) -> (Arc<ClusterLink>, Arc<ClusterLink>) {
        start_two_node_link_with_auth(
            a_pub,
            b_pub,
            new_link_a,
            new_link_b,
            rooms_a,
            roster_a,
            roots_a,
            no_peer_auth_roster(),
            rooms_b,
            roster_b,
            roots_b,
            no_peer_auth_roster(),
        )
        .await
    }

    async fn start_two_node_link_with_auth(
        a_pub: &str,
        b_pub: &str,
        new_link_a: impl Fn(ClusterMembership) -> Arc<ClusterLink>,
        new_link_b: impl Fn(ClusterMembership) -> Arc<ClusterLink>,
        rooms_a: LocalRoomsFn,
        roster_a: LocalRoomRosterFn,
        roots_a: LocalSpaceRootsFn,
        peer_auth_a: LocalPeerAuthRosterFn,
        rooms_b: LocalRoomsFn,
        roster_b: LocalRoomRosterFn,
        roots_b: LocalSpaceRootsFn,
        peer_auth_b: LocalPeerAuthRosterFn,
    ) -> (Arc<ClusterLink>, Arc<ClusterLink>) {
        const ATTEMPTS: u32 = 5;
        for attempt in 1..=ATTEMPTS {
            let (a_addr, b_addr) = reserve_two_addrs();
            let (mem_a, mem_b) = membership_pair(a_pub, b_pub, &a_addr, &b_addr);
            let link_a = new_link_a(mem_a);
            let link_b = new_link_b(mem_b);
            let res_a = link_a
                .start(
                    rooms_a.clone(),
                    roster_a.clone(),
                    roots_a.clone(),
                    peer_auth_a.clone(),
                )
                .await;
            let res_b = if res_a.is_ok() {
                link_b
                    .start(
                        rooms_b.clone(),
                        roster_b.clone(),
                        roots_b.clone(),
                        peer_auth_b.clone(),
                    )
                    .await
            } else {
                Err(anyhow::anyhow!("link A start failed"))
            };
            if res_a.is_ok() && res_b.is_ok() {
                return (link_a, link_b);
            }
            link_a.shutdown();
            link_b.shutdown();
            if attempt == ATTEMPTS {
                panic!(
                    "failed to start cluster link pair after {ATTEMPTS} attempts \
                     (port bind race): a={:?} b={:?}",
                    res_a.err(),
                    res_b.err()
                );
            }
        }
        unreachable!()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_node_cluster_replicates_room_chat_over_quic() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let (a_pub, b_pub) = (id_a.public_id(), id_b.public_id());

        // B records what it receives; A receives nothing in this test.
        let received = Arc::new(parking_lot::Mutex::new(Vec::<ReplicatedMsg>::new()));
        let on_b: OnReplicateFn = {
            let r = received.clone();
            Arc::new(move |m| r.lock().push(m))
        };
        let on_a: OnReplicateFn = Arc::new(|_| {});
        let no_roster_rx: OnRoomRosterFn = Arc::new(|_| {});
        let no_auth: OnPeerAuthFn = Arc::new(|_| {});
        let no_root: OnSpaceRootFn = Arc::new(|_| {});

        // Only B has a local subscriber for "room1".
        let a_rooms: LocalRoomsFn = Arc::new(Vec::new);
        let b_rooms: LocalRoomsFn = Arc::new(|| vec!["room1".to_string()]);

        let (link_a, link_b) = start_two_node_link(
            &a_pub,
            &b_pub,
            |mem_a| {
                ClusterLink::new(
                    id_a.clone(),
                    mem_a,
                    on_a.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            |mem_b| {
                ClusterLink::new(
                    id_b.clone(),
                    mem_b,
                    on_b.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            a_rooms,
            no_roster(),
            Arc::new(Vec::new),
            b_rooms,
            no_roster(),
            Arc::new(Vec::new),
        )
        .await;

        // Once the link is up and B's subscription has reached A, A's replicate
        // routes the frame to B. Retry to absorb connect/propagation latency.
        let mut delivered = false;
        for _ in 0..100 {
            link_a.replicate("room1", "m1", "HELLO-B");
            if !received.lock().is_empty() {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(delivered, "B never received the replicated room chat");
        {
            // Scope the guard — parking_lot mutexes are not reentrant, so it must
            // be dropped before we lock again below.
            let got = received.lock();
            assert_eq!(got[0].room_id, "room1");
            assert_eq!(got[0].message_id, "m1");
            assert_eq!(got[0].raw, "HELLO-B");
        }

        // A frame for a room nobody subscribes to routes nowhere.
        link_a.replicate("ghost-room", "m2", "NOPE");
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(received.lock().iter().all(|m| m.room_id != "ghost-room"));

        // Tear down background tasks/endpoints so the test runtime exits cleanly.
        link_a.shutdown();
        link_b.shutdown();
    }

    /// A freshly-established cluster link advertises the sender's durable rooms
    /// immediately, so a member that never had a room materialized (e.g. the
    /// client only pre-seeded other members) still learns it and can accept a
    /// failed-over join — no explicit trigger, just link establishment. Room
    /// *existence* self-heals this way; per-peer ACL is not cluster-replicated.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_node_cluster_advertises_room_roster_on_link_establish() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let (a_pub, b_pub) = (id_a.public_id(), id_b.public_id());

        // A hosts one durable room, advertised via `local_room_roster`.
        let a_roster: LocalRoomRosterFn = Arc::new(|| {
            vec![RoomDescriptor {
                room_id: "93735a1f6ac6446c".into(),
                room_name: "We Gamin?".into(),
                room_type: "public".into(),
                creator_id: "OWNER".into(),
                invite_policy: "owner".into(),
            }]
        });

        // B records the rooms it is asked to materialize.
        let materialized = Arc::new(parking_lot::Mutex::new(Vec::<RoomDescriptor>::new()));
        let on_roster_b: OnRoomRosterFn = {
            let m = materialized.clone();
            Arc::new(move |desc| m.lock().push(desc))
        };
        let no_repl: OnReplicateFn = Arc::new(|_| {});
        let no_roster_rx: OnRoomRosterFn = Arc::new(|_| {});
        let no_auth: OnPeerAuthFn = Arc::new(|_| {});
        let no_root: OnSpaceRootFn = Arc::new(|_| {});

        let rooms: LocalRoomsFn = Arc::new(Vec::new);
        let (link_a, link_b) = start_two_node_link(
            &a_pub,
            &b_pub,
            |mem_a| {
                ClusterLink::new(
                    id_a.clone(),
                    mem_a,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            |mem_b| {
                ClusterLink::new(
                    id_b.clone(),
                    mem_b,
                    no_repl.clone(),
                    on_roster_b.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            rooms.clone(),
            a_roster,
            Arc::new(Vec::new),
            rooms,
            no_roster(),
            Arc::new(Vec::new),
        )
        .await;

        // No explicit send — B should materialize A's room purely from the
        // on-establish (and periodic) roster advertisement.
        let mut delivered = false;
        for _ in 0..100 {
            if !materialized.lock().is_empty() {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(delivered, "B never received A's durable room roster");
        {
            let got = materialized.lock();
            assert_eq!(got[0].room_id, "93735a1f6ac6446c");
            assert_eq!(got[0].room_name, "We Gamin?");
            assert_eq!(got[0].room_type, "public");
            assert_eq!(got[0].creator_id, "OWNER");
        }
        link_a.shutdown();
        link_b.shutdown();
    }

    /// A freshly-established cluster link advertises the owner's currently-held
    /// Space roots immediately (not just on-change via `replicate_space_root`),
    /// so a member that joins the cluster later converges without waiting for a
    /// client resend.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_node_cluster_advertises_space_root_on_link_establish() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let (a_pub, b_pub) = (id_a.public_id(), id_b.public_id());

        // A owns one signed Space root, advertised via `local_space_roots`.
        let owner = Identity::generate();
        let space = crate::space::Space::new_server(&owner.public_id(), "srv");
        let root = space.signed_root(1000, |b| owner.sign(b).to_vec());
        let a_roots: LocalSpaceRootsFn = {
            let root = root.clone();
            Arc::new(move || vec![root.clone()])
        };

        // B records roots it receives.
        let received = Arc::new(parking_lot::Mutex::new(Vec::<SignedSpaceRoot>::new()));
        let on_root_b: OnSpaceRootFn = {
            let r = received.clone();
            Arc::new(move |root| r.lock().push(root))
        };
        let no_repl: OnReplicateFn = Arc::new(|_| {});
        let no_roster_rx: OnRoomRosterFn = Arc::new(|_| {});
        let no_auth: OnPeerAuthFn = Arc::new(|_| {});
        let no_root: OnSpaceRootFn = Arc::new(|_| {});

        let rooms: LocalRoomsFn = Arc::new(Vec::new);
        let (link_a, link_b) = start_two_node_link(
            &a_pub,
            &b_pub,
            |mem_a| {
                ClusterLink::new(
                    id_a.clone(),
                    mem_a,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            |mem_b| {
                ClusterLink::new(
                    id_b.clone(),
                    mem_b,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    on_root_b.clone(),
                )
            },
            rooms.clone(),
            no_roster(),
            a_roots,
            rooms,
            no_roster(),
            Arc::new(Vec::new),
        )
        .await;

        let mut delivered = false;
        for _ in 0..100 {
            if !received.lock().is_empty() {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            delivered,
            "B never received A's Space root on link establish"
        );
        assert_eq!(received.lock()[0].space_id, root.space_id);
        assert_eq!(received.lock()[0].epoch, root.epoch);

        link_a.shutdown();
        link_b.shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_node_cluster_replicates_peer_auth_over_quic() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let (a_pub, b_pub) = (id_a.public_id(), id_b.public_id());

        let auths = Arc::new(parking_lot::Mutex::new(Vec::<PeerAuthGrant>::new()));
        let on_auth_b: OnPeerAuthFn = {
            let a = auths.clone();
            Arc::new(move |grant| a.lock().push(grant))
        };
        let no_repl: OnReplicateFn = Arc::new(|_| {});
        let no_roster_rx: OnRoomRosterFn = Arc::new(|_| {});
        let no_auth: OnPeerAuthFn = Arc::new(|_| {});
        let no_root: OnSpaceRootFn = Arc::new(|_| {});

        let rooms: LocalRoomsFn = Arc::new(Vec::new);
        let (link_a, link_b) = start_two_node_link(
            &a_pub,
            &b_pub,
            |mem_a| {
                ClusterLink::new(
                    id_a.clone(),
                    mem_a,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            |mem_b| {
                ClusterLink::new(
                    id_b.clone(),
                    mem_b,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    on_auth_b.clone(),
                    no_root.clone(),
                )
            },
            rooms.clone(),
            no_roster(),
            Arc::new(Vec::new),
            rooms,
            no_roster(),
            Arc::new(Vec::new),
        )
        .await;

        let mut delivered = false;
        for _ in 0..100 {
            link_a.replicate_peer_auth("client-XYZ", "Alice", true);
            if !auths.lock().is_empty() {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(delivered, "B never received the replicated peer auth");
        {
            let got = auths.lock();
            assert_eq!(got[0].identity_pub, "client-XYZ");
            assert_eq!(got[0].handle, "Alice");
            assert!(got[0].direct_invite);
        }
        link_a.shutdown();
        link_b.shutdown();
    }

    /// A freshly-established cluster link advertises the sender's trusted-client
    /// roster immediately, so a cold member that missed fire-and-forget
    /// `PeerAuth` still converges without re-inviting every client.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_node_cluster_advertises_peer_auth_roster_on_link_establish() {
        let _ = rustls::crypto::ring::default_provider().install_default();

        let id_a = Identity::generate();
        let id_b = Identity::generate();
        let (a_pub, b_pub) = (id_a.public_id(), id_b.public_id());

        let a_auth: LocalPeerAuthRosterFn = Arc::new(|| {
            vec![PeerAuthDescriptor {
                identity_pub: "client-HISTORIC".into(),
                handle: "Bob".into(),
                direct_invite: true,
            }]
        });

        let auths = Arc::new(parking_lot::Mutex::new(Vec::<PeerAuthGrant>::new()));
        let on_auth_b: OnPeerAuthFn = {
            let a = auths.clone();
            Arc::new(move |grant| a.lock().push(grant))
        };
        let no_repl: OnReplicateFn = Arc::new(|_| {});
        let no_roster_rx: OnRoomRosterFn = Arc::new(|_| {});
        let no_auth: OnPeerAuthFn = Arc::new(|_| {});
        let no_root: OnSpaceRootFn = Arc::new(|_| {});

        let rooms: LocalRoomsFn = Arc::new(Vec::new);
        let (link_a, link_b) = start_two_node_link_with_auth(
            &a_pub,
            &b_pub,
            |mem_a| {
                ClusterLink::new(
                    id_a.clone(),
                    mem_a,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    no_auth.clone(),
                    no_root.clone(),
                )
            },
            |mem_b| {
                ClusterLink::new(
                    id_b.clone(),
                    mem_b,
                    no_repl.clone(),
                    no_roster_rx.clone(),
                    on_auth_b.clone(),
                    no_root.clone(),
                )
            },
            rooms.clone(),
            no_roster(),
            Arc::new(Vec::new),
            a_auth,
            rooms,
            no_roster(),
            Arc::new(Vec::new),
            no_peer_auth_roster(),
        )
        .await;

        let mut delivered = false;
        for _ in 0..100 {
            if !auths.lock().is_empty() {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(
            delivered,
            "B never received A's PeerAuth roster on link establish"
        );
        {
            let got = auths.lock();
            assert_eq!(got[0].identity_pub, "client-HISTORIC");
            assert_eq!(got[0].handle, "Bob");
            assert!(got[0].direct_invite);
        }

        link_a.shutdown();
        link_b.shutdown();
    }

    #[test]
    fn members_for_room_routes_by_remote_subscriptions() {
        let mut st = LinkState::new();
        st.remote_subs
            .insert("B".into(), HashSet::from(["room1".to_string()]));
        st.remote_subs
            .insert("C".into(), HashSet::from(["room2".to_string()]));
        let mut got = st.members_for_room("room1");
        got.sort();
        assert_eq!(got, vec!["B".to_string()]);
        assert!(st.members_for_room("room3").is_empty());
    }
}
