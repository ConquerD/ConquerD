//! `room.audio.sfu` and `room.chat.v1` as `FeatureModule` implementations.
//!
//! These modules sit on the `conquerd-features` registry and own the
//! verify+enumerate+forward pipeline that previously lived as an
//! ad-hoc closure in `main.rs`. The WebTransport bridge dispatches
//! room.* payloads to [`crate::webtransport::ModuleNativeDispatcher`],
//! which calls a single hook that resolves to
//! `state.features.dispatch_message(feature_id, source, payload)` —
//! turning the per-message hot path into a proper framework dispatch.
//!
//! The module holds a `Weak<SupernodeState>` so the registry → module
//! chain does not pin the state Arc.

use std::sync::Weak;

use conquerd_features::{wellknown, CapabilityDescriptor, FeatureModule, PeerId};

use crate::protocol::verify_browser_envelope;
use crate::SupernodeState;

/// One module instance covers either `room.audio.sfu` (audio frames)
/// or `room.chat.v1` (chat envelopes). The descriptor and member-list
/// strategy differ; everything else (verify + per-member signaling
/// send) is identical.
pub struct SfuRoomModule {
    state: Weak<SupernodeState>,
    feature_id: &'static str,
    members_kind: MembersKind,
}

#[derive(Clone, Copy)]
enum MembersKind {
    Audio,
    Chat,
}

impl SfuRoomModule {
    pub fn audio(state: Weak<SupernodeState>) -> Self {
        Self {
            state,
            feature_id: "room.audio.sfu",
            members_kind: MembersKind::Audio,
        }
    }

    pub fn chat(state: Weak<SupernodeState>) -> Self {
        Self {
            state,
            feature_id: "room.chat.v1",
            members_kind: MembersKind::Chat,
        }
    }
}

impl FeatureModule for SfuRoomModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        match self.members_kind {
            MembersKind::Audio => wellknown::room_audio_sfu(),
            MembersKind::Chat => wellknown::room_chat_v1(),
        }
    }

    fn on_message(&self, source: PeerId, payload: &[u8]) {
        let Some(state) = self.state.upgrade() else {
            return;
        };

        // The dispatcher already confirmed the source has a declared
        // room; if the session vanished between dispatch and now,
        // there is nothing to forward to.
        let Some(room_id) = state.web_bridge.session_room(&source) else {
            return;
        };

        // Browser pre-signs a full SignalingMessage; reject anything
        // that doesn't verify against the captured Ed25519 identity.
        let identity = state.web_bridge.session_identity(&source);
        if verify_browser_envelope(identity.as_deref(), payload).is_none() {
            tracing::debug!(
                "[{}] drop unverified envelope: peer={} bytes={}",
                self.feature_id,
                source,
                payload.len()
            );
            return;
        }

        // Re-decoding to &str is safe here: `verify_browser_envelope`
        // already validated UTF-8.
        let raw = match std::str::from_utf8(payload) {
            Ok(s) => s,
            Err(_) => return,
        };

        let Some(ref sfu) = state.sfu else { return };
        let members = match self.members_kind {
            MembersKind::Audio => sfu.read().get_room_members(&room_id),
            MembersKind::Chat => sfu.read().get_chat_recipients(&room_id),
        };
        for native in members {
            // Browser source ids are base64url Ed25519 pubkeys; native
            // ids share the same space, so skip the source itself in
            // case a participant happens to be reachable both ways.
            if native == source {
                continue;
            }
            state.signaling.send_to_peer(&native, raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conquerd_features::wellknown;

    #[test]
    fn audio_module_descriptor_matches_wellknown() {
        let m = SfuRoomModule::audio(Weak::new());
        let d = m.descriptor();
        let expected = wellknown::room_audio_sfu();
        assert_eq!(d.id, expected.id);
        assert_eq!(d.version, expected.version);
    }

    #[test]
    fn chat_module_descriptor_matches_wellknown() {
        let m = SfuRoomModule::chat(Weak::new());
        let d = m.descriptor();
        let expected = wellknown::room_chat_v1();
        assert_eq!(d.id, expected.id);
        assert_eq!(d.version, expected.version);
    }

    #[test]
    fn on_message_with_dropped_state_is_safe_noop() {
        // Weak::new() always fails to upgrade — on_message must return
        // without panicking.
        let m = SfuRoomModule::audio(Weak::new());
        m.on_message("peer-a".into(), b"garbage payload");
    }

    #[test]
    fn chat_on_message_with_dropped_state_is_safe_noop() {
        let m = SfuRoomModule::chat(Weak::new());
        m.on_message("peer-b".into(), b"{\"type\":\"chat\"}");
    }

    /// P0 smoke test (lightweight version): verifies that the two room
    /// FeatureModules can be constructed and that their descriptors match
    /// the well-known catalogue. Full registry dispatch smoke lives in
    /// main.rs tests (where SupernodeState is visible).
    #[test]
    fn smoke_room_module_construction() {
        let _audio = SfuRoomModule::audio(Weak::new());
        let _chat = SfuRoomModule::chat(Weak::new());
        // If we got here without panic, the basic module wiring for
        // room.audio.sfu + room.chat.v1 is alive.
    }
}
