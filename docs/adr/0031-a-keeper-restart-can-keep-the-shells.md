# ADR-0031 — A keeper restart can keep the shells: fd custody and a terminal snapshot

**Status:** Proposed (2026-11-07). Not implemented.

- **Date:** 2026-11-07
- **Related:** `crates/keeper/src/lib.rs` (`spawn_session`, `Session`, `OutBuf`),
  `crates/daemon/src/keeper_supervisor.rs`, `deploy/ldmux-keeper.service`, ADR-0018 (the session
  layer is the keeper), ADR-0021 (the keeper is a unit, the shells are scopes),
  `docs/RELEASE-AUDIT.md` row 8.2, `packages/mobile/native/prebuilt/host/include/ghostty/vt/snapshot.h`

## Context

Replacing the keeper ends every shell. That is correct today and it was measured again on
2026-11-07: `systemctl --user restart ldmux-keeper` ended ten shells and their nine desk windows,
the daemon survived, and the registry was left clean. The cost is real — those were the operator's
live conversations — and it is the reason a keeper-side fix sits unloaded for days (row 8.2).

Two things die with the keeper, and they are different problems:

- the **master file descriptors** of every pty, and
- the **terminal model** per session: the ghostty-vt terminal, its scrollback, its modes, and the
  unfinished parser input.

## What was measured (2026-11-07, this machine)

1. **A pty outlives its owner if another live process holds the master.** With the master passed
   over `SCM_RIGHTS` to a second process, the first process can close it and exit, and the shell is
   still `alive/sleeping` 1.5 s later; the custodian then ran `echo` through that same master and
   read the answer. Without a custodian the shell is a zombie 1.2 s after the owner closes. When the
   custodian exits (the last holder), the shell dies. The pty is never closed and never reopened.
2. **An unread pty blocks its writer; it does not lose bytes.** `seq 1 200000` with nobody reading
   came back complete once reading resumed. The cost of a gap is a stalled pane, not a lost one.
3. **A new holder cannot learn the screen.** A marker printed before the handoff is not visible to
   the new holder, and `read` yields only new bytes. A resize (`TIOCSWINSZ`, SIGWINCH) makes bash
   redraw its prompt line but not its scrollback.
4. **The terminal model is transferable in this engine.** `libghostty-vt.a` under
   `packages/mobile/native/prebuilt/host/lib/` exports `ghostty_snapshot_encode_alloc` and the
   `ghostty_snapshot_decoder_*` family. An 80x24 terminal with SGR attributes, a hidden cursor and
   bracketed paste on encoded to **1256 bytes**, decoded into a new terminal, and re-encoded
   **byte-identical**, cursor `(0,3)` preserved. The engine's own header states the snapshot carries
   "any unfinished VT parser input".

## Decision

When this is implemented, the pty survives a keeper restart, and so does the picture:

1. **The daemon is the custodian.** `spawn_session` passes a duplicate of the master to the daemon
   (or the daemon creates the pair and passes the master to the keeper), and the daemon holds it for
   the session's whole life. The keeper then holds no unique reference, so its death no longer runs
   the hangup. Each process holds the other's resource, so **either** can be restarted: that is the
   "hold each other's shells" property, and it fits ADR-0021's "the keeper outlives the daemon".
2. **Adoption on start.** A new keeper asks the daemon for every master and adopts each over a
   socketpair (`SCM_RIGHTS`), instead of `openpty`. The shells never notice.
3. **The picture comes from a snapshot.** The keeper encodes each terminal with
   `ghostty_snapshot_encode_alloc` and the new keeper decodes it, so text, attributes, cursor,
   modes, alternate screen and scrollback pages are restored exactly — no reliance on the
   application repainting. Bytes are drained to ground before encoding
   (`ghostty_terminal_vt_write_until_ground`, `vt_ground`), because bytes inside the kernel's queues
   are not in the snapshot and no tool can capture them (CRIU documents the same limit).
4. **Cadence.** Encode on `SIGTERM`, which covers `systemctl restart` exactly, and periodically
   (the interval to be measured) for the crash path. On an unclean kill the shells still survive
   because of the custody, and the picture is whatever the last periodic snapshot holds.

## Consequences

- **What a restart costs afterwards:** the gap between the two keepers — one unit restart, about a
  second — during which nobody reads the master, so a busy pane blocks and a TUI freezes. Nothing is
  lost.
- **What a crash costs:** the shells survive; the screen falls back to the last periodic snapshot.
- **What both-dying costs:** shells die with the last holder. Custody raises the number of failures
  needed from one to two; it does not make session loss impossible.
- **Snapshot cost** must be measured before the cadence is chosen: 1256 B for a small pane, larger
  with a full scrollback.
- **Invariants that must hold:** only the keeper reads a master (a second reader steals bytes), the
  custodian's copies are `CLOEXEC` so no shell inherits them (the radio fd had exactly that bug),
  and a batch of descriptors must stay under Linux's 253-per-message `SCM_RIGHTS` limit — with
  `MAX_SESSIONS` 64, one message per session is the safe shape.
- **The wire does not change.** This is daemon↔keeper only; clients see an attach and a snapshot as
  they always did.

## Rejected

- **CRIU** — restores the pty's interface but explicitly not the kernel queues, needs privileges,
  and would still need the terminal model re-encoded.
- **reptyr** — moves a *program* to a new terminal with ptrace (blocked by Yama by default) and does
  not preserve a pty; it solves the opposite problem.
- **A tmux server underneath** — the keeper is the session layer (ADR-0018); tmux would also make the
  blast radius one process and lose the single-engine cell model.
- **A third "PTY bank" process** — the daemon already outlives the keeper, so a third process adds a
  failure domain and a protocol for nothing.
- **systemd's `FileDescriptorStore`** — the same mechanism with less code, but systemd-only (this
  project also runs with a daemon-spawned keeper), silently a no-op when the unit lacks
  `FileDescriptorStoreMax=`, and dropped on stop-then-start (only `restart` keeps it). Worth adding
  as a second custodian later; not as the only one.

## Landing

The change needs one keeper restart to take effect, and that restart is the last one that costs the
current shells. It therefore lands in a single change with its tests, not as a sequence of deploys.
