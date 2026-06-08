//! Browser-transport bridge — the seam between WebTransport (and any
//! future browser-reachable transport) and the supernode's channel-tag
//! fabric.
//!
//! ## Scope
//!
//! This module is intentionally **transport-agnostic**: it does not pull
//! in `wtransport` / `h3` / `axum-ws` directly. Instead it exposes a
//! [`BrowserBridge`] that any concrete listener can feed `(peer_id, tag,
//! payload)` events into. The bridge enforces:
//!
//! * **Capability gating** — the browser peer must have advertised the
//!   feature for the inbound tag, mirroring the desktop-side gate in
//!   [`client_desktop::connection_manager::ConnectionManager::peer_supports`].
//! * **Channel-tag whitelist** — only payloads whose tag falls in the
//!   dynamic range `0x10..=0xEF` are routed; anything else is dropped
//!   (mirroring the Rust `conquerd_features::channel_tag` rules).
//! * **Per-feature send/recv counters** — observable by `BridgeStats`
//!   for telemetry. Quotas remain owned by the per-session
//!   [`crate::ChannelTagBinder`] equivalent on the consumer side; the
//!   bridge counts, routes, and enforces per-feature quotas when a
//!   [`FeatureRegistry`] is installed via [`BrowserBridge::set_features`].
//!
//! ## Lifecycle
//!
//! 1. The h3/WebTransport listener completes a session for `peer_id`.
//! 2. It calls [`BrowserBridge::register_session`] with the peer's
//!    advertised capability id list.
//! 3. For every browser->supernode datagram, the listener calls
//!    [`BrowserBridge::on_inbound`]. Drops are counted in `BridgeStats`.
//! 4. For every supernode->browser datagram, the consumer calls
//!    [`BrowserBridge::send`] which invokes the listener's send hook.
//! 5. On disconnect, [`BrowserBridge::release_session`] frees per-peer
//!    state.

#![allow(dead_code)] // Several methods exercised only by tests until the h3 listener bite lands.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;

use conquerd_features::channel_tag::{DYNAMIC_TAG_END, DYNAMIC_TAG_START};
use conquerd_features::FeatureRegistry;

/// 1-byte channel tag at the head of every feature datagram.
pub type FeatureTag = u8;

fn is_dynamic_feature_tag(tag: FeatureTag) -> bool {
    (DYNAMIC_TAG_START..=DYNAMIC_TAG_END).contains(&tag)
}

/// Reverse-DNS feature id (e.g. `"core.chat.v1"`).
pub type FeatureId = String;

/// Stable browser peer identity (typically the Ed25519 pub-key
/// identifier, hex- or base64url-encoded, matching the desktop client).
pub type BrowserPeerId = String;

/// Send hook invoked by the bridge to push a datagram to a browser.
/// Returns `false` if the underlying transport refused the send.
pub type SendHook = Arc<dyn Fn(&BrowserPeerId, FeatureTag, &[u8]) -> bool + Send + Sync>;

/// Per-feature dispatch trait. Implementations decide how a payload
/// resolved to a specific feature id should be acted on (fan-out to
/// other browser peers, hand off to the SFU, deliver to a `FeatureModule`,
/// etc.). The dispatcher is invoked **after** the bridge has confirmed:
///
/// * the tag is in the dynamic range,
/// * the source peer has an active session,
/// * the feature was advertised by that peer.
///
/// `bridge` is supplied so the dispatcher can call back into
/// [`BrowserBridge::send`] for fan-out without taking another reference.
pub trait FeatureDispatcher: Send + Sync {
    fn on_inbound(
        &self,
        bridge: &BrowserBridge,
        source_peer: &str,
        feature_id: &str,
        payload: &[u8],
    );
}

/// Snapshot counters for one (peer, feature) pair.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PairStats {
    pub inbound_ok: u64,
    pub inbound_dropped_no_capability: u64,
    pub inbound_dropped_unbound_tag: u64,
    pub inbound_dropped_unverified: u64,
    pub outbound_ok: u64,
    pub outbound_failed: u64,
}

#[derive(Debug, Default, Clone)]
pub struct BridgeStats {
    pub per_pair: HashMap<(BrowserPeerId, FeatureId), PairStats>,
    pub sessions_open: u64,
    pub sessions_closed: u64,
}

impl BridgeStats {
    pub fn pair(&self, peer: &str, feature: &str) -> PairStats {
        self.per_pair
            .get(&(peer.to_string(), feature.to_string()))
            .copied()
            .unwrap_or_default()
    }
}

/// One browser session's per-peer state.
#[derive(Debug, Default)]
struct Session {
    /// Feature ids the browser peer advertised in `CAPABILITY_ANNOUNCE`.
    /// Inbound traffic for ids not in this set is dropped at the bridge.
    advertised: HashSet<FeatureId>,
    /// Tag -> feature_id mapping for inbound dispatch. The browser's
    /// listener owns tag allocation (mirroring the desktop binder); the
    /// bridge just consults the table.
    tag_to_feature: HashMap<FeatureTag, FeatureId>,
    /// Reverse mapping for outbound `send()`.
    feature_to_tag: HashMap<FeatureId, FeatureTag>,
    /// SFU room id this browser peer is participating in, if any.
    /// Declared via the WT request path (`?room=<id>`) for now; a
    /// follow-up bite replaces that with a control-stream JOIN_ROOM.
    room_id: Option<String>,
    /// Verified Ed25519 public key (base64url) once the browser has
    /// completed the identity handshake. `None` until verification
    /// succeeds; gated features (`room.*`) refuse to dispatch when
    /// this is `None`.
    verified_identity: Option<String>,
}

/// The bridge itself. Cheaply cloneable (`Arc`-wrapped state).
#[derive(Clone, Default)]
pub struct BrowserBridge {
    inner: Arc<RwLock<BridgeInner>>,
}

#[derive(Default)]
struct BridgeInner {
    sessions: HashMap<BrowserPeerId, Session>,
    send: Option<SendHook>,
    dispatcher: Option<Arc<dyn FeatureDispatcher>>,
    features: Option<Arc<FeatureRegistry>>,
    stats: BridgeStats,
}

