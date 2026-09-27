# ADR-0004: The scroll anchor is client-owned

**Status:** Accepted

## Context

gmux's scroll-jump-to-top bug (PR #500) came from stale `wasAtBottom`/pixel
snapshots racing user wheel input across BSU→ESU→rAF windows. gmux fixed it with
an explicit `following`/`anchored` mode state machine in the client.

A server-facing integer `anchorLine` is meaningless: there are three incompatible
line spaces (xterm.js buffer ids, `capture-pane` padded/reflowed lines, tmux
history indices that shift on eviction/ED3/reflow).

## Decision

> **Correction (2026-09-27): this Decision was never implemented as written.** The
> client does not own a line-identity anchor, there is no `following`/`anchored`
> mode machine in the mobile client, `FOLLOW` is never sent by it, and no frame
> carries a `snapshotEpoch`. The `snapshotEpoch` exists on the wire in `caps` and the
> client never reads it. The anchor that exists is libghostty-vt's own viewport
> (`viewport_active`/`scroll_offset`, read from the frame header every render) and
> `TerminalController.scrollBy` reports the movement it measured rather than the
> movement it asked for. That delivers the property this ADR exists for — output
> never moves the viewport, measured in `native/test_engine.c` — with no line space
> to reconcile, which was the ADR's other premise. Read the decision below as the
> plan of record it once was, not as a description of the code.

- The **client owns the scroll anchor**; the server never resolves line ids.
- Modes: `following` (pinned to bottom) and `anchored` (absolute line identity +
  distance-from-bottom). **Only explicit user intent** changes the mode; output
  never moves the viewport.
- The `FOLLOW` message carries only `{mode}` (no server-resolved line id).
- `SNAPSHOT`/`CAPS` carry a monotonic `snapshotEpoch`. On reconnect the client
  takes the snapshot as a fresh baseline and re-derives the anchor; if the anchored
  line is gone it stands down to `following` rather than guessing.
- BSU/ESU (DEC 2026), ED3, alternate-screen, and resize reflow are handled from the
  parser; anchor survives resize by line identity.

## Consequences

- **+** Stable scrolling; no cross-boundary line-id ambiguity.
- **+** Server stays simple; scrollback paging (the `scrollback` message) is
  independent.
- **−** The client carries the complexity; the state machine is a ~30-line
  module in the browser client's `core.js` with unit tests (that client was later
  deleted). **That module became `Scroller` in the mobile client** — this sentence
  was false when it was written and stayed false for months; `grep` for `Scroller`,
  `scrollMode`, `anchorFromBottom` or `anchoredTarget` over `packages/mobile` finds
  nothing. The mobile client's scroll code is `TerminalController.scrollBy` plus the
  surface's gesture handling, and its anchor is the emulator's viewport.