import { test } from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import {
  DemoSession,
  decodePacket,
  PEER_TIMEOUT,
} from "../../web-sdk/demo-session.mjs";
import {
  newWorld,
  step,
  command,
  validWorld,
  sampleBall,
} from "../brick-breaker/world.mjs";
import { Board, LIMIT } from "../shared-drawing/board.mjs";
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
