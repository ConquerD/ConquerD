# Backlog — deferred / open work

Open work deferred while the core framework stabilized. The two original drivers — **E2E text
chat** and **supernode cluster support** — have both landed, as has Space Merkle **Layer 1** and
room voice/chat E2E (sender keys). What remains is grouped below by theme; ordering within each group
is rough priority. Durable *shipped* invariants live in `agents.md`, not here.

---

## Crypto — group key reliability — shipped (2026-07-09)

TreeKEM was considered here and **declined** (see Declined section) — invite-only rooms sized for
manual invitation don't hit the O(N) rekey cost that would justify it. The real gap — the pairwise
`SfuGroupKey` distribution silently never being consumed (encryption was pinned to the deterministic
per-room key regardless of any distributed key material) and having no keyer at all for the
ownerless built-in `default` room — is now closed:

- `group_key.rs`: `SenderKeysGroup::{current_epoch, epoch_key}` now actually read installed epoch
  key state instead of hardcoding epoch 0 → deterministic key forever; the deterministic key is now
  a true emergency fallback used only until real key material exists for a conversation.
  `has_real_key` distinguishes "holds distributed key material" from the always-available
  deterministic fallback.
- `connection_manager/manager.rs`: distribution is no longer gated on "did I create this room" —
  any member holding real key material can act as a room's "keyer" (bootstrap the first epoch,
  rotate on departure, reseal to newcomers), chosen deterministically per membership snapshot as the
  lexicographically smallest `public_id` present (`is_elected_keyer`). This covers the `default`
  room (no client-side creator exists for it) and reconnect-after-drop (the next-smallest remaining
  member takes over automatically). A narrow bootstrap race remains possible if two members join
  before either observes the other (documented in code); it self-heals on the next shared membership
  snapshot.

**Follow-up hardening — shipped (2026-07-10):** the dual-keyer variant of the bootstrap race (two lone
joiners each mint a different epoch-0 key) is closed by deferring first-key minting until a second
member is visible. Installing a received `SfuGroupKey` now requires the sender to be the current
elected keyer at a plausible epoch (`accept_group_key_from`) — closes the "any room peer can push a
bogus key" DoS caveat. Delivery is now acked (`SfuGroupKeyAck`) with a 750ms/16-attempt reseal loop so
a lost `EncryptedSignal` envelope self-heals instead of permanently desyncing a member. Room chat/file/
audio now fail closed (drop) instead of falling back to cleartext when no real key exists yet. See
`agents.md` Supernode Opacity section.

## Space Merkle tree — remaining

`docs/SPACE-MERKLE-DESIGN.md` is the authoritative not-done list (Layer 1 shipped). In brief:

- **Layer 1 gaps:** `"members"` invite-policy widening, client UI toggle for invite policy, periodic
  Space-root re-broadcast, root-equivocation flagging (lighter mitigation), deeper nesting
  (sub-rooms under any existing room, arbitrary depth via `parent_id`), and **legacy cluster
  RoomGrant ACL removal** (per-peer room membership no longer cluster-replicated; cold nodes admit
  via Space proof / local token rematerialize / creator self-admit; `RoomRoster` remains existence-
  only) are all **done** — see `docs/SPACE-MERKLE-DESIGN.md`. Still open: the CT-style append-only
  history tree redesign (heavier alternative to the shipped equivocation-logging mitigation). The
  backlog's `Closet` idea (a distinct node `kind` for a different semantic, e.g. text-only
  sub-channels) remains a separate, unbuilt, additive refinement — not required for depth.
- **Layer 2:** per-node epoch secrets, HKDF inheritance down `inherit=true` edges
  (`child_key = HKDF(parent_epoch_secret, "space-node" ‖ node_id)`, grants downward only),
  `inherit=false` compartments sealed with their own pairwise-distributed group key (owner → each
  member, same mechanism as room sender-keys — no TreeKEM), and the capability-token admission path
  (`access_proof.rs`: AEAD/MAC under the node key over `{node_id, epoch, relay_id, expiry}` +
  inclusion proof, replacing the reused room ACL). Reserved `inherit` / `key_commit` /
  `space_node_key` fields mean Layer 2 changes leaf *values*, not leaf *shape*; confirm `SpaceGrant`
  stays compatible with "wrap node key to individual" before freezing the `v1` hash labels. Open
  detail: a *direct* member on an inherited node = wrap the derived key to them individually.

## Modularity / plugins

- **WASM plugin sandbox.** Bespoke feature modules are native cdylibs with load-time trust prompts
  today; a WASM sandbox would remove the "trust the binary" requirement.
- **Ollama / plugin UX polish** — currently experimental.

## Audio quality — remaining

- **Polyphase resampling (V7).** Linear interpolation is near-inaudible for 8 kHz voice at typical
  device rates; a windowed-sinc or `rubato` `FastFixedIn` drop-in is the right fix if it becomes
  perceptible.
- **Stereo / spatial mixdown (V8).** Per-peer pan in `mix_pcm_frames` + stereo ring buffer.
  Low-priority, pure UX enhancement.

## Discovery / federation (speculative — only if demand appears)

- **In-band capability gossip.** Connected peers exchange each other's supernode capability bundles
  (cluster descriptors + Space-root references), built on existing signaling — not Gossipsub — while
  the invite-only trust root stays intact.
- **Signed RelayAds + capacity-aware selection.** `RelayAd` (capacity/load from `RelayStats`) +
  weighted selection for picking a cluster member / relay set; `HandoffTicket` for directed
  migration. Only worth building once relay sets grow past a hand-managed cluster.
- **Inter-cluster federation.** Cluster peering or a DHT — only if cross-operator federation beyond
  "link into one cluster" is ever required.

---

## Declined / out of scope

- **Custom minimal TreeKEM.** Considered as a `treekem.rs` implementation behind `GroupKeySource`
  (left-balanced ratchet tree, O(log N) add/remove vs. the pairwise scheme's O(N)). Declined: rooms
  here are invite-only and sized for manual invitation, not the large/high-churn membership that
  makes O(N) pairwise rekeying (already implemented — the `SfuGroupKey` path) a real bottleneck.
  Reimplementing a group-key-agreement protocol from spec was also the single highest-risk item in
  this backlog (needs KATs, fuzzing, external review before trusting it). Revisit only if room size
  or churn actually grows enough for O(N) rekey cost to bite in practice.
- **Per-message forward secrecy (double-ratchet).** Considered as an upgrade over per-epoch keying.
  Declined: per-message compromise recovery matters most for long-lived pairwise conversations
  (Signal's model); marginal benefit for ephemeral SFU room sessions rekeyed on membership change.
- **Payments / usage receipts.** A `UsageReceipt` credits/Lightning layer conflicts with the
  no-backend, volunteer-supernode, privacy-first model. Not planned; revisit only as a standalone
  design if the economics of relay hosting ever demand it.
- **Read receipts (`MessageStatus::Read`).** Deliberately unwired — surfacing read timestamps is at
  odds with the privacy-first stance. If ever revisited, gate behind a privacy toggle defaulting
  **off**.
- **Room-chat history for joiners.** Struck from scope — the supernode does not persist messages.
- **Offline store-and-forward.** Would require the supernode to hold signed, supernode-opaque
  E2E-encrypted messages with a TTL. Struck from scope — the supernode does not persist messages.