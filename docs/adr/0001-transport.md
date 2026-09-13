# ADR-0001: tmux transport for v1 is PTY-free (capture-pane diff polling)

**Status:** Accepted (spike pending, Phase 1)

## Context

`ldmuxd` must stream pane output to WebSocket clients. The obvious tmux mechanism
is **control mode** (`tmux -C`), used by dooboxes and many tools. But control mode
is a **PTY-based** client protocol — tmux attach/control clients require a tty.
A Bun daemon would therefore need a native PTY dependency (`node-pty`/`bun-pty`),
which complicates single-binary packaging and reintroduces the native-module
fragility we chose Bun to avoid.

Alternatives:
1. **Control mode (`-C`)** — lowest latency, real-time notifications, flow control
   (`%pause`/`%continue`); requires PTY; octal-escaped `%output`; more complex.
2. **`pipe-pane`** — raw pane bytes to a daemon-owned FIFO/socket; no PTY; but
   output-only, one pipe per pane, no flow control, pipe continuity/restart is
   fiddly, no topology notifications.
3. **`capture-pane` adaptive diff polling** — no PTY, stateless/restart-safe,
   gives the visible screen with escapes; costs poll latency + repeated
   full-screen diffs for full-screen apps.

## Decision

For v1, use **`capture-pane` adaptive diff polling** behind a `PaneStream`
interface. Rate is adaptive: active ≈ 60–120 ms, idle backoff ≈ 1 s, **stopped
when no viewer**. Input uses `send-keys`; topology uses `list-*` +
`display-message` + event triggers on spawn/input.

Phase 1 includes a **spike** measuring latency/CPU of (1) capture-pane polling,
(2) `pipe-pane`, and (3) control mode on tmux 3.5a, and freezes the decision.
Phase 2 may upgrade `PaneStream` to `pipe-pane` or control mode.

## Consequences

- **+** No native PTY dependency; single-binary Bun packaging stays clean.
- **+** Stateless and restart-safe; reconnect is trivially snapshot-based.
- **+** v1 scope stays small (single-user, one pane/session, a few sessions).
- **−** Higher latency than control mode; more full-screen diffs for TUIs.
- **−** Output-only tombstone: no `%pause` flow control; backpressure handled by
  buffering + snapshot-on-overflow.
- If control mode is adopted later, revisit the Bun packaging rationale.