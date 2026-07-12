// ConquerD Supernode — main.rs
// Standalone Rust supernode binary: QUIC relay + SFU + WebSocket signaling + in-app portal (web.host.app.v1).

mod access;
mod cluster;
mod cluster_link;
mod config;
mod crypto;
mod handshake;
mod identity;
mod manifest;
mod peer_store;
mod protocol;
mod relay;
mod sfu;
mod sfu_module;
mod signaling;
mod space;
mod stats;
mod ticket;
mod web_app_module;
mod webtransport;
mod wire;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde_json::json;
use tracing::{debug, info, warn};

use crate::access::create_access_controller;
use crate::config::Config;
use crate::handshake::HandshakeManager;
use crate::identity::Identity;
use crate::peer_store::{PeerRecord, PeerStore};
use crate::protocol::{MessageType, SignalingMessage};
use crate::relay::QUICRelayServer;
use crate::sfu::SFURoomManager;
use crate::signaling::{SignalingHandler, SignalingServer};
use crate::ticket::RelayTicket;
use conquerd_features::wellknown;
use conquerd_features::FeatureRegistry;
use conquerd_features::{NativeModuleLoader, TrustedKeyStore};

use crate::webtransport::BrowserBridge;

/// Application version (match client APP_VERSION).
const APP_VERSION: &str = "1.0.0";
/// Ticket renewal check interval.
const RENEWAL_CHECK_INTERVAL_S: u64 = 60;
/// Endpoint mailbox TTL (24 h).
const ENDPOINT_MAX_AGE_S: f64 = 86400.0;

/// Build the supernode's [`FeatureRegistry`] from the manifest at
/// `<data_dir>/supernode.toml`, falling back to the legacy env-var
/// `Config` toggles when the file is absent. The manifest is the single
/// source for operator-declared hosted features. Built-in first-party
/// descriptors are upserted later so relay/quota accounting can still classify
/// core, room, and game traffic when the manifest omits those entries.
///
/// After registering well-known capabilities, any manifest entries with a
/// `cdylib_manifest` path are loaded via [`NativeModuleLoader`]. Signer
/// keys must be listed in `<data_dir>/trusted_module_keys.txt`; unknown
/// keys cause the entry to be skipped with a warning (no interactive
/// prompt on the supernode — add keys to the file to pre-authorise them).
fn load_manifest(config: &Config) -> manifest::SupernodeManifest {
    match manifest::SupernodeManifest::load_or_derive(&config.data_dir, config) {
        Ok(m) => m,
        Err(e) => {
            warn!(
                "[features] failed to load supernode.toml ({}); falling back to legacy env-var toggles",
                e
            );
            manifest::SupernodeManifest::from_legacy_config(config)
        }
    }
}

fn build_feature_registry(
    manifest: &manifest::SupernodeManifest,
    config: &Config,
) -> FeatureRegistry {
    let registry = FeatureRegistry::new();
    let caps = manifest.enabled_capabilities();
    info!(
        "[features] loaded {} capability(ies) from manifest: {}",
        caps.len(),
        caps.iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for cap in caps {
        if let Err(e) = registry.register(cap) {
            warn!("[features] failed to register capability: {}", e);
        }
    }

    // ── Native module loading (Phase 5) ─────────────────────────────────────
    //
    // Load cdylib entries from the manifest. Uses the trust store at
    // `<data_dir>/trusted_module_keys.txt`; unknown keys are denied with
    // a warning (headless supernode — no interactive prompt).
    let native_entries: Vec<_> = manifest.native_module_entries().cloned().collect();
    if !native_entries.is_empty() {
        let keys_path = config.data_dir.join("trusted_module_keys.txt");
        let trust_store = match TrustedKeyStore::load(&keys_path) {
            Ok(s) => s,
            Err(e) => {
                warn!("[features] failed to load trusted_module_keys.txt ({}); no native modules will be loaded", e);
                TrustedKeyStore::new()
            }
        };
        let loader = NativeModuleLoader::new(trust_store, |req| {
            warn!(
                "[features] native module '{}' by '{}' has untrusted signer key {}; \
                 add the key to trusted_module_keys.txt to allow loading",
                req.module_id, req.author, req.signer_pubkey
            );
            false // deny unknown keys on the headless supernode
        });

        for entry in native_entries {
            let manifest_path = entry.cdylib_manifest.as_ref().unwrap();
            // Derive cdylib path: same directory as the manifest, platform extension.
            let cdylib_path = {
                let stem = manifest_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&entry.id);
                // Strip trailing ".module" from stem if present.
                let stem = stem.strip_suffix(".module").unwrap_or(stem);
                let ext = if cfg!(target_os = "windows") {
                    "dll"
                } else if cfg!(target_os = "macos") {
                    "dylib"
                } else {
                    "so"
                };
                manifest_path.with_file_name(format!("{stem}.{ext}"))
            };

            match loader.load(manifest_path, &cdylib_path) {
                Ok(module) => {
                    let id = module.descriptor().id.clone();
                    match registry.register_module(module) {
                        Ok(()) => info!("[features] loaded native module '{}'", id),
                        Err(e) => warn!(
                            "[features] failed to register native module '{}': {}",
                            id, e
                        ),
                    }
                }
                Err(e) => warn!(
                    "[features] failed to load native module from '{}': {}",
                    manifest_path.display(),
                    e
                ),
            }
        }
    }

    // QUIC relay fan-out classifies inner channel tags and enforces quotas
    // against these descriptors even when the manifest omitted them.
    for cap in [
        wellknown::core_audio_opus(),
        wellknown::core_chat_v1(),
        wellknown::core_file_v1(),
        wellknown::game_relay_v1(),
        wellknown::room_audio_sfu(),
        wellknown::room_chat_v1(),
        wellknown::room_file_v1(),
    ] {
        let _ = registry.upsert(cap);
    }

    registry
}

/// Core supernode state shared across tasks.
struct SupernodeState {
    config: Config,
    identity: Identity,
    peer_store: RwLock<PeerStore>,
    handshake: RwLock<HandshakeManager>,
    relay: Option<QUICRelayServer>,
    sfu: Option<RwLock<SFURoomManager>>,
    signaling: SignalingServer,
    access_controller: Box<dyn access::AccessController>,
    start_time: Instant,
    /// peer_id → ticket expiry timestamp
    ticket_expiry: RwLock<HashMap<String, f64>>,
    /// peer_id → raw ENDPOINT_UPDATE message
    endpoint_mailbox: RwLock<HashMap<String, String>>,
    /// Pending hole-punch registrations: (peer_a, peer_b) canonical key → PunchRegistration
    pending_punches: RwLock<HashMap<(String, String), PunchRegistration>>,
    /// Capabilities advertised in `SUPERNODE_INFO`.
    features: Arc<FeatureRegistry>,
    /// Browser-transport bridge. Always allocated, but only handed to a
    /// listener task when `web.host.h3.v1` is enabled in the manifest.
    web_bridge: BrowserBridge,
    /// SHA-256 fingerprint (hex) of the self-signed WebTransport TLS cert.
    ///
    /// Advertised in `SUPERNODE_INFO` so native clients can expose it to
    /// game pages via `/_conquerd/ctx.json`.  Games pass it to
    /// `new WebTransport(url, { serverCertificateHashes: [...] })` which
    /// pins the connection to this specific cert without needing a CA.
    /// This IS the ConquerD trust model: the supernode's identity was
    /// already verified via Ed25519 invite/handshake; the cert fingerprint
    /// is just an additional binding over that trusted channel.
    web_cert_fingerprint: Option<String>,
    /// Which SFU room types peers may materialize (`room.audio.sfu` params).
    sfu_room_policy: manifest::SfuRoomCreationPolicy,
    /// This node's cluster membership, when an `[cluster]` section is configured.
    /// `None` ⇒ standalone supernode.
    cluster: Option<cluster::ClusterMembership>,
    /// Live intra-cluster transport (set after startup when clustering is on).
    cluster_link: RwLock<Option<Arc<cluster_link::ClusterLink>>>,
    /// Dedup of replicated room messages (by `message_id`) to guard against
    /// duplicate delivery across cluster links.
    replication_seen: RwLock<cluster_link::SeenCache>,
    /// Highest verified Space root per `space_id` (authenticated room-set sync).
    /// Populated from client `SpaceRootAnnounce`, cluster `SpaceRoot` gossip, and
    /// client-carried roots on join. Used by proof-based admission.
    space_roots: RwLock<SpaceRootStore>,
}

/// Highest verified [`space::SignedSpaceRoot`] per `space_id`. `space_id` embeds
/// the owner (`derive_node_id("", owner_pub, …)`), so it is effectively bound to
/// one signer; we still pin the signer and refuse epoch regression (monotonic,
/// equivocation containment — SPACE-MERKLE-DESIGN §8).
#[derive(Default)]
struct SpaceRootStore {
    roots: HashMap<String, space::SignedSpaceRoot>,
    /// `space_id` → count of detected equivocations (two differently-hashed,
    /// validly-signed roots seen for the same `(space_id, epoch)`). We chose a
    /// set tree, not an append-only log, so there is no consistency proof
    /// between epochs — a malicious owner *can* sign two roots for one epoch.
    /// This is the lighter of the two SPACE-MERKLE-DESIGN §9 mitigations
    /// (flag conflicts instead of building a CT-style history tree): we cannot
    /// tell which root is "true", so we keep the first-seen one (unchanged
    /// behavior) but make the conflict observable for operators instead of
    /// silently dropping it.
    equivocations: HashMap<String, u32>,
}

impl SpaceRootStore {
    /// Accept `root` iff it verifies and is strictly newer than what we hold for
    /// its space (or first-seen), bound to the same signer. Returns whether it
    /// was newly stored (idempotent for equal/older epochs).
    fn accept(&mut self, root: space::SignedSpaceRoot) -> bool {
        if !root.verify() {
            return false;
        }
        if let Some(existing) = self.roots.get(&root.space_id) {
            if existing.signer != root.signer {
                return false;
            }
            if root.epoch == existing.epoch && root.root_hash != existing.root_hash {
                // Same signer, same epoch, different content: root-equivocation.
                // Keep the first-seen root (unchanged acceptance policy) but
                // record the conflict so it surfaces in `/api/stats`.
                let count = self.equivocations.entry(root.space_id.clone()).or_insert(0);
                *count += 1;
                warn!(
                    "space root equivocation detected: space_id={} epoch={} signer={} (conflicting root rejected, {} total)",
                    &root.space_id[..root.space_id.len().min(12)],
                    root.epoch,
                    &root.signer[..root.signer.len().min(12)],
                    count
                );
                return false;
            }
            if root.epoch <= existing.epoch {
                return false;
            }
        }
        self.roots.insert(root.space_id.clone(), root);
        true
    }

    fn get(&self, space_id: &str) -> Option<space::SignedSpaceRoot> {
        self.roots.get(space_id).cloned()
    }

    /// All currently-held roots (one per `space_id`), for periodic cluster
    /// re-gossip (SPACE-MERKLE-DESIGN §8) so members that missed the on-change
    /// gossip, or joined the cluster later, converge without a client resend.
    fn all(&self) -> Vec<space::SignedSpaceRoot> {
        self.roots.values().cloned().collect()
    }

    /// Total detected root-equivocations across all spaces, for `/api/stats`.
    fn equivocation_count(&self) -> u32 {
        self.equivocations.values().sum()
    }
}

/// Pure proof-based admission decision (no side effects) — does the presented
/// `proof` (+ `grant` for private nodes) admit `sender` to `room_id` against the
/// current signed `root`? `now` is unix seconds (grant expiry). Extracted so the
/// security matrix is unit-testable without a full `SupernodeState`.
///
/// - proof must be for exactly `room_id` and verify against `root` (which pins
///   the epoch → current-epoch-only admission);
/// - **public** node: the proof alone admits;
/// - **private** node: additionally an owner-signed grant bound to this peer,
///   not expired, whose epoch is already active (`≤ root.epoch`).
fn space_admission_ok(
    root: &space::SignedSpaceRoot,
    proof: &space::SpaceInclusionProof,
    grant: Option<&space::SpaceGrant>,
    sender: &str,
    room_id: &str,
    now: u64,
) -> bool {
    if proof.node.node_id != room_id || !proof.verify_against(root) {
        return false;
    }
    if proof.node.node_type != "private" {
        return true; // public node — proof-only admission
    }
    let Some(grant) = grant else {
        return false;
    };
    if !grant.verify(&root.signer)
        || grant.node_id != room_id
        || grant.grantee_pub.trim_end_matches('=') != sender.trim_end_matches('=')
        || grant.epoch > root.epoch
    {
        return false;
    }
    grant.expires_at == 0 || now <= grant.expires_at
}

/// A pending hole-punch registration waiting for both peers.
struct PunchRegistration {
    registered_at: f64,
    /// peer_id → endpoint string
    endpoints: HashMap<String, String>,
}

impl SupernodeState {
    /// Replicate a locally-received room chat to cluster peers that have local
    /// subscribers for the room. No-op when standalone. Loop-safe: peers deliver
    /// the frame locally and never re-replicate it.
    pub(crate) fn replicate_room_chat(&self, room_id: &str, msg: &SignalingMessage, raw: &str) {
        let Some(link) = self.cluster_link.read().clone() else {
            return;
        };
        let message_id = msg
            .payload
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        link.replicate(room_id, message_id, raw);
    }

    /// Replicate a locally-received room audio frame to cluster peers hosting
    /// members of the same room. Same `Replicate` transport as chat; message id
    /// is the frame signature (or sender+seq fallback) for dedup. Loop-safe:
    /// receivers deliver locally and never re-replicate.
    pub(crate) fn replicate_room_audio(&self, room_id: &str, msg: &SignalingMessage, raw: &str) {
        let Some(link) = self.cluster_link.read().clone() else {
            return;
        };
        link.replicate(room_id, &audio_replication_id(msg), raw);
    }

    /// Deliver a room chat replicated from another cluster member to this node's
    /// local recipients. Deduped by `message_id`; never re-replicated.
    fn deliver_replicated_chat(&self, room_id: &str, message_id: &str, raw: &str) {
        if !self.replication_seen.write().insert_new(message_id) {
            return; // already delivered
        }
        let Some(ref sfu) = self.sfu else {
            return;
        };
        let recipients = sfu.read().get_chat_recipients(room_id);
        for peer in &recipients {
            if self
                .features
                .gate_through_feature("room.chat.v1", peer, raw.len())
            {
                self.signaling.send_to_peer(peer, raw);
            }
        }
    }

