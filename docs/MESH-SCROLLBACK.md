# Scrollback over the mesh link

**Status, 2026-11-03: the phone has local scrollback; the host-side history this document designs
is not implemented.** `MeshHistory.kt` keeps the rows that leave the top of each screen, using the
absolute line number the host already numbers every screen with (`firstLine`); the host sends no
history messages, so the `History` / `HistoryRows` pair and the `SCROLLBACK` profile bit described
below remain the unfilled half. What that buys: scrolling back over what has already arrived costs
no airtime, and what it cannot do: reach lines that scrolled off before the phone attached. Read the
rest of this as the design for that gap, not as a description of the code.

The mesh route has no host-side scrollback at all. `Profile` and `Rows` carry exactly one screen, the phone
is told `Scrollback` is absent (`mesh_term::unsupported::SCROLLBACK`), and the tailnet route has had
paged history since before this link existed (`crates/protocol/src/messages.rs:241`,
`crates/keeper/src/emulator.rs:55`). This is the design for closing that gap, and the reasoning for
each choice.

## What the tailnet does, because the goal is parity

- The phone keeps every row it has loaded in its own engine, with `scrollTotal` and `scrollOffset`
  (`TerminalController.kt:743`).
- Scrolling inside what it holds is local: no request, no traffic.
- When the user reaches the **top of what is loaded**, it asks the host for a deeper page:
  `{"type":"scrollback","lines":N}` (`AttachClient.kt:644`). The reply **replaces** the loaded
  history with the newest N rows, so `N` is a depth, not a delta.
- One request in flight, page size doubling up to a cap that follows the link class, and a latch
  that stops asking when the host has no more (`TerminalController.kt:710-755`).

The structure to copy is therefore: **local accumulation + a lazy deeper-history request when the
user runs out**, not "the host sends two screens at attach".

## What prior art says

- **Client-side accumulation is what graphical terminals do.** WezTerm moves rows into its own
  buffer as they scroll off ("the scrolled lines are moved into the scrollback buffer"), kitty and
  Termux the same, and Termux's implementation is a circular row array plus a `topRow` viewport
  offset. iTerm2's tmux integration exists precisely to move history from the server to the client.
- **Server-side paging (tmux) costs the server** and needs a copy of the history to keep the view
  stable: `copy mode will be an issue with large histories because we now copy the history from the
  target pane`, and the alternative — freezing the pane — broke long-running processes.
- **mosh has no scrollback by design**: its synchronized object is a framebuffer of exactly the
  screen height, and lines are destroyed at `rows.erase` before any packet describing them exists.
  Its maintainers' answer is "run tmux". That is the failure mode to avoid, and mosh itself shows
  why detection-by-content is the wrong hinge: its renderer has to *guess* whether the screen
  scrolled by matching rows against rows.
- **The phone is an emulator, not a stream sink.** It already renders from a mirror it owns
  (`MeshTerm.kt` `RowMirror`), which is exactly the position Termux and WezTerm are in. History
  belongs there.

## The one thing the mesh protocol lacks

The tailnet's client can accumulate because its engine knows the absolute line number of every row
it holds. The mesh mirror cannot: `Rows { epoch, full, patches }` addresses rows **by position on
the screen**, and `Commit` carries only the cursor. When the host's screen scrolls by three rows,
the phone sees three rows change content and has no way to know that the old row 0 is now history
rather than overwritten in place. So the phone cannot accumulate without help, and it cannot know
which rows it missed during a burst.

That is a small, precise gap, and the fix is a line number — not a history payload.

## Design

### 1. The profile names the first line (protocol version 5)

`Profile` gains `first_line: u32`: the absolute line number of the row at the top of the screen.

- A screen that scrolls by N advances `first_line` by N.
- The phone, on committing a screen whose `first_line` is `old + N`, takes the N rows that were at
  the top of its previous mirror and appends them to its local ring. They are already in memory: no
  traffic, no request, no latency.
- If `first_line` advances by **more** than the phone held (a burst that scrolled 200 lines while
  the phone had 40), the phone records a **gap** of 160 lines. It knows where they were, and can ask
  for them if the user scrolls there. Nothing is silently lost, which is what mosh does and what
  makes its scrollback unusable after `cat`.
- If `first_line` goes backwards or the epoch changes, the ring is discarded: a resize reflow or a
  new shell is not a scroll.

