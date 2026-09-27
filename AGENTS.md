# Agent Instructions — ldmux

`ldmux` is a tmux-native remote terminal system: a daemon + CLI + native mobile
app, reachable only over Tailscale. See
[`docs/SPEC.md`](docs/SPEC.md) for the authoritative design and
[`docs/INVESTIGATION.md`](docs/INVESTIGATION.md) for the evidence base.

## Status

Phase 1 (local core) is implemented and tested; Phase 3 (remote transport) is
implemented (WebSocket on the tailnet address + token auth — `docs/REMOTE.md`, ADR-0016) along with the
desktop mirror, and tailnet pairing. The daemon serves no static files: the old
buildless browser client (`packages/web`) is deleted, so native KMP/Compose is
the client. The mobile app
(`docs/NATIVE-TERMINAL.md`, `docs/adr/0007`, `docs/adr/0008`). Deliberate
divergences from dooboxes/gmux are recorded in `docs/adr/0006-divergences.md`.
The pi extension was removed (ADR-0009): ldmux is not a pi plugin. Keep building
what is requested; do not expand scope unilaterally.

## Rules

- **Tests + types are the gate.** `nix develop --command node --run test` and
  `node --run typecheck` must both pass before committing; the typecheck is strict
  and there is no build step (Node strips the types). **Inside `nix develop` is part
  of the command, not a preference.** The suite drives scratch tmux servers, and the
  daemon resolves its tmux from `LDMUX_TMUX_BIN` before PATH — so running it in a
  shell that *is* an ldmux session points the daemon at the flake's runtime tmux
  while a bare `tmux` on PATH is the system's, and the two versions cannot share a
  socket. The result is 26 failures across five files that read like product defects
  (measured 2026-09-27). One test also needs `zbarimg`, which only the devShell has.

  **`node --run test`, not `node --test "packages/*/test/*.ts"`.** The script is
  `node --test --test-concurrency=4`, and the bound is load-bearing: `a real tmux
  %pause resyncs subscribers` needs tmux to notice a *lagging* client, and 12 files
  in parallel on a 12-core machine starve the producer until the client keeps up and
  the pause never happens. Measured 2026-09-27 on this host: the bare command failed
  that test and killed a whole file's process (4 tests never ran) while the script
  passed. A file whose process dies reports one failure and hides its tests, so treat
  "a file failed" in this suite as "its tests did not run" until proven otherwise.
- **tmux is the source of truth.** Never introduce a second session/PTY layer.
- **Tailnet only.** Never bind a public interface; never disable auth.
- **Scroll fidelity is sacred.** Any terminal UI change must preserve the
  `following`/`anchored` model (SPEC §7.4). Output must never move the viewport.
- **Never split engines per client.** One terminal renderer behind one
  `TerminalSurface` seam.
- **Pin cross-component contracts with e2e tests in the same commit.**
- Keep the design honest: update `docs/SPEC.md` (and add ADRs under `docs/adr/`)
  when decisions change.

## Conventions

- **Nix toolchain:** develop and test inside `nix develop` (Node 26 + tmux + just
  are provided by `flake.nix`). Do not add unpinned system dependencies.
- **Shared contract:** all shared types + the socket client live in
  `packages/protocol`. There is no build step — consumers import the protocol
  module directly, with an explicit `.ts` extension (Node's ESM resolver requires
  it, and it is also what makes type stripping work). Do not add a bundling step
  without an ADR.
- **The code must stay erasable.** Node runs the TypeScript directly by stripping
  types, so only erasable syntax is allowed: no `enum`, no `namespace`, no
  constructor parameter properties. `erasableSyntaxOnly` in `tsconfig.json`
  enforces this — the fix is always to declare the field and assign it.
- **Daemon is a separate process.** Clients talk to it over the Unix socket; the
  daemon owns every long-lived resource.
- Both gates must pass before a commit: the test command above and the typecheck.
- **Children are spawned detached, in their own process group, and killed by
  group** (`packages/daemon/src/proc.ts`). Signalling only the process we know
  about leaves *its* children holding the pipes open — a wedged `tmux` with a
  helper turned a 300 ms timeout into a 300-second hang, and only surfaced once
  the test runner stopped force-exiting.
- **A stub binary must be proven to have run** (e.g. a marker file): a
  missing-socket `tmux` also yields `listSessions() === null`, so a timeout test
  can silently pass against the real binary.
- **Never spawn synchronously in a test that also runs a daemon in-process**
  (`spawnSync` blocks the event loop the daemon needs). Use the async path in
  `packages/daemon/src/proc.ts`.
- **Node's `WebSocket` returns binary messages as a Blob** unless
  `binaryType = "arraybuffer"` is set; ldmux frames are binary, so every test
  client and the latency harness must set it.
- **Control messages on one connection are processed in send order** (chained per
  connection, like the Unix-socket dispatch). Do not fan them out concurrently:
  two `attach` messages racing makes which snapshot a client receives depend on
  transport timing.
- **`@ldmux/testing` is the assertion vocabulary.** Tests import `test`/`expect`
  from `packages/testing/src/expect.ts` (a small Jest-style `expect` over
  `node:test`), not from `bun:test` or `node:assert`. Add a matcher there rather
  than reaching for another library.
- Live tests spawn a scratch tmux server; clean it up with
  `killTmuxServer()` (`packages/daemon/test/tmux-cleanup.ts`), not a bare
  `tmux -L <unique> kill-server` — that leaves the socket file behind when the
  server has already exited. `just clean-tmux` reaps strays after an interrupted
  run, and never touches non-ldmux servers.
- **Docs lag code faster than anything else here.** When a change makes a
  documented statement false (a count, a path, an "out of scope"), grep the whole
  docs tree plus README/AGENTS for the old wording in the same commit.
- Conventional commits: `type(scope): summary`.
- TypeScript: strict; avoid new runtime deps. `ws` is the only production
  dependency (the WebSocket server side of `docs/REMOTE.md`); everything else is
  Node's standard library.
- Rust (if/when used): match the style of the surrounding crate.
- Work on branches off `main`; open PRs against `main`.

## Environment facts (this machine, observed 2026-09-13)

- Tailscale host `debian.tail496756.ts.net` (`100.127.119.54`); system `tailscaled`
  v1.102.3. Phone: `motorola-edge-2024`.
- These are observations, not guarantees: re-check before relying on any of them
  (the devShell toolchain — currently Node 26.8.2, npm 11.19.1, tmux 3.7b — is
  pinned by `flake.nix`; the system node/tmux are older and not used for tests).
- i3 4.24 / X11 (`DISPLAY=:0.0`); workspaces `1..10` (`Mod+0` → `10`).
- System toolchain (unpinned, informational): alacritty, tmux 3.5a, Node 25.8.2,
  Rust 1.95, Go 1.24.4. Neither tests **nor the deployed daemon** use them: `just
  install` runs the daemon on the flake's `.#runtime` (the devShell's pins).
- **Deploy with `just install`**, never by hand-editing `~/.local/bin` or the unit:
  `deploy/` is the source of truth for both.
- iOS builds need a macOS host (an xcframework cannot be produced here).
- gh authed as `Lincoln504`.