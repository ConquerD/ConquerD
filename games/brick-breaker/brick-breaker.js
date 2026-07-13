/**
 * Brick Breaker — classic paddle + ball + bricks over game.relay.v1
 *
 * Demonstrates real-time opaque datagram relay for a simple physics game.
 *
 * All peers in the same ?room=... see the same ball, bricks, score and lives.
 * The first peer to join (or the one that keeps the simulation alive) is the
 * "driver": it runs authoritative physics and broadcasts compact state packets.
 * Every peer contributes its own paddle; the ball bounces off any paddle.
 * Late joiners instantly see the current world state.
 *
 * Wire format (opaque to supernode — 100% client-defined):
 *
 *   0x01 PADDLE  [2 x:f16] [1 colorIdx]
 *   0x02 STATE   [2 ballX] [2 ballY] [2 velX] [2 velY] [2 paddleX]
 *                [1 lives] [2 score] [5 brickMask 40 bits for 8x5]
 *   0x03 RESET   (no body — driver resets the level)
 *
 * Usage:
 *   Open via a ConquerD supernode with game.relay.v1 enabled:
 *   https://<host>/games/brick-breaker/?room=my-lobby
 */

import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

const FEATURE_ID = "game.relay.v1";

// ── Config ───────────────────────────────────────────────────────────────────

const LOGICAL_W = 800;
const LOGICAL_H = 600;

const COLS = 8;
const ROWS = 5;
const BRICK_W = 78;
const BRICK_H = 22;
const BRICK_GAP = 6;
const BRICK_TOP = 70;
const BRICK_LEFT = 47;

const PADDLE_W = 92;
const PADDLE_H = 11;
const PADDLE_Y = LOGICAL_H - 42;
const PADDLE_SPEED = 9; // keyboard pixels per frame

const BALL_R = 7;

const PALETTE = [
  "#58a6ff", "#3fb950", "#f0883e", "#d2a8ff",
  "#79c0ff", "#56d364", "#ffa657", "#ff7b72",
];

// ── Helpers ──────────────────────────────────────────────────────────────────

function resolveRoom() {
  const p = new URLSearchParams(location.search);
  return p.get("room") || "brick-lobby";
}

function f32ToF16(v) {
  // Map -2..+2 range (generous for velocity) into 0..65535
  const n = Math.max(-2, Math.min(2, v));
  return Math.round(((n + 2) / 4) * 65535);
}

function f16ToF32(n) {
  return ((n & 0xffff) / 65535) * 4 - 2;
}

function clamp(v, lo, hi) { return Math.max(lo, Math.min(hi, v)); }

// ── Protocol ─────────────────────────────────────────────────────────────────

const TYPE_PADDLE = 0x01;
const TYPE_STATE  = 0x02;
const TYPE_RESET  = 0x03;

function encodePaddle(xNorm, colorIdx) {
  const x = f32ToF16(xNorm);
  const buf = new Uint8Array(1 + 2 + 1);
  buf[0] = TYPE_PADDLE;
  buf[1] = (x >> 8) & 0xff;
  buf[2] = x & 0xff;
  buf[3] = colorIdx & 0xff;
  return buf;
}

function encodeReset() {
  return new Uint8Array([TYPE_RESET]);
}

/** Pack 8x5=40 brick bits into 5 bytes (row-major) */
function packBrickMask(bricks) {
  const out = new Uint8Array(5);
  for (let i = 0; i < COLS * ROWS; i++) {
    if (bricks[i]) {
      const byte = Math.floor(i / 8);
      const bit = i % 8;
      out[byte] |= (1 << bit);
    }
  }
  return out;
}

function unpackBrickMask(bytes) {
  const bricks = new Array(COLS * ROWS).fill(false);
  for (let i = 0; i < COLS * ROWS; i++) {
    const byte = Math.floor(i / 8);
    const bit = i % 8;
    bricks[i] = (bytes[byte] & (1 << bit)) !== 0;
  }
  return bricks;
}

