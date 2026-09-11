// Keep editable portal examples and the self-contained supernode bundle equal.
// Usage: node scripts/sync_portal_demos.mjs [--check]
import { readFile, writeFile } from "node:fs/promises";

const assets = [
  ["games/example/index.html", "games_example_index.html"],
  ["games/example/game.js", "games_example_game.js"],
  ["games/brick-breaker/index.html", "games_brick_breaker_index.html"],
  [
    "games/brick-breaker/brick-breaker.js",
    "games_brick_breaker_brick_breaker.js",
  ],
  ["games/brick-breaker/world.mjs", "games_brick_breaker_world.mjs"],
  ["games/shared-drawing/index.html", "games_shared_drawing_index.html"],
  ["games/shared-drawing/drawing.js", "games_shared_drawing_drawing.js"],
  ["games/shared-drawing/board.mjs", "games_shared_drawing_board.mjs"],
  ["web-sdk/conquerd.mjs", "web_sdk_conquerd.mjs"],
  ["web-sdk/demo-session.mjs", "web_sdk_demo_session.mjs"],
  ["web-sdk/demo-shell.mjs", "web_sdk_demo_shell.mjs"],
  ["web-sdk/demo-shell.css", "web_sdk_demo_shell.css"],
];
const check = process.argv.includes("--check");
for (const [source, name] of assets) {
  const src = new URL(`../${source}`, import.meta.url);
  const dest = new URL(
    `../rust/conquerd-supernode/templates/${name}`,
    import.meta.url,
  );
  const content = await readFile(src);
  const existing = await readFile(dest).catch(() => Buffer.alloc(0));
  if (content.equals(existing)) continue;
  if (check) {
    console.error(`Out of sync: ${name}`);
    process.exitCode = 1;
  } else {
    await writeFile(dest, content);
    console.log(`Updated ${name}`);
  }
}
