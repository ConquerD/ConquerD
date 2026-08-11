# ConquerD Lightweight Threat Model (P3)

**Date:** 2026  
**Scope:** Invite + handshake, direct/relay sessions, in-app portal games, capability negotiation, supernode surfaces.  
**Style:** One-pager, asset-centric, per agents.md "formal lightweight threat model document".

## Assets
- Long-term Ed25519 identity (private seed)
- Per-session forward-secret keys (X25519 + HKDF)
- Signed invites and capability announcements
- Feature payloads (chat, voice, video, audio shared with a video, files, room state)
- Supernode relay/SFU forwarding (metadata only)
- Local stores (encrypted chat history, peer trust graph)

## Trust Boundaries & Assumptions
- **Client-only trust model**: No first-party backend for identity, discovery, or presence.
- Supernodes are untrusted for content (they see only encrypted traffic + routing metadata they need).
- All cross-peer behavior is gated by `CAPABILITY_ANNOUNCE` intersection + per-feature `auth` tier + quotas.
- Invites are the root of trust; successful Ed25519-signed handshake + transcript binding establishes a session.

## Threat Surfaces & Mitigations

### 1. Invite Creation / Distribution (out-of-band)
**Threats:**  
- Attacker forges or replays an invite.  
- Invite leaks long-term public key or allows tracking.

**Mitigations (current):**  
- Ed25519 signature over canonical bytes (includes `expires_at`, `invite_id`, ephemeral pub, optional handle/relay hints).  
- Timestamp + expiry check on accept.  
- Ephemeral X25519 per-invite for forward secrecy.  
- Optional `is_supernode` / supernode hints are advisory only.

**Residual:** Social engineering or out-of-band channel compromise. (Out of scope for protocol.)

### 2. Handshake (INVITE_HANDSHAKE_INIT / ACCEPT)
**Threats:**  
- Man-in-the-middle on the initial exchange.  
- Replay of old handshake messages.  
- Transcript tampering leading to key mismatch.

**Mitigations (current):**  
- Signed invites + ephemeral X25519 + HKDF transcript binding (`SESSION_KEY_INFO`).  
- `transcript_hash` included in ACCEPT and used in key derivation.  
- Both sides verify signatures and canonical bytes.  
- Nonce + timestamp replay windows on the signaling layer.

**Residual:** None significant if invite is fresh and signatures verify.

### 3. Post-Handshake Signaling (direct QUIC or WebSocket)
**Threats:**  
- Replay of signed signaling messages (chat, call control, capability, endpoint updates).  
- Injection of forged messages after session establishment.

**Mitigations (current):**  
- Every signaling message is Ed25519-signed over canonical bytes by the sender's long-term key.  
- `verify_inbound_signature` + timestamp freshness window (5-minute `MAX_MESSAGE_AGE_SECS` on the client; `is_fresh(300.0)` on the supernode WebSocket path).
- Per-sender `conquerd_features::ReplayGuard` rejects re-delivery of an already-seen message signature within the freshness window; real-time `SfuAudio` frames are exempt from dedup because they are high-rate and ephemeral.
- Capability intersection enforced before any feature activation.

**Residual (documented):** A monotonic sequence-number bitmap would provide stricter ordering semantics for very long-lived sessions, but duplicate signed envelopes inside the active freshness window are already rejected.

### 4. QUIC Direct Sessions
**Threats:**  
- Traffic analysis or correlation via QUIC connection metadata.  
- QUIC implementation bugs (quinn/rustls).

**Mitigations:**  
- Application messages are sent only after the signed invite/session handshake; no custom app-layer early-data path is used for trust establishment.
- ALPN `conquerd/1`; self-signed Ed25519 QUIC certificates carry the identity in the CN, with peer-id checks performed after certificate extraction.
- Per-feature quotas applied at datagram/stream layer before delivery.

**Residual:** QUIC fingerprinting (standard for any QUIC app).

### 5. QUIC Relay (supernode `QUICRelayServer`)
**Threats:**  
- Relay sees source/dest peer indices and byte volumes.  
- Malicious or compromised supernode injects or drops traffic.  
- Ticket forgery or replay.

**Mitigations (current):**  
- Tickets are Ed25519-signed by supernode with short TTL + renewal window.  
- mTLS with client cert CN checked against `allowed` set (extracted from CN).  
- No access to plaintext; all feature payloads are end-to-end signed/encrypted at the capability layer.  
- `handle_datagram` enforces same-room for room-scoped traffic; cross-room injection dropped.

