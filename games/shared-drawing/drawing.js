/**
 * Shared Drawing — collaborative canvas using game.relay.v1
 *
 * Demonstrates real-time opaque datagram relay for a simple drawing game.
 *
 * Wire format (opaque to supernode):
 *   [1 byte type] [payload...]
 *
 *   0x01 = STROKE   { x1:f16, y1:f16, x2:f16, y2:f16, color:utf8 }
 *   0x02 = CLEAR
 *
 * Usage:
 *   Open via a ConquerD-enabled supernode:
 *   https://<host>/games/shared-drawing/?room=my-lobby
 */

import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

const FEATURE_ID = "game.relay.v1";

// ── Helpers ──────────────────────────────────────────────────────────────────

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
  return p.get("room") || "drawing-lobby";
}

function f32ToF16(v) {
  const n = Math.max(0, Math.min(1, v));
  return Math.round(n * 65535);
}

function f16ToF32(n) {
  return (n & 0xffff) / 65535;
}

const TYPE_STROKE = 0x01;
const TYPE_CLEAR  = 0x02;

function encodeStroke(x1, y1, x2, y2, color) {
  const colorBytes = new TextEncoder().encode(color.slice(0, 16));
  const buf = new Uint8Array(1 + 8 + colorBytes.length);
  let i = 0;
  buf[i++] = TYPE_STROKE;
  const x1u = f32ToF16(x1), y1u = f32ToF16(y1);
  const x2u = f32ToF16(x2), y2u = f32ToF16(y2);
  buf[i++] = (x1u >> 8) & 0xff; buf[i++] = x1u & 0xff;
  buf[i++] = (y1u >> 8) & 0xff; buf[i++] = y1u & 0xff;
  buf[i++] = (x2u >> 8) & 0xff; buf[i++] = x2u & 0xff;
  buf[i++] = (y2u >> 8) & 0xff; buf[i++] = y2u & 0xff;
  buf.set(colorBytes, i);
  return buf;
}

function encodeClear() {
  return new Uint8Array([TYPE_CLEAR]);
}

function decodeDatagram(data) {
  if (!(data instanceof Uint8Array)) data = new Uint8Array(data);
  if (data.length < 1) return null;
  const type = data[0];
  if (type === TYPE_CLEAR) return { type: "clear" };
  if (type === TYPE_STROKE && data.length >= 9) {
    const x1 = f16ToF32((data[1] << 8) | data[2]);
    const y1 = f16ToF32((data[3] << 8) | data[4]);
    const x2 = f16ToF32((data[5] << 8) | data[6]);
    const y2 = f16ToF32((data[7] << 8) | data[8]);
    const color = new TextDecoder().decode(data.slice(9)) || "#ffffff";
    return { type: "stroke", x1, y1, x2, y2, color };
  }
  return null;
}

// ── Canvas setup ─────────────────────────────────────────────────────────────

const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d", { alpha: true });

let isDrawing = false;
let lastX = 0, lastY = 0;

function resize() {
  canvas.width = window.innerWidth;
  canvas.height = window.innerHeight;
}
window.addEventListener("resize", resize);
resize();

// Drawing state for remote peers (simple last stroke visualization)
const remoteStrokes = new Map(); // pid -> {x, y, color, time}

function draw() {
  ctx.fillStyle = "#161b22";
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  // Draw grid
  ctx.strokeStyle = "rgba(48, 54, 61, 0.3)";
  ctx.lineWidth = 1;
  const step = 40;
  for (let x = 0; x < canvas.width; x += step) {
    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, canvas.height); ctx.stroke();
  }
  for (let y = 0; y < canvas.height; y += step) {
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(canvas.width, y); ctx.stroke();
  }

  const now = Date.now();

  // Draw remote cursors / last strokes
  for (const [pid, s] of remoteStrokes) {
    const age = now - s.time;
    if (age > 8000) { remoteStrokes.delete(pid); continue; }
    const alpha = Math.max(0.15, 1 - age / 8000);
    ctx.globalAlpha = alpha;
    ctx.strokeStyle = s.color;
    ctx.lineWidth = 3;
    ctx.beginPath();
    ctx.arc(s.x * canvas.width, s.y * canvas.height, 5, 0, Math.PI * 2);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;

  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);

