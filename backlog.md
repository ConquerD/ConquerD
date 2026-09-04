# Backlog — deferred / open work

Open work deferred while the core framework stabilized. The two original drivers — **E2E text
chat** and **supernode cluster support** — have both landed, as has Space Merkle **Layer 1**,
room voice/chat/file E2E (sender keys), and group-key reliability (elected keyer, ack + reseal
loop, fail-closed room content). Video calling — including A/V sync and adaptive bitrate — is complete
on Windows; what remains there is platform reach and validation, not media plumbing. See the
Video section below. What remains is grouped below by theme; ordering within each group is rough
priority.

**Durable *shipped* invariants live in `agents.md`, not here** — when an item below lands, move its
invariant into the relevant `agents.md` section and delete it from this file rather than marking it
"shipped" in place. This file is not a changelog; git history is.

---

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
   `X25519MLKEM768` hybrid group on QUIC / WebSocket TLS. **Not free:** hard-coded
   `rustls::crypto::ring::default_provider()` call sites (`quic_tls.rs`, `relay.rs`, `main.rs`,
   cluster-link tests), larger/slower builds (AWS-LC native toolchain), bigger binaries, possible
   dual-provider graph via `reqwest` / `tokio-tungstenite`, and CI matrix risk (win64 + linux-x86_64
   + linux-aarch64). **Scope of protection is narrow:** only transport TLS HNDL between upgraded
   peers; app-layer invite X25519, pairwise relay keys, and room AES keys stay classical. No
   invite/protocol change, but do not market as “PQ-ready.”
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

**Layer 1 (authenticated room tree) is shipped** — implementation, admission-path table, reserved
Layer 2 leaf fields, and the `v1` hash-label freeze rule now live in `agents.md` (Architecture
Notes → room ownership invariant). Only the open items are tracked here.

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

## Video calling — remaining

Large in-tree feature, **complete on Windows and unproven elsewhere**. Transport, capability ads,
quotas, room E2E, camera-state signaling, codec negotiation, A/V sync, adaptive bitrate, Windows
capture/encode, and call UI are built under `rust/conquerd-client/src/video/` +
`connection_manager/manager/video_session.rs` + the media-layer modules + QML `VideoTile` /
`VideoRegion` / `VideoPopoutWindow` / `VoiceRail` share control / settings preview.

What follows is what stands between that and a cross-platform capability worth calling shipped.
It is now **platform reach and validation**, not media plumbing — if you are about to write a
design for sync or bitrate control, stop and read `agents.md` first, both are done.

### Shipped scaffolding (do not re-litigate)

Merged into `agents.md` — channel tags `0x06`/`0x07` and the relay-datagram-only / one-stream-per-peer
rules (Channel-tag rules), the `core.video.v1` + `room.video.sfu` descriptor rows and the video
codec invariant (Feature Module
Reference), room-video opacity and `MediaKind::Video` AAD separation (Supernode Opacity), and the
`src/video/` module map with the Windows-only capture caveat (Architecture Notes). Also shipped and
not re-litigated here: quality presets (low / balanced / high), settings device list + local preview,
the in-call camera toggle, the `VoiceRail` share menu (source + overlays + audio choice at start),
**audio-led A/V sync** (`media_clock.rs` / `media_sync.rs` / `content_playout.rs`), the
**content-audio track** (`0x08` / `0x09`, `core.audio.content.v1` / `room.audio.content.sfu`), and
**video adaptive bitrate** (`video/sender.rs`, sharing the audio ABR stats tick).

**Docs are done too, as of 2026-08-07** — README video section / Known limitations / capabilities
table, `agents.md` (media-layer invariants, channel tags, codec + opacity rules), and the
`PRIVACY.md` *Camera, screen, and shared-audio capture* disclosure covering whole-screen exposure
and the pre-20348 per-application-audio fallback. Adding a **new capture backend is a privacy-doc
change as well as a code change** — that rule now lives in the Documentation Agent role in
`agents.md`. Do not re-open a docs item for video; extend those sections in place.

### Still open (rough priority)

