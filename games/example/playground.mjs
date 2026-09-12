import { COLORS } from "../../web-sdk/demo-shell.mjs";

// Playground state: the appearance a guest picks, the settings every shared
// button writes, and the snake simulation. Pure functions only - no DOM and no
// transport - so the same snapshot stepped with the same settings gives every
// device the same world, and a coordinator handoff continues the round instead
// of restarting it.

export const PALETTE = [...COLORS, "#edf3fd"];
export const SHAPES = ["arrow", "dot", "ring", "square", "triangle", "star"];
export const SHAPE_NAMES = [
  "Arrow",
  "Dot",
  "Ring",
  "Square",
  "Triangle",
  "Star",
];
// Tick lengths in milliseconds; the index is what travels on the wire.
export const SPEEDS = [220, 150, 95];
export const SPEED_NAMES = ["Slow", "Steady", "Quick"];
export const MODES = ["presence", "snake"];
export const WALLS = ["wrap", "solid"];

export const GRID_W = 32,
  GRID_H = 24,
  CELLS = GRID_W * GRID_H;
export const MAX_SNAKES = 6,
  MAX_LEN = 40,
  START_GROW = 3,
  FOOD = 3;
// Up, right, down, left. The index is the direction that travels on the wire.
export const DIRS = [
  [0, -1],
  [1, 0],
  [0, 1],
  [-1, 0],
];
export const KEY_LEN = 8;
export const snakeKey = (id) => id.slice(0, KEY_LEN);
export const opposite = (dir) => (dir + 2) % 4;
export const cellX = (cell) => cell % GRID_W;
export const cellY = (cell) => Math.floor(cell / GRID_W);

export function hash32(text) {
  let h = 2166136261;
  for (let i = 0; i < text.length; i++)
    h = Math.imul(h ^ text.charCodeAt(i), 16777619);
  return h >>> 0;
}
// A stateless generator: the same (seed, counter) always gives the same value,
// so a world can be replayed or adopted by another peer without carrying any
// generator state in the snapshot.
export function mix32(seed, n) {
  let h = Math.imul((seed ^ (n + 0x9e3779b9)) >>> 0, 0x85ebca6b);
  h ^= h >>> 13;
  h = Math.imul(h, 0xc2b2ae35);
  h ^= h >>> 16;
  return h >>> 0;
}
// The same room and round number always lay out the same board.
export const seedFor = (room, round) => hash32(`${room}#${round}`);

// ---------------------------------------------------------------- appearance

// A guest starts on the color the session panel already shows beside their
// name, and on a shape drawn from the same id, so a room of strangers is
// distinguishable before anyone touches the toolbar.
export function defaultLook(id) {
  const n = parseInt(id.slice(0, 6), 16) || 0;
  return { c: n % COLORS.length, s: mix32(n, 7) % SHAPES.length };
}
// Looks travel as indexes, never as color strings: an unknown index is
// dropped instead of reaching a canvas fill style.
export function validLook(look) {
  if (!look || typeof look !== "object") return null;
  const { c, s } = look;
  if (!Number.isInteger(c) || c < 0 || c >= PALETTE.length) return null;
  if (!Number.isInteger(s) || s < 0 || s >= SHAPES.length) return null;
  return { c, s };
}

export function starPoints(points = 5, inner = 0.46) {
  const pts = [];
  for (let i = 0; i < points * 2; i++) {
    const a = (Math.PI * i) / points - Math.PI / 2,
      r = i % 2 ? inner : 1;
    pts.push([Math.cos(a) * r, Math.sin(a) * r]);
  }
  return pts;
}
// Unit outlines, y down, spanning about two units so every shape draws at the
// same weight. Round shapes have no outline and are drawn as an arc. The arrow
// is anchored at its tip so it can point at a pixel.
export function shapeOutline(shape) {
  switch (shape) {
    case "arrow":
      return [
        [0, 0],
        [0, 1.62],
        [0.44, 1.18],
        [0.88, 2],
        [1.26, 1.78],
        [0.82, 1.04],
        [1.48, 1.04],
      ];
    case "square":
      return [
        [-0.8, -0.8],
        [0.8, -0.8],
        [0.8, 0.8],
        [-0.8, 0.8],
      ];
    case "triangle":
      return [
        [0, -1],
        [0.92, 0.72],
        [-0.92, 0.72],
      ];
    case "star":
      return starPoints();
    default:
      return null;
  }
}
export const isStroked = (shape) => shape === "ring";
export function outlineBounds(outline) {
  const xs = outline.map((p) => p[0]),
    ys = outline.map((p) => p[1]);
  return {
    minX: Math.min(...xs),
    maxX: Math.max(...xs),
    minY: Math.min(...ys),
    maxY: Math.max(...ys),
  };
}
// How far to shift a shape so it sits on its own center. Centered outlines get
// ~0 and the tip-anchored arrow gets a real offset, so one renderer draws a
// pointer, a marker and a snake head from the same outline.
export function centerOffset(shape) {
  const outline = shapeOutline(shape);
  if (!outline) return [0, 0];
  const b = outlineBounds(outline);
  return [-(b.minX + b.maxX) / 2, -(b.minY + b.maxY) / 2];
}

