# supernode-manager

A standalone Rust CLI + TUI for deploying and operating fleets of `conquerd-supernode` instances over SSH. Lives in this repo (`ConquerD_Manager`); independent of the ConquerD application crates but targets the same supernode release artifacts.

> **Status (v0.1.0):** Production-tested. Linux remote hosts with systemd are supported end-to-end; cluster provisioning is operational (acdc a/b/c three-node cluster used for live integration testing). Default entry point is an interactive TUI; all operations are also available as CLI subcommands. Remote targets are **Linux + systemd only** — no launchd or Windows-service backends yet.

---

## 1. Goal

Let one operator, from a laptop, manage many supernodes:

- **Multiple instances on a single machine** (different ports + isolated data dirs).
- **Many machines** (VPS, bare metal, ARM) reached over plain SSH.
- A declarative `inventory.toml` plus commands for install, lifecycle, status, logs, config push, invite, and uninstall.
- No agent pre-installed on hosts: bootstrap everything over SSH.

**Non-goals:** not a ConquerD backend, identity authority, or discovery service. Only provisions and supervises the supernode process. Does not weaken the client-only, invite-only trust model — operates below ConquerD at the OS/process layer.

---

## 2. What a supernode needs to run (host contract)

Derived from `rust/conquerd-supernode` in the ConquerD repo (`config.rs`, `manifest.rs`, `main.rs`) and `docs/SUPERNODE.md`. The manager must honor this contract; it does not import ConquerD crates.

### Binary

- Executable: `conquerd-supernode` (`.exe` on Windows builds).
- Distributed via GitHub Releases (`ConquerD/ConquerD`, overridable via `defaults.release_repo` or `SNM_SUPERNODE_RELEASE_REPO`):
  - **Linux:** `conquerd-supernode-<version>-<platform>.tar.gz` + `.sha256`
  - **Windows x86_64:** `conquerd-supernode-<version>-win64.zip` + `.sha256` (zip extraction **not implemented** in the manager yet)
- Supported release platforms: `linux-x86_64`, `linux-aarch64`, `win64` (Linux only for remote install today).
- On the host the manager stages `{install_root}/bin/conquerd-supernode-{version}` and symlinks `{install_root}/bin/current` → that file.
- `defaults.version` may be a release tag (`1.0.0`, `nightly`), or `"local"` with `defaults.binary_path` pointing at a local binary on the operator machine.

### Data directory (per instance, isolated)

Resolved from `CONQUERD_HOME` (manager sets this per instance). Default layout: `{data_root}/{instance_id}` e.g. `/var/lib/conquerd/a`.

| File / dir | Purpose | Manager concern |
|---|---|---|
| `supernode.toml` | Typed feature manifest (`schema_version = 1`) | Manager renders and pushes via `install` / `config-push` |
| `identity.json` | Ed25519 node identity (first run) | **Persistent — never clobber** |
| `peers.json` | Known peers | Persistent |
| `sfu_rooms.json` | Room persistence (when SFU not ephemeral) | Persistent; restart behavior depends on supernode build |
| `supernode_endpoints.json` | Endpoint mailbox | Persistent |
| `reusable_invite.json` | Invite payload | Read by `invite` command |
| `web/`, `games/<slug>/` | In-app portal assets | Seeded by supernode binary; not synced by manager yet |
| `trusted_module_keys.txt` | Plugin signer keys | Not pushed by manager yet |

### Configuration surface

- **Preferred:** `<data_dir>/supernode.toml` — manager generates per instance via `snm-supernode::manifest::render_supernode_toml`.
- **SFU room policy:** `[defaults.supernode]` / per-instance overrides for `allow_public_rooms` / `allow_private_rooms` are emitted as **inline** `params = { … }` on the `room.audio.sfu` feature row (not a separate `[feature.params]` table).
- **Legacy env vars** (still written in systemd drop-ins): `supernode_host`, `supernode_port`, `supernode_signaling_port`. Manifest fields (`listen_addr`, `ws_listen_addr`) are authoritative in `supernode.toml`. The in-app portal uses QUIC (`web.host.app.v1`) — no public HTTP port.
- **Clustering:** an optional `[cluster]` section (with `[[cluster.member]]` rows) links several supernodes into one logical node. Additive to `schema_version = 1` — the manager renders and pushes the shared roster to every member. Full contract, provisioning flow, and firewalling in §8.
- `CONQUERD_BUILD_ID` is compiled into the binary; status probes it via `strings` when present.

