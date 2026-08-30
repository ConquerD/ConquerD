# supernode-manager Design

`supernode-manager` is a standalone Rust CLI and TUI for deploying and operating
fleets of `conquerd-supernode` processes over SSH. It lives outside the ConquerD
application workspace and treats the supernode as an external binary with a
stable host contract: binary, data directory, manifest, ports, and systemd unit.

Status: working v0.1 prototype. Linux remote hosts with systemd are supported
end to end, including multiple instances on one host. The default user
experience is the TUI; every operation is also exposed as a CLI subcommand.

## Design Goals

- Manage many supernodes from one operator machine.
- Support many hosts and multiple isolated instances per host.
- Require no preinstalled agent on remote hosts.
- Keep the manager outside ConquerD identity and trust flows.
- Prefer declarative state in `inventory.toml`, with direct CLI/TUI controls.
- Preserve persistent node state across installs, restarts, and config pushes.
- Use GitHub nightly release artifacts by default, with sha256 verification.

Non-goals:

- Not a ConquerD backend, identity authority, or discovery service.
- Not a client identity manager.
- Not currently a cross-platform remote supervisor. Remote install targets are
  Linux plus systemd.

## Workspace Layout

```text
ConquerD_Manager/
  Cargo.toml
  DESIGN.md
  agents.md
  inventory.toml
  launch.ps1
  crates/
    snm-cli/
    snm-core/
    snm-transport/
    snm-supernode/
```

Crate responsibilities:

| Crate | Responsibility |
| --- | --- |
| `snm-cli` | Clap CLI, ratatui TUI, command dispatch, worker loop. |
| `snm-core` | Inventory model, selectors, port defaults, supernode config resolution. |
| `snm-transport` | SSH abstraction, embedded russh backend, OpenSSH backend. |
| `snm-supernode` | Supernode-specific ops: install, systemd, manifest render, release download, firewall, invite, status probing. |

The manager does not import ConquerD crates. It encodes only the supernode host
contract and release artifact naming.

## Runtime Model

The operator runs `supernode-manager` locally. The manager reads
`inventory.toml`, opens SSH connections to selected hosts, and reconciles a
small set of files and services:

```text
operator machine
  inventory.toml
  temp release cache
  SSH credentials
        |
        | SSH
        v
remote Linux host
  /opt/conquerd/bin/conquerd-supernode-nightly
  /opt/conquerd/bin/current -> versioned binary
  /var/lib/conquerd/a/supernode.toml
  /var/lib/conquerd/a/identity.json
  /etc/systemd/system/conquerd-supernode@.service
  /etc/systemd/system/conquerd-supernode@a.service.d/override.conf
```

Each supernode instance is identified by `(host.name, instance.id)`.

## Inventory Model

`inventory.toml` is the source of desired fleet configuration.

Current default shape:

```toml
[defaults]
version = "nightly"
access_mode = "open"
user = "conquerd"
install_root = "/opt/conquerd"
data_root = "/var/lib/conquerd"
release_repo = "ConquerD/ConquerD"
privilege = "root"
firewall = "ufw"

[defaults.supernode]
listen_bind = "0.0.0.0"
identity_file = "identity.json"
allow_public_rooms = false
allow_private_rooms = true

[[host]]
name = "edge-1"
ssh = "root@203.0.113.10"

[[host.instance]]
id = "a"
public_host = "203.0.113.10"
relay_port = 3478
ws_port = 34935
features = ["core.chat.v1", "room.audio.sfu", "web.host.app.v1"]
```

Important rules:

- `defaults.version = "nightly"` installs the latest GitHub nightly artifact.
- `defaults.release_repo` controls the GitHub `owner/repo` used for release
  downloads. It can be overridden by `SNM_SUPERNODE_RELEASE_REPO`.
- `version = "local"` requires `defaults.binary_path`.
- Host `arch` is optional; if omitted, install probes the host with `uname`.
- Omitted ports are allocated in memory from defaults:
  - relay: `3478 + 100 * instance_index`
  - websocket: `34935 + 100 * instance_index`
  - web: `8443 + 100 * instance_index` when a `web.host.*` feature is enabled
