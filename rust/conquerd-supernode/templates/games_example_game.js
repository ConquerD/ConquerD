import { DemoSession } from "../../web-sdk/demo-session.mjs";
import { mountShell, shortName } from "../../web-sdk/demo-shell.mjs";
import {
  PALETTE,
  SHAPES,
  SHAPE_NAMES,
  SPEEDS,
  SPEED_NAMES,
  MODES,
  WALLS,
  GRID_W,
  GRID_H,
  MAX_SNAKES,
  snakeKey,
  cellX,
  cellY,
  defaultLook,
  validLook,
  shapeOutline,
  isStroked,
  centerOffset,
  newSettings,
  mergeSettings,
  change,
  worldFor,
  joinSnake,
  leaveSnake,
  queueTurn,
  stepSnake,
  packWorld,
  unpackWorld,
} from "./playground.mjs";

const session = new DemoSession("presence");
const shell = mountShell(session, {
  title: "Presence Playground",
  category: "Connect / shared presence",
  description:
    "A small shared space. Move a pointer, pick how you appear, or take a snake and share the board.",
  controls:
    "Move to share your pointer · Click to mark a point · 1-6 pick a shape · Snake: arrows, WASD, or tap where you want to go",
  detail:
    "Ephemeral presence, idle heartbeats and shared attention markers, plus a snake round on the same session. Appearance is yours alone; mode, speed, edges and round are room settings any button can change, resolved by a Lamport clock so every device lands on the same answer. The board is a deterministic simulation a coordinator steps and snapshots; the same room and round always deal the same food, and a coordinator leaving hands the round on rather than restarting it.",
});
const canvas = document.querySelector("canvas"),
  ctx = canvas.getContext("2d");
const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
const key = snakeKey(session.id);
let width = 800,
  height = 600,
  local = { x: 0.5, y: 0.5 };
let look = defaultLook(session.id),
  settings = newSettings(),
  world = worldFor(session.room, 0);
let playing = false,
  want = null;
let lastSend = 0,
  lastFrame = performance.now(),
  tickAt = 0,
  echoAt = 0,
  wasLeader = false;
const pointers = new Map(),
  looks = new Map(),
  intents = new Map(),
  ripples = [];
let announcement = "",
  announceUntil = 0,
  activity = "";

new ResizeObserver(() => {
  const r = canvas.parentElement.getBoundingClientRect();
  width = r.width;
  height = r.height;
  const dpr = Math.min(devicePixelRatio || 1, 2);
  canvas.width = Math.round(width * dpr);
  canvas.height = Math.round(height * dpr);
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
}).observe(canvas.parentElement);

// One renderer for every shape the toolbar offers: pointers, marks, snake
// heads and the toolbar icons all come out of the same outline.
function drawShape(target, shape, x, y, size, color, center = true) {
  const outline = shapeOutline(shape);
  target.save();
  target.translate(x, y);
  if (center) {
    const [dx, dy] = centerOffset(shape);
    target.translate(dx * size, dy * size);
  }
  target.beginPath();
  if (outline)
    outline.forEach(([px, py], i) =>
      i
        ? target.lineTo(px * size, py * size)
        : target.moveTo(px * size, py * size),
    );
  else
    target.arc(0, 0, size * (isStroked(shape) ? 0.78 : 0.92), 0, Math.PI * 2);
  if (outline) target.closePath();
  if (isStroked(shape)) {
    target.strokeStyle = color;
    target.lineWidth = Math.max(2, size * 0.32);
    target.stroke();
  } else {
    target.fillStyle = color;
    target.fill();
    target.strokeStyle = "#0c1018";
    target.lineWidth = Math.max(1, size * 0.1);
    target.stroke();
  }
  target.restore();
}

// ------------------------------------------------------------------ toolbar

// Each button is one deterministic action. Appearance buttons change only how
// you are drawn; room buttons write a setting every device resolves the same
// way, so the same press has the same effect wherever it lands.
const refreshers = [];
function control(host, { text, title, onPick, active, decorate }) {
  const button = document.createElement("button");
  button.type = "button";
  button.title = title;
  button.setAttribute("aria-label", title);
  if (text) button.textContent = text;
  if (decorate) decorate(button);
  button.onclick = onPick;
  host.append(button);
  refreshers.push(() => {
    const on = active();
    button.classList.toggle("active", on);
    button.setAttribute("aria-pressed", String(on));
  });
  return button;
}
const MODE_NAMES = { presence: "Presence", snake: "Snake" };
const WALL_NAMES = { wrap: "Wrap", solid: "Solid" };
for (const mode of MODES)
  control(document.querySelector("#modes"), {
    text: MODE_NAMES[mode],
    title: `${MODE_NAMES[mode]} mode for everyone in this room`,
    onPick: () => share({ mode }, `${MODE_NAMES[mode]} mode`),
    active: () => settings.mode === mode,
  });