### Ports per instance

Each instance binds at minimum:

- QUIC relay (UDP): default `3478 + 100×index`
- WS signaling (TCP): default `34935 + 100×index`
- **Cluster QUIC link (UDP): only when the instance is part of a cluster** — a dedicated port distinct from the relay port, set via `cluster_addr`. Suggested default `4478 + 100×index` (relay + 1000). Reachable **between cluster members only**, not the public. See §8.

There is **no** public web/game port. The in-app portal is served over the peer's existing QUIC session (`web.host.app.v1`).

Omitted relay/ws/cluster ports in inventory are auto-allocated at resolve time (`snm-core::inventory::default_*_port`). They are **not** written back into `inventory.toml` automatically — pin ports explicitly for stability.

`public_host` on each instance must be the address remote clients use for relay tickets.

### Process lifecycle

- No hot-reload; config/binary changes need a **restart** (`config-push` restarts by default; `--no-restart` to skip).
- Graceful shutdown on SIGTERM.
- Supervisor: systemd templated unit `conquerd-supernode@.service` + per-instance drop-in under `conquerd-supernode@{id}.service.d/override.conf`.

### Health / observability

Status today comes from `systemctl is-active`, binary identity probe (SHA-256 + mtime + optional embedded build id), and `journalctl`. There is no public HTTP health endpoint (portal is in-app over QUIC).

---

## 3. Architecture

```mermaid
flowchart LR
    OP[Operator laptop\nsupernode-manager]
    INV[(inventory.toml\nsecrets.local.ps1)]
    OP --- INV
    OP -- SSH --> H1[Host A\nlinux-x86_64]
    OP -- SSH --> H2[Host B\nlinux-aarch64]
    subgraph H1
      S1A[supernode@a\n:3478/:34935/:8433]
      S1B[supernode@b\n:3579/:35036/:8544]
    end
```

- **Control plane:** operator machine. Holds inventory + SSH credentials. Stateless beyond local config and a release download cache under the system temp dir.
- **Transport:** SSH (port 22). Two backends behind `SshTransport` (see §4).
- **Per host:** one or more instances — shared versioned binary, separate data dir, systemd unit instance, port triple.

**Crate layout:**

```
ConquerD_Manager/
  Cargo.toml                 # workspace
  inventory.toml             # operator fleet definition (not committed secrets)
  launch.ps1                 # Windows launcher: loads SSH credentials, runs TUI/CLI
  crates/
    snm-cli/                 # clap binary `supernode-manager` + ratatui TUI
    snm-core/                # inventory model, selector, supernode config resolution
    snm-transport/           # Transport trait; embedded (russh) + openssh backends
    snm-supernode/           # install/ops, manifest render, systemd, release download,
                             #   firewall (ufw), invite, binary identity probe
```

---

## 4. SSH transport

Both backends are implemented behind `snm_transport::SshTransport`:

| Backend | Flag / env | Notes |
|---|---|---|
| **Embedded** (default) | `--ssh-backend embedded` | Pure Rust (`russh` + `russh-sftp`). Reads `~/.ssh` keys; falls back to `SNM_SSH_PASSWORD[_<HOST>]`, then interactive keyboard-interactive. Host keys via `known_hosts`. |
| **OpenSSH** | `--ssh-backend openssh` | Wraps system `ssh`/`scp`; honors `~/.ssh/config`, agent, jump hosts. |

`SNM_SSH_BACKEND=openssh` also selects OpenSSH. On Windows, `launch.ps1` loads `secrets.local.ps1` for password auth.

Credentials are resolved per host. `SNM_SSH_PASSWORD_<HOST>` and `SNM_SSH_USER_<HOST>` — where `<HOST>` is the `[[host]].name` from `inventory.toml`, uppercased with non-alphanumerics as `_` — take precedence over the bare `SNM_SSH_PASSWORD` / `SNM_SSH_USER`. `SNM_SSH_USER_<HOST>` also overrides the `user@` in that host's `ssh` string; the bare `SNM_SSH_USER` only fills in when there is none. Typed passwords are cached per `user@host:port` for the process, never shared across hosts. Only the user override reaches the OpenSSH backend — it delegates auth to the system `ssh` client.