impl BrowserBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the listener's send hook. Replacing an existing hook is
    /// allowed (e.g. listener restart) but logs a warning at the call
    /// site, not here.
    pub fn set_send_hook(&self, hook: SendHook) {
        self.inner.write().send = Some(hook);
    }

    /// Install a feature dispatcher. Calls to [`on_inbound_dispatch`]
    /// (and `on_inbound` when no closure handler is supplied) consult
    /// this dispatcher to decide what to do with the payload.
    pub fn set_dispatcher(&self, dispatcher: Arc<dyn FeatureDispatcher>) {
        self.inner.write().dispatcher = Some(dispatcher);
    }

    /// Install the supernode feature registry used for symmetric inbound/
    /// outbound quota gating on browser fan-out and quota cleanup on
    /// [`release_session`].
    pub fn set_features(&self, features: Arc<FeatureRegistry>) {
        self.inner.write().features = Some(features);
    }

    /// True when a dispatcher has already been installed.
    pub fn has_dispatcher(&self) -> bool {
        self.inner.read().dispatcher.is_some()
    }

    /// Register a freshly-completed browser session.
    ///
    /// `advertised` is the list of feature ids the browser peer's
    /// `CAPABILITY_ANNOUNCE` declared. `tag_bindings` is the
    /// listener-allocated `(tag, feature_id)` table for this session.
    pub fn register_session<I, T>(&self, peer_id: BrowserPeerId, advertised: I, tag_bindings: T)
    where
        I: IntoIterator<Item = FeatureId>,
        T: IntoIterator<Item = (FeatureTag, FeatureId)>,
    {
        self.register_session_with_room(peer_id, advertised, tag_bindings, None);
    }

    /// Same as [`register_session`] but also records an SFU room id for
    /// this browser peer. `room.*` dispatchers use this to scope
    /// fan-out to peers in the same room.
    pub fn register_session_with_room<I, T>(
        &self,
        peer_id: BrowserPeerId,
        advertised: I,
        tag_bindings: T,
        room_id: Option<String>,
    ) where
        I: IntoIterator<Item = FeatureId>,
        T: IntoIterator<Item = (FeatureTag, FeatureId)>,
    {
        let mut inner = self.inner.write();
        let session = inner.sessions.entry(peer_id.clone()).or_default();
        session.advertised = advertised.into_iter().collect();
        for (tag, feature) in tag_bindings {
            session.tag_to_feature.insert(tag, feature.clone());
            session.feature_to_tag.insert(feature, tag);
        }
        session.room_id = room_id;
        // Re-registering must drop any prior verified identity so a
        // future handshake re-attests possession of the key.
        session.verified_identity = None;
        inner.stats.sessions_open += 1;
    }

    /// Look up the SFU room id for a browser peer, if it declared one.
    pub fn session_room(&self, peer_id: &str) -> Option<String> {
        self.inner
            .read()
            .sessions
            .get(peer_id)
            .and_then(|s| s.room_id.clone())
    }

    /// Look up the verified Ed25519 identity (base64url pubkey) for a
    /// browser peer. Returns `None` until the handshake completes.
    pub fn session_identity(&self, peer_id: &str) -> Option<String> {
        self.inner
            .read()
            .sessions
            .get(peer_id)
            .and_then(|s| s.verified_identity.clone())
    }

    /// Mark *peer_id* as having a verified Ed25519 identity. Called by
    /// the listener after the challenge/response handshake succeeds.
    /// No-op if the peer has no session.
    pub fn mark_verified(&self, peer_id: &str, pubkey_b64: String) {
        let mut inner = self.inner.write();
        if let Some(session) = inner.sessions.get_mut(peer_id) {
            session.verified_identity = Some(pubkey_b64);
        }
    }

    /// True when *peer_id* has a verified identity. Cheaper than
    /// `session_identity().is_some()` for hot paths.
    pub fn is_verified(&self, peer_id: &str) -> bool {
        self.inner
            .read()
            .sessions
            .get(peer_id)
            .is_some_and(|s| s.verified_identity.is_some())
    }

    /// Drop all state for *peer_id* and clear quota buckets when a registry
    /// is installed.
    pub fn release_session(&self, peer_id: &str) {
        let features = {
            let mut inner = self.inner.write();
            let removed = inner.sessions.remove(peer_id).is_some();
            if removed {
                inner.stats.sessions_closed += 1;
            }
            inner.features.clone()
        };
        if let Some(features) = features {
            features.clear_peer_quotas(peer_id);
            features.clear_peer_outbound_quotas(peer_id);
        }
    }

    /// Route an inbound browser->supernode datagram.
    ///
    /// Returns the resolved feature id when the datagram passes all
    /// gates and was handed off to *handler*. Returns `None` (and bumps
    /// the appropriate drop counter) for any of:
    ///
    /// * tag outside the dynamic range
    /// * unknown peer
    /// * tag not bound to a feature for this peer
    /// * feature not in the peer's announced capability set
    pub fn on_inbound<F>(
        &self,
        peer_id: &str,
        tag: FeatureTag,
        payload: &[u8],
        handler: F,
    ) -> Option<FeatureId>
    where
        F: FnOnce(&FeatureId, &[u8]),
    {
        if !is_dynamic_feature_tag(tag) {
            // Reserved tags (0x00-0x0F, 0xF0-0xFE, 0xFF) are not
            // routable as feature traffic. Don't bump the per-pair
            // counter because we have no feature to attribute it to.
            return None;
        }
        let mut inner = self.inner.write();
        let session = inner.sessions.get(peer_id)?;
        let Some(feature) = session.tag_to_feature.get(&tag).cloned() else {
            // Tag in the dynamic range but unknown for this peer.
            // We can't attribute it to a feature so just return None.
            return None;
        };
        let allowed = session.advertised.contains(&feature);
        let verified = session.verified_identity.is_some();
        // Verification gate: room.* and game.* features require a verified
        // identity so the supernode never fans out anonymous traffic
        // into authenticated rooms or game sessions.
        let needs_verification = feature.starts_with("room.") || feature.starts_with("game.");
        let key = (peer_id.to_string(), feature.clone());
        let entry = inner.stats.per_pair.entry(key).or_default();
        if !allowed {
            entry.inbound_dropped_no_capability += 1;
            return None;
        }
        if needs_verification && !verified {
            entry.inbound_dropped_unverified += 1;
            return None;
        }
        let features = inner.features.clone();
        drop(inner);
        if let Some(ref features) = features {
            if !features.gate_inbound_through_feature(&feature, peer_id, payload.len()) {
                tracing::debug!(
                    "[webtransport] inbound quota exceeded for {} on {}; dropping datagram",
                    &peer_id[..12.min(peer_id.len())],
                    feature
                );
                return None;
            }
        }
        {
            let mut inner = self.inner.write();
            inner
                .stats
                .per_pair
                .entry((peer_id.to_string(), feature.clone()))
                .or_default()
                .inbound_ok += 1;
        }
        handler(&feature, payload);
        Some(feature)
    }

    /// Same as [`on_inbound`] but routes the resolved payload through
    /// the registered [`FeatureDispatcher`]. Returns the resolved
    /// feature id (same drop semantics as `on_inbound`).
    pub fn on_inbound_dispatch(
        &self,
        peer_id: &str,
        tag: FeatureTag,
        payload: &[u8],
    ) -> Option<FeatureId> {
        let dispatcher = self.inner.read().dispatcher.clone();
        let bridge_ref = self.clone();
        self.on_inbound(peer_id, tag, payload, |fid, p| {
            if let Some(d) = dispatcher {
                d.on_inbound(&bridge_ref, peer_id, fid, p);
            }
        })
    }

    /// Push a supernode->browser datagram for *feature_id* on *peer_id*.
    /// Returns `false` if the feature isn't bound to a tag for that peer
    /// or the send hook refuses the send.
    pub fn send(&self, peer_id: &str, feature_id: &str, payload: &[u8]) -> bool {
        let inner = self.inner.read();
        let Some(session) = inner.sessions.get(peer_id) else {
            return false;
        };
        let Some(&tag) = session.feature_to_tag.get(feature_id) else {
            return false;
        };
        let send = match inner.send.clone() {
            Some(s) => s,
            None => return false,
        };
        let features = inner.features.clone();
        // Drop the read lock before invoking the hook so the hook may
        // touch the bridge.
        drop(inner);
        if let Some(ref features) = features {
            if !features.gate_through_feature(feature_id, peer_id, payload.len()) {
                let mut inner = self.inner.write();
                let key = (peer_id.to_string(), feature_id.to_string());
                inner.stats.per_pair.entry(key).or_default().outbound_failed += 1;
                return false;
            }
        }
        let ok = send(&peer_id.to_string(), tag, payload);
        let mut inner = self.inner.write();
        let key = (peer_id.to_string(), feature_id.to_string());
        let entry = inner.stats.per_pair.entry(key).or_default();
        if ok {
            entry.outbound_ok += 1;
        } else {
            entry.outbound_failed += 1;
        }
        ok
    }

    /// Snapshot the current counters. Cheap to call from telemetry tasks.
    pub fn stats(&self) -> BridgeStats {
        self.inner.read().stats.clone()
    }

    /// Return the feature ids advertised for *peer_id*, or empty if the
    /// peer has no session.
    pub fn advertised(&self, peer_id: &str) -> HashSet<FeatureId> {
        self.inner
            .read()
            .sessions
            .get(peer_id)
            .map(|s| s.advertised.clone())
            .unwrap_or_default()
    }

    /// Snapshot of every peer id that has currently advertised
    /// *feature_id*. Used by fan-out dispatchers (browser room audio,
    /// browser room chat) to find delivery targets.
    pub fn peers_advertising(&self, feature_id: &str) -> Vec<BrowserPeerId> {
        self.inner
            .read()
            .sessions
            .iter()
            .filter(|(_, s)| s.advertised.contains(feature_id))
            .map(|(pid, _)| pid.clone())
            .collect()
    }
}

