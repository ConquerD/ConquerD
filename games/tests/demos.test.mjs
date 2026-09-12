import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  DemoSession,
  decodePacket,
  PEER_TIMEOUT,
} from "../../web-sdk/demo-session.mjs";
import { peerColor } from "../../web-sdk/demo-shell.mjs";
import {
  newWorld,
  step,
  command,
  validWorld,
  sampleBall,
} from "../brick-breaker/world.mjs";
import { Board, LIMIT } from "../shared-drawing/board.mjs";
import {
  MAX_SNAKES,
  MAX_LEN,
  GRID_W,
  GRID_H,
  PALETTE,
  SHAPES,
  newSettings,
  mergeSettings,
  change,
  validSettings,
  validLook,
  defaultLook,
  centerOffset,
  worldFor,
  newSnakeWorld,
  joinSnake,
  queueTurn,
  stepSnake,
  packWorld,
  unpackWorld,
} from "../example/playground.mjs";
import { ConquerdClient } from "../../web-sdk/conquerd.mjs";

const idA = "a".repeat(24),
  idB = "b".repeat(24);
class Client {
  handlers = new Map();
  sent = [];
  on(event, fn) {
    this.handlers.set(event, fn);
  }
  async connect() {}
  async sendDatagram(feature, bytes) {
    this.sent.push(bytes);
    this.other?.handlers.get("datagram")?.(feature, bytes);
  }
  disconnect() {}
}
const flush = () => new Promise((resolve) => setImmediate(resolve));

test("session membership converges, readiness propagates, coordinator survives timeout", async () => {
  let clock = 100;
  const ca = new Client(),
    cb = new Client();
  ca.other = cb;
  cb.other = ca;
  const a = new DemoSession("test", {
    id: idA,
    room: "room",
    client: ca,
    now: () => clock,
  });
  const b = new DemoSession("test", {
    id: idB,
    room: "room",
    client: cb,
    now: () => clock,
  });
  try {
    await a.connect();
    await b.connect();
    await flush();
    assert.equal(a.peers.size, 1);
    assert.equal(b.peers.size, 1);
    assert.equal(a.leader, idA);
    assert.equal(b.leader, idA);
    a.setReady(true);
    await flush();
    assert.equal(b.peers.get(idA).ready, true);
    clock = 2100;
    a.heartbeat();
    await flush();
    assert.equal(a.peers.get(idB).rtt, 0);
    ca.other = null;
    cb.other = null;
    clock += PEER_TIMEOUT + 1;
    b.heartbeat();
    assert.equal(b.leader, idB);
    assert.equal(b.peers.size, 0);
  } finally {
    a.disconnect();
    b.disconnect();
  }
});

test("bad, oversized, cross-app and duplicate packets do not reach app logic", async () => {
  const c = new Client(),
    s = new DemoSession("canvas", { id: idA, room: "x", client: c });
  let seen = 0;
  s.on("data", () => seen++);
  try {
    await s.connect();
    const encode = (p) => new TextEncoder().encode(JSON.stringify(p));
    const p = {
      v: 1,
      app: "canvas",
      id: idB,
      seq: 1,
      kind: "data",
      body: { t: "x" },
    };
    for (const bad of [
      new Uint8Array(1101),
      encode({ ...p, app: "brick" }),
      encode({ ...p, id: "<script>" }),
      encode({ ...p, seq: -1 }),
    ]) {
      assert.equal(decodePacket(bad, "canvas"), null);
      s.receive(bad);
    }
    s.receive(encode(p));
    s.receive(encode(p));
    assert.equal(seen, 1);
    assert.equal(s.peers.size, 1);
  } finally {
    s.disconnect();
  }
});

test("bridge backpressure bounds outstanding sends and reports failures", async () => {
  const c = new Client(),
    s = new DemoSession("test", { id: idA, room: "r", client: c });
  await s.connect();
  await flush();
  const release = [];
  c.sendDatagram = () => new Promise((resolve) => release.push(resolve));
  try {
    for (let i = 0; i < 12; i++) s.send({ x: i });
    assert.equal(s.pending, 4);
    assert.equal(s.stats.dropped, 8);
    release.forEach((r) => r());
    await flush();
    assert.equal(s.pending, 0);
    c.sendDatagram = async () => {
      throw new Error("offline");
    };
    assert.equal(await s.send({ x: 0 }), false);
    assert.equal(s.stats.errors, 1);
  } finally {
    s.disconnect();
  }
});