    /// Deliver a room audio frame replicated from another cluster member to
    /// this node's local voice participants. Prefer QUIC relay datagrams, fall
    /// back to WebSocket — same transport preference as the local SFU bridge.
    /// Deduped by `message_id`; never re-replicated. Skips the active-speaker
    /// gate (the origin node already applied it; the remote talker is not a
    /// local participant and must not displace local talker scores).
    fn deliver_replicated_audio(&self, room_id: &str, message_id: &str, raw: &str) {
        if !self.replication_seen.write().insert_new(message_id) {
            return;
        }
        let Some(ref sfu) = self.sfu else {
            return;
        };
        // Exclude the original talker if they somehow multi-homed onto this node.
        let sender = SignalingMessage::from_json(raw)
            .map(|m| m.sender)
            .unwrap_or_default();
        let recipients = sfu
            .read()
            .get_room(room_id)
            .map(|r| r.participant_ids())
            .unwrap_or_default();
        if recipients.is_empty() {
            return;
        }
        // Relay path: [sender_index][ROOM_AUDIO_TAG][signed JSON]. Receiver
        // ignores sender_index and verifies the signed JSON (parity with
        // native bridge fan-out).
        use conquerd_features::channel_frame::ROOM_AUDIO_TAG;
        let mut tagged = Vec::with_capacity(1 + raw.len());
        tagged.push(ROOM_AUDIO_TAG);
        tagged.extend_from_slice(raw.as_bytes());
        let fwd = crate::wire::build_forwarded_datagram(crate::wire::BROADCAST_INDEX, &tagged);
        let wire_bytes = raw.len();
        for peer in &recipients {
            if !sender.is_empty() && peer == &sender {
                continue;
            }
            if let Some(ref relay) = self.relay {
                // Some(_) = delivered or quota-dropped on relay; None = fall to WS.
                if relay.send_room_datagram(peer, &fwd).is_some() {
                    continue;
                }
            }
            if self
                .features
                .gate_through_feature("room.audio.sfu", peer, wire_bytes)
            {
                self.signaling.send_to_peer(peer, raw);
            }
        }
    }