**Residual:** Traffic analysis by the relay operator (accepted trade-off for NAT traversal help). No content confidentiality from supernode.

### 6. In-app portal games (`web.host.app.v1` + `game.relay.v1`)
**Threats:**  
- Malicious portal page JS (XSS / compromised asset on a supernode).  
- Injection of game datagrams into other sessions.

**Mitigations (current):**  
- Portal pages load only inside the native client (`conquerd://`); no external browser transport.  
- Pages use the **native peer's** identity — no page-local keypair or TLS cert.  
- Game fan-out is scoped to `GameRelayJoin` sessions on the QUIC relay; payloads are opaque.  
- Navigation is locked to `conquerd://` in portal mode; external links open outside the app.

**Residual:** A compromised supernode can serve hostile portal HTML (same class as any untrusted web host). Peers should only open portals of supernodes they already trust via invite.

### 7. Capability Negotiation & Feature Activation
**Threats:**  
- Downgrade to weaker feature set.  
- Unauthorized invocation of high-privilege features (`x.*` bespoke modules).  
- Quota exhaustion DoS.

**Mitigations (current):**  
- `CAPABILITY_ANNOUNCE` intersection computed on both sides.  
- Per-feature `auth` tier (public / room-member / trusted-peer) enforced before any `on_invoke`/`on_message`.  
- First-party namespaces bypass user prompt; `x.*` require explicit per-(feature,peer) consent (stored in `FeatureTrustStore`).  
- Inbound + outbound token-bucket quotas per `(feature, peer)` with symmetric `clear_*` on disconnect.  
- `gate_through_feature` called on all outbound paths (including audio).

**Residual:** Consent fatigue for many `x.*` features (mitigated by first-party preference).

### 8. Local Persistence & Key Management
**Threats:**  
- Local DB / file theft (chat history, peer store).  
- Keyring extraction on compromised OS.

**Mitigations (current):**  
- v2 identity: Argon2id + AES-GCM; optional OS keyring for passphrase.  
- Chat history encrypted at rest with per-profile AES key derived from identity.  
- `keyring_delete_aes_key` for "lock identity".  
- Peer trust graph only populated after successful signed handshake.

**Residual:** Physical access or OS compromise (standard for client-only apps).

### 9. Local media capture (camera, screen/window, shared audio)
**Threats:**
- A user shares more than they intended — a whole monitor exposes anything that pops up over it, and whole-machine audio capture picks up notifications or another call.
- Per-application audio capture silently widens: it needs Windows build 20348+, and older builds fall back to **system** audio rather than to silence.
- A supernode on the media path tries to read the picture, the sound, or their timing.

**Mitigations (current):**
- Capture starts only on an explicit user action (camera toggle, share start, settings preview) and stops with it; nothing captures in the background, and captured media is never written to disk.
- Room video and room shared audio are E2E-sealed under the per-room sender key with `MediaKind` AAD domain separation before leaving the client; the supernode forwards `ROOM_VIDEO_TAG` / `ROOM_CONTENT_AUDIO_TAG` through the generic opaque relay path with no media arm and never parses the PTS.
- Video PTS and codec are bound into the per-frame signature, so a relay cannot re-time or re-label a stream.
- Both behaviours are disclosed in `PRIVACY.md` (*Camera, screen, and shared-audio capture*), including the pre-20348 fallback.

**Residual:** The exposure is the user's own screen contents and machine audio — a disclosure and UX problem, not a crypto one. The supernode still sees that a media stream exists and its byte volume, plus camera on/off state via `SfuVideoState`.

## Overall Residual Risk Summary
- **Traffic analysis / metadata leakage** by relays or network observers (accepted for usability).  
- **Social engineering** around invite distribution (out-of-band).  
- **Supply-chain** attacks on the binary or browser (mitigated by code-signing, Sigstore, pinned actions).  
- **Long-term session ordering** without full sequence numbers (duplicate envelopes within the freshness window are blocked; strict monotonic ordering remains a possible hardening item).
- **Hostile portal assets** from an untrusted supernode (mitigated by invite-only supernode trust).

## Recommendations (aligned with P3)
- Consider adding monotonic sequence numbers + sliding-window bitmap for all post-handshake signaling if strict ordering becomes necessary.
- Formalize this document and keep it in sync with protocol changes.  
- Add negative-path tests for the replay and consent boundaries (already partially present in P0/P1 work).

---

*This document satisfies the P3 requirement for a "formal lightweight threat model document (one-pager)". It is intentionally concise and focused on the surfaces called out in agents.md.*