/// Default dispatcher: fans out `room.*` and `game.*` datagrams to every
/// other browser session that has advertised the same feature **and** is
/// scoped to the same SFU room / game session id (or, for backward compat,
/// when both peers have no room declared).
pub struct BrowserRoomDispatcher;

impl FeatureDispatcher for BrowserRoomDispatcher {
    fn on_inbound(
        &self,
        bridge: &BrowserBridge,
        source_peer: &str,
        feature_id: &str,
        payload: &[u8],
    ) {
        if !feature_id.starts_with("room.") && !feature_id.starts_with("game.") {
            return;
        }
        let source_room = bridge.session_room(source_peer);
        for target in bridge.peers_advertising(feature_id) {
            if target == source_peer {
                continue;
            }
            if bridge.session_room(&target) != source_room {
                continue;
            }
            let _ = bridge.send(&target, feature_id, payload);
        }
    }
}

/// Single-call hook invoked when an inbound `room.*` payload from a
/// browser session needs native-side delivery. Unlike the older
/// per-member fan-out closure, the hook fires **once** per inbound
/// datagram and the consumer (typically a [`conquerd_features::FeatureModule`])
/// is responsible for any enumeration, verification, and forwarding.
///
/// Arguments: `(source_peer, feature_id, payload)`. The dispatcher
/// only invokes the hook after confirming the source has a declared
/// room (`bridge.session_room(source).is_some()`); the module need
/// not re-check.
pub type NativeMessageHook = Arc<dyn Fn(&str, &str, &[u8]) + Send + Sync>;
//             ^source ^feature_id ^payload

/// Composite dispatcher: browser fan-out (via
/// [`BrowserRoomDispatcher`]) plus a single native-side hook for
/// `room.*` features. Native delivery is skipped when the source peer
/// has no declared room.
pub struct ModuleNativeDispatcher {
    browser: BrowserRoomDispatcher,
    native_hook: NativeMessageHook,
}