1. **Screen share off Windows.**

   The codec question is **closed** (Option C, hybrid, 2026-07-30): VP8 ships on every platform
   via the vendored `conquerd-vpx`, and **camera capture now exists on all three**. Durable
   invariants live in `agents.md`. What is left is the screen/window surface:

   | Platform | Camera | Screen / window |
   |---|---|---|
   | Windows | `MfCamera` ✅ | `Windows.Graphics.Capture` ✅ |
   | Linux | `V4l2Camera` ✅ | **open** — PipeWire + `xdg-desktop-portal` (Wayland), X11 fallback |
   | macOS | `AvfCamera` ✅ | **open** — ScreenCaptureKit |

   Notes for whoever picks this up:
   - **Linux screen capture is the awkward one, and it changes the UX.** Wayland has no
     screen-scraping API by design; capture goes through `xdg-desktop-portal`'s ScreenCast
     interface over D-Bus, which returns a PipeWire node id after the *compositor* draws the
     picker. That dialog is not skippable and cannot be replaced by our own QML source list the
     way `monitor:` / `window:` ids work on Windows — so `SourceSpec::Screen` needs a
     portal-shaped variant rather than an id we choose. An X11 fallback (XComposite/XShm) covers
     older sessions and can keep the current id model.
   - **macOS needs entitlements, not just code.** ScreenCaptureKit requires Screen Recording
     permission granted in System Settings, which cannot be prompted for repeatedly, and the app
     must carry the matching usage strings. Not testable in CI.
   - VP8 already covers encode everywhere, so a new backend only has to produce `RawFrame`
     (tightly-packed I420) and the existing pipeline handles the rest.

2. **Verify the platform backends on real hardware — nothing outside Windows has captured a frame.**

   | Backend | Compiles | Logic tested | Run against a camera |
   |---|---|---|---|
   | Windows `MfCamera` | ✅ | ✅ | ✅ (dev machine) |
   | Linux `V4l2Camera` | ✅ (WSL + CI) | ✅ format choice | ⚠️ opens + negotiates, never streams — see below |
   | macOS `AvfCamera` | ✅ Rust half (verified 2026-08-07) | — | ❌ no Mac on the team |

   The macOS Rust now type-checks clean under `-D warnings` via the `lint-macos` feature, which
   is checkable from any host:

   ```
   cargo clippy -p conquerd-client --no-default-features --features lint-macos -- -D warnings
   ```

   That covers the Rust FFI only. The **Objective-C shim genuinely needs a Mac to compile and
   link** — the `test-macos` CI job is the only thing exercising it, so treat a green run there
   as the real signal, not a local clippy pass.

   **Linux, partially answered on 2026-09-03** against a Logitech C270 passed into WSL with
   `usbipd attach --wsl`. Enumeration and format negotiation now have a real driver behind
   them: the camera advertises `["YUYV", "MJPG"]`, `choose_format` correctly refuses MJPG and
   takes YUYV, and `set_format` returned exactly the requested 640x360 with no silent
   substitution.

   Frame delivery did **not** get answered, and cannot be from WSL. UVC streams over
   *isochronous* endpoints, which USB/IP's `vhci_hcd` does not implement — the kernel says
   so (`vhci_get_frame_number: Not yet implemented`) and `next_frame` then blocks forever
   rather than failing, which is a nastier failure than an error would be. The WSL kernel has
   no `vivid` test driver to stand in either (`CONFIG_V4L_TEST_DRIVERS is not set`). So stride
   handling across the three accepted formats, buffer starvation under load, and
   unplug-mid-call still need real Linux: a machine, a VM with true USB passthrough, or a
   custom WSL kernel built with `CONFIG_VIDEO_VIVID=m` — vivid offers YU12/NV12/YUYV, so it
   would exercise all three converter branches, which the C270 cannot since it offers only
   YUYV.

   On macOS still unverified: the TCC camera prompt, and whether the chosen
   `AVCaptureSessionPreset` yields the requested size.

   The `#[ignore]`d `captures_a_frame_from_the_default_camera` now has a Linux sibling (added
   2026-09-03), which also prints what the driver offers against what `choose_format` picked,
   since that answer is a property of the device rather than of the code. macOS still needs
   one. Run all three on hardware before calling any platform done.

3. **End-to-end product validation (2-client + room).**
   Transport unit tests cover tags, fragmentation, absent-peer drop, and `SfuVideoState` replay;
   that is not a substitute for:
   - Direct 1:1 camera on both legs with decode to `QVideoSink`.
   - Room multi-party fan-out (relay path, mid-join keyframe recovery, camera-off placeholders).
   - Direct-call → temporary SFU fallback still routing video on the room path (`video_route`).
   - Failure modes: no camera, camera in use, encoder unavailable, quota shed, stall vs
     intentional camera-off (`SfuVideoState` is the edge signal).
   Gate “video calls ship” on a written manual checklist, not compile alone. The README now
   describes video as complete on Windows and unvalidated elsewhere — this checklist is what
   replaces that hedge with a claim.