// ── UI ───────────────────────────────────────────────────────────────────────

const statusEl = document.getElementById("status");
const roomLabelEl = document.getElementById("room-label");
const colorInput = document.getElementById("color");
const clearBtn = document.getElementById("clear");

let myColor = colorInput.value;

colorInput.addEventListener("input", () => {
  myColor = colorInput.value;
});

function setStatus(text, cls = "") {
  statusEl.textContent = text;
  statusEl.className = cls;
}

roomLabelEl.textContent = `room: ${resolveRoom()}`;

clearBtn.addEventListener("click", () => {
  if (client) {
    client.sendDatagram(FEATURE_ID, encodeClear());
  }
  clearLocalCanvas();
});

function clearLocalCanvas() {
  ctx.fillStyle = "#161b22";
  ctx.fillRect(0, 0, canvas.width, canvas.height);
}

// ── Main logic ───────────────────────────────────────────────────────────────

let client = null;

async function main() {
  const host = resolveHost();
  const port = resolvePort();
  const roomId = resolveRoom();

  try {
    client = new ConquerdClient({
      host,
      port,
      features: [FEATURE_ID],
      room: roomId,
    });
  } catch (e) {
    setStatus("Init error: " + e.message, "error");
    return;
  }

  client.on("connected", (peerId) => {
    setStatus(`Connected as ${peerId.slice(0, 8)}…`, "connected");
  });

  client.on("disconnected", () => {
    setStatus("Disconnected", "error");
    remoteStrokes.clear();
  });

  client.on("error", (e) => setStatus("Error: " + e, "error"));

  client.on("datagram", (featureId, data) => {
    if (featureId !== FEATURE_ID) return;
    const msg = decodeDatagram(data);
    if (!msg) return;

    const senderId = "peer"; // In real SDK this would come with the event

    if (msg.type === "stroke") {
      // Draw the stroke locally
      ctx.strokeStyle = msg.color;
      ctx.lineWidth = 3;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      ctx.beginPath();
      ctx.moveTo(msg.x1 * canvas.width, msg.y1 * canvas.height);
      ctx.lineTo(msg.x2 * canvas.width, msg.y2 * canvas.height);
      ctx.stroke();

      // Update remote cursor position for visualization
      remoteStrokes.set(senderId, {
        x: msg.x2,
        y: msg.y2,
        color: msg.color,
        time: Date.now()
      });
    } else if (msg.type === "clear") {
      clearLocalCanvas();
    }
  });

  await client.connect();

  // Drawing input
  canvas.addEventListener("pointerdown", (e) => {
    isDrawing = true;
    lastX = e.offsetX / canvas.width;
    lastY = e.offsetY / canvas.height;
  });

  canvas.addEventListener("pointermove", (e) => {
    if (!isDrawing || !client) return;

    const x = e.offsetX / canvas.width;
    const y = e.offsetY / canvas.height;

    // Draw locally immediately
    ctx.strokeStyle = myColor;
    ctx.lineWidth = 3;
    ctx.lineJoin = "round";
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(lastX * canvas.width, lastY * canvas.height);
    ctx.lineTo(x * canvas.width, y * canvas.height);
    ctx.stroke();

    // Send to peers
    const packet = encodeStroke(lastX, lastY, x, y, myColor);
    client.sendDatagram(FEATURE_ID, packet);

    lastX = x;
    lastY = y;
  });

  window.addEventListener("pointerup", () => { isDrawing = false; });
  window.addEventListener("pointerleave", () => { isDrawing = false; });

  // Send leave on unload
  window.addEventListener("beforeunload", () => {
    if (client) client.disconnect();
  });
}

main().catch(console.error);