impl ModuleNativeDispatcher {
    pub fn new(native_hook: NativeMessageHook) -> Self {
        Self {
            browser: BrowserRoomDispatcher,
            native_hook,
        }
    }
}

impl FeatureDispatcher for ModuleNativeDispatcher {
    fn on_inbound(
        &self,
        bridge: &BrowserBridge,
        source_peer: &str,
        feature_id: &str,
        payload: &[u8],
    ) {
        // Browser-side fan-out (room-scoped and game-session-scoped).
        self.browser
            .on_inbound(bridge, source_peer, feature_id, payload);

        if !feature_id.starts_with("room.") && !feature_id.starts_with("game.") {
            return;
        }
        if bridge.session_room(source_peer).is_none() {
            return;
        }
        (self.native_hook)(source_peer, feature_id, payload);
    }
}

/// Per-listener concurrent map of accepted browser sessions. Cloning a
/// [`wtransport::Connection`] is cheap (it is `Arc`-internally) so the
/// send hook can grab a handle without holding the map's lock across
/// the await.
type ConnectionMap = dashmap::DashMap<BrowserPeerId, wtransport::Connection>;

/// Spawn a WebTransport (HTTP/3) listener on *port* using the same TLS
/// material the HTTPS portal serves from (`web_cert.pem` / `web_key.pem`
/// under *data_dir*). Each accepted session is registered with *bridge*
/// and its inbound datagrams are routed through the channel-tag gates.
///
/// This function only returns when the endpoint fails to bind. On bind
/// failure it logs and exits; the caller's `tokio::spawn` task will end
/// without taking the supernode down.
pub async fn run_listener(bridge: BrowserBridge, data_dir: PathBuf, port: u16) {
    let cert_path = data_dir.join("web_cert.pem");
    let key_path = data_dir.join("web_key.pem");

    let identity = match wtransport::Identity::load_pemfiles(&cert_path, &key_path).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(
                "[web.host.h3.v1] failed to load TLS material from {:?} / {:?}: {} — listener disabled",
                cert_path,
                key_path,
                e
            );
            return;
        }
    };

    let server_config = wtransport::ServerConfig::builder()
        .with_bind_default(port)
        .with_identity(identity)
        .build();

    let endpoint = match wtransport::Endpoint::server(server_config) {
        Ok(ep) => ep,
        Err(e) => {
            tracing::warn!(
                "[web.host.h3.v1] failed to bind WebTransport endpoint on port {}: {}",
                port,
                e
            );
            return;
        }
    };

    let connections: Arc<ConnectionMap> = Arc::new(ConnectionMap::new());

    // Install the send hook once. It looks up the connection by peer id
    // and fires off a fire-and-forget send_datagram. WebTransport
    // datagrams are unreliable so we don't await delivery.
    let send_conns = connections.clone();
    let hook: SendHook = Arc::new(move |peer_id, tag, payload| {
        let Some(conn) = send_conns.get(peer_id).map(|r| r.clone()) else {
            return false;
        };
        let mut framed = Vec::with_capacity(payload.len() + 1);
        framed.push(tag);
        framed.extend_from_slice(payload);
        match conn.send_datagram(framed) {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(
                    "[web.host.h3.v1] send_datagram to {} failed: {}",
                    peer_id,
                    e
                );
                false
            }
        }
    });
    bridge.set_send_hook(hook);
    // Install the default browser-only fan-out unless the caller already
    // installed a richer dispatcher (e.g. ModuleNativeDispatcher) before
    // spawning the listener.
    if !bridge.has_dispatcher() {
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
    }

    tracing::info!(
        "[web.host.h3.v1] WebTransport listener bound on UDP/{} (cert {:?})",
        port,
        cert_path
    );

    loop {
        let incoming = endpoint.accept().await;
        let bridge = bridge.clone();
        let connections = connections.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_session(incoming, bridge, connections).await {
                tracing::debug!("[web.host.h3.v1] session ended: {}", e);
            }
        });
    }
}

/// Drive one accepted WebTransport session: parse the request path for
/// peer id and advertised capabilities, register the session with the
/// bridge, then loop on inbound datagrams.
async fn handle_session(
    incoming: wtransport::endpoint::IncomingSession,
    bridge: BrowserBridge,
    connections: Arc<ConnectionMap>,
) -> anyhow::Result<()> {
    let session_request = incoming.await?;
    let path = session_request.path().to_string();
    let (peer_id, advertised, room_id) = parse_session_path(&path);

    let connection = session_request.accept().await?;
    let remote = connection.remote_address();
    tracing::info!(
        "[web.host.h3.v1] session accepted: peer={} from={} room={:?} caps={:?}",
        peer_id,
        remote,
        room_id,
        advertised
    );

    // The listener owns tag allocation for browser sessions. The
    // browser's `CAPABILITY_INVOKE` flow (a follow-up bite) will replace
    // this with negotiated bindings; for now we hand each advertised
    // feature a tag in the dynamic range in announce order.
    let bindings: Vec<(FeatureTag, FeatureId)> = advertised
        .iter()
        .enumerate()
        .filter_map(|(i, fid)| {
            let tag = DYNAMIC_TAG_START.checked_add(i as u8)?;
            (tag <= DYNAMIC_TAG_END).then(|| (tag, fid.clone()))
        })
        .collect();

    bridge.register_session_with_room(peer_id.clone(), advertised.clone(), bindings, room_id);
    connections.insert(peer_id.clone(), connection.clone());

    // RAII cleanup so the slot is freed regardless of exit path.
    struct Cleanup<'a> {
        bridge: &'a BrowserBridge,
        connections: &'a ConnectionMap,
        peer_id: &'a str,
    }
    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            self.connections.remove(self.peer_id);
            self.bridge.release_session(self.peer_id);
        }
    }
    let _cleanup = Cleanup {
        bridge: &bridge,
        connections: &connections,
        peer_id: &peer_id,
    };

    // Identity handshake on a server-initiated bidi stream. The peer id
    // (from the request path) is treated as a base64url Ed25519 public
    // key; the browser must sign a server-issued 32-byte challenge with
    // the matching private key. Until this completes, `room.*` features
    // refuse to dispatch (see `BrowserBridge::on_inbound`).
    match run_identity_handshake(&connection, &peer_id).await {
        Ok(()) => {
            bridge.mark_verified(&peer_id, peer_id.clone());
            tracing::info!("[web.host.h3.v1] identity verified: peer={}", peer_id);
        }
        Err(e) => {
            tracing::warn!(
                "[web.host.h3.v1] identity handshake failed for peer={} ({}); closing session",
                peer_id,
                e
            );
            connection.close(0u32.into(), b"identity handshake failed");
            return Err(e);
        }
    }

    loop {
        let dgram = match connection.receive_datagram().await {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!(
                    "[web.host.h3.v1] datagram recv ended for {}: {}",
                    peer_id,
                    e
                );
                return Ok(());
            }
        };
        let payload = dgram.payload();
        if payload.is_empty() {
            continue;
        }
        let tag = payload[0];
        let body = &payload[1..];
        bridge.on_inbound_dispatch(&peer_id, tag, body);
    }
}

