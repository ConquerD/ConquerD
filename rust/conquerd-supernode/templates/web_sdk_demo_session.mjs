import { ConquerdClient } from "./conquerd.mjs";

export const FEATURE = "game.relay.v1";
export const PEER_TIMEOUT = 6500;
const encoder = new TextEncoder();
const decoder = new TextDecoder();
const MAX_FRAME = 1100;

// App-level session identifiers are ephemeral, not authenticated peer identities.
// The native bridge owns transport auth, capability checks and encryption.
export function sessionId() {
  return Array.from(crypto.getRandomValues(new Uint8Array(12)), (n) =>
    n.toString(16).padStart(2, "0"),
  ).join("");
}

export function decodePacket(bytes, app) {
  if (!bytes || bytes.length > MAX_FRAME) return null;
  try {
    const p = JSON.parse(decoder.decode(bytes));
    if (
      p.v !== 1 ||
      p.app !== app ||
      !/^[a-f0-9]{24}$/.test(p.id) ||
      !Number.isSafeInteger(p.seq) ||
      p.seq < 0 ||
      !["hello", "data", "ping", "pong", "leave"].includes(p.kind)
    )
      return null;
    return p;
  } catch {
    return null;
  }
}

export class DemoSession {
  constructor(
    app,
    {
      room = new URLSearchParams(location.search).get("room") || "lounge",
      client,
      id = sessionId(),
      now = () => performance.now(),
    } = {},
  ) {
    this.app = app;
    this.room = room.slice(0, 48);
    this.id = id;
    this.now = now;
    // A drawing and a game with the same human room name cannot mix packets.
    this.client =
      client ||
      new ConquerdClient({
        features: [FEATURE],
        room: `demo-v1:${app}:${this.room}`,
      });
    this.peers = new Map();
    this.handlers = new Map();
    this.connected = false;
    this.ready = false;
    this.seq = 0;
    this.pending = 0;
    this.stats = {
      sent: 0,
      received: 0,
      bytesOut: 0,
      bytesIn: 0,
      errors: 0,
      dropped: 0,
    };
    this.pings = new Map();
    this.lastPing = 0;
    this.client.on("datagram", (feature, bytes) => {
      if (feature === FEATURE) this.receive(bytes);
    });
    this.client.on("disconnected", () => this.setDisconnected());
    this.client.on("error", (e) => this.emit("error", e));
  }

  on(event, fn) {
    if (!this.handlers.has(event)) this.handlers.set(event, new Set());
    this.handlers.get(event).add(fn);
    return this;
  }
  emit(event, ...args) {
    for (const fn of this.handlers.get(event) || []) fn(...args);
  }
  get leader() {
    return [this.id, ...this.peers.keys()].sort()[0];
  }
  get isLeader() {
    return this.connected && this.leader === this.id;
  }

  async connect() {
    await this.client.connect();
    this.connected = true;
    this.started = this.now();
    this.emit("connected");
    this.heartbeat();
    this.timer = setInterval(() => this.heartbeat(), 1000);
  }

  async transmit(kind, body) {
    if (!this.connected) return false;
    // Bound bridge calls when a slow transport cannot keep up. Snapshots and
    // presence repeat; app controls must be repeated or acknowledged by apps.
    if (this.pending >= 4) {
      this.stats.dropped++;
      return false;
    }
    const bytes = encoder.encode(
      JSON.stringify({
        v: 1,
        app: this.app,
        id: this.id,
        seq: ++this.seq,
        kind,
        body,
      }),
    );
    if (bytes.length > MAX_FRAME) {
      this.stats.errors++;
      return false;
    }
    this.pending++;
    try {
      await this.client.sendDatagram(FEATURE, bytes);
      this.stats.sent++;
      this.stats.bytesOut += bytes.length;
      return true;
    } catch (e) {
      this.stats.errors++;
      this.emit("error", e);
      return false;
    } finally {
      this.pending--;
    }
  }
  send(body) {
    return this.transmit("data", body);
  }
  setReady(ready) {
    this.ready = ready;
    this.transmit("hello", { ready });
    this.emit("members");
  }

  receive(bytes) {
    if (!this.connected) return;
    const p = decodePacket(bytes, this.app);
    if (!p || p.id === this.id) return;
    let peer = this.peers.get(p.id);
    if (peer && p.seq <= peer.seq) return;
    if (!peer) {
      if (this.peers.size >= 32) return;
      peer = { id: p.id, ready: false, seq: -1, rtt: null };
      this.peers.set(p.id, peer);
      this.transmit("hello", { ready: this.ready });
      this.emit("join", p.id);
    }
    peer.seq = p.seq;
    peer.seen = this.now();
    this.stats.received++;
    this.stats.bytesIn += bytes.length;
    if (p.kind === "leave") this.peers.delete(p.id);
    if (p.kind === "hello") peer.ready = p.body?.ready === true;
    if (p.kind === "data") this.emit("data", p.body, p.id);
    if (
      p.kind === "ping" &&
      p.body?.to === this.id &&
      typeof p.body.token === "string"
    ) {
      this.transmit("pong", { to: p.id, token: p.body.token.slice(0, 32) });
    }
    if (p.kind === "pong" && p.body?.to === this.id) {
      const ping = this.pings.get(p.body.token);
      if (ping && ping.id === p.id) {
        peer.rtt = Math.round(this.now() - ping.time);
        this.pings.delete(p.body.token);
      }
    }
    if (p.kind !== "data") this.emit("members");
  }

  heartbeat() {
    const now = this.now();
    for (const [id, peer] of this.peers) {
      if (now - peer.seen > PEER_TIMEOUT) {
        this.peers.delete(id);
        this.emit("leave", id);
      }
    }
    for (const [token, ping] of this.pings)
      if (now - ping.time > PEER_TIMEOUT) this.pings.delete(token);
    this.transmit("hello", { ready: this.ready });
    if (now - this.lastPing >= 2000) {
      this.lastPing = now;
      const ids = [...this.peers.keys()];
      if (ids.length) {
        this.pingIndex = (this.pingIndex || 0) % ids.length;
        const id = ids[this.pingIndex++];
        const token = `${this.seq}-${id.slice(0, 6)}`;
        this.pings.set(token, { id, time: now });
        this.transmit("ping", { to: id, token });
      }
    }
    this.emit("members");
  }
  setDisconnected() {
    this.connected = false;
    clearInterval(this.timer);
    this.peers.clear();
    this.pings.clear();
    this.emit("disconnected");
  }
  disconnect() {
    // Best effort leave; timeouts remove sessions even if navigation cancels it.
    this.transmit("leave", null);
    this.setDisconnected();
    this.client.disconnect();
  }
}
