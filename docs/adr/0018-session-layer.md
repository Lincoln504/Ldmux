# ADR-0018 — The session layer is the keeper

**Status:** accepted, 2026-10-04 (the D4 step of `docs/RUST-PORT-PLAN.md`).
**Supersedes:** ADR-0015 (tmux keeps the PTYs), ADR-0001 (control-mode transport),
and ADR-0006 §2 (ldmux does not own a PTY layer).

## Decision

`crates/keeper` is the session layer. It allocates the PTY for each session,
supervises and reaps the child, runs one **libghostty-vt** emulator per session,
and owns the scrollback grid. tmux is not involved at any point, and the
TypeScript session layer is deleted.

Every session is one keeper session: one PTY, one emulator, one stable id. The
daemon owns the registry and the remote transport, and talks to the keeper over a
Unix socket using the same message vocabulary it speaks to clients.

## Why

- **One terminal engine, both sides.** The client renders with libghostty-vt
  (ADR-0007). Serving snapshots from tmux made a second engine the source of
  truth for the grid, which is what the parity harness (plan §3.11) existed to
  police. With the keeper running the same engine, the snapshot and the client
  agree by construction rather than by test.
- **The compatibility surface was the product risk.** The tmux profile had
  version-dependent options (`-J`/`-N` fallbacks, `window-size`, `history-limit`,
  `default-terminal`), and a profile option that crashed tmux 3.5a passed every
  test and took the deployed daemon down. The spawn env is seven variables and
  does not vary by version.
- **The control-mode parser was unspecified text.** 160 lines parsing an
  undocumented protocol, with a second pipe path beside it, and an A/B harness to
  compare them. Both are gone.
- **Restart survival is preserved, by the same topology.** The keeper is spawned
  detached and outlives the daemon, exactly as the tmux server did: the daemon is
  one unit with `KillMode=process`, and linger keeps the user manager from
  stopping on the last logout.

## Consequences

- A window is a protocol client, not a tmux client: `alacritty -e ldmuxd attach
  <id>`. There is no shim script and no tty polling. The path must be
  **absolute** — `i3-msg exec` runs with i3's own environment, which has no
  `~/.local/bin` on `PATH`.
- The daemon clamping a requested pane size is load-bearing, not defensive: the
  client's frame carries a selection's rows in one byte, so a pane above 255 rows
  silently wraps them (`protocol::MAX_PANE_ROWS`).
- **Two names are kept although they are now false, both deliberate:**
  - the registry field `keeperName` and the wire field `keeperSocket` — they are the
    pairing/session contract with the installed app, and the app reads
    `keeperName` to decide whether a session is attachable. Renaming them is a
    protocol change, not a cleanup.
  - `LDMUX_TMUX_NAME` / `LDMUX_TMUX_SOCK` in the spawn env, because a shell
    profile written against the tmux era reads them. The truthful
    `LDMUX_SESSION_NAME` and `LDMUX_KEEPER_SOCK` are set alongside.
- The keeper is a new single point of failure where tmux was. It is ~2k lines
  we own, with a crash policy documented in the plan (§6 risk tests).

## How it is held

- The keeper's own tests (`crates/keeper`) cover PTY allocation, supervision,
  reaping, the emulator task, and the socket.
- The daemon's suite runs against a real keeper and against a fake keeper.
- The snapshot-parity harness against tmux is kept, `#[ignore]`d, as the R1
  evidence artifact: with tmux deleted there is nothing to compare against, and
  the last green run is recorded in `docs/RUST-PORT-LOG.md`.

## What this does not decide

- `docs/MESH-TRANSPORT.md` (multi-host) is untouched.
- The tmux *investigation* (`docs/TMUX-DEPENDENCY.md`) is kept as the evidence
  base for this decision, not deleted.