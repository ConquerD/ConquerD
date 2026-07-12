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

## Post-quantum crypto (ML-KEM / ML-DSA) — assessed 2026-07-11, deferred

Codebase is fully classical (Ed25519 + X25519 + AES-256-GCM/HKDF-SHA256). Symmetric bulk AEAD is
already PQ-adequate *if keys are*; quantum risk is key agreement (harvest-now-decrypt-later) and
signatures (forge after CRQC). Assessed for **ML-KEM** (FIPS 203, encryption/KEM) + **ML-DSA**
(FIPS 204, signatures). Defaults if/when built: **ML-KEM-768** + **ML-DSA-65**, hybrid with
classical (not pure-PQ cutover).

### Surfaces

| Role | Classical today | PQ approach |
|------|-----------------|-------------|
| Invite session key | Ephemeral X25519 → HKDF (`conquerd-invite-session-v2`) | Hybrid: X25519 ss ‖ ML-KEM ss → new HKDF info (`…-v3-hybrid`) |
| Pairwise relay / `SfuGroupKey` wrap | Static Ed25519→Montgomery DH (no FS) | KEM-DEM (see finding 2); prefer ephemeral hybrid where interactive |
| Room content | AES-GCM under sender keys | Unchanged AEAD; only key *wrap* migrates |
| Identity / signaling / invites / Space roots | Ed25519 | Dual-sign transition; high-rate envelopes stay Ed25519 early |
| Release + module manifests | Offline Ed25519 | Cheap early dual-sign (long-lived, low rate) |
| QUIC/WS/WT TLS | rustls + `ring` | Hybrid group via `aws-lc-rs` provider swap (finding 1 — transport only; build/size cost) |
| Browser `web-sdk` | `@noble/ed25519` | After native wire freeze (JS/WASM PQ) |

### Findings (priority order)

1. **Hybrid PQ TLS (transport only; real engineering cost, still a sensible first step).** Switch
   rustls 0.23 from the pinned `ring` provider to `aws-lc-rs` in client + supernode for the
   `X25519MLKEM768` hybrid group on QUIC / WebSocket / WebTransport TLS. **Not free:** hard-coded
   `rustls::crypto::ring::default_provider()` call sites (`quic_tls.rs`, `relay.rs`, `main.rs`,
   cluster-link tests), `wtransport` 0.7 currently features `ring` (must confirm aws-lc/PQ parity
   or WT stays classical while QUIC moves), larger/slower builds (AWS-LC native toolchain), bigger
   binaries, possible dual-provider graph via `reqwest` / `tokio-tungstenite`, and CI matrix risk
   (win64 + linux-x86_64 + linux-aarch64). **Scope of protection is narrow:** only transport TLS
   HNDL between upgraded peers; app-layer invite X25519, pairwise relay keys, and room AES keys stay
   classical. No invite/protocol change, but do not market as “PQ-ready.”
2. **Structural casualty — `derive_pairwise_relay_key` cannot be ported.** It relies on the
   Ed25519→Montgomery birational map so one identity key does both signing and static DH
   (`crypto.rs`); ML-DSA keys have no map to ML-KEM. Replacement is KEM-DEM: each identity carries
   a **second static ML-KEM key signed by the ML-DSA identity key**, distributed wherever
   `identity_pub` travels; sealing to a peer = encapsulate against their static KEM key and carry
   the ~1.1 KB ciphertext in the envelope header (stays non-interactive / offline-capable; cache a
   per-pair session key to amortize). Direction now matters — the "both sides derive the identical
   key" property and sorted-pair binding go away. Affects `EncryptedSignal` and `SfuGroupKey`
   sealing.
3. **Invite handshake maps cleanly** (inviter's ephemeral X25519 point → ML-KEM-768 encapsulation
   key, joiner replies with the ciphertext), but the invite blob grows 32 B → 1,184 B of key
   material + 1,088 B reply; re-check invite-link and **QR-code size limits**. Prefer out-of-band
   / compressed invite blobs over stuffing full PQ material into `conquerd://` query strings.