Capabilities used today: remote command + exit code, upload bytes/files (SFTP or scp), no streaming log follow over SSH in TUI (fetches journal snapshot).

---

## 5. Inventory model

Declarative TOML; edited by hand or via the TUI (add/edit nodes, settings panel).

```toml
[defaults]
version = "nightly"              # or "1.0.0", "local"
access_mode = "open"
user = "conquerd"
install_root = "/opt/conquerd"
data_root = "/var/lib/conquerd"
release_repo = "ConquerD/ConquerD"
privilege = "root"               # sudo | root | rootless-systemd (last: not implemented)
firewall = "ufw"                 # ufw | off | report
# binary_path = "..."            # required when version = "local"

[defaults.supernode]
listen_bind = "0.0.0.0"
identity_file = "identity.json"
allow_public_rooms = false
allow_private_rooms = true

[[host]]
name = "acdc"
ssh = "root@155.138.244.189"
# arch = "linux-x86_64"          # optional; auto-detected via ping

  [[host.instance]]
  id = "a"
  public_host = "155.138.244.189"
  relay_port = 3478
  ws_port = 34935
  features = ["core.chat.v1", "room.audio.sfu", "web.host.app.v1", "game.relay.v1", ...]
```

Rules:

- `(host.name, instance.id)` is the unique deployment key.
- `[[host.instance]]` in TOML deserializes to `host.instances` in Rust.
- Per-instance overrides: `listen_bind`, `access_mode`, `identity_file`, `allow_public_rooms`, `allow_private_rooms`.
- Features accept bare strings or `{ id, enabled, params }` objects.
- `init` scaffolds `inventory.toml` and a `secrets.toml` template; live SSH passwords typically live in gitignored `secrets.local.ps1` on Windows.

**Clusters (proposed model, §8):** group instances that should act as one logical supernode.

```toml
[[cluster]]
id = "acme-us"                              # becomes cluster_id in every member's supernode.toml
members = ["acdc/a", "acdc/b", "west/a"]    # host/instance keys
cluster_port = 4478                         # base UDP port; per-instance offset like other ports
```

The manager resolves each member's `relay_addr` / `cluster_addr` / `ws_addr` from its `public_host` + port set, and its `identity_pub` (collected after first run — see §8), then renders the shared roster into every member.

---

## 6. Command surface (implemented)

```
supernode-manager [global options] [subcommand]

Global:
  --inventory <path>     default: inventory.toml (also searched upward from exe)
  --ssh-backend <embedded|openssh>

  (no subcommand)        → TUI fleet dashboard (default)

  init [--force]
  connect [--host …] [--instance …] [--all]
  ping    [--host …] [--instance …] [--all]
  install [--host …] [--instance …] [--all]
  config-push [--host …] [--instance …] [--all] [--no-restart]
  start | stop | restart
  status  [--host …] [--instance …] [--all]
  logs    [--host …] [--instance …] [--all] [-f] [-n lines]
  invite  [--host …] [--instance …] [--all]
  exec    [--host …] [--instance …] <remote shell command>
  remove  [--host …] [--instance …] [--yes]   # local inventory only
  uninstall [--host …] [--instance …] [--purge] [--yes]
  tui
```

**TUI** (ratatui): fleet table, node add/edit, per-node supernode config editor (features + SFU policy), settings (version/repo/privilege/firewall), install/start/stop/restart, config push, logs viewer, invite copy, uninstall with purge confirmation, auto-refresh.

**Not implemented yet:** `plan` (dry-run diff), dedicated `update` (stage + symlink flip + health check + rollback), signed `releases_manifest.json` verification, audit log, HTTP metrics scrape.

---

## 7. Multi-instance on one machine

Each instance is independent:

| Concern | Layout |
|---|---|
| Data dir | `{data_root}/{instance_id}` via `CONQUERD_HOME` in systemd drop-in |
| Binary | Shared `{install_root}/bin/conquerd-supernode-{version}` + `current` symlink |
| Unit | `conquerd-supernode@{id}.service` from template `conquerd-supernode@.service` |
| Drop-in | `/etc/systemd/system/conquerd-supernode@{id}.service.d/override.conf` — `CONQUERD_HOME`, `supernode_host`, legacy port env vars |
| Manifest | `{data_root}/{id}/supernode.toml` — listen addrs, access mode, features |