- Auto-allocated ports are not written back to the inventory yet.

## SSH Transport

The transport layer exposes:

- `run(command) -> stdout, stderr, exit_code`
- `upload_bytes(remote_path, bytes, mode)`
- local-file upload helper

Backends:

| Backend | Selection | Notes |
| --- | --- | --- |
| Embedded | default, `--ssh-backend embedded` | Pure Rust `russh` plus SFTP. Uses SSH keys, `SNM_SSH_PASSWORD[_<HOST>]`, and keyboard-interactive auth. |
| OpenSSH | `--ssh-backend openssh` | Wraps system `ssh` and `scp`; useful for existing SSH config, jump hosts, and agents. |

Host-key verification is not disabled. Embedded SSH reads known-hosts data;
OpenSSH delegates verification to the system client.

## CLI Surface

Running with no subcommand opens the TUI by default.

Implemented commands:

```text
init
connect
ping
install
config-push [--no-restart]
start
stop
restart
status
logs [-f] [-n lines]
invite
exec <remote command>
remove [--yes]
uninstall [--purge] [--yes]
tui
```

All host-scoped commands accept selectors:

```text
--host <name>
--instance <id>
--all
```

`remove` edits only local inventory. `uninstall` changes the remote host.
`uninstall --purge` deletes the remote instance data directory and is guarded.

## TUI Model

The TUI is built with ratatui and crossterm. It keeps UI state in
`snm-cli/src/tui/app.rs`, renders in `ui.rs`, handles input in `mod.rs`, and
runs remote work through a Tokio worker in `worker.rs`.

Main panels:

- Fleet table
- Logs / invite viewer
- Help
- Add/edit node form
- Per-node supernode config form
- Fleet settings form
- Confirm remove
- Confirm uninstall

The fleet view groups actions by intent:

- Inventory: add, edit, configs, settings
- Remote: refresh, connect, ping, install, push config
- Lifecycle: start, stop, restart, uninstall
- Inspect: logs, invite, remove

The selected-node summary shows public host, ports, SSH target, status, version,
and install source, for example `ConquerD/ConquerD@nightly`.

Remote operations are async worker commands. The UI receives status messages and
refreshes affected rows after lifecycle, install, and config-push actions.

## Supernode Host Contract

The manager assumes each instance has an isolated data directory:

```text
{data_root}/{instance_id}/
  supernode.toml
  identity.json
  peers.json
  sfu_rooms.json
  supernode_endpoints.json
  reusable_invite.json
  web/                  # in-app portal assets (seeded by binary)
  games/                # portal game demos (seeded by binary)
```

Manager-owned files:

- `supernode.toml`
- systemd template
- systemd drop-in
- shared binary path and `current` symlink
- optional ufw rules tagged by manager

Persistent files that install and config-push must not clobber:

- `identity.json`
- peer and room state
- endpoint mailbox
- reusable invite data

## Install Flow

The CLI and TUI both resolve a local binary path before calling
`install_instance`.

For `version != "local"`:

1. Probe or read host platform, such as `linux-x86_64`.
2. Resolve GitHub release asset URL from `release_repo`, `version`, and platform.
3. Download archive and `.sha256` sidecar.
4. Verify sha256.
5. Extract `conquerd-supernode` to a local temp cache.
6. Upload to `{install_root}/bin/conquerd-supernode-{version}.snm-staging`.
7. Move staging file to `{install_root}/bin/conquerd-supernode-{version}`.
8. Repoint `{install_root}/bin/current`.
9. Render and upload `supernode.toml`.
10. Render and upload systemd template and per-instance drop-in.
11. `systemctl daemon-reload`.
12. Apply firewall behavior.
13. `systemctl enable` and `systemctl start` the instance.

For `version = "local"`, steps 1 through 5 are skipped and
`defaults.binary_path` is uploaded.

Current release support:

- Linux `.tar.gz`: implemented.
- Windows `.zip`: artifact names are known, extraction is not implemented.
- Signed `releases_manifest.json`: not implemented.

## Systemd Layout

Shared template:

