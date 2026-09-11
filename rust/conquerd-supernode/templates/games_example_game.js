import { DemoSession } from "../../web-sdk/demo-session.mjs";
import { mountShell, peerColor, shortName } from "../../web-sdk/demo-shell.mjs";
const session = new DemoSession("presence");
const shell = mountShell(session, {
  title: "Presence Playground",
  category: "Connect / shared presence",
  description:
    "A small shared space. Move a pointer, tap a point, or watch another device arrive.",
  controls: "Move to share your pointer · Click or tap to send a ripple",
  detail:
    "Ephemeral presence, idle heartbeats and shared attention markers. The same pattern can power a team dashboard, a tabletop lobby, a presentation pointer or a collaborative editor. No global directory or account service is involved.",
});
const canvas = document.querySelector("canvas"),
  ctx = canvas.getContext("2d");
let width = 800,
  height = 600,
  local = { x: 0.5, y: 0.5 },
  lastSend = 0,
  lastFrame = performance.now();
const pointers = new Map(),
  ripples = [];
const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
new ResizeObserver(() => {
  const r = canvas.parentElement.getBoundingClientRect();
  width = r.width;
  height = r.height;
  const dpr = Math.min(devicePixelRatio || 1, 2);
  canvas.width = Math.round(width * dpr);
  canvas.height = Math.round(height * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
}).observe(canvas.parentElement);
function position(e) {
  const r = canvas.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)),
    y: Math.max(0, Math.min(1, (e.clientY - r.top) / r.height)),
  };
}
function broadcast() {
  if (session.connected) session.send({ t: "cursor", ...local });
}
canvas.addEventListener("pointermove", (e) => {
  local = position(e);
  if (performance.now() - lastSend > 40) {
    lastSend = performance.now();
    broadcast();
  }
});
canvas.addEventListener("pointerdown", (e) => {
  local = position(e);
  const ripple = { t: "ripple", ...local };
  session.send(ripple);
  addRipple(ripple, session.id);
});
function addRipple(p, id) {
  ripples.push({ ...p, id, time: performance.now() });
  if (ripples.length > 60) ripples.shift();
  document.querySelector("#activity").textContent =
    `${id === session.id ? "You" : shortName(id)} marked a point`;
}
session.on("data", (m, id) => {
  if (
    !m ||
    !["cursor", "ripple"].includes(m.t) ||
    !Number.isFinite(m.x) ||
    !Number.isFinite(m.y) ||
    m.x < 0 ||
    m.x > 1 ||
    m.y < 0 ||
    m.y > 1
  )
    return;
  if (m.t === "ripple") addRipple(m, id);
  else {
    const old = pointers.get(id);
    pointers.set(id, { ...m, dx: old?.dx ?? m.x, dy: old?.dy ?? m.y });
  }
});
const heartbeat = setInterval(broadcast, 1000);
window.addEventListener("pagehide", () => clearInterval(heartbeat), {
  once: true,
});
function pointer(x, y, id, self) {
  ctx.save();
  ctx.translate(x * width, y * height);
  ctx.fillStyle = peerColor(id);
  ctx.strokeStyle = "#0c1018";
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(0, 0);
  ctx.lineTo(0, 22);
  ctx.lineTo(6, 16);
  ctx.lineTo(12, 27);
  ctx.lineTo(17, 24);
  ctx.lineTo(11, 14);
  ctx.lineTo(20, 14);
  ctx.closePath();
  ctx.fill();
  ctx.stroke();
  const label = self ? "You" : shortName(id);
  ctx.font = "12px system-ui";
  const tw = ctx.measureText(label).width;
  ctx.fillStyle = "#1c2c42";
  ctx.beginPath();
  ctx.roundRect(20, 20, tw + 16, 25, 7);
  ctx.fill();
  ctx.fillStyle = peerColor(id);
  ctx.fillText(label, 28, 37);
  ctx.restore();
}
function draw(now) {
  const dt = Math.min((now - lastFrame) / 1000, 0.05);
  lastFrame = now;
  ctx.clearRect(0, 0, width, height);
  ctx.fillStyle = "#526b883e";
  for (let x = 24; x < width; x += 28)
    for (let y = 24; y < height; y += 28) {
      ctx.beginPath();
      ctx.arc(x, y, 1, 0, Math.PI * 2);
      ctx.fill();
    }
  if (!session.peers.size) {
    ctx.textAlign = "center";
    ctx.font = "600 25px system-ui";
    ctx.fillStyle = "#edf3fd";
    ctx.fillText("A place to be here, together.", width / 2, height / 2 - 15);
    ctx.font = "14px system-ui";
    ctx.fillStyle = "#a1aec3";
    ctx.fillText(
      "Copy the link and join from another device.",
      width / 2,
      height / 2 + 18,
    );
    ctx.textAlign = "left";
  }
  for (const [id, p] of pointers) {
    if (!session.peers.has(id)) {
      pointers.delete(id);
      continue;
    }
    const t = reduced ? 1 : 1 - Math.exp(-20 * dt);
    p.dx += (p.x - p.dx) * t;
    p.dy += (p.y - p.dy) * t;
    pointer(p.dx, p.dy, id, false);
  }
  pointer(local.x, local.y, session.id, true);
  for (let i = ripples.length - 1; i >= 0; i--) {
    const p = ripples[i],
      age = (now - p.time) / 1200;
    if (age > 1) {
      ripples.splice(i, 1);
      continue;
    }
    ctx.globalAlpha = 1 - age;
    ctx.strokeStyle = peerColor(p.id);
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(
      p.x * width,
      p.y * height,
      reduced ? 18 : 10 + age * 65,
      0,
      Math.PI * 2,
    );
    ctx.stroke();
  }
  ctx.globalAlpha = 1;
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
