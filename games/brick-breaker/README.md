# Brick Breaker Demo

Classic paddle-and-ball brick breaker built on `game.relay.v1` opaque datagram relay.

Multiple people join the same room and instantly share one game world: one ball, one brick field, shared score and lives. Every participant controls their own paddle — the ball bounces off **any** paddle in the room. The first player to join (or the last one keeping the simulation alive) acts as the authoritative "driver" that runs physics and broadcasts compact state snapshots ~20 times per second.

## Features

- Real-time collaborative Breakout-style gameplay over the ConquerD game relay
- Every peer's paddle is visible and interactive (co-op "save the ball" fun)
- Authoritative driver model with automatic soft takeover if the current driver leaves
- Compact binary wire format (paddle + full state + reset) — completely opaque to the supernode
- Works in any modern browser with WebTransport support
- Keyboard, mouse, and touch input

## How to use

1. Run a supernode with `game.relay.v1` enabled in `supernode.toml`.
2. Open the demo:

   ```
   https://your-supernode.example:8443/games/brick-breaker/?room=my-lobby
   ```

3. Share the link. Everyone using the exact same `room` parameter plays together.

4. The first tab to load starts driving the ball. Late joiners immediately see the live state.

## Controls

- **Mouse** — move paddle (most precise)
- **Arrow keys / A D** — move paddle (great for keyboard-only)
- **R** — broadcast a level reset request
- **Reset Level** button — same as above

## Technical notes

- Uses the high-level `ConquerdClient` wrapper from `web-sdk/conquerd.mjs`
- Wire protocol (defined in `brick-breaker.js`):
  - `0x01` — paddle position + color (everyone sends this)
  - `0x02` — full authoritative snapshot (ball, velocity, paddle, lives, score, 40-bit brick mask)
  - `0x03` — reset request
- Driver runs a deterministic 60 fps physics step (paddle collisions, brick hits, wall bounces, life loss)
- State is broadcast at ~22 fps + immediately on brick breaks and life changes for responsiveness
- Remote paddles time out after ~2.2 s of silence
- If no state packets arrive for ~1.35 s any client can promote itself to driver and continue from the last known world state

This is the third official game demo (after the cursor relay and shared drawing). It shows how easy it is to build low-latency, state-synchronized multiplayer experiences on top of ConquerD's `game.relay.v1` capability without any custom server logic — the supernode only moves opaque bytes between verified peers in the same room.

## Running locally for development

```bash
# From repo root
cargo run -p conquerd-supernode -- --data-dir ./supernode-data
# (enable game.relay.v1 + web.host.h3.v1 in supernode.toml or via the portal)
```

Then open `http://localhost:8443/games/brick-breaker/?room=test` (or the https WebTransport endpoint).

Have fun breaking bricks together!