PALETTE.forEach((color, index) =>
  control(document.querySelector("#palette"), {
    title: `Your color ${color}`,
    onPick: () => setLook({ c: index }, "Color"),
    active: () => look.c === index,
    decorate: (button) => {
      button.className = "swatch";
      button.style.background = color;
    },
  }),
);
SHAPES.forEach((shape, index) => {
  const icon = document.createElement("canvas");
  icon.width = icon.height = 22;
  const button = control(document.querySelector("#shapes"), {
    title: `Your shape: ${SHAPE_NAMES[index]}`,
    onPick: () => setLook({ s: index }, SHAPE_NAMES[index]),
    active: () => look.s === index,
    decorate: (b) => {
      b.className = "shape-btn";
      b.append(icon);
    },
  });
  button.repaint = () => {
    const paint = icon.getContext("2d");
    paint.clearRect(0, 0, 22, 22);
    drawShape(paint, shape, 11, 11, 8, PALETTE[look.c]);
  };
  refreshers.push(button.repaint);
});
SPEED_NAMES.forEach((name, index) =>
  control(document.querySelector("#speeds"), {
    text: name,
    title: `${name} snake · one step every ${SPEEDS[index]} ms`,
    onPick: () => share({ speed: index }, `${name} snake`),
    active: () => settings.speed === index,
  }),
);
for (const walls of WALLS)
  control(document.querySelector("#walls"), {
    text: WALL_NAMES[walls],
    title:
      walls === "wrap"
        ? "Edges wrap around the board"
        : "Edges are walls; hitting one costs the snake",
    onPick: () => share({ walls }, `${WALL_NAMES[walls]} edges`),
    active: () => settings.walls === walls,
  });
const playButton = document.querySelector("#play");
playButton.onclick = () => {
  playing = !playing;
  want = null;
  broadcast();
  announce(playing ? "You took a snake" : "You are watching");
  refresh();
};
document.querySelector("#new-round").onclick = () =>
  share({ round: settings.round + 1 }, `Round ${settings.round + 2}`);

function refresh() {
  for (const fn of refreshers) fn();
  const snake = settings.mode === "snake";
  document.querySelector("#snake-tools").hidden = !snake;
  playButton.textContent = playing ? "Watch instead" : "Take a snake";
  playButton.setAttribute("aria-pressed", String(playing));
  for (const button of document.querySelectorAll(
    "#modes button, #speeds button, #walls button, #new-round",
  ))
    button.disabled = !session.connected;
}
function setLook(patch, note) {
  look = { ...look, ...patch };
  broadcast();
  announce(`${note} · yours only`);
  refresh();
}
// A room setting: send it once and let the coordinator republish the merged
// result, so a dropped datagram costs a press rather than agreement.
function share(patch, note) {
  settings = change(settings, patch, session.id);
  session.send({ t: "set", st: settings });
  announce(`${note} · everyone here`);
  refresh();
}
function announce(text) {
  announcement = text;
  announceUntil = performance.now() + 2600;
}
function statusLine() {
  if (performance.now() < announceUntil) return announcement;
  if (settings.mode !== "snake") return "Presence · move, mark, pick a shape";
  if (playing && !world.snakes[key])
    return `Waiting for a slot · ${MAX_SNAKES} snakes is the limit`;
  return `Snake · ${SPEED_NAMES[settings.speed]} · ${WALL_NAMES[settings.walls]} edges · Round ${settings.round + 1}`;
}

// -------------------------------------------------------------------- input

