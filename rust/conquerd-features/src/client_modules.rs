//! First-party client-side `FeatureModule` implementations.
//!
//! Each struct corresponds to a capability the Conquerd desktop client
//! implements natively.  Modules hold an optional callback hook so any Rust
//! consumer (tests, future native client) can observe or intercept inbound
//! payloads without subclassing.
//!
//! `core.audio.opus` is advertisement-only: the actual audio datagram
//! pipeline runs through `conquerd_quic::QUICTransport::on_audio_datagram`
//! for latency reasons and does not route through `on_message`.

use std::sync::Arc;

use crate::{
    module::{FeatureModule, PeerId},
    registry::FeatureRegistry,
    video_codec::VideoCodec,
    wellknown, CapabilityDescriptor, FeatureError,
};

/// Callback type used by the three client modules.
type MessageHook = Arc<dyn Fn(PeerId, &[u8]) + Send + Sync>;

// ── core.chat.v1 ─────────────────────────────────────────────────────────────

/// `core.chat.v1` — signed chat envelope on the signaling channel.
///
/// Routes `on_message` to an optional hook so consumers can observe
/// inbound chat payloads.  Rate-limiting and Qt signal emission live in
/// the `CoreChatModule`; this struct is the framework-visible
/// registration.
pub struct CoreChatModule {
    hook: Option<MessageHook>,
}

impl CoreChatModule {
    /// Create an advertisement-only module (no `on_message` hook).
    pub fn new() -> Self {
        Self { hook: None }
    }

    /// Create a module that forwards `on_message` to *hook*.
    pub fn with_hook(hook: impl Fn(PeerId, &[u8]) + Send + Sync + 'static) -> Self {
        Self {
            hook: Some(Arc::new(hook)),
        }
    }
}

impl Default for CoreChatModule {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureModule for CoreChatModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::core_chat_v1()
    }

    fn on_message(&self, source: PeerId, payload: &[u8]) {
        if let Some(ref hook) = self.hook {
            hook(source, payload);
        }
    }
}

// ── core.audio.opus ──────────────────────────────────────────────────────────

/// `core.audio.opus` — direct peer voice (Opus over QUIC datagrams).
///
/// Advertisement-only: the datagram pipeline runs through a dedicated
/// low-tag path (`send_audio_datagram`) for latency reasons (see the
/// detailed "Audio Dispatch Decision" comment in connection_manager.rs).
/// `on_message` is a no-op; the descriptor exists purely for capability
/// negotiation and quota definition. Outbound quota is still enforced
/// via `gate_through_feature` before every send.
pub struct CoreAudioOpusModule;

impl FeatureModule for CoreAudioOpusModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::core_audio_opus()
    }

    // on_message: inherits default no-op — audio routes through datagram callback.
}

// ── core.file.v1 ─────────────────────────────────────────────────────────────

/// `core.file.v1` — chunked file transfer on the signaling channel.
pub struct CoreFileModule {
    hook: Option<MessageHook>,
}

impl CoreFileModule {
    /// Create an advertisement-only module (no `on_message` hook).
    pub fn new() -> Self {
        Self { hook: None }
    }

    /// Create a module that forwards `on_message` to *hook*.
    pub fn with_hook(hook: impl Fn(PeerId, &[u8]) + Send + Sync + 'static) -> Self {
        Self {
            hook: Some(Arc::new(hook)),
        }
    }
}

impl Default for CoreFileModule {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureModule for CoreFileModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::core_file_v1()
    }

    fn on_message(&self, source: PeerId, payload: &[u8]) {
        if let Some(ref hook) = self.hook {
            hook(source, payload);
        }
    }
}

// ── Convenience: register all three into a registry ──────────────────────────

/// `room.file.v1` — advertisement-only descriptor for SFU room file broadcasts.
pub struct RoomFileModule;

impl FeatureModule for RoomFileModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::room_file_v1()
    }
}

/// `room.audio.sfu` — SFU room voice relayed via supernode signaling.
///
/// Advertisement-only on the desktop client: inbound `SfuAudio` frames are
/// decoded in `connection_manager` and gated through
/// `FeatureRegistry::dispatch_message` for per-sender quota enforcement
/// before the call controller sees them. Outbound room audio is gated through
/// `room.audio.sfu` in `send_room_audio`.
pub struct RoomAudioSfuModule;

impl FeatureModule for RoomAudioSfuModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::room_audio_sfu()
    }

    // on_message: inherits default no-op — audio is handled in connection_manager.
}

/// `core.video.v1` — direct peer video over QUIC datagrams.
///
/// Advertisement-only, for the same reason as [`CoreAudioOpusModule`]: video
/// fragments ride a dedicated low-tag datagram path rather than the generic
/// feature-datagram multiplexer. The descriptor exists for capability
/// negotiation and quota definition; outbound sends are still gated through
/// `gate_through_feature`.
///
/// Carries the codec set this build can run, because that is what the peer
/// negotiates against — see [`crate::video_codec`].
pub struct CoreVideoModule {
    codecs: Vec<VideoCodec>,
}

