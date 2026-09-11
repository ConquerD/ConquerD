import { DemoSession } from "../../web-sdk/demo-session.mjs";
import { mountShell, peerColor } from "../../web-sdk/demo-shell.mjs";
import {
  W,
  H,
  PADDLE_Y,
  PADDLE_W,
  BALL_R,
  clamp,
  brickRect,
  newWorld,
  validWorld,
  command,
  step,
  sampleBall,
} from "./world.mjs";

const session = new DemoSession("brick-breaker");
const shell = mountShell(session, {
  title: "Brick Breaker",
  category: "Play / shared simulation",
  description:
    "One ball. Every paddle. Open this room on another device and keep the rally alive together.",
  controls: "Mouse, touch or ← → / A D · Space to launch · Esc exits focus",
  detail:
    "A peer simulates the world at 120 steps per second and sends 20 snapshots per second. Other peers buffer and interpolate the ball. Session election hands simulation to a remaining peer when its coordinator leaves. The relay runs no game physics.",
});
const canvas = document.querySelector("canvas"),
  ctx = canvas.getContext("2d");
const score = document.querySelector("#score"),
  launch = document.querySelector("#launch");
let world = newWorld(),
  localX = 400,
  targetX = 400,
  keys = new Set();
let previousTime = performance.now(),
  accumulator = 0,
  sentAt = 0,
  wasLeader = false;
let samples = [],
  trail = [],
  particles = [],
  pendingCommand = null,
  retryAt = 0;