    /// Route an inbound cluster `Replicate` frame to chat or audio delivery
    /// based on the wire `type` of the opaque client envelope.
    fn deliver_replicated_room_frame(&self, room_id: &str, message_id: &str, raw: &str) {
        let is_audio = serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| {
                v.get("type")
                    .and_then(|t| t.as_str())
                    .map(|s| s == "sfu_audio")
            })
            .unwrap_or(false);
        if is_audio {
            self.deliver_replicated_audio(room_id, message_id, raw);
        } else {
            self.deliver_replicated_chat(room_id, message_id, raw);
        }
    }

    /// Local-only room admit: materialize the room if absent and authorize
    /// `peer` on *this* node. Does **not** cluster-replicate membership —
    /// cold members admit via Space proof (or creator / rematerialized token).
    fn local_allow_room_peer(&self, room_id: &str, room_name: &str, room_type: &str, peer: &str) {
        let Some(ref sfu) = self.sfu else {
            return;
        };
        let rtype = match room_type {
            "public" => sfu::RoomType::Public,
            _ => sfu::RoomType::Private,
        };
        let mut s = sfu.write();
        // creator "" → no implicit creator privileges; access is via the
        // explicit allow below.
        s.create_room(Some(room_id), room_name, rtype, "");
        s.allow_peer(room_id, peer);
    }

    /// Materialize a durable room advertised in a peer's `RoomRoster` so this
    /// member can accept a failed-over join for it. Idempotent — `create_room`
    /// leaves an existing room untouched. Preserves the advertised `creator_id`
    /// so the room owner retains self-admit/invite authority on any member.
    /// Non-owner private members re-admit via Space proof on join (or local
    /// invite-token rematerialize) — not via cluster ACL push.
    fn apply_room_roster(&self, desc: &cluster_link::RoomDescriptor) {
        let Some(ref sfu) = self.sfu else {
            return;
        };
        let rtype = match desc.room_type.as_str() {
            "public" => sfu::RoomType::Public,
            _ => sfu::RoomType::Private,
        };
        sfu.write().create_room_with_policy(
            Some(&desc.room_id),
            &desc.room_name,
            rtype,
            &desc.creator_id,
            &desc.invite_policy,
        );
    }

    /// Verify + store a signed Space root (highest epoch per space), and — if it
    /// was newly accepted — cluster-gossip it to peer members. Returns whether it
    /// was newly stored. Used by the owner announce path and by client-carried
    /// roots on join.
    fn accept_and_gossip_space_root(&self, root: space::SignedSpaceRoot) -> bool {
        let gossip = root.clone();
        let accepted = self.space_roots.write().accept(root);
        if accepted {
            if let Some(link) = self.cluster_link.read().clone() {
                link.replicate_space_root(&gossip);
            }
        }
        accepted
    }

    /// Proof-based admission (SPACE-MERKLE-DESIGN §5). If `payload` carries space
    /// fields that verify against the current signed root for the space,
    /// authorize `sender`, materialize the room from the proven node, and return
    /// `true`. Returns `false` to fall through to the local invite-token path —
    /// absence or verification failure just means "not admitted by proof", never
    /// an outright denial. Cluster-wide membership is proof-carried; there is no
    /// supernode-to-supernode room ACL push.
    fn try_space_admission(
        &self,
        sender: &str,
        room_id: &str,
        payload: &serde_json::Value,
    ) -> bool {
        // Inclusion proof for exactly this room.
        let Some(proof) = payload
            .get("space_proof")
            .cloned()
            .and_then(|v| serde_json::from_value::<space::SpaceInclusionProof>(v).ok())
        else {
            return false;
        };
        if proof.node.node_id != room_id {
            return false;
        }
        // A client always carries its current signed root (MTC "fallback
        // certificate", §5) so admission never blocks on gossip propagation.
        // Accept it (verify + highest-epoch), then verify the proof against the
        // CURRENT held root — enforcing current-epoch-only admission (revocation
        // = exclusion, §8): a stale proof against a superseded root is rejected.
        let Some(carried) = payload
            .get("space_root")
            .cloned()
            .and_then(|v| serde_json::from_value::<space::SignedSpaceRoot>(v).ok())
        else {
            return false;
        };
        let space_id = carried.space_id.clone();
        self.accept_and_gossip_space_root(carried);
        let Some(root) = self.space_roots.read().get(&space_id) else {
            return false;
        };
        if !proof.verify_against(&root) {
            return false;
        }

        // The proof shows the room provably exists in the signed Space, so
        // **materialize** it from the proven node if absent (§5.1: a proof is an
        // equally authoritative description) — even when entry is still gated by
        // a local invite token below. This is the roster-free existence
        // guarantee: any cluster member the joiner reaches can serve the room.
        let rtype = if proof.node.node_type == "private" {
            "private"
        } else {
            "public"
        };
        if let Some(ref sfu) = self.sfu {
            let rt = if rtype == "private" {
                sfu::RoomType::Private
            } else {
                sfu::RoomType::Public
            };
            // Carry the proven SpaceNode's `invite_policy` onto the
            // materialized room (§7 "proven SpaceNode" resolution). It is first
            // created with an empty `creator_id`; the adopt step below binds it
            // to the proven Space owner.
            let mut s = sfu.write();
            s.create_room_with_policy(
                Some(room_id),
                &proof.node.name,
                rt,
                "",
                &proof.node.invite_policy,
            );
            // Bind the room to its cryptographically-proven Space owner. The
            // inclusion proof (verified against the current signed root above)
            // authenticates `proof.node.owner_pub` as the room's owner, so
            // adopting it as `creator_id` restores owner minting + self-admit
            // for a room re-materialized after a restart/idle-GC — the durable
            // replacement for the deferred Layer 2 node-key capability path.
            if s.adopt_creator_if_empty(room_id, &proof.node.owner_pub) {
                info!(
                    "[space] room {} adopted proven owner {} as creator",
                    &room_id[..12.min(room_id.len())],
                    &proof.node.owner_pub[..12.min(proof.node.owner_pub.len())]
                );
            }
        }

        // Admission decision: public → proof-only; private → owner-signed grant
        // bound to this peer. Only on a full pass do we allow locally.
        let grant = payload
            .get("space_grant")
            .cloned()
            .and_then(|v| serde_json::from_value::<space::SpaceGrant>(v).ok());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if !space_admission_ok(&root, &proof, grant.as_ref(), sender, room_id, now) {
            // Materialized but not admitted by proof (e.g. private room via a
            // shareable link carrying no grant) → fall through to the local
            // token path, which can now validate against the just-materialized room.
            return false;
        }
        self.local_allow_room_peer(room_id, &proof.node.name, rtype, sender);
        true
    }

    /// Broadcast a client-authorization grant to cluster peers so any member
    /// accepts this client after a failover. No-op when standalone.
    fn replicate_peer_auth(&self, identity_pub: &str) {
        let Some(link) = self.cluster_link.read().clone() else {
            return;
        };
        let handle = self
            .peer_store
            .read()
            .get_peer(identity_pub)
            .map(|p| p.handle.clone())
            .unwrap_or_default();
        link.replicate_peer_auth(identity_pub, &handle);
    }

    /// Apply a client-authorization grant replicated from another member: trust
    /// the peer (peer store + relay allow-list + access grant) so this node
    /// accepts the client if it fails over here. Idempotent.
    fn apply_peer_auth(&self, identity_pub: &str, handle: &str) {
        {
            let mut store = self.peer_store.write();
            if !store.is_trusted(identity_pub) {
                let peer_id = crate::crypto::b64url_decode(identity_pub)
                    .map(|b| crate::crypto::derive_peer_id(&b))
                    .unwrap_or_default();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                store.add_peer(peer_store::PeerRecord {
                    peer_id,
                    identity_pub: identity_pub.to_string(),
                    relay_hints: vec![],
                    handle: handle.to_string(),
                    blocked: false,
                    revoked: false,
                    auto_connect: false,
                    is_supernode: false,
                    transcript_hash: String::new(),
                    created_at: now,
                    last_seen_at: now,
                    quic_port: 0,
                });
                let _ = store.save();
            }
        }
        if let Some(ref relay) = self.relay {
            relay.allow_peer(identity_pub);
        }
        self.access_controller.on_peer_granted(identity_pub);
    }

    /// Send a signed message to a peer via signaling.
    fn send_signed(&self, target: &str, msg_type: MessageType, payload: serde_json::Value) {
        let msg = SignalingMessage::new(msg_type, &self.identity.public_id(), payload)
            .with_target(target)
            .sign(&self.identity);
        self.signaling.send_to_peer(target, &msg.to_json());
    }

    /// Build the JSON payload for `SUPERNODE_INFO`. Always includes the
    /// advertised capability list. When `web.host.app.v1` is enabled we
    /// also advertise the canonical `conquerd://` URL pointing at this
    /// node's identity; native clients use it to open the supernode's
    /// in-app portal in their embedded Chromium view.
    fn supernode_info_payload(&self) -> serde_json::Value {
        let caps: Vec<serde_json::Value> = self
            .features
            .snapshot()
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            .collect();
        let mut payload = json!({
            "node_title": self.config.web_title,
            "capabilities": caps,
        });
        if self.features.get("web.host.app.v1").is_some() {
            let obj = payload.as_object_mut().unwrap();
            obj.insert(
                "app_url".into(),
                json!(format!("conquerd://{}/", self.identity.public_id())),
            );
        }
        // Advertise WebTransport base URL and cert fingerprint when
        // web.host.h3.v1 is enabled.  Both are consumed by the native client
        // and injected into game pages via /_conquerd/ctx.json so that
        // `new WebTransport(url, { serverCertificateHashes: [{ algorithm:
        // 'sha-256', value: fingerprint }] })` can connect without any CA
        // cert.  The fingerprint IS the trust anchor — it was received over
        // the already Ed25519-verified SUPERNODE_INFO channel.
        if self.features.get("web.host.h3.v1").is_some() {
            let port = self.config.web_port.unwrap_or(8443);
            let host = match self.config.external_host.as_deref() {
                Some(h) if !h.is_empty() && h != "0.0.0.0" => h.to_owned(),
                _ => "localhost".to_owned(),
            };
            let obj = payload.as_object_mut().unwrap();
            obj.insert("wt_url".into(), json!(format!("https://{}:{}", host, port)));
            if let Some(ref fp) = self.web_cert_fingerprint {
                obj.insert("cert_fingerprint".into(), json!(fp));
            }
        }
        // Advertise the signed cluster roster so a client can fail over to any
        // member. Signed by this node's identity, which the client already
        // trusts over the Ed25519-verified SUPERNODE_INFO channel.
        if let Some(ref cluster) = self.cluster {
            if let Ok(desc) = serde_json::to_value(cluster.sign(&self.identity)) {
                payload
                    .as_object_mut()
                    .unwrap()
                    .insert("cluster".into(), desc);
            }
        }
        payload
    }

    /// Issue a relay ticket to a peer.
    fn issue_relay_ticket(&self, peer_pub: &str) {
        let Some(ref relay) = self.relay else { return };

        let external = self.config.external_host.as_deref().unwrap_or("0.0.0.0");
        if external == "0.0.0.0" {
            warn!(
                "Relay ticket for {} uses 0.0.0.0 — set supernode_host env var",
                &peer_pub[..12.min(peer_pub.len())]
            );
        }
        let ticket = RelayTicket::create(
            peer_pub,
            external,
            self.config.relay_port,
            &self.identity.signing_key,
        );

        relay.allow_peer_update(peer_pub);

        self.ticket_expiry
            .write()
            .insert(peer_pub.to_string(), ticket.expires_at);

        self.send_signed(
            peer_pub,
            MessageType::RelayGranted,
            json!({
                "ticket": ticket.to_value(),
                "relay_host": ticket.relay_host,
                "relay_port": ticket.relay_port,
            }),
        );

        // Send SUPERNODE_INFO (always includes capabilities; web fields
        // are added when the portal is running).
        self.send_signed(
            peer_pub,
            MessageType::SupernodeInfo,
            self.supernode_info_payload(),
        );

        info!(
            "Issued relay ticket to {}",
            &peer_pub[..12.min(peer_pub.len())]
        );
    }

    /// Handle a newly trusted peer.
    /// Send a `CAPABILITY_ANNOUNCE` to a freshly-trusted peer. Mirrors the
    /// capability snapshot we already include in `SUPERNODE_INFO` so peers
    /// have a single canonical channel to learn what this supernode speaks.
    fn announce_capabilities_to(&self, identity_pub: &str) {
        let caps: Vec<serde_json::Value> = self
            .features
            .snapshot()
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(serde_json::Value::Null))
            .collect();
        self.send_signed(
            identity_pub,
            MessageType::CapabilityAnnounce,
            json!({ "capabilities": caps }),
        );
    }

    fn on_peer_trusted(&self, identity_pub: &str) {
        // Always advertise capabilities so peers can negotiate features
        // independently of whether relay access has been granted.
        self.announce_capabilities_to(identity_pub);

        // Replicate this client's trust to cluster peers so it can fail over to
        // any member. Idempotent on the receiving side; no-op when standalone.
        self.replicate_peer_auth(identity_pub);

        if self.access_controller.check_access(identity_pub) {
            self.issue_relay_ticket(identity_pub);
        } else {
            // Trusted but not yet access-granted. The legacy HTTPS portal
            // redirect (`RelayPaymentRequired`) has been removed; out-of-band
            // grant flow (operator CLI / `access_controller`) is the path now.
            info!(
                "Peer {} is trusted but lacks relay access; awaiting operator grant",
                &identity_pub[..12.min(identity_pub.len())]
            );
        }

        // Replay stored endpoint updates
        self.replay_endpoint_updates(identity_pub);
    }

    /// Revoke a peer's relay access.
    #[allow(dead_code)]
    fn on_peer_revoked(&self, identity_pub: &str) {
        info!(
            "Trust revoked for {}",
            &identity_pub[..12.min(identity_pub.len())]
        );
        if let Some(ref relay) = self.relay {
            relay.revoke_peer(identity_pub);
        }
        self.ticket_expiry.write().remove(identity_pub);
        if let Some(ref sfu) = self.sfu {
            sfu.write().remove_peer_from_all(identity_pub);
        }
        self.send_signed(
            identity_pub,
            MessageType::RelayRevoke,
            json!({"reason": "trust_revoked"}),
        );
    }

    /// Broadcast updated SFU room list to all connected trusted peers.
    fn broadcast_room_list(&self) {
        let Some(ref sfu) = self.sfu else { return };
        for peer_id in self.signaling.connected_peer_ids() {
            if self.peer_store.read().is_trusted(&peer_id) {
                let rooms = sfu.read().get_rooms_for_peer(&peer_id);
                self.send_signed(&peer_id, MessageType::SfuRoomList, json!({"rooms": rooms}));
            }
        }
    }

    /// Resolve the best known endpoint for a peer (for hole punching).
    /// Priority: 1. NAT-mapped addr from relay QUIC conn, 2. endpoint mailbox.
    fn resolve_punch_endpoint(&self, peer_id: &str) -> Option<String> {
        // 1. Observed relay connection address
        if let Some(ref relay) = self.relay {
            if let Some(addr) = relay.get_peer_remote_addr(peer_id) {
                return Some(format!("{}:{}", addr.ip(), addr.port()));
            }
        }
        // 2. Endpoint mailbox — parse the raw JSON to extract listener
        if let Some(raw) = self.endpoint_mailbox.read().get(peer_id) {
            if let Ok(msg) = serde_json::from_str::<serde_json::Value>(raw) {
                if let Some(listener) = msg
                    .get("payload")
                    .and_then(|p| p.get("listener"))
                    .and_then(|v| v.as_str())
                {
                    // listener is typically ws://ip:port — extract host:port
                    if let Some(stripped) = listener.strip_prefix("ws://") {
                        return Some(stripped.to_string());
                    }
                    return Some(listener.to_string());
                }
            }
        }
        None
    }

    /// Attempt relay-coordinated hole punch for a newly-joined room member
    /// with every other relay-connected room member whose endpoint is known.
    fn try_relay_punch_for_room(&self, new_peer: &str, room_id: &str) {
        let Some(ref relay) = self.relay else { return };
        let room_peers = relay.get_room_peers(room_id);
        let new_ep = match self.resolve_punch_endpoint(new_peer) {
            Some(ep) => ep,
            None => {
                debug!(
                    "[relay-punch] No endpoint for new peer {} — skipping",
                    &new_peer[..12.min(new_peer.len())]
                );
                return;
            }
        };

        for other_peer in &room_peers {
            if other_peer == new_peer {
                continue;
            }
            let other_ep = match self.resolve_punch_endpoint(other_peer) {
                Some(ep) => ep,
                None => {
                    debug!(
                        "[relay-punch] No endpoint for peer {} — skipping pair",
                        &other_peer[..12.min(other_peer.len())]
                    );
                    continue;
                }
            };
            self.send_punch_ready(new_peer, other_peer, &new_ep, &other_ep);
        }
    }

    /// Send PUNCH_READY to both peers with coordinated timing.
    fn send_punch_ready(&self, peer_a: &str, peer_b: &str, ep_a: &str, ep_b: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let punch_at = now + 0.5; // 500ms from now

        self.send_signed(
            peer_a,
            MessageType::PunchReady,
            json!({
                "peer_id": peer_b,
                "peer_endpoint": ep_b,
                "your_endpoint": ep_a,
                "punch_at": punch_at,
            }),
        );
        self.send_signed(
            peer_b,
            MessageType::PunchReady,
            json!({
                "peer_id": peer_a,
                "peer_endpoint": ep_a,
                "your_endpoint": ep_b,
                "punch_at": punch_at,
            }),
        );
        info!(
            "[punch] PUNCH_READY sent to {} ↔ {} (punch_at={:.3})",
            &peer_a[..12.min(peer_a.len())],
            &peer_b[..12.min(peer_b.len())],
            punch_at,
        );
    }

    /// Handle a PUNCH_REGISTER message: store the registration, and if
    /// both peers have registered, send PUNCH_READY to both.
    fn handle_punch_register_msg(&self, sender: &str, target_peer: &str, sender_endpoint: &str) {
        // Verify both peers are trusted
        if !self.peer_store.read().is_trusted(sender) {
            warn!(
                "[punch] PUNCH_REGISTER from untrusted peer {}",
                &sender[..12.min(sender.len())]
            );
            return;
        }
        if !self.peer_store.read().is_trusted(target_peer) {
            warn!(
                "[punch] PUNCH_REGISTER for untrusted target {}",
                &target_peer[..12.min(target_peer.len())]
            );
            return;
        }

        // Canonical pair key (sorted order)
        let pair_key = if sender < target_peer {
            (sender.to_string(), target_peer.to_string())
        } else {
            (target_peer.to_string(), sender.to_string())
        };

        let now_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        let mut punches = self.pending_punches.write();

        let entry = punches
            .entry(pair_key.clone())
            .or_insert_with(|| PunchRegistration {
                registered_at: now_ts,
                endpoints: HashMap::new(),
            });

        entry
            .endpoints
            .insert(sender.to_string(), sender_endpoint.to_string());

        info!(
            "[punch] Registration from {} → {} (endpoint={})",
            &sender[..12.min(sender.len())],
            &target_peer[..12.min(target_peer.len())],
            sender_endpoint,
        );

        // Check if both peers have registered
        if entry.endpoints.contains_key(sender) && entry.endpoints.contains_key(target_peer) {
            let ep_a = entry
                .endpoints
                .get(&pair_key.0)
                .cloned()
                .unwrap_or_default();
            let ep_b = entry
                .endpoints
                .get(&pair_key.1)
                .cloned()
                .unwrap_or_default();
            // Remove the entry before sending (release lock)
            punches.remove(&pair_key);
            drop(punches);

            self.send_punch_ready(&pair_key.0, &pair_key.1, &ep_a, &ep_b);
        } else {
            // Clean up stale entries (>30s old)
            let stale_keys: Vec<(String, String)> = punches
                .iter()
                .filter(|(_, info)| now_ts - info.registered_at > 30.0)
                .map(|(k, _)| k.clone())
                .collect();
            for key in stale_keys {
                debug!("[punch] Cleaning up stale punch registration");
                punches.remove(&key);
            }
        }
    }

    /// Store endpoint update in mailbox, persist to disk.
    fn on_endpoint_update(&self, sender: &str, raw: &str) {
        self.endpoint_mailbox
            .write()
            .insert(sender.to_string(), raw.to_string());
        self.save_endpoint_mailbox();
    }

    /// Replay stored endpoint updates to a reconnecting peer.
    fn replay_endpoint_updates(&self, target: &str) {
        let mailbox = self.endpoint_mailbox.read();
        let peer_store = self.peer_store.read();
        for (sender_id, raw) in mailbox.iter() {
            if sender_id != target && peer_store.is_trusted(sender_id) {
                self.signaling.send_to_peer(target, raw);
            }
        }
    }

    /// Persist endpoint mailbox to disk.
    fn save_endpoint_mailbox(&self) {
        let path = self.config.data_dir.join("supernode_endpoints.json");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let mailbox = self.endpoint_mailbox.read();
        let entries: Vec<serde_json::Value> = mailbox
            .iter()
            .map(|(peer_id, raw)| {
                json!({
                    "peer_id": peer_id,
                    "raw": raw,
                    "stored_at": now,
                })
            })
            .collect();
        drop(mailbox);
        if let Ok(data) = serde_json::to_string_pretty(&entries) {
            if let Err(e) = std::fs::write(&path, data) {
                warn!("Failed to save endpoint mailbox: {}", e);
            }
        }
    }

    /// Load endpoint mailbox from disk (filtering entries older than 24h).
    /// Accepts both current format (JSON array of {peer_id, raw, stored_at})
    /// and legacy format (JSON object {peer_id: {timestamp, ...raw_msg}}).
    fn load_endpoint_mailbox(data_dir: &std::path::Path) -> HashMap<String, String> {
        let path = data_dir.join("supernode_endpoints.json");
        let mut result = HashMap::new();
        let data = match std::fs::read_to_string(&path) {
            Ok(d) => d,
            Err(_) => return result,
        };
        let parsed: serde_json::Value = match serde_json::from_str(&data) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse endpoint mailbox: {}", e);
                return result;
            }
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();

        if let Some(entries) = parsed.as_array() {
            // Rust format: [{peer_id, raw, stored_at}, ...]
            for entry in entries {
                let peer_id = entry.get("peer_id").and_then(|v| v.as_str()).unwrap_or("");
                let raw = entry.get("raw").and_then(|v| v.as_str()).unwrap_or("");
                let stored_at = entry
                    .get("stored_at")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if peer_id.is_empty() || raw.is_empty() {
                    continue;
                }
                if now - stored_at > ENDPOINT_MAX_AGE_S {
                    continue;
                }
                result.insert(peer_id.to_string(), raw.to_string());
            }
        } else if let Some(obj) = parsed.as_object() {
            // Legacy format: {peer_id: {timestamp: ..., ...raw_msg}}
            for (peer_id, value) in obj {
                let stored_at = value
                    .get("timestamp")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                if now - stored_at > ENDPOINT_MAX_AGE_S {
                    continue;
                }
                // Store the entire raw message JSON string for replay
                if let Ok(raw) = serde_json::to_string(value) {
                    result.insert(peer_id.clone(), raw);
                }
            }
        } else {
            warn!("Unexpected endpoint mailbox format");
        }

        info!("Loaded {} endpoint mailbox entries", result.len());
        result
    }

    /// Cluster information for the portal. Returns `null` when standalone.
    pub(crate) fn cluster_stats(&self) -> serde_json::Value {
        let Some(membership) = &self.cluster else {
            return serde_json::Value::Null;
        };
        let link = self.cluster_link.read();
        let connected_ids: std::collections::HashSet<String> = link
            .as_ref()
            .map(|l| l.connected_peer_ids().into_iter().collect())
            .unwrap_or_default();
        let peer_versions = link.as_ref().map(|l| l.peer_versions()).unwrap_or_default();
        let self_id = membership
            .self_member()
            .map(|m| m.identity_pub.trim_end_matches('=').to_string())
            .unwrap_or_default();
        let members: Vec<serde_json::Value> = membership
            .self_member()
            .into_iter()
            .chain(membership.peers())
            .map(|m| {
                let norm_id = m.identity_pub.trim_end_matches('=');
                let is_self = norm_id == self_id;
                let (version, source_hash) = if is_self {
                    (
                        Some(APP_VERSION.to_string()),
                        Some(env!("CONQUERD_SOURCE_HASH").to_string()),
                    )
                } else {
                    peer_versions.get(norm_id).cloned().unwrap_or((None, None))
                };
                serde_json::json!({
                    "identity_pub": m.identity_pub,
                    "is_self": is_self,
                    "connected": is_self || connected_ids.contains(norm_id),
                    "version": version,
                    "source_hash": source_hash,
                })
            })
            .collect();
        serde_json::json!({
            "cluster_id": membership.cluster_id(),
            "member_count": membership.member_count(),
            "connected_peers": connected_ids.len(),
            "members": members,
        })
    }

    /// Collect stats for /health and /api/stats.
    pub(crate) fn collect_stats(&self) -> serde_json::Value {
        let mut features = vec![];
        if self.config.chat_enabled {
            features.push("chat");
        }
        if self.config.files_enabled {
            features.push("files");
        }
        if self.config.sfu_enabled {
            features.push("sfu");
        }
        if self.relay.is_some() {
            features.push("relay");
        }

        let mut value = stats::collect_stats(
            APP_VERSION,
            self.start_time,
            self.access_controller.mode_name(),
            &features,
            self.peer_store.read().trusted_count(),
            self.signaling.state().read().connected_count,
            self.relay.as_ref(),
            self.sfu.as_ref(),
        );

        // Merge portal config so the browser-side HTML can read it.
        if let Some(obj) = value.as_object_mut() {
            obj.insert("portal".into(), self.portal_config());
            let cluster = self.cluster_stats();
            if !cluster.is_null() {
                obj.insert("cluster".into(), cluster);
            }
            let equivocations = self.space_roots.read().equivocation_count();
            if equivocations > 0 {
                obj.insert(
                    "space_root_equivocations".into(),
                    serde_json::json!(equivocations),
                );
            }
        }
        value
    }

    /// Returns the WebTransport base URL for the `web.host.h3.v1` listener.
    ///
    /// Used by the `/api/wt-url` endpoint so game pages loaded inside the
    /// native portal (where `location.hostname` is the base64url supernode
    /// ID rather than a resolvable DNS name) can discover the real address
    /// to pass to `new WebTransport(url)`.
    ///
    /// When `external_host` is not configured the URL falls back to
    /// `https://localhost:<web_port>`, which works for local development.
    pub(crate) fn wt_url_json(&self) -> serde_json::Value {
        if self.features.get("web.host.h3.v1").is_none() {
            return json!({"error": "web.host.h3.v1 not enabled on this supernode"});
        }
        let port = self.config.web_port.unwrap_or(8443);
        let host = match self.config.external_host.as_deref() {
            Some(h) if !h.is_empty() && h != "0.0.0.0" => h.to_owned(),
            _ => "localhost".to_owned(),
        };
        let url = format!("https://{host}:{port}");
        // Include the cert fingerprint alongside the URL so the SDK can use
        // serverCertificateHashes even when it reaches us via the /api/wt-url
        // fallback (i.e. before SUPERNODE_INFO has been delivered via the
        // native client trust chain).
        match &self.web_cert_fingerprint {
            Some(fp) => json!({"url": url, "certHash": fp}),
            None => json!({"url": url}),
        }
    }

    /// Returns the public portal configuration used by browser-side templates.
    /// Safe to expose: no secrets (access_code is omitted).
    pub(crate) fn portal_config(&self) -> serde_json::Value {
        json!({
            "title": self.config.web_title,
            "access_mode": self.access_controller.mode_name(),
            "demo_links": self.config.demo_links,
            "ad_duration": self.config.ad_duration,
            "tos_text": self.config.tos_text,
            "ad_content": self.config.ad_content,
            "web_localhost_only": self.config.web_localhost_only,
        })
    }

    pub(crate) fn collect_peers_info(&self) -> serde_json::Value {
        let store = self.peer_store.read();
        let connected_ids = self.signaling.connected_peer_ids();
        let connected_set: HashSet<&str> = connected_ids.iter().map(|s| s.as_str()).collect();

        let peers: Vec<serde_json::Value> = store
            .trusted_peer_ids()
            .iter()
            .filter_map(|id| {
                let rec = store.get_peer(id)?;
                let has_relay = self.access_controller.check_access(id);
                let online = connected_set.contains(id.as_str());
                Some(json!({
                    "handle": if rec.handle.is_empty() { &rec.peer_id[..12.min(rec.peer_id.len())] } else { &rec.handle },
                    "peer_id_short": &rec.peer_id[..12.min(rec.peer_id.len())],
                    "online": online,
                    "relay_access": has_relay,
                    "access_mode": self.access_controller.mode_name(),
                }))
            })
            .collect();

        json!(peers)
    }
}

/// Re-pad an un-padded base64url identifier (as produced by the relay's
/// `extract_peer_id`) into the padded form used by the SFU / signaling layer
/// (`public_id`). Appends `=` until the length is a multiple of 4; a string
/// that is already padded (or whose length is already aligned) is returned
/// unchanged. Cheap and infallible — no decode/re-encode round-trip.
fn pad_base64url(id: &str) -> String {
    match id.len() % 4 {
        0 => id.to_string(),
        rem => {
            let pad = 4 - rem;
            let mut s = String::with_capacity(id.len() + pad);
            s.push_str(id);
            s.extend(std::iter::repeat_n('=', pad));
            s
        }
    }
}

