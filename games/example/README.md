# Presence Playground — `game.relay.v1` example

A shared space with smooth pointers, click/tap marks, idle presence and a snake
round, over `game.relay.v1` on the **identity QUIC relay**. Temporary session
IDs keep participants separate even when colors match. Session details sit
beside the canvas, never over it. See the [shared app guide](../README.md) for
reuse in collaborative tools, session controls and limits.

## Buttons

Each button is one deterministic action, and the toolbar separates the two
kinds. **Appearance is yours**: seven colors and six shapes change only how you
are drawn — for other guests, in their pointer, their marks and their snake
head. You start on the color the session panel shows beside your name and on a
shape derived from the same session id, so a room of strangers is already
distinguishable. Keys `1`-`6` pick a shape.

**Room settings belong to everyone**: mode (Presence / Snake), snake speed
(Slow 220 ms, Steady 150 ms, Quick 95 ms per step), edges (Wrap or Solid) and
New round. A press is stamped with a Lamport clock and the presser's id, so two
guests pressing different buttons in the same moment converge on one answer on
every device instead of flapping. Take a snake / Watch instead is local intent:
the coordinator reconciles who holds a snake from the presence beat, and six
snakes is the cap.

In snake mode, arrows or WASD steer, and tapping the board steers toward the
tap. With no snake of your own, a tap still marks a point.

## Determinism

The board is not shared by luck. `playground.mjs` holds the rules as pure
functions: the same room and round number seed the same layout, food is placed
by linear probing from a hashed cell, turns are queued and applied at the tick
boundary, and collisions are judged after every body has moved, so nothing
depends on the order snakes are visited. One peer — the session's elected
coordinator — steps the simulation and sends a snapshot each tick; a worst case
board of six snakes at full length still fits one 1,100 byte session envelope.
When the coordinator leaves, the next one continues the round from the last
snapshot rather than restarting it. `games/tests/demos.test.mjs` covers the
convergence, collision, wire and envelope rules.

## Requirements

- A running `conquerd-supernode` with `game.relay.v1` and `web.host.app.v1`.
- A native ConquerD client that has accepted the supernode invite (portal + relay).

## Enable features in supernode.toml

```toml
[[feature]]
id = "game.relay.v1"
enabled = true

[[feature]]
id = "web.host.app.v1"
enabled = true
```

## Open the game

From the native client Rooms sidebar, open the supernode portal and navigate to:

```
d://<supernode_id>/games/example/?room=lobby1
```

External HTTPS / browser tabs are not supported.