/**
 * Encode authoritative state.
 * vel is mapped through f16 that supports -2..2
 */
function encodeState(ballX, ballY, velX, velY, paddleX, lives, score, brickMaskBytes) {
  const bx = f32ToF16(ballX / LOGICAL_W);
  const by = f32ToF16(ballY / LOGICAL_H);
  const vx = f32ToF16(velX);
  const vy = f32ToF16(velY);
  const px = f32ToF16(paddleX / LOGICAL_W);

  const buf = new Uint8Array(1 + 2 + 2 + 2 + 2 + 2 + 1 + 2 + 5);
  let i = 0;
  buf[i++] = TYPE_STATE;
  buf[i++] = (bx >> 8) & 0xff; buf[i++] = bx & 0xff;
  buf[i++] = (by >> 8) & 0xff; buf[i++] = by & 0xff;
  buf[i++] = (vx >> 8) & 0xff; buf[i++] = vx & 0xff;
  buf[i++] = (vy >> 8) & 0xff; buf[i++] = vy & 0xff;
  buf[i++] = (px >> 8) & 0xff; buf[i++] = px & 0xff;
  buf[i++] = lives & 0xff;
  buf[i++] = (score >> 8) & 0xff; buf[i++] = score & 0xff;
  buf.set(brickMaskBytes, i);
  return buf;
}

function decodeDatagram(data) {
  if (!(data instanceof Uint8Array)) data = new Uint8Array(data);
  if (data.length < 1) return null;
  const type = data[0];
  if (type === TYPE_RESET) return { type: "reset" };
  if (type === TYPE_PADDLE && data.length >= 4) {
    const x = f16ToF32((data[1] << 8) | data[2]);
    const colorIdx = data[3] & 0x07;
    return { type: "paddle", x: clamp(x, 0, 1), colorIdx };
  }
  if (type === TYPE_STATE && data.length >= 1 + 2 + 2 + 2 + 2 + 2 + 1 + 2 + 5) {
    let i = 1;
    const ballX = f16ToF32((data[i] << 8) | data[i + 1]) * LOGICAL_W; i += 2;
    const ballY = f16ToF32((data[i] << 8) | data[i + 1]) * LOGICAL_H; i += 2;
    const velX = f16ToF32((data[i] << 8) | data[i + 1]); i += 2;
    const velY = f16ToF32((data[i] << 8) | data[i + 1]); i += 2;
    const paddleX = f16ToF32((data[i] << 8) | data[i + 1]) * LOGICAL_W; i += 2;
    const lives = data[i++];
    const score = (data[i] << 8) | data[i + 1]; i += 2;
    const mask = data.slice(i, i + 5);
    return {
      type: "state",
      ballX, ballY, velX, velY,
      paddleX, lives, score,
      bricks: unpackBrickMask(mask),
    };
  }
  return null;
}

// ── Game state ───────────────────────────────────────────────────────────────

const canvas = document.getElementById("canvas");
const ctx = canvas.getContext("2d", { alpha: true });

let myColorIdx = 0;
let myPaddleX = LOGICAL_W / 2;

let isDriver = false;
let lastStateTime = 0;
let driverTimeout = 0;

let ballX = LOGICAL_W / 2;
let ballY = LOGICAL_H / 2;
let velX = 2.8;
let velY = -3.6;

let lives = 3;
let score = 0;
let bricks = null; // boolean[40]

let remotePaddles = new Map(); // colorIdx (as proxy id) -> {x, colorIdx, lastSeen}

let gameOver = false;
let win = false;

// Initialize a fresh level
function resetLevel(keepLives = false) {
  bricks = new Array(COLS * ROWS).fill(true);
  ballX = LOGICAL_W / 2;
  ballY = PADDLE_Y - 30;
  velX = (Math.random() * 2 + 2.2) * (Math.random() < 0.5 ? -1 : 1);
  velY = -3.4 - Math.random() * 0.6;
  if (!keepLives) {
    lives = 3;
    score = 0;
  }
  gameOver = false;
  win = false;
}