/// Decode the Opus payload size from a native `SfuAudio` signaling message.
fn sfu_audio_opus_byte_count(msg: &SignalingMessage) -> usize {
    use base64::Engine;
    msg.payload
        .get("audio")
        .and_then(|v| v.as_str())
        .and_then(|b64| base64::engine::general_purpose::URL_SAFE.decode(b64).ok())
        .map(|v| v.len())
        .unwrap_or(0)
}

/// Stable id for cluster audio-frame dedup. Prefer the Ed25519 signature
/// (unique per signed frame); fall back to room+sender+seq when unsigned
/// (should not happen on the production path).
fn audio_replication_id(msg: &SignalingMessage) -> String {
    if let Some(sig) = msg.signature.as_deref().filter(|s| !s.is_empty()) {
        return format!("a:{sig}");
    }
    let room = msg
        .payload
        .get("room_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let seq = msg.payload.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
    format!("a:{room}:{}:{seq}", msg.sender)
}

/// Payload byte count for `SfuChat` inbound quota accounting.
///
/// Prefer opaque `ciphertext` (E2E envelope) over legacy plaintext `body` so the
/// supernode never needs to parse message content for quota.
fn sfu_chat_byte_count(msg: &SignalingMessage) -> usize {
    msg.payload
        .get("ciphertext")
        .and_then(|v| v.as_str())
        .map(str::len)
        .or_else(|| {
            msg.payload
                .get("body")
                .and_then(|v| v.as_str())
                .map(str::len)
        })
        .unwrap_or(0)
}

/// Payload byte count for `SfuFile*` inbound quota accounting.
fn sfu_file_inbound_byte_count(msg: &SignalingMessage, mt: MessageType) -> usize {
    match mt {
        MessageType::SfuFileChunk => msg
            .payload
            .get("data")
            .and_then(|v| v.as_str())
            .map(str::len)
            .unwrap_or(0),
        MessageType::SfuFileOffer => msg
            .payload
            .get("size")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(0)
            .max(64),
        MessageType::SfuFileComplete => 64,
        _ => 0,
    }
}

/// Re-encode the relay cert CN (base64url **no-pad**) into the padded
/// `URL_SAFE` form used everywhere else as the canonical `public_id`
/// (`peer_sockets`, `quic_senders`, chat-subscriber rosters), so QUIC and
/// WebSocket delivery key peers identically. Falls back to the input on a
/// decode error.
fn canonical_peer_id(relay_cn: &str) -> String {
    use base64::Engine;
    match base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(relay_cn) {
        Ok(bytes) => base64::engine::general_purpose::URL_SAFE.encode(bytes),
        Err(_) => relay_cn.to_string(),
    }
}

/// Drive one peer's reliable QUIC relay **signaling** stream.
///
/// Inbound: length-prefixed signed `room.chat.v1` / `room.file.v1` frames run
/// the same verify + freshness + replay pipeline as the WebSocket path
/// ([`SignalingServer::accept_signed`]) before routing through the shared
/// [`SupernodeHandler::on_message`]. Outbound: room broadcasts addressed to
/// this peer by `send_to_peer` are written back over the same stream. The
/// connection is cert-authenticated, so the signed `sender` must match the
/// stream's peer. Only room broadcast message types are accepted here;
/// membership/handshake/control stay on the WebSocket signaling path.
async fn handle_relay_signaling_stream(
    state: Arc<SupernodeState>,
    relay_peer_id: String,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) {
    const MAX_FRAME: usize = 262_144;
    let peer_id = canonical_peer_id(&relay_peer_id);

    // Outbound: `send_to_peer` pushes JSON into this channel; the writer task
    // frames it onto the QUIC stream as `[u32 BE len][json]`.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    state.signaling.register_quic_sender(&peer_id, tx.clone());

    let writer = tokio::spawn(async move {
        while let Some(json) = rx.recv().await {
            let body = json.as_bytes();
            if body.len() > MAX_FRAME {
                continue;
            }
            if send
                .write_all(&(body.len() as u32).to_be_bytes())
                .await
                .is_err()
            {
                break;
            }
            if send.write_all(body).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
    });

    let handler = SupernodeHandler {
        state: state.clone(),
    };

    loop {
        let mut len_buf = [0u8; 4];
        if recv.read_exact(&mut len_buf).await.is_err() {
            break;
        }
        let len = u32::from_be_bytes(len_buf) as usize;
        if len == 0 || len > MAX_FRAME {
            break;
        }
        let mut buf = vec![0u8; len];
        if recv.read_exact(&mut buf).await.is_err() {
            break;
        }
        let Ok(raw) = String::from_utf8(buf) else {
            continue;
        };
        let Some(msg) = state.signaling.accept_signed(&raw) else {
            continue;
        };
        if msg.sender != peer_id {
            warn!(
                "Relay signaling sender {} != stream peer {} — dropping {:?}",
                &msg.sender[..12.min(msg.sender.len())],
                &peer_id[..12.min(peer_id.len())],
                msg.msg_type,
            );
            continue;
        }
        match msg.msg_type {
            MessageType::SfuChat
            | MessageType::SfuFileOffer
            | MessageType::SfuFileChunk
            | MessageType::SfuFileComplete => {
                handler.on_message(msg, &raw);
            }
            other => {
                debug!(
                    "Ignoring non-broadcast {:?} on relay signaling stream from {}",
                    other,
                    &peer_id[..12.min(peer_id.len())],
                );
            }
        }
    }

    state.signaling.unregister_quic_sender(&peer_id, &tx);
    writer.abort();
}

/// Implements SignalingHandler for the supernode.
struct SupernodeHandler {
    state: Arc<SupernodeState>,
}

impl SignalingHandler for SupernodeHandler {
    fn on_message(&self, msg: SignalingMessage, raw: &str) {
        match msg.msg_type {
            MessageType::InviteHandshakeInit => {
                self.handle_handshake_init(&msg);
            }
            MessageType::EndpointUpdate => {
                self.state.on_endpoint_update(&msg.sender, raw);
            }
            MessageType::SupernodeInfoRequest => {
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SupernodeInfo,
                    self.state.supernode_info_payload(),
                );
            }
            MessageType::SfuJoin => {
                self.handle_sfu_join(&msg);
            }
            MessageType::SfuLeave => {
                self.handle_sfu_leave(&msg);
            }
            MessageType::SfuRoomList => {
                self.handle_sfu_room_list(&msg);
            }
            MessageType::SfuChat => {
                self.handle_sfu_chat_broadcast(&msg, raw);
            }
            MessageType::SfuAudio => {
                self.handle_sfu_audio_broadcast(&msg, raw);
            }
            MessageType::SfuFileOffer
            | MessageType::SfuFileChunk
            | MessageType::SfuFileComplete => {
                self.handle_sfu_broadcast(&msg, raw, msg.msg_type);
            }
            MessageType::SfuSubscribe => {
                self.handle_sfu_subscribe(&msg);
            }
            MessageType::SfuUnsubscribe => {
                self.handle_sfu_unsubscribe(&msg);
            }
            MessageType::SfuRoomCreate => {
                self.handle_sfu_room_create(&msg);
            }
            MessageType::SfuRoomInvite => {
                self.handle_sfu_room_invite(&msg);
            }
            MessageType::SfuRoomInviteGenerate => {
                self.handle_sfu_invite_generate(&msg);
            }
            MessageType::SpaceRootAnnounce => {
                self.handle_space_root_announce(&msg);
            }
            MessageType::PunchRegister => {
                self.handle_punch_register(&msg);
            }
            MessageType::ChatMessage => {
                // Peer-targeted relay only — do not log or inspect payload fields;
                // content may be E2E-encrypted inside `encrypted_signal` envelopes.
            }
            MessageType::TrustRequest | MessageType::TrustAccept => {
                // Clients send these to the supernode (target=supernode_id) with the
                // actual recipient in payload["target"]. Relay raw message to that peer.
                if let Some(target_id) = msg.payload.get("target").and_then(|v| v.as_str()) {
                    self.state.signaling.send_to_peer(target_id, raw);
                    debug!(
                        "Relayed {:?} from {} → {}",
                        msg.msg_type,
                        &msg.sender[..12.min(msg.sender.len())],
                        &target_id[..12.min(target_id.len())],
                    );
                } else {
                    debug!(
                        "[trust] {:?} from {} missing payload.target — dropped",
                        msg.msg_type,
                        &msg.sender[..12.min(msg.sender.len())],
                    );
                }
            }
            MessageType::VersionAnnounce
            | MessageType::UpdateOffer
            | MessageType::UpdateAccept
            | MessageType::UpdateReject
            | MessageType::HandleUpdate
            | MessageType::PresenceUpdate
            | MessageType::SpeakingState
            | MessageType::CallRequest
            | MessageType::CallAccept
            | MessageType::CallReject
            | MessageType::CallEnd
            | MessageType::EncryptedSignal
            | MessageType::FileTransferOffer
            | MessageType::FileTransferAccept
            | MessageType::FileTransferReject
            | MessageType::FileTransferChunk
            | MessageType::FileTransferComplete
            | MessageType::FileTransferAck
            | MessageType::FileTransferError
            | MessageType::ChatAck
            | MessageType::ChatTyping
            | MessageType::PeerRoomInvite
            | MessageType::BuildAttestation
            | MessageType::AttestationResponse
            | MessageType::CapabilityAnnounce
            | MessageType::Pong => {
                // Peer-to-peer relay only — forwarding handled by signaling.rs.
            }
            MessageType::CapabilityInvoke => {
                // Route to a targeted peer when the payload carries a `target` field;
                // otherwise treat as a supernode-directed invocation.
                if let Some(target_id) = msg.payload.get("target").and_then(|v| v.as_str()) {
                    self.state.signaling.send_to_peer(target_id, raw);
                    debug!(
                        "Relayed CAPABILITY_INVOKE from {} → {}",
                        &msg.sender[..12.min(msg.sender.len())],
                        &target_id[..12.min(target_id.len())],
                    );
                } else {
                    let feature_id = msg.payload.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    if feature_id.is_empty() {
                        debug!(
                            "CAPABILITY_INVOKE from {} missing 'id' — dropped",
                            &msg.sender[..12.min(msg.sender.len())],
                        );
                    } else {
                        debug!(
                            "CAPABILITY_INVOKE '{}' from {} (supernode-directed, no module registered)",
                            feature_id,
                            &msg.sender[..12.min(msg.sender.len())],
                        );
                    }
                }
            }
            MessageType::Ping => {
                self.state
                    .send_signed(&msg.sender, MessageType::Pong, json!({}));
            }
            MessageType::RelayRequest => {
                // Client explicitly requests (or refreshes) a relay ticket.
                // Re-issue if the peer is trusted and has relay access.
                let trusted = self.state.peer_store.read().is_trusted(&msg.sender);
                if trusted && self.state.access_controller.check_access(&msg.sender) {
                    debug!(
                        "[relay] RelayRequest from {} — re-issuing ticket",
                        &msg.sender[..12.min(msg.sender.len())]
                    );
                    self.state.issue_relay_ticket(&msg.sender);
                } else {
                    debug!(
                        "[relay] RelayRequest from {} ignored (not trusted / no access)",
                        &msg.sender[..12.min(msg.sender.len())]
                    );
                }
            }
            _ => {
                // Unexpected message type — log for diagnostics.
                debug!(
                    "Unhandled message type {:?} from {}",
                    msg.msg_type,
                    &msg.sender[..12.min(msg.sender.len())],
                );
            }
        }
    }

    fn on_peer_connected(&self, identity_pub: &str) {
        let is_trusted = self.state.peer_store.read().is_trusted(identity_pub);
        if !is_trusted {
            info!(
                "Peer {} connected but NOT trusted — ignoring (handshake required)",
                &identity_pub[..12.min(identity_pub.len())],
            );
            return;
        }
        self.state.peer_store.write().touch_peer(identity_pub);
        self.state.on_peer_trusted(identity_pub);

        // Announce supernode version to the peer
        self.state.send_signed(
            identity_pub,
            MessageType::VersionAnnounce,
            json!({"version": APP_VERSION}),
        );

        // Also attest our build for reproducible build verification by clients.
        self.state.send_signed(
            identity_pub,
            MessageType::BuildAttestation,
            json!({
                "build_id": env!("CONQUERD_BUILD_ID"),
                "source_hash": env!("CONQUERD_SOURCE_HASH"),
                "version": APP_VERSION,
            }),
        );

        // Send SFU room list
        if let Some(ref sfu) = self.state.sfu {
            let rooms = sfu.read().get_rooms_for_peer(identity_pub);
            self.state.send_signed(
                identity_pub,
                MessageType::SfuRoomList,
                json!({"rooms": rooms}),
            );
        }
    }

    fn on_peer_disconnected(&self, identity_pub: &str) {
        self.state.features.clear_peer_quotas(identity_pub);
        self.state.features.clear_peer_outbound_quotas(identity_pub);

        // Remove from SFU rooms
        if let Some(ref sfu) = self.state.sfu {
            let left_rooms = sfu.write().remove_peer_from_all(identity_pub);
            if !left_rooms.is_empty() {
                for (room_id, members) in &left_rooms {
                    // Notify remaining members
                    for member in members {
                        self.state.send_signed(
                            member,
                            MessageType::SfuPeerLeft,
                            json!({"peer_id": identity_pub, "room_id": room_id}),
                        );
                    }
                }
                // Broadcast updated room list (participant counts changed)
                self.state.broadcast_room_list();
            }
        }
    }
}

impl SupernodeHandler {
    fn handle_handshake_init(&self, msg: &SignalingMessage) {
        let hs = self.state.handshake.read();
        match hs.process_init(&msg.payload) {
            Ok((accept_payload, _session_key, joiner_pub)) => {
                drop(hs);

                // Add to peer store
                let joiner_peer_id = msg
                    .payload
                    .get("joiner_peer_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let joiner_handle = msg
                    .payload
                    .get("joiner_handle")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let joiner_listener = msg
                    .payload
                    .get("joiner_listener")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let transcript_hash = accept_payload
                    .get("transcript_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let mut store = self.state.peer_store.write();
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs_f64();
                store.add_peer(PeerRecord {
                    peer_id: joiner_peer_id,
                    identity_pub: joiner_pub.clone(),
                    relay_hints: if joiner_listener.is_empty() {
                        vec![]
                    } else {
                        vec![joiner_listener]
                    },
                    handle: joiner_handle,
                    blocked: false,
                    revoked: false,
                    auto_connect: false,
                    is_supernode: false,
                    transcript_hash,
                    created_at: now,
                    last_seen_at: now,
                    quic_port: msg
                        .payload
                        .get("joiner_quic_port")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u16,
                });
                let _ = store.save();
                drop(store);

                // Send accept
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::InviteHandshakeAccept,
                    accept_payload,
                );

                info!(
                    "Handshake complete with {}",
                    &joiner_pub[..12.min(joiner_pub.len())]
                );

                // Trigger trust flow
                self.state.on_peer_trusted(&joiner_pub);

                // Announce supernode version to the newly trusted peer
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::VersionAnnounce,
                    json!({"version": APP_VERSION}),
                );

                // Attest build ID to the newly trusted peer as well.
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::BuildAttestation,
                    json!({
                        "build_id": env!("CONQUERD_BUILD_ID"),
                        "source_hash": env!("CONQUERD_SOURCE_HASH"),
                        "version": APP_VERSION,
                    }),
                );

                // Send SFU room list so rooms appear immediately
                if let Some(ref sfu) = self.state.sfu {
                    let rooms = sfu.read().get_rooms_for_peer(&joiner_pub);
                    self.state.send_signed(
                        &joiner_pub,
                        MessageType::SfuRoomList,
                        json!({"rooms": rooms}),
                    );
                }

                // Send trusted peer list
                let trusted = self.state.peer_store.read().trusted_peer_ids();
                let peer_list: Vec<serde_json::Value> = trusted
                    .iter()
                    .filter_map(|id| {
                        let store = self.state.peer_store.read();
                        store.get_peer(id).map(|p| {
                            json!({
                                "peer_id": p.peer_id,
                                "identity_pub": p.identity_pub,
                                "handle": p.handle,
                                "is_supernode": p.is_supernode,
                                "auto_connect": p.auto_connect,
                            })
                        })
                    })
                    .collect();
                self.state.send_signed(
                    &joiner_pub,
                    MessageType::Welcome,
                    json!({"peers": peer_list}),
                );
            }
            Err(e) => {
                warn!("Handshake init error: {}", e);
                self.state.send_signed(
                    &msg.sender,
                    MessageType::InviteHandshakeReject,
                    json!({"reason": e}),
                );
            }
        }
    }

    fn handle_sfu_join(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);

        // Proof-based admission (coexist): if the join carries a valid Space
        // proof (+ grant for private nodes), authorize + materialize the room
        // before the ACL check below. A no-op when absent → legacy ACL applies.
        self.state
            .try_space_admission(&msg.sender, room_id, &msg.payload);

        let (ok, members) = sfu.write().join_room(&msg.sender, room_id);
        if !ok {
            // Machine-readable reason for the client; detail stays in logs only.
            let (reason, detail) = {
                let s = sfu.read();
                let reason = s.classify_join_denial(&msg.sender, room_id);
                let detail = match s.get_room(room_id) {
                    None => String::new(),
                    Some(r) => format!(
                        "type={} creator_match={} allowed={} count={}",
                        match r.room_type {
                            sfu::RoomType::Public => "public",
                            sfu::RoomType::Private => "private",
                        },
                        r.creator_id == msg.sender,
                        r.is_peer_allowed(&msg.sender),
                        r.participant_count(),
                    ),
                };
                (reason, detail)
            };
            tracing::warn!(
                "SfuJoin DENIED peer={} room={} reason={} [{}]",
                &msg.sender[..12.min(msg.sender.len())],
                room_id,
                reason,
                detail
            );
            // Tell the client so optimistic voice/UI can roll back — a silent
            // deny previously left peers "in room" with no SFU membership.
            self.state.send_signed(
                &msg.sender,
                MessageType::SfuJoinResult,
                json!({
                    "room_id": room_id,
                    "accepted": false,
                    "reason": reason,
                }),
            );
            return;
        }

        // Join relay room too
        if let Some(ref relay) = self.state.relay {
            relay.join_room(&msg.sender, room_id);
        }

        // Send member list to joiner
        self.state.send_signed(
            &msg.sender,
            MessageType::SfuMembers,
            json!({"room_id": room_id, "members": members}),
        );

        // Notify existing members
        for member in &members {
            if member != &msg.sender {
                self.state.send_signed(
                    member,
                    MessageType::SfuPeerJoined,
                    json!({"peer_id": msg.sender, "room_id": room_id}),
                );
            }
        }

        info!(
            "Peer {} joined room {}",
            &msg.sender[..12.min(msg.sender.len())],
            room_id
        );

        // Participant IDs/counts changed; refresh every connected peer's
        // room sidebar so voice stats stay scoped to the actual room.
        self.state.broadcast_room_list();

        // Attempt relay-coordinated hole punch with existing room members
        self.state.try_relay_punch_for_room(&msg.sender, room_id);
    }

    fn handle_sfu_leave(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);

        let remaining = sfu.write().leave_room(&msg.sender, room_id);

        if let Some(ref relay) = self.state.relay {
            relay.leave_room(&msg.sender);
        }

        for member in &remaining {
            self.state.send_signed(
                member,
                MessageType::SfuPeerLeft,
                json!({"peer_id": msg.sender, "room_id": room_id}),
            );
        }

        // Participant IDs/counts changed; refresh room sidebar stats.
        self.state.broadcast_room_list();
    }

    fn handle_sfu_room_list(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let rooms = sfu.read().get_rooms_for_peer(&msg.sender);
        self.state.send_signed(
            &msg.sender,
            MessageType::SfuRoomList,
            json!({"rooms": rooms}),
        );
    }

    /// Relay native WebSocket `SfuFile*` with symmetric `room.file.v1` quotas.
    fn handle_sfu_broadcast(&self, msg: &SignalingMessage, raw: &str, mt: MessageType) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let payload_bytes = sfu_file_inbound_byte_count(msg, mt);
        if payload_bytes == 0 {
            return;
        }
        if !self.state.features.gate_inbound_through_feature(
            "room.file.v1",
            &msg.sender,
            payload_bytes,
        ) {
            tracing::debug!(
                "[room.file.v1] inbound quota exceeded for {}; dropping relay",
                &msg.sender[..12.min(msg.sender.len())]
            );
            return;
        }

        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);
        let recipients = sfu.read().get_chat_recipients(room_id);
        let wire_bytes = raw.len();
        for peer in &recipients {
            if peer == &msg.sender {
                continue;
            }
            if self
                .state
                .features
                .gate_through_feature("room.file.v1", peer, wire_bytes)
            {
                self.state.signaling.send_to_peer(peer, raw);
            }
        }
    }

    /// Relay native WebSocket `SfuAudio` with symmetric `room.audio.sfu` quotas.
    fn handle_sfu_audio_broadcast(&self, msg: &SignalingMessage, raw: &str) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let opus_bytes = sfu_audio_opus_byte_count(msg);
        if opus_bytes == 0 {
            return;
        }
        if !self.state.features.gate_inbound_through_feature(
            "room.audio.sfu",
            &msg.sender,
            opus_bytes,
        ) {
            tracing::debug!(
                "[room.audio.sfu] inbound quota exceeded for {}; dropping relay",
                &msg.sender[..12.min(msg.sender.len())]
            );
            return;
        }

        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);
        // Active-speaker gate: drop the frame entirely when the sender is over
        // the room's concurrent-talker cap (bounds per-receiver fan-out).
        let recipients = match sfu.write().audio_forward_targets_now(room_id, &msg.sender) {
            Some(r) => r,
            None => return,
        };
        let wire_bytes = raw.len();
        for peer in &recipients {
            if peer == &msg.sender {
                continue;
            }
            if self
                .state
                .features
                .gate_through_feature("room.audio.sfu", peer, wire_bytes)
            {
                self.state.signaling.send_to_peer(peer, raw);
            }
        }
        // Cross-node room members (attached to a sibling supernode) only hear
        // this talker if we fan the same opaque frame over the cluster link —
        // parity with room.chat.v1's replicate_room_chat path.
        self.state.replicate_room_audio(room_id, msg, raw);
    }

    /// Relay native WebSocket `SfuChat` with symmetric `room.chat.v1` quotas.
    fn handle_sfu_chat_broadcast(&self, msg: &SignalingMessage, raw: &str) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let body_bytes = sfu_chat_byte_count(msg);
        if body_bytes == 0 {
            return;
        }
        if !self.state.features.gate_inbound_through_feature(
            "room.chat.v1",
            &msg.sender,
            body_bytes,
        ) {
            tracing::debug!(
                "[room.chat.v1] inbound quota exceeded for {}; dropping relay",
                &msg.sender[..12.min(msg.sender.len())]
            );
            return;
        }

        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(sfu::DEFAULT_ROOM_ID);
        if !sfu.read().is_chat_sender(room_id, &msg.sender) {
            tracing::debug!(
                "[room.chat.v1] sender {} is not a member of room {} — dropping chat",
                &msg.sender[..12.min(msg.sender.len())],
                &room_id[..12.min(room_id.len())]
            );
            return;
        }
        let recipients = sfu.read().get_chat_recipients(room_id);
        let wire_bytes = raw.len();
        for peer in &recipients {
            if peer == &msg.sender {
                continue;
            }
            if self
                .state
                .features
                .gate_through_feature("room.chat.v1", peer, wire_bytes)
            {
                self.state.signaling.send_to_peer(peer, raw);
            }
        }
        // Fan the same opaque frame out to cluster peers that host members of
        // this room, so a member attached to a different supernode still
        // receives it. No-op when standalone.
        self.state.replicate_room_chat(room_id, msg, raw);
    }

    fn handle_sfu_subscribe(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if room_id.is_empty() {
            return;
        }
        let ok = sfu.write().subscribe(&msg.sender, room_id);
        if ok {
            debug!(
                "Peer {} subscribed to room {} text chat",
                &msg.sender[..12.min(msg.sender.len())],
                &room_id[..12.min(room_id.len())]
            );
        }
    }

    fn handle_sfu_unsubscribe(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if room_id.is_empty() {
            return;
        }
        sfu.write().unsubscribe(&msg.sender, room_id);
        debug!(
            "Peer {} unsubscribed from room {} text chat",
            &msg.sender[..12.min(msg.sender.len())],
            &room_id[..12.min(room_id.len())]
        );
    }

    fn handle_sfu_room_create(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_name = msg
            .payload
            .get("room_name")
            .and_then(|v| v.as_str())
            .unwrap_or("Room");
        let room_type: sfu::RoomType = msg
            .payload
            .get("room_type")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or(sfu::RoomType::Public);
        let room_id = msg.payload.get("room_id").and_then(|v| v.as_str());
        let creator_id = msg
            .payload
            .get("creator_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&msg.sender);
        // Client-supplied invite policy (`"owner"` | `"members"`); absent or
        // unrecognized values normalize to the safe `"owner"` default inside
        // `create_room_with_policy`. The client is the authority on a room's
        // Space-linked `invite_policy` (same trust level as `room_name`/
        // `room_type`, which are also client-supplied here and unverified).
        let invite_policy = msg
            .payload
            .get("invite_policy")
            .and_then(|v| v.as_str())
            .unwrap_or("owner");

        let resolved_id = room_id
            .map(String::from)
            .unwrap_or_else(|| crate::crypto::derive_room_id(creator_id, room_name));
        let is_default = resolved_id == sfu::DEFAULT_ROOM_ID;
        let exists = sfu.read().get_room(&resolved_id).is_some();
        if !exists && !is_default {
            let policy = self.state.sfu_room_policy;
            let is_public = matches!(room_type, sfu::RoomType::Public);
            if let Some(reason) = policy.deny_reason_for_new_room(is_public) {
                warn!(
                    "Denied SFU room create from {}: {} ({})",
                    &msg.sender[..12.min(msg.sender.len())],
                    reason,
                    room_name
                );
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SfuRoomCreated,
                    json!({
                        "room_id": "",
                        "room_name": room_name,
                        "room_type": room_type,
                        "denied": true,
                        "reason": reason,
                    }),
                );
                return;
            }
        }

        // Optional durable invite credential the client kept in RoomStore.
        // After idle GC the supernode's token map is empty; replaying a saved
        // definition re-seeds the credential so members can rejoin without a
        // brand-new share link.
        let client_invite_token = msg
            .payload
            .get("invite_token")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_owned);

        let mut sfu_lock = sfu.write();
        let Some((room, created_new)) = sfu_lock.create_room_with_policy(
            room_id,
            room_name,
            room_type,
            creator_id,
            invite_policy,
        ) else {
            return;
        };
        let room_id_out = room.room_id.clone();
        let room_name_out = room.room_name.clone();
        let room_creator = room.creator_id.clone();
        let is_private = room_type == sfu::RoomType::Private;
        drop(sfu_lock);

        // Private-room re-admit after rematerialize (client-owned defs, ephemeral
        // SFU). Trust rules:
        //   * creator (payload or room record) is always re-allowed;
        //   * any peer presenting a non-empty invite_token re-seeds that token
        //     as a multi-use credential and is allowed (possession of the saved
        //     RoomStore entry + token is the membership proof after GC);
        //   * first create without a client token mints a fresh single-use
        //     shareable invite for the creator to distribute.
        // Room-id alone is NOT enough — non-creators without a token are not
        // admitted on a bare materialize of an already-existing room.
        let mut invite_token: Option<String> = None;
        if is_private {
            let is_creator = msg.sender == room_creator || msg.sender == creator_id;
            let mut admitted = false;

            if let Some(ref tok) = client_invite_token {
                let reseeded = sfu.write().reregister_invite_token(
                    &room_id_out,
                    tok,
                    if room_creator.is_empty() {
                        creator_id
                    } else {
                        &room_creator
                    },
                );
                if reseeded {
                    let _ = sfu.write().allow_peer(&room_id_out, &msg.sender);
                    invite_token = Some(tok.clone());
                    admitted = true;
                }
            }

            if !admitted && (is_creator || created_new) {
                // Creator (or first materializer creating a brand-new room)
                // self-admits without a token. Local allow only — cold cluster
                // members re-admit via Space proof or rematerialize + token re-seed.
                let _ = sfu.write().allow_peer(&room_id_out, &msg.sender);
                if created_new && invite_token.is_none() {
                    invite_token = sfu.write().generate_invite_token(&room_id_out, creator_id);
                }
            }
        }

        self.state.send_signed(
            &msg.sender,
            MessageType::SfuRoomCreated,
            json!({
                "room_id": room_id_out,
                "room_name": room_name_out,
                "room_type": room_type,
                "invite_token": invite_token,
            }),
        );
        // Do not broadcast here: the room is still empty (no voice join yet).
        // Broadcasting a count=0 snapshot races the client's immediate SfuJoin
        // and can overwrite the sidebar bubble with stale stats. Join/leave
        // paths call broadcast_room_list() once participant_ids are current.
    }

    fn handle_sfu_room_invite(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let token = msg
            .payload
            .get("invite_token")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Proof-based admission first (cluster-portable). Otherwise fall back
        // to the local invite token or already-allowed re-entry on this node.
        let has_proof = msg.payload.get("space_proof").is_some();
        let room_exists = sfu.read().get_room(room_id).is_some();
        // Re-entry: a peer previously admitted on *this* node is still in its
        // local `allowed` set. The single-use invite token is consumed on first
        // use, so subsequent re-entry re-sends a spent token — admit already-
        // allowed peers directly (same node only; cold nodes need Space proof
        // or token rematerialize).
        let already_member = sfu
            .read()
            .get_room(room_id)
            .is_some_and(|r| r.is_peer_allowed(&msg.sender));
        let by_proof = self
            .state
            .try_space_admission(&msg.sender, room_id, &msg.payload);
        let by_token = !by_proof
            && sfu
                .write()
                .validate_room_invite(room_id, token, &msg.sender);
        let valid = by_proof || by_token || already_member;
        tracing::warn!(
            "SfuRoomInvite peer={} room={} exists={} has_proof={} token_len={} by_proof={} by_token={} already_member={} => valid={}",
            &msg.sender[..12.min(msg.sender.len())],
            room_id,
            room_exists,
            has_proof,
            token.len(),
            by_proof,
            by_token,
            already_member,
            valid
        );
        let room_info = sfu
            .read()
            .get_room(room_id)
            .map(|r| (r.room_name.clone(), r.room_type, r.participant_count()));

        if let Some((name, rtype, count)) = room_info {
            self.state.send_signed(
                &msg.sender,
                MessageType::SfuRoomInviteResult,
                json!({
                    "room_id": room_id,
                    "accepted": valid,
                    "room_name": name,
                    "room_type": rtype,
                    "member_count": count,
                    "reason": if valid { "" } else { "invalid_token" },
                }),
            );

            // Room visibility + counts arrive via the client's pending SfuJoin
            // (SfuMembers sidebar patch + post-join broadcast_room_list). A
            // pre-join SfuRoomList here races that path and resets private-room
            // voice bubbles to the pre-join participant count.
        }
    }

    fn handle_sfu_invite_generate(&self, msg: &SignalingMessage) {
        let Some(ref sfu) = self.state.sfu else {
            return;
        };
        let room_id = msg
            .payload
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Owner-only invite minting: only the room's creator may mint a token.
        // Closes the hole where any authenticated peer could mint an invite for
        // any room and add themselves (SPACE-MERKLE-DESIGN §6.1).
        fn short(s: &str) -> &str {
            &s[..12.min(s.len())]
        }
        match sfu
            .write()
            .generate_invite_token_checked(room_id, &msg.sender)
        {
            sfu::InviteMint::Ok(token) => {
                info!(
                    "[sfu] Generated invite token for room {} requested by owner {}",
                    short(room_id),
                    short(&msg.sender),
                );
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SfuRoomInviteResult,
                    json!({"room_id": room_id, "accepted": true, "invite_token": token}),
                );
            }
            sfu::InviteMint::NotAuthorized => {
                warn!(
                    "[sfu] Invite generate denied for room {} — {} is not the room creator",
                    short(room_id),
                    short(&msg.sender),
                );
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SfuRoomInviteResult,
                    json!({"room_id": room_id, "accepted": false, "reason": "not_room_creator"}),
                );
            }
            sfu::InviteMint::RoomNotFound => {
                warn!(
                    "[sfu] Invite generate failed for room {} — room not found",
                    short(room_id),
                );
                self.state.send_signed(
                    &msg.sender,
                    MessageType::SfuRoomInviteResult,
                    json!({"room_id": room_id, "accepted": false, "reason": "room_not_found"}),
                );
            }
        }
    }

    /// The owner announces a signed Space root. Verify the owner signature,
    /// store the highest epoch, and cluster-gossip it so any member can later
    /// admit by proof against it (authenticated room-set sync, §8).
    fn handle_space_root_announce(&self, msg: &SignalingMessage) {
        let Some(root) = msg
            .payload
            .get("root")
            .cloned()
            .and_then(|v| serde_json::from_value::<space::SignedSpaceRoot>(v).ok())
        else {
            return;
        };
        // The announcer must be the root's signer (owner) — a peer can't push
        // someone else's root here (gossip re-verifies the signature anyway).
        if root.signer.trim_end_matches('=') != msg.sender.trim_end_matches('=') {
            return;
        }
        if self.state.accept_and_gossip_space_root(root) {
            debug!(
                "[space] accepted root from owner {}",
                &msg.sender[..12.min(msg.sender.len())]
            );
        }
    }

    fn handle_punch_register(&self, msg: &SignalingMessage) {
        let target = msg
            .payload
            .get("target_peer")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let endpoint = msg
            .payload
            .get("sender_endpoint")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if target.is_empty() || endpoint.is_empty() {
            return;
        }
        self.state
            .handle_punch_register_msg(&msg.sender, target, endpoint);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install the ring crypto provider before any TLS/QUIC activity.
    // Required when rustls is built with default-features = false.
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,conquerd_supernode=debug".parse().unwrap()),
        )
        .init();

    let mut config = Config::from_env();
    let manifest = load_manifest(&config);
    manifest.apply_to_config(&mut config);
    info!(
        "ConquerD Supernode v{} starting (signaling={}, relay={}, web={:?})",
        APP_VERSION, config.signaling_port, config.relay_port, config.web_port
    );

    // Ensure data directory
    std::fs::create_dir_all(&config.data_dir)?;

    // Load or create identity
    let identity = Identity::load_or_create(&config.data_dir)?;
    info!(
        "Identity: {} (peer: {}...)",
        &identity.public_id()[..12],
        &identity.peer_id()[..12]
    );

    // Load peer store
    let peer_store = PeerStore::new(&config.data_dir.join("peers.json"));
    info!("Loaded {} trusted peers", peer_store.trusted_count());

    // Initialize access controller
    let access_controller = create_access_controller(config.access_mode, &config.access_code);

    // Handshake manager
    let listener_host = config.external_host.as_deref().unwrap_or("0.0.0.0");
    let listener_url = format!("ws://{}:{}", listener_host, config.signaling_port);
    if listener_host == "0.0.0.0" {
        warn!("Invite listener_url uses 0.0.0.0 — set supernode_host env var for remote clients");
    }
    let mut handshake =
        HandshakeManager::new(identity.clone(), listener_url, config.invite_ttl_seconds);
    handshake.node_title = config.web_title.clone();
    // Embed TURN relay hint so clients can fall back to the relay port.
    if listener_host != "0.0.0.0" {
        handshake.turn_hints = Some(vec![format!(
            "turn:{}:{}",
            listener_host, config.relay_port
        )]);
    }

    let features = Arc::new(build_feature_registry(&manifest, &config));

    // QUIC relay server
    let relay = {
        let relay = QUICRelayServer::new(identity.public_id(), Arc::clone(&features));
        let bind = SocketAddr::from(([0, 0, 0, 0], config.relay_port));
        let port = relay.start(bind).await?;
        info!("QUIC relay on port {}", port);
        // Pre-authorize existing trusted peers
        for pid in peer_store.trusted_peer_ids() {
            relay.allow_peer(&pid);
        }
        Some(relay)
    };

    // SFU room manager
    let sfu = if config.sfu_enabled {
        info!("SFU enabled — rooms are ephemeral (peer-owned definitions, idle GC)");
        Some(RwLock::new(SFURoomManager::new()))
    } else {
        None
    };

    // Load endpoint mailbox from disk
    let endpoint_mailbox = SupernodeState::load_endpoint_mailbox(&config.data_dir);

    // Signaling server
    let signaling = SignalingServer::new(identity.public_id());

    // Build shared state using Arc::new_cyclic so feature modules can hold a
    // Weak<SupernodeState> without a circular strong-reference cycle.
    //
    // Generate/reuse the self-signed WebTransport cert before building state
    // so the fingerprint is available for SUPERNODE_INFO immediately.
    let web_cert_fingerprint = if config.web_port.is_some() {
        ensure_web_cert(&config.data_dir)
    } else {
        None
    };

    // Cluster membership: validate the operator-declared roster against this
    // node's identity. An invalid roster disables clustering (run standalone)
    // rather than failing startup.
    let cluster =
        manifest
            .cluster
            .clone()
            .and_then(|cfg| match cfg.validate(&identity.public_id()) {
                Ok(()) => {
                    let membership = cluster::ClusterMembership::new(cfg, &identity.public_id());
                    info!(
                        "Cluster '{}' enabled with {} member(s)",
                        membership.cluster_id(),
                        membership.member_count()
                    );
                    Some(membership)
                }
                Err(e) => {
                    warn!("Ignoring invalid [cluster] config (running standalone): {e}");
                    None
                }
            });

    let state: Arc<SupernodeState> =
        Arc::new_cyclic(|_weak: &std::sync::Weak<SupernodeState>| SupernodeState {
            config: config.clone(),
            identity: identity.clone(),
            peer_store: RwLock::new(peer_store),
            handshake: RwLock::new(handshake),
            relay,
            sfu,
            signaling,
            access_controller,
            start_time: Instant::now(),
            ticket_expiry: RwLock::new(HashMap::new()),
            endpoint_mailbox: RwLock::new(endpoint_mailbox),
            pending_punches: RwLock::new(HashMap::new()),
            features: Arc::clone(&features),
            web_bridge: BrowserBridge::new(),
            web_cert_fingerprint,
            sfu_room_policy: manifest::sfu_room_creation_policy(&features),
            cluster,
            cluster_link: RwLock::new(None),
            // Sized for chat + multi-talker room audio (~50 Hz) dedup windows.
            replication_seen: RwLock::new(cluster_link::SeenCache::new(16_384)),
            space_roots: RwLock::new(SpaceRootStore::default()),
        });

    // Bring up the intra-cluster link when clustering is enabled and this node
    // has a cluster_addr to bind. Callbacks hold a Weak<SupernodeState> so the
    // link doesn't keep the state alive.
    if let Some(membership) = state.cluster.clone() {
        let weak = Arc::downgrade(&state);
        let on_replicate: cluster_link::OnReplicateFn = {
            let weak = weak.clone();
            Arc::new(move |m: cluster_link::ReplicatedMsg| {
                if let Some(state) = weak.upgrade() {
                    state.deliver_replicated_room_frame(&m.room_id, &m.message_id, &m.raw);
                }
            })
        };
        let on_room_roster: cluster_link::OnRoomRosterFn = {
            let weak = weak.clone();
            Arc::new(move |desc: cluster_link::RoomDescriptor| {
                if let Some(state) = weak.upgrade() {
                    state.apply_room_roster(&desc);
                }
            })
        };
        let on_peer_auth: cluster_link::OnPeerAuthFn = {
            let weak = weak.clone();
            Arc::new(move |g: cluster_link::PeerAuthGrant| {
                if let Some(state) = weak.upgrade() {
                    state.apply_peer_auth(&g.identity_pub, &g.handle);
                }
            })
        };
        let on_space_root: cluster_link::OnSpaceRootFn = {
            let weak = weak.clone();
            Arc::new(move |root: space::SignedSpaceRoot| {
                if let Some(state) = weak.upgrade() {
                    // Gossip is full-mesh: accept (verify + highest-epoch) but do
                    // not re-forward — the origin already broadcast to all peers.
                    state.space_roots.write().accept(root);
                }
            })
        };
        let local_rooms: cluster_link::LocalRoomsFn = {
            let weak = weak.clone();
            Arc::new(move || {
                weak.upgrade()
                    .and_then(|s| s.sfu.as_ref().map(|sfu| sfu.read().subscribed_room_ids()))
                    .unwrap_or_default()
            })
        };
        let local_room_roster: cluster_link::LocalRoomRosterFn = {
            let weak = weak.clone();
            Arc::new(move || {
                let Some(state) = weak.upgrade() else {
                    return Vec::new();
                };
                let Some(ref sfu) = state.sfu else {
                    return Vec::new();
                };
                let descriptors = sfu.read().durable_room_descriptors();
                descriptors
                    .into_iter()
                    .map(
                        |(room_id, room_name, room_type, creator_id, invite_policy)| {
                            cluster_link::RoomDescriptor {
                                room_id,
                                room_name,
                                room_type: match room_type {
                                    sfu::RoomType::Public => "public".to_owned(),
                                    sfu::RoomType::Private => "private".to_owned(),
                                },
                                creator_id,
                                invite_policy,
                            }
                        },
                    )
                    .collect()
            })
        };
        let local_space_roots: cluster_link::LocalSpaceRootsFn = {
            let weak = weak.clone();
            Arc::new(move || {
                weak.upgrade()
                    .map(|s| s.space_roots.read().all())
                    .unwrap_or_default()
            })
        };
        let link = cluster_link::ClusterLink::new(
            identity.clone(),
            membership,
            on_replicate,
            on_room_roster,
            on_peer_auth,
            on_space_root,
        );
        match link
            .start(local_rooms, local_room_roster, local_space_roots)
            .await
        {
            Ok(port) => {
                info!("Cluster link started on port {port}");
                *state.cluster_link.write() = Some(link);
            }
            Err(e) => warn!("Cluster link not started: {e}"),
        }
    }

    // Install the `web.host.app.v1` bidi-stream hook on the relay so the
    // embedded Chromium view in the desktop client can fetch
    // `conquerd://<supernode_pub>/<path>` assets from `<data_dir>/web/`
    // and `<data_dir>/games/` over the already-identity-verified QUIC
    // session. The hook is fire-and-forget; the module spawns its own
    // per-stream task with a deadline so a slow client cannot pin us.
    if state.features.get("web.host.app.v1").is_some() {
        if let Some(ref relay) = state.relay {
            let weak = std::sync::Arc::downgrade(&state);
            let data_dir = state.config.data_dir.clone();
            let module =
                std::sync::Arc::new(web_app_module::WebAppHostModule::new(weak, &data_dir));
            let hook: relay::BidiStreamHook = {
                let module = module.clone();
                std::sync::Arc::new(move |peer_id, send, recv, prefetched_len| {
                    let module = module.clone();
                    tokio::spawn(async move {
                        module
                            .handle_stream(peer_id, send, recv, prefetched_len)
                            .await;
                    });
                })
            };
            relay.set_bidi_hook(hook);
            // Ensure the asset roots exist and seed default index pages so
            // the portal has something to show out of the box. Files are
            // only written if they do not already exist so operator
            // customisations are never overwritten.
            seed_web_defaults(&state.config.data_dir);
            info!(
                "[features] web.host.app.v1 serving assets from {}/web/, {}/games/ (example + shared-drawing + brick-breaker) and {}/web-sdk/",
                state.config.data_dir.display(),
                state.config.data_dir.display(),
                state.config.data_dir.display()
            );
        }
    }

    // Install the reliable signaling-stream hook so `room.chat.v1` /
    // `room.file.v1` broadcasts ride the QUIC relay connection (no TCP
    // head-of-line blocking) whenever a peer has a relay session open, with
    // the WebSocket signaling path as automatic fallback. Requires the SFU
    // (rooms) and the relay listener.
    if state.sfu.is_some() {
        if let Some(ref relay) = state.relay {
            let weak = std::sync::Arc::downgrade(&state);
            let hook: relay::SignalStreamHook = std::sync::Arc::new(move |peer_id, send, recv| {
                let Some(state) = weak.upgrade() else {
                    return;
                };
                tokio::spawn(handle_relay_signaling_stream(state, peer_id, send, recv));
            });
            relay.set_signal_hook(hook);
            info!(
                "[features] room.chat.v1/room.file.v1 reliable broadcast over QUIC relay enabled"
            );
        }
    }

    // Install the room-audio datagram bridge so `room.audio.sfu` frames a peer
    // sends over its QUIC relay session (unreliable datagrams — no TCP
    // head-of-line blocking) are fanned out to *every* room member by their
    // best transport: relay datagram for relay-connected members, WebSocket
    // for the rest. The frame stays end-to-end signed, so this never
    // partitions a WS-only member or weakens the signed-forwarder model.
    if state.sfu.is_some() {
        if let Some(ref relay) = state.relay {
            let weak = std::sync::Arc::downgrade(&state);
            let bridge: relay::RoomAudioBridgeHook = std::sync::Arc::new(
                move |from_peer: String, sender_index: u8, room_id: String, inner: Vec<u8>| {
                    let Some(state) = weak.upgrade() else {
                        return;
                    };
                    let Some(ref sfu) = state.sfu else {
                        return;
                    };
                    let Some(ref relay) = state.relay else {
                        return;
                    };
                    // `inner` is `[ROOM_AUDIO_TAG][signed SfuAudio JSON]`. WS
                    // recipients want the signed JSON; relay recipients want the
                    // index-prefixed datagram.
                    let signed_json = &inner[1..];
                    let Ok(raw) = std::str::from_utf8(signed_json) else {
                        return;
                    };
                    let fwd = crate::wire::build_forwarded_datagram(sender_index, &inner);
                    // The relay identifies peers by the *un-padded* base64url id
                    // (`extract_peer_id`), but the SFU room — populated from the
                    // WebSocket `SfuJoin` — keys participants by the *padded*
                    // `public_id`. Re-pad `from_peer` into the SFU's id space so
                    // the active-speaker gate recognizes the sender and the
                    // exclusion below actually fires; otherwise the talker is
                    // never excluded from the fan-out and hears their own audio
                    // echoed back through the codec round-trip.
                    let from_peer = pad_base64url(&from_peer);
                    // Active-speaker gate (same cap as the WS path): drop the
                    // frame server-side when the sender is over the room's
                    // concurrent-talker limit.
                    let members = match sfu.write().audio_forward_targets_now(&room_id, &from_peer)
                    {
                        Some(m) => m,
                        None => return,
                    };
                    for member in members {
                        if member == from_peer {
                            continue;
                        }
                        match relay.send_room_datagram(&member, &fwd) {
                            // Delivered (or dropped on quota/send error) over the
                            // relay — do not also send over WS for this member.
                            Some(_) => {}
                            // Not relay-connected: deliver over WebSocket,
                            // charging the same `room.audio.sfu` outbound quota.
                            None => {
                                if state.features.gate_through_feature(
                                    "room.audio.sfu",
                                    &member,
                                    raw.len(),
                                ) {
                                    state.signaling.send_to_peer(&member, raw);
                                }
                            }
                        }
                    }
                    // Cluster fan-out so members on sibling nodes hear this
                    // talker (same as the WebSocket SfuAudio path).
                    if let Ok(parsed) = SignalingMessage::from_json(raw) {
                        state.replicate_room_audio(&room_id, &parsed, raw);
                    }
                },
            );
            relay.set_room_audio_bridge(bridge);
            info!("[features] room.audio.sfu datagram fan-out over QUIC relay enabled");
        }
    }

    // Start WebTransport listener (stub) when the manifest enabled
    // `web.host.h3.v1`. The bridge is always allocated; the listener
    // task only spins up when the capability is registered.
    if state.features.get("web.host.h3.v1").is_some() {
        let bridge = state.web_bridge.clone();
        let port = state.config.web_port.unwrap_or(8443);
        let data_dir = state.config.data_dir.clone();

        // If the SFU is enabled, register `room.audio.sfu` and
        // `room.chat.v1` as `FeatureModule` instances on the registry,
        // then install `ModuleNativeDispatcher` so browser-originated
        // `room.*` payloads land in the registry exactly once per
        // inbound message. The modules own verify+enumerate+forward.
        if state.sfu.is_some() {
            let weak = std::sync::Arc::downgrade(&state);
            let audio: std::sync::Arc<dyn conquerd_features::FeatureModule> =
                std::sync::Arc::new(sfu_module::SfuRoomModule::audio(weak.clone()));
            let chat: std::sync::Arc<dyn conquerd_features::FeatureModule> =
                std::sync::Arc::new(sfu_module::SfuRoomModule::chat(weak));
            // Manifest-loaded descriptors usually exist already; bind
            // first, fall back to register if absent.
            if !state.features.bind_module("room.audio.sfu", audio.clone()) {
                let _ = state.features.register_module(audio);
            }
            if !state.features.bind_module("room.chat.v1", chat.clone()) {
                let _ = state.features.register_module(chat);
            }

            let native_hook: webtransport::NativeMessageHook = {
                let state_for_hook = std::sync::Arc::downgrade(&state);
                std::sync::Arc::new(move |source, feature_id, payload| {
                    if let Some(s) = state_for_hook.upgrade() {
                        // Inbound quota is enforced in BrowserBridge::on_inbound;
                        // invoke the bound module directly to avoid double-charging.
                        if let Some(module) = s.features.module(feature_id) {
                            module.on_message(source.to_string(), payload);
                        }
                    }
                })
            };
            bridge.set_dispatcher(std::sync::Arc::new(
                webtransport::ModuleNativeDispatcher::new(native_hook),
            ));
        }

        bridge.set_features(Arc::clone(&state.features));

        tokio::spawn(async move {
            webtransport::run_listener(bridge, data_dir, port).await;
        });
    }

    // Start signaling
    let handler = Arc::new(SupernodeHandler {
        state: state.clone(),
    });
    let bind = SocketAddr::from(([0, 0, 0, 0], config.signaling_port));
    let sig_port = state.signaling.start(bind, handler).await?;
    info!("Signaling on port {}", sig_port);

    // Create and print invite (restore from disk if available)
    {
        let mut hs = state.handshake.write();
        if !hs.load_reusable_invite(&config.data_dir) {
            hs.get_or_create_reusable_invite(Some(&config.web_title));
            hs.save_reusable_invite(&config.data_dir);
        }
        let invite = hs.reusable_invite.as_ref().unwrap();
        let uri = invite.to_uri();
        info!("═══════════════════════════════════════");
        info!("Invite URL: {}", uri);
        info!("═══════════════════════════════════════");
    }

    // Ticket renewal timer
    let state_renewal = state.clone();
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(RENEWAL_CHECK_INTERVAL_S));
        loop {
            interval.tick().await;
            check_ticket_renewals(&state_renewal);
        }
    });

    // Idle SFU room GC — peer-materialized rooms are dropped after inactivity.
    let state_gc = state.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let Some(ref sfu) = state_gc.sfu else {
                continue;
            };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64();
            let removed = sfu.write().gc_idle_rooms(now, sfu::IDLE_ROOM_GC_SECS);
            if !removed.is_empty() {
                info!("GC removed {} idle SFU room(s)", removed.len());
                state_gc.broadcast_room_list();
            }
        }
    });

    // Wait for shutdown signal (SIGINT or SIGTERM)
    info!("Supernode running. Press Ctrl+C to stop.");
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = sigterm.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    info!("Shutting down...");

    // Cleanup
    if let Some(ref relay) = state.relay {
        relay.shutdown();
    }
    let _ = state.peer_store.read().save();

    info!("Goodbye.");
    Ok(())
}