Install flow (`snm-supernode::ops::install_instance`): ensure service user + dirs → upload binary → symlink `current` → push `supernode.toml` → push unit template + drop-in → `daemon-reload` → optional ufw rules → `enable` + `start`.

Uninstall: stop/disable → remove drop-in → optional `--purge` of data dir → remove tagged ufw rules. Does not remove the shared unit template or binary.

**Privilege modes:** `sudo` and `root` work. `rootless-systemd` is recognized in inventory/TUI but install/uninstall bail with "not implemented".

**Firewall:** `firewall = "ufw"` adds idempotent tagged `ufw allow` rules on install and removes them on uninstall. `report` prints required ports only. `off` skips mutation.

---

## 8. Clustering — logical supernodes

> **Supernode side: implemented** (`conquerd-supernode` `cluster.rs` / `cluster_link.rs`; client failover in `connection_manager`). **Manager side: implemented** — `cluster-sync` collects identities, renders the shared roster into every member's `supernode.toml`, applies restricted cluster-port firewall rules, and restarts members. See §6 for the CLI and §13 for the testing workflow.

Several supernodes can be **linked into a cluster** that presents as one logical supernode to clients. A client attaches to any member; members replicate room chat/audio, durable **room existence** (`RoomRoster`), Space roots, and client trust (`PeerAuth`) over a dedicated supernode↔supernode QUIC mesh — **not** per-peer private-room ACLs (cold-node admit is Space proof / local token rematerialize / creator self-admit). A client transparently **fails over to a sibling** if its member goes down. Members never see plaintext — clustering is a fan-out/replication fabric, **not** a trust escalation. One cluster hosts a given room; cross-operator sharing means linking those operators' supernodes into one cluster (there is no inter-cluster federation).

### `[cluster]` manifest section

The manager renders this into **every** member's `supernode.toml` (additive to `schema_version = 1`; older builds ignore it):

```toml
[cluster]
cluster_id = "acme-us"                    # shared by every member

[[cluster.member]]
identity_pub = "BASE64URL_ED25519"        # the member node's public_id
relay_addr   = "node-a.example:3478"      # client relay attach point
cluster_addr = "node-a.example:4478"      # dedicated supernode↔supernode QUIC link
ws_addr      = "node-a.example:34935"     # client signaling / failover attach point

[[cluster.member]]
identity_pub = "..."
relay_addr   = "node-b.example:3478"
cluster_addr = "node-b.example:4478"
ws_addr      = "node-b.example:34935"
```

- Every member lists the **full roster, including itself**; the list is identical on every member.
- A member whose own identity is **absent** from the roster logs a warning and **runs standalone** (fail-safe) — the manager must include each node in its own copy.
- `cluster_addr` is a **new dedicated UDP/QUIC port** (see §2), separate from `relay_addr`. A member without a `cluster_addr` cannot be dialed for replication.
- Members authenticate each other's links by cert CN against the roster and require every cluster message to be Ed25519-signed, so a spoofed identity cannot inject replication.
- The roster is signed by each member and advertised to clients in `SUPERNODE_INFO`; clients verify the signature against the supernode they already trust before using siblings for failover.

### Identity-first provisioning (two-phase)

The roster needs each member's `identity_pub`, which only exists **after** the node's first run generates `identity.json`. Cluster provisioning is therefore two-phase:

1. **Install** every member instance normally (first run generates `identity.json` → `public_id`).
2. **Collect** each member's `public_id` by reading `identity.json`'s `public_key` over SSH (same mechanism the manager already uses to read `reusable_invite.json`).
3. **Render** the `[cluster]` roster — all members with `relay_addr` / `cluster_addr` / `ws_addr` resolved from each instance's `public_host` + ports — into **every** member's `supernode.toml`.
4. **`config-push`** (with restart) to all members.

Adding or removing a member re-renders and re-pushes the roster to the whole cluster (a restart is required — no hot reload).

### Firewall

The `cluster_addr` UDP port must be reachable **between cluster members only**, never the public. The manager's ufw integration should open the cluster port **restricted to the member source IPs** (not `0.0.0.0`), tagged like the other rules for clean uninstall. Client-facing ports (relay/ws/web) stay publicly open as today.

