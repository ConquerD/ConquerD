# ConquerD Supernode Operator Guide

This guide covers running a production or volunteer `conquerd-supernode`.

The supernode provides optional transport assistance (QUIC relay, SFU rooms, WebTransport) and hosts opt-in feature modules. It is **never** an identity or trust authority — all trust comes from the invite + handshake between peers.

## Quick Start

```bash
# Build from the outer Rust workspace
cd rust
cargo build -p conquerd-supernode --release

# Run with defaults (data in $HOME/.conquerd, or %USERPROFILE%\.conquerd on Windows)
./target/release/conquerd-supernode
```

### Pre-built binaries (GitHub Releases)

Official tagged releases and the rolling `nightly` prerelease ship standalone supernode packages (each with a `.sha256` sidecar). These are the easiest path for VPS and bare-metal hosts — no Rust toolchain required on the server.

| Platform | Tagged release asset | Nightly asset |
|---|---|---|
| Linux x86_64 | `conquerd-supernode-<version>-linux-x86_64.tar.gz` | `conquerd-supernode-nightly-linux-x86_64.tar.gz` |
| Linux ARM64 (`aarch64`) | `conquerd-supernode-<version>-linux-aarch64.tar.gz` | `conquerd-supernode-nightly-linux-aarch64.tar.gz` |
| Windows x86_64 | `conquerd-supernode-<version>-win64.zip` | `conquerd-supernode-nightly-win64.zip` |

**Linux x86_64** (typical VPS / cloud VM):

```bash
tar -xzf conquerd-supernode-1.0.0-linux-x86_64.tar.gz
sudo install -m 755 conquerd-supernode-1.0.0-linux-x86_64/conquerd-supernode /usr/local/bin/
```

**Linux ARM64** (Raspberry Pi, ARM VPS):

```bash
tar -xzf conquerd-supernode-1.0.0-linux-aarch64.tar.gz
sudo install -m 755 conquerd-supernode-1.0.0-linux-aarch64/conquerd-supernode /usr/local/bin/
```

**Windows x86_64**:

```powershell
Expand-Archive conquerd-supernode-1.0.0-win64.zip -DestinationPath .
# Run: .\conquerd-supernode-1.0.0-win64\conquerd-supernode.exe
```

### Build and package locally

On Linux or macOS, `scripts/build_supernode.sh` detects the host platform and emits a `.tar.gz` under `dist/`:

```bash
CONQUERD_RELEASE=1 ./scripts/build_supernode.sh
# e.g. dist/conquerd-supernode-1.0.0-linux-x86_64.tar.gz
```

On Windows, use the companion script (`.zip` output):

```powershell
$env:CONQUERD_RELEASE = '1'
.\scripts\build_supernode.ps1
# e.g. dist\conquerd-supernode-1.0.0-win64.zip
```

Supported local package suffixes: `linux-x86_64`, `linux-aarch64`, `macos-arm64`, `macos-x86_64` (shell script), and `win64` (PowerShell script).

CI validates packaging on all three release targets (`test-supernode-linux-x86_64`, `test-linux-arm64`, `test-supernode-windows` in `.github/workflows/ci.yml`).

Hosted feature declarations are read from `<data_dir>/supernode.toml` (see below). If the file is missing, a full first-party default manifest is used. Built-in first-party descriptors are always present in the registry for quota and relay accounting.

## Configuration (supernode.toml)

Create `<data_dir>/supernode.toml`. The default data dir is `$CONQUERD_HOME` when set, otherwise `$HOME/.conquerd` on Linux/macOS or `%USERPROFILE%\.conquerd` on Windows.

Example:

```toml
schema_version = 1

# Basic network
listen_addr = "0.0.0.0:3478"          # QUIC relay + feature ports
ws_listen_addr = "0.0.0.0:3479"       # WebSocket signaling (optional)
web_port = 8443                       # For web.host.h3.v1 (WebTransport)

# Feature manifest (recommended)
[[feature]]
id = "core.chat.v1"
enabled = true

[[feature]]
id = "room.audio.sfu"
enabled = true

[[feature]]
id = "room.file.v1"
enabled = true

[[feature]]
id = "web.host.h3.v1"
enabled = true
params = { port = 8443 }

[[feature]]
id = "web.host.app.v1"
enabled = true

# Bespoke / third-party modules (x.*)
[[feature]]
id = "x.acme.matchmaker"
enabled = true
cdylib_manifest = "plugins/acme-matchmaker.toml"
```

See `rust/conquerd-supernode/src/manifest.rs` for the full schema. The example above is the current starting point; the binary does not expose a manifest-printing CLI flag.

## Key Features & Hosting

### QUIC Relay
- Native peers connect via `QuicRelayClient` using Ed25519 mTLS + signed tickets.
- Tickets are issued with 1h TTL, 10min renewal window.
- Endpoint mailbox (`supernode_endpoints.json`) helps clients survive restarts (24h TTL).