/// Check and renew expiring relay tickets.
fn check_ticket_renewals(state: &SupernodeState) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();

    let expiries: Vec<(String, f64)> = state
        .ticket_expiry
        .read()
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();

    for (peer_id, expires_at) in expiries {
        if !state.signaling.is_peer_connected(&peer_id) {
            continue;
        }
        if (expires_at - now) > ticket::RENEWAL_WINDOW_S {
            continue;
        }
        info!(
            "Renewing relay ticket for {}",
            &peer_id[..12.min(peer_id.len())]
        );
        state.issue_relay_ticket(&peer_id);
    }
}

/// Seed default web assets into `data_dir` on first run.
///
/// Files are embedded at compile time with `include_str!` so the binary
/// is self-contained.  Each file is only written if it does not already
/// exist, so operator customisations are never overwritten.
///
/// Directory layout seeded:
/// ```text
/// <data_dir>/
///   web/
///     index.html                     ← portal dashboard (uses window.conquerd bridge)
///   games/
///     example/
///       index.html                   ← cursor-relay demo UI
///       game.js                      ← cursor-relay demo logic
///     shared-drawing/
///       index.html                   ← collaborative canvas demo
///       drawing.js
///     brick-breaker/
///       index.html                   ← multiplayer breakout demo
///       brick-breaker.js
///   web-sdk/
///     conquerd.mjs                   ← browser SDK (imported by all game demos)
/// ```
///
/// The three game demos (`game.relay.v1`) are fully functional browser
/// experiences that connect via WebTransport when `web.host.h3.v1` is enabled.
fn seed_web_defaults(data_dir: &std::path::Path) {
    // Embedded assets — all paths are relative to this source file and
    // live inside the crate so the build works regardless of whether the
    // wider project root (games/, web-sdk/) is present (e.g. on Linux CI).
    const PORTAL_HTML: &str = include_str!("../templates/web_index.html");

    // Cursor relay (original example)
    const CURSOR_HTML: &str = include_str!("../templates/games_example_index.html");
    const CURSOR_JS: &str = include_str!("../templates/games_example_game.js");

    // Shared drawing demo
    const DRAW_HTML: &str = include_str!("../templates/games_shared_drawing_index.html");
    const DRAW_JS: &str = include_str!("../templates/games_shared_drawing_drawing.js");

    // Brick breaker demo
    const BRICK_HTML: &str = include_str!("../templates/games_brick_breaker_index.html");
    const BRICK_JS: &str = include_str!("../templates/games_brick_breaker_brick_breaker.js");

    const CONQUERD_MJS: &str = include_str!("../templates/web_sdk_conquerd.mjs");

    // Directories (always ensure they exist)
    let dirs: &[&[&str]] = &[
        &["web"],
        &["web", "web-sdk"],
        &["games", "example"],
        &["games", "shared-drawing"],
        &["games", "brick-breaker"],
    ];

    // Always-overwrite: all system-owned files must stay current with the
    // binary.  Content is compared first so unchanged files are not touched.
    // Operators add their own games under a different slug — they never
    // customise these first-party files directly.
    let always_update: &[(&[&str], &str)] = &[
        (&["web", "index.html"], PORTAL_HTML),
        // Served at /web-sdk/conquerd.mjs — must live under web/ so that
        // the web_app_module route() function finds it via the web_root.
        (&["web", "web-sdk", "conquerd.mjs"], CONQUERD_MJS),
        (&["games", "example", "index.html"], CURSOR_HTML),
        (&["games", "example", "game.js"], CURSOR_JS),
        (&["games", "shared-drawing", "index.html"], DRAW_HTML),
        (&["games", "shared-drawing", "drawing.js"], DRAW_JS),
        (&["games", "brick-breaker", "index.html"], BRICK_HTML),
        (&["games", "brick-breaker", "brick-breaker.js"], BRICK_JS),
    ];

    // No write-if-missing seeds remain; all built-in files are above.

    for parts in dirs {
        let full = data_dir.join(parts.iter().collect::<std::path::PathBuf>());
        if !full.exists() {
            if let Err(e) = std::fs::create_dir_all(&full) {
                warn!("[seed] could not create {}: {e}", full.display());
            }
        }
    }

    for (parts, content) in always_update {
        let full = data_dir.join(parts.iter().collect::<std::path::PathBuf>());
        if let Some(parent) = full.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::read_to_string(&full) {
            Ok(existing) if existing == *content => {} // unchanged — skip the write
            _ => {
                if let Err(e) = std::fs::write(&full, content.as_bytes()) {
                    warn!("[seed] could not write {}: {e}", full.display());
                } else {
                    info!("[seed] updated {}", full.display());
                }
            }
        }
    }
}

