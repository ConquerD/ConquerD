# Backlog — deferred / open work

Open work deferred while the core framework stabilized. The two original drivers — **E2E text
chat** and **supernode cluster support** — have both landed, as has Space Merkle **Layer 1**,
room voice/chat/file E2E (sender keys), and group-key reliability (elected keyer, ack + reseal
loop, fail-closed room content). Video calling has substantial in-tree scaffolding (transport,
Windows capture/encode, UI) but is **not product-finished** — see the Video section below. What
remains is grouped below by theme; ordering within each group is rough priority.

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

Large in-tree feature, **not product-finished**. README still correctly says “No video calls yet.”
Transport, capability ads, quotas, room E2E, camera-state signaling, Windows capture/encode, and
call-UI plumbing are substantially built under `rust/conquerd-client/src/video/` +
`connection_manager/manager/video_session.rs` + QML `VideoTile` / `VideoRegion` / settings preview.
What follows is open work to make video a shippable, cross-platform capability.

### Shipped scaffolding (do not re-litigate)

Merged into `agents.md` — channel tags `0x06`/`0x07` and the relay-datagram-only / one-stream-per-peer
rules (Channel-tag rules), the `core.video.v1` + `room.video.sfu` descriptor rows and the video
codec invariant (Feature Module
Reference), room-video opacity and `MediaKind::Video` AAD separation (Supernode Opacity), and the
`src/video/` module map with the Windows-only capture caveat (Architecture Notes). Also shipped and
not re-litigated here: quality presets (low / balanced / high), settings device list + local preview,
and the in-call camera toggle.

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

2. **Verify the platform backends on real hardware — nothing outside Windows has run against a camera.**

   | Backend | Compiles | Logic tested | Run against a camera |
   |---|---|---|---|
   | Windows `MfCamera` | ✅ | ✅ | ✅ (dev machine) |
   | Linux `V4l2Camera` | ✅ (WSL + CI) | ✅ format choice | ❌ WSL has no `/dev/video*` |
   | macOS `AvfCamera` | ❌ **never compiled** | — | ❌ no Mac on the team |

   The macOS row is the important one: the Objective-C shim and its Rust FFI have **never been
   through a compiler**. The new `test-macos` CI job is the first thing that will build them, and
   it should be expected to need a round or two of fixes rather than passing first try.

   Unverified on Linux specifically: format negotiation against a real driver (V4L2 substitutes
   silently), stride handling in each of the three accepted formats, buffer starvation under
   load, and unplug-mid-call. On macOS additionally: the TCC camera prompt, and whether the
   chosen `AVCaptureSessionPreset` yields the requested size.

   The `#[ignore]`d `captures_a_frame_from_the_default_camera` test is the intended harness;
   give it Linux and macOS siblings and run all three on hardware before calling any platform
   done.

3. **End-to-end product validation (2-client + room).**
   Transport unit tests cover tags, fragmentation, absent-peer drop, and `SfuVideoState` replay;
   that is not a substitute for:
   - Direct 1:1 camera on both legs with decode to `QVideoSink`.
   - Room multi-party fan-out (relay path, mid-join keyframe recovery, camera-off placeholders).
   - Direct-call → temporary SFU fallback still routing video on the room path (`video_route`).
   - Failure modes: no camera, camera in use, encoder unavailable, quota shed, stall vs
     intentional camera-off (`SfuVideoState` is the edge signal).
   Gate “video calls ship” (and drop the README bullet) on a written manual checklist, not compile
   alone.

