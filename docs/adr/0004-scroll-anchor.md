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
deleted; the module it named is now `Scroller` in the mobile client), `scrollMode`,
  `anchorFromBottom`, `anchoredTarget`), not a separate xterm addon.