/// Ensure a self-signed TLS cert exists for the WebTransport listener.
///
/// Returns the SHA-256 fingerprint (lowercase hex) of the cert's DER bytes.
/// The fingerprint is the trust anchor: it is delivered to native clients via
/// the already Ed25519-verified `SUPERNODE_INFO` message, so no CA is needed.
///
/// Validity is capped at 13 days to stay within Chromium's 14-day maximum
/// for `serverCertificateHashes`.  The cert is rotated when it is older than
/// 7 days (the fingerprint file's mtime is used as a cheap age check).
///
/// **Always** re-derives the fingerprint from the on-disk cert DER rather
/// than reading the stored `.hex` file, so the advertised hash is never stale
/// (e.g. after a crash between the cert write and the fingerprint write, or
/// after a manual cert replacement).
fn ensure_web_cert(data_dir: &std::path::Path) -> Option<String> {
    use sha2::{Digest, Sha256};

    let cert_path = data_dir.join("web_cert.pem");
    let key_path = data_dir.join("web_key.pem");
    let fp_path = data_dir.join("web_cert_fingerprint.hex");

    // Helper: derive the fingerprint and DER bytes directly from the PEM
    // on disk so both are always authoritative (not dependent on .hex cache).
    let derive_fp_and_der_from_disk = || -> Option<(String, Vec<u8>)> {
        let pem = std::fs::read_to_string(&cert_path).ok()?;
        // Strip the PEM header/footer and decode the base64 body.
        let b64: String = pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<Vec<_>>()
            .join("");
        use base64::Engine;
        let der = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .ok()?;
        let fp = hex::encode(Sha256::digest(&der));
        Some((fp, der))
    };

    // Reuse existing cert if it is still fresh (< 7 days old) AND it
    // carries the serverAuth EKU that Chrome WebTransport requires.
    // Certs generated before the EKU was added are silently rotated.
    let still_fresh = std::fs::metadata(&fp_path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| std::time::SystemTime::now().duration_since(mtime).ok())
        .map(|age| age.as_secs() < 7 * 24 * 3600)
        .unwrap_or(false);
    if still_fresh && cert_path.exists() && key_path.exists() {
        // Re-derive the fingerprint from the actual cert DER so the
        // advertised hash is always in sync with what wtransport presents.
        if let Some((fp, der)) = derive_fp_and_der_from_disk() {
            // Check for serverAuth EKU (OID 1.3.6.1.5.5.7.3.1).
            // DER encoding of this OID: 06 08 2b 06 01 05 05 07 03 01
            const SERVER_AUTH_OID: &[u8] =
                &[0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];
            let has_server_auth = der
                .windows(SERVER_AUTH_OID.len())
                .any(|w| w == SERVER_AUTH_OID);
            if has_server_auth {
                // Keep the .hex cache file up to date.
                let _ = std::fs::write(&fp_path, &fp);
                info!(
                    "[web-cert] reusing existing cert (fingerprint {}…)",
                    &fp[..16.min(fp.len())]
                );
                return Some(fp);
            }
            // Old cert missing serverAuth EKU — Chrome TLS rejects it.
            // Remove the files so we fall through to regeneration.
            warn!("[web-cert] existing cert missing serverAuth EKU — regenerating");
            let _ = std::fs::remove_file(&cert_path);
            let _ = std::fs::remove_file(&key_path);
            let _ = std::fs::remove_file(&fp_path);
        } else {
            // Cert PEM unreadable — fall through to regeneration.
            warn!("[web-cert] existing cert unreadable — regenerating");
        }
    }

    // Generate a fresh self-signed ECDSA cert valid for 13 days.
    use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};

    let now = time::OffsetDateTime::now_utc();
    let mut params = CertificateParams::default();
    params.not_before = now;
    params.not_after = now + time::Duration::days(13);
    // Chromium WebTransport with serverCertificateHashes bypasses hostname
    // verification, but it still requires the certificate to be valid for
    // TLS server authentication.  Add the serverAuth EKU explicitly so
    // Chrome's TLS stack accepts the cert at the handshake level.
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let key = match KeyPair::generate() {
        Ok(k) => k,
        Err(e) => {
            warn!("[web-cert] key generation failed: {e}");
            return None;
        }
    };
    let cert = match params.self_signed(&key) {
        Ok(c) => c,
        Err(e) => {
            warn!("[web-cert] cert generation failed: {e}");
            return None;
        }
    };

    let cert_pem = cert.pem();
    let key_pem = key.serialize_pem();
    let fingerprint = hex::encode(Sha256::digest(cert.der().as_ref()));

    if let Err(e) = std::fs::write(&cert_path, &cert_pem) {
        warn!("[web-cert] write cert failed: {e}");
        return None;
    }
    if let Err(e) = std::fs::write(&key_path, &key_pem) {
        warn!("[web-cert] write key failed: {e}");
        return None;
    }
    if let Err(e) = std::fs::write(&fp_path, &fingerprint) {
        // Non-fatal: the cert and key were written; we have the fingerprint
        // in memory. The .hex cache is only used to check freshness on the
        // next restart. Without it the next startup regenerates the cert
        // one restart early — acceptable, not a reason to drop the fingerprint.
        warn!("[web-cert] write fingerprint cache failed (non-fatal): {e}");
    }

    info!(
        "[web-cert] generated new self-signed cert (fingerprint {}…)",
        &fingerprint[..16.min(fingerprint.len())]
    );
    Some(fingerprint)
}