4. **`public_id` under ML-DSA.** Today public_id *is* the verifying key (44 chars b64); ML-DSA-65
   pubkeys are 1,952 B (~2.6k chars b64). Preferred transition: **keep Ed25519-derived `public_id`
   as the stable peer id** (room ACL / peer-store / SFU padding unchanged) and attach `ml_dsa_pub`
   as a verified binding (cross-cert by both keys). Alternative: short hash fingerprint + explicit
   full-pubkey distribution protocol-wide. `peer_id` (SHA-256 of classical pubkey) can survive
   either path if binding is explicit.
5. **Signature payload growth is fine, sigs go last.** ML-DSA-65 sigs are 3,309 B vs 64 B — do
   **not** dual-sign every high-rate signaling envelope or SFU audio frame early. Prefer dual-sign
   on low-rate objects first: **invites, Space roots** (one treehead per Space per epoch — best
   amortization), grants, tickets. Signatures have no harvest-now risk, so they migrate *after*
   KEM work — **except release-manifest (and module-manifest) signing**, which is isolated and
   long-lived: cheap to dual-sign early.
6. **Identity storage barely changes.** FIPS 203/204 support seed-based keygen (32 B ξ / 64 B d‖z);
   identity file + keyring keep storing small seeds, derive both keypairs via HKDF domain
   separation.

### Shape when built

- **Hybrid, not pure PQ** — length-prefixed X25519 ‖ ML-KEM-768 through HKDF (as TLS does) and
  dual-sign rather than replace Ed25519, so a break in either primitive isn't fatal.
- **Negotiation** (capability-style): advertise e.g. `crypto.kem.v1` /
  `crypto.sig.v1` lists; pick the best mutual hybrid. Supernode accepts classical and hybrid
  joiners during transition; opacity model unchanged (still no content access).
- **Suggested phases:** (1) hybrid TLS provider (finding 1 — build/CI cost, not free), (2) hybrid
  invite handshake + new session-key info, (3) KEM-DEM pairwise / group-key wrap, (4) dual-sign
  low-rate + release/module manifests, (5) identity ML-DSA binding + optional high-rate policy,
  (6) browser parity, (7) deprecate pure classical by policy only after ecosystem age.
- Pre-release status still allows freer wire changes than a dual-stack fleet would; app-layer
  ML-KEM CPU is usually fine vs X25519. Real costs: finding 1 build/size/CI, finding 2's redesign,
  finding 4's id strategy, ~1–3 KB envelope/invite growth.
- Crates: `aws-lc-rs` (rustls side); app layer RustCrypto `ml-kem`/`ml-dsa` (dalek-style, check
  maturity) or `libcrux-ml-kem` (formally verified). Require NIST KATs + client↔supernode cross
  KATs before calling anything “PQ-ready” in user docs.

### PQ non-goals (near-term)

- Pure ML-KEM without X25519 hybrid on first ship.
- ML-DSA on every SFU audio / high-rate control frame.
- Reviving TreeKEM for PQ rooms (still declined for membership scale; hybrid wrap of current
  pairwise sender-keys is enough).
- QUIC-stack PQ as a hard dependency of app-layer PQ (TLS hybrid is additive).
- SLH-DSA / FN-DSA unless a later size/speed trade-off demands it.

See also the Space-section post-quantum note: Layer 1 already confines directory trust to one
treehead signature per Space per epoch.

## Space Merkle tree — remaining

**Layer 1 (authenticated room tree) is shipped.** Durable invariants, file locations, and tests
live in `agents.md` (Architecture Notes / room ownership / UX agent / Roadmap). Implementation:
`space.rs` (byte-identical client + supernode), signed wire structs (`SignedSpaceRoot`,
`SpaceInclusionProof`, `SpaceGrant`), owner builder, cluster gossip + `SpaceRootStore`,
proof-based admission (`try_space_admission`) with local invite-token / creator self-admit
fallbacks, invite envelope fields, client persistence
(`RoomEntry.{space_id, parent_id, invite_policy}` in `my_rooms.dat`), `"members"` invite-policy
+ UI toggle, periodic Space-root re-broadcast, root-equivocation flagging (lighter mitigation),
and arbitrary-depth sub-room nesting via `parent_id`.

