//! Capability descriptor — the self-describing on-wire feature record.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Auth tier required to invoke a feature on a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AuthTier {
    /// Anyone connected may invoke (e.g. public web endpoints).
    Public,
    /// Only members of the same room/SFU group.
    RoomMember,
    /// Only peers the user has explicitly trusted (default).
    #[default]
    TrustedPeer,
}

/// Transport channel binding for a feature invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelKind {
    /// Unreliable QUIC datagram with a 1-byte channel tag.
    Datagram,
    /// Bidirectional QUIC stream (length-prefixed frames).
    Stream,
    /// Single-shot request/response on a short-lived stream.
    Request,
}

/// A capability descriptor as advertised over the wire.
///
/// The on-wire JSON shape is stable and language-agnostic:
///
/// ```json
/// {
///   "id": "core.audio.opus",
///   "version": "1.0",
///   "kind": "datagram",
///   "params": { "codec": "opus", "max_bitrate": 64000 },
///   "auth": "trusted-peer",
///   "experimental": false
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    /// Reverse-DNS-style id, e.g. `core.chat.v1`, `x.acme.matchmaker`.
    pub id: String,
    /// Semver-compatible version string. Negotiation is by major version.
    pub version: String,
    /// Transport binding requested when the feature is invoked.
    pub kind: ChannelKind,
    /// Free-form per-feature parameters (codec, bitrate, paths, etc.).
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
    /// Auth tier required to invoke.
    #[serde(default)]
    pub auth: AuthTier,
    /// Marks the feature as experimental — clients should warn the user.
    #[serde(default, skip_serializing_if = "is_false")]
    pub experimental: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl CapabilityDescriptor {
    /// Construct a new descriptor with no params and default auth tier.
    pub fn new(id: impl Into<String>, version: impl Into<String>, kind: ChannelKind) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
            kind,
            params: Value::Null,
            auth: AuthTier::default(),
            experimental: false,
        }
    }

    /// Builder: attach free-form params.
    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }

    /// Builder: set the auth tier.
    pub fn with_auth(mut self, auth: AuthTier) -> Self {
        self.auth = auth;
        self
    }

    /// Builder: mark as experimental.
    pub fn experimental(mut self) -> Self {
        self.experimental = true;
        self
    }

    /// Reverse-DNS namespace (the part before the first `.`).
    /// Returns `""` if the id has no namespace prefix.
    pub fn namespace(&self) -> &str {
        self.id.split('.').next().unwrap_or("")
    }

    /// Major version, parsed from the leading `<u32>` of `version`.
    /// Returns `None` if the version cannot be parsed.
    pub fn major_version(&self) -> Option<u32> {
        self.version.split('.').next()?.parse().ok()
    }

    /// True iff `self` and `other` share the same id and major version.
    /// This is the negotiation rule used between peers.
    pub fn is_compatible_with(&self, other: &CapabilityDescriptor) -> bool {
        if self.id != other.id {
            return false;
        }
        match (self.major_version(), other.major_version()) {
            (Some(a), Some(b)) => a == b,
            _ => self.version == other.version,
        }
    }

    /// Validate basic well-formedness. Returns `Err` for empty or
    /// obviously invalid ids/versions.
    pub fn validate(&self) -> Result<(), FeatureError> {
        if self.id.is_empty() {
            return Err(FeatureError::InvalidId("empty id".into()));
        }
        if self.id.contains(char::is_whitespace) {
            return Err(FeatureError::InvalidId(
                "id must not contain whitespace".into(),
            ));
        }
        if !self.id.contains('.') {
            return Err(FeatureError::InvalidId(
                "id must use reverse-DNS form (namespace.subname)".into(),
            ));
        }
        if self.version.is_empty() {
            return Err(FeatureError::InvalidVersion("empty version".into()));
        }
        Ok(())
    }
}

/// Errors raised by the features layer.
#[derive(Debug, Error)]
pub enum FeatureError {
    #[error("invalid capability id: {0}")]
    InvalidId(String),
    #[error("invalid capability version: {0}")]
    InvalidVersion(String),
    #[error("capability {0} already registered")]
    Duplicate(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trips_json() {
        let cap = CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream)
            .with_params(json!({ "max_message_bytes": 65536 }))
            .with_auth(AuthTier::TrustedPeer);
        let s = serde_json::to_string(&cap).unwrap();
        let back: CapabilityDescriptor = serde_json::from_str(&s).unwrap();
        assert_eq!(cap, back);
    }