function position(e) {
  const r = canvas.getBoundingClientRect();
  return {
    x: Math.max(0, Math.min(1, (e.clientX - r.left) / r.width)),
    y: Math.max(0, Math.min(1, (e.clientY - r.top) / r.height)),
  };
}
function broadcast() {
  if (!session.connected) return;
  lastSend = performance.now();
  session.send({
    t: "here",
    x: local.x,
    y: local.y,
    c: look.c,
    s: look.s,
    p: playing ? 1 : 0,
    ...(want === null ? {} : { d: want }),
  });
}
// The wanted direction rides the presence heartbeat as well as its own press.
// Re-applying the same turn is a no-op, so a lost datagram costs one beat of
// latency instead of a missed corner.
function steer(dir) {
  want = dir;
  if (session.isLeader) queueTurn(world, key, dir);
  broadcast();
}
function steerToward(point) {
  const snake = world.snakes[key];
  if (!snake) return false;
  // Measured from the middle of the head cell, so the dominant axis of the tap
  // is the direction the player pointed at.
  const board = boardRect();
  const x =
    (point.x * width - board.x) / board.scale - cellX(snake.body[0]) - 0.5;
  const y =
    (point.y * height - board.y) / board.scale - cellY(snake.body[0]) - 0.5;
  if (!x && !y) return false;
  steer(Math.abs(x) > Math.abs(y) ? (x > 0 ? 1 : 3) : y > 0 ? 2 : 0);
  return true;
}
canvas.addEventListener("pointermove", (e) => {
  local = position(e);
  if (performance.now() - lastSend > 40) broadcast();
});
canvas.addEventListener("pointerdown", (e) => {
  canvas.focus();
  local = position(e);
  // In snake mode a tap steers the snake you hold; with no snake it still
  // marks a point, because watching a round is still being here.
  if (settings.mode === "snake" && steerToward(local)) return;
  const mark = { t: "mark", x: local.x, y: local.y, c: look.c, s: look.s };
  session.send(mark);
  addMark(mark, session.id);
});
window.addEventListener("keydown", (e) => {
  if (e.target.matches("input,button,summary,a")) return;
  const dir = {
    arrowup: 0,
    arrowright: 1,
    arrowdown: 2,
    arrowleft: 3,
    w: 0,
    d: 1,
    s: 2,
    a: 3,
  }[e.key.toLowerCase()];
  if (dir !== undefined && settings.mode === "snake") {
    e.preventDefault();
    steer(dir);
  }
  const shape = Number(e.key) - 1;
  if (Number.isInteger(shape) && shape >= 0 && shape < SHAPES.length)
    setLook({ s: shape }, SHAPE_NAMES[shape]);
});
function addMark(m, id) {
  ripples.push({ ...m, id, time: performance.now() });
  if (ripples.length > 60) ripples.shift();
  announce(`${id === session.id ? "You" : shortName(id)} marked a point`);
}

// ------------------------------------------------------------------ session

const lookFor = (id) => looks.get(id) || defaultLook(id);
// Snake keys are an id prefix, and the default look is derived from that same
// prefix, so an unseen owner still gets the color their name carries.
function lookForKey(k) {
  if (k === key) return look;
  for (const [id, seen] of looks) if (snakeKey(id) === k) return seen;
  return defaultLook(k);
}
function describe(before, after) {
  if (before.mode !== after.mode) return `${MODE_NAMES[after.mode]} mode`;
  if (before.round !== after.round) return `Round ${after.round + 1}`;
  if (before.speed !== after.speed) return `${SPEED_NAMES[after.speed]} snake`;
  if (before.walls !== after.walls) return `${WALL_NAMES[after.walls]} edges`;
  return "";
}
function accept(incoming, id) {
  const merged = mergeSettings(settings, incoming);
  if (merged === settings) return;
  const note = describe(settings, merged);
  settings = merged;
  if (note && id !== session.id) shell.notice(`${shortName(id)} set ${note}`);
  refresh();
}
session.on("data", (m, id) => {
  if (!m || typeof m !== "object") return;
  if (m.t === "set") return accept(m.st, id);
  if (m.t === "state") {
    // Only the coordinator's board counts, except in the first moment after
    // joining, when this peer cannot yet know who that is.
    if (id !== session.leader && performance.now() - session.started > 1200)
      return;
    const next = unpackWorld(m.w);
    if (next) world = next;
    accept(m.st, id);
    return;
  }
  if (m.t !== "here" && m.t !== "mark") return;
  const seen = validLook(m);
  if (
    !seen ||
    !Number.isFinite(m.x) ||
    !Number.isFinite(m.y) ||
    m.x < 0 ||
    m.x > 1 ||
    m.y < 0 ||
    m.y > 1
  )
    return;
  looks.set(id, seen);
  if (m.t === "mark") return addMark(m, id);
  const old = pointers.get(id);
  pointers.set(id, { x: m.x, y: m.y, dx: old?.dx ?? m.x, dy: old?.dy ?? m.y });
  intents.set(id, { play: m.p === 1, dir: m.d });
});
session.on("members", refresh);
const heartbeat = setInterval(broadcast, 1000);
window.addEventListener("pagehide", () => clearInterval(heartbeat), {
  once: true,
});