### SFU Rooms (`room.audio.sfu` + `room.chat.v1` + `room.file.v1`)
- Up to 32 participants per room.
- Native + WebTransport (browser) clients supported.
- Room file broadcasts use signed `SfuFile*` frames and are verified by recipients before saving.
- Room membership is enforced at the capability layer (`room-member` auth tier).
- Operators can restrict which room types peers may **create** via `room.audio.sfu` manifest params:
  - `allow_public_rooms` (default `false`) — when `false`, new public room materialization is rejected. The built-in **Public Voice/Chat Room** (`room_id = "default"`) is always present on SFU-enabled nodes.
  - `allow_private_rooms` (default `true`) — when `false`, new private room materialization is rejected.
  - Existing rooms may still be replayed/materialized by id; policy applies only to **new** room creation.
  - Denied creates return `sfu_room_created` with `denied: true` and a `reason` (`public_rooms_disabled` / `private_rooms_disabled`).

Example:

```toml
[[feature]]
id = "room.audio.sfu"
enabled = true
params = { allow_public_rooms = false, allow_private_rooms = true }
```

### Web Hosting
- `web.host.app.v1` and `web.host.h3.v1` ship **enabled by default** (legacy-derived manifests and supernode-manager installs). Set `web_port` in `supernode.toml` (or `supernode_web_port` env) to bind WebTransport.
- `web.host.app.v1`: In-app portal for the desktop client's embedded browser (`conquerd://` scheme).
- `web.host.h3.v1`: WebTransport bridge so browser clients can participate in the same channel fabric as native peers.

Static assets live in `<data_dir>/web/` and `<data_dir>/games/<slug>/`.

### Plugin / Bespoke Modules (`x.<vendor>.*`)
- Native cdylib plugins are loaded at startup from paths declared in the manifest.
- Each plugin must provide a manifest + signed binary (see loader in main.rs).
- First-party namespaces (`core.*`, `room.*`, `web.*`, `game.*`) bypass user consent prompts. `x.*` namespaces require explicit consent.

## Access Control

Supported modes are currently selected with the `supernode_access_mode` environment variable:

- `open`
- `tos` (terms of service acceptance)
- `code`
- `ad` (timer / ad-gate)

See `src/access.rs` for the trait and examples.

## Tickets & Endpoint Mailbox

- Relay tickets are Ed25519-signed by the supernode.
- Clients should renew tickets when `needs_renewal()` returns true.
- The endpoint mailbox allows clients to discover the current relay address after a supernode restart without re-onboarding.

## Hot Reload & Operations

- The supernode does **not** currently support hot-reload of the binary.
- For plugin updates, restart is required.
- Use a process supervisor (systemd, docker, etc.) for production.

Graceful shutdown is supported (closes QUIC endpoints cleanly).

## Security Notes

- The supernode only sees encrypted traffic and metadata it needs for routing (peer indices, room membership).
- All feature dispatch goes through `conquerd-features` (auth tier + quota enforcement).
- Never trust the supernode for identity — only for transport assistance and opt-in hosting.

## Monitoring & Stats

The supernode exposes basic stats via:
- `web.host.app.v1` portal endpoints (`/health`, `/api/stats`, `/api/metrics`, `/api/peers`, `/api/config`)
- Internal `stats.rs` (exposed to plugins and the operator console)

`/api/metrics` (P3) returns the same payload as `/api/stats` plus a `metrics_version` and `generated_uptime_seconds` field for easier scraping / dashboards.

## Example Deployment

Start from the `supernode.toml` example in this guide and place it under the data directory before launching the service.

Typical systemd unit (example):

```ini
[Unit]
Description=ConquerD Supernode
After=network.target

[Service]
Environment=CONQUERD_HOME=/var/lib/conquerd
ExecStart=/usr/local/bin/conquerd-supernode
User=conquerd
Restart=on-failure
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

## Troubleshooting

- **Clients can't connect**: Check firewall for the QUIC port, certificate CN matching, and that the peer is allowed.
- **Room audio not working**: Verify `room.audio.sfu` is enabled in the manifest and the client negotiated the capability.
- **High memory**: Look at datagram receive buffers and concurrent streams in the transport config.

For more details, see the source:
- `src/main.rs` (startup & wiring)
- `src/manifest.rs` (typed config)
- `src/relay.rs`, `src/sfu.rs`, `src/webtransport.rs`

## Contributing

Improvements to the supernode (especially better observability, hot-reload for plugins, or more access control modes) are welcome. Please keep the "no first-party backend for identity" principle.

---

*Maintained as part of the ConquerD project. Last updated for multi-platform release binaries (linux-x86_64, linux-aarch64, win64).*
