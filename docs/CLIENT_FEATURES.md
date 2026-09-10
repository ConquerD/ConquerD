# Client Feature Reference

The desktop client is the reference implementation. This document says what it
does, feature by feature, so a second client — Android today, others later —
can be measured against it rather than guessed at.

It is deliberately *not* an architecture document. [ARCHITECTURE.md](ARCHITECTURE.md)
explains how the pieces fit and how data flows; [ANDROID.md](ANDROID.md) covers
the JNI boundary and the Android build. This one answers: **what can a user do,
what does the core already provide, and does my client have it yet?**

## How to read it

Every feature area lists three things:

| Field | Meaning |
|---|---|
| **Core** | The module in `rust/conquerd-client/src/`. Shared by every client — if it is here, you do not reimplement it, you expose it. |
| **Desktop** | The `#[qinvokable]` methods on `AppBridge` (`src/ui/bridge.rs`) and the QML that drives them. This is the full desktop surface: 98 invokables. |
| **Android** | The command name in `rust/conquerd-android/src/command.rs` (`KNOWN_COMMANDS`), or what is missing. |

The core is the same Rust library in both clients. A feature missing on Android
is almost never missing from the core — it is missing a command, a UI, or both.
That distinction is the point of this document, so it is called out per feature.

---

## The client contract

Things every client must get right, regardless of platform.

### Two identity forms, and they are not interchangeable

An identity is one Ed25519 keypair (`identity.rs`). It surfaces as three
strings, and mixing them up is the most common cross-client bug:

| Form | Encoding | Produced by | Used for |
|---|---|---|---|
| `public_id` | URL-safe base64 **with padding** — ends in `=` | `derive_public_id` | Signaling, room membership, peer records, ACLs |
| `peer_id` | Hex SHA-256 of the public key | `derive_peer_id` | Stable short identifier, chat store keys, UI |
| `fingerprint` | Colon-separated hex of the 32 public key bytes | `Identity::fingerprint` | Human verification only |

`crypto::b64url_decode` accepts **padded or un-padded** input, so parsing is
forgiving and emitting is not. Do not assume the form you received is the form
the next hop wants. The invite handshake is the live example: ephemeral X25519
public keys go on the wire **un-padded** (`b64url_encode_nopad` in
`connection_manager/manager/invite.rs`), while `public_id` is padded. A client
that round-trips one through the other's encoding produces IDs that compare
unequal to the same key.

### The identity file

`identity.dat` is AES-256-GCM over the Ed25519 seed, keyed by Argon2id
(t=3, m=64 MiB, p=4) over the passphrase, with `public_id` as AAD. Version 2.
The KDF parameters live in the file and are floor-checked on load — a file
claiming weaker parameters than the current minimum is rejected as tampered
rather than honoured.

Two things follow for any client:

* **Never store the passphrase.** To skip the prompt, store the *derived* key
  (`load_with_passphrase_keyed` / `save_encrypted_keyed` hand it back). It opens
  one file on one device, so a leaked copy is worthless elsewhere and a
  passphrase the user reused is never written down.
* **A keyfile can substitute for or augment the passphrase.**
  `crypto::build_passphrase_material` concatenates the text with SHA-256 of a
  keyfile. Desktop exposes both; Android exposes only the text passphrase.

### Local stores

Three SQLite stores, all opened with the identity and all client-owned:
`peer_store.rs`, `chat_store.rs`, `room_store.rs`. They resolve their paths
through `Identity::default_key_dir()`, which reads `CONQUERD_HOME` — Android
must set that before opening anything, since it has no meaningful `HOME`.

### Transport

`connection_manager/` owns signaling (WebSocket to a supernode), QUIC peer
sessions, and the invite handshake. `connection_fallback.rs` decides direct
versus relayed. A client does not implement transport; it starts the manager
and consumes its events.

---

## Feature areas

### 1. Identity and unlock

Create or unlock an identity at start-up; optionally stay unlocked.

