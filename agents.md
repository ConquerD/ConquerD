# Agents.md

## Overview
This document defines agent roles for Conquerd, a privacy-first **modular peer-connectivity framework** with a client-only, invite-only trust model. Voice, chat, files, rooms, and games are *features* negotiated between peers and supernodes — not hard-coded behaviors. See the [Feature Module Reference](#feature-module-reference) below for the full capability catalogue.

Core scope:
- No first-party backend for identity, discovery, or presence.
- Invite + handshake bootstraps a signed, end-to-end secure session.
- Every QUIC primitive (reliable streams, unidirectional streams, datagrams) is exposed as a generic `Channel`.
- Capability-based feature negotiation: peers and supernodes advertise typed capability sets; consumers pick from what's enabled.
- Supernodes are optional volunteer peers that provide QUIC relay, SFU rooms, web/WebTransport hosting, and bespoke feature modules.

Transport stack:
- **Direct sessions**: QUIC peer-to-peer via `ConnectionManager` + embedded `quinn::Endpoint` (conquerd-client) — generic streams + datagrams + channel multiplexer.
- **Relay sessions**: QUIC relay (`QuicRelayClient` → supernode `QUICRelayServer`); same channel multiplexer; WebSocket used for membership/signaling fallback only.
- **Signaling**: Signed, transcript-bound messages; prefers QUIC signaling stream when a peer session is connected, falls back to WebSocket.
- **Web/games**: WebTransport (QUIC, via `wtransport`) and QUIC reliable streams on supernodes let browser game clients (via `web-sdk/conquerd.mjs` and the Ed25519 identity handshake) participate in the same channel fabric as native peers. Games opened in the native portal receive trust-chain context automatically; standalone browser access requires the SDK plus cert fingerprint (see README).
- **Capability exchange**: `CAPABILITY_ANNOUNCE` after handshake; `CAPABILITY_INVOKE` opens feature channels.

## Agent Roles

### 1. Project Manager Agent
Responsibilities:
- Track roadmap progress and delivery risks.
- Coordinate priorities across stability, UX, and security.
- Escalate lockups, race conditions, and call reliability regressions.

Working style:
- Keep status updates short: done, in progress, risks.
- Use the [Roadmap & Status](#roadmap--status) section below as the source of truth for roadmap progress and delivery risks; this file's role definitions and guardrails apply throughout.

### 2. Developer Agent (Signaling + Handshake)
Responsibilities:
- Maintain direct client-to-client signaling and handshake lifecycle.
- Enforce signed, transcript-bound messaging for all signaling.
  - Invite/handshake bootstrap has strong replay protection (expiry + transcript binding).
  - Post-handshake signaling is Ed25519-signed and enforces a 5-minute timestamp freshness window (`MAX_MESSAGE_AGE_SECS = 300.0` in `connection_manager/manager.rs` on the client; `is_fresh(300.0)` in `protocol.rs` on the supernode WS path) **plus** a per-sender sliding-window replay guard (`conquerd_features::ReplayGuard`) keyed on the message signature, which rejects re-delivery of an already-seen message *within* the freshness window. Real-time `SfuAudio` frames are exempt from the dedup guard (ephemeral, high-rate). `ReplayGuard` negative-path tests cover replays; client `protocol.rs` and `connection_manager::tests` cover stale/future timestamp rejection.
- Keep endpoint/invite behavior stable and restart-safe.

Working style:
- Prefer deterministic state transitions and de-duplication.
- Add or update tests for signaling and invite persistence changes.

### 3. Developer Agent (Transport + Features)
Responsibilities:
- Maintain QUIC peer-to-peer transport (`ConnectionManager` + embedded `quinn::Endpoint`) and the generic channel multiplexer (datagrams, uni/bidi streams, priority hints, channel-tag registry in `conquerd-client`).
- Maintain QUIC relay client path (`QuicRelayClient`) using the same channel abstraction as direct sessions.
- Maintain the `conquerd-features` capability registry, negotiation, quotas, and per-feature auth tiers.
- Maintain inbound and outbound quota enforcement symmetry: `ConnectionManager` / `QuicRelayClient` datagram-layer quotas and `FeatureRegistry::gate_through_feature` signaling-layer quotas must stay consistent across connect/disconnect cycles.
- Maintain first-party feature modules: `core.chat.v1`, `core.audio.opus`, `core.file.v1`, `room.audio.sfu`, `room.chat.v1`, `room.file.v1`.
- Maintain desktop UX consumers (chat panel, call overlay, session banner) on top of the feature modules.
- Maintain relay ticket auto-renewal and endpoint mailbox for robust connectivity across restarts.
- Keep SFU room hosting ephemeral on supernodes (`sfu.rs` idle GC, no disk persistence); client `RoomStore` owns definitions and replays `SfuRoomCreate` on connect.
- Keep session status banner accurate across direct, relay, and room modes; preserve participant-state consistency.

Working style:
- Avoid renegotiation churn and thread-contention patterns.
- Keep DSP and per-tick feature loops within real-time budget.
- Treat first-party UI as one consumer of the framework — don't shortcut around the capability layer.
- Validate with focused tests first, then broader runs.

### 4. Security Agent
Responsibilities:
- Review identity, invite, handshake, and trust transitions.
- Validate signature checks, transcript binding, and replay controls.
- Review QUIC relay authentication (Ed25519 session keys from handshake).
- Enforce the **supernode opacity model** (see [Supernode Opacity](#supernode-opacity-agent-contract) below): supernodes must not read, log, persist, or branch on application payload content — only routing metadata, membership, signatures, and quota byte counts on opaque wire bytes.
- Review the **capability/feature trust model**: per-feature `auth` tiers, no implicit escalation across features, user-consent prompts for non-`core.*` namespaces, per-feature byte/stream/datagram quotas.
- Review third-party feature module signing and load-time trust prompts (native cdylib now; WASM sandbox later).
- Ensure WebTransport surface on supernodes does not bypass capability gates that apply to native peers.
- Review release manifest verification and Ed25519 signing.
- Validate peer-to-peer build attestation challenges and responses.
- Review installer supply-chain trust (SignPath, Apple notary, Sigstore).
- Ensure action SHA pinning in CI/CD workflows.
- Keep threat model aligned with client-only topology even as bespoke supernode features are added.

Working style:
- Add negative-path tests for invalid, expired, or replayed messages.
- Verify profile/key material isolation across multi-profile runs.

### 5. QA/Testing Agent
Responsibilities:
- Run unit and integration tests and targeted manual checks (613 Rust unit tests; 114 in `conquerd-features`).
- Stress race-prone flows (rapid connect/disconnect, duplicate signaling).
- Validate trusted-peer persistence and UI synchronization.
- Cover QUIC relay path and WebSocket membership signaling for room audio.

Working style:
- Prioritize lockup prevention, audio continuity, and deterministic outcomes.
- Keep acceptance checklist current.
- Before sign-off on Rust changes: `cargo fmt --all -- --check` in affected workspace(s), then targeted `cargo test` / `cargo clippy -D warnings` for touched crates; use `scripts/ci_local.ps1` when changes span workspaces or hit release/installer paths.

### 6. DevOps/Infra Agent (Transport + Feature Hosting)
Responsibilities:
- Maintain QUIC relay, SFU, and WebTransport + QUIC-stream deployment guidance and scripts.
- Maintain supernode packaging (`scripts/build_supernode.sh` for Linux/macOS `.tar.gz`, `scripts/build_supernode.ps1` for Windows `win64` `.zip`) and release/CI jobs for **linux-x86_64**, **linux-aarch64**, and **win64**.
- Maintain the supernode feature manifest (`supernode.toml`-style typed capability list replacing ad-hoc env-var toggles).
- Support hot-reload of feature modules and bespoke `x.<vendor>.*` plug-ins.
- Keep infra docs aligned with no-backend policy: supernodes assist transport and host feature modules; they are never identity authorities.
- Ensure endpoint mailbox (`supernode_endpoints.json`, 24h TTL) and ticket renewal (1h TTL, 10-min renewal window) persist across restarts. SFU room state must **not** be persisted — only peer trust (`peers.json`), identity, manifest, and endpoint mailbox belong on disk.
- Document how to host static + WebTransport-enabled web games under `games/<slug>/`.
- **Maintain the supernode manager** (`rust/conquerd-supernode-manager/`) as the primary integration-testing and cluster-ops tool: provisioning, `cluster-sync`, `exec`-based remote debugging, and `build-deploy` for live cluster testing against the acdc test cluster (nodes a/b/c). See `rust/conquerd-supernode-manager/agents.md` for the full operator contract.

Working style:
- Treat infra as transport + opt-in feature hosting only.
- Do not introduce app-layer central services or mandatory features.
- Use `cluster-sync` after any install or redeploy that changes cluster membership; `install`/`config-push` preserve the existing `[cluster]` section automatically.

### 7. Documentation Agent
Responsibilities:
- Keep README, plan docs, and runbooks aligned with implementation.
- Document invite flow, troubleshooting, and migration-impact changes.

Working style:
- Update docs in the same change as behavior/protocol updates.
- Keep language user-focused and architecture-accurate.

### 8. UX/UI Agent
Responsibilities:
- Improve chat-first usability and call-state clarity in the native Rust Qt/QML client (`rust/conquerd-client/src/ui/`).
- Keep session status banner consistent across voice modes (direct peer vs room); `AppBridge::connection_mode` property drives the native banner colour.
- Maintain unread/badge/tray behavior consistency, including the `missed_calls` qproperty increment/clear cycle.
- Preserve DPI-aware behavior and accessible layout constraints.
- Keep the Privacy and Data `SettingCard` in `SettingsPage.qml` (Privacy tab) in sync with `ChatStore` methods and `keyring_delete_aes_key`.
- Keep the peer block/unblock context menu toggle in `PeerList.qml` in sync with `ConnectionCommand::BlockPeer` / `UnblockPeer`.
- Keep the **Peers vs Rooms** split: the Peers rail (`PeerList.qml` / `PeerListModel`) must list only `PeerStore::list_non_supernode_peers()`; the Rooms sidebar (`MainWindow.qml` `nodeListModel`, `RoomPanel.qml`) must list only trusted supernodes (`PeerStore::supernodes()`, `AppBridge::isKnownSupernode`). Never show supernodes in Peers or ordinary peers in Rooms.
- Keep **room UX** aligned with client-owned definitions + ephemeral supernode hosting: `CreateRoomDialog.qml` / `create_room` for user-initiated create (auto-join); `RoomStore::hide_from_sidebar` for sidebar remove (local only — no `SfuRoomDelete`); `SupernodeConnected` replay via `CreateRoom { materialize_only: true }` must not auto-join; room list filtering uses `filter_sfu_rooms_for_sidebar` + composite `(supernodeId, roomId)` keys.
- Keep the Avatar section on the Identity settings tab (`SettingsPage.qml`, `settingsTab = 1`) in sync with `AvatarConfig` fields in `avatar_config.rs`; the `settings.avatar_config_json` qproperty on `SettingsModel` bridges the two. Avatar SVGs are rendered via `backend.avatarSvg(peerId, configJson)` → `data:image/svg+xml;base64,...` in `Avatar.qml`.

Working style:
- Avoid backend-dependent UX affordances.
- Verify key flows with two-client manual checks.

## Global Guardrails
- Keep architecture client-owned and invite-only.
- No backend drift; no mandatory features.
- Supernodes provide transport assistance and *opt-in* feature hosting; they are never identity authorities.
- **Supernode opacity**: treat every supernode as an untrusted relay — it may see connection metadata (peer ids, room ids, indices, byte volumes, timestamps) but must never receive decryptable chat, file, or voice content. Forward opaque bytes only; never log or persist payload fields.
- Every cross-peer behavior must be expressible as a `FeatureModule` with a stable capability id; no hidden side channels.
- Per-feature `auth` tier and quota are mandatory — don't bypass the capability layer for convenience.
- Stability and security take precedence over new features.
- Pair meaningful code changes with tests or reproducible validation steps.
- **Format before finish**: after any Rust edit, run `cargo fmt --all` in every workspace you touched (`rust/` and/or `rust/conquerd-client/`), then verify with `cargo fmt --all -- --check`. CI runs both checks; do not leave formatting for the pipeline to catch. If `--check` prints a diff, apply it (or re-run `cargo fmt --all`) and re-check before moving on.

## Architecture Notes (Agent Contract)

This section captures implementation locations and invariants that agents must respect. For a human-oriented overview of crates, layers, and the modular framework, see the README.

### Critical Crates & Agent Invariants
- `conquerd-client` (Qt 6 / QML via CXX-Qt, `cargo build -p conquerd-client --features qt-ui`): primary desktop binary. All first-party `core.*` modules and desktop UX live here. Headless mode (no `qt-ui`) is used for integration tests.
  - `src/ui/bridge.rs` + models: `AppBridge` QObject and QML-facing state (`connection_mode`, `call_duration_secs`, `missed_calls`, `mic_level`, etc.).
  - `src/peer_store.rs`: trusted-peer persistence (`is_supernode`, `supernode_from_invite`, `relay_hints`); `supernodes()` vs `list_non_supernode_peers()` split drives Rooms vs Peers (see supernode detection invariant below).
  - `src/avatar_config.rs`: compiled unconditionally (so `peer_store.rs` can hold `Option<AvatarConfig>` even without `qt-ui`); `settings.avatar_config_json` qproperty lives on `SettingsModel`.
  - `src/chat_store.rs`: per-peer `trim_by_age` / `trim_by_count` / `purge_all` (identity lock also drops the AES key via `keyring_delete_aes_key` in `identity.rs`).
  - `src/room_store.rs`: client-owned encrypted room definitions (`my_rooms.dat`), keyed by `(supernode_id, room_id)`; sidebar hide list; replay source for `SfuRoomCreate` on supernode connect. **Never** persist room definitions on the supernode.
  - `src/identity.rs`: Ed25519 + OS keyring integration.
  - `ConnectionManager` (`src/connection_manager/` module — `mod.rs`, `manager.rs`, `internal.rs`, `quic.rs`, `ws.rs`, `events.rs`, `tests.rs`): direct QUIC + relay client paths; outbound `core.chat.v1` / `core.file.v1` must call `gate_through_feature`; audio datagrams must use the quota-checked send helpers.
- `conquerd-features`: the spine. `FeatureRegistry`, `FeatureModule` trait, `dispatch_message` / `dispatch_invoke_datagram`, inbound/outbound quota enforcement (token-bucket per `(feature, peer)`), auth tiers, channel-tag registry. 114 unit tests. All transports (direct QUIC, relay, WS, WebTransport) must go through the registry for capability-gated paths; hot paths may call `gate_inbound_through_feature` directly but must still respect the same buckets.
- `conquerd-opus`: first-party libopus wrapper (DRED + OSCE). Requires DNN data (see Build Notes). Linked only into `conquerd-client`.
- `conquerd-supernode`: sole supernode implementation (QUIC relay, WS signaling, WebTransport `web.host.h3.v1`, QUIC bidi `web.host.app.v1` portal, SFU, manifest-driven feature hosting).
- `conquerd-installer`: release download + apply + manifest verification + signing helper.

**Quota symmetry invariant**: inbound (`dispatch_message`, `dispatch_invoke_datagram`, transport hot-path `gate_inbound_through_feature`) and outbound (`gate_through_feature` called from `ConnectionManager::dispatch_outbound`, audio send helpers) must use consistent per-feature/per-peer token buckets. Buckets are cleared on `drop_peer` / `peer_left` / disconnect paths (including WS and WebTransport `release_session`).

**CXX-Qt qproperty rule**: every `#[qproperty(T, name)]` in a `#[cxx_qt::bridge]` block must have a matching field in the Rust state struct (`AppBridgeRust` etc.) and be initialised in `impl Default`. Missing fields are silent in headless mode but fail at runtime/Qt meta-object construction.

**Replay / freshness rule**: post-handshake signaling uses Ed25519 signatures + 5-minute freshness window (`MAX_MESSAGE_AGE_SECS` on the client; `is_fresh(300.0)` on the supernode WS path) + per-sender `ReplayGuard` (keyed on signature) inside the freshness window. `SfuAudio` frames are exempt. `ReplayGuard` replay negative-path tests are in `replay.rs`; client stale/future timestamp rejection is covered in `protocol.rs` and `connection_manager::tests`.

**Supernode detection invariant** (client UI + transport):
- **Authoritative source**: signed invite payload `is_supernode` → persisted on accept as `PeerRecord.is_supernode` and `PeerRecord.supernode_from_invite` (`connection_manager/manager.rs` `AcceptInvite` / `InviteHandshakeAccept`). `docs/THREAT_MODEL.md` calls the invite field advisory for *security escalation* — the client still uses it as the canonical UI/transport classifier.
- **Never infer on new accepts**: do not treat `relay_hints` / `ws://` / `wss://` alone as supernode identity. Ordinary peers may carry a supernode ws URL for NAT/relay traversal; that must not open a WS session keyed under their identity or add them to the Rooms sidebar.
- **Transport**: `ConnectionManager::connect_supernode_ws` and startup WS auto-reconnect (`PeerStore::supernodes()` in `run_inner`) run only for trusted supernode records.
- **UI**: `PeerStore::list_non_supernode_peers()` → `peersUpdated` / Peers rail; `PeerStore::supernodes()` + `AppBridge::resolveSupernodeNodeId` / `isKnownSupernode` → Rooms sidebar (`nodesUpdated`, `sfuRoomsUpdated`). Room selection is scoped by `(supernodeId, roomId)` — never `room_id` alone.
- **Migration-only repair** (`PeerStore::repair_all_supernode_flags` on `peers.dat` load): demote false positives whose ws hint matches another trusted supernode's signaling URL, or `is_supernode` rows with a non-zero direct `quic_port` and a non-operator handle; legacy-promote ws rows with operator/default titles only; grandfather unique-ws `is_supernode` rows to `supernode_from_invite`. Do not reintroduce broad ws-hint promotion for personal-handle peers. Negative-path tests live in `peer_store.rs`.

**Room ownership invariant** (client definitions, supernode ephemeral only):
- **Authoritative room definitions live on peers** — encrypted `my_rooms.dat` via `RoomStore`, keyed by `(supernode_id, room_id)`. Entries are written on user create (`RoomCreated`), join (`join_room`), or chat subscribe (`subscribe_room_chat`). Chat history stays in `ChatStore` / `room_chat_history` on each peer; the supernode does not store messages or room metadata to disk.
- **Supernode rooms are in-memory only** — `SFURoomManager` in `rust/conquerd-supernode/src/sfu.rs`. Do **not** reintroduce `sfu_rooms.json` or other room persistence on the supernode. The built-in `default` room is always present; user-created rooms idle-GC after `IDLE_ROOM_GC_SECS` (900 s) with zero voice participants and zero chat subscribers.
- **Materialize on connect** — when a trusted supernode WS session comes up, `AppBridge` replays non-hidden `RoomStore` entries with `ConnectionCommand::CreateRoom { materialize_only: true, room_id, creator_id, ... }`. `ConnectionManager::pending_materialize` suppresses auto-join on the matching `SfuRoomCreated`. User-initiated create (`materialize_only: false`) still auto-joins.
- **Wire shape** — `SfuRoomCreate` payload may include `room_name`, `room_type`, optional `room_id`, optional `creator_id` (original creator when a non-creator peer replays a saved definition). Supernode `handle_sfu_room_create` uses `creator_id` from payload when present.
- **Sidebar hide is local** — `RoomStore::hide_from_sidebar` + `remove_room` in `bridge.rs` / `MainWindow.qml`; does not delete on the supernode (ephemeral GC handles server-side cleanup). Hidden rooms are filtered from `sfuRoomsUpdated` and skipped on replay.
- **Outbound routing** — room signaling must target the correct supernode WS session (`resolve_supernode_ws_target` in `dispatch_outbound`); never fan `SfuRoomCreate` to the first connected supernode when multiple nodes share a host.

### Supernode Opacity (Agent Contract)

Supernodes are **untrusted for content**. They assist NAT traversal, WS/QUIC relay, SFU fan-out, and opt-in feature hosting — never identity or decryption.

| Supernode may see (routing metadata) | Supernode must NOT see (application content) |
|---|---|
| Peer `public_id`, room id, relay peer indices | Chat message bodies |
| WS/QUIC connection timing, byte volumes | Opus/audio payloads (E2E-sealed under the room sender key) |
| Ed25519 signatures + message `type` on the wire | File chunk bytes (E2E-sealed under the room sender key) |
| Room membership rosters (who joined/left) | Room content keys or session keys |
| `SfuRoomCreate` room name/type (ephemeral, not persisted) | Decrypted `game.relay.v1` / bespoke datagram payloads |

**Opaque today (correct):**
- `game.relay.v1` — raw datagram fan-out; supernode never parses inner bytes (`webtransport.rs`, `relay.rs`).
- QUIC relay datagram forwarding — inner channel tag + payload forwarded verbatim (`wire.rs`).
- `EncryptedSignal` — pass-through relay type on the WS path (`MessageType::EncryptedSignal` in `conquerd-supernode/src/main.rs`); the envelope for direct 1:1 E2E ciphertext and the sealed `SfuGroupKey` distribution.
- `SfuAudio` / `room.audio.sfu` — Opus frames are E2E-sealed as `[epoch:u8][nonce:12][AES-256-GCM(opus)]` (AAD = conv_id ‖ sender ‖ seq) under a per-room sender key; the supernode fans out verbatim and cannot decode. Active-speaker selection uses frame arrival only, never decode (`sfu.rs`).
- `SfuChat` / `room.chat.v1` — the `body` is E2E-sealed as `nonce ‖ AES-256-GCM(body)` (AAD = conv_id ‖ sender ‖ message_id) under the same per-room sender key. Quota helpers prefer the opaque `ciphertext` length (`sfu_chat_byte_count` in `main.rs`).
- `SfuFile*` / `room.file.v1` — each chunk's `data` is E2E-sealed as `nonce ‖ AES-256-GCM(data)` (AAD = conv_id ‖ sender ‖ transfer_id ‖ chunk_index) under the same per-room sender key (`group_key::seal_file_chunk` / `open_file_chunk`); falls back to cleartext only when no group key is available yet (race right after join), auto-detected via the `e2e` flag. `SfuFileOffer` / `SfuFileComplete` metadata (size, sha256, rel_path) stays cleartext — it is not content.
- Room sender keying: the room **owner** generates a per-epoch key and seals it to each member inside an `EncryptedSignal` carrying an inner `SfuGroupKey` (supernode forwards blind); rotates on member-leave (FS/PCS). Codec + `SenderKeysGroup` in `group_key.rs`; wired in `connection_manager/manager.rs`. **v1 caveats:** any paired room peer can push a bogus `SfuGroupKey` (DoS on decrypt, not a confidentiality break — hardens with Space grants); `epoch` is `u8` (wraps after 256 rotations/session).

**Signed but not yet opaque (must not regress):**
- Invite handshake derives an X25519/HKDF `session_key` (`crypto.rs` / `handshake.rs`); direct 1:1 relay uses a pairwise key (see the E2E backlog note), and room content uses the `SenderKeysGroup` per-room key.

**Supernode operator prohibitions (code review checklist):**
- Never `info!` / `debug!` payload fields (`body`, `audio`, `data`, file names).
- Never persist chat, audio, or file bytes to disk (room definitions stay client-owned; SFU rooms are in-memory only).
- Quota accounting uses wire-byte or `ciphertext` length — not decrypted content.
- Membership gates (`is_chat_sender`, `audio_forward_targets`) use sender id + room id only.

All room-content types (`SfuChat`, `SfuAudio`, `SfuFile*`) are now E2E-sealed under the per-room `SenderKeysGroup` key — the room-content opacity gap is closed. Direct QUIC P2P may remain signed-only (no supernode on path) unless users opt into pairwise encryption there too.

### Build Gotchas (Agent-Relevant)
- **Two Cargo workspaces — fmt both when unsure**: `rust/` (features, supernode, installer, opus) and `rust/conquerd-client/` (desktop client). `scripts/ci_local.ps1` runs `cargo fmt --all -- --check` in each. Touching only one crate still requires fmt in that crate's workspace root.
- **`conquerd-opus` DNN data** (required for default `dnn` feature): run `scripts/fetch_opus_weights.ps1` (Windows) or `.sh` (Linux/macOS) before building. Extracts Xiph.Org C arrays into `rust/conquerd-opus/opus/dnn/`. Idempotent. Set `default-features = false` on the dep to build without DNN support.
- **Qt requirement**: `conquerd-client --features qt-ui` needs Qt 6.x on `PATH` (`QMAKE` or `CMAKE_PREFIX_PATH`). Headless builds (no `qt-ui`) are valid for tests.
- **Optional `aec` feature** (`conquerd-client`): experimental pure-Rust NLMS acoustic echo cancellation on the capture path (`src/aec.rs`). Off by default and dependency-free; the canceller always compiles (its DSP unit tests run on every `cargo test`) but only activates at runtime when built with `--features aec`. Integration: `mix_and_play` tees the far-end into a reference ring → capture closure pops it in `process_capture_mono_f32` and subtracts the modelled echo before the noise gate. Needs real two-device tuning (delay must fall within the filter tap span); not yet enabled in shipped builds.
- **Version sync (SignPath requirement)**: `rust/conquerd-client/Cargo.toml` and `rust/conquerd-installer/Cargo.toml` **must** carry the identical version so PE `ProductVersion` metadata matches across signed artifacts.
- **CXX-Qt qproperty alignment** (see Architecture Notes above): missing Rust-side fields for `#[qproperty]` entries are silent in headless mode but hard-fail when the Qt meta-object system is active.
- **Windows signing** (optional for local builds): `signtool.exe` on `PATH`; `build_win64.ps1` skips gracefully if absent or no cert env vars are set.
- **Supernode PE metadata**: `rust/conquerd-supernode/build.rs` derives Windows version info from `CARGO_PKG_VERSION`; keep its `Cargo.toml` in sync if distributing a signed supernode binary.
- See README "Developer Guide" and "Code Signing Policy" for human-oriented build, portable packaging, and SignPath bootstrap details. Code signing team roles (`conquerd-authors`, `reviewers`, `approvers`) are documented in the README.

## Using the Modular Framework (Agent Contract)

This section defines the precise runtime contract for the modular framework. Every cross-peer behavior must be expressed as a `FeatureModule` registered against a `CapabilityDescriptor`. Agents working on transport, supernode, or UX **must** go through the framework — no hidden side channels, no bypassing auth/quota gates.

The README presents a human-oriented view of the same concepts (lighter tables, operator guidance, authoring examples). This section is authoritative for implementation details, dispatch paths, enforcement order, and negative-path requirements.

### Registering a feature module (Rust)

```rust
use conquerd_features::{
    AuthTier, CapabilityDescriptor, ChannelKind, FeatureModule, PeerId,
};

pub struct MyModule;

impl FeatureModule for MyModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        CapabilityDescriptor::new("x.vendor.thing", "1.0", ChannelKind::Datagram)
            .with_auth(AuthTier::TrustedPeer)
    }
    fn on_message(&self, source: PeerId, payload: &[u8]) { /* handle */ }
}

let m = std::sync::Arc::new(MyModule);
if !state.features.bind_module("x.vendor.thing", m.clone()) {
    let _ = state.features.register_module(m);
}
```

- `register_module` adds descriptor + module in one step (errors on duplicate id).
- `bind_module(id, m)` attaches a module to a descriptor that the supernode manifest already loaded — preferred for first-party modules so operators retain manifest control.
- `dispatch_message(id, source, payload)` is the single inbound entry point used by all transports (QUIC peer, QUIC relay, WebTransport). The runtime enforces auth tier + quota before calling the module; returns `false` silently if the quota bucket is exhausted.
- `dispatch_invoke_datagram(id, peer, params, tags)` is the invoke entry point (from `CAPABILITY_INVOKE`). Returns `ModuleError::Internal` (e.g. `"datagram quota exceeded"`) if over limit.

### Supernode capability surface

Hosted feature declarations come from `<data_dir>/supernode.toml` (typed schema in `rust/conquerd-supernode/src/manifest.rs`). The supernode also upserts built-in core/room/game descriptors into the registry so quota gates and relay fan-out can classify first-party traffic even when a manifest omits those entries. **Do not** add new env-var toggles — extend the manifest. Reserved namespaces: `core.*`, `transport.*`, `room.*`, `web.*`, `game.*`. Bespoke modules use `x.<vendor>.*`.

### Capability negotiation

`rust/conquerd-features/src/wellknown.rs` is the source of truth for well-known capability IDs. Negotiation rule: same `id` + same major `version` (see `CapabilityDescriptor.is_compatible_with`). Consumers (chat panel, call overlay, room controller) only enable UI/logic for capabilities present in the negotiated intersection.

### Channel-tag rules

The shared tagged-frame contract lives in `rust/conquerd-features/src/channel_frame.rs` (single source of truth, re-exported from the crate root and mirrored in `web-sdk/conquerd.mjs` as `ChannelTag` / `encodeFrame` / `decodeFrame`). Fixed first-party tags `0x00`–`0x0F`: `CONTROL_TAG=0x00`, `AUDIO_TAG=0x01`, `CHAT_TAG=0x02`, `FILE_TAG=0x03`, `ROOM_AUDIO_TAG=0x04`. On the direct QUIC peer stream, `core.audio.opus` (datagram), `core.chat.v1`, and `core.file.v1` now ride their dedicated fixed tags via `encode_frame`/`classify`; untagged leading-`{` JSON is still accepted as `UntaggedControl` for backward compatibility. `ROOM_AUDIO_TAG=0x04` (`room.audio.sfu`) only appears on the relayed datagram path (peer→supernode QUIC relay session) so relay quota accounting attributes the frame to `room.audio.sfu` rather than the direct call; `classify` leaves it in `FrameClass::Other` and the relay client decodes it manually. The supernode relay (`wire.rs`) is transparent to these tags — the channel byte rides *inside* the forwarded payload.

`conquerd-client` dynamic registry: `0x10`–`0xEF` per session (negotiated/bespoke caps), `0xFF` broadcast, others reserved. Allocate dynamic tags via the registry — never hard-code.

### Auth + quota enforcement

The runtime enforces `auth` (`public` | `room-member` | `trusted-peer`) and per-feature byte/datagram quotas before invoking `on_invoke` / `on_message`. Modules MUST NOT re-implement these checks. Non-`core.*` namespaces without explicit `quota_bytes_per_sec` / `quota_datagrams_per_sec` fall back to `DEFAULT_BYTES_PER_SEC` / `DEFAULT_DATAGRAMS_PER_SEC` in `quota.rs` (64 KB/s, 256 datagrams/s) automatically.

Outbound sends are gated symmetrically: `FeatureRegistry::gate_through_feature(feature_id, peer_id, byte_count) → bool` runs the same token-bucket logic against a separate `outbound_quotas` registry. The Rust `ConnectionManager::dispatch_outbound` calls it for `core.chat.v1` and `core.file.v1` before signing and transmitting; `ConnectionManager::send_audio_datagram` / `send_room_audio` gate `core.audio.opus` and `room.audio.sfu` datagrams via dedicated helpers. Transport-layer inbound paths that skip `dispatch_message` (direct QUIC `AUDIO_TAG`, client `SfuAudio`, supernode QUIC relay `handle_datagram`, supernode WS `SfuAudio` fan-out) call `gate_inbound_through_feature` with the same token buckets. Quota buckets (both directions) are cleared on `drop_peer` / `peer_left` / `disconnect` (native WS signaling) and on WebTransport `BrowserBridge::release_session`.

### Browser parity

When `web.host.h3.v1` is enabled, ConquerD web clients use `web-sdk/conquerd.mjs` to participate in the exact same channel fabric over WebTransport. The supernode runs the Ed25519 identity handshake and verifies signed envelopes (`verify_browser_envelope`) before fanning payloads out to native peers. Any new `room.*` or `game.*` feature should be tested with both a native and a ConquerD web client.

### Reference modules in-tree

- `core.chat.v1`, `core.audio.opus`, `core.file.v1` — desktop client (`conquerd-client`).
- `room.audio.sfu`, `room.chat.v1`, `room.file.v1` — supernode room modules and SFU broadcast paths (`rust/conquerd-supernode/src/sfu_module.rs`, `rust/conquerd-supernode/src/main.rs`).
- `web.host.h3.v1` — supernode WebTransport listener (`webtransport.rs`).
- `x.conquerd.matchmaker.v1` — **reference bespoke `x.*` example** (`rust/conquerd-features/src/examples.rs`). A complete `FeatureModule` template (Request kind, `TrustedPeer` auth, explicit quota, stateful lobby `on_invoke` with a "lobby ready" hook). Opt-in only via `register_example_modules`; never auto-advertised. Demonstrates how a coordination feature composes with the opaque `game.relay.v1` relay (the ready roster is exactly the set of peers wired together over `game.relay.v1`).

### Feature trust

Inbound `CAPABILITY_INVOKE` gating is enforced by `conquerd-features` at the Rust layer before any module callback:
- First-party namespaces (`core.*`, `transport.*`, `room.*`, `web.*`, `game.*`) bypass the user-consent prompt.
- Bespoke `x.*` namespaces require explicit user consent (prompted once per `(feature, peer)` pair) and are subject to `DEFAULT_BYTES_PER_SEC` / `DEFAULT_DATAGRAMS_PER_SEC` until the operator sets explicit quotas in the feature descriptor.
- Three gates are enforced in order: (1) feature intersection check, (2) auth tier (`trusted-peer` / `room-member` / `public`), (3) consent gate for non-first-party namespaces.

## Feature Module Reference (Agent Contract)

The authoritative implementation is the `conquerd-features` crate (linked into both `conquerd-client` and `conquerd-supernode`). This is the condensed capability catalogue and wire/behaviour spec for agents; see "Using the Modular Framework" above for the registration/dispatch API and enforcement rules.

The README contains a friendlier "Built-in Capabilities" table and operator guidance for humans. Numbers, auth enforcement order, quota symmetry requirements, and negative-path expectations here take precedence for code changes.

### Discovery

Invite-only, no central registry:
1. **Out-of-band invite** (primary, mandatory): a signed `conquerd://` URL bootstraps the first connection and establishes the trust root — preserving the invite-only model.

On connect, each peer sends `CAPABILITY_ANNOUNCE`; the runtime activates only the **negotiated intersection**. Two descriptors are compatible if they share the same `id` **and** the same major version (`CapabilityDescriptor.is_compatible_with`). Missing support means silent non-negotiation — no fallback, no error.

Planned (not yet implemented): **in-band capability gossip** — connected peers exchanging each other's supernode capability bundles for organic discovery while the invite-only trust root stays intact (see P3 backlog).

### Capability descriptor wire shape

Exchanged as JSON inside `CAPABILITY_ANNOUNCE`:

```json
{ "id": "core.chat.v1", "version": "1.0", "kind": "stream",
  "auth": "trusted-peer",
  "params": { "quota_bytes_per_sec": 32768, "quota_datagrams_per_sec": 50 },
  "experimental": false }
```

| Field | Description |
|---|---|
| `id` | Reverse-DNS capability identifier |
| `version` | Semver; negotiation uses **major version only** |
| `kind` | `datagram` (unreliable), `stream` (reliable), or `request` (single-shot RPC) |
| `auth` | Required auth tier (default `trusted-peer`) |
| `params` | Optional feature params (codec, quota limits, framing) |
| `experimental` | Advisory flag; clients may skip experimental features |

### Auth tiers

Enforced by the runtime in `dispatch_message` / `dispatch_invoke_datagram` **before** any callback. Modules must not re-implement these checks.

| Tier (`auth`) | Who can use it |
|---|---|
| `public` | Any connected peer, no prior trust |
| `room-member` | Peers holding a valid room membership token |
| `trusted-peer` | Peers in the local trust store (default for all `core.*`) |

### First-party module catalogue

Desktop peer modules (active in direct P2P sessions; bundled and audited, never prompt):

| Capability ID | Kind | Auth | Quota (bytes/s · dgram/s) | Notes |
|---|---|---|---|---|
| `core.chat.v1` | stream | trusted-peer | 32 KB · 50 | Signed text chat, delivery acks, typing indicators; per-feature token-bucket quota. Supernode WS signaling also enforces 60 control messages / 10 s per connection. Send path in `ConnectionManager`. |
| `core.audio.opus` | datagram | trusted-peer | 32 KB · 200 | Direct voice via Opus over QUIC datagrams; latency-optimised in `ConnectionManager::send_audio_datagram`. |
| `core.file.v1` | stream | trusted-peer | 8 MB · 4096 | Chunked file transfer; sub-types: offer/accept/reject/chunk/completed/ack/error. |

Supernode-hosted modules (multi-party; require a connected supernode):

| Capability ID | Kind | Auth | Notes |
|---|---|---|---|
| `room.audio.sfu` | datagram | room-member | Ephemeral SFU voice in supernode memory (`sfu.rs`); idle-GC after 900 s empty; definitions owned by clients (`room_store.rs`). Transport: prefers an unreliable **QUIC relay datagram** (`ROOM_AUDIO_TAG`, no TCP head-of-line blocking) when the sender holds a relay session, else falls back to base64-in-JSON `SfuAudio` over the WebSocket signaling path. Frames are **Ed25519-signed** (receivers verify identically on both paths) **and E2E-sealed** under a per-room sender key (`[epoch:u8][nonce:12][AES-256-GCM(opus)]`, AAD = conv_id ‖ sender ‖ seq; see [Supernode Opacity](#supernode-opacity-agent-contract)). The supernode relays verbatim and cannot decode Opus for routing (active-speaker gate uses frame arrival only). The supernode bridges per member (`relay.rs` `set_room_audio_bridge` → `main.rs`): relay datagram for relay-connected members, WS for the rest — no member is partitioned. `JoinRoom` requests a relay grant (`ensure_room_relay`) so members acquire a relay session; WS fallback is automatic if the grant never lands. **Active-speaker cap**: the SFU forwards at most `MAX_ACTIVE_SPEAKERS` (5) concurrent talkers per room — frames from speakers over the cap are dropped server-side (receiver fills with Opus PLC) so per-receiver decode/bandwidth stays bounded as rooms grow. Selection is energy-free: `SFURoom::note_audio_should_forward` ranks senders by a decaying frame-activity score (Opus DTX makes "frames arriving" a good proxy for "talking"), with a sticky active set + displacement hysteresis. The gate (`SFURoomManager::audio_forward_targets`) is applied once at the inbound point of all three fan-out paths (WS, relay bridge, browser); rooms with ≤5 active talkers are unaffected. |
| `room.chat.v1` | stream | room-member | Room text chat broadcast via supernode; `body` is E2E-sealed under the per-room sender key (content not persisted server-side). |
| `room.file.v1` | stream | room-member | Signed room file broadcast via supernode; each chunk's `data` is E2E-sealed under the per-room sender key (AAD = conv_id ‖ sender ‖ transfer_id ‖ chunk_index); recipients verify + decrypt chunks before saving. Offer/complete metadata stays cleartext. |
| `web.host.app.v1` | stream | public | In-app `conquerd://` portal over QUIC bidi streams in embedded Chromium (4 MB/s). |
| `web.host.h3.v1` | datagram | public | WebTransport (HTTP/3) bridge so browser clients join the same channel fabric. |
| `game.relay.v1` | datagram | room-member | Opaque session-scoped datagram relay for in-session games. |

Transport descriptors (handled by the QUIC layer directly; no application module code): `transport.quic.audio.v1`, `transport.quic.relay.v1`, `transport.quic.stream.v1`, `transport.quic.feature_datagram.v1`, `transport.quic.uni_stream.v1`, `transport.quic.stream_priority.v1`, `transport.quic.zero_rtt.v1`, `transport.quic.pmtud.v1`, `transport.quic.migration.v1`, `transport.quic.flow_control.v1`.

### `web.host.app.v1` portal

The native client browses a supernode's in-app portal without leaving the app: an embedded Chromium view navigates to `conquerd://<supernode_pub>/<path>`. The scheme handler issues QUIC bidi-stream requests tagged with this capability instead of HTTPS — one stream per request:

1. Client → supernode: one length-prefixed `WebAppRequest` JSON frame (`{ "path": "/index.html", "method": "GET" }`).
2. Supernode → client: one `WebAppResponseHeader` JSON frame (`{ "status": 200, "content_type": "text/html", "total_len": N }`) then length-prefixed binary body chunks terminated by a zero-length chunk.

The QUIC connection is the identity gate (the supernode already knows which Ed25519 key opened the stream), so requests are not re-signed. Dynamic routes answered inline: `/health` · `/api/stats` (relay/SFU/peer counts), `/api/peers`, `/api/config`, `/api/metrics`. Static assets are served from `<data_dir>/web/`. The view is restricted to `conquerd://` URLs (external links open in the system browser); a `window.conquerd` JS bridge (`supernodeId`, `ready` → `{ myPeerId, supernodeId, version, fetch() }`) is injected at document creation.

### Quotas and channel tags

Token-bucket per `(feature_id, peer_id)`, refilled each second. On exhaustion `dispatch_message` returns `false` (payload dropped) and `dispatch_invoke_datagram` returns `ModuleError::Internal("quota exceeded")`. Bespoke `x.*` modules without explicit `params` fall back to `DEFAULT_BYTES_PER_SEC` / `DEFAULT_DATAGRAMS_PER_SEC` (64 KB/s · 256 dgram/s). The channel-tag multiplexer maps a 1-byte tag to a feature: `0x10`–`0xEF` dynamic per session (~224 channels), `0xFF` broadcast, others reserved — always allocate via the registry.

### Non-goals

No central feature registry, no mandatory features, no implicit cross-feature privilege escalation, and supernodes are never identity authorities.

## Roadmap & Status

This section is the single source of truth for delivery status (condensed from the former `ROADMAP.md` / `IMPROVEMENT_PLAN.md` / `TODO.md`).

**Last reviewed:** 2026-07-08 (Space Merkle Layer 1 gaps 1–3 and 5 closed — `"members"` invite-policy widening, client UI toggle, periodic Space-root re-broadcast, and root-equivocation flagging; see `docs/SPACE-MERKLE-DESIGN.md`. Test totals refreshed to 620. Durable per-feature invariants live in Architecture Notes / Using the Modular Framework / Feature Module Reference above).

### Health summary

ConquerD is in strong shape for a 1.0 privacy-first modular P2P framework: near-zero authored tech debt, dense unit coverage (620 unit tests listed by `cargo test -- --list`: 114 features + 246 supernode + 184 headless client + 76 installer), architecture compliant with the capability-gated, client-only, invite-only model, and solid supply-chain hardening (SHA-pinned actions, version sync, optional signing with graceful fallbacks). Game relay (`game.relay.v1` over WebTransport) is confirmed working end-to-end with native clients. SFU room definitions are client-owned; supernodes host rooms ephemerally only. **Supernode manager** (`rust/conquerd-supernode-manager/`) is production-tested: the acdc three-node cluster (a/b/c) is the live integration-testing target — `build-deploy`, `cluster-sync`, and `exec`-based remote debugging are operational.

### Foundations — stable ✅

P0–P2 delivery is complete and covered by tests: CI hardening, post-handshake replay protection, relay/SFU smoke tests, quota symmetry, cross-platform CI, platform notification/UPnP TODOs, supply-chain scanning, operator runbook, threat model, version automation, metrics export, game relay (`game.relay.v1` over WebTransport), ephemeral SFU rooms, and Space Merkle **Layer 1** (authenticated room tree, including the `"members"` invite-policy widening, its client UI toggle, periodic Space-root re-broadcast, and root-equivocation flagging).

The durable invariants for each of these live in **Architecture Notes**, **Using the Modular Framework**, and **Feature Module Reference** above; operator/build/signing detail is in the README and `docs/`. This file tracks invariants and open work — not a changelog, so completed-work detail is not re-logged here.

Open / deferred work is tracked in **`backlog.md`** (Layer 2 crypto, Space Merkle remaining items, plugin sandbox, audio-quality polish, speculative discovery/federation, and declined items); the Space specifics live in `docs/SPACE-MERKLE-DESIGN.md`.


### Pre-signing checklist (SignPath Foundation)

Code-side items (LICENSE, PE metadata, Code Signing Policy + Uninstalling sections in the README) are done.

**Release manifest signing (Ed25519)**: A project-controlled Ed25519 keypair is used for `releases_manifest.json` (the list of version+build_hash+build_id verified by the installer). The public key is committed in source (`keys/release-signer-public.pem` and the hex constant). The private key is kept offline/secure.

Because SignPath Foundation (for Windows Authenticode / PE signatures) and similar programs usually require a public OSS project with at least one release to grant free access, the first release(s) may use unsigned binaries for the PE files while still shipping a properly signed manifest (using the project Ed25519 key).

Once approved for SignPath:
- Subsequent releases get automated binary signatures.
- The Ed25519 manifest key continues to be the canonical root of trust for the manifest (easy to rotate via a signed rotation entry).

The remaining human action for binary signing is to apply for a SignPath Foundation subscription at https://signpath.io — the README `## Code Signing Policy` section satisfies the project-home-page requirement. See the bootstrap language in README.md for details on the initial release(s).

Update `agents.md` (this section) in the same change as any signing-related work.

**Initial / per-release manifest steps (approvers):**
- `cargo run -p conquerd-installer --bin sign-release-manifest -- --generate-unsigned`
- Fill the three platform entries with the real `build_hash` (from CI artifacts or local `build_*.ps1` .sha256) and `build_id` (the exact string injected via `CONQUERD_BUILD_ID` or derived at tag build time; this is what peers will see in attestations).
- Sign with the private key → produces `releases_manifest.json` (overwrite).
- Commit the signed manifest (public) as part of the release prep / tag.
- The release workflow (publish-release job) now includes it in the GitHub Release assets.
- The skeleton generator + signer live in the installer crate so the canonical + verify code never drifts from the signing code.

### Process

- Update this section in the same change as any work that shifts status or adds risk.
- Before touching quotas, dispatch, signaling, or capability paths: run the relay/SFU/room tests + a manual 2-client check.
- Use the 8 Agent Roles above as the per-release checklist; PM keeps updates short (done / in progress / risks).

**Pre-submit validation (agents — run locally, do not defer to CI):**

1. **Format** — in each touched workspace root (`rust/`, `rust/conquerd-client/`):
   - `cargo fmt --all`
   - `cargo fmt --all -- --check` (must exit 0; if it fails, read the diff, apply, repeat)
2. **Compile / lint** — `cargo clippy -D warnings` and `cargo test` for affected crates; full gate: `scripts/ci_local.ps1` (Windows) or equivalent steps on Linux/macOS runners.
3. **Debug failures before hand-off** — reproduce CI errors locally (fmt diff, clippy, test panic, cross-target naming like Linux CI vs Windows-only paths). Fix root cause in the same change; do not stop at "CI will tell us."

Typical fmt commands from repo root:

```powershell
# rust/ workspace (features, supernode, installer, opus)
cd rust; cargo fmt --all; cargo fmt --all -- --check

# client workspace (when conquerd-client changed)
cd rust/conquerd-client; cargo fmt --all; cargo fmt --all -- --check
```