4. **Dual audio roles: voice mic vs stream / content audio.**
   Live video (especially screen share) often needs **application or system audio**, not the call
   microphone. Today the client has a single `audio_input_device` setting and one CPAL capture
   stream in `AudioPipeline` (`call_controller` / Settings → input combo). That is correct for
   pure voice calls; it is **not** enough for “stream what this app is playing.”

   **Product model (recommended):** treat two roles, not one device list:
   | Role | Purpose | Typical source | Codec mode |
   |---|---|---|---|
   | **Voice** | Conversation (existing call path) | Mic / line-in via CPAL | Opus `Application::Voip` (today) |
   | **Content / stream** | Audio that belongs with the video (game, browser, DAW, movie) | WASAPI loopback, process/app capture, stereo-mix, virtual cable, or a second hardware input | Prefer Opus `Application::Audio` (music-friendly); higher bitrate preset |

   **Why not just “pick a different mic” once:**
   - Users still need the **mic for commentary** while content audio plays; a single input forces
     either mute-the-game or no voiceover.
   - Loopback / process capture is not a normal cpal “input device” on many hosts (Windows needs
     WASAPI loopback or a virtual cable; Linux Pulse/PipeWire monitor sources; macOS is harder).
   - Content audio should **not** run the voice DSP stack unchanged (noise gate, VAD-gated send,
     aggressive VoIP encode) or music/game audio will sound wrong and may be suppressed.
   - Screen capture today is **pixels only** (`video::screen`); there is no paired audio track.

   **Design choices to settle before coding:**
   - **Mix vs dual outbound tracks.** v1 can **mix** voice + content into the existing single
     Opus send path (one `core.audio.opus` / `room.audio.sfu` stream, local ducking/levels). That
     needs no wire change and keeps receivers simple. Dual tracks (separate content capability or
     second sequence) only if receivers must mute commentary independently or we need different
     FEC/bitrate per role — protocol bump, not required for first streaming UX.
   - **Selection UI:** keep Settings “Call microphone” as today; add a stream-session control
     (“Share system audio” / “Share audio from: [app | output device | none]”) when video or
     screen share is active — not a second global default that rewires every voice call.
   - **Echo / feedback:** if content is system loopback, local playback of remote peers can re-enter
     the loop; need exclude-ConquerD-output, ducking, or require a virtual cable / per-app capture.
   - **Platform order:** Windows WASAPI loopback (or WGC-adjacent process audio where available)
     first, matching Windows-first video; Linux monitor sources next; macOS last.

   **Reuse:** still send on the existing audio capabilities and room E2E path — content is a
   **capture/mix source**, not a new media type. Do not invent a parallel “video soundtrack”
   channel unless mix-at-sender proves insufficient.

