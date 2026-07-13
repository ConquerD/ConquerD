/**
 * Shared Drawing — collaborative canvas using game.relay.v1
 *
 * Demonstrates real-time opaque datagram relay for a simple drawing game.
 *
 * Wire format (opaque to supernode):
 *   [1 byte type] [payload...]
 *
 *   0x01 = STROKE   { x1:f16, y1:f16, x2:f16, y2:f16, width:u8, color:utf8 }
 *   0x02 = CLEAR
 *   0x03 = ERASE    { x1:f16, y1:f16, x2:f16, y2:f16, width:u8 }
 *
 * Usage:
 *   Open via a ConquerD-enabled supernode:
 *   https://<host>/games/shared-drawing/?room=my-lobby
 */

import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

const FEATURE_ID = "game.relay.v1";

const PALETTE = [
  "#58a6ff", "#3fb950", "#f0883e", "#d2a8ff",
  "#79c0ff", "#56d364", "#ffa657", "#ff7b72",
  "#e6edf3", "#ffffff", "#f778ba", "#bc8cff",
];

const STROKE_SIZES = [2, 4, 8, 16];
const DEFAULT_STROKE_WIDTH = 4;

// ── Helpers ──────────────────────────────────────────────────────────────────

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
const TYPE_ERASE  = 0x03;

function encodeStroke(x1, y1, x2, y2, color, width) {
  const colorBytes = new TextEncoder().encode(color.slice(0, 16));
  const buf = new Uint8Array(1 + 8 + 1 + colorBytes.length);
  let i = 0;
  buf[i++] = TYPE_STROKE;
  const x1u = f32ToF16(x1), y1u = f32ToF16(y1);
  const x2u = f32ToF16(x2), y2u = f32ToF16(y2);
  buf[i++] = (x1u >> 8) & 0xff; buf[i++] = x1u & 0xff;
  buf[i++] = (y1u >> 8) & 0xff; buf[i++] = y1u & 0xff;
  buf[i++] = (x2u >> 8) & 0xff; buf[i++] = x2u & 0xff;
  buf[i++] = (y2u >> 8) & 0xff; buf[i++] = y2u & 0xff;
  buf[i++] = Math.max(1, Math.min(64, Math.round(width)));
  buf.set(colorBytes, i);
  return buf;
}

function encodeErase(x1, y1, x2, y2, width) {
  const buf = new Uint8Array(1 + 8 + 1);
  let i = 0;
  buf[i++] = TYPE_ERASE;
  const x1u = f32ToF16(x1), y1u = f32ToF16(y1);
  const x2u = f32ToF16(x2), y2u = f32ToF16(y2);
  buf[i++] = (x1u >> 8) & 0xff; buf[i++] = x1u & 0xff;
  buf[i++] = (y1u >> 8) & 0xff; buf[i++] = y1u & 0xff;
  buf[i++] = (x2u >> 8) & 0xff; buf[i++] = x2u & 0xff;
  buf[i++] = (y2u >> 8) & 0xff; buf[i++] = y2u & 0xff;
  buf[i++] = Math.max(1, Math.min(64, Math.round(width)));
  return buf;
}

function encodeClear() {
  return new Uint8Array([TYPE_CLEAR]);
}

function decodeCoordsAndWidth(data) {
  const x1 = f16ToF32((data[1] << 8) | data[2]);
  const y1 = f16ToF32((data[3] << 8) | data[4]);
  const x2 = f16ToF32((data[5] << 8) | data[6]);
  const y2 = f16ToF32((data[7] << 8) | data[8]);
  const width = data.length >= 10 ? data[9] : DEFAULT_STROKE_WIDTH;
  return { x1, y1, x2, y2, width };
}

function decodeDatagram(data) {
  if (!(data instanceof Uint8Array)) data = new Uint8Array(data);
  if (data.length < 1) return null;
  const type = data[0];
  if (type === TYPE_CLEAR) return { type: "clear" };
  if (type === TYPE_ERASE && data.length >= 9) {
    return { type: "erase", ...decodeCoordsAndWidth(data) };
  }
  if (type === TYPE_STROKE && data.length >= 9) {
    const { x1, y1, x2, y2, width } = decodeCoordsAndWidth(data);
    const colorStart = data.length >= 10 ? 10 : 9;
    const color = new TextDecoder().decode(data.slice(colorStart)) || "#ffffff";
    return { type: "stroke", x1, y1, x2, y2, color, width };
  }
  return null;
}

