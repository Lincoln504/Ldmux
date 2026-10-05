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
- **Amendment, 2026-10-05: the two tmux-era names were renamed, not kept.** The
  first version of this ADR kept the registry/wire field `tmuxName` and the ping
  field `tmuxSocket` (renaming them is a protocol change) and the spawn env
  `LDMUX_TMUX_NAME` / `LDMUX_TMUX_SOCK` (a tmux-era shell profile might read
  them). All four are gone: the fields are `keeperName` and `keeperSocket`
  (the installed app must be rebuilt; the registry migrates the old key once on
  load) and the env aliases are no longer set (nothing on the box read them). The
  `paneInMode` snapshot flag and the `copymode` message went with them.
- The keeper is a new single point of failure where tmux was. It is ~2k lines
  we own, with a crash policy documented in the plan (§6 risk tests).

## How it is held

- The keeper's own tests (`crates/keeper`) cover PTY allocation, supervision,
  reaping, the emulator task, and the socket.
- The daemon's suite runs against a real keeper and against a fake keeper.
- The snapshot-parity gate (R1) runs in the normal suite against committed
  goldens (`crates/daemon/tests/parity/`, captured once from a reference
  emulator on 2026-10-05; regenerate with `scripts/regen_parity_golden.sh`).
  Originally it was an `#[ignore]`d harness against a live tmux.

## What this does not decide

- `docs/MESH-TRANSPORT.md` (multi-host) is untouched.
- The tmux *investigation* (`docs/TMUX-DEPENDENCY.md`) is kept as the evidence
  base for this decision, not deleted.