impl CoreVideoModule {
    /// Advertise exactly `codecs`. Callers should pass what the local build
    /// can really encode and decode, not the full known set.
    pub fn new(codecs: Vec<VideoCodec>) -> Self {
        Self { codecs }
    }
}

impl FeatureModule for CoreVideoModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::core_video_v1_for(&self.codecs)
    }

    // on_message: inherits default no-op — video routes through datagram callback.
}

/// `room.video.sfu` — SFU room video relayed via supernode.
///
/// Advertisement-only, mirroring [`RoomAudioSfuModule`]. Inbound video
/// fragments are reassembled in `connection_manager` and gated through
/// `FeatureRegistry::dispatch_message` for per-sender quota enforcement;
/// outbound fragments are gated in `send_room_video`.
pub struct RoomVideoSfuModule {
    codecs: Vec<VideoCodec>,
}

impl RoomVideoSfuModule {
    /// See [`CoreVideoModule::new`].
    pub fn new(codecs: Vec<VideoCodec>) -> Self {
        Self { codecs }
    }
}

impl FeatureModule for RoomVideoSfuModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::room_video_sfu_for(&self.codecs)
    }

    // on_message: inherits default no-op — video is handled in connection_manager.
}

/// `core.audio.content.v1` — direct peer content audio.
///
/// Advertisement-only, like the other media descriptors: the datagram path is
/// dedicated rather than routed through the generic multiplexer. Registering it
/// is what makes the client advertise `av_sync`, which is how a peer learns it
/// can synchronise video against this stream.
pub struct CoreContentAudioModule;

impl FeatureModule for CoreContentAudioModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::core_audio_content_v1()
    }
}

/// `room.audio.content.sfu` — content audio relayed via supernode.
pub struct RoomContentAudioModule;

impl FeatureModule for RoomContentAudioModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::room_audio_content_sfu()
    }
}

/// `room.chat.v1` — advertisement-only descriptor for SFU room text chat.
///
/// Advertisement-only on the desktop client: inbound `SfuChat` messages are
/// surfaced in `connection_manager` and gated through `room.chat.v1` for
/// per-sender quota enforcement; outbound room chat is gated through
/// `room.chat.v1` in `dispatch_outbound`. Registering the descriptor here is
/// what makes the client advertise the capability in `CAPABILITY_ANNOUNCE`
/// and gives the quota gates a descriptor to meter against.
pub struct RoomChatModule;

impl FeatureModule for RoomChatModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        wellknown::room_chat_v1()
    }
}

/// Register the first-party client modules into *registry*.
///
/// Descriptors are taken from their `wellknown` constructors.  All use the
/// no-hook (advertisement-only) variant.  Callers that need `on_message`
/// dispatch should use the `with_hook` constructors directly.
///
/// Video advertises the full known codec set. A real client knows which codecs
/// its platform and build actually provide and **must** use
/// [`register_client_modules_with_video_codecs`] instead — advertising a codec
/// this build cannot run is exactly the dishonesty the codec-neutral capability
/// id exists to prevent.
///
/// Returns `Err` if any registration fails (e.g. duplicate id).
pub fn register_client_modules(registry: &FeatureRegistry) -> Result<(), FeatureError> {
    register_client_modules_with_video_codecs(registry, crate::video_codec::PREFERENCE.to_vec())
}

