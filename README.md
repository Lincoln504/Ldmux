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
| [`docs/INVESTIGATION.md`](docs/INVESTIGATION.md) | Investigation log: what exists on this machine, prior art, and citations. |
| [`AGENTS.md`](AGENTS.md) | Instructions for agents working in this repo. |

## Naming

The user's working name for the old idea was "Doombox"/"Dooboxes"; the existing
`~/Documents/dooboxes` project is a different (Rust, container-workspace) product.
`ldmux` is a **new, separate project** — no rename of `dooboxes` is performed.