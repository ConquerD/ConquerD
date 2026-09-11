import { DemoSession } from "../../web-sdk/demo-session.mjs";
import { mountShell, COLORS } from "../../web-sdk/demo-shell.mjs";
import { Board, LIMIT } from "./board.mjs";
const session = new DemoSession("shared-canvas");
const shell = mountShell(session, {
  title: "Shared Canvas",
  category: "Create / collaborative tools",
  description:
    "A shared sketchpad for plans, diagrams and a little creative chaos. New arrivals catch up from a peer.",
  controls:
    "Draw with a pointer or touch · Your strokes stay when the view resizes",
  detail:
    "An app-defined operation log supports shared ink, erasing, deterministic replay and peer-supplied late-join history. History is bounded and lives only in open pages. Datagram reconciliation repairs missed updates while a peer still has them; this is not durable document storage.",
});
const canvas = document.querySelector("canvas"),
  ctx = canvas.getContext("2d");
const ink = document.createElement("canvas"),
  inkCtx = ink.getContext("2d");
const board = new Board();
let serial = 0,
  color = COLORS[0],
  width = 4,
  eraser = false,
  drawing = false,
  last = null,
  pointerId = null,
  rendered = -1;
let renderedOps = [];
const replayRequests = new Map();
let outgoing = null,
  lastDigest = "";
const palette = document.querySelector("#palette"),
  sizes = document.querySelector("#sizes"),
  eraserButton = document.querySelector("#eraser");
