# ConquerD Privacy Policy

**Effective date:** 2026-05-23

ConquerD is a local-first, invite-only peer-to-peer application. All
peer-to-peer communication (voice calls, chat messages, file transfers)
travels directly between clients or through volunteer supernodes that you
explicitly choose to trust. End-to-end encryption is applied to all
communication. ConquerD does not operate any servers that store your identity,
messages, or call data.

---

## Information stored on your device

All persistent data is written only to your local `CONQUERD_HOME` directory
(default `~/.conquerd/`). This data never leaves your device unless you
explicitly connect to a peer and exchange it as part of normal app operation.

| File | What it contains |
|---|---|
| `identity.json` | Your Ed25519 keypair and display name |
| `peer_store.json` | Cryptographic public keys and display names of peers you have trusted |
| `chat_history.db` | Local copy of messages you have sent and received |
| `settings.ini` | Your app preferences |
| `my_rooms.json` | Invites for group voice rooms you have joined or created |
| `received_files/` | Files sent to you by peers |
| `crash_*.log`, `installer.log` | Crash diagnostics and installer activity written locally |

No telemetry, analytics, or usage data is collected.

---

## Information transmitted automatically

The following network contacts occur automatically without additional user
action. In each case, no account credentials, message content, contact lists,
or persistent identifiers are transmitted.

### STUN public IP discovery

**What:** When *Network mode* is set to `public` (the default), ConquerD
sends a standard STUN Binding Request (RFC 5389) to discover your external
IP address and UDP port. This is required to establish peer-to-peer
connections through NAT.

**Servers contacted:**
- `stun.l.google.com:19302` (Google LLC)
- `stun1.l.google.com:19302` (Google LLC)
- `stun.cloudflare.com:3478` (Cloudflare, Inc.)

Additional STUN servers may be configured in Settings.

**What is disclosed:** Your IP address is observed by the STUN server as
part of the standard UDP request/response. No other data is sent.

**How to disable:** Set *Network mode* to `local` in Settings. In local
mode, ConquerD uses only your LAN IP and does not contact any STUN server.

### Update check (GitHub Releases API)

**What:** If *Check for updates* is enabled (default: on), ConquerD polls
the GitHub Releases API once per hour to check whether a newer version is
available.

**Endpoint:** `https://api.github.com/repos/vbawol/ConquerD/releases/latest`

**What is disclosed:** Your IP address is visible to GitHub (Microsoft Corp.)
as part of the HTTPS request. The GitHub API requires all clients to send a
`User-Agent` header; ConquerD sends `ConquerD/{version}` (e.g.
`ConquerD/1.2.0`). GitHub may log this alongside your IP. No personal
information beyond what any HTTPS request carries is sent.

**How to disable:** Uncheck *Check for updates* in Settings, or set
`update/check_enabled = false` in `~/.conquerd/settings.ini`.

### UPnP port mapping

**What:** When *UPnP enabled* is on (the default), ConquerD sends an SSDP
discovery multicast on your **local area network** to locate a UPnP-capable
router (e.g. your home router) and then requests a temporary port-forwarding
rule. This improves the likelihood of establishing a direct peer-to-peer
connection without a relay server.

**Servers contacted:** No external Internet servers are contacted. All
UPnP traffic stays on your local network (broadcast to `239.255.255.250`).
The only device that responds is your own router.

**What is disclosed:** Your internal IP address and the port number ConquerD
is listening on are sent to your local router. Nothing leaves your network.

**How to disable:** Uncheck *UPnP enabled* in Settings, or set
`network/upnp_enabled = false` in `~/.conquerd/settings.ini`. ConquerD falls
back to the other NAT traversal methods (STUN + hole punch + supernode relay).

---

## Peer-to-peer communication

When you connect to a peer, the following data is transmitted over an
end-to-end encrypted channel:

- Your display name (chosen during onboarding)
- Your long-term Ed25519 public key (your "identity")
- Messages, voice audio, and files you explicitly send

When you use a volunteer **supernode** for relay or group voice:

- The supernode forwards encrypted payloads between peers but **cannot
  decrypt** them — it has no access to your messages, audio content, or
  identity key material.
- The supernode operator can observe that a connection is being relayed and
  approximate traffic volumes, but not the content.

Supernodes are configured by you and run by peers or operators you choose to
trust. ConquerD does not operate any supernodes.

---

## Third-party components

ConquerD bundles the following open-source components that may contact
external services when you use them:

| Component | Purpose | Privacy policy |
|---|---|---|
| [Qt 6 / CXX-Qt](https://www.qt.io/privacy-policy) | UI framework | No external contacts from Qt itself |
| [quinn (QUIC)](https://github.com/quinn-rs/quinn) | QUIC transport | No external contacts |
| [egui / eframe](https://github.com/emilk/egui) | Installer UI | No external contacts |
| [egui / eframe](https://github.com/emilk/egui) | Installer UI | No external contacts |

---

## Children's privacy

ConquerD is not directed at children under 13. It does not knowingly collect
personal information from children.

---

## Changes to this policy

Material changes will be noted in the [GitHub releases](https://github.com/ConquerD/ConquerD/releases) (and the README "Release Notes" section) and
this file updated with a new effective date.

---

## Contact

For privacy concerns, open an issue in the project repository or contact
the maintainers via the repository's contact information.