Reserved fields `inherit`, `key_commit`, and invite `space_node_key` are present but defaulted
empty/false so Layer 2 changes leaf *values*, not leaf *shape* — no `v1` label bump when it lands.

**Admission model (post RoomGrant removal):** cluster no longer replicates per-peer private-room
ACLs. Paths in use:

| Path | Scope |
|---|---|
| **Space proof + grant** (`try_space_admission`) | Cluster-portable: materialize + local `allow_peer` on any node |
| **RoomRoster** gossip | Room *existence* + `creator_id` / `invite_policy` on every member |
| **Local invite token** + local `allowed` | Same-node rejoin, shareable links, GC rematerialize re-seed |
| **Creator self-admit** | Owner joins without token/proof |
| **PeerAuth** | Unrelated: client trust / relay auth (kept) |

### Still open

- **Root-equivocation: append-only history tree (deferred).** We use a **set** tree, not an
  append-only log, so there are no consistency proofs between epochs. A malicious owner can sign
  two different roots for the same epoch. Today's equivalent (creator controls the room set on the
  supernode it talks to) is strictly weaker, so this is not a regression; the lighter mitigation
  (equivocation detection + logging) is shipped. **Still deferred:** CT-style append-only history
  with consistency proofs — which could also make the cluster roster itself (replacing the
  single-signer `SignedClusterDescriptor`) an auditable membership log.
- **`Closet` node kind.** Distinct node `kind` for a different semantic (e.g. text-only
  sub-channels). Separate, unbuilt, additive refinement — not required for nesting depth.
- **Layer 2 — key hierarchy (entirely unbuilt).** Extends the pairwise sender-keys
  `GroupKeySource` (TreeKEM declined — see Declined). Scope:
  - Per-node epoch secrets; HKDF inheritance down `inherit=true` edges
    (`child_key = HKDF(parent_epoch_secret, "space-node" ‖ node_id)`), grants downward only.
  - `inherit=false` compartments sealed as their own pairwise-distributed group (owner → each
    member, same mechanism as room sender-keys).
  - Real values for reserved `inherit` / `key_commit` leaf fields and the invite `space_node_key`
    slot ("subtree invite = node key + Merkle inclusion proof", minus the key today).
  - Capability-token admission (`access_proof.rs`: AEAD/MAC under the node key over
    `{node_id, epoch, relay_id, expiry}` + inclusion proof), swapping Ed25519 `SpaceGrant` for a
    token MAC'd under the node key **without** changing the admission call shape; under
    `"members"`, holding the node key is what mints tokens.
  - **Confirm before freezing `v1` hash labels:** a `SpaceGrant` targets exactly one `node_id` and
    proofs don't inherit — a *direct member on an inherited node* is fine in Layer 1; grant shape
    must stay compatible with "wrap node key to individual" (direct member on an inherited node =
    wrap the derived key to them individually).

### Space non-goals

- **Literal MTC / X.509.** No CA and no name→key binding authority; identities are self-authenticating
  Ed25519 keys and QUIC TLS certs are throwaway self-signed key carriers. Layer 1 already adopts only
  the MTC **pattern** (sign one treehead per epoch, disseminate out-of-band, authenticate leaves with
  inclusion proofs). Origin: evaluation of Merkle Tree Certificates
  ([Cloudflare MTC](https://blog.cloudflare.com/bootstrap-mtc/), draft-ietf-tls-trust-anchor-ids).
- **Subtree delegation.** v1: only the Space owner signs roots. Owner-signed delegation leaves are a
  compatible later addition and do not block `v1` labels.
- **Inter-cluster federation, blinded/unlinkable tokens, payments** — see Discovery / Declined
  sections; not Space-specific work.

**Post-quantum note:** codebase is classical today (full assessment: see the *Post-quantum crypto*
section above). Layer 1 still helps a future PQ migration the same way MTC does: admission/directory
trust costs **one signature per Space per epoch**, so swapping Ed25519 → ML-DSA later inflates one
treehead signature, not a per-room/per-grant fan-out.

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