for (const c of [...COLORS, "#edf3fd"]) {
  const b = document.createElement("button");
  b.className = "swatch";
  b.style.background = c;
  b.title = c;
  b.setAttribute("aria-label", `Ink ${c}`);
  b.onclick = () => {
    color = c;
    eraser = false;
    updateTools();
  };
  palette.append(b);
}
for (const size of [2, 4, 8, 16]) {
  const b = document.createElement("button");
  b.className = "size-btn";
  b.title = `${size} pixel stroke`;
  b.setAttribute("aria-label", b.title);
  const dot = document.createElement("span");
  dot.className = "dot";
  dot.style.width = dot.style.height = `${Math.max(3, size)}px`;
  b.append(dot);
  b.onclick = () => {
    width = size;
    updateTools();
  };
  sizes.append(b);
}
function updateTools() {
  [...palette.children].forEach((b, i) =>
    b.classList.toggle(
      "active",
      !eraser && [...COLORS, "#edf3fd"][i] === color,
    ),
  );
  [...sizes.children].forEach((b, i) =>
    b.classList.toggle("active", [2, 4, 8, 16][i] === width),
  );
  eraserButton.setAttribute("aria-pressed", String(eraser));
  canvas.classList.toggle("eraser-cursor", eraser);
}
eraserButton.onclick = () => {
  eraser = !eraser;
  updateTools();
};
updateTools();
new ResizeObserver(() => {
  const r = canvas.parentElement.getBoundingClientRect(),
    dpr = Math.min(devicePixelRatio || 1, 2);
  canvas.width = ink.width = Math.max(1, Math.round(r.width * dpr));
  canvas.height = ink.height = Math.max(1, Math.round(r.height * dpr));
  rendered = -1;
}).observe(canvas.parentElement);
function create(kind, coords = {}) {
  const op = {
    id: `${session.id}:${++serial}`,
    clock: board.clock + 1,
    kind,
    ...coords,
  };
  if (board.add(op)) {
    session.send({ t: "ops", ops: [op] });
    return true;
  }
  shell.notice(
    `Canvas limit reached (${LIMIT} segments). Clear to begin again.`,
  );
  return false;
}
let clearArmed = false;
document.querySelector("#clear").onclick = () => {
  if (!clearArmed) {
    clearArmed = true;
    document.querySelector("#clear").textContent = "Clear for everyone?";
    setTimeout(() => {
      clearArmed = false;
      document.querySelector("#clear").textContent = "Clear canvas";
    }, 3000);
    return;
  }
  create("clear");
  clearArmed = false;
  document.querySelector("#clear").textContent = "Clear canvas";
};
function position(e) {
  const r = canvas.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)),
    y: Math.max(0, Math.min(1, (e.clientY - r.top) / r.height)),
  };
}
function segment(p) {
  if (!last) return;
  create(eraser ? "erase" : "ink", {
    x1: last.x,
    y1: last.y,
    x2: p.x,
    y2: p.y,
    width,
    color,
  });
  last = p;
}
canvas.addEventListener("pointerdown", (e) => {
  if (e.button !== 0 || !session.connected) return;
  drawing = true;
  pointerId = e.pointerId;
  canvas.setPointerCapture(pointerId);
  last = position(e);
  segment(last);
});
let lastDraw = 0;
canvas.addEventListener("pointermove", (e) => {
  if (
    !drawing ||
    e.pointerId !== pointerId ||
    performance.now() - lastDraw < 20
  )
    return;
  lastDraw = performance.now();
  segment(position(e));
});
function end(e) {
  if (!drawing || e.pointerId !== pointerId) return;
  segment(position(e));
  drawing = false;
  last = null;
  if (canvas.hasPointerCapture(e.pointerId))
    canvas.releasePointerCapture(e.pointerId);
}
canvas.addEventListener("pointerup", end);
canvas.addEventListener("pointercancel", () => {
  drawing = false;
  last = null;
});
canvas.addEventListener("lostpointercapture", () => {
  drawing = false;
  last = null;
});
// Only one bounded replay runs per page; peers retry on digest mismatch.
async function replay(to) {
  if (outgoing) return;
  outgoing = to;
  try {
    const ops = board.sorted();
    for (let i = 0; i < ops.length && session.connected; i += 3) {
      await session.send({ t: "ops", to, ops: ops.slice(i, i + 3) });
      await new Promise((r) => setTimeout(r, 45));
    }
  } finally {
    outgoing = null;
  }
}
session.on("data", (m, id) => {
  if (!m || typeof m !== "object") return;
  if (
    m.t === "ops" &&
    (!m.to || m.to === session.id) &&
    Array.isArray(m.ops) &&
    m.ops.length <= 3
  )
    for (const op of m.ops) board.add(op);
  if (
    m.t === "digest" &&
    typeof m.value === "string" &&
    m.value !== board.digest()
  ) {
    const now = performance.now();
    if (now - (replayRequests.get(id) || -10000) > 8000) {
      replayRequests.set(id, now);
      session.send({ t: "sync", to: id });
    }
  }
  if (m.t === "sync" && m.to === session.id) replay(id);
});
session.on("join", () => {
  lastDigest = "";
});
const sync = setInterval(() => {
  if (!session.connected) return;
  const digest = board.digest();
  session.send({ t: "digest", value: digest });
  if (digest !== lastDigest) {
    lastDigest = digest;
    shell.notice(`${board.ops.size} shared operations`);
  }
  for (const id of replayRequests.keys())
    if (!session.peers.has(id)) replayRequests.delete(id);
}, 2000);
window.addEventListener("pagehide", () => clearInterval(sync), { once: true });
function draw() {
  const w = canvas.width,
    h = canvas.height;
  if (rendered !== board.revision) {
    const ordered = board.sorted();
    const appendOnly =
      rendered >= 0 &&
      renderedOps.length <= ordered.length &&
      renderedOps.every((op, i) => op.id === ordered[i].id);
    if (!appendOnly) inkCtx.clearRect(0, 0, w, h);
    for (const o of ordered.slice(appendOnly ? renderedOps.length : 0)) {
      if (o.kind === "clear") continue;
      inkCtx.globalCompositeOperation =
        o.kind === "erase" ? "destination-out" : "source-over";
      inkCtx.strokeStyle = o.color;
      inkCtx.fillStyle = o.color;
      // A fixed logical width keeps brush scale consistent across device sizes.
      inkCtx.lineWidth = (o.width * w) / 1000;
      inkCtx.lineCap = inkCtx.lineJoin = "round";
      if (o.x1 === o.x2 && o.y1 === o.y2) {
        inkCtx.beginPath();
        inkCtx.arc(o.x1 * w, o.y1 * h, inkCtx.lineWidth / 2, 0, Math.PI * 2);
        inkCtx.fill();
      } else {
        inkCtx.beginPath();
        inkCtx.moveTo(o.x1 * w, o.y1 * h);
        inkCtx.lineTo(o.x2 * w, o.y2 * h);
        inkCtx.stroke();
      }
    }
    rendered = board.revision;
    renderedOps = ordered;
    ctx.fillStyle = "#111d2d";
    ctx.fillRect(0, 0, w, h);
    ctx.fillStyle = "#526b8848";
    const grid = 32 * (devicePixelRatio || 1);
    for (let x = grid; x < w; x += grid)
      for (let y = grid; y < h; y += grid) {
        ctx.beginPath();
        ctx.arc(x, y, 1.2, 0, Math.PI * 2);
        ctx.fill();
      }
    ctx.drawImage(ink, 0, 0);
  }
  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);
session
  .connect()
  .catch(() =>
    shell.setStatus(
      "Open in DoubleSlash through a connected supernode",
      "error",
    ),
  );
