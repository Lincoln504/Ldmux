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

> **Status: design / spec phase.** Nothing here is implemented yet. The
> authoritative design is [`docs/SPEC.md`](docs/SPEC.md); the evidence base from
> which it was derived is [`docs/INVESTIGATION.md`](docs/INVESTIGATION.md).

## Documents

| Doc | Purpose |
|-----|---------|
| [`docs/SPEC.md`](docs/SPEC.md) | The authoritative software specification. |
| [`docs/TRANSPORT.md`](docs/TRANSPORT.md) | Transport deep-dive + decision (control mode `-C` over pipes, no PTY). |
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
bun test               # unit + live tmux tests (run inside nix develop)
just daemon            # run ldmuxd in the foreground
just cli run shell     # create a session and attach tmux
just link-extension    # symlink the pi extension, then /reload in pi
bun run packages/daemon/src/probe.ts <tmux-socket> <session>   # stream a pane
```

The live tests spawn a scratch tmux server (`tmux -L <unique>`) and exercise the
control-mode transport end to end; they require `tmux` on `PATH` (provided by the
devShell).

## Status

**Phase 1 complete; Phase 2 (pi extension) underway** (see [`docs/SPEC.md`](docs/SPEC.md) §13). Implemented and
tested (**35 tests**, `bun test`):

- `packages/protocol` — one canonical frame codec, control message types, runtime
  path resolution, and the typed daemon RPC contract + socket client (shared by
  every consumer).
- `packages/daemon` — tmux control-mode parser + live client; control plane;
  JSON session registry with reconciliation + pruning; Unix-socket JSON daemon
  (pid-lockfile single-instance, owner-only 0600 socket); CLI
  (`ping`/`list`/`new`/`run`/`send`/`key`/`capture`/`snapshot`/`kill`/`adopt`).
- `packages/extension` — pi extension: ensures the daemon is running, registers the
  pi session (reason-branched lifecycle), and exposes a `/ldmux` command + `ldmux`
  tool. Verified loading under pi's jiti loader and end-to-end from `session_start`.

See [`docs/PI-EXTENSIONS.md`](docs/PI-EXTENSIONS.md) for the pi extension contract.

Not yet: tailnet transport/auth, mobile app, desktop mirror.