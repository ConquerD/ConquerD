export const W = 800,
  H = 600,
  PADDLE_Y = 551,
  PADDLE_W = 100,
  BALL_R = 7;
export const clamp = (v, min, max) => Math.max(min, Math.min(max, v));
export const brickRect = (i) => ({
  x: 42 + (i % 8) * 90,
  y: 66 + Math.floor(i / 8) * 32,
  w: 84,
  h: 24,
});

export function newWorld() {
  return {
    x: 400,
    y: 530,
    vx: 170,
    vy: -310,
    bricks: Array(40).fill(1),
    lives: 3,
    score: 0,
    phase: "serve",
    round: 0,
    ack: [],
  };
}
export function validWorld(s) {
  return (
    s &&
    ["serve", "playing", "won", "over"].includes(s.phase) &&
    ["x", "y", "vx", "vy"].every((k) => Number.isFinite(s[k])) &&
    s.x >= 0 &&
    s.x <= W &&
    s.y >= 0 &&
    s.y <= H + 20 &&
    Math.abs(s.vx) <= 700 &&
    Math.abs(s.vy) <= 700 &&
    Number.isInteger(s.lives) &&
    s.lives >= 0 &&
    s.lives <= 3 &&
    Number.isInteger(s.score) &&
    s.score >= 0 &&
    s.score <= 400 &&
    Number.isInteger(s.round) &&
    s.round >= 0 &&
    Array.isArray(s.bricks) &&
    s.bricks.length === 40 &&
    s.bricks.every((v) => v === 0 || v === 1) &&
    Array.isArray(s.ack) &&
    s.ack.length <= 8 &&
    s.ack.every((v) => typeof v === "string" && v.length <= 36)
  );
}
export function command(world, kind, id) {
  if (world.ack.includes(id)) return false;
  const ack = [...world.ack, id].slice(-8);
  if (kind === "reset")
    Object.assign(world, newWorld(), { round: world.round + 1 });
  if (kind === "launch" && world.phase === "serve") world.phase = "playing";
  world.ack = ack;
  return true;
}

// Fixed-step, client-owned simulation; velocity is pixels/second everywhere.
export function step(world, dt, paddles) {
  if (world.phase !== "playing") return;
  const oldX = world.x,
    oldY = world.y;
  world.x += world.vx * dt;
  world.y += world.vy * dt;
  if (world.x < BALL_R) {
    world.x = BALL_R;
    world.vx = Math.abs(world.vx);
  }
  if (world.x > W - BALL_R) {
    world.x = W - BALL_R;
    world.vx = -Math.abs(world.vx);
  }
  if (world.y < BALL_R) {
    world.y = BALL_R;
    world.vy = Math.abs(world.vy);
  }
  if (world.y > H + BALL_R) {
    world.lives--;
    world.x = 400;
    world.y = 530;
    world.vx = 170;
    world.vy = -310;
    world.phase = world.lives ? "serve" : "over";
    return;
  }
  if (
    world.vy > 0 &&
    oldY + BALL_R <= PADDLE_Y &&
    world.y + BALL_R >= PADDLE_Y
  ) {
    const paddle = paddles.find(
      (x) => Math.abs(world.x - x) <= PADDLE_W / 2 + BALL_R,
    );
    if (paddle !== undefined) {
      const offset = clamp((world.x - paddle) / (PADDLE_W / 2), -1, 1);
      const speed = clamp(Math.hypot(world.vx, world.vy) * 1.025, 350, 560);
      world.vx = Math.sin(offset * 1.05) * speed;
      world.vy = -Math.cos(offset * 1.05) * speed;
      world.y = PADDLE_Y - BALL_R;
    }
  }
  for (let i = 0; i < 40; i++) {
    if (!world.bricks[i]) continue;
    const b = brickRect(i);
    const cx = clamp(world.x, b.x, b.x + b.w),
      cy = clamp(world.y, b.y, b.y + b.h);
    if ((world.x - cx) ** 2 + (world.y - cy) ** 2 > BALL_R ** 2) continue;
    world.bricks[i] = 0;
    world.score += 10;
    if (oldX + BALL_R <= b.x || oldX - BALL_R >= b.x + b.w) {
      world.vx *= -1;
      world.x = oldX;
    } else {
      world.vy *= -1;
      world.y = oldY;
    }
    if (world.bricks.every((v) => !v)) world.phase = "won";
    break;
  }
}

// Buffered interpolation with a short, bounded extrapolation at the edge.
export function sampleBall(samples, time) {
  if (!samples.length) return null;
  for (let i = 1; i < samples.length; i++) {
    const a = samples[i - 1],
      b = samples[i];
    if (time <= b.time) {
      const t = clamp((time - a.time) / (b.time - a.time || 1), 0, 1);
      if (
        a.round !== b.round ||
        a.phase !== b.phase ||
        Math.hypot(b.x - a.x, b.y - a.y) > 120
      )
        return b;
      return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
    }
  }
  const b = samples.at(-1),
    dt = b.phase === "playing" ? clamp((time - b.time) / 1000, 0, 0.05) : 0;
  return {
    x: clamp(b.x + b.vx * dt, BALL_R, W - BALL_R),
    y: clamp(b.y + b.vy * dt, BALL_R, H),
  };
}
