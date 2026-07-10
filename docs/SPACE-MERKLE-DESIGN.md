# Space Merkle Tree — Remaining Work (Layer 1 shipped; open items below)

Status: **Layer 1 (authenticated room tree) is implemented and shipped.** This document now tracks
only what is **not** done. Origin and full design rationale: evaluation of Merkle Tree Certificates
([Cloudflare MTC post](https://blog.cloudflare.com/bootstrap-mtc/), draft-ietf-tls-trust-anchor-ids
ecosystem) against ConquerD, plus the "Space hierarchy" / "Trustless access proofs" backlog items
([backlog.md](../backlog.md)).

---

## Already shipped (Layer 1 — do not re-design; see `agents.md` Roadmap)

Byte-identical `space.rs` on client + supernode (cross-crate KAT guards divergence):

- **Data model + hashing**: `SpaceNode`, `derive_node_id`, canonical leaf encoding
  (`conquerd-space-leaf-v1`), RFC 6962 leaf/interior hashing, `merkle_root` / `audit_path` /
  `root_from_audit`, `MAX_PROOF_DEPTH = 32`.
- **Signed wire structs** (all owner-Ed25519-signed, domain-separated): `SignedSpaceRoot` + `verify`,
  `SpaceInclusionProof` + `verify_against`, `SpaceGrant` + `verify`.
- **Owner builder**: `Space::{new_server, upsert_node, remove_node, root_hash, signed_root, prove,
  grant}`, epoch bump per mutation, node_id-sorted set tree.
- **Cluster + transport**: `ClusterMsgKind::SpaceRoot` gossip + `replicate_space_root` +
  `OnSpaceRootFn`; supernode `SpaceRootStore` (highest-epoch-per-space, cross-signer rejection) via
  `accept_and_gossip_space_root`; client→host `SpaceRootAnnounce` + `handle_space_root_announce`.
- **Coexist admission**: `try_space_admission` — materializes rooms from proven nodes, accepts the
  client-carried root (MTC fallback-cert / staleness path), enforces current-epoch-only + grant
  expiry, then falls through to the legacy token/ACL path unchanged.
- **Invite envelope**: `space_root` / `space_proof` / `space_grant` fields (`build_space_invite_fields`).
- **§6.1 invite-minting hole closed**: `generate_invite_token_checked` is owner-only (`InviteMint`).
- **Client persistence**: `RoomEntry.{space_id, parent_id, invite_policy}` (`#[serde(default)]`,
  golden-field test, no schema bump), `StoreData.spaces` in `my_rooms.dat`,
  `RoomStore::{space_id_for, get_space, adopt_room_into_space, set_space_linkage}`, sidebar nesting.

The `inherit`, `key_commit`, and invite `space_node_key` fields are **reserved** (present, defaulted
empty/false) so Layer 2 changes leaf *values*, not leaf *shape* — no `v1` label bump when it lands.

**Also shipped (moved here from "Open items" — see `agents.md` Roadmap "Last reviewed" note):**

- **`"members"` invite-policy enforcement (§6.4)**: `generate_invite_token_checked` resolves the
  room's effective `invite_policy` (from `SFURoom`, set at create time from the `SfuRoomCreate`
  payload or a proven `SpaceNode`) and mints for the creator under `"owner"`, or for the creator
  **or** any eligible member (participant, subscriber, or in `allowed`) under `"members"`;
  unknown/empty policy values normalize to `"owner"` (safe default). Rooms materialized from a proof
  still cannot widen minting (empty `creator_id`) — real member-*signed* capabilities arrive with
  Layer 2. See `sfu.rs::{normalize_invite_policy, is_invite_eligible_member, create_room_with_policy,
  generate_invite_token_checked}`.
- **Client UI for invite policy (§6.4)**: `CreateRoomDialog.qml` has a "members can invite" toggle
  (private rooms only), wired through `createRoom`/`createSubRoom` → `AppBridge::create_room_impl` →
  `ConnectionCommand::CreateRoom` → `SfuRoomCreate.invite_policy`. The creator's client persists the
  chosen policy in `RoomEntry.invite_policy` (`RoomStore::with_invite_policy`) and replays it on
  supernode reconnect (`replay_saved_rooms_on_supernode_connect`).
- **Periodic Space-root re-broadcast (§8)**: on top of the existing on-change gossip
  (`AnnounceSpaceRoot` / `replicate_space_root`) — cluster-side, `ClusterLink` carries a
  `LocalSpaceRootsFn` (mirroring `LocalRoomsFn`) and re-gossips every currently-held
  `SpaceRootStore` root on the same `SUBSCRIPTION_REFRESH` cadence as `broadcast_subscriptions`,
  plus once immediately when a link is freshly established (`run_link`); client-side,
  `replay_saved_rooms_on_supernode_connect` re-announces the owner's current signed Space root on
  every supernode (re)connect, covering a supernode restart with no cluster peer to re-gossip from.