#[cfg(test)]
mod identity_normalization_tests {
    use super::*;
    use base64::Engine;

    /// The relay derives `from_peer` as un-padded base64url of the 32-byte
    /// public key (`extract_peer_id`), while the SFU/signaling layer uses the
    /// padded `public_id` (`URL_SAFE.encode`). `pad_base64url` must bridge the
    /// two exactly, or the room-audio fan-out fails to exclude the sender and
    /// the talker hears their own voice echoed back through the codec.
    #[test]
    fn pad_base64url_matches_padded_public_id_for_all_keys() {
        let no_pad = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let padded = base64::engine::general_purpose::URL_SAFE;
        // Sweep several distinct 32-byte keys; Ed25519 keys are always 32 bytes
        // → 43 unpadded chars → 44 padded (one trailing '=').
        for seed in [0u8, 1, 7, 42, 255] {
            let key = [seed; 32];
            let relay_id = no_pad.encode(key); // what the relay sees
            let sfu_id = padded.encode(key); // what the SFU stores
            assert_ne!(relay_id, sfu_id, "test premise: forms differ");
            assert_eq!(
                pad_base64url(&relay_id),
                sfu_id,
                "re-padded relay id must equal the SFU's padded public_id"
            );
        }
    }

    #[test]
    fn pad_base64url_is_idempotent_on_already_padded() {
        // Feeding an already-padded id (or one whose length is aligned) back
        // through must not append spurious '='.
        assert_eq!(pad_base64url("YWJj"), "YWJj"); // len 4, aligned
        assert_eq!(pad_base64url("YWJjZA=="), "YWJjZA=="); // already padded
        assert_eq!(pad_base64url("YWJjZGU="), "YWJjZGU="); // already padded
    }
}