// The coordinator owns the board: it reconciles who holds a snake, steps the
// simulation on the shared clock and snapshots the result. Every input is
// idempotent, so reconciling from the latest presence beat is enough.
function reconcile() {
  const wanted = new Set(playing ? [key] : []);
  for (const [id, intent] of intents) {
    if (!session.peers.has(id)) {
      intents.delete(id);
      continue;
    }
    if (intent.play) wanted.add(snakeKey(id));
  }
  for (const k of Object.keys(world.snakes))
    if (!wanted.has(k)) leaveSnake(world, k);
  for (const k of [...wanted].sort()) joinSnake(world, k);
  if (playing && want !== null) queueTurn(world, key, want);
  for (const [id, intent] of intents)
    if (intent.dir !== undefined) queueTurn(world, snakeKey(id), intent.dir);
}
function simulate(now) {
  const leader = session.isLeader && now - session.started > 1200;
  const snapshot = () =>
    session.send({ t: "state", w: packWorld(world), st: settings });
  if (wasLeader && !leader && session.connected) snapshot();
  wasLeader = leader;
  if (!leader || !session.connected) return;
  if (world.round !== settings.round) {
    world = worldFor(session.room, settings.round);
    tickAt = now;
  }
  reconcile();
  if (settings.mode === "snake" && now - tickAt >= SPEEDS[settings.speed]) {
    tickAt = now;
    stepSnake(world, settings.walls);
    echoAt = now;
    snapshot();
  } else if (now - echoAt >= 1000) {
    // Between rounds the same message keeps settings and late arrivals in step.
    echoAt = now;
    snapshot();
  }
}

// ------------------------------------------------------------------- render