/// [`register_client_modules`], advertising only the video codecs in
/// `video_codecs`.
///
/// Pass an empty vec on a platform with no encoder or decoder: the video
/// descriptors are still registered (so quota gates have something to meter
/// against) but advertise no codecs, and a peer negotiating against them gets
/// `None` and does not send video.
pub fn register_client_modules_with_video_codecs(
    registry: &FeatureRegistry,
    video_codecs: Vec<VideoCodec>,
) -> Result<(), FeatureError> {
    let modules: Vec<crate::module::SharedModule> = vec![
        Arc::new(CoreChatModule::new()),
        Arc::new(CoreAudioOpusModule),
        Arc::new(CoreFileModule::new()),
        Arc::new(RoomFileModule),
        Arc::new(RoomAudioSfuModule),
        Arc::new(CoreVideoModule::new(video_codecs.clone())),
        Arc::new(RoomVideoSfuModule::new(video_codecs)),
        Arc::new(RoomChatModule),
        Arc::new(CoreContentAudioModule),
        Arc::new(RoomContentAudioModule),
    ];
    for m in modules {
        registry.register_module(m)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_client_modules_succeeds() {
        let reg = FeatureRegistry::new();
        register_client_modules(&reg).expect("registration should succeed");
        assert!(reg.get("core.chat.v1").is_some());
        assert!(reg.get("core.audio.opus").is_some());
        assert!(reg.get("core.file.v1").is_some());
        assert!(reg.get("room.file.v1").is_some());
        assert!(reg.get("room.audio.sfu").is_some());
        assert!(reg.get("core.video.v1").is_some());
        assert!(reg.get("room.video.sfu").is_some());
        assert!(reg.get("room.chat.v1").is_some());
        assert!(reg.get("core.audio.content.v1").is_some());
        assert!(reg.get("room.audio.content.sfu").is_some());
    }

    /// Content audio must advertise `av_sync`: it is the signal a peer uses to
    /// decide whether it can slave video to this stream, and without it the
    /// receiver correctly falls back to free-running video.
    #[test]
    fn content_audio_advertises_av_sync() {
        let reg = FeatureRegistry::new();
        register_client_modules(&reg).unwrap();
        for id in ["core.audio.content.v1", "room.audio.content.sfu"] {
            let desc = reg.get(id).expect("registered");
            assert_eq!(desc.params.get("av_sync").and_then(|v| v.as_u64()), Some(1));
            assert_eq!(
                desc.params.get("pts_unit").and_then(|v| v.as_str()),
                Some("us")
            );
        }
    }

    #[test]
    fn register_client_modules_fails_on_duplicate() {
        let reg = FeatureRegistry::new();
        register_client_modules(&reg).unwrap();
        // Second call must fail — all ids already registered.
        assert!(register_client_modules(&reg).is_err());
    }

    #[test]
    fn chat_module_with_hook_fires() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&counter);
        let m = CoreChatModule::with_hook(move |_src, _payload| {
            c2.fetch_add(1, Ordering::SeqCst);
        });
        m.on_message("peer-a".into(), b"hello");
        m.on_message("peer-a".into(), b"world");
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn audio_module_is_advertisement_only() {
        // on_message is a no-op — just verify it doesn't panic.
        let m = CoreAudioOpusModule;
        m.on_message("peer-a".into(), b"\x00\x01opus-data");
    }

    #[test]
    fn room_audio_sfu_module_is_advertisement_only() {
        let m = RoomAudioSfuModule;
        m.on_message("peer-a".into(), b"opus-frame");
    }

    #[test]
    fn room_audio_sfu_inbound_quota_enforced_via_dispatch() {
        let reg = FeatureRegistry::new();
        reg.register_module(Arc::new(RoomAudioSfuModule)).unwrap();
        // room.audio.sfu: 200 datagrams/s burst bucket.
        for _ in 0..200 {
            assert!(reg.dispatch_message("room.audio.sfu", "peer-a".into(), b"x"));
        }
        assert!(
            !reg.dispatch_message("room.audio.sfu", "peer-a".into(), b"x"),
            "inbound room.audio.sfu quota should exhaust after burst"
        );
    }

    #[test]
    fn file_module_with_hook_fires() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&counter);
        let m = CoreFileModule::with_hook(move |_src, _payload| {
            c2.fetch_add(1, Ordering::SeqCst);
        });
        m.on_message("peer-b".into(), b"chunk-data");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn descriptors_match_wellknown() {
        assert_eq!(CoreChatModule::new().descriptor().id, "core.chat.v1");
        assert_eq!(CoreAudioOpusModule.descriptor().id, "core.audio.opus");
        assert_eq!(CoreFileModule::new().descriptor().id, "core.file.v1");
        assert_eq!(RoomAudioSfuModule.descriptor().id, "room.audio.sfu");
        assert_eq!(
            CoreVideoModule::new(vec![VideoCodec::H264]).descriptor().id,
            "core.video.v1"
        );
        assert_eq!(
            RoomVideoSfuModule::new(vec![VideoCodec::H264])
                .descriptor()
                .id,
            "room.video.sfu"
        );
    }

    /// The whole point of the codec-neutral id: what a build advertises must
    /// be what it can actually run.
    #[test]
    fn video_modules_advertise_only_the_codecs_they_were_given() {
        let reg = FeatureRegistry::new();
        register_client_modules_with_video_codecs(&reg, vec![VideoCodec::H264]).unwrap();
        for id in ["core.video.v1", "room.video.sfu"] {
            let desc = reg.get(id).expect("registered");
            assert_eq!(
                crate::video_codec::codecs_from_params(&desc.params),
                vec![VideoCodec::H264],
                "{id} should advertise exactly the codec it was given"
            );
        }
    }

    /// A platform with no video backend still registers the descriptors (the
    /// quota gates need them) but must advertise an empty codec set, so a peer
    /// negotiates `None` rather than sending frames nothing can decode.
    #[test]
    fn a_platform_with_no_codecs_advertises_none() {
        let reg = FeatureRegistry::new();
        register_client_modules_with_video_codecs(&reg, vec![]).unwrap();
        let desc = reg.get("core.video.v1").expect("still registered");
        assert!(crate::video_codec::codecs_from_params(&desc.params).is_empty());
    }

    #[test]
    fn the_stub_codec_is_never_advertised_even_if_available() {
        let reg = FeatureRegistry::new();
        register_client_modules_with_video_codecs(&reg, vec![VideoCodec::H264, VideoCodec::Stub])
            .unwrap();
        let desc = reg.get("core.video.v1").unwrap();
        assert_eq!(
            crate::video_codec::codecs_from_params(&desc.params),
            vec![VideoCodec::H264]
        );
    }
}