test("ball velocity survives snapshots, commands are idempotent, terminal state transfers", () => {
  const s = newWorld();
  command(s, "launch", "one");
  step(s, 1 / 120, [400]);
  assert.ok(s.vy < -2);
  assert.ok(validWorld(JSON.parse(JSON.stringify(s))));
  s.score = 60;
  command(s, "reset", "two");
  s.score = 10;
  assert.equal(command(s, "reset", "two"), false);
  assert.equal(s.score, 10);
  s.phase = "playing";
  s.lives = 1;
  s.y = 606;
  s.vy = 400;
  step(s, 1 / 120, []);
  assert.equal(s.phase, "over");
  assert.equal(s.lives, 0);
  assert.ok(validWorld(s));
  assert.equal(validWorld({ ...s, vx: Infinity }), false);
  assert.equal(validWorld({ ...s, bricks: [1] }), false);
});

test("fixed steps give the same world at different render rates", () => {
  const run = (fps) => {
    const s = newWorld();
    command(s, "launch", "a");
    let acc = 0;
    for (let i = 0; i < fps * 2; i++) {
      acc += 1 / fps;
      while (acc >= 1 / 120 - 1e-10) {
        step(s, 1 / 120, [400]);
        acc -= 1 / 120;
      }
    }
    return s;
  };
  assert.deepEqual(run(30), run(144));
});

test("interpolation smooths remote motion and never extrapolates indefinitely", () => {
  const a = {
    ...newWorld(),
    phase: "playing",
    x: 100,
    y: 200,
    time: 0,
    vx: 200,
    vy: 0,
  };
  const b = { ...a, x: 120, time: 100 };
  assert.equal(sampleBall([a, b], 50).x, 110);
  assert.equal(sampleBall([a, b], 10000).x, 130);
  assert.equal(sampleBall([a, { ...b, round: 2, x: 400 }], 50).x, 400);
});

const op = (n, kind = "ink") => ({
  id: `${idA}:${n}`,
  clock: n,
  kind,
  x1: 0.1,
  y1: 0.1,
  x2: 0.2,
  y2: 0.2,
  width: 4,
  color: "#87b7ff",
});
test("drawing replay converges despite reorder/duplicates and clear prevents resurrection", () => {
  const a = new Board(),
    b = new Board();
  const ops = [op(1), op(2, "erase"), op(3, "clear"), op(4)];
  ops.forEach((o) => a.add(o));
  [...ops].reverse().forEach((o) => b.add(o));
  ops.forEach((o) => b.add(o));
  assert.deepEqual(a.sorted(), b.sorted());
  assert.equal(a.digest(), b.digest());
  assert.equal(a.ops.size, 2);
  assert.equal(a.add({ ...op(5), x1: NaN }), false);
  assert.equal(a.add({ ...op(5), color: "url(javascript:alert(1))" }), false);
});
test("drawing history is bounded and can be cleared when full", () => {
  const b = new Board();
  for (let i = 1; i <= LIMIT + 10; i++) b.add(op(i));
  assert.equal(b.ops.size, LIMIT);
  assert.equal(b.add(op(LIMIT + 11, "clear")), true);
  assert.equal(b.ops.size, 1);
});

test("full boards converge regardless of operation arrival order", () => {
  const a = new Board(),
    b = new Board();
  const ops = Array.from({ length: LIMIT + 10 }, (_, i) => op(i + 2));
  ops.forEach((o) => a.add(o));
  ops.reverse().forEach((o) => b.add(o));
  assert.equal(a.digest(), b.digest());
  // A very old clear is still a tombstone and cannot exceed the memory cap.
  a.add(op(1, "clear"));
  b.add(op(1, "clear"));
  assert.equal(a.ops.size, LIMIT);
  assert.equal(a.digest(), b.digest());
});

const at = (x, y) => y * GRID_W + x;
const solo = (seed, body, dir, grow = 0) => {
  const world = newSnakeWorld(seed);
  world.food = [];
  world.snakes = {
    "00000001": { body, dir, want: dir, grow, score: 0, deaths: 0 },
  };
  return world;
};

