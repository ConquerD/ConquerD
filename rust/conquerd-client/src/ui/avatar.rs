//! Qt/QML avatar re-exports.
//!
//! All logic lives in `crate::avatar_config` (always compiled) so that
//! `AvatarConfig` is available to `peer_store` regardless of feature flags.
//! This module merely re-exports the public surface for use by the bridge.

pub use crate::avatar_config::{
    avatar_tint_hex, build_avatar_svg, pattern_for_peer, pattern_to_json, AvatarConfig,
    AvatarPattern,
};
