//! In-process registry of advertised capabilities.

use parking_lot::RwLock;
use std::collections::BTreeMap;

use crate::channel_tag::ChannelTagRegistry;
use crate::descriptor::{CapabilityDescriptor, ChannelKind, FeatureError};
use crate::module::{InvocationContext, ModuleError, ModuleResult, PeerId, SharedModule};
use crate::quota::{QuotaParams, QuotaRegistry};

/// Owns the set of capabilities a peer or supernode advertises, and
/// optionally the [`FeatureModule`] implementation that handles
/// invocations for each id.
///
/// The registry is intentionally simple: it stores one descriptor per
/// id (and at most one module per id) and yields a deterministic,
/// sorted list for advertisement. Higher-level concerns (channel
/// multiplexing, async dispatch) live in transport-specific consumers.
///
/// Runtime quota enforcement (byte + datagram token-bucket) is
/// provided by embedded [`QuotaRegistry`] instances — every call to
/// [`dispatch_message`] and [`dispatch_invoke_datagram`] checks the
/// per-(feature, peer) **inbound** quota before invoking the module.
/// Outbound sends are gated via [`gate_through_feature`], which
/// consults the matching **outbound** quota bucket.
#[derive(Default)]
pub struct FeatureRegistry {
    inner: RwLock<BTreeMap<String, Entry>>,
    /// Inbound token-bucket quota state, keyed by (feature_id, peer_id).
    quotas: QuotaRegistry,
    /// Outbound token-bucket quota state, keyed by (feature_id, peer_id).
    /// Separate from inbound so each direction has its own token budget.
    outbound_quotas: QuotaRegistry,
}

#[derive(Clone)]
struct Entry {
    descriptor: CapabilityDescriptor,
    module: Option<SharedModule>,
}