test("room settings converge on the same answer whoever pressed first", () => {
  const base = newSettings();
  const mode = change(base, { mode: "snake" }, "aaaa"),
    speed = change(base, { speed: 2 }, "bbbb");
  // Concurrent presses: both devices resolve the tie the same way, and the
  // loser is dropped rather than applied on one side only.
  assert.deepEqual(mergeSettings(mode, speed), mergeSettings(speed, mode));
  assert.equal(mergeSettings(mode, speed), speed);
  const later = change(speed, { mode: "presence" }, "aaaa");
  assert.equal(mergeSettings(mode, later), later);
  assert.equal(mergeSettings(later, mode), later);
  assert.equal(mergeSettings(base, { ...base, mode: "chaos", clock: 9 }), base);
  assert.equal(validSettings({ ...base, by: "<script>" }), false);
  assert.equal(validSettings({ ...base, speed: 9 }), false);
  assert.equal(validSettings({ ...base, walls: "none" }), false);
});

test("appearance travels as bounded indexes and defaults match the session panel", () => {
  assert.deepEqual(validLook({ c: 0, s: 0 }), { c: 0, s: 0 });
  assert.equal(validLook({ c: PALETTE.length, s: 0 }), null);
  assert.equal(validLook({ c: 0, s: "star" }), null);
  assert.equal(validLook({ c: 1.5, s: 0 }), null);
  assert.equal(validLook({ c: 0, s: 0, extra: "url(javascript:0)" }).s, 0);
  const look = defaultLook(idA);
  assert.equal(PALETTE[look.c], peerColor(idA));
  assert.ok(look.s >= 0 && look.s < SHAPES.length);
  // A snake is keyed by an id prefix; the fallback look must not shift with it.
  assert.deepEqual(defaultLook(idA.slice(0, 8)), look);
  assert.ok(Math.abs(centerOffset("arrow")[1]) > 0.1);
  assert.ok(Math.abs(centerOffset("square")[0]) < 1e-9);
});

test("the same room and round deal the same board on every device", () => {
  const layout = (room, round) => JSON.stringify(worldFor(room, round).food);
  assert.equal(layout("lounge", 2), layout("lounge", 2));
  assert.notEqual(layout("lounge", 2), layout("lounge", 3));
  assert.notEqual(layout("lounge", 2), layout("other", 2));
  const run = (order) => {
    const world = worldFor("lounge", 1);
    order.forEach((key) => joinSnake(world, key));
    for (let i = 0; i < 30; i++) {
      queueTurn(world, "aa11bb22", i % 2 ? 1 : 2);
      stepSnake(world, "wrap");
    }
    return JSON.stringify(packWorld(world));
  };
  assert.equal(run(["aa11bb22", "cc33dd44"]), run(["cc33dd44", "aa11bb22"]));
  const world = worldFor("lounge", 1);
  for (let i = 0; i < MAX_SNAKES + 2; i++)
    assert.equal(joinSnake(world, `0000000${i}`), i < MAX_SNAKES);
  assert.equal(joinSnake(world, "00000000"), false);
});

test("snake collisions resolve simultaneously and edges follow the setting", () => {
  const loop = [at(3, 0), at(4, 0), at(4, 1), at(3, 1)];
  // Chasing a tail that moves out of the way is fine; growing into it is not.
  assert.deepEqual(stepSnake(solo(7, loop, 2), "wrap"), []);
  assert.deepEqual(stepSnake(solo(7, loop, 2, 1), "wrap"), ["00000001"]);
  assert.deepEqual(stepSnake(solo(7, loop, 1), "wrap"), ["00000001"]);
  const edge = (walls) => stepSnake(solo(9, [at(0, 0)], 0), walls);
  assert.deepEqual(edge("solid"), ["00000001"]);
  assert.deepEqual(edge("wrap"), []);
  const wrapped = solo(9, [at(0, 0)], 0);
  stepSnake(wrapped, "wrap");
  assert.equal(wrapped.snakes["00000001"].body[0], at(0, GRID_H - 1));
  // Two heads meeting kill each other whichever order the snakes are visited.
  const pair = solo(11, [at(5, 5)], 1);
  pair.snakes["00000002"] = {
    body: [at(7, 5)],
    dir: 3,
    want: 3,
    grow: 0,
    score: 0,
    deaths: 0,
  };
  assert.deepEqual(stepSnake(pair, "wrap").sort(), ["00000001", "00000002"]);
  assert.equal(pair.snakes["00000001"].deaths, 1);
  assert.equal(pair.snakes["00000001"].body.length, 1);
  const eating = solo(13, [at(5, 5)], 1);
  eating.food = [at(6, 5)];
  stepSnake(eating, "wrap");
  assert.equal(eating.snakes["00000001"].score, 1);
  assert.equal(eating.food.length, 3);
  assert.ok(!eating.food.includes(at(6, 5)));
  const turning = solo(17, [at(5, 5), at(4, 5)], 1);
  assert.equal(queueTurn(turning, "00000001", 3), false);
  assert.equal(queueTurn(turning, "00000001", 0), true);
  assert.equal(queueTurn(turning, "nobody00", 0), false);
  assert.equal(queueTurn(turning, "00000001", 9), false);
});