    #[test]
    fn skips_default_fields() {
        let cap = CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream);
        let s = serde_json::to_string(&cap).unwrap();
        // experimental + null params are omitted from the wire form.
        assert!(!s.contains("experimental"));
        assert!(!s.contains("params"));
    }

    #[test]
    fn major_version_negotiation() {
        let a = CapabilityDescriptor::new("core.chat.v1", "1.3", ChannelKind::Stream);
        let b = CapabilityDescriptor::new("core.chat.v1", "1.7", ChannelKind::Stream);
        let c = CapabilityDescriptor::new("core.chat.v1", "2.0", ChannelKind::Stream);
        assert!(a.is_compatible_with(&b));
        assert!(!a.is_compatible_with(&c));
    }

    #[test]
    fn validate_rejects_bad_ids() {
        assert!(CapabilityDescriptor::new("", "1.0", ChannelKind::Stream)
            .validate()
            .is_err());
        assert!(
            CapabilityDescriptor::new("nodots", "1.0", ChannelKind::Stream)
                .validate()
                .is_err()
        );
        assert!(
            CapabilityDescriptor::new("has space.x", "1.0", ChannelKind::Stream)
                .validate()
                .is_err()
        );
        assert!(
            CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn namespace_extraction() {
        let cap = CapabilityDescriptor::new("x.acme.matchmaker", "1.0", ChannelKind::Request);
        assert_eq!(cap.namespace(), "x");
    }

    // --- Capability negotiation version-skew tests ---

    #[test]
    fn different_ids_are_never_compatible() {
        let a = CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream);
        let b = CapabilityDescriptor::new("core.file.v1", "1.0", ChannelKind::Stream);
        assert!(!a.is_compatible_with(&b));
    }

    #[test]
    fn malformed_version_falls_back_to_string_equality() {
        // Non-numeric versions: major_version() returns None → equality check.
        let a = CapabilityDescriptor::new("x.test.thing", "preview", ChannelKind::Request);
        let b = CapabilityDescriptor::new("x.test.thing", "preview", ChannelKind::Request);
        let c = CapabilityDescriptor::new("x.test.thing", "beta", ChannelKind::Request);
        assert!(
            a.is_compatible_with(&b),
            "same non-numeric version must match"
        );
        assert!(
            !a.is_compatible_with(&c),
            "different non-numeric versions must not match"
        );
    }

    #[test]
    fn major_version_parses_correctly() {
        let a = CapabilityDescriptor::new("x.test.v", "3.14", ChannelKind::Request);
        assert_eq!(a.major_version(), Some(3));
        let b = CapabilityDescriptor::new("x.test.v", "notanumber", ChannelKind::Request);
        assert_eq!(b.major_version(), None);
        let c = CapabilityDescriptor::new("x.test.v", "0.9", ChannelKind::Request);
        assert_eq!(c.major_version(), Some(0));
    }

    #[test]
    fn same_major_different_minor_is_compatible() {
        let a = CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream);
        let b = CapabilityDescriptor::new("core.chat.v1", "1.99", ChannelKind::Stream);
        assert!(a.is_compatible_with(&b));
        assert!(b.is_compatible_with(&a));
    }

    #[test]
    fn different_major_same_minor_is_incompatible() {
        let a = CapabilityDescriptor::new("core.chat.v1", "1.5", ChannelKind::Stream);
        let b = CapabilityDescriptor::new("core.chat.v1", "2.0", ChannelKind::Stream);
        let c = CapabilityDescriptor::new("core.chat.v1", "3.5", ChannelKind::Stream);
        assert!(!a.is_compatible_with(&b));
        assert!(!b.is_compatible_with(&c));
    }

    #[test]
    fn empty_intersection_when_no_compatible_caps() {
        // Simulate peer A with chat v1 and audio v2; peer B with chat v2 and audio v2.
        // Only audio v2 should appear in the negotiated intersection.
        let local = [
            CapabilityDescriptor::new("core.chat.v1", "1.0", ChannelKind::Stream),
            CapabilityDescriptor::new("core.audio.opus", "2.0", ChannelKind::Datagram),
        ];
        let remote = [
            CapabilityDescriptor::new("core.chat.v1", "2.0", ChannelKind::Stream),
            CapabilityDescriptor::new("core.audio.opus", "2.0", ChannelKind::Datagram),
        ];
        let intersection: Vec<_> = local
            .iter()
            .filter(|l| remote.iter().any(|r| l.is_compatible_with(r)))
            .collect();
        assert_eq!(
            intersection.len(),
            1,
            "only audio should survive negotiation"
        );
        assert_eq!(intersection[0].id, "core.audio.opus");
    }

    #[test]
    fn fully_empty_intersection() {
        let local = [CapabilityDescriptor::new(
            "core.chat.v1",
            "1.0",
            ChannelKind::Stream,
        )];
        let remote = [CapabilityDescriptor::new(
            "core.chat.v1",
            "2.0",
            ChannelKind::Stream,
        )];
        let intersection: Vec<_> = local
            .iter()
            .filter(|l| remote.iter().any(|r| l.is_compatible_with(r)))
            .collect();
        assert!(
            intersection.is_empty(),
            "incompatible major versions → empty intersection"
        );
    }
}
