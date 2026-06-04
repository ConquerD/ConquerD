# ConquerD Lightweight Threat Model (P3)

**Date:** 2026  
**Scope:** Invite + handshake, direct/relay sessions, WebTransport/browser clients, capability negotiation, supernode surfaces.  
**Style:** One-pager, asset-centric, per agents.md "formal lightweight threat model document".

## Assets
- Long-term Ed25519 identity (private seed)
- Per-session forward-secret keys (X25519 + HKDF)
- Signed invites and capability announcements
- Feature payloads (chat, audio, files, room state)
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
- `verify_inbound_signature` + timestamp freshness window (P0 replay protection: 5-minute `MAX_MESSAGE_AGE_SECS`).  
- Per-peer transcript ordering on some paths; capability intersection enforced before any feature activation.

**Residual (documented):** Sliding-window counter/bitmap not yet implemented for all signaling (P0 partial mitigation via timestamp). Full sequence + bitmap would be ideal for very long-lived sessions.

### 4. QUIC Direct Sessions
**Threats:**  
- Traffic analysis or correlation via QUIC connection metadata.  
- QUIC implementation bugs (quinn/rustls).

**Mitigations:**  
- 0-RTT disabled for initial handshake; forward-secret resumption.  
- ALPN `conquerd/1`; strict Ed25519 client cert verification (CN = peer identity).  
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

### 6. WebTransport / Browser Clients (`web.host.h3.v1`)
**Threats:**  
- Browser-origin attacks (XSS, malicious JS, compromised tab).  
- Weaker client identity (browser has no OS keyring).  
- Injection into native peer sessions.

**Mitigations (current):**  
- Ed25519 handshake identical to native (browser SDK performs it over WebTransport).  
- `verify_browser_envelope` on supernode before dispatching to native peers.  
- Capability intersection + consent gate still enforced (`FeatureRegistry`).  
- Browser only ever talks to a user-chosen supernode; no direct P2P from arbitrary origins.

**Residual:** Browser supply-chain / extension attacks. (Same as any web app.)

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

## Overall Residual Risk Summary
- **Traffic analysis / metadata leakage** by relays or network observers (accepted for usability).  
- **Social engineering** around invite distribution (out-of-band).  
- **Supply-chain** attacks on the binary or browser (mitigated by code-signing, Sigstore, pinned actions).  
- **Long-term session replay** without full sequence numbers (partially mitigated by timestamp windows; full bitmap is future work).  
- **Browser-origin attacks** on WebTransport clients (same risk surface as any web app).

## Recommendations (aligned with P3)
- Consider adding monotonic sequence numbers + sliding-window bitmap for all post-handshake signaling (closes the remaining replay window).  
- Formalize this document and keep it in sync with protocol changes.  
- Add negative-path tests for the replay and consent boundaries (already partially present in P0/P1 work).

---

*This document satisfies the P3 requirement for a "formal lightweight threat model document (one-pager)". It is intentionally concise and focused on the surfaces called out in agents.md.*