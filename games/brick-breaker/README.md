# Brick Breaker — `game.relay.v1` demo

Multiplayer paddle game over opaque `game.relay.v1` datagrams.

The play area has no corner overlay. Mouse, touch or arrow/A-D keys move your
paddle; Space or Launch ball starts a rally. Any participant can request a new
game. A peer runs fixed-step physics, while other peers interpolate its state;
the remaining sessions elect a new coordinator when it leaves. Open Session for
participants, readiness and actual network measurements, or Focus for more room.
See the [shared app guide](../README.md) for protocol limits and validation.

## Requirements

- Native ConquerD client + trusted supernode with `game.relay.v1` and `web.host.app.v1`.

## Open

```
d://<supernode_id>/games/brick-breaker/?room=my-lobby
```

Open only from the in-app portal. External browsers are not supported.