5. **A/V sync — shared sender timeline + PTS + audio-led playout (implementation plan).**

   Product requirement: “some sync,” not frame-perfect. Audio and video are independent paths
   today (separate capabilities, tags, sequence counters, playout). There is **no shared media
   clock** and no presentation timestamp on either media wire:
   - Direct audio: `[AUDIO_TAG][id_len][peer_id][opus]` — no PTS (`send_audio_datagram`).
   - Room audio: JSON `SfuAudio` + crypto `seq` + wall-clock `timestamp` (freshness/replay only).
   - Video fragments: `frame_seq` only (ordering/reassembly); signing/AAD do not carry media time.
   - Receiver: Opus → per-peer jitter queue + 20 ms tick (`call_controller`); video → reassemble
     → decode → `QVideoSink` ASAP. No coupling.

   Without this work, mic+camera and especially content-audio+screen (item 4) drift under loss,
   unequal buffers, and keyframe stalls. **Gate “video + content audio ships” on this plan.**

   #### Goals / non-goals

   | | v1 |
   |---|---|
   | Quality | A/V offset roughly **±40–80 ms** under normal loss |
   | Master | **Audio-led** — never stretch/warp Opus to chase video |
   | Clock scope | One **sender** timeline per call/room session; peers need not share wall clocks |
   | Multi-party | Sync is **per sender** — never compare PTS across peers |
   | Non-goals | NTP between peers, full RTP/RTCP, sample-accurate editorial sync, dual content-audio wire tracks, muxing A/V into one container |

   #### Architecture

   ```text
   SessionMediaClock (sender, t0 = voice session start)
           │ stamp at capture/mix (before encode)
     ┌─────┴──────┐
     ▼            ▼
   voice(+content mix) → Opus@pts_a    camera/screen → RawFrame@pts_v → encode
     │                                  │
     seal/sign (PTS in AAD/sig)         seal + sign (PTS bound)
     │                                  │
     wire with pts                      fragments (pts on frame; ver bump)
     │                                  │
     └──────── receiver ────────────────┘
              audio 20ms tick = master (record playout_anchor per peer)
              video hold/drop to extrapolated audio PTS (bounded queue)
   ```

   **Stamp points (discipline):** audio after mix / at the PCM boundary that becomes that Opus
   packet; video at capture or composite output **before** encode. Post-encode stamps hide encode
   latency as false skew.

   **Content audio (item 4):** mix voice + content **before** stamping the Opus PTS so one audio
   stream remains the master. Do not run two audio clocks against one video.

   #### Wire + crypto

   - **Units:** prefer `u64` microseconds since session `t0` (avoid long-session wrap bugs).
   - **Direct audio v2:** versioned framing after peer id, e.g.
     `[AUDIO_TAG][id_len][peer_id][fmt:u8][pts:u64 BE][opus]` with a clear format byte so parsers
     do not guess. Old v1 frames remain `[…][opus]` only.
   - **Room audio:** add `"pts": <u64>` on `SfuAudio` (JSON is fine for v1; do not block sync on a
     binary room-audio rewrite). Wall-clock `timestamp` stays for freshness only.
   - **Video fragments:** bump `FRAGMENT_VERSION` (e.g. `0x02`); carry `pts:u64` on the frame
     (fragment 0 is enough, or every frag for simpler reassembly). Extend
     `video_frame_signing_bytes` to bind PTS.
   - **Auth:** bind PTS into voice/media AAD (`group_key`) and video signatures so a modified PTS
     fails open/verify. Advisory-only PTS is not “done right.”
   - **Capability:** advertise `av_sync=1` + `pts_unit=us` on audio/video descriptors (or params).
     Mutual support → stamp + enforce; otherwise degrade to today’s best-effort (no hard fail of
     voice-only peers). Room: stamp when local client supports it; old receivers ignore PTS.

   #### Receiver policy (audio master, video slave)

   - Keep existing audio jitter buffer + 20 ms playout tick as master.
   - On each played Opus frame for peer P, set `playout_anchor[P] = (pts_a, Instant::now())`.
   - On PLC / silence, **advance expected audio PTS** by ~20 ms so video does not wait forever.
   - Decoded video `{pts_v, pixels}` → per-peer hold queue; display tick (~30–60 Hz):
     - `audio_now_pts = extrapolate(playout_anchor[P])`
     - drop if `pts_v < audio_now_pts - late_tol`
     - show if `pts_v <= audio_now_pts + early_tol`
     - else hold last frame
   - Starting constants (tunable): `late_tol` 40–60 ms; hold window 40–80 ms; max queue ~5–8
     frames; stall placeholder after ~300–500 ms without a renderable frame.
   - Light EMA of measured offset; step-correct by skip/hold frames only — **never** resample
     audio in v1. Reset on camera off/on, long gap, keyframe after stall, session restart.
   - **Never block the audio tick on video.**

   #### Phased implementation (PR-sized)

   | Phase | Work | Rough effort |
   |---|---|---|
   | **A — Clock + types** | New `SessionMediaClock` (e.g. `media_clock.rs`); create on voice active / destroy on end-call or leave voice room; thread-safe `now_pts_us()`; wire lifecycle from `AppBridge` / `CallController`. No wire change yet. | 0.5–1 d |
   | **B — Wire + crypto + unit tests** | Video fragment v2 + signing/AAD; direct audio framing v2; room `SfuAudio` `pts` + seal AAD; golden vectors (old frames still verify; tampered PTS fails). | 2–4 d |
   | **C — Sender plumbing** | Stamp audio at PCM/mix boundary in `AudioPipeline`; stamp video in capture/composite before encode; pass PTS through `SendAudio` / `SendVideoFrame` / `SendRoomVideo`. | 1–2 d |
   | **D — Receiver audio-led** | Plumb PTS on `DirectAudioReceived` / `SfuAudioReceived` / video frame events; store PTS in jitter queue entries; per-peer playout anchor; video hold/drop queue (extend `video::receiver` or small `VideoPlayout`); gate on `av_sync`; debug metrics for measured offset. **Hardest product logic.** | 3–5 d |
   | **E — Validation + docs** | Lab clap+flash / A/V test pattern under clean net, ~1–2% loss, keyframe burst; multi-peer per-sender check; README / capability notes; privacy unchanged (PTS is media timing). | 2–3 d |

   **Total focused estimate:** ~1.5–2.5 engineer-weeks for sync alone (Windows video path already
   capturing). Content loopback (item 4) is separate capture work; this plan still applies once
   content is mixed into the same Opus timeline.

   #### Files (expected touch set)

   | Area | Likely paths |
   |---|---|
   | Clock | new `conquerd-client/src/media_clock.rs`; `call_controller.rs`; `ui/bridge.rs` lifecycle |
   | Video wire | `video/fragment.rs`, `video/mod.rs` (signing), `video/sender.rs`, `connection_manager/manager/video_session.rs` |
   | Direct audio | `connection_manager/manager/peer_session.rs` (`send_audio_datagram` + inbound parse) |
   | Room audio | room send path, `manager/inbound.rs` `SfuAudio`, `group_key.rs` AAD |
   | Features | `conquerd-features` `wellknown.rs` params; channel_frame docs if needed |
   | Playout | `call_controller.rs` jitter; `video/receiver.rs` + bridge → `QVideoSink` |
   | Events | `connection_manager/events.rs` |
   | Tests | fragment / crypto / transport; new sync tests with a fake clock |

   Supernode stays **content-opaque**: forward binary video fragments and existing audio envelopes
   without parsing PTS. Do not teach the SFU a media timeline.

   #### Decisions to lock before Phase B

   1. `u64` µs PTS (preferred) vs `u32` coarser ticks.
   2. PTS **authenticated** (required for “done right”).
   3. Room audio keeps JSON for PTS v1 (no binary rewrite gate).
   4. Video without active voice: refuse camera, or free-run video with no sync (prefer refuse /
      voice-session-only video to keep one clock lifecycle).
   5. Capability name/params: `av_sync=1`, `pts_unit=us`.

   #### Ship checklist

   - [ ] One `SessionMediaClock` per voice session; not reused across calls
   - [ ] Audio + video stamped at capture/mix, not post-encode
   - [ ] PTS on direct audio, room audio, and video frames
   - [ ] PTS bound in signature / AAD
   - [ ] Receiver: audio jitter master; video hold/drop to PTS
   - [ ] Per-peer timelines only; PLC advances audio PTS
   - [ ] Lab offset inside ±40–80 ms (clean / light loss / keyframe burst)
   - [ ] `av_sync` graceful degrade for old peers
   - [ ] Supernode still opaque; no SFU PTS logic
   - [ ] Docs note sync expectations (seq counters alone are not enough)

   #### Validation

   Scripted or manual clap+flash (or known A/V test pattern); measure receiver playout offset
   under clean net, ~1–2% loss, and during a keyframe burst. Export or log measured A/V offset
   in debug builds to tune `late_tol` / hold window without guessing.