* **Core** — `identity.rs`. Passphrase and/or keyfile, OS keyring helpers
  (`keyring_store_aes_key` / `keyring_load_aes_key` / `keyring_delete_aes_key`).
* **Desktop** — `unlockWithPassphraseAndFile(passphrase, keyfile, remember)`,
  `lockIdentityAndQuit`. `PassphraseDialog.qml`. Auto-unlock stores the derived
  key in Windows Credential Manager / macOS Keychain / the Linux Secret Service
  via the `keyring` crate, opt-in per unlock and cleared when unticked.
* **Android** — `identity.info`, `identity.export_key`, `identity.set_handle`. The `keyring` crate has
  no Android backend, so the Kotlin side seals the exported key with an
  AndroidKeyStore AES-GCM key (`IdentityVault.kt`). Keyfiles are supported:
  the picked document is staged into the sandbox first, because the core reads
  the file itself rather than taking bytes.

### 2. Peers and trust

The peer list is the client's own record of who it trusts.

* **Core** — `peer_store.rs`: add, `remove`, `remove_by_any_id`, block state,
  display names, per-peer audio preferences.
* **Desktop** — `selectPeer`, `removePeer`, `blockPeer`, `unblockPeer`,
  `peerDisplayName`, `copyPeerId`, `clearPeerHistory`, `setPeerAudioPref`,
  `applyPeerAudioPrefs`. Right-click context menu in `PeerList.qml`.
* **Android** — `peer.list`, `peer.block`, `peer.unblock`, `peer.remove`.
  Long-press a peer row for Copy peer ID / Block / Remove; removal confirms
  first, and blocked peers are labelled in the list. No display-name editing
  and no per-peer audio preferences.

### 3. Invites

The only way a peer relationship starts. A `d://` URL carrying an
ephemeral X25519 key; the handshake completes over the supernode.

* **Core** — `connection_manager/manager/invite.rs`.
* **Desktop** — `generateInvite`, `copyInvite`, `pasteInvite`.
* **Android** — `invite.generate`, `invite.accept`, plus a `d://` intent
  filter so a tapped link opens the app. Links that arrive before unlock are
  held and replayed.

### 4. Direct chat

* **Core** — `chat_store.rs`. Persistent history, delivery acks, failure state,
  unread counts, retention trimming.
* **Desktop** — `sendChat`, `sendTyping`, `deleteMessage`, `retryMessage`,
  `loadMoreHistory`, `clearUnread`, `getStoredMessageCount`,
  `purgeAllChatHistory`, `trimMessagesByAge`, `trimMessagesByCount`.
* **Android** — `chat.history`, `chat.send`, `chat.mark_read`,
  `chat.unread_total`, `chat.typing`, `chat.delete`, `chat.retry`,
  `chat.purge_all`, `chat.trim`. Long-press a bubble for Copy / Try again /
  Delete, with retry offered only on your own failed messages; Settings carries
  purge and age trimming. `chat.trim` folds the desktop's two trim invokables
  into one command with `days` and `keep_per_peer` bounds.

### 5. Rooms

Multi-party chat and voice hosted by a supernode. End-to-end encrypted with
sender keys (`group_key.rs`), so the supernode fans out ciphertext it cannot
read.

* **Core** — `room_store.rs`, `group_key.rs`, `sfu_client.rs`.
* **Desktop** — `createRoom`, `createSubRoom`, `joinRoom`, `joinRoomWithInvite`,
  `joinRoomWithVoice`, `leaveRoom`, `removeRoom`, `subscribeRoomChat`,
  `sendRoomChat`, `loadRoomChatHistory`, `generateRoomInvite`,
  `generateRoomInviteForPeer`.
