# Conquerd – Zero-Trust Invite-Only P2P Voice & Chat

**Website**: [conquerd.com](https://conquerd.com)

Conquerd is a privacy-first peer-to-peer voice and chat application. Identity, discovery, and trust live entirely on your device — there is no central server, no account, no sign-up. Peers connect through cryptographically signed invite links and communicate directly over encrypted QUIC channels.

No telemetry. No cloud accounts. No third-party infrastructure required.

---

## Features

### Chat-First P2P
- Text messaging is the primary interaction after connecting with a peer.
- Voice calls are opt-in, initiated from within a conversation — never from a contacts list.
- Per-conversation scroll position persistence and floating "↓ Latest" button.
- Typing indicators with 4-second auto-hide.
- Unread badges on taskbar and system tray notifications.
- Chat history persisted to SQLite with delivery states (sending → sent → delivered).
- Right-click message context menu: delete individual messages or clear full per-peer history.
- Room chat history stored locally per `(supernode, room)` on each peer (not on the supernode).

### Voice Calls
- Low-latency Opus audio over QUIC peer-to-peer transport (Rust [quinn](https://github.com/quinn-rs/quinn)).
- Push-to-talk and voice activation modes with fade-in/fade-out.
- Real-time noise suppression (spectral-gate FFT, Rust-native).
- Fixed-depth jitter buffer (60ms default, tuneable 1–20 frames) with silence-to-audio de-click.
- Incoming call ringtone + overlay dialog with Answer, Decline, and Block options.
- Session status banner accurately reflects transport mode (direct P2P vs. relay).
- Live call duration display in the call overlay.
- Missed call counter badge shown in the peer list and tray; increments when a call ends unanswered.

### Video & Screen Sharing

> Working on Windows; the Linux and macOS camera backends compile but have not been validated against real hardware, and screen capture is Windows-only. See [Known limitations](#known-limitations).

- **Share a camera, a screen, or a single window** from one **Share video** control. Starting a share asks what to do about audio; stopping does not ask anything. Screen and window capture use `Windows.Graphics.Capture` and are **Windows-only** — cameras work on all three platforms.
- **Codecs are negotiated, not assumed.** H.264 via Media Foundation on Windows (using the codec licence the OS already holds) and VP8 everywhere via the vendored `conquerd-vpx`. A build advertises only what it can actually encode *and* decode, so a Windows peer and a Linux peer always share VP8. Direct calls intersect both peers' sets; a room sender picks its own codec and stamps every frame with it.
- **One stream per peer.** Picture-in-picture overlays are composited *before* encoding rather than sent as extra streams, so adding an overlay costs no extra bandwidth and no extra decoder.
- **Audio shared with a video is its own track**, not a second microphone. It is stamped from the same session clock as the picture, so it stays in sync, and it is mixed independently of voice: muting someone does not silence what they are presenting, and shared audio is only mixed for peers whose video you actually have open. Capturing shared audio is currently **Windows-only** (WASAPI loopback, whole-machine or per-application).
- **Audio-led A/V sync.** Video is held or dropped to meet the shared-audio timeline. A camera-only call has no such timeline and simply free-runs, exactly as it did before sync existed.
- **Adaptive bitrate** driven by the same loss measurement that drives audio ABR — it backs off above 10 % loss, recovers below 4 %, and never drops below 120 kbps (past that, video stops being a picture and just costs bandwidth). Your quality preset sets the ceiling; adaptation only moves downward from it.
- **Room video is relay-datagram-only.** There is no WebSocket fallback for video media by design: a member on a WS-only path keeps audio rather than stalling the call. Frames are E2E-sealed under the same per-room sender key as voice, with domain separation so the two can never be confused. Supernodes ship with **public room creation disabled**, so rooms — and the video in them — are private unless an operator opts in.
- Video tiles, an expandable video region, and a detachable pop-out window; camera device list, quality presets, and a live local preview in **Settings → Video**.

### Rooms (Multi-Peer Voice)
- SFU (Selective Forwarding Unit) room hosting on volunteer supernodes — **ephemeral in memory only**; the supernode does not persist room definitions or chat history.
- **Client-owned room definitions** — rooms you create, join, or subscribe to are saved in encrypted `my_rooms.dat` on your device, keyed by `(supernode, room_id)`. When you reconnect to a supernode, saved rooms are materialized automatically via `SfuRoomCreate` (without auto-joining voice).
- Idle user-created rooms are removed on the supernode after ~15 minutes with no voice participants or chat subscribers; the built-in `default` room is always present.
- QUIC relay transport for NAT-traversed room audio plus room chat/file signaling when available; WebSocket remains the membership and fallback signaling path.
- Room parity with direct-peer features: chat, voice, file transfer.
- Create rooms from the Rooms sidebar; right-click **Remove room** hides the room locally (does not delete server-side state — there is none to delete). Whether **public** rooms can be created is the operator's call: supernodes default to private-only (`allow_public_rooms = false`), and a refused create comes back as a signed denial rather than a silent failure. The built-in `default` room is always there regardless.
- **Sub-rooms**: nest a room under any existing room (not just directly under the supernode) via **Create Public/Private Sub-room…**; the sidebar shows expand/collapse for rooms that have children.
- Private rooms can enable **"Members can invite"** so any current member — not just the creator — can mint new invite tokens for that room.
- Peer room invites with accept/decline flow.
- Up to 32 participants per room.
- Room (and sub-room) membership is backed by a client-signed, authenticated **Space** tree (Merkle inclusion proofs over room definitions) so any cluster member can admit a proven joiner even if it never saw the room's original grant; remaining Space work is tracked in `backlog.md`.
- Invited members of a private room can always rejoin, even after the supernode's temporary room state was cleared (idle timeout or restart) — no need to request a fresh invite link.
- If a room join is refused (full, private without access, etc.), the app reports the denial and rolls back optimistic room state. A transient `room_absent` denial from a cold cluster member is retried with bounded exponential backoff before it is surfaced.
- On a clustered deployment (multiple supernodes hosting the same room), voice now carries across every node in the cluster, matching how room chat already worked.

### In-App Supernode Portal & Browser Games
- Supernodes with `web.host.app.v1` serve an in-app portal over a QUIC bidi-stream channel. The native client browses `conquerd://` pages using an embedded Chromium view from the **Rooms** sidebar (supernode avatar click) — no external browser, no public game ports, and no public web TLS certificates.
- **`game.relay.v1`** — opaque datagram relay for in-app portal games: the supernode fans raw QUIC-relay datagrams among peers that joined the same game session (identity path; no external browser / WebTransport). Three demo games are bundled: **cursor relay**, **brick breaker**, **shared drawing**.
- Game pages are served from `<data_dir>/games/<slug>/` and reachable only via the native portal at `conquerd://<supernode_id>/games/<slug>/`. The `window.conquerd` JS bridge exposes channel APIs over the authenticated QUIC session.
- Portal requests use the identity-authenticated QUIC connection; there is no public HTTPS/WebTransport listener, game TLS certificate, or certificate fingerprint passed to portal pages.

### Security & Identity
- Cryptographic identity via long-term Ed25519 keys with derived peer IDs (SHA-256).
- Invite-only discovery through signed `conquerd://` links (timestamped, expiry-checked).
- Forward-secret handshakes using ephemeral X25519 + HKDF + AES-GCM.
- All signaling is Ed25519-signed, transcript-bound, freshness-checked, and protected by a per-sender replay guard keyed on message signatures.
- Peer revocation with propagation (socket drop, relay eject, SFU eject).
- Local trust graph — successful handshakes persist to local peer store.
- Avatar configs (`AVATAR_CONFIG` message) are only exchanged after the Ed25519 handshake completes — unknown or untrusted peers never receive a peer's custom visual identity.
- Optional release-signed P2P updates with Ed25519 signatures and threshold validation.
- Per-feature token-bucket rate limiting (inbound and outbound) enforced at the QUIC transport layer — each capability (`core.chat.v1`, `core.file.v1`, `core.audio.opus`, `room.audio.sfu`, tagged feature channels) has independent byte/datagram quota buckets per peer, preventing any single peer from flooding a channel beyond descriptor-defined rates.

#### Supply-Chain Trust
- **Code Signing policy**: Windows uses SignPath.io when available, macOS uses Apple Developer ID, and release CI can publish GitHub/Sigstore attestations. Early builds may be unsigned at the platform-binary layer; the project Ed25519 release manifest remains the application trust root.
- **Release Manifest**: Signed JSON listing official builds (version, platform, build_hash) verified by installer.
- **Peer Attestation**: Runtime challenges where peers prove they're running official builds via nonce-signed claims.
- **Policy Enforcement**: Configurable attestation policy (off/warn/strict) gating relay access for unverified peers.
- **CI/CD Hardening**: GitHub Actions pinned to immutable commit SHAs to prevent supply-chain attacks.

### NAT Traversal
- Multi-layer connection strategy: direct QUIC → WebSocket candidates (connected supernode endpoints, the peer's invite endpoint, LAN hint, then any further relay hints) → supernode QUIC relay.
- UPnP automatic port mapping.
- QUIC relay fallback through trusted supernodes, with relay tickets auto-renewed and an endpoint mailbox so peers find each other again across restarts.
- Direct calls that cannot establish within five seconds move to a temporary private SFU room and upgrade back to direct if it becomes available.

### Desktop Application
- Native Rust desktop binary with a Qt 6 / QML UI (via [CXX-Qt](https://kdab.github.io/cxx-qt/)).
- Modern dark theme with DPI-aware scaling (125%, 150%, 200%+).
- First-run onboarding wizard (display name, identity fingerprint + QR, optional supernode).
- `conquerd://` URI scheme for one-click invite joining.
- Invite QR codes with toggle display and save-to-PNG.
- System tray with badge notifications for unread messages and missed calls.
- Collapsible event log panel (toggle with `Ctrl+B`); `Ctrl+K` creates a new invite, `Ctrl+,` opens Settings.
- Audio input/output device selector in Settings.
- **Rooms** sidebar lists supernodes (avatar per node) with grouped SFU rooms; avatar left-click opens the operator portal, right-click offers portal / create public or private room / copy node ID / remove supernode (Qt WebEngine when `webengine` feature is enabled). Room right-click can hide a room from the sidebar (local only).
- Handles / display names broadcast to all peers and shown everywhere (chat, calls, event log).
- Peer block/unblock toggle in the right-click context menu; blocked peers show a visual indicator in the peer list.
- Privacy & Data controls in Settings: trim message history by age (days) or count (keep newest N), purge all chat history, and lock identity & quit (removes the OS-keyring AES key so the next launch requires a passphrase).
- Optional AI chat assistant via the `x.ollama.v1` plugin (Ollama backend required); enable in Settings.
- **Identity-derived avatars**: every peer has a deterministic, horizontally-symmetric identicon generated from their Ed25519 public key — no image uploads, no servers. Visual complexity signals trust tier: untrusted peers (no completed handshake) get a simple 8×8 flat-hue icon; trusted peers render a full 16×16 multi-shade avatar. Trusted peers can share a custom `AvatarConfig` after the handshake so all clients render an identical SVG. Customise your own avatar in **Settings → Identity → Avatar** with a live preview.

### Updates
- Update notifications via the GitHub Releases API; the bundled `conquerd-installer` binary downloads and applies signed releases.
- Release automation supports Windows SignPath, macOS Apple Developer ID, and Sigstore attestations when credentials/services are available. The installer always relies on the project-signed Ed25519 release manifest; see [Code Signing Policy](#code-signing-policy) for the initial-release fallback.

---

## Quick Start

### Prerequisites
- Rust toolchain (stable, MSVC on Windows)
- Qt 6.x (e.g. `C:\Qt\6.8.3\msvc2022_64`); set `QMAKE` or `CMAKE_PREFIX_PATH`. Qt WebEngine is optional but required for the in-app supernode portal.
- A working audio input/output device
- No server, no account, no sign-up required
- **Git submodules** — two C libraries are vendored (libopus and libvpx), so a plain `git clone` is not enough:
  ```bash
  git submodule update --init --recursive
  ```
- **CMake** (builds libopus) and **perl** (generates libvpx's runtime-dispatch headers). Perl ships with Git for Windows and is in the base install on Linux and macOS; the build finds it automatically.
- **Linux only:** `libasound2-dev libdbus-1-dev libclang-dev` (the last is for the V4L2 camera bindings).
- **Opus DNN model data** (for DRED + OSCE neural features, enabled by default): run once before building —
  ```powershell
  powershell -ExecutionPolicy Bypass -File scripts/fetch_opus_weights.ps1
  ```
  Downloads the Xiph.Org DNN tarball and extracts C source arrays into `rust/conquerd-opus/opus/dnn/`. Idempotent.

### Run (debug build)
```powershell
$env:QMAKE = "C:\Qt\6.8.3\msvc2022_64\bin\qmake6.exe"
$env:PATH  = "C:\Qt\6.8.3\msvc2022_64\bin;$env:PATH"
cd rust\conquerd-client
cargo build --features qt-ui
cd ..\..
.\run_client.bat
```

The build script `build_win64.ps1` produces a portable distribution under `dist\ConquerD\` plus a `dist\ConquerD-<version>-win64.7z` archive.

### Connect Two Peers

Open two terminals and run the debug client in each:

```bat
run_client.bat
```

1. Client A: click **Create Invite** → link copied to clipboard.
2. Client B: paste link into the invite input and press Enter.
3. Both clients see each other in the trusted peers sidebar.
4. Select a peer to open chat → send messages.
5. Click **Start Call** in the chat header for voice.

On first launch, an onboarding wizard walks you through choosing a display name, viewing your identity fingerprint and QR code, and optionally configuring a supernode.

---

## Platform Support & Installation

| Platform | Package | URI Scheme |
|----------|---------|------------|
| Windows  | Rust installer or portable folder | Registry (`conquerd://`) |
| macOS    | `.app` bundle + `.dmg` | `CFBundleURLTypes` in Info.plist |
| Linux    | AppImage | `.desktop` file + `xdg-mime` |

### Windows
Run `conquerd-installer.exe` or extract the portable `conquerd/` folder. The installer registers the `conquerd://` URI scheme, creates Start Menu shortcuts, and supports silent upgrades (`--silent`) and uninstallation (`--uninstall`).

### macOS
Open the `.dmg` and drag Conquerd to Applications. Grant microphone access when prompted.

### Linux
```bash
chmod +x ConquerD-x86_64.AppImage
./ConquerD-x86_64.AppImage
```

To register the `conquerd://` URI scheme:
```bash
cp packaging/conquerd.desktop ~/.local/share/applications/
update-desktop-database ~/.local/share/applications/
xdg-mime default conquerd.desktop x-scheme-handler/conquerd
```

### Uninstalling

**Windows (installer):** Open *Add or Remove Programs* (Settings → Apps → Installed apps), search for **ConquerD**, and click Uninstall. Alternatively, run `conquerd-installer.exe --uninstall` from the command line for a silent uninstall.

**Windows (portable):** Delete the extracted `conquerd\` folder. No registry keys are written by the portable version.

**macOS:** Drag the ConquerD app from Applications to the Trash. User data in `~/.conquerd/` can be removed manually if desired.

**Linux (AppImage):** Delete the `.AppImage` file. If you registered the URI scheme, remove `~/.local/share/applications/conquerd.desktop` and run `update-desktop-database ~/.local/share/applications/`. User data in `~/.conquerd/` can be removed manually.

### System Requirements
- **OS:** Windows 10+, macOS 10.15+, Linux (glibc 2.31+)
- **Audio:** Working microphone and speakers/headphones
- **Network:** Internet connection for P2P (LAN-only mode also available)

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│  Client A                            Client B                   │
│  ┌──────────┐   invite link    ┌──────────┐                    │
│  │ Identity  │ ──────────────► │ Identity  │                    │
│  │ PeerStore │ ◄── handshake ──│ PeerStore │                    │
│  │ Signaling │ ◄── ws:// ─────►│ Signaling │                    │
│  │ Chat      │ ◄── messages ──►│ Chat      │                    │
│  │ QUIC      │ ◄── voice ─────►│ QUIC      │                    │
│  └──────────┘                  └──────────┘                    │
│         Optional: supernode QUIC relay (rooms, NAT traversal)    │
└─────────────────────────────────────────────────────────────────┘
```

### Language Boundaries

Conquerd is pure Rust.

| Layer | Crate / Runtime |
|---|---|
| Desktop client — UI, signaling, chat, call state, file transfer, audio, settings | **`conquerd-client`** (Qt 6 / QML via CXX-Qt, standalone binary) |
| Capability registry, `FeatureModule` trait, quota enforcement, auth-tier gating | **`conquerd-features`** (rlib linked into `conquerd-client` and the supernode) |
| Supernode relay server (SFU + QUIC relay + portal) | **`conquerd-supernode`** (standalone binary) |
| Installer / updater | **`conquerd-installer`** (standalone binary) |

`conquerd-client` is the sole desktop client. Built with `cargo build --features qt-ui` (from `rust/conquerd-client/`); requires Qt 6.x (`QMAKE` or `CMAKE_PREFIX_PATH`). The audio pipeline (CPAL, spectral-gate noise suppression, jitter buffer), crypto (Ed25519, X25519, AES-GCM, HKDF, Argon2id), and QUIC transport (quinn) are Rust modules in `conquerd-client`; Opus codec support comes from the first-party `conquerd-opus` crate. Headless builds (without `--features qt-ui`) are used for CI integration tests.

### Transport Stack
- **Direct calls**: QUIC peer-to-peer via `ConnectionManager` (`quinn::Endpoint`, inside `conquerd-client`).
- **Direct-call fallback**: after a call is accepted, the caller waits up to 5 seconds for direct QUIC. If it is still unavailable, both peers are moved to a temporary private SFU room on a mutually trusted supernode.
- **Room audio and room broadcasts**: QUIC relay (`QuicRelayClient` → supernode `QUICRelayServer`) for room audio plus room chat/file signaling when available; WebSocket handles room membership and remains the fallback signaling path.
- **Video and shared audio**: dedicated datagram channels (`0x06`/`0x07` video, `0x08`/`0x09` content audio) kept separate from the voice path, so the battle-tested voice wire is unchanged. Room video and room shared-audio are relay-datagram-only — deliberately no WebSocket fallback, so a member on a WS-only path keeps audio rather than stalling.
- **Signaling/chat**: Ed25519-signed, transcript-bound messages; prefers QUIC signaling stream when a peer session is connected, falls back to WebSocket.
- **Relay**: QUIC relay protocol on supernodes (transport-only; no app-layer decryption).

### Core Model
- **Invite-only discovery**: peers connect only from signed `conquerd://` links.
- **Zero trust relay**: relays forward signed/encrypted payloads only — no app-layer central services.
- **Cryptographic identity**: long-term Ed25519 identity key; `peer_id` = SHA-256 of public key.
- **Forward secrecy**: invite handshakes use ephemeral X25519 + HKDF + AES-GCM.
- **Local trust graph**: successful handshakes persist to local peer store.
- **Endpoint stability**: signaling port persisted across restarts; peers notified of changes via `ENDPOINT_UPDATE`.
- **Auto-connect**: trusted peers can be marked for automatic reconnection on startup. Direct-QUIC reconnects are scanned every second and backed off exponentially to a 60-second ceiling, so a peer that stays offline costs a probe a minute rather than a tight retry loop.

### Key Data Flows

**Direct voice call (1-on-1)**
```
CPAL capture
  → NoiseSuppressor (spectral-gate FFT)
  → VoiceActivityDetector → speaking_changed signal → UI level meter
  → OpusEncoder (20ms frames, 48 kHz mono)
  → ConnectionManager.send_audio_datagram() → quinn QUIC datagram
  → QUIC unreliable datagram [2-byte seq][opus payload]
  ──────────────────────────────── (network) ────────────────────────────────
  → quinn on_audio_datagram callback
  → JitterBuffer.push(seq, opus)   ← 3-frame / 60ms reorder buffer
  → OpusDecoder.decode() → PCM
  → AudioEngine playback mix (CPAL)
```

**Room audio (multi-peer via supernode)**
```
OpusEncoder
  → E2E seal under the current room sender key
  → Ed25519-signed SfuAudio JSON
  → QuicRelayClient.send_room_audio([0xFF][ROOM_AUDIO_TAG=0x04][signed JSON])
  ──────→ supernode QUICRelayServer (quota + membership checks; opaque fan-out)
  ──────→ [sender_idx][ROOM_AUDIO_TAG][same signed JSON]
  → verify signature → decrypt room frame → OpusDecoder (per sender) → playback

Room membership (join/leave/state) flows over WebSocket. Room audio falls back to the same signed, E2E-sealed `SfuAudio` envelope over WebSocket when no relay datagram path is available. Room chat/file broadcasts prefer the QUIC relay signaling stream when a live full-access relay session exists and fall back to WebSocket otherwise.
```

**Room lifecycle (definitions vs hosting)**
```
Peer creates/joins/subscribes → RoomStore (my_rooms.dat) saves definition
Supernode connect            → client replays SfuRoomCreate (materialize only)
Everyone leaves + 15 min idle → supernode drops in-memory room (default kept)
Peer reconnects              → saved definition materializes room again
Chat history                 → stays on each peer's device (ChatStore / session cache)
```

**Chat message**
```
User types → ChatManager.send_message()
  → SQLite (status = SENDING)
  → ConnectionManager → QUIC stream 0 or WebSocket
    (Ed25519-signed + AES-GCM encrypted with session cipher)
  → Peer verifies signature → sends CHAT_ACK
  → ChatManager updates status → DELIVERED
```

---

## Modular Framework

Conquerd is structured as a **modular peer-connectivity framework**: chat, voice, files, rooms, and games are not hard-coded behaviors but **features** advertised and negotiated between peers and supernodes. The spine is the `conquerd-features` crate.

For the precise runtime contract (auth tier enforcement order, quota symmetry across inbound/outbound and all transport paths, dispatch rules, negative-path requirements, and channel tag allocation), see `agents.md` → "Using the Modular Framework (Agent Contract)" and "Feature Module Reference (Agent Contract)". The material below is the human-oriented view suitable for operators and module authors.

### Concepts

- **Capability descriptor** — a self-describing record advertised after handshake. Fields: `id` (reverse-DNS, e.g. `core.chat.v1`), `version` (semver, negotiated by major), `kind` (`datagram` | `stream` | `request`), `params` (free-form), `auth` (`public` | `room-member` | `trusted-peer`), `experimental`.
- **`FeatureRegistry`** — in-process registry of descriptors and (optionally) bound `FeatureModule`s. Lives on every peer and every supernode.
- **`FeatureModule` trait** — the implementation behind a capability id. Defines `descriptor()`, `on_invoke(ctx)` (capability invocation), `on_message(source, payload)` (inbound datagram/stream payload), and `shutdown()`.
- **Channel multiplexer** — generic QUIC `Channel` API exposing reliable streams, unidirectional streams, and unreliable datagrams. Datagram tags `0x10`–`0xEF` are dynamic per-session, `0xFF` is broadcast, others reserved.
- **Capability exchange** — after the Ed25519 handshake, both sides send `CAPABILITY_ANNOUNCE`. Each consumer (chat panel, call overlay, game module) only enables UI/logic for capabilities present in the negotiated intersection.
- **Trust tiers** — `auth` is enforced by the runtime: `public` is open, `room-member` requires SFU membership in the same room, `trusted-peer` requires an explicit local trust entry. Per-feature byte/datagram quotas (`quota_bytes_per_sec`, `quota_datagrams_per_sec`) are mandatory for non-`core.*` namespaces.

### Built-in Capabilities

| ID | Kind | Auth | Provided by |
|---|---|---|---|
| `transport.quic.audio.v1` | datagram | trusted-peer | `conquerd-client` QUIC layer |
| `transport.quic.relay.v1` | datagram | room-member | `conquerd-client` (`QuicRelayClient`) |
| `transport.quic.stream.v1` | stream | trusted-peer | `conquerd-client` QUIC layer |
| `transport.quic.feature_datagram.v1` | datagram | trusted-peer | `conquerd-client` QUIC layer |
| `transport.quic.uni_stream.v1` | stream | trusted-peer | tagged unidirectional QUIC stream framing |
| `transport.quic.stream_priority.v1` | stream | trusted-peer | advisory stream priority hints |
| `transport.quic.zero_rtt.v1` | stream | trusted-peer | advertised 0-RTT/resumption capability descriptor |
| `transport.quic.pmtud.v1` | datagram | trusted-peer | path MTU discovery capability descriptor |
| `transport.quic.migration.v1` | stream | trusted-peer | QUIC connection migration capability descriptor |
| `transport.quic.flow_control.v1` | stream | trusted-peer | tuned QUIC flow-control window descriptor |
| `core.chat.v1` | stream | trusted-peer | desktop client |
| `core.audio.opus` | datagram | trusted-peer | `conquerd-client` (via `conquerd-opus`) |
| `core.file.v1` | stream | trusted-peer | desktop client |
| `core.video.v1` | datagram | trusted-peer | `conquerd-client` (H.264 / VP8; `params.codecs` carries what this build can run) |
| `core.audio.content.v1` | datagram | trusted-peer | `conquerd-client` — audio shared *with* a video, on its own synchronised track |
| `room.audio.sfu` | datagram | room-member | supernode SFU/relay routing (`sfu.rs`, `main.rs`) |
| `room.video.sfu` | datagram | room-member | supernode opaque relay fan-out (relay datagrams only — no WS fallback) |
| `room.audio.content.sfu` | datagram | room-member | supernode opaque relay fan-out (audio shared with room video) |
| `room.chat.v1` | stream | room-member | supernode SFU/relay routing (`sfu.rs`, `main.rs`) |
| `room.file.v1` | stream | room-member | supernode SFU file broadcast |
| `web.host.app.v1` | stream | public | supernode QUIC bidi-stream portal (`conquerd://` pages for native client) |
| `game.relay.v1` | datagram | room-member | opaque in-app portal game session relay over identity QUIC |

### Enabling Features on a Supernode

Operator-declared supernode capabilities live in `<data_dir>/supernode.toml`. The supernode also upserts built-in core, room, and game descriptors into its registry so quota gates and relay fan-out can classify first-party traffic even when a manifest omits those entries:

```toml
schema_version = 1

[[feature]]
id = "core.chat.v1"
enabled = true

[[feature]]
id = "room.audio.sfu"
enabled = true
params = { codec = "opus", quota_bytes_per_sec = 131072 }

[[feature]]
id = "room.chat.v1"
enabled = true

[[feature]]
id = "room.file.v1"
enabled = true

[[feature]]
id = "x.acme.matchmaker"                # bespoke third-party module
enabled = true
version = "1.0"
kind    = "request"
auth    = "trusted-peer"
params  = { config_path = "etc/matchmaker.json" }
```

Disabled entries are kept on disk so an operator can flip them back on without retyping the descriptor. When `supernode.toml` is missing the supernode uses a full first-party default capability set (prefer committing a real manifest).

### Authoring a Feature Module (Rust)

Implement `FeatureModule` and register it on the supernode (or any peer) at startup:

```rust
use conquerd_features::{
    AuthTier, CapabilityDescriptor, ChannelKind, FeatureModule, PeerId,
};

pub struct MatchmakerModule;

impl FeatureModule for MatchmakerModule {
    fn descriptor(&self) -> CapabilityDescriptor {
        CapabilityDescriptor::new("x.acme.matchmaker", "1.0", ChannelKind::Request)
            .with_auth(AuthTier::TrustedPeer)
    }

    fn on_message(&self, source: PeerId, payload: &[u8]) {
        // Parse, verify, route. The runtime has already enforced
        // auth tier + per-feature quotas before calling you.
    }
}

// Wire-up (e.g. in supernode `main.rs`):
let module = std::sync::Arc::new(MatchmakerModule);
if !state.features.bind_module("x.acme.matchmaker", module.clone()) {
    let _ = state.features.register_module(module);
}
```

`bind_module` attaches a module to a descriptor that's already loaded from the manifest; `register_module` adds both the descriptor and its module in one step. Inbound messages are delivered through `FeatureRegistry::dispatch_message(id, source, payload)`.

### In-app portal games (`game.relay.v1`)

Games run only inside the native client portal. The SDK is served at `/web-sdk/conquerd.mjs` and imported with a relative path:

```js
import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

// Games run only inside the native portal (window.conquerd).
const client = new ConquerdClient({
  features: ["game.relay.v1"],
  room: "my-lobby",
});

client.on("connected",    (peerId) => { /* ... */ });
client.on("datagram",     (featureId, data) => { /* handle inbound relay datagram */ });
client.on("disconnected", ()       => { /* ... */ });
client.on("error",        (e)      => { /* ... */ });

await client.connect();   // opens portal channel over identity QUIC relay
client.sendDatagram("game.relay.v1", myPayload);
```

`window.conquerd.ready` exposes portal channel APIs (`openChannel`, `sendDatagramB64`, `pollDatagrams`, `closeChannel`) and `myPeerId` from the native trust chain — no host/port/cert parameters.

Three bundled game demos are deployed to `<data_dir>/games/` on first supernode start:

| Path | Description |
|------|-------------|
| `/games/example/` | Cursor relay — real-time shared cursor canvas |
| `/games/brick-breaker/` | Brick breaker — multiplayer paddle game |
| `/games/shared-drawing/` | Shared drawing — collaborative canvas with stroke broadcast |

All three use `game.relay.v1` and open only via `conquerd://<supernode_id>/games/<slug>/` from the in-app portal (Rooms sidebar). External browsers are not supported.

The SDK also exports `ChannelTag`, `encodeFrame`, `decodeFrame`, `fixedTagFor`, and `featureForFixedTag` for games that interoperate with first-party `core.*` channels.

### Quota and Trust Defaults

Anything outside the `core.*`, `transport.*`, `room.*`, `web.*`, `game.*` namespaces that ships without explicit `quota_bytes_per_sec` / `quota_datagrams_per_sec` is pinned to a conservative default (64 KB/s, 256 datagrams/s) so a buggy or hostile third-party module cannot saturate the link before the user explicitly trusts it.

Quota enforcement is **symmetric** — separate inbound and outbound token-bucket registries guard both directions. Outbound chat and file messages are gated through `FeatureRegistry::gate_through_feature` before signing and transmitting; outbound audio datagrams are gated via `ConnectionManager::send_audio_datagram`. All per-peer quota buckets are cleared on disconnect to prevent state leaking into the next session.

---

## Connecting to Peers

Conquerd uses an **invite-only** model. There is no user directory or friend search.

### Creating an Invite
1. Click the **"+"** button or use the invite dialog.
2. Copy the generated invite link (or scan/save the QR code).
3. Share it with the person you want to connect with (via any channel — email, Signal, in person, etc.).

### Joining via Invite
1. Receive an invite link from a peer.
2. Paste it into the **Join** dialog in Conquerd.
3. The handshake completes automatically — both peers verify each other's identity cryptographically.
4. The peer appears in your left panel as a trusted contact.

### URI Launch
If Conquerd is installed, clicking a `conquerd://invite/...` link opens the app and processes the invite automatically.

---

## NAT Traversal

Most consumer NATs silently drop unsolicited inbound connections. Conquerd employs a multi-layer connection strategy to maximise reachability — each layer is tried in sequence and the first to succeed wins.

| Priority | Strategy | Requires | Transport |
|---|---|---|---|
| 0 | **Direct QUIC** | Peer's QUIC endpoint known (from the invite, a prior session, or relay hints) | QUIC (quinn) |
| 1 | **WebSocket via a connected supernode** | A mutually trusted supernode already connected | WebSocket (TCP) |
| 2 | **WebSocket to the peer's own endpoint** | Peer's invite/stored endpoint reachable | WebSocket (TCP) |
| 3 | **WebSocket to a LAN hint** | Both peers on the same network | WebSocket (TCP) |
| 4 | **Supernode QUIC relay** | Both peers trusted by a common supernode | QUIC relay via supernode |

Candidate ordering lives in `build_ws_candidates` (`connection_fallback.rs`) and is de-duplicated,
so a supernode that is already connected is tried before anything that needs a fresh dial.

For most home users, **no manual port configuration is needed** — UPnP handles it automatically. If you're behind a strict firewall, forward one TCP port for signaling and let QUIC use ephemeral UDP ports.

### Do I Need a Supernode?

**In many cases, no.**

You **don't** need a supernode if:
- You and your peers are on the **same local network** (LAN).
- You have a **public IP** or your router supports **UPnP** (most home routers do).
- You only do **1-on-1 calls** (direct peer-to-peer).

You **do** need a supernode if:
- Both you and your peer are behind **strict NATs** that block direct connections (double-NAT, CGNAT, corporate firewalls).
- You want **group voice rooms** (SFU) with 3+ participants.
- You want a **reliable rendezvous** across restarts — the supernode's endpoint mailbox (24 h TTL) is how peers relocate each other after an address change.

### On STUN and UDP hole punching

Neither is implemented in the current Rust client, and the supernode relay covers the cases they
would have. Two leftovers are worth knowing about so they are not mistaken for working features:

- The supernode still implements **coordinated hole punch** (`PUNCH_REGISTER` → `PUNCH_READY`
  with a synchronised `punch_at`), and the message types remain in the protocol enum, but the
  client never registers for it. It is server-side scaffolding awaiting a client.
- The invite envelope carries a `udp_hole_punch_hint` field that is always `null`.

Peers behind symmetric NAT (roughly 10–20 % of users, mostly mobile carrier and corporate
networks) therefore connect through a supernode relay rather than punching through.

---

## Supernode Relay

A supernode is a volunteer peer that provides QUIC relay and SFU (group voice) hosting. Supernodes are **content-opaque transport helpers**: they persist their own identity and trusted-peer public records for access control, but they are not identity authorities and never store message bodies, room definitions, chat history, audio, or file content. SFU rooms are held in memory while active and dropped after ~15 minutes idle; clients rematerialize saved rooms from `my_rooms.dat` on reconnect.

### Connecting to a Supernode
1. Get the supernode's invite link from the operator.
2. Paste it into Conquerd's **Join** dialog.
3. The handshake completes — the supernode appears in the **Rooms** sidebar. Supernodes are intentionally excluded from the ordinary **Peers** list.
4. Your session banner shows the connection mode:

| Banner | Meaning |
|--------|---------|
| **Direct P2P** (green) | Connected directly to your peer — no relay |
| **Relay** (green) | Using a supernode relay (free) |
| **Relay (gated)** (yellow) | Using a gated supernode — access was granted via the portal |

### Supernode Access Modes

Each supernode operator decides how peers gain relay access:

| Mode | What you see as a peer |
|------|------------------------|
| **Open** | Relay access is granted immediately — nothing to do. |
| **Terms of Service** | A web page opens asking you to accept the operator's terms before access is granted. |
| **Access Code** | A web page asks for a code provided by the operator (e.g. shared in a group chat). |
| **Ad / timer** | A countdown page is shown; access is granted after the timer expires. |

When a gated supernode requires portal access, Conquerd opens the supernode's web page in the in-app portal view. Complete the required step and relay access is granted automatically.

Operators can add custom access-controller code that integrates other verification or payment systems. No wallet or payment infrastructure is built into the Conquerd client itself.

---

## Running a Supernode

Running a supernode is **optional**. Peers who can connect directly to each other do not need one at all.

### Pre-built binaries

GitHub Releases (tagged + `nightly`) include standalone supernode packages with SHA-256 sidecars:

| Platform | Release asset |
|---|---|
| Linux x86_64 | `conquerd-supernode-<version>-linux-x86_64.tar.gz` |
| Linux ARM64 | `conquerd-supernode-<version>-linux-aarch64.tar.gz` |
| Windows x86_64 | `conquerd-supernode-<version>-win64.zip` |

See [`docs/SUPERNODE.md`](docs/SUPERNODE.md) for install examples. Nightly builds use the `conquerd-supernode-nightly-<platform>.*` naming on the rolling `nightly` release.

### Basic Setup (build from source)

Build the Rust supernode binary (one time):

```bash
cd rust/conquerd-supernode
cargo build --release
```

Or package a redistributable archive locally:

```bash
# Linux / macOS
CONQUERD_RELEASE=1 ./scripts/build_supernode.sh

# Windows
$env:CONQUERD_RELEASE = '1'
.\scripts\build_supernode.ps1
```

#### Windows
```bat
set CONQUERD_HOME=%USERPROFILE%\.conquerd
set supernode_invite_ttl=-1
set supernode_port=3478
set supernode_signaling_port=34935
rust\target\release\conquerd-supernode.exe
```

Or use the bundled helper:
```bat
start_supernode.bat
```

#### Linux / macOS
```bash
export CONQUERD_HOME="$HOME/.conquerd"
export supernode_invite_ttl=-1
export supernode_port=3478
export supernode_signaling_port=34935
./rust/target/release/conquerd-supernode
```

Or use the bundled helper:
```bash
./start_supernode.sh
```

On startup, the supernode will:
1. Generate an Ed25519 identity (first run only).
2. Print an **invite link** to the console — share this with your peers.
3. Begin accepting QUIC relay connections on port `3478` (UDP).
4. Begin accepting WebSocket signaling connections on port `34935` (TCP).

The invite link is also persisted in `~/.conquerd/reusable_invite.json` and survives restarts.

### Supernode Configuration

Runtime ports and access settings are read from environment variables; hosted feature capabilities are preferably declared in `<data_dir>/supernode.toml`.

#### Core Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `supernode_port` | `3478` | UDP port for QUIC relay traffic |
| `supernode_signaling_port` | `34935` | TCP port for WebSocket signaling. **Always set a fixed value** — changing it breaks firewall rules and stored peer endpoints |
| `supernode_invite_ttl` | `-1` | Invite expiry in minutes. `-1` = never expires |
| `supernode_host` | *(unset)* | Public DNS name or IP used in invite URLs and relay tickets for remote clients |
| `CONQUERD_HOME` | `~/.conquerd` | Data directory for identity, settings, files |

#### Feature / process toggles

Prefer `supernode.toml` for capability advertisement (see [Enabling Features on a Supernode](#enabling-features-on-a-supernode)). These env vars still gate runtime subsystems (e.g. whether the SFU process starts) and stats labels — they do **not** synthesize a capability list when the manifest is absent.

| Variable | Default | Description |
|----------|---------|-------------|
| `supernode_chat` | `1` | Report chat as enabled in stats (`0` to hide) |
| `supernode_files` | `1` | Report files as enabled in stats (`0` to hide) |
| `supernode_sfu` | `1` | Start the in-process SFU room manager (`0` to disable) |
| `supernode_updates` | `1` | Enable P2P auto-update distribution (`0` to disable) |
| `supernode_auto_restart` | `1` | Auto-restart after applying an update (`0` to disable) |

#### Portal / Access Control Settings

The in-app portal is served over QUIC (`web.host.app.v1`) — there is **no** public HTTPS/WebTransport port and **no** TLS game certs.

| Variable | Default | Description |
|----------|---------|-------------|
| `supernode_web_title` | `Relay Node` | Human-readable name shown on the in-app portal |
| `supernode_access_mode` | `open` | Access mode: `open`, `tos`, `ad`, `code` |
| `supernode_access_code` | `conquerd` | Access code (only used when mode is `code`) |
| `supernode_ad_duration` | `30` | Countdown seconds (only used when mode is `ad`) |
| `supernode_ad_content` | *(empty)* | HTML content for the ad/timer waiting area |
| `supernode_tos_text` | *(built-in)* | Custom TOS text (or override `portal/tos.html`) |

### Firewall and Port Forwarding

Peers need to reach your supernode on two ports:

| Port | Protocol | Purpose |
|------|----------|---------|
| `3478` (or your `supernode_port`) | **UDP** | QUIC relay — voice, files, data |
| `34935` (or your `supernode_signaling_port`) | **TCP** | Signaling, chat, presence |

**Router / cloud firewall**: Forward both ports to your supernode's local IP.

**Linux firewall** (example with `ufw`):
```bash
sudo ufw allow 3478/udp
sudo ufw allow 34935/tcp
```

**Windows Firewall**: The first launch will prompt you to allow `conquerd-supernode.exe` through the firewall. Accept both private and public network access.

### Running as a systemd Service (Linux)

Create a dedicated user:

```bash
sudo useradd -r -s /usr/sbin/nologin -m -d /opt/conquerd conquerd
sudo -u conquerd git clone <repo-url> /opt/conquerd/app
sudo mkdir -p /opt/conquerd/.conquerd
sudo chown conquerd: /opt/conquerd/.conquerd
```

#### Option A — Pre-built or Rust binary (recommended)

Install from a GitHub Release tarball (no Rust toolchain on the server):

```bash
# x86_64 VPS example
tar -xzf conquerd-supernode-1.0.0-linux-x86_64.tar.gz
sudo install -m 755 conquerd-supernode-1.0.0-linux-x86_64/conquerd-supernode /usr/local/bin/
```

Or build from source once:

```bash
. "$HOME/.cargo/env"
cd /opt/conquerd/app/rust/conquerd-supernode
cargo build --release
sudo cp target/release/conquerd-supernode /usr/local/bin/
```

Create `/etc/systemd/system/conquerd-supernode.service`:

```ini
[Unit]
Description=Conquerd Supernode (QUIC relay + SFU)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=conquerd
Group=conquerd

Environment=CONQUERD_HOME=/opt/conquerd
Environment=supernode_invite_ttl=-1
Environment=supernode_port=3478
Environment=supernode_signaling_port=34935

# In-app portal title / access (portal is QUIC — no public web_port)
#Environment=supernode_web_title=My Relay Node
#Environment=supernode_access_mode=open
# Required so remote peers get routable invite/relay tickets:
#Environment=supernode_host=relay.example.com

ExecStart=/usr/local/bin/conquerd-supernode
Restart=on-failure
RestartSec=5

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/opt/conquerd
PrivateTmp=true

[Install]
WantedBy=multi-user.target
```

#### Enable and start

```bash
sudo systemctl daemon-reload
sudo systemctl enable conquerd-supernode
sudo systemctl start conquerd-supernode
```

Retrieve the invite link:
```bash
sudo journalctl -u conquerd-supernode | grep 'Invite URL'
```

### Portal Customisation

The supernode portal supports **two-tier template loading**:

1. **User overrides:** `$CONQUERD_HOME/.conquerd/portal/` (checked first)
2. **Built-in defaults:** embedded HTML in the Rust binary

To customise any page, create a complete HTML file with the same name in your portal override directory. Use `{{variable}}` placeholders for dynamic content.

| Template file | Purpose |
|--------------|--------|
| `homepage.html` | Supernode homepage (shown to connected peers) |
| `portal.html` | Default access gating entry page |
| `tos.html` | Terms of Service gate page |
| `ad.html` | Countdown/timer gate page |
| `code.html` | Access code entry page |
| `granted.html` | “Access granted” confirmation |
| `denied.html` | “Session expired / invalid” error |
| `health.html` | Stats dashboard with live relay/SFU metrics |

The Rust binary embeds all templates at compile time — overrides are loaded from disk at runtime and take precedence without requiring a rebuild.

### Supernode Stats Dashboard

The in-app QUIC portal exposes a `/health` page with live stats (uptime, version, connected/trusted peers, QUIC relay stats, SFU room details) and an `/api/stats` JSON endpoint. Access is bound to the caller's authenticated QUIC identity rather than a separate HTTP session token. The dashboard auto-refreshes every 30 seconds; gated guests are directed through the in-app access page before receiving full relay access.

---

## Updates

Conquerd checks the GitHub Releases API in the background and offers in-app upgrade prompts when a newer signed release is available. The bundled `conquerd-installer` binary downloads, verifies, and applies the release.

- Before replacing files, the installer verifies the project Ed25519 release manifest and the archive SHA-256 recorded in it. Platform signatures (Windows SignPath / macOS Apple Developer ID) and Sigstore attestations are additional distribution checks when available.
- `VERSION_ANNOUNCE` is still exchanged between peers so each side can show the other peer's version in the event log, but application code is **not** pushed peer-to-peer — a connected peer running an older build is informational only.
- The *Check for updates* setting is persisted but background checks are not yet gated on that preference in 1.0.0; block `api.github.com` or run on a restricted network to prevent the startup check.

---

## Settings Reference

Access settings via the gear icon in Conquerd.

### Network

| Setting | Default | Description |
|---------|---------|-------------|
| Direct P2P | `true` | Accept direct peer QUIC sessions (`direct_p2p_enabled`) |
| Direct P2P port | `0` (auto) | Fixed listening port when you need a firewall rule (`direct_p2p_port`) |
| UPnP port mapping | `true` | Auto port-mapping on the router (`upnp_enabled`) |
| Relay port | (auto) | Local port used for supernode relay sessions (`relay_port`) |
| Allow gated supernodes | `true` | Allow supernodes that require portal access (`relay_allow_gated`) |
| Auto-renew relay tickets | `true` | Keep relay access alive without a reconnect (`relay_auto_renew`) |
| Auto-connect | `true` | Reconnect trusted peers on startup (`auto_connect`) |

### Audio

| Setting | Default | Description |
|---------|---------|-------------|
| Input device | (system default) | Microphone selection |
| Output device | (system default) | Speaker/headphone selection |
| PTT key | `Space` | Push-to-talk key binding |
| Voice activation | `false` | Auto-transmit when sound detected |
| Noise suppression | `true` | Background noise reduction |
| Jitter buffer depth | `3` | Frames (1–20). Higher = more latency, smoother audio |

### Video

| Setting | Default | Description |
|---------|---------|-------------|
| Camera source | (none) | Capture device — or a monitor/window on Windows — with a live local preview (`video_input_device`) |
| Quality preset | `balanced` | Sets the **ceiling** for resolution, frame rate, and bitrate; adaptation only moves downward from it (`video_quality`) |
| Adaptive bitrate | `true` | Let measured loss lower the bitrate. Off pins the stream at the chosen rate (`video_adaptive_bitrate`) |
| Codec | `auto` | Preferred encoder where more than one is available; `auto` negotiates (`video_codec`). H.264 is Windows-only, VP8 is everywhere |
| Keyframe interval | `4 s` | How often a full frame is sent — the recovery point after loss (`video_keyframe_secs`) |
| Shared audio | `auto` | Which audio accompanies a share: `auto` follows the source, `system` always shares the machine, `off` shares picture only (`content_audio_mode`). Also asked when a share starts |
| Picture-in-picture | (none) | Extra sources composited over the main one *before* encoding — a webcam over a game — so overlays cost no extra stream (`video_overlays_json`) |

### Security

| Setting | Default | Description |
|---------|---------|-------------|
| Attestation policy | `warn` | `off` = no peer build checks; `warn` = challenge but don't block; `strict` = deny relay to unverified peers |

---

## Data and Files

All Conquerd data is stored under `CONQUERD_HOME` (default `~/.conquerd/`):

| File | Purpose |
|------|---------|
| `identity.dat` | Your Ed25519 keypair (v2, encrypted at rest) — **back this up** |
| `identity.json` | Legacy v1 plaintext identity (read-only after migration, if present) |
| `peers.dat` | Trusted peers list (encrypted) |
| `chat_history.db` | Chat messages (SQLite; message bodies encrypted at rest) |
| `settings.json` | All preferences |
| `my_rooms.dat` | Client-owned SFU room definitions per supernode (encrypted); used to rematerialize rooms on reconnect. Sidebar hide list is stored here too. |
| `installer.log` | Installer/updater activity (when `conquerd-installer` runs) |

Received files are saved to your OS **Downloads** folder on completion (not under `CONQUERD_HOME`). The desktop client logs through `tracing` to stderr and to the current-session file `~/.conquerd/logs/conquerd-client.log` (truncated on each launch). The **Verbose debug logging** setting changes the runtime/file filter immediately; an explicit `RUST_LOG` overrides it. An optional OS keyring entry (`conquerd` service) caches your unlock key locally.

Supernodes additionally store:

| File | Purpose |
|------|---------|
| `identity.json` | The supernode's Ed25519 node identity |
| `reusable_invite.json` | Persistent invite payload/link |
| `supernode_endpoints.json` | Endpoint mailbox for peer reconnection (24h TTL) |
| `peers.json` | Trusted peer records for relay/SFU access control |
| `supernode.toml` | Typed listener, cluster, and hosted-capability manifest (when operator-provided) |
| `trusted_module_keys.txt` | Optional trusted signer keys for native feature modules |

SFU **room state is not persisted** on the supernode — rooms exist in memory while in use and are idle-GC'd after ~15 minutes empty. Room definitions and chat history live on clients.

> **What to back up**: At minimum, back up `identity.dat` (and your passphrase). Losing it means peers will see you as a new, untrusted identity.

---

## Troubleshooting

### Can't connect to a peer
- Both peers need network reachability — check UPnP status in settings.
- Ensure both peers completed the invite handshake (trusted peer appears in left panel).
- Try a supernode as relay if direct connections fail.
- If neither peer is directly reachable, both need to trust a common supernode — that relay is the fallback path, since the client does not hole-punch.

### No audio in calls
- Check audio input/output device selection in Settings.
- Verify microphone permissions (Windows: Settings → Privacy → Microphone).
- Try toggling between PTT and voice activation.
- Set `RUST_LOG=conquerd_client=debug` for detailed pipeline logging.

### Crash dumps
- Rust panic backtraces are written to `conquerd-client.log` in the working directory; set `RUST_BACKTRACE=1` for full traces.
- The `.bat`/`.sh` launchers keep the console window open after a crash so the trace is visible.

### Supernode troubleshooting
- **Peers can't connect**: Verify both `supernode_port` (UDP) and `supernode_signaling_port` (TCP) are forwarded and open. Set `supernode_host` to the public DNS name or IP when remote peers need to connect.
- **Port changes on restart**: Always set `supernode_signaling_port` to a fixed value (e.g. `34935`). Changing it breaks firewall rules and stored peer endpoints.
- **Service fails with exit code 226/NAMESPACE**: LXC, OpenVZ, or some VPS hosts don't support mount namespaces. Comment out the hardening block in the systemd unit file and restart.
- **Peers get relay access without the portal**: Ensure `supernode_access_mode` is set to `tos`, `ad`, or `code` (not `open`) so operators can gate grants without a public web surface.

---

## Developer Guide

### Entry Points

| File | Purpose |
|---|---|
| `rust/conquerd-client/src/main.rs` | Desktop client entry. Initialises identity (keyring + passphrase), Qt `QGuiApplication`, the `AppBridge` QObject, and the QML engine. Handles `conquerd://` URIs on argv and single-instance forwarding. |
| `rust/conquerd-supernode/src/main.rs` | Headless relay binary. Reads env / `supernode.toml`, starts QUIC relay + WebSocket signaling + in-app portal (`web.host.app.v1`) + game session fan-out. |
| `rust/conquerd-installer/src/main.rs` | Standalone updater. Downloads and applies signed releases from GitHub. |

### Building from Source
```powershell
# Desktop client (Qt UI)
cd rust\conquerd-client
cargo build --release --features qt-ui            # optionally: ,webengine,console

# Supernode + installer (server-side workspace)
cd ..
cargo build --release -p conquerd-supernode
cargo build --release -p conquerd-installer
```

`conquerd-client` lives in its own Cargo workspace (`rust/conquerd-client/Cargo.toml`) so Qt/CXX-Qt dependencies stay isolated from server-side builds. The outer workspace (`rust/Cargo.toml`) contains `conquerd-features`, `conquerd-supernode`, `conquerd-installer`, `conquerd-opus`, and `conquerd-vpx`. The cluster-operations tool is a third workspace at `rust/conquerd-supernode-manager/`.

### Run Tests
```powershell
# Outer workspace (features + supernode + installer + Opus)
cd rust
cargo test --workspace

# Conquerd-client (binary crate; tests run from its own workspace)
cd conquerd-client
cargo test

# Supernode manager (separate cluster-ops workspace)
cd ..\conquerd-supernode-manager
cargo test --workspace
```

See `agents.md` (Roadmap & Status) for current coverage areas and P0–P2 delivery status. Test inventories change frequently; use `cargo test -- --list` in each workspace when an exact count is needed. The suite emphasises capability negotiation, quota symmetry (inbound/outbound), replay protection, relay/SFU/room flows, and installer manifest verification.

### Coverage % (line / region)

LLVM source coverage via [`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) for the high-ROI packages (`conquerd-features`, `conquerd-supernode`, headless `conquerd-client`). Native Opus/DNN and Qt/QML UI are out of scope for the default report.

```powershell
# Windows — default "hot" scope; writes coverage/summary.md + .lcov/.json
powershell -ExecutionPolicy Bypass -File scripts/coverage.ps1

# Full product crates (adds installer); optional HTML
powershell -ExecutionPolicy Bypass -File scripts/coverage.ps1 -Scope all -Html

# Fail the process if any package is under N% lines (0 = report only)
powershell -ExecutionPolicy Bypass -File scripts/coverage.ps1 -FailUnderLines 50
```

```bash
# Linux / macOS
bash scripts/coverage.sh
bash scripts/coverage.sh --scope all --html
bash scripts/coverage.sh --scope features --fail-under-lines 50
```

CI runs the same hot-scope report on every push/PR (`Rust coverage %` job) and uploads `coverage/` as an artifact. Floors are report-only until a baseline is chosen; raise them with `--fail-under-lines` / `-FailUnderLines` when ready.

### Local CI

Mirror the GitHub Actions `CI` workflow before pushing:

```powershell
# Windows (full: fmt, clippy, tests, audit)
.\scripts\ci_local.ps1

# Faster lint-only pass
.\scripts\ci_local.ps1 -SkipTests -SkipAudit
```

```bash
# Linux / macOS
bash scripts/ci_local.sh
```

CI also runs dedicated supernode packaging jobs on **Linux x86_64**, **Linux ARM64**, and **Windows** (`test-supernode-linux-x86_64`, `test-linux-arm64`, `test-supernode-windows` in `.github/workflows/ci.yml`).

### Two-Client Local Testing

The profile directories `.clientA/` and `.clientB/` (at the repo root) are populated by overriding `USERPROFILE` (Windows) or `HOME` (Linux/macOS) before launching the binary. There is no longer a separate launcher per profile — use `run_client.bat` after pointing the home directory at the desired profile:

```powershell
# Terminal 1
$env:USERPROFILE = "$PWD\.clientA"
.\run_client.bat

# Terminal 2
$env:USERPROFILE = "$PWD\.clientB"
.\run_client.bat
```

### Portable Build

```powershell
# Windows client: dist\ConquerD\ + dist\ConquerD-<version>-win64.7z
.\build_win64.ps1

# Linux client AppImage
./build_linux.sh

# macOS client DMG
./build_macos.sh

# Supernode (standalone relay binary — no Qt)
$env:CONQUERD_RELEASE = '1'
.\scripts\build_supernode.ps1    # dist\conquerd-supernode-<version>-win64.zip
```

```bash
# Supernode on Linux / macOS
CONQUERD_RELEASE=1 ./scripts/build_supernode.sh   # dist/conquerd-supernode-<version>-<platform>.tar.gz
```

Release CI builds client artifacts plus supernode packages for **linux-x86_64**, **linux-aarch64**, and **win64** (see `.github/workflows/release.yml`).

`build_win64.ps1` runs `cargo build --release --features qt-ui[,webengine]` for `conquerd-client`, `cargo build --release -p conquerd-installer`, then invokes `windeployqt6` to gather the Qt runtime DLLs into `dist\ConquerD\`. Set `QT_DIR` if Qt is not in one of the auto-detected default locations. Set `CONQUERD_DEBUG=1` for a debug build, or `CONQUERD_DEBUG_CONSOLE=1` to keep a console window attached.

#### Code Signing (Windows, optional)

The build script automatically signs `conquerd-client.exe` and `conquerd-installer.exe` when a certificate is configured. Signing is **optional** — the build completes without it.

`signtool.exe` must be on `PATH`. Install it via:
- **Visual Studio Installer** → Modify → Individual Components → search "Windows SDK" (e.g. Windows 11 SDK 10.0.26100.x) — signing tools are included.
- **Standalone** — [Windows SDK installer](https://developer.microsoft.com/windows/downloads/windows-sdk/) → check "Windows SDK Signing Tools for Desktop Apps".

After install, add the `x64` bin path (e.g. `C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0\x64`) to your `PATH`, or open a Visual Studio Developer Command Prompt.

Configure signing via environment variables before running `build_win64.ps1`:

| Variable | Description |
|---|---|
| `CONQUERD_SIGN_THUMBPRINT` | SHA-1 thumbprint of a cert in the Windows Certificate Store (recommended for CI / EV tokens) |
| `CONQUERD_SIGN_PFX` | Path to a `.pfx` file (OV cert, local builds) |
| `CONQUERD_SIGN_PASSWORD` | Password for the `.pfx` file |
| `CONQUERD_SIGN_TIMESTAMP` | RFC 3161 timestamp server URL (default: `http://timestamp.digicert.com`) |
| `CONQUERD_SIGN_AUTO` | Set to sign with the best-available cert in the user store |

If none of these are set the signing step is silently skipped.

### Version Bumping

Version is set in `rust/conquerd-client/Cargo.toml`. **Keep `rust/conquerd-installer/Cargo.toml` in sync** — SignPath requires consistent `ProductVersion` across all signed PE files. `scripts/check_version_sync.ps1` verifies the two values match.

**When to bump:**
- Sprint / feature-batch complete → bump **minor** (or **major** for breaking protocol changes)
- Bug-fix round complete → bump **patch**

**Do not bump** for mid-feature saves, tests/docs-only changes, or whitespace edits.

### Project Structure

```
├── rust/
│   ├── Cargo.toml                 # Outer workspace: features + supernode + installer + Opus
│   ├── conquerd-client/           # Native desktop binary (own workspace; Qt 6 / QML via CXX-Qt)
│   │   ├── Cargo.toml             # features: qt-ui, webengine, console
│   │   ├── build.rs               # CXX-Qt codegen + windres icon embedding
│   │   ├── assets.qrc / icons.qrc # Qt resource bundles (QML + icons)
│   │   ├── qml/                   # MainWindow, ChatPanel, CallPanel, RoomPanel, SettingsPage, …
│   │   └── src/
│   │       ├── main.rs            # Entry point: identity init, QGuiApplication, QML engine
│   │       ├── identity.rs        # Ed25519 keypair, keyring AES key, passphrase handling
│   │       ├── connection_manager/ # Invite handshake, signaling, peer tracking
│   │       ├── connection_fallback.rs # WS candidate ordering + direct-call → temp SFU room fallback
│   │       ├── call_controller.rs # Call state machine; audio + QUIC peer wiring
│   │       ├── chat_store.rs      # SQLite chat history (per-peer trim_by_age / count / purge)
│   │       ├── file_transfer.rs   # P2P file send/receive with chunking + progress
│   │       ├── video/             # Camera/screen capture, codec registry (H.264/VP8), composite PiP,
│   │       │                      #   fragmentation, sender ABR, per-sender decode
│   │       ├── content_capture.rs / content_sender.rs / content_playout.rs / content_audio.rs
│   │       │                      # Audio shared *with* a video: loopback capture, send, wire
│   │       │                      #   format, jitter-buffered playout (separate from the mic path)
│   │       ├── media_clock.rs / media_sync.rs # One session clock per video call; audio-led A/V sync
│   │       ├── group_key.rs       # Per-room sender keys (E2E seal for room chat/voice/file/video)
│   │       ├── space.rs           # Space Merkle tree (signed roots, inclusion proofs, grants)
│   │       ├── sfu_client.rs      # SFU membership (join/leave/member list)
│   │       ├── room_manager.rs    # Per-participant room state
│   │       ├── room_store.rs      # Encrypted client-owned room definitions (my_rooms.dat)
│   │       ├── cluster.rs         # Verified cluster roster from SUPERNODE_INFO; member failover
│   │       ├── quic_relay_client.rs # QUIC relay client (room audio + signaling fallback)
│   │       ├── quic_tls.rs        # rustls config for QUIC peer/relay sessions
│   │       ├── metrics.rs / network_monitor.rs # Per-peer QUIC stats → ConnectionQuality
│   │       ├── peer_store.rs      # Trusted peer persistence (incl. AvatarConfig)
│   │       ├── avatar_config.rs   # Deterministic SVG identicon generator
│   │       ├── feature_trust.rs   # FeatureTrustStore + user-consent gate for bespoke namespaces
│   │       ├── plugin_manager.rs / plugin_runtime.rs / ollama_module.rs # Plug-in host + AI plugin
│   │       ├── github_updater.rs  # GitHub Releases API poll + installer spawn
│   │       ├── ringtone.rs / taskbar_badge.rs / upnp.rs / uri_scheme.rs / web_app_client.rs
│   │       └── ui/                # AppBridge QObject + QML models (Peer/Chat/Call/Room/Settings/FileTransfer)
│   ├── conquerd-features/         # rlib: capability registry, FeatureModule trait, quota enforcement
│   ├── conquerd-supernode/        # Standalone binary: QUIC relay, ephemeral SFU, WS signaling, in-app portal
│   ├── conquerd-installer/        # Standalone binary: signed-release download + apply
│   ├── conquerd-opus/             # First-party libopus wrapper (DRED + OSCE)
│   ├── conquerd-vpx/              # First-party VP8 wrapper over vendored libvpx (cross-platform video)
│   └── conquerd-supernode-manager/ # Separate workspace: provisioning, cluster sync, deploy, remote exec
├── web-sdk/conquerd.mjs           # In-app portal game SDK (identity QUIC channel; no WebTransport)
├── games/                         # Example portal games (conquerd:// only)
├── packaging/                     # Linux .desktop file, macOS Info.plist template, AppRun
├── scripts/check_version_sync.ps1 # Verify Cargo.toml versions stay aligned (PowerShell)
├── scripts/build_supernode.sh     # Package conquerd-supernode (.tar.gz; Linux/macOS hosts)
├── scripts/build_supernode.ps1    # Package conquerd-supernode (.zip; Windows hosts)
├── scripts/ci_local.ps1 / ci_local.sh  # Local mirror of .github/workflows/ci.yml
├── build_win64.ps1                # Portable Windows build (cargo + windeployqt6 + optional sign + 7z)
├── build_linux.sh / build_macos.sh
├── run_client.bat / run_client.sh # Debug-build launchers
├── start_supernode.bat / start_supernode.sh
├── agents.md / PRIVACY.md
└── README.md
```

---

## Protocol Messages

| Category | Types |
|----------|-------|
| Invite | `invite_handshake_init`, `invite_handshake_accept`, `invite_handshake_reject` |
| Chat | `chat_message`, `chat_ack`, `chat_typing` |
| Call | `call_request`, `call_accept`, `call_reject`, `call_end` |
| Relay | `relay_request`, `relay_granted`, `relay_revoke` |
| Relay Access | `relay_payment_required`, `relay_access_granted`, `relay_access_denied` |
| Supernode Info | `supernode_info`, `supernode_info_request` |
| Room Mgmt | `hello`, `welcome`, `room_join`, `room_leave`, `room_state`, `room_peer_joined`, `room_peer_left`, `room_list_request`, `room_list_response` |
| SFU / Room | `sfu_join`, `sfu_join_result`, `sfu_leave`, `sfu_members`, `sfu_offer`, `sfu_answer`, `sfu_audio`, `sfu_chat`, `sfu_room_list`, `sfu_peer_joined`, `sfu_peer_left` |
| SFU Subscription | `sfu_subscribe`, `sfu_unsubscribe` |
| SFU Room Mgmt | `sfu_room_create`, `sfu_room_created`, `sfu_room_invite`, `sfu_room_invite_result`, `sfu_room_invite_generate` |
| SFU File Transfer | `sfu_file_offer` (advertisement), `sfu_file_request` (a member accepting), `sfu_file_revoke` (sender withdrew it), `sfu_file_chunk`, `sfu_file_complete` |
| SFU Video (control plane only) | `sfu_video_state` (camera on/off + join-time reannounce), `sfu_video_keyframe_request` |
| SFU Group Key | `sfu_group_key`, `sfu_group_key_ack` (inside `encrypted_signal`) |
| Space Tree | `space_root_announce` |
| File Transfer | `file_transfer_offer`, `file_transfer_accept`, `file_transfer_reject`, `file_transfer_chunk`, `file_transfer_complete`, `file_transfer_ack`, `file_transfer_error` |
| Trust | `trust_request`, `trust_accept` |
| Peer Room Invite | `peer_room_invite` |
| Hole Punch (supernode-side only; no client participates today) | `punch_register`, `punch_ready` |
| Endpoint | `endpoint_update` |
| Handle | `handle_update` |
| Avatar | `avatar_config` |
| Encrypted | `encrypted_signal` |
| Peer Updates | `version_announce` |
| Build Attestation | `build_attestation`, `attestation_response` |
| Capability | `capability_announce`, `capability_invoke` |
| Game Relay | `game_relay_join`, `game_relay_leave`, `game_relay_joined` |
| Utility | `ping`, `pong`, `error`, `speaking_state`, `presence_update` |

## Technology Stack

| Layer | Library / Runtime |
|---|---|
| UI | Qt 6 / QML via [CXX-Qt](https://kdab.github.io/cxx-qt/) |
| QUIC transport | `quinn` + `tokio` + `rustls` |
| Audio capture / playback | `cpal` + `ringbuf` |
| Audio codec | `conquerd-opus` — first-party libopus 1.6.x wrapper (vendored submodule) with DRED (Deep Redundancy Encoding) and OSCE (Opus Speech Coding Enhancement) neural voice enhancement. DNN model data compiled in from Xiph.Org source arrays; no third-party crate dependency. |
| Video codecs | `conquerd-vpx` — first-party VP8 wrapper over a vendored libvpx submodule, built without libvpx's own `configure`/`make` (needs **perl** for RTCD codegen). Available on every platform. Media Foundation H.264 on Windows via the `windows` crate, using the codec licence the OS already holds. |
| DSP | `rustfft` (spectral-gate noise suppression), in-house VAD + jitter buffer |
| Cryptography | `ed25519-dalek`, `x25519-dalek`, `aes-gcm`, `argon2`, `hkdf` |
| Signaling serialisation | JSON over WebSocket (`tokio-tungstenite`) and QUIC bidirectional streams |
| Local storage | SQLite (`rusqlite`) for chat history; JSON for settings/peers/rooms |
| Testing | `cargo test` |
| Packaging | `cargo`, `windeployqt6`, 7-Zip (Windows); AppImage (Linux); `.dmg` (macOS) |

## Code Signing Policy

Free code signing provided by [SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org).

### Team Roles

| Role | Members |
|---|---|
| **Authors** (trusted committers) | [Members](https://github.com/orgs/ConquerD/teams/conquerd-authors) |
| **Reviewers** (PR reviewers) | [Members](https://github.com/orgs/ConquerD/teams/conquerd-reviewers) |
| **Approvers** (release signing) | [Owners](https://github.com/orgs/ConquerD/teams/conquerd-approvers) |

### Bootstrap for Free OSS Code Signing

ConquerD uses a project-controlled Ed25519 key for signing `releases_manifest.json` (verified by the installer for update integrity and build hashes). This key was generated locally with `openssl genpkey -algorithm Ed25519`.

The public key is committed in source (see `keys/release-signer-public.pem` and the hex constant in `rust/conquerd-installer/src/release_manifest.rs`).

A helper binary to produce signed manifests lives in the installer crate:

```
# 1. Generate a skeleton for the current version (no private key needed)
cargo run -p conquerd-installer --bin sign-release-manifest -- --generate-unsigned

# 2. Edit the generated releases_manifest.json: fill real build_hash (from the .sha256
#    asset or `sha256sum` of the final archive) + build_id (the value of CONQUERD_BUILD_ID
#    that was baked into the binaries for that release, visible via `--version` or attestation).

# 3. Sign it (approver only, with the offline private seed)
cargo run -p conquerd-installer --bin sign-release-manifest -- \
  -i releases_manifest.json -o releases_manifest.json \
  --private-key /path/to/secure/release-signer-private.pem
```

It accepts the PEM from `openssl genpkey` (after the `openssl pkey ... | tail -c 32` extract), raw 32-byte seed, or hex seed. It always uses BTreeMap canonicalization + Ed25519 over the no-`signature` object (matching `parse_and_verify` in the installer).

The signed `releases_manifest.json` is committed to the repo (public) and also attached as a release asset by the publish job. The installer fetches it (when present) and verifies before trusting any archive hash for an update.

Windows and macOS *binary* signatures (Authenticode / Apple Developer ID) are provided by SignPath Foundation and Apple programs. Because these services typically require a public OSS project with releases to approve free access, the first 1–2 releases may ship with unsigned (or self-signed) binaries. The release manifest is still signed with the project Ed25519 key from the start.

Once approved:
- Future releases use automated SignPath signing for the PE files.
- The Ed25519 manifest key remains the root of trust for `releases_manifest.json` (you control rotation).

Users downloading the very first release should verify the GitHub release page, checksums, and (when available) the manifest signature using the published public key. See the installer source for verification details.

### Privacy Policy

See [PRIVACY.md](PRIVACY.md) for the full privacy policy.

ConquerD is a local-first application. All peer-to-peer communication (voice, chat, file transfer) travels directly between clients or through volunteer supernodes chosen by the user, and is end-to-end encrypted. ConquerD does not operate servers that store your identity, messages, or call data.

The following network contacts occur automatically or on user action (see [PRIVACY.md](PRIVACY.md) for full detail):

| Feature | External service contacted | When | How to disable |
|---|---|---|---|
| **UPnP port mapping** | Your local router only (LAN multicast) | On startup when *Enable UPnP port mapping* is on (default) | Uncheck UPnP in Settings (`upnp_enabled` in `settings.json`) |
| **GitHub update check** | GitHub Releases API (`api.github.com/repos/vbawol/ConquerD/releases/latest`) | Once per client launch | Block `api.github.com`, or avoid running the client on restricted networks. The *Check for updates* toggle is saved in `settings.json` but not yet enforced in 1.0.0 |
| **YouTube / Vimeo inline preview** | Video host CDNs (e.g. `youtube.com`, `googlevideo.com`, `vimeo.com`) | Only when you expand an inline player or open a preview link — Qt WebEngine embed, not yt-dlp | Uncheck *Show YouTube preview cards in chat* in Settings |
| **Ollama assistant** (optional) | Your configured Ollama URL (default `http://localhost:11434`) | When the AI plugin is enabled and you use it | Turn off *Enable AI assistant* in Settings |
| **Supernode portal / gated relay** | The supernode operator you chose | When you open their portal or complete an access gate | Do not connect to that supernode |
| **Installer download** | GitHub release assets + `releases_manifest.json` | When you apply an in-app update | Do not apply updates |

No account credentials, message content, or contact lists are transmitted to ConquerD-operated servers (there are none). Build-attestation metadata (version / build id) may be exchanged directly between peers you connect to.

**Local capture** (camera, screen or window, and audio shared with a video) starts only when you turn the camera on, open the Settings preview, or start a share — never in the background. Captured media goes to the peers in that call and nowhere else: it is not recorded, not written to disk, and room media is E2E-sealed before it reaches a supernode. Two things are worth reading before you share: a whole-screen share includes anything that pops up over it, and per-application audio capture needs Windows 10 build 20348 or later — on older builds it **falls back to whole-machine audio**. Full detail in [PRIVACY.md](PRIVACY.md#camera-screen-and-shared-audio-capture).

## Release Notes

Detailed, per-version release notes are published with each [GitHub release](https://github.com/ConquerD/ConquerD/releases). The summary below covers the **1.0** milestone.

### 1.0 — Highlights

- **Zero-trust P2P architecture** — direct peer-to-peer; no central server stores your data. Ed25519 identity with derived peer IDs, invite-only discovery via signed `conquerd://` links, forward-secret handshakes (ephemeral X25519 + HKDF + AES-GCM).
- **Chat-first UX** — text is the primary interaction after connecting; voice is opt-in per conversation. Per-conversation scroll persistence, typing indicators, unread badges on taskbar + tray.
- **Voice calls** — low-latency Opus over QUIC, push-to-talk and voice activation, spectral-gate noise suppression, jitter buffer with de-click.
- **Video and screen sharing** — negotiated H.264/VP8, pre-encode picture-in-picture, a separately-mixed synchronised track for audio shared with the video, and adaptive bitrate. Complete on Windows; camera capture on Linux and macOS is built but unvalidated, and screen capture is Windows-only (see [Known limitations](#known-limitations)).
- **Rooms (multi-peer voice)** — client-owned room definitions (`my_rooms.dat`); supernodes host SFU sessions ephemerally over QUIC relay with chat/voice/file parity, idle GC, and reconnect materialization.
- **Game relay & in-app portal**: `game.relay.v1` opaque datagrams over the identity QUIC relay; three bundled demos (cursor relay, brick breaker, shared drawing) under `<data_dir>/games/`, opened only from the native portal at `conquerd://<supernode_id>/games/<slug>/`. No public WebTransport port or TLS game certs.
- **Supernode release binaries**: pre-built packages for Linux x86_64, Linux ARM64, and Windows x86_64 on GitHub Releases and nightlies (`scripts/build_supernode.sh` / `scripts/build_supernode.ps1`).
- **NAT traversal** — UPnP port mapping, QUIC/WebSocket direct connect, ordered WebSocket candidates, and supernode QUIC relay fallback with auto-renewed tickets and an endpoint mailbox.
- **Security** — signed, transcript-bound signaling with timestamp freshness checks and per-sender replay deduplication; peer revocation with propagation; release-signed P2P updates with Ed25519 + threshold validation; crash/installer logging.
- **Desktop application** — DPI-aware dark theme, first-run onboarding wizard (display name, identity fingerprint + QR, supernode setup), `conquerd://` URI scheme for one-click invites, system tray with badges, collapsible event log, save-to-PNG invite QR codes.

### Known limitations

- Desktop only (no mobile clients) for 1.0.
- **Video is complete on Windows and unvalidated elsewhere.** The full path is built and encrypted end to end — VP8 on every platform (vendored libvpx), H.264 via Media Foundation on Windows, negotiated codecs, camera capture written for Windows/Linux/macOS, audio-led A/V synchronisation, and adaptive bitrate. What is not done is platform reach and proof: **screen and window capture are Windows-only**, **shared application audio is Windows-only**, and the Linux and macOS camera backends have not been run against real hardware. Treat video as working on Windows and provisional elsewhere; remaining items are tracked in `backlog.md`.
- Supernode discovery is manual (invite-link based).

## License
MIT
