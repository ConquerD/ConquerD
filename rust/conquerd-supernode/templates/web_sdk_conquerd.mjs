// ConquerD in-app portal SDK — identity-path game channel.
//
// Games load inside the native client (`conquerd://` + window.conquerd).
// Datagrams ride the authenticated QUIC relay via portal channel APIs
// (`/_conquerd/channel/*`). There is no WebTransport / self-signed TLS path.
//
// Wire (relay):
//   [BROADCAST_INDEX=0xFF][GAME_RELAY_TAG=0x05][opaque payload]
//
// Portal API (scheme bridge):
//   openChannel(room) / sendDatagramB64(b64) / pollDatagrams() / closeChannel()

/** Reverse-DNS feature id (e.g. `"game.relay.v1"`). */
/** @typedef {string} FeatureId */

/**
 * Fixed first-party channel tags (reserved range `0x00..=0x0F`), mirroring
 * `conquerd_features::channel_frame`.
 *
 * @readonly
 * @enum {number}
 */
export const ChannelTag = Object.freeze({
    CONTROL: 0x00,
    AUDIO: 0x01,
    CHAT: 0x02,
    FILE: 0x03,
    ROOM_AUDIO: 0x04,
    /** Game relay (`game.relay.v1`) on the identity QUIC relay. */
    GAME_RELAY: 0x05,
});

/** Fixed channel tag for a first-party feature id, or `undefined`. */
export function fixedTagFor(featureId) {
    switch (featureId) {
        case "core.audio.opus": return ChannelTag.AUDIO;
        case "core.chat.v1": return ChannelTag.CHAT;
        case "core.file.v1": return ChannelTag.FILE;
        case "room.audio.sfu": return ChannelTag.ROOM_AUDIO;
        case "game.relay.v1": return ChannelTag.GAME_RELAY;
        default: return undefined;
    }
}

/** First-party feature id bound to a fixed channel tag, or `null`. */
export function featureForFixedTag(tag) {
    switch (tag) {
        case ChannelTag.AUDIO: return "core.audio.opus";
        case ChannelTag.CHAT: return "core.chat.v1";
        case ChannelTag.FILE: return "core.file.v1";
        case ChannelTag.ROOM_AUDIO: return "room.audio.sfu";
        case ChannelTag.GAME_RELAY: return "game.relay.v1";
        default: return null;
    }
}

/** Frame a payload as `[tag][payload]`. */
export function encodeFrame(tag, payload) {
    const body = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
    const framed = new Uint8Array(1 + body.length);
    framed[0] = tag;
    framed.set(body, 1);
    return framed;
}

/** Split `[tag][payload]`, or `null` for an empty frame. */
export function decodeFrame(frame) {
    if (!frame || frame.length === 0) return null;
    return { tag: frame[0], payload: frame.subarray(1) };
}

/** base64url-encode without padding. */
function b64urlEncodeNoPad(bytes) {
    let bin = "";
    for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    const b64 = btoa(bin);
    return b64.replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** base64url-decode (with or without padding). */
function b64urlDecodeNoPad(s) {
    const pad = s.length % 4 === 0 ? "" : "=".repeat(4 - (s.length % 4));
    const b64 = (s + pad).replace(/-/g, "+").replace(/_/g, "/");
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
}

/**
 * Identity-path portal transport: game.relay over the native client's
 * already-authenticated QUIC relay session.
 */
class PortalNativeTransport {
    constructor(api, room) {
        this.api = api;
        this.room = room || "default";
        this.peerId = api.myPeerId || "";
        this._pollTimer = null;
        this.onDatagram = null; // (featureId, Uint8Array)
        this._closed = false;
    }

    async connect() {
        const res = await this.api.openChannel(this.room);
        if (res && res.ok === false) {
            throw new Error(res.error || "portal channel open failed");
        }
        this._closed = false;
        this._pollTimer = setInterval(() => { this._poll(); }, 33);
    }

    async _poll() {
        if (this._closed || !this.api.pollDatagrams) return;
        try {
            const res = await this.api.pollDatagrams();
            const frames = res?.frames || [];
            for (const b64 of frames) {
                try {
                    const body = b64urlDecodeNoPad(b64);
                    if (this.onDatagram) this.onDatagram("game.relay.v1", body);
                } catch { /* skip bad frame */ }
            }
        } catch { /* transient poll errors */ }
    }

    async sendRawDatagram(_featureId, payload) {
        if (this._closed) throw new Error("not connected");
        const bytes = payload instanceof Uint8Array ? payload : new Uint8Array(payload);
        const b64 = b64urlEncodeNoPad(bytes);
        const res = await this.api.sendDatagramB64(b64);
        if (res && res.ok === false) {
            throw new Error(res.error || "portal send failed");
        }
    }

    close() {
        this._closed = true;
        if (this._pollTimer) {
            clearInterval(this._pollTimer);
            this._pollTimer = null;
        }
        try { this.api.closeChannel?.(); } catch { /* ignore */ }
    }
}

/**
 * High-level client for in-app portal games.
 *
 *   new ConquerdClient({ features, room })  // host/port ignored (portal-only)
 *   .on("connected", peerId => ...)
 *   .on("datagram", (featureId, data) => ...)
 *   await .connect()
 *   .sendDatagram(featureId, bytes)
 *   .disconnect()
 */
export class ConquerdClient {
    constructor({ host, port, features, room } = {}) {
        // host/port retained for call-site compatibility with older demos;
        // transport is always the native portal bridge.
        this.host = host || "";
        this.port = port || 0;
        this.features = Array.isArray(features) ? features : [];
        this.room = room || null;

        this._transport = null;
        this._peerId = null;
        this._handlers = Object.create(null);
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
        if (typeof window === "undefined" || !window?.conquerd?.ready) {
            const err = new Error(
                "ConquerD games require the native in-app portal " +
                "(conquerd:// + window.conquerd). External browsers are not supported."
            );
            this._emit("error", err);
            throw err;
        }
        try {
            const ctx = await window.conquerd.ready;
            if (typeof ctx.openChannel !== "function") {
                throw new Error(
                    "window.conquerd is missing portal channel APIs — rebuild the native client"
                );
            }
            const transport = new PortalNativeTransport(ctx, this.room);
            await transport.connect();
            this._transport = transport;
            this._peerId = transport.peerId;
            transport.onDatagram = (featureId, body) => {
                this._emit("datagram", featureId || "unknown", body);
            };
            this._connected = true;
            this._emit("connected", this._peerId);
        } catch (e) {
            this._emit("error", e);
            throw e;
        }
    }

    sendDatagram(featureId, payload) {
        if (!this._transport) throw new Error("not connected");
        return this._transport.sendRawDatagram(featureId, payload);
    }

    disconnect() {
        if (this._transport) {
            try { this._transport.close(); } catch { /* ignore */ }
            this._transport = null;
        }
        if (this._connected) {
            this._connected = false;
            this._emit("disconnected");
        }
    }
}
