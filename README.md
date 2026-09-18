# ldmux

A **tmux-native remote terminal**: a daemon + CLI on the host, and a **native**
Kotlin Multiplatform mobile client, reachable **only over Tailscale**.

`ldmux` is the successor idea to [`gmux`](https://gmux.app) and borrows heavily
from the lessons of the local `dooboxes` project — but it is *not* a fork of
either. It is deliberately simpler and tmux-native:

- **tmux is the single source of truth.** Every session is a tmux session. There
  is no custom PTY/session layer to keep in sync.
- **Tailnet only.** System Tailscale is already present on this machine
  (`debian.tail496756.ts.net`, `100.127.119.54`); no public exposure, no hosted relay.
- **Six panes of thought on the client, a thin daemon on the host.** The viewing
  client owns the pane geometry (ADR-0008) and paints the terminal itself, from a
  native VT emulator (`docs/NATIVE-TERMINAL.md`, ADR-0007), so there is no
  re-rendering of a server-side screenshot and no WebView.
- **Light on the network by construction.** Output is coalesced on the daemon, the
  client repaints on its own clock rather than per packet, and predictive local
  echo is available (off by default) — `docs/PREDICTIVE-ECHO.md`.

> **Status: Phase 1 (local core) + Phase 3 (remote transport) done; the native
> mobile client is in progress.** 210 tests pass
> (`node --test "packages/*/test/*.test.ts"`), typecheck clean. The authoritative
> design is [`docs/SPEC.md`](docs/SPEC.md); the transport decision is
> [`docs/TRANSPORT.md`](docs/TRANSPORT.md); prior art is
> [`docs/INVESTIGATION.md`](docs/INVESTIGATION.md).
>
> Note: `SPEC.md` predates two decisions and still describes their old state —
> the runtime is **Node 26**, not Bun, and the **pi extension has been removed**
> ([ADR-0009](docs/adr/0009-node-runtime-and-no-pi-extension.md)).

## Documents

| Doc | Purpose |
|-----|---------|
| [`docs/SPEC.md`](docs/SPEC.md) | The authoritative software specification. |
| [`docs/TRANSPORT.md`](docs/TRANSPORT.md) | Transport deep-dive + decision (control mode `-C` over pipes, no PTY). |
| [`docs/REMOTE.md`](docs/REMOTE.md) | Remote WebSocket transport, auth, and framing. |
| [`docs/NATIVE-TERMINAL.md`](docs/NATIVE-TERMINAL.md) | The native terminal engine (libghostty-vt) and the JNI seam. |
| [`docs/PREDICTIVE-ECHO.md`](docs/PREDICTIVE-ECHO.md) | Perceived latency: what can be predicted, and what must not be. |
| [`docs/adr/`](docs/adr/) | Architecture decision records. |
| [`AGENTS.md`](AGENTS.md) | Instructions for agents working in this repo. |

## Naming

The user's working name for the old idea was "Doombox"/"Dooboxes"; the existing
`~/Documents/dooboxes` project is a different (Rust, container-workspace) product.
`ldmux` is a **new, separate project** — no rename of `dooboxes` is performed.

## Development

The project uses a **Nix flake** for a reproducible toolchain (Node 26, tmux,
just). `packages/mobile` additionally needs the Android/KMP toolchain
(`nix develop .#mobile`).

```bash
nix develop            # node 26.8.2, npm 11.19.1, tmux 3.7b, just
npm install            # one runtime dep: ws
node --test "packages/*/test/*.test.ts"   # unit + live tmux tests
node --run typecheck   # tsc --noEmit (strict, no build step)
just daemon            # run ldmuxd in the foreground
just cli run shell     # create a session and attach tmux
just cli list          # sessions (and their tmux names)
just cli mirror <id>   # read-only desktop mirror (i3 workspace 10)
just pair              # phone url + ldmux:// deep link + tailscale serve command
just expose            # expose the loopback remote over the tailnet
just latency <id>      # round-trip echo latency against a session
```

There is **no build step**: Node strips the types, so `node
packages/daemon/src/main.ts` is the daemon. That requires the source to stay
*erasable* (no `enum`, no `namespace`, no constructor parameter properties);
`erasableSyntaxOnly` in `tsconfig.json` enforces it. See
[ADR-0009](docs/adr/0009-node-runtime-and-no-pi-extension.md).

The live tests spawn a scratch tmux server (`tmux -L <unique>`) and exercise the
control-mode transport end to end; they require `tmux` on `PATH` (provided by the
devShell).

## Status

Implemented and tested (210 tests, typecheck clean):

- `packages/protocol` — one canonical frame codec, control message types, runtime
  path resolution, and the typed daemon RPC contract + socket client (shared by
  every consumer).
- `packages/daemon` — tmux control-mode parser + live client; control plane; a
  stateful **output sanitizer** (drops OSC 52/DCS/device-status before a client
  sees them); JSON session registry with reconciliation and pruning; Unix-socket
  JSON daemon (pid-lockfile single-instance, owner-only 0600 socket); **live
  `PaneStream` fan-out** with per-subscriber output coalescing; **loopback
  WebSocket remote transport with bearer-token auth** (`docs/REMOTE.md`);
  desktop mirror + tailnet pairing; CLI (`ping`/`list`/`new`/`run`/`send`/`key`/
  `capture`/`snapshot`/`kill`/`forget`/`adopt`/`mirror`/`remote`/`pair`/`expose`).
- `packages/testing` — the assertion vocabulary (`test`/`expect`) the suite uses,
  over `node:test`.
- The daemon serves **no** static files. It used to ship a buildless xterm.js
  browser client at `/`; that client has been deleted, because native KMP/Compose
  is the client (ADR-0007) and a second renderer meant two copies of the snapshot
  and mode-sequence logic to keep correct. `GET /` now answers 404 with
  `ldmux remote: the endpoint is /ws`, so the port explains itself.
- `packages/mobile` — native KMP + Compose Multiplatform client (Android first).
  A Compose `Canvas` renderer over a **libghostty-vt** emulator behind a narrow
  JNI seam; client-driven pane resize; two button-switched UI modes (landscape
  side panel / portrait typing) with locked orientations; scrollback, cursor
  rendering, tap-to-position, and predictive local echo at a low, adaptive frame
  rate. See [`docs/NATIVE-TERMINAL.md`](docs/NATIVE-TERMINAL.md) and
  [`packages/mobile/README.md`](packages/mobile/README.md).
- pairing/desktop helpers — `ldmux pair` / `ldmux expose` (tailscale URL +
  `ldmux://` deep link) and `ldmux mirror` (read-only i3 + alacritty view).

Not yet: iOS (needs a macOS host for the xcframework), and the mobile client's
Compose renderer hardening (cursor blink, zoom).

Design divergences from dooboxes/gmux are recorded in
[`docs/adr/0006-divergences.md`](docs/adr/0006-divergences.md).