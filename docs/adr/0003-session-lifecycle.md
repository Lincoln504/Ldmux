# ADR-0003: pi extension session lifecycle is reason-branched

**Status:** Superseded by [ADR-0009](0009-node-runtime-and-no-pi-extension.md) (the pi extension is gone) and [ADR-0018](0018-session-layer.md). Kept as history.

> **Superseded (2026-10-04, D4).** The tmux session layer this ADR decided on is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018.

> The pi extension is gone (ADR-0009); the lifecycle it described is the daemon's
> registry, and the keeper is the process that holds the shell across a restart.

## Context

pi fires `session_shutdown` for `quit`, `reload`, `new`, `resume`, and `fork`
(`event.reason`). Only `quit` means the process is going away; the others are
**session-runtime** transitions where the pi process (and its tmux pane) stays
alive and the extension is reloaded/rebound. Treating every `session_shutdown` as
"session ended" would remove live sessions from the phone on a mere `/new`, and
tearing down the daemon there would break persistence.

## Decision

The extension branches on `event.reason`:

| reason | action |
|--------|--------|
| `quit` | mark the pi session ended; **never** kill tmux or the daemon. |
| `reload` | release per-session subscriptions; re-register on next `session_start`. |
| `new` / `resume` / `fork` | release subscriptions; on the following `session_start`, register the **new** pi session id (pi→tmux mapping can change within one tmux session's life). |

The extension never stops the daemon or kills tmux from `session_shutdown`.
Long-lived resources are started in `session_start` and are idempotent.

## Consequences

- **+** No false "session ended" events on `/new`,`/resume`,`/fork`,`/reload`.
- **+** The daemon and tmux persist across pi session switches (FR-9/FR-12).
- **−** The extension must handle re-registration and possible mapping changes.