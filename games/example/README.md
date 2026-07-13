# Cursor Relay — `game.relay.v1` example

A minimal in-app portal game that relays each participant's cursor position to
other players in the same session via `game.relay.v1` over the **identity QUIC
relay** (no external browser / WebTransport).

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
conquerd://<supernode_id>/games/example/?room=lobby1
```

External HTTPS / browser tabs are not supported.