resetLevel();

// ── Input ────────────────────────────────────────────────────────────────────

let mouseX = myPaddleX;
let keys = { left: false, right: false };

function resize() {
  // We render at fixed logical size and CSS scales the canvas
  canvas.width = LOGICAL_W;
  canvas.height = LOGICAL_H;
}
window.addEventListener("resize", resize);
resize();

canvas.addEventListener("mousemove", (e) => {
  const rect = canvas.getBoundingClientRect();
  const norm = (e.clientX - rect.left) / rect.width;
  mouseX = clamp(norm * LOGICAL_W, PADDLE_W / 2, LOGICAL_W - PADDLE_W / 2);
});

canvas.addEventListener("pointerdown", (e) => {
  const rect = canvas.getBoundingClientRect();
  const norm = (e.clientX - rect.left) / rect.width;
  mouseX = clamp(norm * LOGICAL_W, PADDLE_W / 2, LOGICAL_W - PADDLE_W / 2);
});

window.addEventListener("keydown", (e) => {
  if (e.key === "ArrowLeft" || e.key.toLowerCase() === "a") keys.left = true;
  if (e.key === "ArrowRight" || e.key.toLowerCase() === "d") keys.right = true;
  if (e.key.toLowerCase() === "r" && client) {
    client.sendDatagram(FEATURE_ID, encodeReset());
  }
});

window.addEventListener("keyup", (e) => {
  if (e.key === "ArrowLeft" || e.key.toLowerCase() === "a") keys.left = false;
  if (e.key === "ArrowRight" || e.key.toLowerCase() === "d") keys.right = false;
});

// Touch support (single finger drag)
canvas.addEventListener("touchmove", (e) => {
  e.preventDefault();
  const rect = canvas.getBoundingClientRect();
  const norm = (e.touches[0].clientX - rect.left) / rect.width;
  mouseX = clamp(norm * LOGICAL_W, PADDLE_W / 2, LOGICAL_W - PADDLE_W / 2);
}, { passive: false });

// ── UI ───────────────────────────────────────────────────────────────────────

const statusEl = document.getElementById("status");
const roomLabelEl = document.getElementById("room-label");
const scoreEl = document.getElementById("score");
const resetBtn = document.getElementById("reset");

function setStatus(text, cls = "") {
  statusEl.textContent = text;
  statusEl.className = cls;
}

roomLabelEl.textContent = `room: ${resolveRoom()}`;

resetBtn.addEventListener("click", () => {
  if (client) client.sendDatagram(FEATURE_ID, encodeReset());
});

function updateScoreUI() {
  scoreEl.textContent = `Score: ${score}   Lives: ${lives}`;
  if (isDriver) {
    scoreEl.textContent += "  • DRIVING";
  }
}

// ── Driver / simulation ──────────────────────────────────────────────────────

function becomeDriver() {
  if (isDriver) return;
  isDriver = true;
  lastStateTime = Date.now();
  // If we have no bricks (late joiner catching a dead game) force a reset
  const alive = bricks ? bricks.filter(b => b).length : 0;
  if (alive === 0 || lives <= 0) resetLevel();
}