```text
/etc/systemd/system/conquerd-supernode@.service
```

Instance drop-in:

```text
/etc/systemd/system/conquerd-supernode@{id}.service.d/override.conf
```

The template is generic. Instance-specific data lives in the drop-in:

- `CONQUERD_HOME`
- `supernode_host`
- legacy port env vars

The manifest carries the actual listen addresses, ports, access mode, and
feature list.

Privilege modes:

- `root`: direct privileged commands.
- `sudo`: prefixes privileged commands with `sudo`.
- `rootless-systemd`: parsed and shown, but install/uninstall currently bail.

## Config Rendering

`snm-core` resolves the effective supernode config from:

- fleet defaults
- per-instance overrides
- resolved ports
- feature list

`snm-supernode` renders that into TOML.

Feature handling:

- Known feature IDs are exposed in the TUI as toggles.
- Unknown/extra feature IDs can be entered as comma-separated text.
- `room.audio.sfu` receives room policy parameters inline.
- Empty feature lists fall back to default instance features.

`config-push` uploads `supernode.toml` and the systemd drop-in. It restarts by
default, with `--no-restart` available in the CLI.

## Status and Observability

Status uses systemd and a lightweight binary probe:

- `systemctl is-active`
- `readlink -f` of `bin/current`
- `sha256sum` first 12 characters
- binary mtime from `stat`
- optional embedded build id found with `strings`

Displayed version examples:

```text
nightly@878696fcec9e
nightly@878696fcec9e.06-14
1.0.0@deadbeefcafe
```

The CLI status path also prints build id, mtime, and binary path when available.

Logs use `journalctl -u conquerd-supernode@{id}.service`. The TUI fetches a
snapshot; the CLI supports `--follow`.

Invite reads `reusable_invite.json` from the instance data directory and prints
the `conquerd://` invite URL only (no cert fingerprints — portal is QUIC-only).

HTTP health scraping is not used; the portal is not a public HTTP surface.

## Firewall Behavior

`defaults.firewall` controls install/uninstall firewall work:

| Mode | Behavior |
| --- | --- |
| `ufw` | Add/remove tagged ufw allow rules for required TCP/UDP ports. |
| `report` | Print required ports only. |
| `off` | Do nothing. |

Firewall mutation is deliberately local and simple. Cloud security groups and
other host firewalls are out of scope for the current implementation.

## Security Properties

- SSH host-key verification remains enabled.
- SSH password can be provided by `SNM_SSH_PASSWORD`, or per host by
  `SNM_SSH_PASSWORD_<HOST>` (`<HOST>` = inventory host name, uppercased, with
  non-alphanumerics as `_`); `SNM_SSH_USER[_<HOST>]` names the login user.
  `launch.ps1` can load them from gitignored `secrets.local.ps1`.
- Passwords typed interactively are cached per `user@host:port` for the life of
  the process, so one server's password is never replayed against another.
- Release archives are verified against `.sha256`.
- Running binary hashes are shown so operators can compare deployed bytes.
- Client identities and trust material are not managed by this tool.
- Destructive remote data deletion requires an explicit purge path.

Known security gaps:

- No signed manifest verification yet.
- No append-only audit log yet.
- No rollback-aware update command yet.

## Current Limitations

- Remote install targets are Linux plus systemd only.
- `plan` is not implemented.
- `update` is not implemented as a distinct safe flow.
- Rootless systemd is a stub.
- Auto-allocated ports are not persisted.
- Windows supernode zip extraction is not implemented.
- Static web assets, games, plugin signer keys, and module trust files are not
  synced by the manager.
- HTTP health/metrics scraping is not implemented.

## Near-Term Design Direction

Recommended next steps:

1. Add `plan` to compare inventory with remote state before mutation.
2. Add `update` with stage, symlink flip, restart, verify, and rollback.
3. Verify signed `releases_manifest.json` in addition to `.sha256`.
4. Persist auto-allocated ports into inventory or a lock file.
5. Implement rootless systemd.
6. Add a local audit log for operations.
7. Optional deeper journal/status probes for portal health (no public HTTP port).