* **Android** — `room.list`, `room.create`, `room.join`, `room.leave`,
  `room.hide`, `room.unhide`, `room.chat.subscribe`, `room.chat.unsubscribe`,
  `room.chat.send`, `room.history`, `room.request_list`, `room.voice.join`,
  `room.voice.leave`, `room.invite`. `room.create` takes an optional
  `parent_room_id`. A shareable link carries a Space inclusion proof and no
  grant; per-peer grants (`generateRoomInviteForPeer`) are not wired.

  Creation was not just a missing command. Nothing else on Android writes to
  the room store — the rooms a phone lists were persisted by a desktop client
  and travelled with the identity — so `persist_if_room_created` in
  `session.rs` had to be added alongside it, mirroring `bridge.rs`: skip rooms
  already known (cluster replay re-announces them), persist the entry, then
  adopt it into the Space tree and announce the signed root. **A client that
  creates rooms without the Space adoption produces rooms that cannot be
  admitted to by proof**, so they behave differently from desktop-created ones.

Hiding is local-only and exists on both clients — the room stays on the
supernode and other members are unaffected.

### 6. Spaces

A signed Merkle tree over nested rooms: rooms can contain rooms, the root is
signed, and admission is proved against it rather than asserted.

* **Core** — `space.rs` (Layer 1: nested tree, signed-root sync, proof-based
  admission).
* **Desktop** — nested room UI; membership proofs on join.
* **Android** — nested room list and sub-room creation.

  Two things were already true before any Android work, and are worth knowing
  before you build this for another client. **Proof-carrying joins are handled
  by the core, not the client**: `handle_accept_room_invite` stashes the
  invite's space creds and `send_room_join` attaches them to the `SfuJoin`
  single-use, so any client that forwards a `d://…room#…` link to
  `AcceptInvite` gets proof-based admission for free. And **`room.list`
  already carries the nesting** — `space_id` and `parent_id` are stamped onto
  the stored entry by `adopt_room_into_space` — so a flat list is a UI choice,
  not a data gap.

  What a client does have to supply is the parent for a create it initiates:
  the supernode's `RoomCreated` reply carries the new room id but not what you
  asked to nest it under, so the intent is held between request and reply
  (`pending_sub_room_parent`, keyed `supernode_id:room_name`, the same shape
  the desktop bridge uses).

### 7. Direct voice calls

* **Core** — `call_controller.rs` (lifecycle state machine), `aec.rs`,
  jitter buffering, Opus via `conquerd-opus`.
* **Desktop** — `startCall`, `acceptCall`, `rejectCall`, `endCall`, `setMuted`,
  plus the full audio stack: `listAudioDevices`, `setAudioDevices`,
  `setInputVolume`, `setOutputVolume`, `startMicTest`, `stopMicTest`,
  `testSpeaker`, `setVoiceActivation`, `setNoiseStrength`, `setJitterDepth`,
  `setVoiceBitrate`, `enablePtt`, `disablePtt`.
* **Android** — `call.start`, `call.accept`, `call.reject`, `call.end`,
  `audio.start`, `audio.stop`, `audio.set_muted`, `audio.tune`. The last takes
  input/output gain, noise suppression and strength, bitrate ceiling and voice
  activation as optional fields — they are set together from one screen, and
  the call controller persists none of them, so the client re-applies on every
  start. No device selection (the platform picks), no mic test, no PTT, and no
  jitter-depth control — that one has no `CallCommand` behind it.

`ndk_context` must be initialised in `nativeStart` before any audio starts, or
cpal's Oboe backend panics and takes the call controller with it.

### 8. Room voice

Same audio pipeline, fanned out by the supernode's SFU instead of sent
peer-to-peer, sealed under the room sender key.

* **Desktop** — `joinRoomWithVoice`, the `VoiceRail` and `ParticipantWidget` UI.
* **Android** — `room.voice.join`, `room.voice.leave`, with a voice rail in the
  room screen.

### 9. Video

* **Core** — `video/`: camera capture, codec seam (VP8 everywhere via
  `conquerd-vpx`, H.264 via Media Foundation on Windows), fragmentation over
  datagrams, picture-in-picture compositing before encode.
