# Cursor Relay — `game.relay.v1` example

A minimal browser game that relays each participant's cursor position to all
other players in the same session via `game.relay.v1` over WebTransport.

The supernode receives `[tag][payload]` datagrams from each browser peer and
fans them out verbatim to every other peer in the same session — it never
parses the game payload.

---

## Requirements

- A running `conquerd-supernode` with `game.relay.v1` enabled.
- A TLS certificate that browsers trust (or a dev cert accepted via a browser
  flag — see the supernode README).

---

## Enable `game.relay.v1` in supernode.toml

```toml
schema_version = 1

[[feature]]
id = "game.relay.v1"
enabled = true

[[feature]]
id = "web.host.h3.v1"
enabled = true
```

---

## Serve the game files

The supernode's built-in HTTPS portal will serve any directory placed under
`<data_dir>/games/<slug>/`. Copy the `games/example/` folder there and the
game will be reachable at:

```
https://<host>:<web_port>/games/example/
```

Alternatively serve the files from any static HTTPS host and point query
params at the supernode:

```
https://your-static-host/game/?host=supernode.example.com&port=8443&room=lobby1
```

---

## URL parameters

| Parameter | Default             | Description                            |
|-----------|---------------------|----------------------------------------|
| `host`    | `location.hostname` | Supernode hostname                     |
| `port`    | `8443`              | Supernode WebTransport port            |
| `room`    | `default`           | Game session id (shared with players)  |

---

## Wire format

The game uses a simple 5-byte header; all payloads are opaque to the relay:

```
[u8 type][u16 BE x][u16 BE y][utf-8 color hex]

  type 0x01 = CURSOR_UPDATE
  type 0x02 = CURSOR_LEAVE
  x, y      = normalised float in [0,1] encoded as uint16 (0..65535)
  color     = up to 16 bytes of "#rrggbb" (omitted for LEAVE)
```

---

## Extending this example

- Replace cursor positions with player state (position, health, action).
- Add a server-authoritative lobby (`game.lobby.v1`) for joining/leaving.
- Use `transport.quic.uni_stream.v1` for ordered reliable events alongside
  `game.relay.v1` unreliable datagrams for real-time state.
