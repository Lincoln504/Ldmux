# ldmux

A **tmux-native remote terminal** built as a **pi coding-agent extension**, with a
landscape-only **mobile client** that connects **only over Tailscale**.

`ldmux` is the successor idea to [`gmux`](https://gmux.app) and borrows heavily
from the lessons of the local `dooboxes` project — but it is *not* a fork of
either. It is deliberately simpler and tmux-native:

- **tmux is the single source of truth.** Every session (shell or `pi`) is a tmux
  session. There is no custom PTY/session layer to keep in sync.
- **pi integration by construction.** A pi extension starts/announces the daemon
  whenever a pi session is alive, so `ldmux` is always "up and ready to go".
- **Tailnet only.** System Tailscale is already present on this machine
  (`debian.tail496756.ts.net`, `100.127.119.54`); no public exposure, no hosted relay.
- **Mobile-first UX.** Landscape-only; terminal gets ~3/4 of the screen, a
  configurable left/right control quarter holds keys + the session pager;
  horizontal swipe switches sessions; scroll position is stable (the #1 gmux pain
  point).

> **Status: Phase 1 (local core) + Phase 2 (pi extension) done; Phase 3 (remote
> transport) + the web client implemented.** 43 tests pass. The authoritative
> design is [`docs/SPEC.md`](docs/SPEC.md); the transport decision is
> [`docs/TRANSPORT.md`](docs/TRANSPORT.md); prior art is
> [`docs/INVESTIGATION.md`](docs/INVESTIGATION.md).

## Documents

| Doc | Purpose |
|-----|---------|
| [`docs/SPEC.md`](docs/SPEC.md) | The authoritative software specification. |
| [`docs/TRANSPORT.md`](docs/TRANSPORT.md) | Transport deep-dive + decision (control mode `-C` over pipes, no PTY). |
| [`docs/REMOTE.md`](docs/REMOTE.md) | Remote WebSocket transport, auth, and framing. |
| [`docs/WEB.md`](docs/WEB.md) | The buildless web/terminal client served by the daemon. |
| [`docs/INVESTIGATION.md`](docs/INVESTIGATION.md) | Investigation log: what exists on this machine, prior art, and citations. |
| [`AGENTS.md`](AGENTS.md) | Instructions for agents working in this repo. |

## Naming

The user's working name for the old idea was "Doombox"/"Dooboxes"; the existing
`~/Documents/dooboxes` project is a different (Rust, container-workspace) product.
`ldmux` is a **new, separate project** — no rename of `dooboxes` is performed.

## Development

The project uses a **Nix flake** for a reproducible toolchain (bun, node, tmux,
just); the pinned `nixpkgs` rev is shared with the `dooboxes` flake so the store
and binary cache are reused.

```bash
nix develop            # bun 1.3.x, node 24, tmux 3.7b, just
bun install            # dev-only type definitions (@types/bun, typescript)
bun test               # unit + live tmux tests (run inside nix develop)
bun run typecheck      # tsc --noEmit (strict, no build step)
just daemon            # run ldmuxd in the foreground
just cli run shell     # create a session and attach tmux
just cli list          # sessions (and their tmux names)
just cli mirror <id>   # read-only desktop mirror (i3 workspace 10)
just pair              # phone url + ldmux:// deep link + tailscale serve command
just expose            # expose the loopback remote over the tailnet
just link-extension    # symlink the pi extension, then /reload in pi
bun run packages/daemon/src/probe.ts <tmux-socket> <session>   # stream a pane
```

The live tests spawn a scratch tmux server (`tmux -L <unique>`) and exercise the
control-mode transport end to end; they require `tmux` on `PATH` (provided by the
devShell).

## Status

**Phase 1 complete; Phase 2 (pi extension) + Phase 3 (remote transport + web client) implemented** (see [`docs/SPEC.md`](docs/SPEC.md) §13). Implemented and
tested (**53 tests**, `bun test`; `bun run typecheck` clean):

- `packages/protocol` — one canonical frame codec, control message types, runtime
  path resolution, and the typed daemon RPC contract + socket client (shared by
  every consumer).
- `packages/daemon` — tmux control-mode parser + live client; control plane; JSON
  session registry with reconciliation + pruning; Unix-socket JSON daemon
  (pid-lockfile single-instance, owner-only 0600 socket); **live `PaneStream`
  fan-out**; **loopback WebSocket remote transport with bearer-token auth**
  (`docs/REMOTE.md`); CLI
  (`ping`/`list`/`new`/`run`/`send`/`key`/`capture`/`snapshot`/`kill`/`adopt`/`remote`).
- `packages/extension` — pi extension: ensures the daemon is running, registers the
  pi session (reason-branched lifecycle), and exposes a `/ldmux` command + `ldmux`
  tool. Verified loading under pi's jiti loader and end-to-end from `session_start`
  in a real `pi` run. Only **root** pi sessions register; fork/subagent children
  are skipped.
- `packages/web` — buildless xterm.js client served by the daemon at `/`
  (canonical grid scaled to fit, session pager, swipe, mobile key bar,
  reconnect/resync). See [`docs/WEB.md`](docs/WEB.md).
- pairing/desktop helpers — `ldmux pair` / `ldmux expose` (tailscale URL +
  `ldmux://` deep link) and `ldmux mirror` (read-only i3 + alacritty view).

See [`docs/PI-EXTENSIONS.md`](docs/PI-EXTENSIONS.md) for the pi extension contract.

Not yet: Android WebView shell, desktop mirror.