* **Desktop** — `setVideoEnabled`, `setVideoPreviewEnabled`, `listVideoDevices`,
  `listVideoCodecs`, `setVideoAdaptiveBitrate`, `setVideoSubscriptions`, plus
  screen share and a pop-out video window.
* **Android** — `video.start`, `video.stop`, with CameraX capture feeding
  `nativeSubmitCameraFrame`. **Send-only**: there is no decode-to-Surface path,
  so a phone can be seen but cannot see. No screen share.

### 10. Content audio and A/V sync

Sending the audio a machine is *playing* alongside video, and holding video to
meet it.

* **Core** — `content_audio.rs`, `content_capture.rs`, `content_sender.rs`,
  `content_playout.rs`, `media_clock.rs`, `media_sync.rs`.
* **Desktop** — `setContentAudioEnabled`, `setContentAudioPref`,
  `setContentAudioViewers`. Capture is WASAPI loopback, Windows-only.
* **Android** — **nothing**. No capture source and no playout anchor.

### 11. File transfer

* **Core** — `file_transfer.rs`. Streaming to 250 MB, quota-gated through the
  feature registry, direct (`core.file.v1`) and room (`room.file.v1`) paths.
  Room files are advertised then pulled rather than pushed.
* **Desktop** — `sendFile`, `acceptFile`, `rejectFile`, `sendRoomFile`,
  `acceptRoomFile`, `declineRoomFile`, `openContainingFolder`.
* **Android** — `file.send`, `file.accept`, `file.reject`, `file.cancel`, plus
  `file.send_room`, `file.accept_room`, `file.decline_room`. The room path is
  advertise-then-pull: nothing moves until a member accepts, so accepting is a
  request back to the originator rather than a local decision, and declining
  tells nobody.

  Two Android-specific constraints shape this. A SAF `content://` uri cannot be
  handed to the core — the core streams from a path for the length of the
  transfer, and the picker's grant does not last that long — so `FileStaging`
  copies a picked document into the sandbox first, and copies a finished
  download back out to wherever the user saves it. And `FileComplete` carries a
  payload that cannot cross the JNI boundary, so `persist_if_file_complete`
  writes it, records it in chat history, and stamps the path onto the event.

### 12. Supernodes and clusters

Several supernodes can present as one logical node; a room recorded under any
member must resolve to the same place, and sessions fail over between members.

* **Core** — `cluster.rs`, `quic_relay_client.rs`, `connection_fallback.rs`.
* **Desktop** — `isKnownSupernode`, `removeSupernode`, `resolveSupernodeNodeId`,
  `clusterRepresentative`, `configureDirectP2p`, `openNodePortal`.
* **Android** — `supernode.list` and `supernode.remove`, surfaced in Settings
  with the cluster size beside each node. Adding one is still done by accepting
  its invite, as on the desktop. Removing leaves the rooms it hosted in the
  store so re-adding finds them again.

### 13. Connection state

* **Core** — `session_state.rs`, `connection_fallback.rs`. Direct-versus-relay
  decisions, per-peer session scalars, online presence.
* **Desktop** — `ConnectionStatsChip`, `StatsPanel`, `SessionBanner`.
* **Android** — a `ConnectionBanner` showing DIRECT / RELAYED / OFFLINE, and an
  online dot per peer. No stats.

### 14. Portal and web apps

`d://` pages hosted by a supernode, with a JS SDK that reaches the
client's QUIC channels — the multiplayer game demos run on this.