### Manager work (complete)

| Capability | Notes |
|---|---|
| Cluster declaration in inventory | `[[cluster]]` id + members + `cluster_port` (see §5) |
| Cluster-port allocation | fourth port per instance; default `4478 + 100×index`, pin for stability |
| Identity collection | read `identity.json` `public_key` per member over SSH |
| Roster render into `supernode.toml` | shared `[cluster]` block pushed to every member |
| Re-push on membership change | `cluster-sync` re-renders + restarts fleet-wide |
| Restricted cluster-port firewall | ufw allows the cluster port from member IPs only |
| Cluster section preservation | `install` and `config-push` read and re-append the existing `[cluster]` block so binary updates never wipe the cluster config |

---

## 9. Binary version identity (status)

`status` and the TUI fleet table show more than the inventory pin so operators can compare builds across instances:

| Field | Source |
|---|---|
| Inventory pin | `defaults.version` (`nightly`, `1.0.0`, …) |
| SHA-256 short | First 12 hex chars of running binary behind `bin/current` |
| Date suffix | Binary mtime `·MM-DD` |
| Build id | Optional `CONQUERD_BUILD_ID` / git sha from `strings` |

**Display format:** `nightly@878696fcec9e·06-14` (CLI `version_detail()` also prints `build=…`, `mtime=…`, `bin=…`).

Implementation: `snm-supernode::binary_probe` — remote `readlink`, `sha256sum`, `stat`, `strings`.

---

## 10. Release download and updates

**Implemented:**

- Resolve GitHub release URL from `version` + detected/normalized platform (`linux-x86_64`, `linux-aarch64`).
- Download archive + `.sha256` sidecar; verify SHA-256 before extract.
- Cache extracted binary locally; upload on `install`.

**Partial / gaps:**

- Re-running `install` overwrites the versioned binary path and refreshes the `current` symlink — there is no separate `update` command with rollback.
- No Ed25519 verification against ConquerD `releases_manifest.json`.
- `version = "local"` skips download; operator must set `defaults.binary_path`.
- Windows `.zip` artifacts are rejected at extract time.

`identity.json` and other persistent state are never regenerated by install/config-push (manifest upload only touches `supernode.toml`).

---

## 11. Security considerations

- **SSH host-key verification** enabled for embedded backend (`known_hosts`); OpenSSH uses system `known_hosts`.
- **Secrets:** `SNM_SSH_PASSWORD[_<HOST>]` / `SNM_SSH_USER[_<HOST>]` via gitignored `secrets.local.ps1`; `secrets.toml` template for future access codes — not logged.
- **Least privilege:** dedicated service user created on install; `sudo`/`root` for unit installation.
- **Binary provenance:** SHA-256 sidecar verified on download; running binary SHA shown in status. Signed manifest verification not yet implemented.
- **Destructive actions:** TUI confirm dialogs for remove/uninstall/purge; CLI `--yes` to skip prompts.
- **No trust escalation into ConquerD:** manager never mints client identities or invites — only reads `reusable_invite.json` from the node data dir.
- **Cluster links:** supernode↔supernode QUIC authenticates peers by cert CN against the signed roster and requires Ed25519-signed cluster messages, so a spoofed identity cannot inject replication. The manager must firewall `cluster_addr` to member IPs only (§8) — it is a trusted, intra-cluster surface, not a public one.

---

## 12. Implementation status

| Area | State |
|---|---|
| Single + multi-instance Linux deploy | Done |
| Multi-host fleet | Done |
| TUI dashboard | Done (default entry) |
| Embedded + OpenSSH transport | Done |
| GitHub release fetch + sha256 | Done |
| `config-push` + SFU inline params | Done |
| ufw firewall tagging | Done |
| Binary identity in status/TUI | Done |
| Supernode clustering (chat/ACL/trust replication + client failover) | Done — supernode + client sides |
| Manager cluster provisioning (roster render, identity collection, cluster port + firewall, cluster section preservation across redeploys) | Done |
| `plan`, `update` + rollback | Not started |
| rootless systemd | Stub only |
| Signed manifest verify | Not started |
| Audit log | Not started |
| macOS/Windows remote supervisors | Not started |
| Plugin keys / asset sync | Not started |

