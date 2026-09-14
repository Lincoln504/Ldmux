# Agent Instructions — ldmux

`ldmux` is a tmux-native remote terminal system: a pi extension + daemon + CLI +
landscape-only mobile app, reachable only over Tailscale. See
[`docs/SPEC.md`](docs/SPEC.md) for the authoritative design and
[`docs/INVESTIGATION.md`](docs/INVESTIGATION.md) for the evidence base.

## Status

Phase 1 (local core) is implemented and tested; Phase 2 (pi extension) is done
(verified in a real `pi` run); Phase 3 (remote transport) is in progress
(loopback WebSocket + token auth — see `docs/REMOTE.md`). Keep building what is
requested; do not expand scope unilaterally. `docs/PI-EXTENSIONS.md` is the
contract for the extension.

## Rules

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
- Run `bun test` after changes; live tests spawn a scratch tmux server and must
  clean it up (`tmux -L <unique> kill-server`).
- Conventional commits: `type(scope): summary`.
- TypeScript: strict; avoid new runtime deps (the extension uses plain JSON-Schema
  tool params instead of typebox).
- Rust (if/when used): match the style of the surrounding crate.
- Work on branches off `main`; open PRs against `main`.

## Environment facts (this machine)

- Tailscale host `debian.tail496756.ts.net` (`100.127.119.54`); system `tailscaled`
  v1.102.3. Phone: `motorola-edge-2024`.
- i3 4.24 / X11 (`DISPLAY=:0.0`); workspaces `1..10` (`Mod+0` → `10`).
- alacritty; tmux 3.5a (control mode OK); Node 25.8.2; Bun 1.3.9; Rust 1.95; Go 1.24.4.
- pi 0.85.1, including `pi --mode rpc` (see `~/.config/nvm/.../pi-coding-agent/docs/rpc.md`).
- gh authed as `Lincoln504`.