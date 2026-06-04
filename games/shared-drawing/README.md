# Shared Drawing Demo

A simple real-time collaborative drawing board demonstrating `game.relay.v1`.

## Features
- Multiple people can draw on the same canvas in real time
- Stroke data is sent as opaque datagrams through the supernode relay
- The supernode never inspects or modifies the drawing payload
- Works in any modern browser that supports WebTransport

## How to use

1. Run a supernode with `game.relay.v1` enabled in its manifest.
2. Open the demo at:

   ```
   https://your-supernode.example:8443/games/shared-drawing/?room=my-room
   ```

3. Share the link with friends (same `room` parameter = same canvas).

## Technical notes

- Uses the official `web-sdk/conquerd.mjs`
- Wire format is extremely simple (see `drawing.js`)
- Throttled client-side to ~20 fps to avoid flooding the relay
- Demonstrates a practical use of the opaque game relay for low-latency game state

This is the second official game demo (after the cursor relay example). It shows how easy it is to build lightweight multiplayer experiences on top of ConquerD's game relay capability.