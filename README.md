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

### Rooms (Multi-Peer Voice)
- SFU (Selective Forwarding Unit) room hosting on volunteer supernodes — **ephemeral in memory only**; the supernode does not persist room definitions or chat history.
- **Client-owned room definitions** — rooms you create, join, or subscribe to are saved in encrypted `my_rooms.dat` on your device, keyed by `(supernode, room_id)`. When you reconnect to a supernode, saved rooms are materialized automatically via `SfuRoomCreate` (without auto-joining voice).
- Idle user-created rooms are removed on the supernode after ~15 minutes with no voice participants or chat subscribers; the built-in `default` room is always present.
- QUIC relay transport for NAT-traversed room audio plus room chat/file signaling when available; WebSocket remains the membership and fallback signaling path.
- Room parity with direct-peer features: chat, voice, file transfer.
- Create public or private rooms from the Rooms sidebar; right-click **Remove room** hides the room locally (does not delete server-side state — there is none to delete).
- **Sub-rooms**: nest a room under any existing room (not just directly under the supernode) via **Create Public/Private Sub-room…**; the sidebar shows expand/collapse for rooms that have children.
- Private rooms can enable **"Members can invite"** so any current member — not just the creator — can mint new invite tokens for that room.
- Peer room invites with accept/decline flow.
- Up to 32 participants per room.
- Room (and sub-room) membership is backed by a client-signed, authenticated **Space** tree (Merkle inclusion proofs over room definitions) so any cluster member can admit a proven joiner even if it never saw the room's original grant; see `docs/SPACE-MERKLE-DESIGN.md`.

### In-App Supernode Portal & Browser Games
- Supernodes with `web_port` configured serve an in-app portal over a QUIC bidi-stream channel (`web.host.app.v1`). The native client browses `conquerd://` pages using an embedded Chromium view from the **Rooms** sidebar (supernode avatar click) — no external browser, no CA needed.
- A WebTransport listener (`web.host.h3.v1`) on the same port lets browser-side game clients join the exact same channel fabric as native peers using the `web-sdk/conquerd.mjs` JavaScript SDK. Ed25519 identity handshake is performed; the supernode verifies signed envelopes before any fan-out.
- **`game.relay.v1`** — opaque datagram relay: the supernode fans raw datagrams to all WebTransport peers in the same room without reading or modifying the payload. Three demo games are bundled: **cursor relay** (multi-cursor canvas), **brick breaker** (multiplayer paddle game), **shared drawing** (collaborative canvas).
- Game pages are served from `<data_dir>/games/<slug>/` and reachable at `conquerd://<supernode_id>/games/<slug>/`. The `window.conquerd` JS bridge is injected automatically; games call `window.conquerd.ready` to obtain the WebTransport URL and cert fingerprint via the ConquerD trust chain — no HTTPS CA required.
- Self-signed TLS cert (13-day validity, ECDSA P-256, `serverAuth` EKU) is generated automatically on first start and rotated after 7 days. The SHA-256 fingerprint is delivered to native clients in `SUPERNODE_INFO` and forwarded to game pages via `/_conquerd/ctx.json`.

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
- **Code Signing**: Windows (SignPath.io), macOS (Apple Developer Program), all platforms (GitHub Sigstore attestations).
- **Release Manifest**: Signed JSON listing official builds (version, platform, build_hash) verified by installer.
- **Peer Attestation**: Runtime challenges where peers prove they're running official builds via nonce-signed claims.
- **Policy Enforcement**: Configurable attestation policy (off/warn/strict) gating relay access for unverified peers.
- **CI/CD Hardening**: GitHub Actions pinned to immutable commit SHAs to prevent supply-chain attacks.

