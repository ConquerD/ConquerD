// ConquerD browser SDK — minimal client for the WebTransport bridge
// hosted by `conquerd-supernode`'s `web.host.h3.v1` capability.
//
// Wire contract (locked by the Rust supernode):
//
//   * Connect to `https://<host>:<port>/channels/<peer_id>?caps=<csv>&room=<id>`
//     where `peer_id` is the **base64url Ed25519 public key** the SDK
//     just generated. The supernode parses this path; missing/wrong
//     shape → fail-closed (no advertised capabilities).
//
//   * Identity handshake on a server-initiated bidi stream: server
//     writes 32 bytes (challenge), client writes back a 64-byte Ed25519
//     signature over those bytes. Until verified, `room.*` traffic is
//     dropped server-side (`PairStats.inbound_dropped_unverified`).
//
//   * Datagram framing: `[1-byte channel tag][payload]`. Tags in
//     `0x10..=0xEF` are the dynamic feature range; the supernode
//     allocates one tag per advertised capability in announce order, so
//     `caps=room.audio.sfu,room.chat.v1` gives tags `0x10` and `0x11`.
//     The fixed range `0x00..=0x0F` is reserved for first-party core
//     channels (see {@link ChannelTag}) — `control`, `core.audio.opus`,
//     `core.chat.v1`, `core.file.v1` — agreed statically by all
//     components (native peer, relay, supernode, this SDK) with no
//     negotiation. Use {@link encodeFrame}/{@link decodeFrame} for both.
//
//   * For `room.*` features the payload MUST be a UTF-8 JSON
//     `SignalingMessage` envelope (see {@link buildEnvelope}). The
//     supernode verifies the envelope's signature and sender against
//     the verified identity before relaying onto the native signaling
//     fabric.
//
// Dependencies: `@noble/ed25519` for the Ed25519 primitives. Loaded
// from esm.sh by default; pass `{ed25519}` to the constructor to
// override (e.g. for a bundled offline copy).

const DEFAULT_ED25519_URL = "https://esm.sh/@noble/ed25519@2.1.0";
// noble/ed25519 v2 uses webcrypto for SHA-512 in browsers, but its
// detection can fail inside custom schemes (conquerd://).  We always
// configure sha512Sync via @noble/hashes so the SDK works everywhere.
const DEFAULT_SHA512_URL  = "https://esm.sh/@noble/hashes@1.7.1/sha512";

/** Reverse-DNS feature id (e.g. `"room.audio.sfu"`). */
/** @typedef {string} FeatureId */

/** Raw envelope shape — keys not yet ordered. */
/**
 * @typedef {Object} EnvelopeInput
 * @property {string} type      One of the supernode's `MessageType` strings
 *                              (e.g. `"sfu_audio"`, `"sfu_chat"`).
 * @property {object} payload   Feature-specific JSON payload.
 * @property {string} [target]  Optional recipient peer id.
 */

const PROTOCOL_VERSION = 2;

/**
 * Fixed first-party channel tags (reserved range `0x00..=0x0F`), mirroring
 * `conquerd_features::channel_frame` on the native side. Unlike the dynamic
 * `0x10..=0xEF` feature tags the supernode allocates per advertised
 * capability, these are statically agreed by every component (native peer,
 * relay client, supernode, and this SDK) so the core channels need no
 * negotiation round-trip.
 *
 * @readonly
 * @enum {number}
 */
export const ChannelTag = Object.freeze({
    /** Control / signaling (handshake, capability announce). */
    CONTROL: 0x00,
    /** Direct peer audio (`core.audio.opus`). */
    AUDIO: 0x01,
    /** Text chat (`core.chat.v1`). */
    CHAT: 0x02,
    /** File transfer (`core.file.v1`). */
    FILE: 0x03,
});

/** Fixed channel tag for a first-party feature id, or `undefined`. */
export function fixedTagFor(featureId) {
    switch (featureId) {
        case "core.audio.opus": return ChannelTag.AUDIO;
        case "core.chat.v1": return ChannelTag.CHAT;
        case "core.file.v1": return ChannelTag.FILE;
        default: return undefined;
    }
}