function boardRect() {
  const scale = Math.max(
    2,
    Math.min(
      Math.floor((width - 20) / GRID_W),
      Math.floor((height - 20) / GRID_H),
    ),
  );
  return {
    scale,
    x: Math.round((width - scale * GRID_W) / 2),
    y: Math.round((height - scale * GRID_H) / 2),
  };
}
function pointer(x, y, id, self) {
  const { c, s } = self ? look : lookFor(id);
  ctx.save();
  ctx.translate(x * width, y * height);
  drawShape(ctx, SHAPES[s], 0, 0, 13, PALETTE[c], SHAPES[s] !== "arrow");
  const label = self ? "You" : shortName(id);
  ctx.font = "12px system-ui";
  const tw = ctx.measureText(label).width;
  ctx.fillStyle = "#1c2c42";
  ctx.beginPath();
  ctx.roundRect(16, 16, tw + 16, 25, 7);
  ctx.fill();
  ctx.fillStyle = PALETTE[c];
  ctx.fillText(label, 24, 33);
  ctx.restore();
}
function drawPointers(dt, alpha) {
  ctx.globalAlpha = alpha;
  for (const [id, p] of pointers) {
    if (!session.peers.has(id)) {
      pointers.delete(id);
      looks.delete(id);
      intents.delete(id);
      continue;
    }
    const t = reduced ? 1 : 1 - Math.exp(-20 * dt);
    p.dx += (p.x - p.dx) * t;
    p.dy += (p.y - p.dy) * t;
    pointer(p.dx, p.dy, id, false);
  }
  pointer(local.x, local.y, session.id, true);
  ctx.globalAlpha = 1;
}
function drawMarks(now) {
  for (let i = ripples.length - 1; i >= 0; i--) {
    const p = ripples[i],
      age = (now - p.time) / 1200;
    if (age > 1) {
      ripples.splice(i, 1);
      continue;
    }
    ctx.globalAlpha = 1 - age;
    drawShape(
      ctx,
      SHAPES[p.s],
      p.x * width,
      p.y * height,
      reduced ? 20 : 12 + age * 52,
      PALETTE[p.c],
    );
  }
  ctx.globalAlpha = 1;
}
function drawPresence(now, dt) {
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
  drawPointers(dt, 1);
  drawMarks(now);
}
function drawSnake(now, dt) {
  const { scale, x: ox, y: oy } = boardRect();
  const cell = (c) => [ox + cellX(c) * scale, oy + cellY(c) * scale];
  ctx.fillStyle = "#0d1523";
  ctx.fillRect(ox, oy, scale * GRID_W, scale * GRID_H);
  ctx.strokeStyle = "#1b2740";
  ctx.lineWidth = 1;
  for (let x = 0; x <= GRID_W; x += 4) {
    ctx.beginPath();
    ctx.moveTo(ox + x * scale, oy);
    ctx.lineTo(ox + x * scale, oy + scale * GRID_H);
    ctx.stroke();
  }
  for (let y = 0; y <= GRID_H; y += 4) {
    ctx.beginPath();
    ctx.moveTo(ox, oy + y * scale);
    ctx.lineTo(ox + scale * GRID_W, oy + y * scale);
    ctx.stroke();
  }
  ctx.strokeStyle = settings.walls === "solid" ? "#6be4bc" : "#344363";
  ctx.lineWidth = 2;
  ctx.strokeRect(ox - 1, oy - 1, scale * GRID_W + 2, scale * GRID_H + 2);
  // Food is a pellet rather than one of the shapes a guest can pick, so it is
  // never mistaken for somebody's head.
  const pulse = reduced ? 1 : 0.86 + Math.sin(now / 260) * 0.14;
  for (const food of world.food) {
    const [fx, fy] = cell(food),
      r = scale * 0.3 * pulse;
    ctx.fillStyle = "#6be4bc";
    ctx.beginPath();
    ctx.arc(fx + scale / 2, fy + scale / 2, r, 0, Math.PI * 2);
    ctx.fill();
    ctx.strokeStyle = "#6be4bc55";
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(fx + scale / 2, fy + scale / 2, r + 3, 0, Math.PI * 2);
    ctx.stroke();
  }
  const keys = Object.keys(world.snakes).sort();
  for (const k of keys) {
    const snake = world.snakes[k],
      { c, s } = lookForKey(k),
      color = PALETTE[c];
    snake.body.forEach((part, i) => {
      if (!i) return;
      const [px, py] = cell(part);
      ctx.globalAlpha = Math.max(0.3, 0.9 - (i / snake.body.length) * 0.5);
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.roundRect(px + 1, py + 1, scale - 2, scale - 2, 3);
      ctx.fill();
    });
    ctx.globalAlpha = 1;
    // The head is a full cell wearing its owner's shape, so who is who reads
    // at a glance without shrinking the part that matters.
    const [hx, hy] = cell(snake.body[0]);
    ctx.fillStyle = color;
    ctx.beginPath();
    ctx.roundRect(hx, hy, scale, scale, 5);
    ctx.fill();
    drawShape(
      ctx,
      SHAPES[s],
      hx + scale / 2,
      hy + scale / 2,
      scale * 0.3,
      "#0c1018",
    );
  }
  ctx.font = "12px system-ui";
  keys.forEach((k, row) => {
    const snake = world.snakes[k],
      { c } = lookForKey(k);
    ctx.fillStyle = PALETTE[c];
    ctx.fillRect(ox + 8, oy + 12 + row * 17, 8, 8);
    ctx.fillStyle = k === key ? "#edf3fd" : "#a1aec3";
    ctx.fillText(
      `${k === key ? "You" : shortName(k)} · ${snake.score} · ${snake.body.length} long`,
      ox + 22,
      oy + 20 + row * 17,
    );
  });
  if (!keys.length) {
    ctx.textAlign = "center";
    ctx.font = "600 22px system-ui";
    ctx.fillStyle = "#edf3fd";
    ctx.fillText("Take a snake to start the round.", width / 2, height / 2);
    ctx.font = "14px system-ui";
    ctx.fillStyle = "#a1aec3";
    ctx.fillText(
      "Everyone here shares one board and one clock.",
      width / 2,
      height / 2 + 26,
    );
    ctx.textAlign = "left";
  }
  drawPointers(dt, 0.32);
  drawMarks(now);
}
function draw(now) {
  const dt = Math.min((now - lastFrame) / 1000, 0.05);
  lastFrame = now;
  simulate(now);
  ctx.clearRect(0, 0, width, height);
  if (settings.mode === "snake") drawSnake(now, dt);
  else drawPresence(now, dt);
  const line = statusLine();
  if (line !== activity) {
    activity = line;
    document.querySelector("#activity").textContent = line;
  }
  requestAnimationFrame(draw);
}
refresh();
requestAnimationFrame(draw);
session.connect().catch((err) => {
  // The reason goes to the console as well as the status line. A bare
  // `.catch` here made every failure look identical from outside the page,
  // which on Android means nothing reaches logcat at all - the one place
  // you can look when the portal is not cooperating.
  console.error("[demo] session connect failed:", err);
  shell.setStatus("Open in DoubleSlash through a connected supernode", "error");
});
