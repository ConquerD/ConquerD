//! conquerd-features — capability registry and feature module spine.
//!
//! This crate is the language-agnostic core of the Conquerd peer-connectivity
//! framework. It defines:
//!
//! * [`CapabilityDescriptor`] — a self-describing, on-wire feature record
//!   advertised by peers and supernodes after handshake.
//! * [`FeatureRegistry`] — an in-process registry that owns the set of
//!   advertised capabilities.
//! * Reverse-DNS-style namespace helpers for the reserved `core.*`,
//!   `transport.*`, `room.*`, `web.*`, `game.*`, and `x.<vendor>.*` prefixes.
//!
//! The crate is intentionally transport-agnostic: it does not depend on
//! `quinn` or any I/O. Higher-level crates (`conquerd-quic`,
//! `conquerd-supernode`, the desktop client) consume it to wire feature
//! negotiation onto their own message pipelines.
//!
//! See `FRAMEWORK_PLAN.md` at the repo root for the full design.

pub mod channel_frame;
pub mod channel_tag;
pub mod client_modules;
pub mod descriptor;
pub mod examples;
pub mod loader;
pub mod module;
pub mod quota;
pub mod registry;
pub mod replay;
pub mod signing;
pub mod web_app;
pub mod wellknown;

pub use channel_frame::{
    classify, decode_frame, encode_frame, feature_for_fixed_tag, fixed_tag_for, FrameClass,
    AUDIO_TAG, CHAT_TAG, CONTROL_TAG, FILE_TAG, MAX_FIRST_PARTY_TAG,
};
pub use channel_tag::{ChannelTagError, ChannelTagRegistry};
pub use client_modules::register_client_modules;
pub use descriptor::{AuthTier, CapabilityDescriptor, ChannelKind, FeatureError};
pub use examples::{register_example_modules, x_conquerd_matchmaker_v1, Matchmaker, MATCHMAKER_ID};
pub use loader::{ConquerdModuleVtable, LoadError, NativeModuleLoader, TrustRequest, ABI_VERSION};
pub use module::{
    FeatureModule, InvocationContext, ModuleError, ModuleResult, PeerId, SharedModule,
};
pub use quota::{QuotaParams, QuotaRegistry, DEFAULT_BYTES_PER_SEC, DEFAULT_DATAGRAMS_PER_SEC};
pub use registry::FeatureRegistry;
pub use replay::{ReplayGuard, DEFAULT_REPLAY_WINDOW_SECS};
pub use signing::{
    sign_manifest, ManifestCapability, ModuleManifest, SigningError, TrustedKeyStore,
    MANIFEST_SCHEMA_VERSION,
};