/** First-party feature id bound to a fixed channel tag, or `null`. */
export function featureForFixedTag(tag) {
    switch (tag) {
        case ChannelTag.AUDIO: return "core.audio.opus";
        case ChannelTag.CHAT: return "core.chat.v1";
        case ChannelTag.FILE: return "core.file.v1";
        default: return null;
    }
}

/** Frame a payload as `[tag][payload]`, matching `channel_frame::encode_frame`. */
export function encodeFrame(tag, payload) {
    const body = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
    const framed = new Uint8Array(1 + body.length);
    framed[0] = tag;
    framed.set(body, 1);
    return framed;
}

/** Split `[tag][payload]` into `{tag, payload}`, or `null` for an empty frame. */
export function decodeFrame(frame) {
    if (!frame || frame.length === 0) return null;
    return { tag: frame[0], payload: frame.subarray(1) };
}

/** base64url-encode a `Uint8Array`. */
function b64urlEncode(bytes) {
    let bin = "";
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    return btoa(bin)
        .replace(/\+/g, "-")
        .replace(/\//g, "_")
        .replace(/=+$/, "");
}

/** base64url-decode to `Uint8Array`. */
function b64urlDecode(s) {
    const pad = (4 - (s.length % 4)) % 4;
    const b64 = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat(pad);
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
}

/**
 * Build the canonical bytes of a `SignalingMessage` exactly as the
 * Rust supernode does: sorted-key JSON containing `payload`, `sender`,
 * optionally `target`, then `timestamp`, `type`, `v`. The `signature`
 * field is excluded.
 *
 * `serde_json::Map` is a `BTreeMap` so its iteration order is
 * lexicographic; we mirror that here.
 */
function canonicalBytes(env) {
    const ordered = {
        payload: env.payload,
        sender: env.sender,
        ...(env.target !== undefined ? { target: env.target } : {}),
        timestamp: env.timestamp,
        type: env.type,
        v: env.v,
    };
    return new TextEncoder().encode(JSON.stringify(ordered));
}

/**
 * Build and sign a `SignalingMessage` envelope ready for the wire.
 *
 * @param {EnvelopeInput} input
 * @param {{publicKey: Uint8Array, secretKey: Uint8Array}} keys
 * @param {{sign(msg: Uint8Array, sk: Uint8Array): Promise<Uint8Array>}} ed25519
 * @returns {Promise<string>} compact JSON string
 */
export async function buildEnvelope(input, keys, ed25519) {
    const sender = b64urlEncode(keys.publicKey);
    const env = {
        type: input.type,
        sender,
        payload: input.payload,
        // Integer seconds-since-epoch round-trips identically through
        // Rust's serde_json number formatter and JS `JSON.stringify`.
        timestamp: Math.floor(Date.now() / 1000),
        v: PROTOCOL_VERSION,
    };
    if (input.target !== undefined) env.target = input.target;
    const sig = await ed25519.sign(canonicalBytes(env), keys.secretKey);
    return JSON.stringify({
        type: env.type,
        sender: env.sender,
        payload: env.payload,
        timestamp: env.timestamp,
        v: env.v,
        ...(env.target !== undefined ? { target: env.target } : {}),
        signature: b64urlEncode(sig),
    });
}

/**
 * Generate a fresh Ed25519 keypair. `secretKey` is the 32-byte seed;
 * `publicKey` is the 32-byte public point. The base64url public key is
 * also the `peer_id` the supernode expects on the WT path.
 */
export async function generateKeypair(ed25519) {
    const secretKey = ed25519.utils.randomPrivateKey();
    const publicKey = await ed25519.getPublicKey(secretKey);
    return { secretKey, publicKey, peerId: b64urlEncode(publicKey) };
}

/**
 * Low-level browser <-> supernode WebTransport session (post-handshake).
 *
 * Most game demos use the higher-level `ConquerdClient` wrapper instead.
 * Use this directly only when you need signed envelopes (room.* features)
 * or full control over the transport.
 *
 * Use {@link RawConquerdClient.connect} to obtain an instance.
 */
export class RawConquerdClient {
    constructor({ transport, keys, peerId, caps, ed25519 }) {
        this.transport = transport;
        this.keys = keys;
        this.peerId = peerId;
        this.caps = caps;
        this.ed25519 = ed25519;
        // Feature -> 1-byte channel tag, mirroring the supernode's
        // announce-order allocation in `handle_session`.
        this.tagFor = new Map();
        caps.forEach((fid, i) => this.tagFor.set(fid, 0x10 + i));
        // Reverse for fast tag -> feature lookup in datagram path
        this.featureForTag = new Map();
        caps.forEach((fid, i) => this.featureForTag.set(0x10 + i, fid));
        this.onDatagram = null; // (tag, body, featureId|null) set by caller
        this._readerTask = this._readDatagrams();
    }

    /**
     * Connect, perform the identity handshake, and return a ready
     * client.
     *
     * @param {{
     *   url: string,            // e.g. "https://relay.example:8443"
     *   caps: FeatureId[],
     *   room?: string,
     *   certHash?: string,      // hex SHA-256 of server cert DER — enables serverCertificateHashes
     *   ed25519?: object,       // override the default @noble import
     * }} opts
     */
    static async connect(opts) {
        const ed25519 = opts.ed25519 ?? await (async () => {
            const mod = await import(DEFAULT_ED25519_URL);
            // Configure sha512Sync so noble/ed25519 v2 works inside
            // conquerd:// where webcrypto detection may fail.
            const { sha512 } = await import(DEFAULT_SHA512_URL);
            mod.etc.sha512Sync = (...msgs) => {
                let len = 0;
                for (const m of msgs) len += m.length;
                const buf = new Uint8Array(len);
                let off = 0;
                for (const m of msgs) { buf.set(m, off); off += m.length; }
                return sha512(buf);
            };
            return mod;
        })();
        const keys = await generateKeypair(ed25519);
        const params = new URLSearchParams();
        if (opts.caps.length) params.set("caps", opts.caps.join(","));
        if (opts.room) params.set("room", opts.room);
        const path = `/channels/${keys.peerId}${
            params.toString() ? "?" + params.toString() : ""
        }`;
        const url = new URL(path, opts.url).toString();
        // Build WebTransport options.  When a cert fingerprint is provided
        // (delivered via the ConquerD trust chain, not from a CA), we use
        // serverCertificateHashes so Chromium pins to that specific self-signed
        // cert without needing any external certificate authority.
        const wtOpts = {};
        if (opts.certHash) {
            const bytes = Uint8Array.from(
                opts.certHash.match(/../g).map(h => parseInt(h, 16))
            );
            wtOpts.serverCertificateHashes = [
                { algorithm: "sha-256", value: bytes.buffer }
            ];
        }
        // eslint-disable-next-line no-undef
        const transport = new WebTransport(url, wtOpts);
        await transport.ready;

        await runIdentityHandshake(transport, keys, ed25519);

        return new RawConquerdClient({
            transport,
            keys,
            peerId: keys.peerId,
            caps: opts.caps,
            ed25519,
        });
    }

    /**
     * Send a signed envelope on the channel for *featureId*. Throws if
     * the feature wasn't declared at connect time. (Used for room.* features.)
     */
    async send(featureId, envelopeInput) {
        const tag = this.tagFor.get(featureId);
        if (tag === undefined) {
            throw new Error(
                `feature '${featureId}' not advertised at connect time`,
            );
        }
        const json = await buildEnvelope(envelopeInput, this.keys, this.ed25519);
        const body = new TextEncoder().encode(json);
        const framed = new Uint8Array(1 + body.length);
        framed[0] = tag;
        framed.set(body, 1);
        const writer = this.transport.datagrams.writable.getWriter();
        try {
            await writer.write(framed);
        } finally {
            writer.releaseLock();
        }
    }

    /**
     * Send a raw (opaque) datagram for game.* relay features.
     * The payload is sent verbatim; the supernode performs no interpretation.
     * Use this for game.relay.v1 and similar low-latency opaque relays.
     */
    async sendRawDatagram(featureId, payload) {
        const tag = this.tagFor.get(featureId);
        if (tag === undefined) {
            throw new Error(
                `feature '${featureId}' not advertised at connect time`,
            );
        }
        const bytes = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
        const framed = new Uint8Array(1 + bytes.length);
        framed[0] = tag;
        framed.set(bytes, 1);
        const writer = this.transport.datagrams.writable.getWriter();
        try {
            await writer.write(framed);
        } finally {
            writer.releaseLock();
        }
    }

    /** Resolve when the underlying WebTransport closes. */
    closed() {
        return this.transport.closed;
    }

    close(reason) {
        try {
            this.transport.close({ closeCode: 0, reason: reason ?? "" });
        } catch {
            // best-effort
        }
    }

    async _readDatagrams() {
        const reader = this.transport.datagrams.readable.getReader();
        try {
            for (;;) {
                const { value, done } = await reader.read();
                if (done) return;
                if (!value || value.length === 0) continue;
                const tag = value[0];
                const body = value.subarray(1);
                const featureId = this.featureForTag.get(tag) || null;
                if (this.onDatagram) this.onDatagram(tag, body, featureId);
            }
        } catch {
            // transport closed; surface via `closed()`.
        }
    }
}

/**
 * Accept the supernode-initiated bidi stream, sign the 32-byte
 * challenge, and send back the 64-byte signature. The Rust side opens
 * the stream, so the client only needs to accept it.
 */
async function runIdentityHandshake(transport, keys, ed25519) {
    const reader = transport.incomingBidirectionalStreams.getReader();
    const { value: stream, done } = await reader.read();
    reader.releaseLock();
    if (done || !stream) {
        throw new Error("supernode did not open identity stream");
    }
    const r = stream.readable.getReader();
    const challenge = await readExact(r, 32);
    r.releaseLock();
    const sig = await ed25519.sign(challenge, keys.secretKey);
    if (sig.length !== 64) {
        throw new Error(`unexpected signature length ${sig.length}`);
    }
    const w = stream.writable.getWriter();
    try {
        await w.write(sig);
        await w.close();
    } finally {
        try { w.releaseLock(); } catch { /* already released by close */ }
    }
}

async function readExact(reader, n) {
    const out = new Uint8Array(n);
    let off = 0;
    while (off < n) {
        const { value, done } = await reader.read();
        if (done) throw new Error(`stream ended after ${off}/${n} bytes`);
        const remaining = n - off;
        if (value.length > remaining) {
            // The handshake protocol writes exactly 32 bytes; trim defensively.
            out.set(value.subarray(0, remaining), off);
            off = n;
        } else {
            out.set(value, off);
            off += value.length;
        }
    }
    return out;
}

// Exported for tests.
export const _internal = {
    b64urlEncode,
    b64urlDecode,
    canonicalBytes,
    PROTOCOL_VERSION,
};

// ─────────────────────────────────────────────────────────────────────────────
// High-level convenience wrapper used by the official game demos
// (cursor, shared-drawing, brick-breaker, etc.).
//
// This provides the exact surface the demos expect:
//   new ConquerdClient({ host, port, features, room })
//   .on("connected", peerId => ...)
//   .on("disconnected", () => ...)
//   .on("error", e => ...)
//   .on("datagram", (featureId, data) => ...)   // data = Uint8Array (raw payload)
//   await .connect()
//   .sendDatagram(featureId, bytes)
//   .disconnect()
//
// For game.* features the datagrams are opaque (no signing). For room.*
// the high-level wrapper currently focuses on the raw game relay path;
// use the lower-level RawConquerdClient + buildEnvelope if you need
// signed room.* envelopes from the browser.
// ─────────────────────────────────────────────────────────────────────────────

export class ConquerdClient {
    constructor({ host, port, features, room }) {
        if (!host) throw new Error("host is required");
        this.host = host;
        this.port = port || 8443;
        this.features = Array.isArray(features) ? features : [];
        this.room = room || null;

        this._raw = null;           // RawConquerdClient instance
        this._peerId = null;
        this._handlers = Object.create(null); // event -> Set<fn>
        this._connected = false;
    }

    on(event, fn) {
        if (!this._handlers[event]) this._handlers[event] = new Set();
        this._handlers[event].add(fn);
        return this;
    }

    off(event, fn) {
        if (this._handlers[event]) this._handlers[event].delete(fn);
    }

    _emit(event, ...args) {
        const set = this._handlers[event];
        if (set) for (const fn of set) {
            try { fn(...args); } catch (e) { console.error("[ConquerdClient] handler error", e); }
        }
    }

    get peerId() { return this._peerId; }

    async connect() {
        let url = `https://${this.host}:${this.port}`;
        let certHash = null;
        // When loaded inside the native client portal (conquerd:// scheme),
        // window.conquerd.ready contains the actual WebTransport base URL
        // sent by the supernode in SUPERNODE_INFO.  location.hostname in that
        // context is the supernode's base64url peer ID — not a real hostname.
        // Two-step resolution:
        //   1. ctx.wtBaseUrl  — set when supernode advertises wt_url in
        //                       SUPERNODE_INFO (requires external_host config)
        //   2. /api/wt-url    — queried directly from the supernode; works
        //                       for local dev (falls back to localhost) without
        //                       needing external_host to be configured.
        let inPortal = false;
        const fallbackUrl = url;
        try {
            if (typeof window !== "undefined" && window?.conquerd?.ready) {
                const ctx = await window.conquerd.ready;
                inPortal = true;
                if (ctx?.wtBaseUrl) {
                    url = ctx.wtBaseUrl;
                } else if (ctx?.fetch) {
                    const wtCfg = await ctx.fetch("/api/wt-url")
                        .then(r => r.ok ? r.json() : null)
                        .catch(() => null);
                    if (wtCfg?.url) url = wtCfg.url;
                    // Fingerprint is also included in the /api/wt-url response
                    // so the fallback path can also use serverCertificateHashes.
                    if (wtCfg?.certHash) certHash = wtCfg.certHash;
                }
                // Cert fingerprint is delivered via the ConquerD trust chain
                // (SUPERNODE_INFO → ctx.json → bridge script).  No CA needed.
                if (ctx?.wtCertHash) certHash = ctx.wtCertHash;
            }
        } catch { /* not in portal context — fall back to host:port */ }
        // When inside the native portal but neither SUPERNODE_INFO nor
        // /api/wt-url provided a real WebTransport URL, the fallback URL
        // contains the base64url peer-id as hostname — unreachable — so
        // surface a clear error immediately instead of a 10-second timeout.
        if (inPortal && url === fallbackUrl) {
            throw new Error(
                "WebTransport unavailable: supernode does not advertise " +
                "web.host.h3.v1 — enable it in supernode.toml to use game relay features"
            );
        }
        // 10-second hard timeout so the status never hangs at "Connecting…"
        // forever when the WebTransport endpoint is unreachable.
        const timeoutMs = 10_000;
        const abort = new Promise((_, rej) =>
            setTimeout(() => rej(new Error(`WebTransport connection timed out after ${timeoutMs / 1000} s (url: ${url})`)), timeoutMs)
        );
        try {
            this._raw = await Promise.race([
                RawConquerdClient.connect({
                    url,
                    caps: this.features,
                    room: this.room,
                    certHash,
                }),
                abort,
            ]);
            this._peerId = this._raw.peerId;

            // Wire raw datagrams → high-level "datagram" events with featureId
            this._raw.onDatagram = (tag, body, featureId) => {
                this._emit("datagram", featureId || "unknown", body);
            };

            this._connected = true;
            this._emit("connected", this._peerId);

            // Watch for underlying close
            this._raw.closed().then(() => {
                if (this._connected) {
                    this._connected = false;
                    this._emit("disconnected");
                }
            }).catch(() => {});
        } catch (e) {
            this._emit("error", e);
            throw e;
        }
    }

    sendDatagram(featureId, payload) {
        if (!this._raw) throw new Error("not connected");
        // Always use the raw (opaque) path — correct for game.relay.v1 and
        // any other game.* relay feature. The supernode fans the bytes as-is.
        return this._raw.sendRawDatagram(featureId, payload);
    }

    disconnect() {
        if (this._raw) {
            try { this._raw.close("client disconnect"); } catch {}
        }
        if (this._connected) {
            this._connected = false;
            this._emit("disconnected");
        }
    }
}
