# Space Merkle Tree — Remaining Work (Layer 1 shipped; open items below)

Status: **Layer 1 (authenticated room tree) is implemented and shipped.** This document now tracks
only what is **not** done. Origin and full design rationale: evaluation of Merkle Tree Certificates
([Cloudflare MTC post](https://blog.cloudflare.com/bootstrap-mtc/), draft-ietf-tls-trust-anchor-ids
ecosystem) against ConquerD, plus the "Space hierarchy" / "Trustless access proofs" backlog items
([backlog.md](../backlog.md)).

---

## Layer 1 — shipped

Authenticated room tree: data model + hashing (`space.rs`, byte-identical on client + supernode),
signed wire structs (`SignedSpaceRoot`, `SpaceInclusionProof`, `SpaceGrant`), owner builder, cluster
gossip + `SpaceRootStore`, proof-based admission (`try_space_admission`) with local invite-token /
creator self-admit fallbacks (no cluster per-peer ACL push), invite envelope fields, the §6.1
invite-minting fix, and client persistence (`RoomEntry.{space_id, parent_id, invite_policy}` in
`my_rooms.dat`) — plus the `"members"` invite-policy widening (§6.4), its client UI toggle, periodic
Space-root re-broadcast (§8), root-equivocation flagging (§9, lighter mitigation), and arbitrary-depth
sub-room nesting (§3.1, §10).

The `inherit`, `key_commit`, and invite `space_node_key` fields are **reserved** (present, defaulted
empty/false) so Layer 2 changes leaf *values*, not leaf *shape* — no `v1` label bump when it lands.

Durable invariants, exact function/file locations, and test coverage live in `agents.md` — see
Architecture Notes ("Room ownership invariant"), the UX/UI Agent role (`CreateRoomDialog.qml`), the
Supernode Opacity section, and the Roadmap & Status "Last reviewed" entries. This file does not
re-track shipped status; do not re-design any of the above without a reason logged here first.

---

## Open items

### 1. Legacy cluster RoomGrant path — **shipped / removed** (pre-release, 2026-07)

Cluster no longer replicates per-peer private-room ACLs (`ClusterMsgKind::RoomGrant` deleted).
Admission model:

| Path | Scope |
|---|---|
| **Space proof + grant** (`try_space_admission`) | Cluster-portable: materialize + local `allow_peer` on any node |
| **RoomRoster** gossip | Room *existence* + `creator_id` / `invite_policy` on every member |
| **Local invite token** + local `allowed` | Same-node rejoin, shareable links, GC rematerialize re-seed |
| **Creator self-admit** | Owner joins without token/proof |
| **PeerAuth** | Unrelated: client trust / relay auth (kept) |

Pre-release: all cluster nodes redeploy together; mixed-version RoomGrant is not supported.

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