function stepSimulation(dt) {
  if (!isDriver || gameOver) return;

  // Keyboard influence on our paddle (mouse is authoritative too)
  let target = mouseX;
  if (keys.left) target = myPaddleX - PADDLE_SPEED * 1.6;
  if (keys.right) target = myPaddleX + PADDLE_SPEED * 1.6;
  myPaddleX = clamp(target, PADDLE_W / 2, LOGICAL_W - PADDLE_W / 2);

  // Move ball
  ballX += velX * (dt / 16);
  ballY += velY * (dt / 16);

  // Wall collisions
  if (ballX - BALL_R < 0) { ballX = BALL_R; velX = Math.abs(velX); }
  if (ballX + BALL_R > LOGICAL_W) { ballX = LOGICAL_W - BALL_R; velX = -Math.abs(velX); }
  if (ballY - BALL_R < 0) { ballY = BALL_R; velY = Math.abs(velY); }

  // Bottom = life lost (only driver)
  if (ballY + BALL_R > LOGICAL_H + 10) {
    lives -= 1;
    updateScoreUI();
    if (lives <= 0) {
      gameOver = true;
      win = false;
      broadcastState(true);
      return;
    }
    // Reset ball on our paddle
    ballX = myPaddleX;
    ballY = PADDLE_Y - 18;
    velX = (Math.random() * 2.4 + 2.1) * (Math.random() < 0.5 ? -1 : 1);
    velY = -3.5;
    broadcastState(true);
    return;
  }

  // Paddle collision (our paddle + all known remote paddles)
  const paddles = [{ x: myPaddleX, w: PADDLE_W }];
  for (const p of remotePaddles.values()) {
    paddles.push({ x: p.x * LOGICAL_W, w: PADDLE_W });
  }

  for (const pd of paddles) {
    const px = pd.x;
    const py = PADDLE_Y;
    if (ballY + BALL_R >= py && ballY - BALL_R <= py + PADDLE_H &&
        ballX >= px - pd.w / 2 && ballX <= px + pd.w / 2 &&
        velY > 0) {
      // Hit paddle
      const offset = (ballX - px) / (pd.w / 2); // -1 .. +1
      velX = offset * 4.8;
      velY = -Math.abs(velY) * 1.03;
      // Nudge ball out
      ballY = py - BALL_R - 0.5;
      break;
    }
  }

  // Brick collisions
  let hit = false;
  const brickRects = [];
  for (let r = 0; r < ROWS; r++) {
    for (let c = 0; c < COLS; c++) {
      const idx = r * COLS + c;
      if (!bricks[idx]) continue;
      const bx = BRICK_LEFT + c * (BRICK_W + BRICK_GAP);
      const by = BRICK_TOP + r * (BRICK_H + BRICK_GAP);
      brickRects.push({ idx, x: bx, y: by, w: BRICK_W, h: BRICK_H });
    }
  }

  for (const br of brickRects) {
    // Simple circle vs AABB
    const closestX = clamp(ballX, br.x, br.x + br.w);
    const closestY = clamp(ballY, br.y, br.y + br.h);
    const dx = ballX - closestX;
    const dy = ballY - closestY;
    if (dx * dx + dy * dy < BALL_R * BALL_R) {
      // Hit
      bricks[br.idx] = false;
      score += 10;
      updateScoreUI();
      hit = true;

      // Determine bounce axis by penetration
      const prevX = ballX - velX * (dt / 16);
      const prevY = ballY - velY * (dt / 16);
      const fromLeftRight = Math.abs(prevX - closestX) > Math.abs(prevY - closestY);
      if (fromLeftRight) velX = -velX; else velY = -velY;

      // Small speed ramp
      const speed = Math.hypot(velX, velY);
      if (speed < 6.5) {
        const s = 6.5 / Math.max(0.1, speed);
        velX *= s; velY *= s;
      }
      break;
    }
  }

  if (hit) {
    const alive = bricks.filter(b => b).length;
    if (alive === 0) {
      gameOver = true;
      win = true;
      broadcastState(true);
      return;
    }
    broadcastState(true); // immediate update on brick break feels good
  }

  // Cap velocity
  const speed = Math.hypot(velX, velY);
  if (speed > 7.8) {
    const s = 7.8 / speed;
    velX *= s; velY *= s;
  }
}

// Broadcast full state (throttled by caller)
function broadcastState(force = false) {
  if (!client || !isDriver) return;
  const now = Date.now();
  if (!force && now - lastStateTime < 45) return; // ~22 fps max
  lastStateTime = now;

  const mask = packBrickMask(bricks);
  const pkt = encodeState(ballX, ballY, velX, velY, myPaddleX, lives, score, mask);
  client.sendDatagram(FEATURE_ID, pkt);
}

// ── Render loop ──────────────────────────────────────────────────────────────

