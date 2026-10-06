# ADR-0015 — The session layer: tmux keeps the PTYs

**Status:** Superseded by [ADR-0018](0018-session-layer.md) (2026-10-04). Kept as the record of the survey.

> **Superseded (2026-10-04, D4).** The tmux session layer this ADR decided on is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018 and `docs/archive/RUST-PORT-LOG.md`.

> Superseded by ADR-0018, which is the decision that replaced it.
>
> Amended by ADR-0021 (2026-10-06): the keeper is its own systemd unit and each shell is
> a scope; restart survival no longer rests on the daemon unit's `KillMode=process`.

- **Date:** 2026-09-19
- **Supersedes:** nothing
- **Related:** ADR-0001 (transport), ADR-0003 (session lifecycle), `docs/archive/TMUX-DEPENDENCY.md`

## Context

ldmux depends on tmux for three distinct things, and they had been discussed as if
they were one:

1. **PTY ownership** — a process owns the pseudo-terminals, so shells outlive our daemon.
2. **State ownership** — the pane's screen and its scrollback are held somewhere durable.
3. **Transport** — how pane bytes reach a client.

The user asked for the option of building (1) ourselves to be investigated properly,
"including how to solve whatever issues would be", with robustness and permanence
considered to whatever degree the design needs. This ADR records what that investigation
found and why it is **not** being built.

## The investigation

Six systems that solve this problem were examined: **dtach**, **abduco**, **mosh**,
**Zellij**, **VS Code's pty host**, and **WezTerm's mux server**. Full detail in
`docs/archive/TMUX-DEPENDENCY.md` §9.

**Every one of them uses the same shape**: a long-lived supervisor process owns the PTY
master file descriptors, and clients attach over a Unix socket and reattach after a
restart. **None of the six uses SCM_RIGHTS descriptor passing.** The PTY never leaves the
process that opened it. This is the single most useful finding, because it removes the
one mechanism that would have made the design look exotic.

Their answers to "what happens to output while nobody is attached" differ, and the
difference matters more than the topology:

| System | While detached |
|---|---|
| dtach, abduco | **Discarded.** The pty fd is not in the `select()` set with no clients, so it is never read |
| mosh | Processed through the emulator, **dropped** at the UDP layer |
| Zellij | Held in the server's **in-memory pane grid** |
| WezTerm | Held in the emulator's `Surface`, reattach sends only changed lines |
| VS Code | Buffered in the host, with **ack-based flow control** |

And in **all six, a supervisor crash loses every session.** VS Code restarts its host up
to five times and loses the sessions each time. WezTerm has no persistence at all.

## Decision

**tmux keeps the PTYs. This layer is not built.**

## Why

1. **The capability we would be buying already exists and is measured.** A daemon restart
   does not touch a running shell — verified repeatedly, including a restart with three
   live sessions. Nothing in the survey beats that, and two of the six (dtach, abduco)
   are *worse* on the property that matters most, because they discard output while
   detached and keep no screen state at all.

2. **The hard part of this layer is not the terminal.** The VT engine is already in the
   client, so the escape-sequence problem is solved. What is left is process supervision,
   PTY lifecycle, reaping, resize negotiation and backpressure — the part where a mistake
   costs the user their work rather than a test failure, and the part that tmux has been
   getting right for twenty years.

3. **Permanence is a property we would have to add, not inherit.** Zellij serialises
   layouts to disk every second and still cannot restore a running process. Any design we
   wrote would either hold sessions only in memory (crash = loss, like all six) or
   introduce a persistence mechanism that is its own project.

4. **The transport question is separable and cheaper.** `docs/archive/TMUX-DEPENDENCY.md` §8
   measured `pipe-pane` and found it byte-verbatim, lossless and bounded, so the
   maintainability argument that motivated "build our own" can be answered by replacing
   *control mode* with a pipe — 595 lines deleted — without touching PTY ownership at all.

## What would have to be true to revisit this

Stated so the decision can be falsified rather than merely believed:

- tmux becomes unmaintained, or its server stops being packaged on the hosts we target.
- A requirement appears that tmux cannot express and that is not a transport concern —
  for example per-session CPU or memory accounting, or a hard guarantee about what
  survives a host reboot.
- `pipe-pane` turns out to be unreliable in production, removing the cheap transport fix
  and making the protocol the only reason to leave.

## Consequences

- The dependency is accepted deliberately, not by accident. `docs/archive/TMUX-DEPENDENCY.md` §4
  records what it costs and what has already been paid for it.
- The keeper design is documented but **not planned**, and no work is scheduled from it.
  A future implementer has the six-system survey and the detach-buffering table, which is
  the expensive part of the research.
- If the pipe transport wins its A/B, the protocol parser goes and tmux's role narrows to
  the one thing it is genuinely best at here: owning the PTYs and holding the screen.