* **Core** — `web_app_client.rs` (`web.host.app.v1`), `ui/scheme.rs`.
* **Desktop** — `openNodePortal`, `ConquerdWebView.qml` (Qt WebEngine).
* **Android** — a `WebView` reached from a supernode row in Settings.
  `portal.fetch` answers every request the WebView makes, because a
  `d://` URL is not fetchable by a browser: there is no such network
  protocol, and the page is served over the identity QUIC relay. Bodies run to
  32 MB, so a fetch is written to a cache file and the reply carries the path —
  `shouldInterceptRequest` wants a stream anyway.

  `portal.open` / `portal.send` / `portal.poll` / `portal.close` back the game
  channel. The web SDK **polls** (`pollDatagrams` every 33 ms) rather than being
  pushed to, so inbound datagrams are buffered in the session and drained by
  the poll instead of crossing the JNI event pump — `event::to_json` already
  refused to render them. The queue is bounded and drops oldest: these are
  real-time frames, so a page that stopped polling wants current state, not a
  backlog. That is the opposite of the file path, where a dropped chunk is data
  loss.

  `addJavascriptInterface` can only pass strings, so `PortalBridge.BOOTSTRAP_JS`
  builds the promise-shaped `window.conquerd` the SDK awaits on top of the
  string calls. Every bridge call returns JSON rather than throwing: an
  exception across that boundary reaches the page as a bare "Error", losing
  what the core said.

### 15. Plugins and feature modules

Bespoke `x.*` namespaces, discovered and instantiated at run time, with signed
manifests (`MANIFEST_SCHEMA_VERSION`) and trust gating for non-first-party
code.

* **Core** — `plugin_manager.rs`, `plugin_runtime.rs`, `feature_trust.rs`.
* **Desktop** — plugin discovery and lifecycle; attestation policy in settings.
* **Android** — **nothing**.

### 16. Ollama

A local LLM as a feature module, optionally auto-replying in direct chats or
rooms.

* **Core** — `ollama_module.rs` (`x.ollama.v1`).
* **Desktop** — `askOllama`, `cancelOllama`, `fetchOllamaModels`, with base URL,
  model, system prompt and auto-respond toggles in settings.
* **Android** — **nothing**.

### 17. Avatars and handles

Identity-derived avatars, so a peer looks the same everywhere without anyone
hosting an image.

* **Core** — `avatar_config.rs`.
* **Desktop** — `avatarSvg`, `avatarTintColor`, `avatarImageSmooth`,
  `setAvatarConfigJson`, `broadcastAvatarConfig`, `broadcastAvatarConfigToAll`,
  `broadcastHandleToAll`.
* **Android** — `avatar.svg` returns the core-rendered SVG and tint; the peer
  list draws it beside the status dot.

  The SVG is parsed rather than reimplemented. The colour rules (islands,
  dual-hue modes, shade modes) are intricate enough that a second
  implementation would drift, and an avatar that differs between a peer's
  desktop and phone is worse than none. `Avatar.kt`'s parser is deliberately
  strict — it expects the fixed shape `build_avatar_svg` emits and falls back
  to a flat tint rather than guessing, so a change there surfaces as a plain
  square instead of a wrong picture.

  `avatar.set_config` stores the config on our own peer record — the same place
  the handle lives, and the place `avatar.svg` already reads a peer's config
  from — then broadcasts it, because peers cache it. The editor previews
  through `avatar.svg` with an explicit config, so what it shows is what peers
  will draw. It exposes grid, shading, dual-hue and islands; the remaining
  knobs are refinements that need a bigger screen to be worth the space.

### 18. Updates

* **Core** — `github_updater.rs`, polling GitHub Releases.
* **Desktop** — `applyUpdate`, `setAutomaticUpdateChecks`; hands off to
  `conquerd-installer`.
* **Android** — not applicable in this form; distribution is the store or a
  sideloaded APK.

### 19. Desktop-only platform integration

`platform.rs`, `uri_scheme.rs`, `taskbar_badge.rs`, `upnp.rs`. Protocol
registration (`registerUriScheme` / `unregisterUriScheme`), desktop shortcuts
(`createDesktopShortcuts`, `removeDesktopShortcuts`, `hasDesktopShortcuts`),
taskbar unread and missed-call badges, tray and minimise behaviour, UPnP port
mapping, ringtone.