6. **Video adaptive bitrate (ABR).**
   Audio has room/direct ABR in `call_controller`. Video encoder exposes `set_bitrate` (MF path
   retargets a live MFT without full rebuild) but there is no closed-loop controller driving it
   from loss / underrun / fragment-drop signals. Wire a video ABR loop (or share a media-quality
   signal) so keyframe bursts and 720p presets do not melt constrained relays. Content-audio
   bitrate (item 4) should participate in the same congestion story so video + music do not
   starve each other on the relay quota. ABR must not fight A/V sync (item 5): prefer dropping
   or downscaling video before lengthening the audio buffer in ways that push lip-sync out of band.

7. **Receiver resilience polish.**
   Decode thread, per-peer decoder map, queue drop, and keyframe-request-on-decode-failure exist;
   still open: PLI/FIR cadence tuning, idle decoder GC under many room members, graceful degrade
   when Qt Multimedia / sink is absent (`cfg(qt_multimedia)` already degrades UI — keep that
   path honest in docs and settings). Align drop/keyframe policy with the A/V sync hold queue
   (item 5) so a PLI storm does not empty the video timeline while audio keeps playing.

8. **UX completeness.**
   Camera toggle + settings preview + tiles exist; remaining polish as the path stabilizes:
   - Clear “video unavailable on this platform / no encoder” messaging (not a silent toggle fail).
   - Screen-share picker UX (monitor/window ids are `monitor:` / `window:` in settings today;
     discovery UI may still be thin) — pair with content-audio toggle from item 4.
   - Multi-tile layout under several active room cameras (layout stress, not a new wire feature).
   - Confirm local-preview vs remote tile identity (preview uses local public id).
   - Separate level meters / mute for voice vs content when both are live (mix-at-sender still
     benefits from local “content mute” and “mic mute”).

9. **Docs + agent contract.**
   When video is product-ready (or when capability IDs/codec change): update README (“No video
   calls yet”, Built-in Capabilities table, privacy surface — content/loopback capture is a new
   disclosure), `agents.md` Feature Module Reference / channel-tag table, and supernode opacity
   notes if any control-plane fields grow. Keep supernode opacity: still forward opaque
   fragments; never log or persist frame payloads. Document A/V sync expectations (item 5)
   so agents do not treat independent seq counters as “good enough” for shipping video.

### Video non-goals (near-term)

- **Multiple independent outbound streams per peer** (separate camera + screen without composite)
  — wire identifies a stream by sender only; changing that is a protocol bump, not a toggle.
- **Separate content-audio wire track in v1** — prefer mix-at-sender onto existing Opus (item 4);
  dual tracks only if product requires independent remote mute of commentary vs content.
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