**Suggested next work:** dedicated `update` with symlink flip + health poll + rollback; `plan` diff; persist auto-allocated ports; rootless systemd; signed release verification.

---

## 13. Operator quick reference (Windows)

```powershell
# Build
cargo build --release

# Launch TUI (loads password from secrets.local.ps1)
.\launch.ps1

# CLI examples
.\launch.ps1 status --host acdc --all
.\launch.ps1 config-push --host acdc --instance a
.\launch.ps1 invite --host acdc --instance b
.\launch.ps1 logs --host acdc --instance a -n 200
```

Compare `VERSION` column hashes after deploy — matching `@` suffix means the same binary bytes even when both pins read `nightly`.

---

## 13a. Testing real supernodes and clusters

The manager is the primary integration-testing tool for the ConquerD supernode. Use it to set up a live cluster, verify cluster state, and debug remotely without logging into VMs.

### Cluster bring-up

```powershell
# 1. Install all nodes (or build + deploy from source)
.\launch.ps1 install --host acdc --all         # download nightly
# or
.\launch.ps1 build-deploy --host acdc --all    # compile local source

# 2. Provision cluster — collect identity keys, push shared roster, restart
.\launch.ps1 cluster-sync

# 3. Verify — each node should show 8 cluster-related lines
.\launch.ps1 exec --host acdc --instance a "grep -c cluster /var/lib/conquerd/a/supernode.toml"
.\launch.ps1 exec --host acdc --instance b "grep -c cluster /var/lib/conquerd/b/supernode.toml"
.\launch.ps1 exec --host acdc --instance c "grep -c cluster /var/lib/conquerd/c/supernode.toml"
```

### Direct remote debugging via `exec`

`exec` runs an arbitrary shell command on the first matching instance over SSH and streams the output. Use it to inspect live state without an SSH session:

```powershell
# Read the live manifest
.\launch.ps1 exec --host acdc --instance a "cat /var/lib/conquerd/a/supernode.toml"

# Check the web portal template is seeded
.\launch.ps1 exec --host acdc --instance a "head -5 /var/lib/conquerd/a/web/index.html"

# Confirm identity
.\launch.ps1 exec --host acdc --instance a "cat /var/lib/conquerd/a/identity.json | python3 -c 'import sys,json; d=json.load(sys.stdin); print(d[\"public_key\"][:20])'"

# Tail recent logs
.\launch.ps1 logs --host acdc --instance a -n 100

# Check systemd unit status
.\launch.ps1 exec --host acdc --instance a "systemctl status conquerd-supernode@a --no-pager"
```

### Workflow: redeploy without losing cluster config

`install` and `config-push` both preserve the existing `[cluster]` section when re-rendering `supernode.toml`. Only `cluster-sync` intentionally replaces the roster (with freshly collected identity keys). Safe redeploy sequence:

```powershell
# Build and redeploy — cluster section is preserved, service restarted
.\launch.ps1 build-deploy --host acdc --all

# Spot-check cluster config survived
.\launch.ps1 exec --host acdc --instance a "grep cluster_id /var/lib/conquerd/a/supernode.toml"
```

If a manifest somehow loses the cluster section (e.g. manual edit), just re-run `cluster-sync` to restore it.

### Fetch invite for client testing

```powershell
.\launch.ps1 invite --host acdc --instance a
```

Prints the reusable invite URL. Paste the invite into the ConquerD client to join the cluster via node a; the client will receive the full cluster roster in `SUPERNODE_INFO` and can fail over to b or c automatically.

---

## 14. Open questions

- Should auto-allocated ports be written back to `inventory.toml` or a sidecar lock file?
- Should `update` replace `install` for binary-only changes, or remain separate?
- macOS as a remote target (launchd) — needed?
- Firewall: keep ufw-only, or add firewalld/cloud SG reporting?
- Long-running reconciliation daemon vs strict invocation-based CLI?
- Clusters: auto-collect member identities and re-push the roster on every membership change, or gate it behind an explicit `cluster sync` command?
- Cluster port: restrict the ufw rule to member source IPs automatically, or leave the scoping to the operator?
- Roster source of truth: derive it entirely from inventory each push, or persist the resolved roster (with collected identities) in a sidecar to avoid re-reading `identity.json` every time?