// ── Canvas setup ─────────────────────────────────────────────────────────────

const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d", { alpha: true });
const inkCanvas = document.createElement("canvas");
const inkCtx = inkCanvas.getContext("2d", { alpha: true });

let isDrawing = false;
let lastX = 0, lastY = 0;

function resize() {
  const w = window.innerWidth;
  const h = window.innerHeight;
  canvas.width = w;
  canvas.height = h;
  inkCanvas.width = w;
  inkCanvas.height = h;
}
window.addEventListener("resize", resize);
resize();

/** Persistent stroke segments in normalized 0..1 coordinates. */
const strokes = [];

/** Remote peer cursor hints keyed by color (stable per tab). */
const remoteCursors = new Map(); // color -> {x, y, time}

function pointerNorm(e) {
  const rect = canvas.getBoundingClientRect();
  return {
    x: (e.clientX - rect.left) / rect.width,
    y: (e.clientY - rect.top) / rect.height,
  };
}

function addStroke(x1, y1, x2, y2, color, width = DEFAULT_STROKE_WIDTH) {
  strokes.push({ x1, y1, x2, y2, color, width, eraser: false });
}

function addErase(x1, y1, x2, y2, width = DEFAULT_STROKE_WIDTH) {
  strokes.push({ x1, y1, x2, y2, width, eraser: true });
}

function drawInkSegment(targetCtx, s, w, h) {
  targetCtx.save();
  if (s.eraser) {
    targetCtx.globalCompositeOperation = "destination-out";
    targetCtx.strokeStyle = "rgba(0,0,0,1)";
  } else {
    targetCtx.globalCompositeOperation = "source-over";
    targetCtx.strokeStyle = s.color;
  }
  targetCtx.lineWidth = s.width ?? DEFAULT_STROKE_WIDTH;
  targetCtx.lineJoin = "round";
  targetCtx.lineCap = "round";
  targetCtx.beginPath();
  targetCtx.moveTo(s.x1 * w, s.y1 * h);
  targetCtx.lineTo(s.x2 * w, s.y2 * h);
  targetCtx.stroke();
  targetCtx.restore();
}

function rebuildInkLayer(w, h) {
  inkCtx.clearRect(0, 0, w, h);
  // Apply strokes in chronological order so erasers only affect ink drawn
  // before them, not lines added afterwards.
  for (const s of strokes) {
    drawInkSegment(inkCtx, s, w, h);
  }
}

function draw() {
  const w = canvas.width;
  const h = canvas.height;

  ctx.fillStyle = "#161b22";
  ctx.fillRect(0, 0, w, h);

  // Draw grid
  ctx.strokeStyle = "rgba(48, 54, 61, 0.3)";
  ctx.lineWidth = 1;
  const step = 40;
  for (let x = 0; x < w; x += step) {
    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, h); ctx.stroke();
  }
  for (let y = 0; y < h; y += step) {
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(w, y); ctx.stroke();
  }

  rebuildInkLayer(w, h);
  ctx.drawImage(inkCanvas, 0, 0);

  const now = Date.now();

  // Draw remote peer cursor hints
  for (const [key, c] of remoteCursors) {
    const age = now - c.time;
    if (age > 8000) { remoteCursors.delete(key); continue; }
    const alpha = Math.max(0.15, 1 - age / 8000);
    ctx.globalAlpha = alpha;
    ctx.strokeStyle = c.eraser ? "#8b949e" : key;
    ctx.lineWidth = 2;
    ctx.beginPath();
    const radius = Math.max(4, (c.width ?? DEFAULT_STROKE_WIDTH) * 0.75);
    ctx.arc(c.x * w, c.y * h, radius, 0, Math.PI * 2);
    ctx.stroke();
  }
  ctx.globalAlpha = 1;

  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);

// ── UI ───────────────────────────────────────────────────────────────────────

const statusEl = document.getElementById("status");
const roomLabelEl = document.getElementById("room-label");
const paletteEl = document.getElementById("palette");
const sizesEl = document.getElementById("sizes");
const eraserBtn = document.getElementById("eraser");
const clearBtn = document.getElementById("clear");

let myColor = PALETTE[0];
let myWidth = DEFAULT_STROKE_WIDTH;
let eraserActive = false;