- **Root-equivocation flagging — lighter mitigation (§9)**: `SpaceRootStore::accept` (supernode,
  `main.rs`) distinguishes a same-epoch/same-content resend (idempotent, unchanged) from a
  same-epoch/**different**-`root_hash` conflict — the latter is logged as a `space root equivocation
  detected` warning and counted per `space_id`, surfaced via `/api/stats`
  (`space_root_equivocations`) for operator visibility. The first-seen root is still retained since
  there is no consistency proof to say which root is "true". The heavier alternative (append-only
  history tree with consistency proofs) remains open — see below.
- **Deeper trees beyond Server → Room (§3.1, §10)**: sub-room creation and nesting is built and
  shipped — `CreateRoomDialog.qml` "Create Sub-room" → `AppBridge::create_sub_room` /
  `create_room_impl` → `RoomStore::adopt_room_into_space` nests the new room under any existing room
  node via `parent_node_id` (not just the Server root), and `MainWindow.qml` renders
  expand/collapse for rooms with children. No new `kind` value was needed for this — `parent_id`
  already points at any node id, so the existing `"room"` kind already supports arbitrary depth.
  The backlog's `Closet` idea (a *distinct* node kind, e.g. for a different semantic like text-only
  sub-channels) is a separate, still-unbuilt, additive refinement — it is not required for depth.

---

## Open items

### 1. Legacy roster-path removal + migration criteria (§5, deferred by decision — not actionable yet)

Proofs are currently added **beside** the token/ACL flow (`SFURoom.allowed`,
`generate_invite_token`/`validate_and_consume_token`, `RoomGrant`/`PeerAuth` replication), all
unchanged. Removal of the roster path is gated on: (a) failover joins succeeding on cluster members
that never saw the room's `RoomGrant`s, verified on the acdc dev cluster; (b) golden tests locked;
(c) no schema bump for one full client/supernode release cycle. Until then room creation keeps
emitting `RoomGrant` replication in parallel. **Status: criteria not yet met** — this is a
production-verification gate, not an engineering task, and must not be actioned until (a)–(c) are
satisfied on the live cluster over a full release cycle.

### 2. Root-equivocation: append-only history tree redesign (§9, heavier alternative — deferred)

We chose a **set** tree, not an append-only log, so there are no consistency proofs between epochs. A
malicious owner can sign two different roots for the same epoch. Today's equivalent (creator controls
the room set on the supernode it talks to) is strictly weaker, so this is not a regression, and the
lighter mitigation (equivocation detection + logging, above) is shipped. **Still deferred:** moving to
an append-only history tree (CT-style) with consistency proofs, which would also let the cluster
roster itself (replacing the single-signer `SignedClusterDescriptor`) become an auditable membership
log — a separate future item, not designed here.

### 3. Layer 2 — key hierarchy (explicit non-goal for Layer 1; entirely unbuilt)

The whole key layer is future work, extending the existing pairwise sender-keys `GroupKeySource`
(TreeKEM was considered for this and declined — see `backlog.md` Declined section — invite-only
rooms don't hit the O(N) rekey cost that would justify it):

- Per-node epoch secrets, HKDF inheritance down `inherit=true` edges
  (`child_key = HKDF(parent_epoch_secret, …)`).
- `inherit=false` compartments sealed as their own pairwise-distributed group (owner → each member,
  same mechanism as room sender-keys).
- Real values for the reserved `inherit` / `key_commit` leaf fields and the invite `space_node_key`
  slot ("subtree invite = node key + Merkle inclusion proof", minus the key today).
- Swap the Ed25519 `SpaceGrant` for a token MAC'd under the node key **without** changing the
  admission call shape; under `"members"`, holding the node key is what mints tokens.
- **Open detail to confirm before Layer 2 freezes anything**: a `SpaceGrant` targets exactly one
  `node_id` and proofs don't inherit, so a *direct member on an inherited node* is fine in Layer 1;
  confirm the grant shape stays compatible with "wrap node key to individual" before freezing the
  `v1` hash labels.

---

## Non-goals (unchanged — will not be built here)

- **Literal MTC / X.509.** ConquerD has no CA and no name→key binding authority; identities are
  self-authenticating Ed25519 keys and QUIC TLS certs are throwaway self-signed key carriers. We
  adopt only the MTC **pattern** (sign one treehead per epoch, disseminate out-of-band, authenticate
  leaves with inclusion proofs) — already done in Layer 1.
- **Inter-cluster federation, blinded/unlinkable tokens, payments.** Backlog items; out of scope.
- **Subtree delegation.** v1 decision: only the Space owner signs roots. Delegation (an owner-signed
  delegation leaf) is a compatible later addition that does not block `v1` labels.

## Relationship to post-quantum

None today — the codebase is entirely classical. The design still helps a future PQ migration for the
same reason MTC does: admission and directory trust cost **one signature per Space per epoch**
regardless of room or member count, so swapping Ed25519 → ML-DSA later inflates one treehead
signature, not a per-room/per-grant fan-out.