/// Run the Ed25519 challenge/response handshake on a server-initiated
/// bidi stream. The peer id is the browser's claimed Ed25519 public key
/// in base64url. We send 32 random bytes; the browser must reply with a
/// 64-byte signature over those bytes. Verification uses
/// [`Identity::verify_with_pub`] so the supernode never needs the
/// browser's private key.
async fn run_identity_handshake(
    connection: &wtransport::Connection,
    peer_id_b64: &str,
) -> anyhow::Result<()> {
    use rand::RngCore;

    let pubkey = crate::crypto::b64url_decode(peer_id_b64)
        .map_err(|e| anyhow::anyhow!("peer id is not valid base64url: {}", e))?;
    if pubkey.len() != 32 {
        anyhow::bail!(
            "peer id is not a 32-byte Ed25519 public key (got {} bytes)",
            pubkey.len()
        );
    }

    // Open the bidi stream ourselves so we control the ordering: no
    // datagrams should be processed until the handshake resolves.
    let opening = connection.open_bi().await?;
    let (mut send, mut recv) = opening.await?;

    let mut challenge = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut challenge);
    send.write_all(&challenge).await?;

    let mut signature = [0u8; 64];
    recv.read_exact(&mut signature).await?;

    if !crate::identity::Identity::verify_with_pub(&pubkey, &signature, &challenge) {
        anyhow::bail!("signature did not verify against claimed public key");
    }
    // Best-effort close of the write half; ignore errors.
    let _ = send.finish().await;
    Ok(())
}

/// Parse a WebTransport request path of the shape
/// `/channels/<peer_id>?caps=<csv>&room=<room_id>` into
/// `(peer_id, advertised, room_id)`.
///
/// Anything we can't parse falls back to a synthetic peer id so the
/// session is still tracked but advertises no capabilities (so the
/// bridge will drop all of its inbound traffic - fail-closed).
fn parse_session_path(path: &str) -> (BrowserPeerId, Vec<FeatureId>, Option<String>) {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, q),
        None => (path, ""),
    };
    let peer_id = route
        .trim_start_matches('/')
        .strip_prefix("channels/")
        .map(|s| s.trim_end_matches('/'))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("anon-{}", rand::random::<u32>()));

    let mut advertised: Vec<FeatureId> = Vec::new();
    let mut room_id: Option<String> = None;
    for pair in query.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "caps" => {
                    for fid in v.split(',').filter(|s| !s.is_empty()) {
                        advertised.push(fid.to_string());
                    }
                }
                "room" if !v.is_empty() => room_id = Some(v.to_string()),
                _ => {}
            }
        }
    }
    (peer_id, advertised, room_id)
}