test("a full snake board survives a snapshot and fits one session envelope", async () => {
  const world = worldFor("lounge", 4);
  for (let i = 0; i < MAX_SNAKES; i++) joinSnake(world, `0000000${i}`);
  for (const key of Object.keys(world.snakes))
    Object.assign(world.snakes[key], {
      body: Array.from({ length: MAX_LEN }, (_, n) => n),
      score: 9999,
      deaths: 9999,
    });
  const packed = packWorld(world);
  assert.deepEqual(packWorld(unpackWorld(packed)), packed);
  const bad = (mutate) => {
    const copy = JSON.parse(JSON.stringify(packed));
    mutate(copy);
    return unpackWorld(copy);
  };
  assert.equal(
    bad((p) => (p.s[0][0] = "<script>")),
    null,
  );
  assert.equal(
    bad((p) => (p.s[0][1] = "zzzz")),
    null,
  );
  assert.equal(
    bad((p) => (p.s[0][1] = "0".repeat((MAX_LEN + 1) * 2))),
    null,
  );
  assert.equal(
    bad((p) => (p.s[0][2] = 7)),
    null,
  );
  assert.equal(
    bad((p) => p.s.push(p.s[0])),
    null,
  );
  assert.equal(
    bad((p) => (p.f = [-1])),
    null,
  );
  assert.equal(unpackWorld(null), null);
  // The worst case board still has to leave the coordinator every tick.
  const client = new Client(),
    session = new DemoSession("presence", {
      id: idA,
      room: "r",
      client,
    });
  try {
    await session.connect();
    await flush();
    const settings = change(newSettings(), { mode: "snake" }, idA);
    assert.equal(
      await session.send({ t: "state", w: packed, st: settings }),
      true,
    );
    assert.equal(session.stats.errors, 0);
  } finally {
    session.disconnect();
  }
});

test("SDK polls never overlap and discards a poll completed after disconnect", async () => {
  let polls = 0,
    resolvePoll,
    delivered = 0;
  globalThis.window = {
    conquerd: {
      ready: Promise.resolve({
        myPeerId: "fixture",
        openChannel: async () => ({ ok: true }),
        pollDatagrams: () => {
          polls++;
          return new Promise((r) => (resolvePoll = r));
        },
        closeChannel: async () => {},
      }),
    },
  };
  const c = new ConquerdClient({ features: ["game.relay.v1"], room: "x" });
  c.on("datagram", () => delivered++);
  try {
    await c.connect();
    await new Promise((r) => setTimeout(r, 160));
    assert.equal(polls, 1);
    c.disconnect();
    resolvePoll({ frames: ["eA"] });
    await flush();
    assert.equal(delivered, 0);
  } finally {
    c.disconnect();
    delete globalThis.window;
  }
});

test("embedded supernode assets match the editable examples", async () => {
  const pairs = [
    ["example/index.html", "games_example_index.html"],
    ["example/game.js", "games_example_game.js"],
    ["example/playground.mjs", "games_example_playground.mjs"],
    ["brick-breaker/index.html", "games_brick_breaker_index.html"],
    ["brick-breaker/brick-breaker.js", "games_brick_breaker_brick_breaker.js"],
    ["brick-breaker/world.mjs", "games_brick_breaker_world.mjs"],
    ["shared-drawing/index.html", "games_shared_drawing_index.html"],
    ["shared-drawing/drawing.js", "games_shared_drawing_drawing.js"],
    ["shared-drawing/board.mjs", "games_shared_drawing_board.mjs"],
    ["../web-sdk/conquerd.mjs", "web_sdk_conquerd.mjs"],
    ["../web-sdk/demo-session.mjs", "web_sdk_demo_session.mjs"],
    ["../web-sdk/demo-shell.mjs", "web_sdk_demo_shell.mjs"],
    ["../web-sdk/demo-shell.css", "web_sdk_demo_shell.css"],
  ];
  for (const [src, dst] of pairs)
    assert.equal(
      await readFile(new URL(`../${src}`, import.meta.url), "utf8"),
      await readFile(
        new URL(
          `../../rust/conquerd-supernode/templates/${dst}`,
          import.meta.url,
        ),
        "utf8",
      ),
      src,
    );
});
