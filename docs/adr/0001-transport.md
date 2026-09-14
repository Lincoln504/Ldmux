# ADR-0001: Transport is tmux control mode `-C` over pipes (no PTY) + capture-pane snapshots

**Status:** Accepted — **frozen** (verified by experiment on this machine: tmux 3.5a, Bun 1.3.9)

## Context

`ldmuxd` must stream pane output to WebSocket clients. Candidate mechanisms:

1. **Control mode (`tmux -C`)** — event-driven notifications; lowest latency;
   flow control (`refresh-client -f pause-after`, `%pause`/`%continue`,
   `%extended-output`); topology events.
2. **`pipe-pane`** — raw pane bytes to a sink; no PTY; output-only; no flow
   control; pipe continuity/restart is fiddly.
3. **`capture-pane` diff polling** — no PTY; stateless/restart-safe; poll latency;
   repeated full-screen diffs for TUIs.

The v0.1/v0.2 drafts *assumed* control mode is inherently PTY-based (echoing
dooboxes' `TMUX_SESSION_MANAGEMENT.md`: "PTY-based tmux control mode connection"),
which would force a native PTY dependency (`node-pty`/`bun-pty`) into the Bun
daemon and complicate single-binary packaging. That assumption was tested.

## Experiment (this machine)

Scratch tmux server `-L ldmux-probe`, `x 100x30`, `window-size manual`:

| Test | Result |
|------|--------|
| `tmux -C attach` with **stdin/stdout pipes, no tty** | **Works** — `%begin/%end`, `%session-changed`, `%output`, `%exit` streamed over the pipe; stderr empty. |
| `tmux -CC attach` over a pipe | **Fails** — `tcgetattr failed: Inappropriate ioctl for device`. `-CC` requires a tty; `-C` does not. |
| Live `%output` over a pipe | `%output %0 echo …\015\012`, ANSI preserved. |
| `refresh-client -f pause-after=2` | Client lists as `control-mode,pause-after=2`; output switches to `%extended-output %0 <age_ms> : <data>`. |
| `window-size manual` | Control client reports `80x`, window stays `100x30` — **no reflow**. |
| Real **Bun 1.3.9** `Bun.spawn([... "-C","attach"], {stdin:"pipe",stdout:"pipe"})` | **Works end to end**; parsed `%extended-output`; correct bytes. |
| Burst (`seq 1 30000`) via Bun | ~383 KB in ~255 ms (≈1.5 MB/s), 184 `%extended-output` frames, no stalls; ~0 tmux CPU idle. |
| `capture-pane -p -e` cost | ~3.1 ms/call (spawn overhead dominates). |

**Conclusion:** the PTY requirement is **false for `-C`** on Linux/tmux 3.5a. Use
`-C`, never `-CC`.

## Decision

v1 transport = **control mode `-C` over pipes** for live streaming +
**`capture-pane`** for snapshots/scrollback + **automatic `capture-pane` fallback**,
all behind a `PaneStream` interface. Stack stays **Bun + TypeScript**, no native
modules.

Runtime shape:
- One `Bun.spawn(["tmux","-L","ldmux","-C","attach","-t",name], {stdin:"pipe",stdout:"pipe"})`
  per **viewed** session; start on first viewer, stop shortly after the last leaves.
- `stdin` = command pipe (`send-keys`, `refresh-client`, …); `stdout` = notification
  pipe.
- `refresh-client -f pause-after=2` for backpressure.
- On connect/reconnect: `capture-pane -p -e` + `display-message -p` state fields →
  `SNAPSHOT`, then live stream.
- Robust parser: byte→line on `\n` (never readline); octal→**bytes**; **per-pane
  incremental UTF-8 decoder**; `%begin/%end/%error <seq>` correlation; tolerate
  unknown `%…` lines; stateful sanitizer on by default (**implemented**: `packages/daemon/src/sanitize.ts`, one instance per pane, drops OSC 52, DCS/SOS/PM/APC, and device-status queries).
- Fallback on `%exit`/stall/EOF: the control client is **reconnected with bounded
  backoff** and subscribers resync from a snapshot. Adaptive `capture-pane` diff
  polling is **not implemented** (documented here as the original intent only).
- Pinned `window-size manual` + canonical geometry (ADR-0002).

## Consequences

- **+** Control-mode latency + flow control **without** a native PTY dependency;
  Bun single-binary packaging stays clean.
- **+** Event-driven (no polling CPU), topology events available, low bytes for TUIs.
- **+** Restart-safe: tmux owns state; the daemon is a cache.
- **−** A real parser is required (framing/octal/UTF-8/sequence); dooboxes' shipped
  parser is buggy (drops split UTF-8; `byte as char`) and was **dead code** — write
  a new one with tests.
- **−** One control client per viewed session (acceptable: a handful).
- `pipe-pane` and a server-side VT emulator remain behind the `PaneStream` seam.