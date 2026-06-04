//! Channel-tag registry for the QUIC datagram multiplexer.
//!
//! When a feature is invoked with [`ChannelKind::Datagram`], it is bound
//! to a 1-byte tag at the head of every datagram on the shared QUIC
//! connection. This module owns the per-session tag allocation so that
//! multiple features (`core.audio.opus`, `game.netcode.lockstep.v1`,
//! telemetry, etc.) can share one connection without colliding.
//!
//! Tag layout (1 byte):
//!
//! | range        | use                                                     |
//! |--------------|---------------------------------------------------------|
//! | `0x00..0x0F` | reserved transport tags (audio/relay/system) — fixed    |
//! | `0x10..0xEF` | dynamically allocated per session for feature datagrams |
//! | `0xF0..0xFE` | reserved for future framework use                       |
//! | `0xFF`       | relay broadcast (legacy `transport.quic.relay.v1`)      |
//!
//! Allocation is FIFO over the dynamic range; tags are returned to the
//! pool on feature shutdown. The registry is `!Send`-free (uses
//! `parking_lot::Mutex`) so it can be shared across async tasks.
//!
//! [`ChannelKind::Datagram`]: crate::ChannelKind::Datagram

use parking_lot::Mutex;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// First tag in the dynamically-allocated range.
pub const DYNAMIC_TAG_START: u8 = 0x10;
/// Last tag in the dynamically-allocated range (inclusive).
pub const DYNAMIC_TAG_END: u8 = 0xEF;
/// Legacy broadcast tag from `transport.quic.relay.v1`.
pub const BROADCAST_TAG: u8 = 0xFF;

/// Errors raised by the channel-tag registry.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChannelTagError {
    #[error("channel tag space exhausted (256 tags max per session)")]
    Exhausted,
    #[error("feature `{0}` already has a channel tag bound")]
    AlreadyBound(String),
    #[error("no channel tag bound for feature `{0}`")]
    NotBound(String),
    #[error("channel tag 0x{0:02X} is outside the dynamic range (0x10..=0xEF)")]
    OutOfRange(u8),
    #[error("channel tag 0x{0:02X} is already bound to another feature")]
    TagInUse(u8),
}

/// Per-session channel-tag allocator.
#[derive(Default)]
pub struct ChannelTagRegistry {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// feature_id → tag
    by_feature: BTreeMap<String, u8>,
    /// tag → feature_id (reverse lookup for incoming datagrams)
    by_tag: BTreeMap<u8, String>,
    /// Tags currently in use within the dynamic range.
    in_use: BTreeSet<u8>,
}

impl ChannelTagRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a fresh tag in the dynamic range for a feature.
    /// Returns the assigned tag.
    pub fn bind(&self, feature_id: &str) -> Result<u8, ChannelTagError> {
        let mut g = self.inner.lock();
        if g.by_feature.contains_key(feature_id) {
            return Err(ChannelTagError::AlreadyBound(feature_id.to_string()));
        }
        let tag = (DYNAMIC_TAG_START..=DYNAMIC_TAG_END)
            .find(|t| !g.in_use.contains(t))
            .ok_or(ChannelTagError::Exhausted)?;
        g.in_use.insert(tag);
        g.by_feature.insert(feature_id.to_string(), tag);
        g.by_tag.insert(tag, feature_id.to_string());
        Ok(tag)
    }

    /// Bind a feature to a caller-specified tag (e.g. the tag chosen by the
    /// remote peer in a `CAPABILITY_INVOKE` message). The tag must be in the
    /// dynamic range and must not already be in use.
    pub fn bind_to(&self, feature_id: &str, tag: u8) -> Result<(), ChannelTagError> {
        if !(DYNAMIC_TAG_START..=DYNAMIC_TAG_END).contains(&tag) {
            return Err(ChannelTagError::OutOfRange(tag));
        }
        let mut g = self.inner.lock();
        if g.by_feature.contains_key(feature_id) {
            return Err(ChannelTagError::AlreadyBound(feature_id.to_string()));
        }
        if g.in_use.contains(&tag) {
            return Err(ChannelTagError::TagInUse(tag));
        }
        g.in_use.insert(tag);
        g.by_feature.insert(feature_id.to_string(), tag);
        g.by_tag.insert(tag, feature_id.to_string());
        Ok(())
    }

    /// Release the tag bound to a feature.
    pub fn release(&self, feature_id: &str) -> Result<u8, ChannelTagError> {
        let mut g = self.inner.lock();
        let tag = g
            .by_feature
            .remove(feature_id)
            .ok_or_else(|| ChannelTagError::NotBound(feature_id.to_string()))?;
        g.by_tag.remove(&tag);
        g.in_use.remove(&tag);
        Ok(tag)
    }

    /// Lookup the tag bound to a feature.
    pub fn tag_for(&self, feature_id: &str) -> Option<u8> {
        self.inner.lock().by_feature.get(feature_id).copied()
    }

    /// Lookup the feature bound to a tag (for incoming datagram dispatch).
    pub fn feature_for(&self, tag: u8) -> Option<String> {
        self.inner.lock().by_tag.get(&tag).cloned()
    }

    /// Number of features currently bound to a dynamic tag.
    pub fn bound_count(&self) -> usize {
        self.inner.lock().by_feature.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_returns_first_dynamic_tag() {
        let r = ChannelTagRegistry::new();
        assert_eq!(r.bind("a.x").unwrap(), DYNAMIC_TAG_START);
        assert_eq!(r.bind("a.y").unwrap(), DYNAMIC_TAG_START + 1);
    }

    #[test]
    fn duplicate_bind_fails() {
        let r = ChannelTagRegistry::new();
        r.bind("a.x").unwrap();
        assert_eq!(
            r.bind("a.x").unwrap_err(),
            ChannelTagError::AlreadyBound("a.x".into())
        );
    }

    #[test]
    fn release_frees_tag_for_reuse() {
        let r = ChannelTagRegistry::new();
        let t = r.bind("a.x").unwrap();
        r.release("a.x").unwrap();
        // Next bind reuses the lowest free tag.
        assert_eq!(r.bind("a.y").unwrap(), t);
    }

    #[test]
    fn reverse_lookup_works() {
        let r = ChannelTagRegistry::new();
        let t = r.bind("game.foo.v1").unwrap();
        assert_eq!(r.feature_for(t).as_deref(), Some("game.foo.v1"));
        assert_eq!(r.tag_for("game.foo.v1"), Some(t));
    }

    #[test]
    fn release_unknown_fails() {
        let r = ChannelTagRegistry::new();
        assert_eq!(
            r.release("missing").unwrap_err(),
            ChannelTagError::NotBound("missing".into())
        );
    }

    #[test]
    fn exhaustion_returns_error() {
        let r = ChannelTagRegistry::new();
        let span = (DYNAMIC_TAG_END - DYNAMIC_TAG_START + 1) as usize;
        for i in 0..span {
            r.bind(&format!("f.{}", i)).unwrap();
        }
        assert_eq!(r.bound_count(), span);
        assert_eq!(
            r.bind("f.overflow").unwrap_err(),
            ChannelTagError::Exhausted
        );
    }
}
