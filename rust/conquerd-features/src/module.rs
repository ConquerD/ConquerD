//! `FeatureModule` trait — the dispatch contract for capability-bound features.
//!
//! A `FeatureModule` is the in-process implementation of a capability id.
//! The framework uses the trait to:
//!
//! 1. Collect descriptors for advertisement (via [`FeatureModule::descriptor`]).
//! 2. Notify the module when a peer invokes the capability
//!    ([`FeatureModule::on_invoke`]).
//! 3. Tear the module down on session end ([`FeatureModule::shutdown`]).
//!
//! This module deliberately stays sync and I/O-free so the trait works
//! identically from blocking, async, FFI, and (eventually) WASM
//! consumers. Implementations spawn their own tasks if needed.
//!
//! Concrete transport binding (opening a QUIC stream, allocating a
//! channel tag, etc.) is the responsibility of the consuming crate;
//! `FeatureModule` only sees the resulting [`InvocationContext`].

use serde_json::Value;
use std::sync::Arc;

use crate::descriptor::CapabilityDescriptor;

/// Stable identifier for a remote peer (base64url Ed25519 public key).
pub type PeerId = String;

/// Context handed to a module when its capability is invoked.
///
/// Today this is intentionally minimal — it carries only the identity
/// of the caller and the params they sent. Phase 3 will extend this
/// with a `Channel` handle (datagram tag or stream id) once
/// `conquerd-quic` exposes the multiplexer API.
#[derive(Debug, Clone)]
pub struct InvocationContext {
    /// Peer that invoked the capability.
    pub peer: PeerId,
    /// `params` field from the `CAPABILITY_INVOKE` envelope.
    pub params: Value,
    /// Channel-tag bound for `Datagram` features, `None` for streams.
    pub channel_tag: Option<u8>,
}

/// Result type for module operations.
pub type ModuleResult<T> = Result<T, ModuleError>;

/// Errors a module may return from `on_invoke` / `on_message`.
#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("internal module error: {0}")]
    Internal(String),
}

/// In-process implementation of a capability id.
///
/// Implementations must be `Send + Sync` so the framework can dispatch
/// from any task. Consumers typically wrap modules in [`Arc`].
pub trait FeatureModule: Send + Sync {
    /// Return the descriptor advertised for this module. The `id` and
    /// `version` here are the negotiation key.
    fn descriptor(&self) -> CapabilityDescriptor;

    /// Called when a peer sends `CAPABILITY_INVOKE` for this module.
    /// Default impl rejects the invocation as unimplemented.
    fn on_invoke(&self, _ctx: InvocationContext) -> ModuleResult<()> {
        Err(ModuleError::Internal("on_invoke not implemented".into()))
    }

    /// Called when a transport delivers an inbound feature payload
    /// for this module. Unlike [`on_invoke`] (one-shot capability
    /// activation) this is the per-message hot path and is invoked
    /// once per inbound datagram or stream message routed to this
    /// feature id.
    ///
    /// `source` is the verified peer id that produced the payload
    /// (see [`crate::FeatureRegistry::dispatch_message`]). Payload
    /// framing (envelope / raw) is module-defined; the framework only
    /// guarantees that capability gates and quotas were applied before
    /// dispatch.
    ///
    /// Default impl is a no-op so descriptor-only registrations
    /// (modules used purely for advertisement) compile without
    /// boilerplate.
    fn on_message(&self, _source: PeerId, _payload: &[u8]) {}

    /// Called when the framework is tearing the session down.
    fn shutdown(&self) {}
}

/// Convenience alias for the boxed/shared form modules are stored as.
pub type SharedModule = Arc<dyn FeatureModule>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::ChannelKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingModule {
        invocations: AtomicUsize,
    }

    impl FeatureModule for CountingModule {
        fn descriptor(&self) -> CapabilityDescriptor {
            CapabilityDescriptor::new("x.test.counter", "1.0", ChannelKind::Stream)
        }
        fn on_invoke(&self, _ctx: InvocationContext) -> ModuleResult<()> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn module_dispatch_via_trait_object() {
        let m: SharedModule = Arc::new(CountingModule {
            invocations: AtomicUsize::new(0),
        });
        let ctx = InvocationContext {
            peer: "peer-a".into(),
            params: Value::Null,
            channel_tag: None,
        };
        m.on_invoke(ctx.clone()).unwrap();
        m.on_invoke(ctx).unwrap();
        // Cast back to access counter only for the test.
        let raw = Arc::into_raw(m) as *const CountingModule;
        let count = unsafe { (*raw).invocations.load(Ordering::SeqCst) };
        // Re-wrap so Arc drops correctly.
        let _ = unsafe { Arc::from_raw(raw) };
        assert_eq!(count, 2);
    }

    #[test]
    fn default_on_invoke_rejects() {
        struct Stub;
        impl FeatureModule for Stub {
            fn descriptor(&self) -> CapabilityDescriptor {
                CapabilityDescriptor::new("x.test.stub", "1.0", ChannelKind::Stream)
            }
        }
        let s = Stub;
        let err = s
            .on_invoke(InvocationContext {
                peer: "p".into(),
                params: Value::Null,
                channel_tag: None,
            })
            .unwrap_err();
        assert!(matches!(err, ModuleError::Internal(_)));
    }
}
