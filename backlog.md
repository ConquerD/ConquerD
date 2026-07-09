# Backlog — deferred / open work

Open work deferred while the core framework stabilized. The two original drivers — **E2E text
chat** and **supernode cluster support** — have both landed, as has Space Merkle **Layer 1** and
room voice/chat E2E (sender keys). What remains is grouped below by theme; ordering within each group
is rough priority. Durable *shipped* invariants live in `agents.md`, not here.

---

## Crypto — group key reliability

TreeKEM was considered here and **declined** (see Declined section) — invite-only rooms sized for
manual invitation don't hit the O(N) rekey cost that would justify it, and it's the highest-risk
item that was on this list. The one real gap is making the *existing* pairwise `SfuGroupKey`
distribution (owner → each member, sealed inside `EncryptedSignal`) the reliable default for every
room-join path, instead of falling back to the deterministic per-room key (which gives zero
confidentiality vs. the relay — see `agents.md` Supernode Opacity notes). Concretely: a joiner
needs to reliably receive the current epoch key from the owner (or another already-keyed member) on
join, including the `default` room and reconnect-after-drop cases, before the deterministic key can
be treated as an emergency fallback only (not the common path).

## Space Merkle tree — remaining

`docs/SPACE-MERKLE-DESIGN.md` is the authoritative not-done list (Layer 1 shipped). In brief:

- **Layer 1 gaps:** `"members"` invite-policy widening, client UI toggle for invite policy, periodic
  Space-root re-broadcast, root-equivocation flagging (lighter mitigation), and deeper nesting
  (sub-rooms under any existing room, arbitrary depth via `parent_id`) are all **done** — see the
  "Already shipped" section of `docs/SPACE-MERKLE-DESIGN.md`. Still open: legacy-roster-path removal
  + migration criteria (gated on production verification, not actionable yet) and the CT-style
  append-only history tree redesign (the heavier alternative to the shipped equivocation-logging
  mitigation). The backlog's `Closet` idea (a distinct node `kind` for a different semantic, e.g.
  text-only sub-channels) remains a separate, unbuilt, additive refinement — not required for depth.
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