# Presence Playground — `game.relay.v1` example

A shared space with smooth pointers, click/tap ripples and idle presence via
`game.relay.v1` over the **identity QUIC relay**. Temporary session IDs keep
participants separate even when colors match. Session details sit beside the
canvas, never over it. See the [shared app guide](../README.md) for reuse in
collaborative tools, session controls and limits.

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