### NAT Traversal
- Multi-layer connection strategy: QUIC direct → WebSocket → UDP hole punch → supernode relay.
- STUN public IP discovery (Google `stun.l.google.com`, `stun1.l.google.com`; Cloudflare `stun.cloudflare.com`).
- UPnP automatic port mapping.
- UDP hole punching for peers behind non-symmetric NATs.
- Supernode-coordinated hole punch for timing alignment.
- QUIC relay fallback through trusted supernodes.

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
- Releases are code-signed (Windows via [SignPath.io](https://signpath.io), macOS via Apple Developer ID) and accompanied by Sigstore attestations.

---

## Quick Start

### Prerequisites
- Rust toolchain (stable, MSVC on Windows)
- Qt 6.x (e.g. `C:\Qt\6.8.3\msvc2022_64`); set `QMAKE` or `CMAKE_PREFIX_PATH`. Qt WebEngine is optional but required for the in-app supernode portal.
- A working audio input/output device
- No server, no account, no sign-up required
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
│         Optional: STUN / supernode QUIC relay (rooms)           │
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

`conquerd-client` is the sole desktop client. Built with `cargo build --features qt-ui` (from `rust/conquerd-client/`); requires Qt 6.x (`QMAKE` or `CMAKE_PREFIX_PATH`). Audio (CPAL + Opus + spectral-gate noise suppression + jitter buffer), crypto (Ed25519, X25519, AES-GCM, HKDF, Argon2id), and QUIC transport (quinn) are implemented as Rust modules inside `conquerd-client` — no separate extension crates required. Headless builds (without `--features qt-ui`) are used for CI integration tests.

### Transport Stack
- **Direct calls**: QUIC peer-to-peer via `ConnectionManager` (`quinn::Endpoint`, inside `conquerd-client`).
- **Room audio and room broadcasts**: QUIC relay (`QuicRelayClient` → supernode `QUICRelayServer`) for room audio plus room chat/file signaling when available; WebSocket handles room membership and remains the fallback signaling path.
- **Signaling/chat**: Ed25519-signed, transcript-bound messages; prefers QUIC signaling stream when a peer session is connected, falls back to WebSocket.
- **Relay**: QUIC relay protocol on supernodes (transport-only; no app-layer decryption).

### Core Model
- **Invite-only discovery**: peers connect only from signed `conquerd://` links.
- **Zero trust relay**: relays forward signed/encrypted payloads only — no app-layer central services.
- **Cryptographic identity**: long-term Ed25519 identity key; `peer_id` = SHA-256 of public key.
- **Forward secrecy**: invite handshakes use ephemeral X25519 + HKDF + AES-GCM.
- **Local trust graph**: successful handshakes persist to local peer store.
- **Endpoint stability**: signaling port persisted across restarts; peers notified of changes via `ENDPOINT_UPDATE`.
- **Auto-connect**: trusted peers can be marked for automatic reconnection on startup (30-second retry, up to 5 attempts).

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
  → QuicRelayClient.send_audio([0xFF][opus])   ← broadcast index
  ──────→ supernode QUICRelayServer
           distributes [peer_idx][opus] to each room member
  ──────→ QuicRelayClient.on_audio_received([peer_idx][opus])
  → OpusDecoder (per sender) → playback

Room membership (join/leave/state) flows over WebSocket. Room chat/file broadcasts prefer the QUIC relay signaling stream when a live relay session exists and fall back to WebSocket otherwise.
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
| `room.audio.sfu` | datagram | room-member | supernode `SfuRoomModule` |
| `room.chat.v1` | stream | room-member | supernode `SfuRoomModule` |
| `room.file.v1` | stream | room-member | supernode SFU file broadcast |
| `web.host.app.v1` | stream | public | supernode QUIC bidi-stream portal (`conquerd://` pages for native client) |
| `web.host.h3.v1` | datagram | public | supernode WebTransport (HTTP/3) listener — browser game clients |
| `game.relay.v1` | datagram | room-member | supernode opaque datagram relay for browser game sessions |

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
params = { codec = "opus", quota_bytes_per_sec = 32768 }

[[feature]]
id = "room.chat.v1"
enabled = true

[[feature]]
id = "room.file.v1"
enabled = true

[[feature]]
id = "web.host.h3.v1"
enabled = true                          # exposes /wt/<feature_id> over WebTransport

[[feature]]
id = "x.acme.matchmaker"                # bespoke third-party module
enabled = true
version = "1.0"
kind    = "request"
auth    = "trusted-peer"
params  = { config_path = "etc/matchmaker.json" }
```

Disabled entries are kept on disk so an operator can flip them back on without retyping the descriptor. When `supernode.toml` is missing the manifest is derived from legacy env vars for back-compat.

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

### Browser Participation (WebTransport / Game Relay)

When `web.host.h3.v1` is enabled on a supernode, browser-side game clients connect over WebTransport using `web-sdk/conquerd.mjs`. The same SDK is served from the portal at `/web-sdk/conquerd.mjs`. Games import it with a relative path:

```js
import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

const client = new ConquerdClient({
  host: resolvedHost,  // from window.conquerd.ready or query param
  port: 8443,
  features: ["game.relay.v1"],
  room: "my-lobby",
});

client.on("connected",    (peerId) => { /* ... */ });
client.on("datagram",     (featureId, data) => { /* handle inbound relay datagram */ });
client.on("disconnected", ()       => { /* ... */ });
client.on("error",        (e)      => { /* ... */ });

await client.connect();   // performs Ed25519 identity handshake
client.sendDatagram("game.relay.v1", myPayload);
```

When opened inside the native client portal (`conquerd://` scheme), `window.conquerd.ready` resolves with the WebTransport URL and cert fingerprint from the ConquerD trust chain — no HTTPS CA required, no `host`/`port` query params needed:

```js
const ctx = await window.conquerd.ready;
// ctx.wtBaseUrl and ctx.wtCertHash are pre-populated from SUPERNODE_INFO
// ConquerdClient.connect() picks them up automatically.
```

Three bundled game demos are deployed to `<data_dir>/games/` on first supernode start:

| Path | Description |
|------|-------------|
| `/games/example/` | Cursor relay — real-time shared cursor canvas |
| `/games/brick-breaker/` | Brick breaker — multiplayer paddle game |
| `/games/shared-drawing/` | Shared drawing — collaborative canvas with stroke broadcast |

All three use the same `game.relay.v1` opaque datagram relay and can be opened via `conquerd://<supernode_id>/games/<slug>/` from the in-app portal (supernode avatar in the Rooms sidebar) or directly at `https://<host>:<web_port>/games/<slug>/` from a browser (with `?host=<host>&port=<port>` query params when not in portal context).

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
| 0 | **QUIC direct** | Peer's QUIC port known (from relay hints or prior session) | QUIC (quinn) |
| 1 | **WebSocket assist** (1.25 s timeout) | At least one candidate endpoint reachable | WebSocket (TCP) |
| 2 | **WebSocket fallback** (4 s timeout) | Same candidates, longer timeout for slower paths | WebSocket (TCP) |
| 3 | **UDP hole punch** | Both sides online, non-symmetric NAT | Custom UDP protocol |
| 4 | **Supernode relay** | Both peers trusted by a common supernode | QUIC relay via supernode |

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

### UDP Hole Punching

When QUIC direct and WebSocket candidates both fail, Conquerd automatically attempts UDP hole punching. Each invite contains a `udp_hole_punch_hint` with the sender's STUN-discovered external UDP endpoint. Both peers simultaneously send probe packets to each other's external endpoint, creating NAT mappings that allow the other side's packets through.

```
Alice                              Internet                              Bob
NAT-A (104.54.197.38:57100)                         NAT-B (72.133.90.194:57240)

Alice sends probe ────────────────────────────────────────────→ NAT-B maps it
                  ←──────────────────────────────────────────── Bob sends probe
NAT-A maps it

Both receive ack → channel ESTABLISHED (bidirectional UDP)
```

**NAT compatibility:**

| NAT Type | Works? |
|---|---|
| Full-cone NAT (most home routers) | ✅ Yes |
| Address-restricted cone NAT | ✅ Yes |
| Port-restricted cone NAT | ✅ Yes |
| Symmetric NAT (CGNAT, corporate) | ❌ No — use supernode relay instead |

Symmetric NAT affects roughly 10–20% of users, primarily on mobile carrier NATs and some corporate networks. These users connect through a supernode relay automatically.

### Supernode-Coordinated Hole Punch

When uncoordinated hole punching fails due to timing mismatch, a trusted supernode can coordinate the attempt. Both peers send a `PUNCH_REGISTER` message to the supernode, which responds with `PUNCH_READY` containing each peer's endpoint and a synchronised `punch_at` timestamp so both sides begin probing simultaneously.

---

## Supernode Relay

A supernode is a volunteer peer that provides QUIC relay and SFU (group voice) hosting. Supernodes are **transport-only** — they never store identity, messages, room definitions, or chat history, and do not act as a central server. SFU rooms are held in memory while active and dropped after ~15 minutes idle; clients rematerialize saved rooms from `my_rooms.dat` on reconnect.

### Connecting to a Supernode
1. Get the supernode's invite link from the operator.
2. Paste it into Conquerd's **Join** dialog.
3. The handshake completes — the supernode appears in your peer list.
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

The invite link is also persisted in `~/.conquerd/supernode_invite.json` and survives restarts.

### Supernode Configuration

Runtime ports and access settings are read from environment variables; hosted feature capabilities are preferably declared in `<data_dir>/supernode.toml`.

#### Core Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `supernode_port` | `3478` | UDP port for QUIC relay traffic |
| `supernode_signaling_port` | `34935` | TCP port for WebSocket signaling. **Always set a fixed value** — changing it breaks firewall rules and stored peer endpoints |
| `supernode_invite_ttl` | `-1` | Invite expiry in minutes. `-1` = never expires |
| `supernode_host` | *(unset)* | Public DNS name or IP used in invite URLs, relay tickets, and WebTransport URLs for remote clients |
| `CONQUERD_HOME` | `~/.conquerd` | Data directory for identity, settings, files |

#### Feature Toggles (legacy)

Prefer `supernode.toml` for feature enablement (see [Enabling Features on a Supernode](#enabling-features-on-a-supernode)). The variables below are retained for backward compatibility when no manifest is present:

| Variable | Default | Description |
|----------|---------|-------------|
| `supernode_chat` | `1` | Enable chat relay (`0` to disable) |
| `supernode_files` | `1` | Enable file transfer relay (`0` to disable) |
| `supernode_sfu` | `1` | Enable SFU group voice rooms (`0` to disable) |
| `supernode_updates` | `1` | Enable P2P auto-update distribution (`0` to disable) |
| `supernode_auto_restart` | `1` | Auto-restart after applying an update (`0` to disable) |

#### Portal / Access Control Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `supernode_web_port` | *(unset)* | HTTPS port for portal/homepage. Set to enable (e.g. `8443`). Also enables the WebTransport listener and `game.relay.v1` on the same port |
| `supernode_web_localhost_only` | `0` | Bind the portal/WebTransport surface to localhost only |
| `supernode_web_title` | `Relay Node` | Human-readable name shown on the homepage |
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

# Portal + game relay (uncomment to enable)
#Environment=supernode_web_port=8443
#Environment=supernode_web_title=My Relay Node
#Environment=supernode_access_mode=open
# Required for remote clients to reach the WebTransport game relay:
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

The supernode exposes a `/health` page with live stats (uptime, version, connected/trusted peers, QUIC relay stats, SFU room details) and an `/api/stats` JSON endpoint (requires a valid session token). The dashboard auto-refreshes every 30 seconds. The `/arena` page redirects unauthenticated visitors to `/portal`.

---

## Updates

Conquerd checks the GitHub Releases API in the background and offers in-app upgrade prompts when a newer signed release is available. The bundled `conquerd-installer` binary downloads, verifies, and applies the release.

- Release artefacts (Windows: SignPath; macOS: Apple Developer ID) and accompanying Sigstore attestations are verified before any file is replaced.
- `VERSION_ANNOUNCE` is still exchanged between peers so each side can show the other peer's version in the event log, but application code is **not** pushed peer-to-peer — a connected peer running an older build is informational only.
- The *Check for updates* setting is persisted but background checks are not yet gated on that preference in 1.0.0; block `api.github.com` or run on a restricted network to prevent the startup check.

---

## Settings Reference

Access settings via the gear icon in Conquerd.

### Network

| Setting | Default | Description |
|---------|---------|-------------|
| Network mode | `public` | `public` = use STUN for IP discovery; `local` = LAN only |
| Public endpoint | (auto) | Manual override: `IP:port` |
| Signaling port | `0` (auto) | Set a fixed port if you need a firewall rule |
| UPnP enabled | `true` | Auto port-mapping on router |
| STUN servers | 3 built-in | Google (`stun.l.google.com`, `stun1.l.google.com`) + Cloudflare (`stun.cloudflare.com`) + custom servers with per-server enable/disable |

### Audio

| Setting | Default | Description |
|---------|---------|-------------|
| Input device | (system default) | Microphone selection |
| Output device | (system default) | Speaker/headphone selection |
| PTT key | `Space` | Push-to-talk key binding |
| Voice activation | `false` | Auto-transmit when sound detected |
| Noise suppression | `true` | Background noise reduction |
| Jitter buffer depth | `3` | Frames (1–20). Higher = more latency, smoother audio |

### Relay

| Setting | Default | Description |
|---------|---------|-------------|
| Allow gated supernodes | `true` | Allow connecting to supernodes that require portal access |

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

Received files are saved to your OS **Downloads** folder on completion (not under `CONQUERD_HOME`). The desktop client logs to **stderr** via `tracing` (`RUST_LOG`); there is no persistent client log file by default. An optional OS keyring entry (`conquerd` service) caches your unlock key locally.

Supernodes additionally store:

| File | Purpose |
|------|---------|
| `supernode_invite.json` | Persistent invite link |
| `supernode_endpoints.json` | Endpoint mailbox for peer reconnection (24h TTL) |
| `peers.json` | Trusted peer records for relay/SFU access control |

SFU **room state is not persisted** on the supernode — rooms exist in memory while in use and are idle-GC'd after ~15 minutes empty. Room definitions and chat history live on clients.

> **What to back up**: At minimum, back up `identity.dat` (and your passphrase). Losing it means peers will see you as a new, untrusted identity.

---

## Troubleshooting

### Can't connect to a peer
- Both peers need network reachability — check UPnP status in settings.
- Ensure both peers completed the invite handshake (trusted peer appears in left panel).
- Try a supernode as relay if direct connections fail.
- For UDP hole punching, both sides need to exchange invites at roughly the same time (within 30 seconds).

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
- **Peers get relay access without the portal**: Ensure `supernode_access_mode` is set to `tos`, `ad`, or `code` (not `open`) and `supernode_web_port` is set.

---

## Developer Guide

### Entry Points

| File | Purpose |
|---|---|
| `rust/conquerd-client/src/main.rs` | Desktop client entry. Initialises identity (keyring + passphrase), Qt `QGuiApplication`, the `AppBridge` QObject, and the QML engine. Handles `conquerd://` URIs on argv and single-instance forwarding. |
| `rust/conquerd-supernode/src/main.rs` | Headless relay binary. Reads env / `supernode.toml`, starts QUIC relay + WebSocket signaling + optional HTTPS portal + HTTP/3/WebTransport listener. |
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

`conquerd-client` lives in its own Cargo workspace (`rust/conquerd-client/Cargo.toml`) so the Windows-local `cxx-qt` patch stays isolated and does not affect server-side builds on Linux. The outer workspace (`rust/Cargo.toml`) contains `conquerd-features`, `conquerd-supernode`, and `conquerd-installer`.

### Run Tests
```powershell
# Outer workspace (features + supernode + installer)
cd rust
cargo test --workspace

# Conquerd-client (binary crate; tests run from its own workspace)
cd conquerd-client
cargo test
```

See `agents.md` (Roadmap & Status) for the current authoritative test counts, coverage areas, and P0–P2 delivery status. The test suite emphasises capability negotiation, quota symmetry (inbound/outbound), replay protection, relay/SFU/room flows, and installer manifest verification.

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
│   ├── Cargo.toml                 # Outer workspace: features + supernode + installer
│   ├── conquerd-client/           # Native desktop binary (own workspace; Qt 6 / QML via CXX-Qt; 139 unit tests)
│   │   ├── Cargo.toml             # features: qt-ui, webengine, console
│   │   ├── build.rs               # CXX-Qt codegen + windres icon embedding
│   │   ├── assets.qrc / icons.qrc # Qt resource bundles (QML + icons)
│   │   ├── qml/                   # MainWindow, ChatPanel, CallPanel, RoomPanel, SettingsPage, …
│   │   └── src/
│   │       ├── main.rs            # Entry point: identity init, QGuiApplication, QML engine
│   │       ├── identity.rs        # Ed25519 keypair, keyring AES key, passphrase handling
│   │       ├── connection_manager/ # Invite handshake, signaling, peer tracking
│   │       ├── connection_fallback.rs # QUIC → WS → hole-punch → relay strategy ladder
│   │       ├── call_controller.rs # Call state machine; audio + QUIC peer wiring
│   │       ├── chat_store.rs      # SQLite chat history (per-peer trim_by_age / count / purge)
│   │       ├── file_transfer.rs   # P2P file send/receive with chunking + progress
│   │       ├── sfu_client.rs      # SFU membership (join/leave/member list)
│   │       ├── room_manager.rs    # Per-participant room state
│   │       ├── room_store.rs      # Encrypted client-owned room definitions (my_rooms.dat)
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
│   ├── conquerd-features/         # rlib: capability registry, FeatureModule trait, quota enforcement (114 unit tests)
│   ├── conquerd-supernode/        # Standalone binary: QUIC relay, ephemeral SFU, WS signaling, WebTransport + QUIC-stream portal (212 unit tests)
│   └── conquerd-installer/        # Standalone binary: signed-release download + apply (74 unit tests)
├── web-sdk/conquerd.mjs           # Browser SDK (WebTransport client matching the native channel fabric)
├── games/                         # Example browser games served over `web.host.h3.v1`
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
| SFU / Room | `sfu_join`, `sfu_leave`, `sfu_members`, `sfu_offer`, `sfu_answer`, `sfu_audio`, `sfu_chat`, `sfu_room_list`, `sfu_peer_joined`, `sfu_peer_left` |
| SFU Subscription | `sfu_subscribe`, `sfu_unsubscribe` |
| SFU Room Mgmt | `sfu_room_create`, `sfu_room_created`, `sfu_room_invite`, `sfu_room_invite_result`, `sfu_room_invite_generate` |
| SFU File Transfer | `sfu_file_offer`, `sfu_file_chunk`, `sfu_file_complete` |
| File Transfer | `file_transfer_offer`, `file_transfer_accept`, `file_transfer_reject`, `file_transfer_chunk`, `file_transfer_complete`, `file_transfer_ack`, `file_transfer_error` |
| Trust | `trust_request`, `trust_accept` |
| Peer Room Invite | `peer_room_invite` |
| Hole Punch | `punch_register`, `punch_ready` |
| Endpoint | `endpoint_update` |
| Handle | `handle_update` |
| Encrypted | `encrypted_signal` |
| Peer Updates | `version_announce` |
| Capability | `capability_announce`, `capability_invoke` |
| Utility | `ping`, `pong`, `error`, `speaking_state`, `presence_update` |

## Technology Stack

| Layer | Library / Runtime |
|---|---|
| UI | Qt 6 / QML via [CXX-Qt](https://kdab.github.io/cxx-qt/) |
| QUIC transport | `quinn` + `tokio` + `rustls` |
| Audio capture / playback | `cpal` + `ringbuf` |
| Codec | `conquerd-opus` — first-party libopus 1.6.1 wrapper with DRED (Deep Redundancy Encoding) and OSCE (Opus Speech Coding Enhancement) neural voice enhancement. DNN model data compiled in from Xiph.Org source arrays; no third-party crate dependency. |
| DSP | `rustfft` (spectral-gate noise suppression), in-house VAD + jitter buffer |
| Cryptography | `ed25519-dalek`, `x25519-dalek`, `aes-gcm`, `argon2`, `hkdf` |
| Signaling serialisation | JSON over WebSocket (`tokio-tungstenite`) and QUIC bidirectional streams |
| Local storage | SQLite (`rusqlite`) for chat history; JSON for settings/peers/rooms |
| SDP/WebTransport (browser) | WebTransport + HTTP/3 (`h3`, `quinn`) on the supernode |
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

## Release Notes

Detailed, per-version release notes are published with each [GitHub release](https://github.com/ConquerD/ConquerD/releases). The summary below covers the **1.0** milestone.

### 1.0 — Highlights

- **Zero-trust P2P architecture** — direct peer-to-peer; no central server stores your data. Ed25519 identity with derived peer IDs, invite-only discovery via signed `conquerd://` links, forward-secret handshakes (ephemeral X25519 + HKDF + AES-GCM).
- **Chat-first UX** — text is the primary interaction after connecting; voice is opt-in per conversation. Per-conversation scroll persistence, typing indicators, unread badges on taskbar + tray.
- **Voice calls** — low-latency Opus over QUIC, push-to-talk and voice activation, spectral-gate noise suppression, jitter buffer with de-click.
- **Rooms (multi-peer voice)** — client-owned room definitions (`my_rooms.dat`); supernodes host SFU sessions ephemerally over QUIC relay with chat/voice/file parity, idle GC, and reconnect materialization.
- **Game relay & in-app portal**: `game.relay.v1` opaque datagram relay over WebTransport; three bundled browser game demos (cursor relay, brick breaker, shared drawing) served from `<data_dir>/games/` and accessible from the in-app portal at `conquerd://<supernode_id>/games/<slug>/`. Self-signed TLS cert with `serverAuth` EKU auto-generated and rotated every 7 days; fingerprint delivered via `SUPERNODE_INFO` trust chain.
- **Supernode release binaries**: pre-built packages for Linux x86_64, Linux ARM64, and Windows x86_64 on GitHub Releases and nightlies (`scripts/build_supernode.sh` / `scripts/build_supernode.ps1`).
- **NAT traversal** — UPnP port mapping, QUIC/WebSocket direct connect, supernode relay fallback, relay-coordinated hole punching.
- **Security** — signed, transcript-bound signaling with timestamp freshness checks and per-sender replay deduplication; peer revocation with propagation; release-signed P2P updates with Ed25519 + threshold validation; crash/installer logging.
- **Desktop application** — DPI-aware dark theme, first-run onboarding wizard (display name, identity fingerprint + QR, supernode setup), `conquerd://` URI scheme for one-click invites, system tray with badges, collapsible event log, save-to-PNG invite QR codes.

### Known limitations

- Desktop only (no mobile clients) for 1.0.
- No video calls yet.
- Supernode discovery is manual (invite-link based).

## License
MIT