Version gating is the existing pattern (`MIN_VERSION..=VERSION`, `Differ::for_attach(version)`):
a version-4 phone gets the old profile shape and is told `Scrollback` is absent, exactly as a
version-2 phone is told `Resize` is absent.

### 2. A history request for what the phone never saw

Only when the user scrolls above the oldest row the phone holds — or into a gap — does the phone ask:

```
History { before: u32, want: u8 }          // phone -> host, 0x08
HistoryRows { before: u32, lines: Vec<String>, more: bool }   // host -> phone, 0x86
```

- `before` is the absolute line number of the oldest row the phone holds; `want` is a row count,
  bounded to one screen.
- The reply carries rows **immediately above** `before`, oldest first, and `more` says whether
  anything older exists.
- Pages are cached on the phone, so scrolling back over them again is free.

The host serves this from the keeper's scrollback, which already exists: the keeper's snapshot RPC
takes `historyLines` and reports how many it actually captured
(`crates/protocol/src/keeper.rs:191-215`). The daemon needs the absolute line count for a session —
see "What the host must add" below.

### 3. Scrolling is a viewport offset, not a stream

The terminal surface draws rows from `(history ++ screen)` at a `topRow` offset, as Termux does. The
live screen keeps being painted into the mirror while the viewport is back; returning to the bottom
is instant, and nothing about being scrolled back changes what the host sends. This is the property
tmux had to buy with a copy of the history and mosh could not buy at all.

## Cost, honestly

A 172-byte record at 910.525 MHz / 500 kHz / SF7 / CR5 is about 66 ms of airtime (258 symbols at
256 µs, plus preamble), so the link carries roughly **15 records a second**, and one row of up to 78
characters is one record.

| Action | Records | Airtime |
|---|---|---|
| Scroll within what the phone has seen | 0 | 0 |
| Fetch one screen of history (40 rows) | ~40 | ~2.6 s |
| Fetch a page for a 200-line burst gap | 40 per page | ~2.6 s per page |

That is why the design is not "always send two screens": two screens at attach would cost ~5 s
before the first prompt is usable, on every attach, for history the user may never read. Paying
2.6 s once, when the user actually scrolls back, and never paying again for the same rows, is both
cheaper in airtime and faster to first screen.

The phone's ring is bounded by rows, not bytes: 2000 rows of 78 characters is ~156 KB of text, and
the mesh link carries no style (`unsupported::STYLE`), so a row is a `String` and nothing else. That
bound is 40 screens of history held at zero cost.

## What the host must add

1. **An absolute line counter per session.** The keeper's emulator knows how many lines have scrolled
   (its scrollback total). Expose it in the snapshot result next to `historyLines`, and thread it
   into `Screen::from_keeper` (`mesh_bridge.rs:473`).
2. **A history slice.** The keeper's snapshot already captures `historyLines` from the oldest; the
   mesh needs a *window* above a given line, which is the same capture with an offset. Either extend
   the keeper's snapshot with `historyStart` or compute the slice in the daemon from a full capture —
   the former is one field, the latter is one capture of up to the whole scrollback per request.

## What the phone must add

1. `firstLine` on `MeshScreen` and the history ring in `MeshTermDriver`, fed on `Commit`.
2. A viewport offset in the surface's frame model, and the gesture that moves it (the tailnet
   surface already has this concept).
3. The `History` request, issued only at the top of what is loaded, one at a time, with the page
   cached.

## Rejected alternatives

- **Send the whole scrollback at attach.** Costs minutes of airtime for history nobody asked for,
  and delays the first prompt. This is the failure mode mosh avoided by having no history at all.
- **Server-side paging for every scroll (tmux).** Every scroll is a round trip; on this link a round
  trip is seconds. It also requires the host to hold a copy so the view is stable while output
  arrives.
- **Detect scrolls by matching rows (mosh's renderer).** Ambiguous by construction: an in-place
  rewrite of identical content is indistinguishable from a scroll. A line number is one field and
  removes the guess.
- **No scrollback (mosh).** Its own issue tracker has been open since 2012 and its maintainers'
  answer is "run tmux". The phone has no tmux.

## Rollout

1. Protocol version 5: `Profile.first_line`, `History`, `HistoryRows`; fixtures regenerated; the
   phone keeps working on version 4 unchanged.
2. Host: line counter and the slice; `absent::SCROLLBACK` cleared for version 5.
3. Phone: ring, viewport, request — then parity testing against the tailnet route with the same
   shell and the same output, comparing what the user can scroll to.