/// Inert listener placeholder kept for callers that want a no-op task
/// (e.g. a build that disables wtransport in the future). Prefer
/// [`run_listener`] for the real path.
pub async fn run_listener_stub(_bridge: BrowserBridge, port: u16) {
    tracing::info!(
        "[web.host.h3.v1] WebTransport bridge enabled on port {} (listener stub)",
        port
    );
    // Park forever; the runtime drops the task on shutdown.
    std::future::pending::<()>().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn capturing_hook() -> (
        SendHook,
        Arc<Mutex<Vec<(BrowserPeerId, FeatureTag, Vec<u8>)>>>,
    ) {
        let log: Arc<Mutex<Vec<(BrowserPeerId, FeatureTag, Vec<u8>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let log_for_hook = log.clone();
        let hook: SendHook = Arc::new(move |peer, tag, payload| {
            log_for_hook
                .lock()
                .unwrap()
                .push((peer.clone(), tag, payload.to_vec()));
            true
        });
        (hook, log)
    }

    fn session_with(
        peer: &str,
        features: &[&str],
        bindings: &[(FeatureTag, &str)],
    ) -> BrowserBridge {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            peer.to_string(),
            features.iter().map(|s| s.to_string()),
            bindings.iter().map(|(t, f)| (*t, f.to_string())),
        );
        bridge
    }

    #[test]
    fn inbound_ok_path_routes_to_handler_and_counts() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        let mut received: Option<(String, Vec<u8>)> = None;
        let resolved = bridge.on_inbound("peer-a", 0x10, b"hello", |fid, p| {
            received = Some((fid.clone(), p.to_vec()));
        });
        assert_eq!(resolved.as_deref(), Some("core.chat.v1"));
        assert_eq!(received, Some(("core.chat.v1".into(), b"hello".to_vec())));
        let s = bridge.stats().pair("peer-a", "core.chat.v1");
        assert_eq!(s.inbound_ok, 1);
        assert_eq!(s.inbound_dropped_no_capability, 0);
    }

    #[test]
    fn inbound_drops_when_feature_not_advertised() {
        let bridge = session_with(
            "peer-a",
            // No "core.chat.v1" in the advertised set.
            &[],
            &[(0x10, "core.chat.v1")],
        );
        let mut called = false;
        let resolved = bridge.on_inbound("peer-a", 0x10, b"x", |_, _| called = true);
        assert!(resolved.is_none());
        assert!(!called);
        let s = bridge.stats().pair("peer-a", "core.chat.v1");
        assert_eq!(s.inbound_dropped_no_capability, 1);
        assert_eq!(s.inbound_ok, 0);
    }

    #[test]
    fn inbound_drops_unknown_tag_silently() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        // Tag 0x11 not bound for this peer.
        let resolved = bridge.on_inbound("peer-a", 0x11, b"x", |_, _| {
            panic!("handler should not be invoked");
        });
        assert!(resolved.is_none());
    }

    #[test]
    fn inbound_drops_reserved_tags() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        for &reserved in &[0x00u8, 0x0F, 0xFF] {
            assert!(bridge
                .on_inbound("peer-a", reserved, b"x", |_, _| panic!("not routed"))
                .is_none());
        }
    }

    #[test]
    fn inbound_unknown_peer_returns_none() {
        let bridge = BrowserBridge::new();
        let resolved = bridge.on_inbound("ghost", 0x10, b"x", |_, _| panic!("not routed"));
        assert!(resolved.is_none());
    }

    #[test]
    fn send_dispatches_via_hook_and_counts() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        assert!(bridge.send("peer-a", "core.chat.v1", b"pong"));
        assert_eq!(log.lock().unwrap().len(), 1);
        assert_eq!(log.lock().unwrap()[0].1, 0x10);
        assert_eq!(bridge.stats().pair("peer-a", "core.chat.v1").outbound_ok, 1);
    }

    #[test]
    fn send_returns_false_for_unknown_peer_or_feature() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        let (hook, _log) = capturing_hook();
        bridge.set_send_hook(hook);
        assert!(!bridge.send("ghost", "core.chat.v1", b"x"));
        assert!(!bridge.send("peer-a", "core.file.v1", b"x"));
    }

    #[test]
    fn send_returns_false_when_no_hook_installed() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        assert!(!bridge.send("peer-a", "core.chat.v1", b"x"));
    }

    #[test]
    fn send_records_failure_when_hook_refuses() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        let hook: SendHook = Arc::new(|_, _, _| false);
        bridge.set_send_hook(hook);
        assert!(!bridge.send("peer-a", "core.chat.v1", b"x"));
        let s = bridge.stats().pair("peer-a", "core.chat.v1");
        assert_eq!(s.outbound_failed, 1);
        assert_eq!(s.outbound_ok, 0);
    }

    #[test]
    fn release_session_clears_state_and_counts() {
        let bridge = session_with("peer-a", &["core.chat.v1"], &[(0x10, "core.chat.v1")]);
        assert_eq!(bridge.stats().sessions_open, 1);
        bridge.release_session("peer-a");
        assert_eq!(bridge.stats().sessions_closed, 1);
        // Subsequent inbound returns None — peer is unknown.
        assert!(bridge
            .on_inbound("peer-a", 0x10, b"x", |_, _| panic!("not routed"))
            .is_none());
    }

    #[test]
    fn advertised_returns_session_capabilities() {
        let bridge = session_with("peer-a", &["core.chat.v1", "core.file.v1"], &[]);
        let advertised = bridge.advertised("peer-a");
        assert!(advertised.contains("core.chat.v1"));
        assert!(advertised.contains("core.file.v1"));
        assert!(bridge.advertised("ghost").is_empty());
    }

    #[test]
    fn parse_session_path_extracts_peer_and_caps() {
        let (peer, caps, room) =
            parse_session_path("/channels/peer-xyz?caps=core.chat.v1,core.file.v1");
        assert_eq!(peer, "peer-xyz");
        assert_eq!(caps, vec!["core.chat.v1", "core.file.v1"]);
        assert!(room.is_none());
    }

    #[test]
    fn parse_session_path_handles_trailing_slash_and_no_query() {
        let (peer, caps, room) = parse_session_path("/channels/peer-xyz/");
        assert_eq!(peer, "peer-xyz");
        assert!(caps.is_empty());
        assert!(room.is_none());
    }

    #[test]
    fn parse_session_path_falls_back_to_anonymous_id_for_unknown_route() {
        let (peer, caps, room) = parse_session_path("/garbage");
        assert!(peer.starts_with("anon-"));
        assert!(caps.is_empty());
        assert!(room.is_none());
    }

    #[test]
    fn parse_session_path_ignores_unknown_query_keys() {
        let (peer, caps, room) = parse_session_path("/channels/p?other=1&caps=core.chat.v1&x=2");
        assert_eq!(peer, "p");
        assert_eq!(caps, vec!["core.chat.v1"]);
        assert!(room.is_none());
    }

    #[test]
    fn parse_session_path_extracts_room_id() {
        let (peer, caps, room) = parse_session_path("/channels/p?caps=room.audio.sfu&room=lobby");
        assert_eq!(peer, "p");
        assert_eq!(caps, vec!["room.audio.sfu"]);
        assert_eq!(room.as_deref(), Some("lobby"));
    }

    /// End-to-end fanout: two browser peers in the same room, payload
    /// from peer-a goes only to peer-b (not back to peer-a).
    #[test]
    fn browser_room_dispatcher_fans_out_to_peers_with_same_feature() {
        let bridge = BrowserBridge::new();
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
        bridge.register_session(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );
        bridge.register_session(
            "peer-b".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );
        bridge.mark_verified("peer-a", "k".to_string());

        let resolved = bridge.on_inbound_dispatch("peer-a", 0x10, b"opus-frame");
        assert_eq!(resolved.as_deref(), Some("room.audio.sfu"));

        let sent = log.lock().unwrap();
        assert_eq!(sent.len(), 1, "exactly one fanout target");
        assert_eq!(sent[0].0, "peer-b");
        assert_eq!(sent[0].1, 0x10);
        assert_eq!(sent[0].2, b"opus-frame".to_vec());
    }

    #[test]
    fn browser_room_dispatcher_skips_non_room_features() {
        let bridge = BrowserBridge::new();
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
        bridge.register_session(
            "peer-a".to_string(),
            ["core.chat.v1".to_string()],
            [(0x10u8, "core.chat.v1".to_string())],
        );
        bridge.register_session(
            "peer-b".to_string(),
            ["core.chat.v1".to_string()],
            [(0x10u8, "core.chat.v1".to_string())],
        );

        bridge.on_inbound_dispatch("peer-a", 0x10, b"hello");
        assert!(log.lock().unwrap().is_empty(), "core.* not fanned out");
    }

    #[test]
    fn on_inbound_dispatch_respects_capability_gate() {
        let bridge = BrowserBridge::new();
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
        // peer-a binds the tag but does NOT advertise the feature.
        bridge.register_session(
            "peer-a".to_string(),
            Vec::<String>::new(),
            [(0x10u8, "room.audio.sfu".to_string())],
        );
        bridge.register_session(
            "peer-b".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );

        let resolved = bridge.on_inbound_dispatch("peer-a", 0x10, b"x");
        assert!(resolved.is_none());
        assert!(log.lock().unwrap().is_empty());
    }

    #[test]
    fn peers_advertising_lists_only_matching_sessions() {
        let bridge = BrowserBridge::new();
        bridge.register_session("peer-a".to_string(), ["room.audio.sfu".to_string()], []);
        bridge.register_session("peer-b".to_string(), ["core.chat.v1".to_string()], []);
        bridge.register_session("peer-c".to_string(), ["room.audio.sfu".to_string()], []);
        let mut peers = bridge.peers_advertising("room.audio.sfu");
        peers.sort();
        assert_eq!(peers, vec!["peer-a".to_string(), "peer-c".to_string()]);
    }

    #[test]
    fn browser_room_dispatcher_only_fans_out_to_same_room() {
        let bridge = BrowserBridge::new();
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
        bridge.register_session_with_room(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
            Some("lobby".to_string()),
        );
        bridge.register_session_with_room(
            "peer-b".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
            Some("lobby".to_string()),
        );
        bridge.register_session_with_room(
            "peer-c".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
            Some("other-room".to_string()),
        );
        bridge.mark_verified("peer-a", "k".to_string());

        bridge.on_inbound_dispatch("peer-a", 0x10, b"frame");

        let sent = log.lock().unwrap();
        assert_eq!(sent.len(), 1, "only peer-b in same room");
        assert_eq!(sent[0].0, "peer-b");
    }

    #[test]
    fn module_native_dispatcher_invokes_hook_for_room_features() {
        let bridge = BrowserBridge::new();
        let (hook, browser_log) = capturing_hook();
        bridge.set_send_hook(hook);

        let native_log: Arc<Mutex<Vec<(String, String, Vec<u8>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let nl = native_log.clone();
        let native_hook: NativeMessageHook = Arc::new(move |source, fid, payload| {
            nl.lock()
                .unwrap()
                .push((source.to_string(), fid.to_string(), payload.to_vec()));
        });
        bridge.set_dispatcher(Arc::new(ModuleNativeDispatcher::new(native_hook)));

        bridge.register_session_with_room(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
            Some("lobby".to_string()),
        );
        bridge.register_session_with_room(
            "peer-b".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
            Some("lobby".to_string()),
        );
        bridge.mark_verified("peer-a", "k".to_string());

        bridge.on_inbound_dispatch("peer-a", 0x10, b"opus");

        // Browser fan-out: peer-b only.
        let browser = browser_log.lock().unwrap();
        assert_eq!(browser.len(), 1);
        assert_eq!(browser[0].0, "peer-b");

        // Native hook: fires exactly once per inbound, not per member.
        let native = native_log.lock().unwrap();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].0, "peer-a");
        assert_eq!(native[0].1, "room.audio.sfu");
        assert_eq!(native[0].2, b"opus".to_vec());
    }

    #[test]
    fn module_native_dispatcher_skips_hook_when_no_room() {
        let bridge = BrowserBridge::new();
        let (hook, _log) = capturing_hook();
        bridge.set_send_hook(hook);

        let native_log: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let nl = native_log.clone();
        let native_hook: NativeMessageHook = Arc::new(move |_s, _f, _p| {
            *nl.lock().unwrap() += 1;
        });
        bridge.set_dispatcher(Arc::new(ModuleNativeDispatcher::new(native_hook)));

        bridge.register_session(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );
        bridge.mark_verified("peer-a", "k".to_string());

        bridge.on_inbound_dispatch("peer-a", 0x10, b"x");
        assert_eq!(*native_log.lock().unwrap(), 0, "no room - no hook call");
    }

    #[test]
    fn module_native_dispatcher_skips_hook_for_non_room_features() {
        let bridge = BrowserBridge::new();
        let (hook, _log) = capturing_hook();
        bridge.set_send_hook(hook);

        let native_log: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let nl = native_log.clone();
        let native_hook: NativeMessageHook = Arc::new(move |_s, _f, _p| {
            *nl.lock().unwrap() += 1;
        });
        bridge.set_dispatcher(Arc::new(ModuleNativeDispatcher::new(native_hook)));

        bridge.register_session_with_room(
            "peer-a".to_string(),
            ["core.chat.v1".to_string()],
            [(0x10u8, "core.chat.v1".to_string())],
            Some("lobby".to_string()),
        );

        bridge.on_inbound_dispatch("peer-a", 0x10, b"hi");
        assert_eq!(
            *native_log.lock().unwrap(),
            0,
            "core.* not native-forwarded"
        );
    }

    #[test]
    fn mark_verified_sets_session_identity() {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );
        assert!(bridge.session_identity("peer-a").is_none());
        assert!(!bridge.is_verified("peer-a"));

        bridge.mark_verified("peer-a", "pubkey-b64".to_string());
        assert_eq!(
            bridge.session_identity("peer-a").as_deref(),
            Some("pubkey-b64")
        );
        assert!(bridge.is_verified("peer-a"));
    }

    #[test]
    fn mark_verified_is_noop_for_unknown_peer() {
        let bridge = BrowserBridge::new();
        bridge.mark_verified("ghost", "x".to_string());
        assert!(bridge.session_identity("ghost").is_none());
    }

    #[test]
    fn release_session_clears_verified_identity() {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            "peer-a".to_string(),
            ["room.chat.v1".to_string()],
            [(0x10u8, "room.chat.v1".to_string())],
        );
        bridge.mark_verified("peer-a", "k".to_string());
        bridge.release_session("peer-a");
        assert!(bridge.session_identity("peer-a").is_none());
    }

    #[test]
    fn re_register_resets_verified_identity() {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            "peer-a".to_string(),
            ["room.chat.v1".to_string()],
            [(0x10u8, "room.chat.v1".to_string())],
        );
        bridge.mark_verified("peer-a", "k".to_string());
        // Same peer reconnects: handshake must run again before any
        // gated dispatch is allowed.
        bridge.register_session(
            "peer-a".to_string(),
            ["room.chat.v1".to_string()],
            [(0x10u8, "room.chat.v1".to_string())],
        );
        assert!(bridge.session_identity("peer-a").is_none());
    }

    #[test]
    fn unverified_room_payload_is_dropped_with_counter() {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );

        let resolved = bridge.on_inbound("peer-a", 0x10, b"opus", |_, _| {});
        assert!(resolved.is_none(), "room.* must require verification");
        let stats = bridge.stats();
        let pair = stats
            .per_pair
            .get(&("peer-a".to_string(), "room.audio.sfu".to_string()))
            .copied()
            .unwrap_or_default();
        assert_eq!(pair.inbound_dropped_unverified, 1);
        assert_eq!(pair.inbound_ok, 0);
    }

    #[test]
    fn verified_room_payload_dispatches() {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            "peer-a".to_string(),
            ["room.audio.sfu".to_string()],
            [(0x10u8, "room.audio.sfu".to_string())],
        );
        bridge.mark_verified("peer-a", "k".to_string());

        let resolved = bridge.on_inbound("peer-a", 0x10, b"opus", |_, _| {});
        assert_eq!(resolved.as_deref(), Some("room.audio.sfu"));
    }

    #[test]
    fn non_room_feature_does_not_require_verification() {
        let bridge = BrowserBridge::new();
        bridge.register_session(
            "peer-a".to_string(),
            ["core.chat.v1".to_string()],
            [(0x10u8, "core.chat.v1".to_string())],
        );
        // Not verified: non-room features still pass the gate. (Other
        // policies such as per-feature `auth` tiers apply elsewhere.)
        let resolved = bridge.on_inbound("peer-a", 0x10, b"hi", |_, _| {});
        assert_eq!(resolved.as_deref(), Some("core.chat.v1"));
    }

    // ── game.relay.v1 tests ──────────────────────────────────────────────────

    #[test]
    fn game_relay_fans_out_within_session() {
        let bridge = BrowserBridge::new();
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
        bridge.register_session_with_room(
            "player-a".to_string(),
            ["game.relay.v1".to_string()],
            [(0x10u8, "game.relay.v1".to_string())],
            Some("lobby1".to_string()),
        );
        bridge.register_session_with_room(
            "player-b".to_string(),
            ["game.relay.v1".to_string()],
            [(0x10u8, "game.relay.v1".to_string())],
            Some("lobby1".to_string()),
        );
        bridge.mark_verified("player-a", "k".to_string());

        let resolved = bridge.on_inbound_dispatch("player-a", 0x10, b"game-state");
        assert_eq!(resolved.as_deref(), Some("game.relay.v1"));

        let sent = log.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "player-b");
        assert_eq!(sent[0].2, b"game-state".to_vec());
    }

    #[test]
    fn game_relay_does_not_cross_sessions() {
        let bridge = BrowserBridge::new();
        let (hook, log) = capturing_hook();
        bridge.set_send_hook(hook);
        bridge.set_dispatcher(Arc::new(BrowserRoomDispatcher));
        bridge.register_session_with_room(
            "player-a".to_string(),
            ["game.relay.v1".to_string()],
            [(0x10u8, "game.relay.v1".to_string())],
            Some("session-1".to_string()),
        );
        bridge.register_session_with_room(
            "player-b".to_string(),
            ["game.relay.v1".to_string()],
            [(0x10u8, "game.relay.v1".to_string())],
            Some("session-2".to_string()),
        );
        bridge.mark_verified("player-a", "k".to_string());

        bridge.on_inbound_dispatch("player-a", 0x10, b"state");
        assert!(
            log.lock().unwrap().is_empty(),
            "must not cross session boundary"
        );
    }

    #[test]
    fn game_relay_requires_verified_identity() {
        let bridge = BrowserBridge::new();
        bridge.register_session_with_room(
            "player-a".to_string(),
            ["game.relay.v1".to_string()],
            [(0x10u8, "game.relay.v1".to_string())],
            Some("lobby1".to_string()),
        );
        // Not verified — must be dropped.
        let resolved = bridge.on_inbound("player-a", 0x10, b"x", |_, _| {});
        assert!(resolved.is_none());
        let s = bridge.stats().pair("player-a", "game.relay.v1");
        assert_eq!(s.inbound_dropped_unverified, 1);
    }

    #[test]
    fn module_native_dispatcher_invokes_hook_for_game_relay() {
        let bridge = BrowserBridge::new();
        let (hook, _browser_log) = capturing_hook();
        bridge.set_send_hook(hook);

        let native_log: Arc<Mutex<Vec<(String, String, Vec<u8>)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let nl = native_log.clone();
        let native_hook: NativeMessageHook = Arc::new(move |src, fid, payload| {
            nl.lock()
                .unwrap()
                .push((src.to_string(), fid.to_string(), payload.to_vec()));
        });
        bridge.set_dispatcher(Arc::new(ModuleNativeDispatcher::new(native_hook)));

        bridge.register_session_with_room(
            "player-a".to_string(),
            ["game.relay.v1".to_string()],
            [(0x10u8, "game.relay.v1".to_string())],
            Some("arena".to_string()),
        );
        bridge.mark_verified("player-a", "k".to_string());

        bridge.on_inbound_dispatch("player-a", 0x10, b"pos");

        let native = native_log.lock().unwrap();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].0, "player-a");
        assert_eq!(native[0].1, "game.relay.v1");
    }
}