4. **Content-audio capture off Windows.**

   The media layer itself is **done** — `SessionMediaClock`, PTS on video fragments, the
   `core.audio.content.v1` / `room.audio.content.sfu` wire, audio-led receiver sync, and the
   opaque supernode fan-out all shipped 2026-08-02. Durable invariants live in `agents.md`
   (media-layer module map + content-audio playout invariants); do not re-derive the design here.

   What is left is capture on the other two platforms:

   | Platform | Source | State |
   |---|---|---|
   | Windows | WASAPI endpoint loopback (whole machine) and `VAD\Process_Loopback` via `ActivateAudioInterfaceAsync` (one app's process tree) | Built |
   | Linux | PipeWire / PulseAudio monitor source | Unbuilt |
   | macOS | Hardest — needs a virtual device or ScreenCaptureKit audio | Unbuilt |

   Whichever backend a platform gets, it must report **device capture offsets** per frame rather
   than counting frames out: a loopback device emits nothing at all while its source is quiet, and
   a counter silently converts every silence into permanent audio-behind-video lag. See
   `CaptureTimeline`, and the content-audio playout invariants in `agents.md`.

   Pairs naturally with screen capture (item 1), which needs the same permissions on Linux/macOS.
   **Echo hazard (open on every platform):** if content is system loopback, remote peers' audio
   played locally re-enters the loop. Needs exclude-our-own-output, ducking, or a virtual cable.

   Still unsolved by design: **lip sync for a talking-head call.** The mic is not on the synced
   timeline, so a face and its voice can drift by whatever the two paths' buffering differs by.
   Accepted: the target was "some sync, not frame-perfect", and a small webcam tile is forgiving.
   If it proves inadequate, the mic can be *additionally* carried on the media layer during video
   calls — a follow-on, and one that would need a clean handoff at camera toggle.

5. **Validate A/V sync on a real network.**

   The sync logic has unit coverage with a fake clock, but the lab measurement it was specified
   against has not been run: clap+flash under a clean network, ~1–2 % loss, and a keyframe burst,
   plus a multi-peer check that per-sender timelines never cross. Target is ±40–80 ms audio-led.
   Also confirm the voice path is byte-identical to before the media layer landed — that was the
   central premise of building beside it rather than modifying it.

6. **Receiver resilience polish.**
   Decode thread, per-peer decoder map, queue drop, and keyframe-request-on-decode-failure exist;
   still open: PLI/FIR cadence tuning, idle decoder GC under many room members, graceful degrade
   when Qt Multimedia / sink is absent (`cfg(qt_multimedia)` already degrades UI — keep that
   path honest in docs and settings). Align drop/keyframe policy with the shipped A/V sync hold
   queue (`media_sync.rs`; invariants in `agents.md`) so a PLI storm does not empty the video
   timeline while audio keeps playing.

7. **UX completeness.**
   Camera toggle + settings preview + tiles exist; remaining polish as the path stabilizes:
   - Screen-share picker UX (monitor/window ids are `monitor:` / `window:` in settings today;
     discovery UI may still be thin) — pair with content-audio toggle from item 4.
   - Multi-tile layout under several active room cameras (layout stress, not a new wire feature).
   - Confirm local-preview vs remote tile identity (preview uses local public id).
   - Separate level meters / mute for voice vs content when both are live (mix-at-sender still
     benefits from local “content mute” and “mic mute”).

### Video non-goals (near-term)

- **Multiple independent outbound streams per peer** (separate camera + screen without composite)
  — wire identifies a stream by sender only; changing that is a protocol bump, not a toggle.
- ~~**Separate content-audio wire track in v1** — prefer mix-at-sender onto existing Opus.~~
  **Reversed 2026-07-30.** Mix-at-sender was chosen because it needed no wire change, but that
  benefit evaporated once A/V sync needed a media layer anyway — and it contradicted item 4's own
  requirement that content audio not run the voice DSP stack, since one Opus encoder cannot hold
  two application modes. Content audio now gets its own stream; see item 4.
- **Frame-perfect / broadcast A/V sync** — v1 target is ±40–80 ms audio-led (item 5), not
  sample-accurate editorial sync, NTP-coupled peers, or full RTP/RTCP.
- **WebRTC / browser video** — in-app portal stays on identity QUIC; do not reintroduce public
  WebTransport or page-side SFU video clients.
- **openh264 (or other in-tree AVC) in our binary** — rejected for MPEG-LA exposure; Windows uses
  OS MFTs. Any software H.264 path needs an explicit licensing review before packaging.
- **Simulcast / SVC layers** — single encode ladder + ABR first; layered encode only if room scale
  demands it after a real multi-party load test.
- **WS fallback for room video media** — deliberately relay-datagram-only; members on WS-only
  keep audio, not video (documented on `SendRoomVideo`).

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