#[cfg(test)]
mod cluster_audio_replication_tests {
    use super::*;

    #[test]
    fn audio_replication_id_prefers_signature() {
        let mut msg = SignalingMessage::new(
            MessageType::SfuAudio,
            "senderPub=",
            serde_json::json!({"room_id": "default", "seq": 7}),
        );
        msg.signature = Some("sigABC".into());
        assert_eq!(audio_replication_id(&msg), "a:sigABC");
    }

    #[test]
    fn audio_replication_id_falls_back_to_room_sender_seq() {
        let msg = SignalingMessage::new(
            MessageType::SfuAudio,
            "senderPub=",
            serde_json::json!({"room_id": "r1", "seq": 42}),
        );
        assert_eq!(audio_replication_id(&msg), "a:r1:senderPub=:42");
    }

    #[test]
    fn replicated_frame_type_detection_audio_vs_chat() {
        let audio = r#"{"type":"sfu_audio","sender":"x","payload":{}}"#;
        let chat = r#"{"type":"sfu_chat","sender":"x","payload":{}}"#;
        let is_audio = |raw: &str| {
            serde_json::from_str::<serde_json::Value>(raw)
                .ok()
                .and_then(|v| {
                    v.get("type")
                        .and_then(|t| t.as_str())
                        .map(|s| s == "sfu_audio")
                })
                .unwrap_or(false)
        };
        assert!(is_audio(audio));
        assert!(!is_audio(chat));
        assert!(!is_audio("not-json"));
    }
}

#[cfg(test)]
mod build_feature_registry_tests {
    use super::*;

    fn cfg(data_dir: std::path::PathBuf) -> Config {
        Config {
            signaling_port: 0,
            relay_port: 0,
            web_port: None,
            chat_enabled: true,
            files_enabled: true,
            sfu_enabled: true,
            updates_enabled: false,
            auto_restart: false,
            invite_ttl_seconds: -1,
            web_title: String::new(),
            access_mode: crate::config::AccessMode::Open,
            access_code: String::new(),
            ad_duration: 0,
            tos_text: String::new(),
            ad_content: String::new(),
            demo_links: false,
            external_host: None,
            data_dir,
            web_localhost_only: false,
        }
    }

    fn tempdir() -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "conquerd-build-registry-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn falls_back_to_legacy_config_when_no_manifest() {
        let dir = tempdir();
        let config = cfg(dir);
        let manifest = load_manifest(&config);
        let registry = build_feature_registry(&manifest, &config);
        let ids: Vec<String> = registry.snapshot().iter().map(|c| c.id.clone()).collect();
        // Legacy config has all toggles on => chat+files+sfu+portal present.
        assert!(ids.iter().any(|i| i == "core.chat.v1"));
        assert!(ids.iter().any(|i| i == "core.file.v1"));
        assert!(ids.iter().any(|i| i == "room.audio.sfu"));
        assert!(ids.iter().any(|i| i == "web.host.app.v1"));
        assert!(ids.iter().any(|i| i == "web.host.h3.v1"));
    }

    #[test]
    fn manifest_file_overrides_legacy_toggles() {
        let dir = tempdir();
        std::fs::write(
            dir.join("supernode.toml"),
            "schema_version = 1\n\
             [[feature]]\n\
             id = \"core.chat.v1\"\n",
        )
        .unwrap();
        let config = cfg(dir);
        let manifest = load_manifest(&config);
        let registry = build_feature_registry(&manifest, &config);
        let mut ids: Vec<String> = registry.snapshot().iter().map(|c| c.id.clone()).collect();
        ids.sort();
        // Manifest declares chat only; relay + room quota descriptors are always upserted.
        assert_eq!(
            ids,
            vec![
                "core.audio.opus".to_string(),
                "core.chat.v1".to_string(),
                "core.file.v1".to_string(),
                "game.relay.v1".to_string(),
                "room.audio.sfu".to_string(),
                "room.chat.v1".to_string(),
                "room.file.v1".to_string(),
            ]
        );
    }
}

#[cfg(test)]
mod space_admission_tests {
    use super::*;
    use crate::crypto::{b64url_encode, ed25519_sign};
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    /// Owner + a Space with one public and one private room, plus a fresh signed
    /// root. Returns (owner_pub, sign_closure-ready key, Space, root).
    fn fixture() -> (String, SigningKey, space::Space, space::SignedSpaceRoot) {
        let key = SigningKey::generate(&mut OsRng);
        let owner = b64url_encode(key.verifying_key().as_bytes());
        let mut sp = space::Space::new_server(&owner, "srv");
        for (name, ntype) in [("Public", "public"), ("Secret", "private")] {
            let id = space::derive_node_id(&sp.space_id, &owner, name);
            sp.upsert_node(space::SpaceNode {
                node_id: id,
                parent_id: sp.space_id.clone(),
                kind: "room".to_owned(),
                name: name.to_owned(),
                node_type: ntype.to_owned(),
                owner_pub: owner.clone(),
                invite_policy: String::new(),
                inherit: false,
                key_commit: String::new(),
            });
        }
        let root = sp.signed_root(1000, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        (owner, key, sp, root)
    }

    fn room_id(sp: &space::Space, name: &str) -> String {
        sp.nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap()
            .node_id
            .clone()
    }

    #[test]
    fn space_root_store_keeps_highest_epoch_and_rejects_regression() {
        let (_owner, key, mut sp, root0) = fixture();
        let mut store = SpaceRootStore::default();
        assert!(store.accept(root0.clone()), "first root accepted");
        assert!(!store.accept(root0.clone()), "same epoch not re-accepted");

        // Newer epoch accepted; then the older one is refused.
        sp.upsert_node(space::SpaceNode {
            node_id: space::derive_node_id(&sp.space_id, &sp.owner_pub, "New"),
            parent_id: sp.space_id.clone(),
            kind: "room".to_owned(),
            name: "New".to_owned(),
            node_type: "public".to_owned(),
            owner_pub: sp.owner_pub.clone(),
            invite_policy: String::new(),
            inherit: false,
            key_commit: String::new(),
        });
        let root1 = sp.signed_root(1001, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        assert!(root1.epoch > root0.epoch);
        assert!(store.accept(root1.clone()));
        assert!(!store.accept(root0), "older epoch refused after newer seen");
        assert_eq!(store.get(&sp.space_id).unwrap().epoch, root1.epoch);
    }

    #[test]
    fn space_root_store_refuses_unsigned_and_cross_signer() {
        let (_o, _k, _sp, mut root) = fixture();
        let mut store = SpaceRootStore::default();
        root.signature = "AAAA".to_owned(); // broken sig
        assert!(!store.accept(root), "unsigned/invalid root refused");

        // A different signer cannot displace an accepted space_id.
        let (_o2, key2, _sp2, mut root2) = fixture();
        let (_o3, _k3, sp3, good) = fixture();
        let mut store2 = SpaceRootStore::default();
        assert!(store2.accept(good.clone()));
        // Forge a higher-epoch root for the SAME space_id but a different signer.
        root2.space_id = good.space_id.clone();
        root2.epoch = good.epoch + 5;
        let attacker = b64url_encode(key2.verifying_key().as_bytes());
        root2.signer = attacker;
        // (signature won't verify for the tampered fields anyway)
        assert!(!store2.accept(root2), "cross-signer takeover refused");
        assert_eq!(store2.get(&sp3.space_id).unwrap().epoch, good.epoch);
    }

    #[test]
    fn space_root_store_flags_same_epoch_content_conflict_as_equivocation() {
        // Same owner key, same space_id, same epoch — but two different node
        // sets produce two different `root_hash`es. A malicious (or buggy)
        // owner signing both is exactly the equivocation SPACE-MERKLE-DESIGN §9
        // says a set tree cannot prevent structurally; we can only detect and
        // flag it (lighter mitigation), which is what this test verifies.
        let (_owner, key, sp, root_a) = fixture();
        let mut sp_fork = sp.clone();
        sp_fork.upsert_node(space::SpaceNode {
            node_id: space::derive_node_id(&sp_fork.space_id, &sp_fork.owner_pub, "Fork"),
            parent_id: sp_fork.space_id.clone(),
            kind: "room".to_owned(),
            name: "Fork".to_owned(),
            node_type: "public".to_owned(),
            owner_pub: sp_fork.owner_pub.clone(),
            invite_policy: String::new(),
            inherit: false,
            key_commit: String::new(),
        });
        // Same epoch (forced, simulating an attacker/bug re-using an epoch
        // number) as `root_a`, different node set → different root_hash.
        sp_fork.epoch = sp.epoch;
        let root_b = sp_fork.signed_root(1000, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        assert_eq!(root_a.epoch, root_b.epoch);
        assert_ne!(root_a.root_hash, root_b.root_hash);

        let mut store = SpaceRootStore::default();
        assert!(store.accept(root_a.clone()), "first root accepted");
        assert_eq!(store.equivocation_count(), 0);
        assert!(
            !store.accept(root_b),
            "conflicting same-epoch root rejected"
        );
        assert_eq!(
            store.equivocation_count(),
            1,
            "conflicting same-epoch root flagged as an equivocation"
        );
        // The first-seen root is retained (unchanged acceptance policy).
        assert_eq!(store.get(&sp.space_id).unwrap().root_hash, root_a.root_hash);
    }

    #[test]
    fn public_room_admits_by_proof_only() {
        let (_owner, _key, sp, root) = fixture();
        let rid = room_id(&sp, "Public");
        let proof = sp.prove(&rid).unwrap();
        assert!(space_admission_ok(&root, &proof, None, "any-peer", &rid, 0));
    }

    #[test]
    fn private_room_requires_valid_grant_for_this_peer() {
        let (owner, key, sp, root) = fixture();
        let rid = room_id(&sp, "Secret");
        let proof = sp.prove(&rid).unwrap();
        let peer = "peer-b-pub";

        // No grant → refused.
        assert!(!space_admission_ok(&root, &proof, None, peer, &rid, 0));

        // Valid grant for this peer → admitted.
        let grant = sp.grant(&rid, peer, 0, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        assert!(space_admission_ok(
            &root,
            &proof,
            Some(&grant),
            peer,
            &rid,
            0
        ));

        // Grant for a *different* peer → refused (replay by a third party).
        assert!(!space_admission_ok(
            &root,
            &proof,
            Some(&grant),
            "someone-else",
            &rid,
            0
        ));

        // Grant signed by a non-owner → refused.
        let attacker = SigningKey::generate(&mut OsRng);
        let forged = sp.grant(&rid, peer, 0, |b| {
            ed25519_sign(&attacker.to_bytes(), b).unwrap()
        });
        assert!(!space_admission_ok(
            &root,
            &proof,
            Some(&forged),
            peer,
            &rid,
            0
        ));

        // Expired grant → refused.
        let expiring = sp.grant(&rid, peer, 500, |b| {
            ed25519_sign(&key.to_bytes(), b).unwrap()
        });
        assert!(space_admission_ok(
            &root,
            &proof,
            Some(&expiring),
            peer,
            &rid,
            499
        ));
        assert!(!space_admission_ok(
            &root,
            &proof,
            Some(&expiring),
            peer,
            &rid,
            501
        ));

        // Grant for a different node id → refused.
        let other = sp.grant("other-room", peer, 0, |b| {
            ed25519_sign(&key.to_bytes(), b).unwrap()
        });
        assert!(!space_admission_ok(
            &root,
            &proof,
            Some(&other),
            peer,
            &rid,
            0
        ));
        let _ = owner;
    }

    #[test]
    fn proof_for_wrong_room_or_stale_root_is_refused() {
        let (_owner, key, mut sp, root0) = fixture();
        let rid = room_id(&sp, "Public");
        let proof0 = sp.prove(&rid).unwrap();

        // Proof node id must equal the room being joined.
        assert!(!space_admission_ok(
            &root0,
            &proof0,
            None,
            "peer",
            "different-room",
            0
        ));

        // After the Space changes (new epoch), the OLD proof no longer verifies
        // against the NEW root → current-epoch-only admission.
        sp.upsert_node(space::SpaceNode {
            node_id: space::derive_node_id(&sp.space_id, &sp.owner_pub, "Extra"),
            parent_id: sp.space_id.clone(),
            kind: "room".to_owned(),
            name: "Extra".to_owned(),
            node_type: "public".to_owned(),
            owner_pub: sp.owner_pub.clone(),
            invite_policy: String::new(),
            inherit: false,
            key_commit: String::new(),
        });
        let root1 = sp.signed_root(1002, |b| ed25519_sign(&key.to_bytes(), b).unwrap());
        assert!(!space_admission_ok(&root1, &proof0, None, "peer", &rid, 0));
        // A fresh proof against the new root admits again.
        let proof1 = sp.prove(&rid).unwrap();
        assert!(space_admission_ok(&root1, &proof1, None, "peer", &rid, 0));
    }

    #[test]
    fn tampered_proof_node_is_refused() {
        let (_owner, _key, sp, root) = fixture();
        let rid = room_id(&sp, "Public");
        let mut proof = sp.prove(&rid).unwrap();
        proof.node.name = "Renamed".to_owned(); // leaf no longer matches the root
        assert!(!space_admission_ok(&root, &proof, None, "peer", &rid, 0));
    }
}