const remote = new Map();
const reducedMotion = matchMedia("(prefers-reduced-motion: reduce)").matches;
function resize() {
  const rect = canvas.parentElement.getBoundingClientRect();
  const scale = Math.max(
    0.05,
    Math.min((rect.width - 24) / W, (rect.height - 24) / H),
  );
  canvas.style.width = `${W * scale}px`;
  canvas.style.height = `${H * scale}px`;
  const dpr = Math.min(devicePixelRatio || 1, 2);
  canvas.width = Math.round(W * dpr);
  canvas.height = Math.round(H * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
}
new ResizeObserver(resize).observe(canvas.parentElement);
function move(e) {
  const rect = canvas.getBoundingClientRect();
  targetX = clamp(
    ((e.clientX - rect.left) / rect.width) * W,
    PADDLE_W / 2,
    W - PADDLE_W / 2,
  );
}
canvas.addEventListener("pointermove", move);
canvas.addEventListener("pointerdown", (e) => {
  canvas.focus();
  canvas.setPointerCapture(e.pointerId);
  move(e);
  if (world.phase === "serve") request("launch");
});
window.addEventListener("keydown", (e) => {
  if (e.target.matches("input,button,summary,a")) return;
  if (["ArrowLeft", "ArrowRight", "a", "d", "A", "D", " "].includes(e.key)) {
    e.preventDefault();
    keys.add(e.key.toLowerCase());
  }
  if (e.key === " " && !e.repeat) request("launch");
});
window.addEventListener("keyup", (e) => keys.delete(e.key.toLowerCase()));
window.addEventListener("blur", () => keys.clear());
function request(action) {
  if (!session.connected) return;
  pendingCommand = { t: "command", action, id: crypto.randomUUID() };
  retryAt = 0;
}
launch.onclick = () => request("launch");
document.querySelector("#reset").onclick = () => request("reset");
function burst(i) {
  if (reducedMotion) return;
  const b = brickRect(i);
  for (let n = 0; n < 10; n++)
    particles.push({
      x: b.x + b.w / 2,
      y: b.y + b.h / 2,
      vx: (Math.random() - 0.5) * 240,
      vy: (Math.random() - 0.5) * 240,
      life: 1,
      color: `hsl(${170 + Math.floor(i / 8) * 28} 80% 70%)`,
    });
  particles = particles.slice(-300);
}
function acceptState(s) {
  world.bricks.forEach((v, i) => {
    if (v && !s.bricks[i]) burst(i);
  });
  world = s;
  samples.push({ ...s, time: performance.now() });
  if (samples.length > 8) samples.shift();
  if (pendingCommand && s.ack.includes(pendingCommand.id))
    pendingCommand = null;
}
session.on("data", (msg, id) => {
  if (!msg || typeof msg !== "object") return;
  if (
    msg.t === "paddle" &&
    Number.isFinite(msg.x) &&
    msg.x >= 50 &&
    msg.x <= 750
  )
    remote.set(id, { x: msg.x, display: remote.get(id)?.display ?? msg.x });
  if (
    msg.t === "state" &&
    validWorld(msg.s) &&
    (id === session.leader || performance.now() - session.started < 1200)
  )
    acceptState(msg.s);
  if (
    msg.t === "command" &&
    session.isLeader &&
    ["launch", "reset"].includes(msg.action) &&
    typeof msg.id === "string" &&
    msg.id.length <= 36
  ) {
    command(world, msg.action, msg.id);
    session.send({ t: "state", s: world });
  }
});
function rounded(x, y, w, h, r, color) {
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.roundRect(x, y, w, h, r);
  ctx.fill();
}
function draw(now) {
  const dt = Math.min((now - previousTime) / 1000, 0.05);
  previousTime = now;
  const direction =
    (keys.has("arrowright") || keys.has("d") ? 1 : 0) -
    (keys.has("arrowleft") || keys.has("a") ? 1 : 0);
  if (direction) targetX = clamp(targetX + direction * 620 * dt, 50, 750);
  localX += (targetX - localX) * (1 - Math.exp(-24 * dt));
  for (const id of remote.keys()) if (!session.peers.has(id)) remote.delete(id);
  const leader = session.isLeader && now - session.started > 1200;
  if (wasLeader && !leader && session.connected)
    session.send({ t: "state", s: world });
  if (leader) {
    accumulator += dt;
    const before = world.bricks.slice();
    while (accumulator >= 1 / 120) {
      step(world, 1 / 120, [localX, ...[...remote.values()].map((p) => p.x)]);
      accumulator -= 1 / 120;
    }
    before.forEach((v, i) => {
      if (v && !world.bricks[i]) burst(i);
    });
  } else accumulator = 0;
  wasLeader = leader;
  if (session.connected && now - sentAt >= 50) {
    sentAt = now;
    session.send({ t: "paddle", x: Math.round(localX) });
    if (leader) session.send({ t: "state", s: world });
  }
  if (pendingCommand && now - retryAt > 250) {
    retryAt = now;
    if (leader) {
      command(world, pendingCommand.action, pendingCommand.id);
      pendingCommand = null;
    } else session.send(pendingCommand);
  }
  const ball = leader ? world : sampleBall(samples, now - 85) || world;
  ctx.fillStyle = "#0d1523";
  ctx.fillRect(0, 0, W, H);
  ctx.strokeStyle = "#223148";
  ctx.lineWidth = 0.5;
  for (let x = 40; x < W; x += 40) {
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, H);
    ctx.stroke();
  }
  for (let y = 40; y < H; y += 40) {
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(W, y);
    ctx.stroke();
  }
  world.bricks.forEach((v, i) => {
    if (!v) return;
    const b = brickRect(i);
    rounded(
      b.x,
      b.y,
      b.w,
      b.h,
      5,
      `hsl(${170 + Math.floor(i / 8) * 28} 67% 62%)`,
    );
    rounded(b.x + 3, b.y + 2, b.w - 6, 3, 2, "#ffffff55");
  });
  for (const [id, p] of remote) {
    p.display += (p.x - p.display) * (1 - Math.exp(-18 * dt));
    rounded(p.display - 50, PADDLE_Y, 100, 12, 6, peerColor(id));
  }
  ctx.shadowColor = peerColor(session.id);
  ctx.shadowBlur = reducedMotion ? 0 : 16;
  rounded(localX - 50, PADDLE_Y, 100, 12, 6, peerColor(session.id));
  ctx.shadowBlur = 0;
  if (!reducedMotion && world.phase === "playing") {
    trail.push({ x: ball.x, y: ball.y });
    if (trail.length > 14) trail.shift();
  } else trail = [];
  trail.forEach((p, i) => {
    ctx.beginPath();
    ctx.arc(p.x, p.y, (BALL_R * i) / 14, 0, Math.PI * 2);
    ctx.fillStyle = `rgba(135,183,255,${i / 40})`;
    ctx.fill();
  });
  ctx.beginPath();
  ctx.arc(ball.x, ball.y, BALL_R, 0, Math.PI * 2);
  ctx.fillStyle = "#f5fcff";
  ctx.fill();
  particles = particles.filter((p) => p.life > 0);
  for (const p of particles) {
    p.life -= dt * 2;
    p.x += p.vx * dt;
    p.y += p.vy * dt;
    ctx.globalAlpha = Math.max(0, p.life);
    rounded(p.x, p.y, 4, 4, 1, p.color);
  }
  ctx.globalAlpha = 1;
  if (world.phase !== "playing") {
    ctx.textAlign = "center";
    ctx.fillStyle = "#edf3fd";
    ctx.font = "600 30px system-ui";
    ctx.fillText(
      world.phase === "won"
        ? "A perfect clear."
        : world.phase === "over"
          ? "One more round?"
          : "Keep it in play. Together.",
      400,
      335,
    );
    ctx.font = "15px system-ui";
    ctx.fillStyle = "#a1aec3";
    ctx.fillText(
      world.phase === "serve"
        ? "Launch when you are ready. Every paddle counts."
        : "Start a new game with the controls above.",
      400,
      365,
    );
    ctx.textAlign = "left";
  }
  score.textContent = `${world.score.toString().padStart(3, "0")} points · ${world.lives} lives`;
  launch.disabled = !session.connected || world.phase !== "serve";
  document.querySelector("#reset").disabled = !session.connected;
  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);
session
  .connect()
  .catch((err) => {
    // The reason goes to the console as well as the status line. A bare
    // `.catch` here made every failure look identical from outside the page,
    // which on Android means nothing reaches logcat at all - the one place
    // you can look when the portal is not cooperating.
    console.error("[demo] session connect failed:", err);
    shell.setStatus(
      "Open in DoubleSlash through a connected supernode",
      "error",
    );
  });