function draw() {
  ctx.fillStyle = "#0d1117";
  ctx.fillRect(0, 0, LOGICAL_W, LOGICAL_H);

  // Subtle grid
  ctx.strokeStyle = "rgba(48, 54, 61, 0.35)";
  ctx.lineWidth = 1;
  for (let x = 40; x < LOGICAL_W; x += 40) {
    ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, LOGICAL_H); ctx.stroke();
  }
  for (let y = 40; y < LOGICAL_H; y += 40) {
    ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(LOGICAL_W, y); ctx.stroke();
  }

  // Bricks
  for (let r = 0; r < ROWS; r++) {
    for (let c = 0; c < COLS; c++) {
      const idx = r * COLS + c;
      if (!bricks[idx]) continue;
      const x = BRICK_LEFT + c * (BRICK_W + BRICK_GAP);
      const y = BRICK_TOP + r * (BRICK_H + BRICK_GAP);
      const hue = 200 + (r * 28);
      ctx.fillStyle = `hsl(${hue}, 72%, 58%)`;
      ctx.fillRect(x, y, BRICK_W, BRICK_H);
      ctx.fillStyle = "rgba(255,255,255,0.25)";
      ctx.fillRect(x, y, BRICK_W, 5); // highlight
    }
  }

  // Paddles (remote first so local is on top)
  const now = Date.now();
  ctx.lineJoin = "round";
  ctx.lineCap = "round";

  for (const [key, p] of remotePaddles) {
    const age = now - p.lastSeen;
    if (age > 2200) { remotePaddles.delete(key); continue; }
    const alpha = Math.max(0.35, 1 - age / 2200);
    const px = p.x * LOGICAL_W;
    ctx.globalAlpha = alpha;
    ctx.fillStyle = PALETTE[p.colorIdx % PALETTE.length];
    ctx.fillRect(px - PADDLE_W / 2, PADDLE_Y, PADDLE_W, PADDLE_H);
    // tiny label
    ctx.fillStyle = "#e6edf3";
    ctx.globalAlpha = alpha * 0.6;
    ctx.font = "10px ui-monospace";
    ctx.fillText("p" + (p.colorIdx + 1), px + PADDLE_W / 2 + 4, PADDLE_Y + 9);
  }
  ctx.globalAlpha = 1;

  // Local paddle (always crisp)
  ctx.fillStyle = PALETTE[myColorIdx];
  ctx.fillRect(myPaddleX - PADDLE_W / 2, PADDLE_Y, PADDLE_W, PADDLE_H);
  ctx.fillStyle = "rgba(255,255,255,0.3)";
  ctx.fillRect(myPaddleX - PADDLE_W / 2, PADDLE_Y, PADDLE_W, 3);

  // Ball
  if (!gameOver) {
    ctx.beginPath();
    ctx.arc(ballX, ballY, BALL_R, 0, Math.PI * 2);
    ctx.fillStyle = "#f0f6fc";
    ctx.fill();
    ctx.strokeStyle = "rgba(88, 166, 255, 0.6)";
    ctx.lineWidth = 1.5;
    ctx.stroke();
  }

  // Overlays
  if (gameOver) {
    ctx.fillStyle = "rgba(13,17,23,0.82)";
    ctx.fillRect(0, LOGICAL_H / 2 - 70, LOGICAL_W, 140);
    ctx.fillStyle = win ? "#3fb950" : "#f85149";
    ctx.font = "bold 42px ui-monospace";
    ctx.textAlign = "center";
    ctx.fillText(win ? "LEVEL CLEAR" : "GAME OVER", LOGICAL_W / 2, LOGICAL_H / 2 - 8);
    ctx.fillStyle = "#8b949e";
    ctx.font = "16px ui-monospace";
    ctx.fillText(win ? "Nice! Waiting for a reset..." : "Out of lives. Waiting for a reset...", LOGICAL_W / 2, LOGICAL_H / 2 + 32);
    ctx.textAlign = "left";
  }

  // Driver badge
  if (isDriver) {
    ctx.fillStyle = "#d29922";
    ctx.font = "12px ui-monospace";
    ctx.fillText("★ YOU ARE DRIVING THE BALL", 20, 26);
  }

  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);

