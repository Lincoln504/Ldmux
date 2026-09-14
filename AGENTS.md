# Agent Instructions — ldmux

`ldmux` is a tmux-native remote terminal system: a pi extension + daemon + CLI +
landscape-only mobile app, reachable only over Tailscale. See
[`docs/SPEC.md`](docs/SPEC.md) for the authoritative design and
[`docs/INVESTIGATION.md`](docs/INVESTIGATION.md) for the evidence base.

## Status

Phase 1 (local core) is implemented and tested; Phase 2 (pi extension) is done
(verified in a real `pi` run); Phase 3 (remote transport) is implemented
(loopback WebSocket + token auth — `docs/REMOTE.md`) along with the buildless web
client served at `/` (installable PWA — `docs/WEB.md`), the desktop mirror, and
tailnet pairing. Deliberate divergences from dooboxes/gmux are recorded in
`docs/adr/0006-divergences.md`. Keep building what is requested; do not expand
scope unilaterally. `docs/PI-EXTENSIONS.md` is the contract for the extension.

## Rules

- **Tests + types are the gate.** `bun test` and `bun run typecheck` must both
  pass before committing; the typecheck is strict and has no build step.
- **tmux is the source of truth.** Never introduce a second session/PTY layer.
- **Tailnet only.** Never bind a public interface; never disable auth.
- **Scroll fidelity is sacred.** Any terminal UI change must preserve the
  `following`/`anchored` model (SPEC §7.4). Output must never move the viewport.
- **Never split engines per client.** One terminal renderer behind one
  `TerminalSurface` seam.
- **Defer long-lived resources** in the pi extension to `session_start` and clean
  up in `session_shutdown` (pi extension contract). **Branch on `event.reason`:**
  only `quit` means the process is exiting; never stop the daemon or kill tmux on
  `new`/`resume`/`fork`/`reload`. See `docs/PI-EXTENSIONS.md`.
- **Pin cross-component contracts with e2e tests in the same commit.**
- Keep the design honest: update `docs/SPEC.md` (and add ADRs under `docs/adr/`)
  when decisions change.

## Conventions

- **Nix toolchain:** develop and test inside `nix develop` (bun + tmux + just are
  provided by `flake.nix`). Do not add unpinned system dependencies.
- **Shared contract:** all shared types + the socket client live in
  `packages/protocol`. There is no build step — consumers import the protocol
  module directly (relative import) so the code loads under both Bun (daemon/CLI)
  and pi's jiti loader (extension). Do not add a bundling step without an ADR.
- **Daemon is a separate process.** The pi extension must never host long-lived
  state; it supervises the daemon and talks to it over the Unix socket.
- Both gates must pass before a commit: `bun test` and `bun run typecheck`.
- **`Bun.spawn` ignores a mutated `process.env`** (it uses the environment
  snapshot from process start), so `process.env.PATH = stubDir + ":" + real`
  before spawning does *not* pick up a stub binary — pass `env` explicitly
  (`env: process.env`) and the change is seen. `exec.ts` and the tmux control
  plane both do this. A test that stubs a binary must also assert the stub
  *ran* (e.g. a marker file): a missing-socket `tmux` also yields
  `listSessions() === null`, so a timeout test can silently pass against the
  real binary.
- **`Bun.spawnSync` deadlocks when the process under test lives in the same
  process** (it blocks the event loop the daemon needs). CLI/integration tests
  must use async `Bun.spawn` and drain via `Response(proc.stdout).text()`.
- Live tests spawn a scratch tmux server; clean it up with
  `killTmuxServer()` (`packages/daemon/test/tmux-cleanup.ts`), not a bare
  `tmux -L <unique> kill-server` — that leaves the socket file behind when the
  server has already exited. `just clean-tmux` reaps strays after an interrupted
  run, and never touches non-ldmux servers.
- **Docs lag code faster than anything else here.** When a change makes a
  documented statement false (a count, a path, an "out of scope"), grep the whole
  docs tree plus README/AGENTS for the old wording in the same commit.
- Conventional commits: `type(scope): summary`.
- TypeScript: strict; avoid new runtime deps (the extension uses plain JSON-Schema
  tool params instead of typebox).
- Rust (if/when used): match the style of the surrounding crate.
- Work on branches off `main`; open PRs against `main`.

## Environment facts (this machine, observed 2026-09-13)

- Tailscale host `debian.tail496756.ts.net` (`100.127.119.54`); system `tailscaled`
  v1.102.3. Phone: `motorola-edge-2024`.
- These are observations, not guarantees: re-check before relying on any of them
  (the devShell toolchain — currently bun 1.3.x, node 24, tmux 3.7b — is pinned
  by `flake.nix`; system tmux/node are older and not used for tests).
- i3 4.24 / X11 (`DISPLAY=:0.0`); workspaces `1..10` (`Mod+0` → `10`).
- System toolchain (unpinned, informational): alacritty, tmux 3.5a, Node 25.8.2,
  Bun 1.3.9, Rust 1.95, Go 1.24.4. Tests run against the *devShell* versions.
- pi 0.85.1, including `pi --mode rpc` (see `~/.config/nvm/.../pi-coding-agent/docs/rpc.md`).
- gh authed as `Lincoln504`.