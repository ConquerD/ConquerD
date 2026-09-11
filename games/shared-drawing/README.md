# Shared Drawing — `game.relay.v1` demo

Real-time collaborative canvas over opaque `game.relay.v1` datagrams.

Ink and eraser tools stay above the canvas. Ordered operations survive resizing
and replay from open peers to late joiners; clear applies to everyone after a
second click. History is bounded to 3,000 operations and lives only in open
pages. See the [shared app guide](../README.md) for session controls and limits.

## Requirements

- Native ConquerD client + trusted supernode with `game.relay.v1` and `web.host.app.v1`.

## Open

```
d://<supernode_id>/games/shared-drawing/?room=my-room
```

Open only from the in-app portal. External browsers are not supported.