// ------------------------------------------------------------- room settings

// Every shared button writes here. A Lamport clock with an id tiebreak means
// two guests pressing different buttons in the same moment converge on one
// answer on every device instead of flapping between two.
export function newSettings() {
  return {
    mode: "presence",
    speed: 1,
    walls: "wrap",
    round: 0,
    clock: 0,
    by: "",
  };
}
export function validSettings(s) {
  return !!(
    s &&
    MODES.includes(s.mode) &&
    WALLS.includes(s.walls) &&
    Number.isInteger(s.speed) &&
    s.speed >= 0 &&
    s.speed < SPEEDS.length &&
    Number.isInteger(s.round) &&
    s.round >= 0 &&
    s.round <= 100000 &&
    Number.isSafeInteger(s.clock) &&
    s.clock >= 0 &&
    s.clock <= 1e9 &&
    typeof s.by === "string" &&
    /^[a-f0-9]{0,24}$/.test(s.by)
  );
}
export const newer = (a, b) =>
  a.clock > b.clock || (a.clock === b.clock && a.by > b.by);
export function mergeSettings(a, b) {
  if (!validSettings(b)) return a;
  if (!validSettings(a)) return b;
  return newer(b, a) ? b : a;
}
// What a button press produces. Sending it once is enough: the coordinator
// repeats the merged result, so a dropped datagram costs a press, not
// agreement.
export function change(settings, patch, by) {
  return { ...settings, ...patch, clock: settings.clock + 1, by };
}

// -------------------------------------------------------------------- snake

export function newSnakeWorld(seed, round = 0) {
  const world = {
    seed: seed >>> 0,
    round,
    tick: 0,
    spawns: 0,
    food: [],
    snakes: {},
  };
  refill(world);
  return world;
}
export const worldFor = (room, round) =>
  newSnakeWorld(seedFor(room, round), round);

function occupied(world) {
  const cells = new Set(world.food);
  for (const key of Object.keys(world.snakes))
    for (const cell of world.snakes[key].body) cells.add(cell);
  return cells;
}
// Linear probing from a hashed cell: deterministic, and it always terminates
// on a 768 cell board that six capped snakes cannot fill.
function freeCell(world, n) {
  const taken = occupied(world),
    start = mix32(world.seed, n) % CELLS;
  for (let i = 0; i < CELLS; i++) {
    const cell = (start + i) % CELLS;
    if (!taken.has(cell)) return cell;
  }
  return start;
}
function refill(world) {
  while (world.food.length < FOOD)
    world.food.push(freeCell(world, world.spawns++));
}
// A new snake is one cell long and grows into its length, so it never has to
// be placed onto cells a neighbour is already using.
function spawn(world, key) {
  const n = hash32(`${key}:${world.seed}:${world.tick}`),
    dir = n % DIRS.length;
  return {
    body: [freeCell(world, n)],
    dir,
    want: dir,
    grow: START_GROW,
    score: 0,
    deaths: 0,
  };
}
export function joinSnake(world, key) {
  if (world.snakes[key]) return false;
  if (Object.keys(world.snakes).length >= MAX_SNAKES) return false;
  world.snakes[key] = spawn(world, key);
  return true;
}
export function leaveSnake(world, key) {
  return delete world.snakes[key];
}
// Turns are queued, not applied: the tick boundary is the only place direction
// changes, so devices running different frame rates still agree. Reversing
// onto your own neck is ignored rather than fatal.
export function queueTurn(world, key, dir) {
  const snake = world.snakes[key];
  if (!snake || !Number.isInteger(dir) || dir < 0 || dir >= DIRS.length)
    return false;
  if (snake.body.length > 1 && dir === opposite(snake.dir)) return false;
  snake.want = dir;
  return true;
}

