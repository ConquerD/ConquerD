/**
 * Cursor Relay — example game using `game.relay.v1` over WebTransport.
 *
 * Wire format (opaque to the supernode relay):
 *   [1 byte type][2 bytes x f16][2 bytes y f16][1-16 bytes color utf-8]
 *
 *   type 0x01 = CURSOR_UPDATE  { x: 0..1, y: 0..1, color }
 *   type 0x02 = CURSOR_LEAVE   { color utf-8 }
 *
 * The color string is used as the stable peer key so cursors are tracked
 * correctly across update and leave messages.
 *
 * The supernode relays these verbatim to every other peer in the same
 * game session; it never reads or modifies the payload.
 *
 * Setup: host the supernode with `game.relay.v1` enabled in supernode.toml,
 * then open this page at:
 *   https://<host>:<web_port>/games/example/?room=<lobby>
 */

import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

// ── Config ───────────────────────────────────────────────────────────────────

const FEATURE_ID = "game.relay.v1";

/** Resolve the supernode WebTransport host from query params or origin. */
function resolveHost() {
  const p = new URLSearchParams(location.search);
  return p.get("host") || location.hostname;
}

function resolvePort() {
  const p = new URLSearchParams(location.search);
  return parseInt(p.get("port") || location.port || "8443", 10);
}

function resolveRoom() {
  const p = new URLSearchParams(location.search);
  return p.get("room") || "default";
}

// ── Wire helpers ─────────────────────────────────────────────────────────────

/** Encode a float in [-∞, +∞] to a 16-bit half-precision big-endian. */
function f32ToF16(v) {
  // Clamp to 0..1 range then map to uint16 for simplicity.
  const n = Math.max(0, Math.min(1, v));
  return Math.round(n * 65535);
}

function f16ToF32(n) {
  return (n & 0xffff) / 65535;
}

const TYPE_CURSOR_UPDATE = 0x01;
const TYPE_CURSOR_LEAVE  = 0x02;

function encodeCursorUpdate(xNorm, yNorm, colorHex) {
  const colorBytes = new TextEncoder().encode(colorHex.slice(0, 16));
  const buf = new Uint8Array(1 + 2 + 2 + colorBytes.length);
  let i = 0;
  buf[i++] = TYPE_CURSOR_UPDATE;
  const xU = f32ToF16(xNorm);
  const yU = f32ToF16(yNorm);
  buf[i++] = (xU >> 8) & 0xff;
  buf[i++] = xU & 0xff;
  buf[i++] = (yU >> 8) & 0xff;
  buf[i++] = yU & 0xff;
  buf.set(colorBytes, i);
  return buf;
}

function encodeCursorLeave(colorHex) {
  const colorBytes = new TextEncoder().encode(colorHex.slice(0, 16));
  const buf = new Uint8Array(1 + colorBytes.length);
  buf[0] = TYPE_CURSOR_LEAVE;
  buf.set(colorBytes, 1);
  return buf;
}

function decodeDatagram(data) {
  if (!(data instanceof Uint8Array)) data = new Uint8Array(data);
  if (data.length < 1) return null;
  const type = data[0];
  if (type === TYPE_CURSOR_LEAVE) {
    const color = data.length > 1 ? new TextDecoder().decode(data.slice(1)) : null;
    return { type: "leave", color };
  }
  if (type === TYPE_CURSOR_UPDATE && data.length >= 5) {
    const x = f16ToF32((data[1] << 8) | data[2]);
    const y = f16ToF32((data[3] << 8) | data[4]);
    const color = new TextDecoder().decode(data.slice(5)) || "#888888";
    return { type: "update", x, y, color };
  }
  return null;
}

// ── Canvas renderer ──────────────────────────────────────────────────────────

const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d");

/** Map of peerId → { x, y, color, lastSeen } */
const peers = new Map();

function resize() {
  canvas.width  = window.innerWidth;
  canvas.height = window.innerHeight;
}
window.addEventListener("resize", resize);
resize();