impl FeatureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Extract quota params from a descriptor, using defaults where absent.
    fn quota_params_for(descriptor: &CapabilityDescriptor) -> QuotaParams {
        QuotaParams::from_params(&descriptor.params)
    }

    /// Register a capability. Fails if `id` is already present or the
    /// descriptor fails validation.
    pub fn register(&self, cap: CapabilityDescriptor) -> Result<(), FeatureError> {
        cap.validate()?;
        let mut g = self.inner.write();
        if g.contains_key(&cap.id) {
            return Err(FeatureError::Duplicate(cap.id));
        }
        g.insert(
            cap.id.clone(),
            Entry {
                descriptor: cap,
                module: None,
            },
        );
        Ok(())
    }

    /// Register a capability **with** an in-process module. The
    /// descriptor is taken from `module.descriptor()`. Fails if a
    /// descriptor with the same id is already present.
    pub fn register_module(&self, module: SharedModule) -> Result<(), FeatureError> {
        let cap = module.descriptor();
        cap.validate()?;
        let mut g = self.inner.write();
        if g.contains_key(&cap.id) {
            return Err(FeatureError::Duplicate(cap.id));
        }
        g.insert(
            cap.id.clone(),
            Entry {
                descriptor: cap,
                module: Some(module),
            },
        );
        Ok(())
    }

    /// Attach (or replace) the module bound to an existing
    /// descriptor. Returns `false` if no descriptor with `id` is
    /// registered yet.
    pub fn bind_module(&self, id: &str, module: SharedModule) -> bool {
        let mut g = self.inner.write();
        match g.get_mut(id) {
            Some(entry) => {
                entry.module = Some(module);
                true
            }
            None => false,
        }
    }

    /// Replace (or insert) a capability. Used for hot-reload of supernode
    /// feature configs. Preserves any module previously bound to *id*.
    pub fn upsert(&self, cap: CapabilityDescriptor) -> Result<(), FeatureError> {
        cap.validate()?;
        let mut g = self.inner.write();
        let prior = g.remove(&cap.id).and_then(|e| e.module);
        g.insert(
            cap.id.clone(),
            Entry {
                descriptor: cap,
                module: prior,
            },
        );
        Ok(())
    }

    /// Remove a capability by id. Returns the prior descriptor if any.
    pub fn remove(&self, id: &str) -> Option<CapabilityDescriptor> {
        self.inner.write().remove(id).map(|e| e.descriptor)
    }

    /// Lookup a capability by id.
    pub fn get(&self, id: &str) -> Option<CapabilityDescriptor> {
        self.inner.read().get(id).map(|e| e.descriptor.clone())
    }

    /// Lookup the module bound to *id*, if any.
    pub fn module(&self, id: &str) -> Option<SharedModule> {
        self.inner.read().get(id).and_then(|e| e.module.clone())
    }

    /// Convenience: dispatch an invocation to the module bound to
    /// *id*. Returns `ModuleError::Internal` if no module is bound.
    pub fn dispatch_invoke(&self, id: &str, ctx: InvocationContext) -> ModuleResult<()> {
        let module = self
            .module(id)
            .ok_or_else(|| ModuleError::Internal(format!("no module bound to '{id}'")))?;
        module.on_invoke(ctx)
    }

    /// Per-message dispatch with quota enforcement. Returns `true` if a module
    /// was bound and `on_message` was invoked; `false` if no module is
    /// registered for *id* or if the per-(feature, peer) byte/datagram quota
    /// is exceeded.  Transports use this for the per-datagram hot path so
    /// callers don't have to handle errors per message.
    pub fn dispatch_message(
        &self,
        id: &str,
        source: crate::module::PeerId,
        payload: &[u8],
    ) -> bool {
        // Look up module + descriptor under the same read lock to stay
        // consistent and avoid a second lock acquisition.
        let (module, quota_params) = {
            let g = self.inner.read();
            match g.get(id) {
                Some(e) => match &e.module {
                    Some(m) => (m.clone(), Self::quota_params_for(&e.descriptor)),
                    None => return false,
                },
                None => return false,
            }
        };

        if !self
            .quotas
            .try_consume(id, &source, payload.len(), quota_params)
        {
            // Quota exceeded — drop silently; the transport layer should log.
            return false;
        }

        module.on_message(source, payload);
        true
    }

    /// Number of registered capabilities.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }

    /// Snapshot of all advertised capabilities, sorted by id for stable
    /// on-wire ordering.
    pub fn snapshot(&self) -> Vec<CapabilityDescriptor> {
        self.inner
            .read()
            .values()
            .map(|e| e.descriptor.clone())
            .collect()
    }

    /// Allocate a channel tag from *tags* and invoke the datagram feature *id*
    /// on *peer* with *params*.
    ///
    /// This is the canonical "invoke at feature startup" helper for
    /// datagram-kind features (Phase 3 §"Allocate a tag at feature invoke time"):
    ///
    /// 1. Verifies the feature is registered and is a `Datagram` kind.
    /// 2. Allocates a tag from *tags* via [`ChannelTagRegistry::bind`].
    /// 3. Calls [`dispatch_invoke`] with `channel_tag = Some(tag)` in the context.
    /// 4. Returns the allocated tag on success, or releases the tag and
    ///    returns an error if the module rejects the invocation.
    pub fn dispatch_invoke_datagram(
        &self,
        id: &str,
        peer: PeerId,
        params: serde_json::Value,
        tags: &ChannelTagRegistry,
    ) -> ModuleResult<u8> {
        // Verify the feature is registered and is datagram-kind.
        let descriptor = {
            let g = self.inner.read();
            let entry = g.get(id).ok_or_else(|| {
                ModuleError::Internal(format!("no feature registered for `{id}`"))
            })?;
            entry.descriptor.clone()
        };
        if descriptor.kind != ChannelKind::Datagram {
            return Err(ModuleError::InvalidParams(format!(
                "`{id}` is a {:?} feature, not Datagram",
                descriptor.kind
            )));
        }

        // Quota check before allocating a tag so we don't waste a tag slot.
        let quota_params = Self::quota_params_for(&descriptor);
        if !self.quotas.try_consume_datagram(id, &peer, quota_params) {
            return Err(ModuleError::Internal(format!(
                "datagram quota exceeded for '{id}' from peer '{peer}'"
            )));
        }

        // Allocate a tag — fail fast so we don't invoke a module when the
        // tag space is exhausted.
        let tag = tags
            .bind(id)
            .map_err(|e| ModuleError::Internal(format!("tag allocation failed: {e}")))?;
        let ctx = InvocationContext {
            peer,
            params,
            channel_tag: Some(tag),
        };
        match self.dispatch_invoke(id, ctx) {
            Ok(()) => Ok(tag),
            Err(e) => {
                // Release the tag so it can be reused.
                let _ = tags.release(id);
                Err(e)
            }
        }
    }
    /// Remove all quota state for *peer_id*. Call when a peer disconnects.
    pub fn clear_peer_quotas(&self, peer_id: &str) {
        self.quotas.clear_peer(peer_id);
    }

    /// Gate an inbound receive through the local feature descriptor's quota
    /// **without** invoking the bound module's `on_message` callback.
    ///
    /// Use on real-time hot paths (audio datagrams, SFU relay fan-out) where
    /// the transport layer handles the payload after the quota check.
    ///
    /// Returns `true` when allowed (or when no local descriptor is registered).
    pub fn gate_inbound_through_feature(
        &self,
        feature_id: &str,
        peer_id: &str,
        byte_count: usize,
    ) -> bool {
        let quota_params = {
            let g = self.inner.read();
            match g.get(feature_id) {
                Some(e) => Self::quota_params_for(&e.descriptor),
                None => return true,
            }
        };
        self.quotas
            .try_consume(feature_id, peer_id, byte_count, quota_params)
    }

    // -- Outbound gating ------------------------------------------------

    /// Gate an outbound send through the local feature descriptor's quota.
    ///
    /// Before sending `byte_count` bytes of a `feature_id` message to
    /// `peer_id`, call this method.  It consumes tokens from the
    /// **outbound** bucket (separate from the inbound one) and returns:
    ///
    /// * `true`  — allowed; the send may proceed.
    /// * `false` — quota exhausted; the caller should drop or defer the send.
    ///
    /// If no descriptor for `feature_id` is registered locally (e.g. a
    /// transport-layer message outside the feature namespace), the method
    /// returns `true` without touching any bucket — "no descriptor = no gate".
    pub fn gate_through_feature(&self, feature_id: &str, peer_id: &str, byte_count: usize) -> bool {
        let quota_params = {
            let g = self.inner.read();
            match g.get(feature_id) {
                Some(e) => Self::quota_params_for(&e.descriptor),
                None => return true, // no local descriptor ⇒ pass through unmetered
            }
        };
        self.outbound_quotas
            .try_consume(feature_id, peer_id, byte_count, quota_params)
    }

    /// Remove all **outbound** quota state for *peer_id*.
    /// Call alongside [`clear_peer_quotas`] when a peer disconnects so the
    /// next connection starts with a fresh outbound budget.
    pub fn clear_peer_outbound_quotas(&self, peer_id: &str) {
        self.outbound_quotas.clear_peer(peer_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::ChannelKind;
    use crate::wellknown;

    #[test]
    fn register_and_snapshot_is_sorted() {
        let r = FeatureRegistry::new();
        r.register(CapabilityDescriptor::new(
            "core.file.v1",
            "1.0",
            ChannelKind::Stream,
        ))
        .unwrap();
        r.register(CapabilityDescriptor::new(
            "core.chat.v1",
            "1.0",
            ChannelKind::Stream,
        ))
        .unwrap();
        let snap = r.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].id, "core.chat.v1");
        assert_eq!(snap[1].id, "core.file.v1");
    }

    #[test]
    fn duplicate_register_fails_but_upsert_succeeds() {
        let r = FeatureRegistry::new();
        let cap = CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream);
        r.register(cap.clone()).unwrap();
        assert!(r.register(cap.clone()).is_err());
        r.upsert(CapabilityDescriptor::new(
            "core.chat.v1",
            "1.1",
            ChannelKind::Stream,
        ))
        .unwrap();
        assert_eq!(r.get("core.chat.v1").unwrap().version, "1.1");
    }

    #[test]
    fn invalid_descriptor_rejected() {
        let r = FeatureRegistry::new();
        let bad = CapabilityDescriptor::new("nodot", "1.0", ChannelKind::Stream);
        assert!(r.register(bad).is_err());
    }

    use crate::module::{FeatureModule, InvocationContext, ModuleResult, SharedModule};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingModule {
        id: &'static str,
        invocations: AtomicUsize,
    }

    impl FeatureModule for CountingModule {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor::new(self.id, "1.0", ChannelKind::Stream)
        }
        fn on_invoke(&self, _ctx: InvocationContext) -> ModuleResult<()> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn ctx() -> InvocationContext {
        InvocationContext {
            peer: "peer-a".into(),
            params: serde_json::Value::Null,
            channel_tag: None,
        }
    }

    #[test]
    fn register_module_advertises_descriptor_and_dispatches() {
        let r = FeatureRegistry::new();
        let m: SharedModule = Arc::new(CountingModule {
            id: "x.test.counter",
            invocations: AtomicUsize::new(0),
        });
        r.register_module(m.clone()).unwrap();
        assert!(r.get("x.test.counter").is_some());
        assert!(r.module("x.test.counter").is_some());
        r.dispatch_invoke("x.test.counter", ctx()).unwrap();
        r.dispatch_invoke("x.test.counter", ctx()).unwrap();
        // Recover the concrete module to read the counter.
        let raw = Arc::into_raw(m) as *const CountingModule;
        let count = unsafe { (*raw).invocations.load(Ordering::SeqCst) };
        let _ = unsafe { Arc::from_raw(raw) };
        assert_eq!(count, 2);
    }

    #[test]
    fn dispatch_without_module_errors() {
        let r = FeatureRegistry::new();
        r.register(CapabilityDescriptor::new(
            "core.chat.v1",
            "1.0",
            ChannelKind::Stream,
        ))
        .unwrap();
        let err = r.dispatch_invoke("core.chat.v1", ctx()).unwrap_err();
        assert!(matches!(err, ModuleError::Internal(_)));
    }

    #[test]
    fn dispatch_invoke_datagram_allocates_tag_and_invokes() {
        use crate::channel_tag::ChannelTagRegistry;

        let tags = ChannelTagRegistry::new();
        let r = FeatureRegistry::new();

        // Register a datagram feature with a module that tracks invocations.
        let m: SharedModule = Arc::new(CountingModule {
            id: "core.audio.opus",
            invocations: AtomicUsize::new(0),
        });
        // Override descriptor to be Datagram kind.
        let desc = CapabilityDescriptor::new("core.audio.opus", "1.0", ChannelKind::Datagram);
        r.register(desc).unwrap();
        r.bind_module("core.audio.opus", m.clone());

        let tag = r
            .dispatch_invoke_datagram(
                "core.audio.opus",
                "peer-a".into(),
                serde_json::Value::Null,
                &tags,
            )
            .expect("invoke_datagram should succeed");

        // Tag must be in the dynamic range.
        assert!((0x10..=0xEF).contains(&tag));
        // The tag should be bound in the registry.
        assert_eq!(tags.tag_for("core.audio.opus"), Some(tag));
        // The module must have been invoked once.
        let raw = Arc::into_raw(m) as *const CountingModule;
        let count = unsafe { (*raw).invocations.load(Ordering::SeqCst) };
        let _ = unsafe { Arc::from_raw(raw) };
        assert_eq!(count, 1);

        // Releasing the tag should make it available again.
        tags.release("core.audio.opus").unwrap();
        assert!(tags.tag_for("core.audio.opus").is_none());
    }

    #[test]
    fn dispatch_invoke_datagram_rejects_stream_feature() {
        use crate::channel_tag::ChannelTagRegistry;

        let tags = ChannelTagRegistry::new();
        let r = FeatureRegistry::new();
        r.register(CapabilityDescriptor::new(
            "core.chat.v1",
            "1.0",
            ChannelKind::Stream,
        ))
        .unwrap();

        let err = r
            .dispatch_invoke_datagram(
                "core.chat.v1",
                "peer-a".into(),
                serde_json::Value::Null,
                &tags,
            )
            .unwrap_err();
        assert!(matches!(err, ModuleError::InvalidParams(_)));
        // No tag should be allocated.
        assert!(tags.tag_for("core.chat.v1").is_none());
    }

    #[test]
    fn dispatch_invoke_datagram_releases_tag_on_module_error() {
        use crate::channel_tag::ChannelTagRegistry;

        struct FailingModule;
        impl FeatureModule for FailingModule {
            fn descriptor(&self) -> CapabilityDescriptor {
                CapabilityDescriptor::new("x.test.fail", "1.0", ChannelKind::Datagram)
            }
            fn on_invoke(&self, _ctx: InvocationContext) -> ModuleResult<()> {
                Err(ModuleError::Internal("intentional failure".into()))
            }
        }

        let tags = ChannelTagRegistry::new();
        let r = FeatureRegistry::new();
        r.register_module(Arc::new(FailingModule)).unwrap();

        let err = r
            .dispatch_invoke_datagram(
                "x.test.fail",
                "peer-a".into(),
                serde_json::Value::Null,
                &tags,
            )
            .unwrap_err();
        assert!(matches!(err, ModuleError::Internal(_)));
        // Tag must have been released so the space is not leaked.
        assert_eq!(tags.bound_count(), 0);
    }

    #[test]
    fn bind_module_attaches_to_existing_descriptor() {
        let r = FeatureRegistry::new();
        r.register(CapabilityDescriptor::new(
            "x.test.bind",
            "1.0",
            ChannelKind::Stream,
        ))
        .unwrap();
        assert!(r.module("x.test.bind").is_none());
        let m: SharedModule = Arc::new(CountingModule {
            id: "x.test.bind",
            invocations: AtomicUsize::new(0),
        });
        assert!(r.bind_module("x.test.bind", m));
        assert!(r.module("x.test.bind").is_some());
        assert!(!r.bind_module(
            "x.test.unknown",
            Arc::new(CountingModule {
                id: "x.test.unknown",
                invocations: AtomicUsize::new(0),
            })
        ));
    }

    #[test]
    fn upsert_preserves_bound_module() {
        let r = FeatureRegistry::new();
        let m: SharedModule = Arc::new(CountingModule {
            id: "x.test.upsert",
            invocations: AtomicUsize::new(0),
        });
        r.register_module(m).unwrap();
        // Upsert a new descriptor version.
        r.upsert(CapabilityDescriptor::new(
            "x.test.upsert",
            "2.0",
            ChannelKind::Stream,
        ))
        .unwrap();
        assert_eq!(r.get("x.test.upsert").unwrap().version, "2.0");
        // Module survives the upsert so dispatch keeps working.
        assert!(r.dispatch_invoke("x.test.upsert", ctx()).is_ok());
    }

    #[test]
    fn register_module_rejects_duplicate_id() {
        let r = FeatureRegistry::new();
        let m1: SharedModule = Arc::new(CountingModule {
            id: "x.test.dup",
            invocations: AtomicUsize::new(0),
        });
        let m2: SharedModule = Arc::new(CountingModule {
            id: "x.test.dup",
            invocations: AtomicUsize::new(0),
        });
        r.register_module(m1).unwrap();
        assert!(r.register_module(m2).is_err());
    }

    struct MessageRecorder {
        id: &'static str,
        log: parking_lot::Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl FeatureModule for MessageRecorder {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor::new(self.id, "1.0", ChannelKind::Datagram)
        }
        fn on_message(&self, source: crate::module::PeerId, payload: &[u8]) {
            self.log.lock().push((source, payload.to_vec()));
        }
    }

    #[test]
    fn dispatch_message_invokes_bound_module() {
        let r = FeatureRegistry::new();
        let m = Arc::new(MessageRecorder {
            id: "x.test.msg",
            log: parking_lot::Mutex::new(Vec::new()),
        });
        r.register_module(m.clone()).unwrap();
        assert!(r.dispatch_message("x.test.msg", "peer-a".into(), b"hello"));
        assert!(r.dispatch_message("x.test.msg", "peer-b".into(), b"world"));
        let log = m.log.lock();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].0, "peer-a");
        assert_eq!(log[1].1, b"world");
    }

    #[test]
    fn dispatch_message_returns_false_when_no_module() {
        let r = FeatureRegistry::new();
        assert!(!r.dispatch_message("nope.v1", "peer".into(), b"x"));
    }

    // --- Outbound quota symmetry tests ---

    fn tiny_quota_cap(id: &'static str) -> CapabilityDescriptor {
        // 10 bytes/sec, 2 datagrams/sec — easy to exhaust synchronously.
        CapabilityDescriptor::new(id, "1.0", ChannelKind::Stream).with_params(serde_json::json!({
            "quota_bytes_per_sec": 10,
            "quota_datagrams_per_sec": 2,
        }))
    }

    #[test]
    fn gate_through_feature_passes_when_within_budget() {
        let r = FeatureRegistry::new();
        r.register(tiny_quota_cap("x.test.outbound")).unwrap();
        // First small send must be allowed.
        assert!(r.gate_through_feature("x.test.outbound", "peer-a", 5));
    }

    #[test]
    fn gate_through_feature_blocks_after_budget_exhausted() {
        let r = FeatureRegistry::new();
        r.register(tiny_quota_cap("x.test.gate")).unwrap();
        // Drain the 10-byte bucket.
        assert!(r.gate_through_feature("x.test.gate", "peer-a", 10));
        // Next send — even 1 byte — must be blocked.
        assert!(!r.gate_through_feature("x.test.gate", "peer-a", 1));
    }

    #[test]
    fn clear_peer_outbound_quotas_resets_budget() {
        let r = FeatureRegistry::new();
        r.register(tiny_quota_cap("x.test.clear")).unwrap();
        // Drain the bucket.
        r.gate_through_feature("x.test.clear", "peer-a", 10);
        assert!(
            !r.gate_through_feature("x.test.clear", "peer-a", 1),
            "should be blocked before clear"
        );
        // Clear peer state and verify the bucket is fresh again.
        r.clear_peer_outbound_quotas("peer-a");
        assert!(
            r.gate_through_feature("x.test.clear", "peer-a", 5),
            "should pass after clear"
        );
    }

    #[test]
    fn inbound_and_outbound_quotas_are_independent() {
        let r = FeatureRegistry::new();
        let m = Arc::new(MessageRecorder {
            id: "x.test.sym",
            log: parking_lot::Mutex::new(Vec::new()),
        });
        r.register(tiny_quota_cap("x.test.sym")).unwrap();
        r.bind_module("x.test.sym", m);

        // Drain the inbound byte bucket (10 bytes).
        assert!(r.dispatch_message("x.test.sym", "peer-a".into(), &[0u8; 10]));
        // Inbound is now exhausted.
        assert!(
            !r.dispatch_message("x.test.sym", "peer-a".into(), &[0u8; 1]),
            "inbound should be exhausted"
        );
        // Outbound bucket is separate — must still pass.
        assert!(
            r.gate_through_feature("x.test.sym", "peer-a", 5),
            "outbound must be independent from inbound"
        );
    }

    #[test]
    fn gate_returns_true_for_unknown_feature() {
        let r = FeatureRegistry::new();
        // No descriptor registered → no gate → always pass through.
        assert!(r.gate_through_feature("unknown.feature.v1", "peer-a", 99_999));
    }

    #[test]
    fn outbound_quotas_are_per_peer() {
        let r = FeatureRegistry::new();
        r.register(tiny_quota_cap("x.test.perpeer")).unwrap();
        // Drain peer-a's bucket.
        r.gate_through_feature("x.test.perpeer", "peer-a", 10);
        assert!(
            !r.gate_through_feature("x.test.perpeer", "peer-a", 1),
            "peer-a should be blocked"
        );
        // peer-b has its own independent bucket.
        assert!(
            r.gate_through_feature("x.test.perpeer", "peer-b", 10),
            "peer-b should still pass"
        );
    }

    // --- P1 #6: Room feature quota symmetry tests ---
    #[test]
    fn room_audio_sfu_outbound_quota_exhaustion() {
        let r = FeatureRegistry::new();
        r.register(wellknown::room_audio_sfu()).unwrap();

        // Use tiny payloads so this test specifically exhausts the
        // descriptor's datagram bucket without hitting the byte bucket first.
        for _ in 0..200 {
            assert!(r.gate_through_feature("room.audio.sfu", "room-peer-1", 1));
        }
        assert!(
            !r.gate_through_feature("room.audio.sfu", "room-peer-1", 1),
            "room.audio.sfu outbound should be exhausted after burst"
        );

        // Different peer still has budget
        assert!(
            r.gate_through_feature("room.audio.sfu", "room-peer-2", 100),
            "other room peer should still have quota"
        );
    }

    #[test]
    fn room_audio_sfu_inbound_quota_exhaustion() {
        let r = FeatureRegistry::new();
        r.register(wellknown::room_audio_sfu()).unwrap();

        for _ in 0..200 {
            assert!(r.gate_inbound_through_feature("room.audio.sfu", "room-peer-1", 1));
        }
        assert!(
            !r.gate_inbound_through_feature("room.audio.sfu", "room-peer-1", 1),
            "room.audio.sfu inbound should be exhausted after burst"
        );
        assert!(
            r.gate_inbound_through_feature("room.audio.sfu", "room-peer-2", 100),
            "inbound quota is per-peer"
        );
    }

    #[test]
    fn gate_inbound_returns_true_for_unknown_feature() {
        let r = FeatureRegistry::new();
        assert!(r.gate_inbound_through_feature("unknown.feature.v1", "peer-a", 99_999));
    }

    #[test]
    fn room_chat_v1_outbound_quota_and_clear() {
        let r = FeatureRegistry::new();
        r.register(wellknown::room_chat_v1()).unwrap();

        // Exhaust chat quota
        for _ in 0..100 {
            let _ = r.gate_through_feature("room.chat.v1", "chat-peer", 400);
        }
        assert!(
            !r.gate_through_feature("room.chat.v1", "chat-peer", 10),
            "room.chat.v1 should be blocked"
        );

        r.clear_peer_outbound_quotas("chat-peer");
        assert!(
            r.gate_through_feature("room.chat.v1", "chat-peer", 100),
            "quota should reset after clear"
        );
    }
}