The Android equivalents are different mechanisms for the same jobs: an intent
filter for the URI scheme, a foreground service for session survival, and
notification channels for unread state.

### 20. Settings

The desktop persists roughly sixty settings (`ui/settings_model.rs`) covering
audio devices and tuning, video codec/resolution/fps/bitrate, relay and UPnP,
theme, notifications, tray and window geometry, Ollama, attestation policy,
update checks, and onboarding state.

**Android persists a deliberate handful.** `AppSettings` (SharedPreferences)
holds camera facing, voice activation and theme; the display name is *not*
there — it lives on the identity's own peer record via `identity.set_handle`,
because peers cache their copy of it and every outbound message already reads
the sender handle from that record. The rest of the desktop's list is device
pickers, window geometry and tray behaviour that a phone either decides for
itself or does not have.

A client adding more should check whether the setting needs a core command
(volumes, noise strength, jitter depth, bitrates all do — none are exposed to
Android yet) or is purely local (theme, camera choice).

---

## Parity at a glance

| Feature | Desktop | Android |
|---|---|---|
| Identity create / unlock | Passphrase + keyfile | Passphrase + keyfile |
| Stay unlocked | OS keyring, opt-in | AndroidKeyStore, opt-in |
| Peer list / presence | Yes | Yes |
| Peer block / unblock | Yes | Yes |
| Peer remove | Yes | Yes |
| Invites generate / accept | Yes | Yes |
| Direct chat | Full | Full, minus per-peer clear |
| Rooms join / leave / chat | Yes | Yes |
| Room create | Yes | Yes |
| Sub-rooms | Yes | Yes |
| Room invites | Link + per-peer grant | Link only |
| Room hide | Yes | Yes |
| Spaces (nested, proof admission) | Yes | Nested list, sub-rooms, proof joins |
| Direct voice | Full stack + tuning | Call, mute, gain/noise/bitrate |
| Room voice | Yes | Yes |
| Video send | Camera + screen share | Camera only |
| Video receive | Yes | **No** |
| Content audio / A/V sync | Windows only | **No** |
| File transfer | Direct + room | Direct + room |
| Supernode / cluster management | Yes | List and remove |
| Portal / web apps | Yes | Pages + game channel |
| Plugins (`x.*`) | Yes | **No** |
| Ollama | Yes | **No** |
| Avatars / handles | Yes, editable | Yes, editable |
| Settings | ~60 persisted | Name, avatar, camera, voice tuning, history, theme |

---

## Traps that bite second clients

1. **ID encoding.** Padded `public_id` versus hex `peer_id` versus un-padded
   ephemeral keys. Decode leniently, emit exactly.
2. **`CONQUERD_HOME` before any store.** The stores resolve their own paths;
   set it first or they land somewhere unwritable.
3. **Cluster fan-out duplicates rooms.** The same room arrives from every
   member of a cluster. Fold on the delivering node before showing a list.
4. **Room frames arrive N times in a multi-homed session.** Deduplicate rather
   than rendering each copy.
5. **Signed manifests are version-gated.** `MANIFEST_SCHEMA_VERSION` mismatches
   are rejected outright, not negotiated.
6. **Never drop on a chunk path.** Rate limiting a file-transfer chunk stream is
   data loss, not backpressure — use a token bucket and a byte ceiling that
   blocks instead.

---

## Keeping this true

The desktop surface is enumerable, so drift is detectable:

```bash
# Every desktop capability
grep -A2 '#\[qinvokable\]' rust/conquerd-client/src/ui/bridge.rs \
  | grep -oE 'fn [a-zA-Z_]+' | sed 's/fn //' | sort

# Every Android capability
sed -n '/KNOWN_COMMANDS/,/];/p' rust/conquerd-android/src/command.rs
```

`KNOWN_COMMANDS` is enforced by a test — every entry must have a match arm — so
the Android column cannot silently rot. The desktop column has no such test;
when you add an invokable, add it here.