export function stepSnake(world, walls = "wrap") {
  const keys = Object.keys(world.snakes).sort();
  const heads = new Map(),
    bodies = new Map();
  for (const key of keys) {
    const snake = world.snakes[key];
    if (snake.body.length < 2 || snake.want !== opposite(snake.dir))
      snake.dir = snake.want;
    const head = snake.body[0];
    let x = cellX(head) + DIRS[snake.dir][0],
      y = cellY(head) + DIRS[snake.dir][1];
    const off = x < 0 || x >= GRID_W || y < 0 || y >= GRID_H;
    x = (x + GRID_W) % GRID_W;
    y = (y + GRID_H) % GRID_H;
    heads.set(key, { cell: y * GRID_W + x, wall: off && walls === "solid" });
  }
  for (const key of keys) {
    const snake = world.snakes[key],
      { cell } = heads.get(key);
    const body = [cell, ...snake.body];
    const grows = world.food.includes(cell) || snake.grow > 0;
    if (!grows || body.length > MAX_LEN) body.pop();
    bodies.set(key, body);
  }
  // Collisions are judged after every body has moved, so the outcome does not
  // depend on the order snakes are visited: a head sharing a cell with any
  // body - two heads meeting included - is a collision for that head alone.
  const used = new Map();
  for (const body of bodies.values())
    for (const cell of body) used.set(cell, (used.get(cell) || 0) + 1);
  const dead = [];
  for (const key of keys) {
    const { cell, wall } = heads.get(key);
    if (wall || used.get(cell) > 1) dead.push(key);
  }
  for (const key of keys) {
    if (dead.includes(key)) continue;
    const snake = world.snakes[key],
      { cell } = heads.get(key);
    snake.body = bodies.get(key);
    if (world.food.includes(cell)) {
      world.food = world.food.filter((c) => c !== cell);
      snake.score = Math.min(snake.score + 1, 9999);
      snake.grow = Math.min(snake.grow + 2, MAX_LEN);
    } else if (snake.grow > 0) snake.grow--;
  }
  for (const key of dead) {
    const deaths = Math.min(world.snakes[key].deaths + 1, 9999);
    world.snakes[key] = { ...spawn(world, key), deaths };
  }
  refill(world);
  world.tick++;
  return dead;
}

// --------------------------------------------------------------- wire format

// Two base36 characters per cell keeps a full six snake board inside the
// session's 1,100 byte envelope, which a snapshot has to fit every tick.
export function encodeCells(cells) {
  return cells.map((c) => c.toString(36).padStart(2, "0")).join("");
}
export function decodeCells(text) {
  if (typeof text !== "string" || !text.length || text.length % 2) return null;
  if (text.length > MAX_LEN * 2 || !/^[0-9a-z]+$/.test(text)) return null;
  const cells = [];
  for (let i = 0; i < text.length; i += 2) {
    const cell = parseInt(text.slice(i, i + 2), 36);
    if (cell >= CELLS) return null;
    cells.push(cell);
  }
  return cells;
}
export function packWorld(world) {
  return {
    e: world.seed,
    r: world.round,
    t: world.tick,
    n: world.spawns,
    f: [...world.food],
    s: Object.keys(world.snakes)
      .sort()
      .map((key) => {
        const s = world.snakes[key];
        return [key, encodeCells(s.body), s.dir, s.grow, s.score, s.deaths];
      }),
  };
}
const KEY_PATTERN = new RegExp(`^[a-f0-9]{${KEY_LEN}}$`);
export function unpackWorld(packed) {
  const int = (v, max) => Number.isInteger(v) && v >= 0 && v <= max;
  if (
    !packed ||
    typeof packed !== "object" ||
    !int(packed.e, 0xffffffff) ||
    !int(packed.r, 100000) ||
    !int(packed.t, 1e9) ||
    !int(packed.n, 1e9) ||
    !Array.isArray(packed.f) ||
    packed.f.length > FOOD ||
    !packed.f.every((c) => int(c, CELLS - 1)) ||
    !Array.isArray(packed.s) ||
    packed.s.length > MAX_SNAKES
  )
    return null;
  const snakes = {};
  for (const entry of packed.s) {
    if (!Array.isArray(entry) || entry.length !== 6) return null;
    const [key, body, dir, grow, score, deaths] = entry;
    const cells = decodeCells(body);
    if (
      typeof key !== "string" ||
      !KEY_PATTERN.test(key) ||
      snakes[key] ||
      !cells ||
      !int(dir, DIRS.length - 1) ||
      !int(grow, MAX_LEN) ||
      !int(score, 9999) ||
      !int(deaths, 9999)
    )
      return null;
    snakes[key] = { body: cells, dir, want: dir, grow, score, deaths };
  }
  return {
    seed: packed.e,
    round: packed.r,
    tick: packed.t,
    spawns: packed.n,
    food: [...packed.f],
    snakes,
  };
}
