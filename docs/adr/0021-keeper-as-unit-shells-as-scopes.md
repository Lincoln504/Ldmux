# ADR-0021 — The keeper is a unit; every shell is a scope

**Status:** Accepted (2026-10-06). Amends ADR-0015 and ADR-0018 (restart survival no longer rests on `KillMode=process` of the daemon unit).

- **Date:** 2026-10-06
- **Related:** ADR-0018 (the keeper), `docs/SPEC.md` §4.3, `deploy/ldmux.service`, `deploy/ldmux-keeper.service`

## Context

The daemon was a systemd user unit and the keeper a child it spawned detached.
A detached process cannot leave the cgroup it started in, so the keeper, every shell,
and every process any shell ever daemonized (a qemu VM, php servers) lived in
`ldmux.service`'s cgroup. Measured on the deployed machine:

- systemd attributed **4.6 GB** (peak **19 GB**, with swap) to the daemon unit;
- `systemctl --user status ldmux` was a wall of unrelated processes;
- the journal filled with `Found left-over process ... in control group while starting unit`;
- restart survival depended entirely on `KillMode=process`, a property of one unit file
  (without it a restart killed every session, `docs/DESKTOP-INTEGRATION.md`).

## Decision

1. **`ldmux-keeper.service`** (Type=simple, `KillMode=process`, `Restart=on-failure`)
   runs `ldmux-keeper %t/ldmux/keeper.sock`: the daemon's state dir plus `keeper.sock`.
   It carries the same `Environment`/`EnvironmentFile` as the daemon (shells inherit
   them), and a `RuntimeDirectory=ldmux` kept across stops. `ldmux.service` has
   `Wants=` and `After=` it, and sets `LDMUX_KEEPER_UNIT=ldmux-keeper.service`.
2. **The daemon starts the keeper through systemd.** When `LDMUX_KEEPER_UNIT` is set
   and `systemctl` is found (absolute path, resolved from PATH and `/usr/bin:/bin`),
   "ensure the keeper" is `systemctl --user start <unit>` (idempotent) and then the
   existing wait on the socket. If that command fails, the daemon falls back to the
   detached spawn. The socket connect remains the only liveness test and the flock
   still serializes daemons; the keeper refuses to bind a socket a live keeper serves,
   so a unit start and a detached spawn cannot leave two.
3. **Each shell gets its own scope** under `ldmux-shells.slice`
   (`ldmux-shell-<session>-<pid>.scope`). After forking, the keeper calls the user
   manager's `StartTransientUnit` with `PIDs=[child pid]`, `Slice`, and
   `CollectMode=inactive-or-failed`, through `busctl` (absolute path, resolved once at
   start). The child is **held on a pipe before it execs** until the move is visible in
   `/proc/<pid>/cgroup`, so nothing it forks can land in the keeper's cgroup. The scope
   is **not** stopped when the shell is killed: a job the user daemonized keeps running,
   now in its own scope. The keeper's process-group kill and reaping are unchanged.
4. **Failure never blocks a shell.** The scope step is a request about an existing
   process. If `busctl` is missing or fails (no bus, collision, the process already
   gone) or the move does not show within 2 s, the keeper logs one line and releases
   the child, which then runs in the keeper's cgroup exactly as before.
5. **Off switch and conditions.** Scopes are attempted only when `XDG_RUNTIME_DIR` is
   set, `$XDG_RUNTIME_DIR/bus` exists and `busctl` is found. `LDMUX_NO_SCOPE=1`
   disables them (e.g. in `~/.config/ldmux/env`). `LDMUX_SCOPE_SLICE` overrides the slice
   (tests).

## Alternatives

- **Keep the daemon-spawned keeper and `KillMode=process`.** Works, and is what ran. It
  is the problem: all memory and processes charged to the daemon, and one unit-file
  property carries the whole design.
- **One scope for the keeper only.** Moves the keeper out, but every shell and
  daemonized job still shares one cgroup: no per-session accounting.
- **A systemd service per shell.** Heavier (unit files or full transient services with
  stdio handled by systemd), and the PTY slave and process-group handling the keeper does
  would have to be re-expressed as unit settings; a scope adopts a process the keeper
  already forked.
- **`systemd-run --scope -- <shell>` exec'd in the child.** It moves itself and execs,
  so the pid is unchanged (verified on a throwaway scope), but a failure before the exec
  and the shell exiting with the same status are indistinguishable: a
  `systemd-run ... || exec shell` fallback can start the shell twice, and no fallback
  can start it zero times. Asking for the scope from the parent, on a held child, has
  no such ambiguity.
- **sd_bus (libsystemd) instead of `busctl`.** Avoids an exec per shell (about 20 ms) but
  adds a C dependency to a workspace that allows none, and an unaccounted pinned bus
  client in a long-lived process. `busctl` ships with systemd.

## Consequences

- `systemctl --user status ldmux` shows the daemon; `ldmux-keeper` the keeper; memory per
  session is `systemctl --user status ldmux-shell-<id>-<pid>.scope` or the slice's total.
- A keeper restart (`scripts/restart-keeper.sh`, `systemctl --user restart ldmux-keeper`)
  still ends every shell: the PTY masters close. A daemon restart touches neither.
- The user manager stopping (no linger, last logout) still ends everything.
- Migration from a daemon-spawned keeper ends every shell once: the old keeper holds the
  socket and must go before the unit's keeper can bind (`restart-keeper.sh` does this).
- Machines without systemd, and tests, keep the previous behaviour: the daemon spawns
  the keeper detached and shells are direct children in the keeper's cgroup.
- A scope outlives its shell while a daemonized job runs; it is collected when empty.
- Tests that run the real keeper binary under a user manager create short-lived scopes in
  `ldmux-shells.slice`.
