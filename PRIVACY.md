# ConquerD Privacy Policy

**Effective date:** 2026-06-08

ConquerD is a local-first, invite-only peer-to-peer application. Voice, chat,
and file transfer travel directly between clients you connect to, or through
volunteer supernodes you explicitly choose to trust. Application payloads are
encrypted on the wire; supernodes relay signed/encrypted frames and cannot read
message or audio content. ConquerD does not operate any central servers that
store your identity, messages, or call data.

---

## Information stored on your device

All persistent ConquerD data is written under `CONQUERD_HOME` (default
`~/.conquerd/` on Linux/macOS, `%USERPROFILE%\.conquerd\` on Windows). This data
does not leave your device unless you explicitly connect to a peer or
supernode and exchange it as part of normal operation, or you initiate an
optional feature described below (updates, link previews, Ollama, and so on).

| File / location | What it contains |
|---|---|
| `identity.dat` | Your Ed25519 identity (v2, AES-256-GCM encrypted at rest) |
| `identity.json` | Legacy v1 plaintext identity (read-only after migration, if present) |
| `peers.dat` | Trusted-peer records (encrypted): public keys, handles, relay hints, block state, optional avatar config, build-attestation metadata |
| `chat_history.db` | Local chat history; message bodies and sender handles are AES-256-GCM encrypted at rest |
| `settings.json` | App preferences (audio, plugins, privacy toggles, window size, etc.) |
| `my_rooms.dat` | Saved room invites (encrypted) |
| OS keyring (`conquerd` service) | Optional cached AES unlock key so you are not prompted for your passphrase every launch |
| OS **Downloads** folder | Files received from peers (saved by the desktop client on completion) |
| `installer.log` | Installer/updater activity log (written when `conquerd-installer` runs) |

The desktop client logs to **stderr** via Rust `tracing` (controlled by the
`RUST_LOG` environment variable). It does not write a persistent client log file
by default.

No telemetry, analytics, or usage reporting is collected by ConquerD.

---

## Information transmitted automatically

The following network contacts can occur without an extra confirmation step
beyond normal app use. No account credentials, message content, or contact lists
are sent in these paths.

### Update check (GitHub Releases API)

**What:** On each client launch, the desktop client polls the GitHub Releases
API once to see whether a newer version is available. (A one-hour polling
interval is defined in code but not yet wired to repeat checks while the app
stays open.)

**Endpoint:** `https://api.github.com/repos/vbawol/ConquerD/releases/latest`

**What is disclosed:** Your IP address is visible to GitHub (Microsoft Corp.) as
part of the HTTPS request. The client sends `User-Agent:
conquerd-client/{version}` (for example `conquerd-client/1.0.0`) and accepts
`application/vnd.github+json`. GitHub may log this alongside your IP. No
personal information beyond what any HTTPS request carries is sent.

**Settings note:** A *Check for updates automatically* toggle is shown in
Settings and persisted as `update_check_enabled` in `settings.json`, but
background checks are not yet gated on that preference in 1.0.0 — a GitHub
API request still occurs on each launch.

**How to limit:** Block outbound HTTPS to `api.github.com`, or do not run the
client on networks where that contact is unacceptable. When you choose to apply
an update, `conquerd-installer` additionally downloads release archives,
checksums, and (when published) `releases_manifest.json` from GitHub.

### UPnP port mapping

**What:** When *Enable UPnP port mapping* is on (the default), ConquerD sends
SSDP discovery multicast on your **local area network** to locate a UPnP-capable
router and requests a temporary port-forwarding rule. This can improve direct
peer-to-peer reachability without a relay.

**Servers contacted:** No external Internet servers are contacted. UPnP traffic
stays on your LAN (multicast to `239.255.255.250`). Only your router responds.

**What is disclosed:** Your internal IP address and the port ConquerD is
listening on are sent to your local router. Nothing leaves your network.

**How to disable:** Uncheck *Enable UPnP port mapping* in Settings, or set
`upnp_enabled` to `false` in `settings.json`. ConquerD falls back to direct
QUIC/WebSocket candidates and supernode relay when needed.

---

## Information transmitted when you opt in or take an action

### Inline link / video previews in chat

**What:** When *Show YouTube preview cards in chat* is enabled (default: on),
messages containing YouTube, Vimeo, or direct video URLs show a local preview
card in the chat UI. Expanding the inline player (after the first-time
acknowledgement, if shown) loads the embed in an embedded Chromium view
(Qt WebEngine).

**Servers contacted (only after you expand inline playback or open the link):**
YouTube (`youtube.com`, `googlevideo.com`, `ytimg.com`, …), Vimeo
(`vimeo.com`, `player.vimeo.com`, …), or the direct video host. ConquerD does
**not** use `yt-dlp` for this feature.

**What is disclosed:** The video host sees a normal browser/embed request from
your IP. No chat message text is sent to ConquerD-operated servers (there are
none).

**How to disable:** Uncheck *Show YouTube preview cards in chat* in Settings
(`youtube_preview_enabled` in `settings.json`). Opening links with *Open* still
launches your system browser.

### Ollama AI assistant (optional plugin)

**What:** When *Enable AI assistant* is on, ConquerD sends HTTP requests to the
Ollama base URL you configure (default `http://localhost:11434`) to list models
and stream completions. Chat text you route to the assistant is included in
those local requests.

**Servers contacted:** Only the Ollama instance you configure — by default your
own machine. No ConquerD cloud service is involved.

**Peer visibility:** The `x.ollama.v1` capability may be advertised to
connected peers as a presence signal; message content is not sent to peers
through the plugin.

**How to disable:** Turn off *Enable AI assistant* in Settings
(`ollama_enabled` in `settings.json`).

### Supernode portal and gated relay pages

**What:** When you connect to a supernode that exposes a portal or access gate,
ConquerD may load operator-hosted pages inside the embedded browser (typically
`conquerd://` over the QUIC portal, or HTTPS where configured). External links
from those pages open in your system browser.

**Servers contacted:** The supernode operator you chose — not ConquerD.

**What is disclosed:** The operator can see that you visited their portal and
your IP address for any HTTPS content they host. Portal traffic over
`conquerd://` is carried on your authenticated QUIC session to that supernode.

### Build attestation between peers

**What:** After connecting, peers may exchange signed build-attestation
metadata (version string, reproducible build id, source hash) so each side can
show whether the other appears to be running an official release. This is
governed by the *Attestation policy* setting (`off` / `warn` / `strict`).

**What is disclosed:** Build metadata only — not message or audio content.

---

## Peer-to-peer communication

When you connect to a peer, the following data is transmitted over encrypted
signaling and session channels:

- Your display name (chosen during onboarding)
- Your long-term Ed25519 public key (your identity)
- Messages, voice audio, and files you explicitly send
- Optional `AvatarConfig` after handshake (trusted peers only)
- Negotiated capability descriptors (`CAPABILITY_ANNOUNCE`)

When you use a volunteer **supernode** for relay or group voice:

- The supernode forwards encrypted/signed payloads between peers but **cannot
  decrypt** application content — it has no access to your identity key material
  or session keys.
- The operator can observe connection/relay activity and approximate traffic
  volumes, but not message or audio content.

Supernodes are configured by you and run by peers or operators you choose to
trust. ConquerD does not operate any supernodes.

---

## Third-party components

ConquerD bundles the following open-source components. They do not phone home
on their own; external contact happens only through the behaviors described
above.

| Component | Purpose | External contacts |
|---|---|---|
| [Qt 6 / CXX-Qt](https://www.qt.io/privacy-policy) | Desktop UI | None from Qt itself |
| [Qt WebEngine](https://www.qt.io/privacy-policy) | Inline previews, supernode portal | Only when you load external or embed URLs (see above) |
| [quinn](https://github.com/quinn-rs/quinn) | QUIC transport | None |
| [egui / eframe](https://github.com/emilk/egui) | Installer UI | None |
| [Ollama](https://ollama.com/) (user-installed, optional) | Local AI backend | Only the URL you configure |

---

## Children's privacy

ConquerD is not directed at children under 13. It does not knowingly collect
personal information from children.

---

## Changes to this policy

Material changes will be noted in the
[GitHub releases](https://github.com/ConquerD/ConquerD/releases) (and the README
"Release Notes" section) and this file updated with a new effective date.

---

## Contact

For privacy concerns, open an issue in the project repository or contact the
maintainers via the repository's contact information.