function drawFrame() {
  const W = canvas.width, H = canvas.height;
  ctx.clearRect(0, 0, W, H);

  // Grid
  ctx.strokeStyle = "rgba(48,54,61,0.4)";
  ctx.lineWidth = 1;
  const step = 60;
  for (let x = 0; x < W; x += step) { ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, H); ctx.stroke(); }
  for (let y = 0; y < H; y += step) { ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(W, y); ctx.stroke(); }

  const now = Date.now();
  for (const [pid, p] of peers) {
    const age = now - p.lastSeen;
    if (age > 10000) { peers.delete(pid); continue; }
    const alpha = Math.max(0, 1 - age / 10000);
    const px = p.x * W, py = p.y * H;

    ctx.globalAlpha = alpha;
    // Outer glow
    ctx.beginPath();
    ctx.arc(px, py, 14, 0, Math.PI * 2);
    ctx.fillStyle = p.color + "33";
    ctx.fill();
    // Dot
    ctx.beginPath();
    ctx.arc(px, py, 7, 0, Math.PI * 2);
    ctx.fillStyle = p.color;
    ctx.fill();
    // Label
    ctx.globalAlpha = alpha * 0.85;
    ctx.fillStyle = "#e6edf3";
    ctx.font = "10px ui-monospace";
    ctx.fillText(pid.slice(0, 8), px + 10, py - 6);
    ctx.globalAlpha = 1;
  }

  requestAnimationFrame(drawFrame);
}
requestAnimationFrame(drawFrame);

// ── UI helpers ───────────────────────────────────────────────────────────────

const statusEl     = document.getElementById("status");
const peersCountEl = document.getElementById("peers-count");
const roomLabelEl  = document.getElementById("room-label");

function setStatus(text, cls) {
  statusEl.textContent = text;
  statusEl.className = cls || "";
}

function updatePeersUI() {
  const n = peers.size;
  peersCountEl.textContent = n ? `${n} peer${n > 1 ? "s" : ""} online` : "";
}

// ── Pick a stable random color for this tab ──────────────────────────────────

const PALETTE = [
  "#58a6ff", "#3fb950", "#f0883e", "#d2a8ff",
  "#79c0ff", "#56d364", "#ffa657", "#ff7b72",
];
const myColor = PALETTE[Math.floor(Math.random() * PALETTE.length)];

// ── Main ─────────────────────────────────────────────────────────────────────

async function main() {
  const host   = resolveHost();
  const port   = resolvePort();
  const roomId = resolveRoom();

  roomLabelEl.textContent = `room: ${roomId}`;

  let client;
  try {
    client = new ConquerdClient({
      host,
      port,
      features: [FEATURE_ID],
      room: roomId,
    });
  } catch (e) {
    setStatus(`Init error: ${e.message}`, "error");
    return;
  }

  client.on("connected", (peerId) => {
    setStatus(`Connected  (${peerId.slice(0, 8)}…)`, "connected");
  });

  client.on("disconnected", () => {
    setStatus("Disconnected", "error");
    peers.clear();
    updatePeersUI();
  });

  client.on("error", (e) => {
    setStatus(`Error: ${e}`, "error");
  });

  // Inbound datagrams
  client.on("datagram", (featureId, data) => {
    if (featureId !== FEATURE_ID) return;
    const msg = decodeDatagram(data);
    if (!msg) return;
    // Use the cursor color as a stable per-peer key; it is included in
    // both CURSOR_UPDATE and CURSOR_LEAVE messages so the peers Map stays
    // consistent.  Fall back to a synthetic id for malformed datagrams.
    const senderId = msg.color || ("peer-" + Array.from(new Uint8Array(data).slice(0, 2))
      .map(b => b.toString(16).padStart(2, "0")).join(""));
    if (msg.type === "update") {
      peers.set(senderId, { x: msg.x, y: msg.y, color: msg.color, lastSeen: Date.now() });
    } else if (msg.type === "leave") {
      peers.delete(senderId);
    }
    updatePeersUI();
  });

  await client.connect();

  // Track local cursor and send updates
  let lastSent = 0;
  const THROTTLE_MS = 50; // ~20 fps

  canvas.addEventListener("mousemove", (e) => {
    const now = Date.now();
    if (now - lastSent < THROTTLE_MS) return;
    lastSent = now;
    const x = e.clientX / canvas.width;
    const y = e.clientY / canvas.height;
    client.sendDatagram(FEATURE_ID, encodeCursorUpdate(x, y, myColor));
  });

  window.addEventListener("beforeunload", () => {
    client.sendDatagram(FEATURE_ID, encodeCursorLeave(myColor));
    client.disconnect();
  });
}

main().catch((e) => {
  console.error("[cursor-relay]", e);
  document.getElementById("status").textContent = `Fatal: ${e.message}`;
  document.getElementById("status").className = "error";
});
