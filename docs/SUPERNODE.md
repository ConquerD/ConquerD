# ConquerD Supernode Operator Guide

This guide covers running a production or volunteer `conquerd-supernode`.

The supernode provides optional transport assistance (QUIC relay, SFU rooms, WebTransport) and hosts opt-in feature modules. It is **never** an identity or trust authority — all trust comes from the invite + handshake between peers.

## Quick Start

```bash
# Build
cargo build -p conquerd-supernode --release

# Run with defaults (data in ./data)
./target/release/conquerd-supernode
```

### Pre-built Linux ARM64 binary

Official and nightly releases include a standalone `conquerd-supernode-*-linux-aarch64.tar.gz` asset (GitHub Releases). This is the easiest path for ARM64 VPS hosts, Raspberry Pi, and other `aarch64` Linux servers.

```bash
# Example: install from a tagged release asset
tar -xzf conquerd-supernode-1.0.0-linux-aarch64.tar.gz
sudo install -m 755 conquerd-supernode-1.0.0-linux-aarch64/conquerd-supernode /usr/local/bin/

# Or build/package locally on any supported host:
CONQUERD_RELEASE=1 ./scripts/build_supernode.sh
```

Nightly builds publish `conquerd-supernode-nightly-linux-aarch64.tar.gz` on the rolling `nightly` release.

Configuration is read from `<data_dir>/supernode.toml` (see below). Legacy environment variables are supported for backward compatibility but are deprecated.

## Configuration (supernode.toml)

Create `<data_dir>/supernode.toml` (default data dir is `./data` or `$CONQUERD_DATA_DIR`).

Example:

```toml
schema_version = 1

# Basic network
listen_addr = "0.0.0.0:3478"          # QUIC relay + feature ports
ws_listen_addr = "0.0.0.0:3479"       # WebSocket signaling (optional)
web_port = 8443                       # For web.host.h3.v1 (WebTransport)

# Identity (generated on first run if missing)
identity_file = "identity.json"

# Access control (see access.rs for details)
access_mode = "open"                  # open | tos | access_code | timer | custom

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

See `rust/conquerd-supernode/src/manifest.rs` for the full schema.

Run `conquerd-supernode --print-default-manifest` for a starting point.

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

### Web Hosting
- `web.host.app.v1`: In-app portal for the desktop client's embedded browser (`conquerd://` scheme).
- `web.host.h3.v1`: WebTransport bridge so browser clients can participate in the same channel fabric as native peers.

Static assets live in `<data_dir>/web/` and `<data_dir>/games/<slug>/`.

### Plugin / Bespoke Modules (`x.<vendor>.*`)
- Native cdylib plugins are loaded at startup from paths declared in the manifest.
- Each plugin must provide a manifest + signed binary (see loader in main.rs).
- First-party namespaces (`core.*`, `room.*`, `web.*`, `game.*`) bypass user consent prompts. `x.*` namespaces require explicit consent.

## Access Control

Supported modes (set via manifest or legacy env vars):

- `open`
- `tos` (terms of service acceptance)
- `access_code`
- `timer` / ad-gate (time or ad-based)
- `custom` (implement your own `AccessController`)

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

See the example `supernode.toml` in the repo root or generate one with:

```bash
conquerd-supernode --example-config > /etc/conquerd/supernode.toml
```

Typical systemd unit (example):

```ini
[Unit]
Description=ConquerD Supernode
After=network.target

[Service]
ExecStart=/usr/local/bin/conquerd-supernode --data-dir /var/lib/conquerd
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

*Maintained as part of the ConquerD project. Last major update aligned with P1 #7 (2026).*
