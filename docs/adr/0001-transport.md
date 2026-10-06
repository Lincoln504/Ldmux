# ADR-0001: Transport is tmux control mode `-C` over pipes (no PTY) + capture-pane snapshots

**Status:** Superseded by [ADR-0018](0018-session-layer.md) (2026-10-04). Kept as history.

> **Superseded (2026-10-04, D4).** The tmux session layer this ADR decided on is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018 and `docs/archive/RUST-PORT-LOG.md`.

> The replacement: one framed binary protocol the project owns (ADR-0016 for the
> remote half), with the keeper speaking it over a Unix socket. Control mode's
> 160-line text parser and the pipe∥control split are both gone; there is no
> `capture-pane` any more, the emulator *is* the screen.

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
which would force a native PTY dependency (`node-pty`) into the Node
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
| Real **Node 1.3.9** `Node.spawn([... "-C","attach"], {stdin:"pipe",stdout:"pipe"})` | **Works end to end**; parsed `%extended-output`; correct bytes. |
| Burst (`seq 1 30000`) via Node | ~383 KB in ~255 ms (≈1.5 MB/s), 184 `%extended-output` frames, no stalls; ~0 tmux CPU idle. |
| `capture-pane -p -e` cost | ~3.1 ms/call (spawn overhead dominates). |

**Conclusion:** the PTY requirement is **false for `-C`** on Linux/tmux 3.5a. Use
`-C`, never `-CC`.

## Decision

v1 transport = **control mode `-C` over pipes** for live streaming +
**`capture-pane`** for snapshots/scrollback + **automatic `capture-pane` fallback**,
all behind a `PaneStream` interface. Stack stays **Node + TypeScript**, no native
modules.

Runtime shape:
- One `Node.spawn(["tmux","-L","ldmux","-C","attach","-t",name], {stdin:"pipe",stdout:"pipe"})`
  per **viewed** session; start on first viewer, stop shortly after the last leaves.
- `stdin` = command pipe (`send-keys`, `refresh-client`, …); `stdout` = notification
  pipe.
- `refresh-client -f pause-after=2` for backpressure.
- On connect/reconnect: `capture-pane -p -e` + `display-message -p` state fields →
  `SNAPSHOT`, then live stream.
- Robust parser: byte→line on `\n` (never readline); octal→**bytes**; **per-pane
  incremental UTF-8 decoder**; `%begin/%end/%error <seq>` correlation; tolerate
  unknown `%…` lines; stateful sanitizer on by default (**implemented**: `crates/daemon/src/sanitize.rs`, one instance per pane, drops OSC 52, DCS/SOS/PM/APC, and device-status queries). OSC 52 is the only sequence a host may re-enable: `ldmux clipboard-write on`, off by default, policy file in the state dir (`crates/daemon/src/clipboard_policy.rs`).
- Fallback on `%exit`/stall/EOF: the control client is reconnected with bounded
  backoff (3 attempts), and if control mode still cannot be established the
  stream switches to `capture-pane` **row-diff polling** (~250 ms) so a viewer
  keeps seeing the pane; control mode is retried every 30 s. A `%pause` (tmux
  dropped queued output because we fell behind) is treated as a resync point and
the pane is resumed with `refresh-client -A <pane>:continue`.
- Pinned `window-size manual` + canonical geometry (ADR-0002).

## Consequences

- **+** Control-mode latency + flow control **without** a native PTY dependency;
  Node single-binary packaging stays clean.
- **+** Event-driven (no polling CPU), topology events available, low bytes for TUIs.
- **+** Restart-safe: tmux owns state; the daemon is a cache.
- **−** A real parser is required (framing/octal/UTF-8/sequence); dooboxes' shipped
  parser is buggy (drops split UTF-8; `byte as char`) and was **dead code** — write
  a new one with tests.
- **−** One control client per viewed session (acceptable: a handful).
- `pipe-pane` and a server-side VT emulator remain behind the `PaneStream` seam.