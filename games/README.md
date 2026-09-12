# Portal app showcase

The seven examples share an app shell and run inside DoubleSlash using
`web.host.app.v1` and the native `game.relay.v1` datagram channel. The name of
that existing capability does not require an app to be a game.

| App | Demonstrates |
| --- | --- |
| [Brick Breaker](brick-breaker/) | Cooperative paddles, a peer-owned simulation, interpolated snapshots, coordinator handoff |
| [Shared Canvas](shared-drawing/) | Collaborative ink, erasing, ordered operations, peer replay for late arrivals |
| [Presence Playground](example/) | Live pointers, idle presence, shared attention markers, per-guest appearance, and a deterministic snake round with room-wide settings |
| [Task Board](task-board/) | Eight shared checklist rows, independent edits, completion and late-join catch-up |
| [Focus Timer](focus-timer/) | Shared focus/break deadlines, pause/resume and group reset |
| [Four in a Row](four-in-a-row/) | Open tabletop turns, gravity, win detection and shared round reset |
| [Memory Match](memory-match/) | Cooperative card reveals, pair matching, attempt counts and shuffled rounds |

The Session panel contains a room picker, readiness, a participant list, actual
application traffic counters and measured peer round trips. It sits beside the
workspace; on narrow screens it replaces the workspace until closed. Focus
hides the header, tools and panel, with an Exit focus button and Escape shortcut.
The play area is never underneath an information overlay.

Open the same app and `?room=...` on the **same supernode, or any member of
its cluster**, on both devices. A game session is held by whichever member a
client is connected to, and its frames are replicated to the other members
holding part of the same session — so players spread across a cluster still
meet, which they have to be able to do, because a cluster is presented to
clients as a single node and there is no way to pick a member.
Copy link includes the room. Each app namespaces its native session as
`demo-v1:<app>:<room>` so unrelated example protocols cannot mix. A room name is
a rendezvous label, not an authorization secret; the native feature's admission
rules still apply. These are example app sessions, not persisted native SFU rooms
or a global server directory. Updating from the previous demos requires everyone
to reopen the page; their old wire formats are intentionally isolated.

## Reuse and limits

- `web-sdk/demo-session.mjs` demonstrates bounded app envelopes, ephemeral session
  IDs, presence/expiry, readiness, a deterministic coordinator election, round
  trip probes and send backpressure. All messages use the existing native
  capability, quota and transport path; no web socket or external backend is added.
- Guest IDs identify app instances. They are **not verified peer identities** or
  an anti-cheat boundary. Native identity/auth stays in the framework. Election
  is an example coordination mechanism, not distributed consensus.
- Round trip includes native bridge polling and peer response time. The graph
  counts encoded app bytes (including session messages), not QUIC overhead or
  link capacity. No RTT is shown until a peer actually answers.
- At most 32 remote sessions are tracked; presence expires after 6.5 seconds.
  Envelopes are capped at 1,100 bytes, with at most four bridge sends in flight.
  Snapshots/presence repeat; missed drawing operations reconcile from open peers.
- The playground's room settings resolve by Lamport clock and presser id, so a
  dropped datagram costs a press, never agreement. Its snake board holds at most
  six snakes of 40 cells, packed two base36 characters per cell so a full
  snapshot still fits one envelope every tick.
- Canvas history is capped at 3,000 operations; clear frees it. Data lives in open
  pages only. Closing the last page loses the drawing. Delivery is not a durable
  document guarantee. Readiness is informational and does not block launch.
- The four new demos use `web-sdk/demo-state.mjs`: bounded, last-writer-wins
  snapshots repeated every 400 ms, one register per tick. Task Board has eight
  registers (one per row), so a full catch-up takes about 3.2 seconds without
  loss. Clearing a row keeps its version so stale snapshots cannot restore it.
  Simultaneous changes to the same row or game resolve by version and guest ID;
  a losing edit or move may need to be entered again. This is friendly shared
  state, not durable storage, competitive matchmaking or distributed consensus.
- Task text is limited to 80 characters per row and shared on Enter/blur.
  Four in a Row allows anyone to play either side. Memory Match keeps the deal
  in page memory, so it is not an anti-cheat boundary. Focus Timer uses device
  wall clocks: keep them synchronized for matching countdowns. It is silent,
  does not send notifications, and requires an open page.

These patterns can be adapted into team boards, presentation controls, tabletop
lobbies or shared dashboards. Additional native capabilities still need their
own negotiated descriptors, quotas and consent rules.

## Develop and verify

Edit `games/` and `web-sdk/`, then update the embedded copies:

```powershell
node scripts/sync_portal_demos.mjs
node scripts/sync_portal_demos.mjs --check
node --test games/tests/demos.test.mjs
```

The Qt integration suite runs the real custom scheme handler and browser, with
an in-process opaque relay replacing QUIC. It checks two app instances, launch/
reset/leave, drawing catch-up, room links and desktop/mobile layout boundaries:

```powershell
cmake -S games/tests -B .tmp-run/app-build -G "Visual Studio 17 2022" -A x64 -DCMAKE_PREFIX_PATH=C:/Qt/6.8.3/msvc2022_64
cmake --build .tmp-run/app-build --config Release
$env:PATH = "C:/Qt/6.8.3/msvc2022_64/bin;$env:PATH"
ctest --test-dir .tmp-run/app-build -C Release --output-on-failure
```

Chromium needs permission to start renderer subprocesses; run the tests outside
an agent execution sandbox if loads stall. These fixtures do not replace a real
desktop/phone check over QUIC. For that check, open the same app/room on both
devices, move both paddles, launch from either device, disconnect the coordinator,
and verify continuation. Draw before opening the second canvas and verify catch-up;
then resize, erase and clear from either side. Check Focus and Session in portrait.
For the additional demos, edit a task before joining from the second device,
check it off there, then clear it and verify it stays cleared. Start a five-minute
timer and pause/reset from the other device. Alternate columns in Four in a Row
until a win and start a new round. Reveal cards in Memory Match from both devices,
hide a mismatch, find a pair and shuffle. Close the first device and verify the
remaining page can continue each activity.

The supernode embeds all assets and updates built-in files on startup. Rebuild
and redeploy the supernode, then reopen its portal to use the new examples.
Operator apps under other slugs are preserved.