function setEraserActive(active) {
  eraserActive = active;
  eraserBtn.classList.toggle("active", eraserActive);
  canvas.classList.toggle("eraser-cursor", eraserActive);
  paletteEl.classList.toggle("disabled", eraserActive);
  if (eraserActive) {
    paletteEl.querySelectorAll(".swatch").forEach((s) => s.classList.remove("active"));
  } else {
    paletteEl.querySelectorAll(".swatch").forEach((s) => {
      s.classList.toggle("active", s.title === myColor);
    });
  }
}

function selectColor(color, activeBtn) {
  setEraserActive(false);
  myColor = color;
  paletteEl.querySelectorAll(".swatch").forEach((s) => s.classList.remove("active"));
  activeBtn.classList.add("active");
}

function buildPalette() {
  for (const color of PALETTE) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "swatch" + (color === myColor ? " active" : "");
    btn.style.background = color;
    btn.title = color;
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      selectColor(color, btn);
    });
    paletteEl.appendChild(btn);
  }
}
buildPalette();

function buildSizePicker() {
  for (const size of STROKE_SIZES) {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "size-btn" + (size === myWidth ? " active" : "");
    btn.title = `${size}px stroke`;
    const dot = document.createElement("span");
    dot.className = "dot";
    dot.style.width = `${Math.max(4, size)}px`;
    dot.style.height = `${Math.max(4, size)}px`;
    btn.appendChild(dot);
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      myWidth = size;
      sizesEl.querySelectorAll(".size-btn").forEach((s) => s.classList.remove("active"));
      btn.classList.add("active");
    });
    sizesEl.appendChild(btn);
  }
}
buildSizePicker();

eraserBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  setEraserActive(!eraserActive);
});

function setStatus(text, cls = "") {
  statusEl.textContent = text;
  statusEl.className = cls;
}

roomLabelEl.textContent = `room: ${resolveRoom()}`;

clearBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  if (client) {
    client.sendDatagram(FEATURE_ID, encodeClear());
  }
  strokes.length = 0;
});

// ── Main logic ───────────────────────────────────────────────────────────────

let client = null;

async function main() {
  const roomId = resolveRoom();

  try {
    client = new ConquerdClient({
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
    remoteCursors.clear();
  });

  client.on("error", (e) => setStatus("Error: " + e, "error"));

  client.on("datagram", (featureId, data) => {
    if (featureId !== FEATURE_ID) return;
    const msg = decodeDatagram(data);
    if (!msg) return;

    if (msg.type === "stroke") {
      addStroke(msg.x1, msg.y1, msg.x2, msg.y2, msg.color, msg.width);
      remoteCursors.set(msg.color, {
        x: msg.x2,
        y: msg.y2,
        width: msg.width,
        eraser: false,
        time: Date.now(),
      });
    } else if (msg.type === "erase") {
      addErase(msg.x1, msg.y1, msg.x2, msg.y2, msg.width);
      remoteCursors.set("__eraser__", {
        x: msg.x2,
        y: msg.y2,
        width: msg.width,
        eraser: true,
        time: Date.now(),
      });
    } else if (msg.type === "clear") {
      strokes.length = 0;
    }
  });

  await client.connect();

  // Drawing input
  canvas.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    isDrawing = true;
    canvas.setPointerCapture(e.pointerId);
    const { x, y } = pointerNorm(e);
    lastX = x;
    lastY = y;
  });

  canvas.addEventListener("pointermove", (e) => {
    if (!isDrawing || !client) return;

    const { x, y } = pointerNorm(e);
    if (eraserActive) {
      addErase(lastX, lastY, x, y, myWidth);
      client.sendDatagram(FEATURE_ID, encodeErase(lastX, lastY, x, y, myWidth));
    } else {
      addStroke(lastX, lastY, x, y, myColor, myWidth);
      client.sendDatagram(FEATURE_ID, encodeStroke(lastX, lastY, x, y, myColor, myWidth));
    }
    lastX = x;
    lastY = y;
  });

  function endStroke(e) {
    if (isDrawing && e.pointerId !== undefined) {
      try { canvas.releasePointerCapture(e.pointerId); } catch (_) { /* ok */ }
    }
    isDrawing = false;
  }

  canvas.addEventListener("pointerup", endStroke);
  canvas.addEventListener("pointercancel", endStroke);

  // Send leave on unload
  window.addEventListener("beforeunload", () => {
    if (client) client.disconnect();
  });
}

main().catch(console.error);