// ── Main loop + relay ────────────────────────────────────────────────────────

let client = null;
let lastPaddleSend = 0;
const PADDLE_THROTTLE = 32;

let simLast = Date.now();

async function main() {
  const roomId = resolveRoom();

  myColorIdx = Math.floor(Math.random() * PALETTE.length);

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
    // Self-elect as driver quickly if nobody else is sending state
    setTimeout(() => {
      if (!lastStateTime) becomeDriver();
    }, 280);
  });

  client.on("disconnected", () => {
    setStatus("Disconnected", "error");
    remotePaddles.clear();
    isDriver = false;
  });

  client.on("error", (e) => setStatus("Error: " + e, "error"));

  client.on("datagram", (featureId, data) => {
    if (featureId !== FEATURE_ID) return;
    const msg = decodeDatagram(data);
    if (!msg) return;

    if (msg.type === "paddle") {
      // Use colorIdx as lightweight id (good enough for 8 players)
      remotePaddles.set(msg.colorIdx, {
        x: msg.x,
        colorIdx: msg.colorIdx,
        lastSeen: Date.now()
      });
    } else if (msg.type === "state") {
      lastStateTime = Date.now();
      // Adopt authoritative world
      ballX = msg.ballX; ballY = msg.ballY;
      velX = msg.velX; velY = msg.velY;
      lives = msg.lives;
      score = msg.score;
      bricks = msg.bricks;
      updateScoreUI();

      // If we see a state but we thought we were driver, yield (unless timeout later)
      if (isDriver && Date.now() - driverTimeout > 1200) {
        isDriver = false;
      }
    } else if (msg.type === "reset") {
      resetLevel();
      updateScoreUI();
      // Whoever receives a reset may become driver if current one vanished
      setTimeout(() => {
        if (!lastStateTime || Date.now() - lastStateTime > 900) becomeDriver();
      }, 60);
    }
  });

  await client.connect();

  // Initial paddle broadcast so others see us immediately
  setTimeout(() => {
    if (client) client.sendDatagram(FEATURE_ID, encodePaddle(myPaddleX / LOGICAL_W, myColorIdx));
  }, 80);

  // Game + network loop
  function tick() {
    const now = Date.now();
    const dt = Math.min(now - simLast, 50);
    simLast = now;

    // Update our local paddle from mouse + keyboard (smooth)
    let target = mouseX;
    if (keys.left) target = myPaddleX - PADDLE_SPEED * 1.8;
    if (keys.right) target = myPaddleX + PADDLE_SPEED * 1.8;
    myPaddleX = clamp(target, PADDLE_W / 2, LOGICAL_W - PADDLE_W / 2);

    // Driver simulation
    if (isDriver && !gameOver) {
      stepSimulation(dt);
    }

    // Send our paddle position (everyone does this)
    if (client && now - lastPaddleSend >= PADDLE_THROTTLE) {
      lastPaddleSend = now;
      const pkt = encodePaddle(myPaddleX / LOGICAL_W, myColorIdx);
      client.sendDatagram(FEATURE_ID, pkt);
    }

    // Driver broadcasts state at a steady rate
    if (isDriver) {
      broadcastState();
    }

    // Soft driver takeover if we haven't seen state in a while
    if (!isDriver && lastStateTime && now - lastStateTime > 1350) {
      becomeDriver();
      // continue from last known good state (already in our variables)
    }

    updateScoreUI();
    setTimeout(tick, 16);
  }
  tick();

  // Send leave on unload
  window.addEventListener("beforeunload", () => {
    if (client) {
      try { client.disconnect(); } catch {}
    }
  });
}

main().catch((e) => {
  console.error("[brick-breaker]", e);
  setStatus("Fatal: " + (e.message || e), "error");
});
