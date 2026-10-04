# ldmux

A **remote terminal**: a daemon + CLI on the host, and a **native** Kotlin
Multiplatform mobile client, reachable **only over Tailscale**.

`ldmux` is the successor idea to [`gmux`](https://gmux.app) and borrows heavily
from the lessons of the local `dooboxes` project — but it is *not* a fork of
either. It is deliberately simple:

- **The keeper is the single source of truth.** Every session is a PTY owned by a
  `ldmux-keeper` process; the daemon auto-spawns the keeper and talks to it over
  a Unix socket. There is no tmux and no second session layer.
- **Tailnet only.** System Tailscale is already present on this machine
  (`debian.tail496756.ts.net`, `100.127.119.54`); no public exposure, no hosted relay.
- **Six panes of thought on the client, a thin daemon on the host.** The viewing
  client owns the pane geometry (ADR-0008) and paints the terminal itself, from a
  native VT emulator (`docs/NATIVE-TERMINAL.md`, ADR-0007), so there is no
  re-rendering of a server-side screenshot and no WebView.
- **Light on the network by construction.** Output is coalesced on the daemon, the
  client repaints on its own clock rather than per packet, and predictive local
  echo is available (off by default) — `docs/PREDICTIVE-ECHO.md`.

> **Status: the Rust daemon is deployed and running.** `crates/daemon`
> (`ldmuxd`) is the live daemon under the systemd user unit; the keeper owns the
> PTYs and runs libghostty-vt; the native mobile client is done and
> device-verified for Android, with iOS deferred as a build. The Rust workspace
> (`cargo test --workspace`, inside `nix develop`) and the TypeScript source of
> record (`node --run test` + `node --run typecheck`) are the two gates. The
> authoritative design is [`docs/SPEC.md`](docs/SPEC.md); the transport decision
> is [`docs/TRANSPORT.md`](docs/TRANSPORT.md); prior art is
> [`docs/INVESTIGATION.md`](docs/INVESTIGATION.md); the port's own record is
> [`docs/RUST-PORT-LOG.md`](docs/RUST-PORT-LOG.md).

**Current position, open work and known unknowns: [`docs/STATE.md`](docs/STATE.md).**

## Documents

| Doc | Purpose |
|-----|---------|
| [`docs/STATE.md`](docs/STATE.md) | **Where everything stands**: what is verified and how, what is open, what is not known. Start here. |
| [`docs/SPEC.md`](docs/SPEC.md) | The authoritative software specification (its tmux sections are historical). |
| [`docs/RUST-PORT-PLAN.md`](docs/RUST-PORT-PLAN.md) | The Rust port + tmux-removal plan, and its crate map. |
| [`docs/RUST-PORT-LOG.md`](docs/RUST-PORT-LOG.md) | The port's continuity record: what was done, what was found, what is open. |
| [`docs/RUST-PORT-AUDIT.md`](docs/RUST-PORT-AUDIT.md) | The critical audit of the port. |
| [`docs/REMOTE.md`](docs/REMOTE.md) | Remote WebSocket transport, auth, and framing. |
| [`docs/NATIVE-TERMINAL.md`](docs/NATIVE-TERMINAL.md) | The native terminal engine (libghostty-vt) and the JNI seam. |
| [`docs/WIRE-COST.md`](docs/WIRE-COST.md) | What each interaction actually costs on the wire, measured. |
| [`docs/DESKTOP-INTEGRATION.md`](docs/DESKTOP-INTEGRATION.md) | Putting a shell on the machine's own screen, and the i3 binding. |
| [`docs/IOS-DEVELOPMENT.md`](docs/IOS-DEVELOPMENT.md) | What the iOS build needs, and what is still unknown about it. |
| [`docs/PREDICTIVE-ECHO.md`](docs/PREDICTIVE-ECHO.md) | Perceived latency: what can be predicted, and what must not be. |
| [`docs/TMUX-DEPENDENCY.md`](docs/TMUX-DEPENDENCY.md) | Historical: the decision about how much of the session layer should be tmux's (decided: none). |
| [`docs/adr/`](docs/adr/) | Architecture decision records. |
| [`AGENTS.md`](AGENTS.md) | Instructions for agents working in this repo. |

## Naming

The user's working name for the old idea was "Doombox"/"Dooboxes"; the existing
`~/Documents/dooboxes` project is a different (Rust, container-workspace) product.
`ldmux` is a **new, separate project** — no rename of `dooboxes` is performed.

## Development

The project uses a **Nix flake** for a reproducible toolchain (Node 26 for the
TypeScript source of record, rustup for the Rust workspace, tmux for the parity
harness, just). `packages/mobile` additionally needs the Android/KMP toolchain
(`nix develop .#mobile`).

```bash
nix develop                  # node 26.8.2, rustc/cargo, tmux 3.7b, just, zbarimg
cargo build --workspace      # build every Rust crate
cargo test --workspace       # the Rust gate (keeper, daemon, protocol, vt-sys, attach)
node --run test              # the TypeScript gate (the source of record)
node --run typecheck         # tsc --noEmit (strict, no build step)
just install                 # build the release workspace and install ldmuxd/ldmux/ldmux-keeper
just rust-daemon             # run the Rust daemon in the foreground
just rust-cli list           # sessions, through the Rust CLI
just cli run shell           # create a session and attach
just remote                  # remote host, port, token, and whether it is listening
just code                    # pairing QR code for the phone to scan (ADR-0017)
just pair                    # the same pairing, as an ldmux:// link
just latency <id>            # round-trip echo latency against a session
```

The TypeScript tree has **no build step** (Node strips the types; the source
must stay *erasable*; `erasableSyntaxOnly` enforces it — see
[ADR-0009](docs/adr/0009-node-runtime-and-no-pi-extension.md)). It is retained as
the source of record and the parity corpus while the tmux removal completes; the
deployed daemon is the Rust one.

## Status

Implemented and tested (see [`docs/STATE.md`](docs/STATE.md) §4.2 for the counts
and the commands that produce them):

- `crates/protocol` — one canonical frame codec, the daemon RPC contract, the
  keeper message set, runtime path resolution, the pairing link, and the socket
  client, shared by every consumer.
- `crates/keeper` — the session layer: a PTY per session in its own process
  group, libghostty-vt in-process, the `Spawn`/`Kill`/`Write`/`Resize`/`Read`/
  `Peek`/`Snapshot`/`List`/`Close` server.
- `crates/vt-sys` — the FFI surface of libghostty-vt (the vendored `.a`).
- `crates/daemon` — the RPC daemon (Unix socket + pid lockfile), the session
  registry with reconciliation and pruning, ownership with the holder guard and
  the idle sweep, a `PaneStream` fan-out with per-subscriber coalescing, the
  **WebSocket remote transport on the tailnet address with bearer-token auth at
  the HTTP upgrade** (ADR-0016, `docs/REMOTE.md`), the desktop mirror, tailnet
  pairing, and the CLI (`ldmux`).
- `crates/attach` — the desktop desk-window client (a daemon remote-WS client).
- `packages/mobile` — native KMP + Compose Multiplatform client (Android first).
  A Compose `Canvas` renderer over a **libghostty-vt** emulator behind a narrow
  JNI seam; client-driven pane resize; **the shell view follows the device's
  rotation and the app owns the keyboard** — portrait is typing with a two-row
  bar, landscape is the side panel, and there is no flip button; the launcher
  holds portrait (ADR-0011); scrollback, cursor rendering, tap-to-position, and
  predictive local echo at a low, adaptive frame rate. See
  [`docs/NATIVE-TERMINAL.md`](docs/NATIVE-TERMINAL.md) and
  [`packages/mobile/README.md`](packages/mobile/README.md).
- The daemon serves **no** static files. `GET /ws` is the only endpoint; a plain
  GET answers 401/400 so the phone's connection probe can tell a refused token
  from a machine that is off.

Not yet: iOS (needs a macOS host for the xcframework), the mobile client's
Compose renderer hardening (cursor blink, zoom), and the final tmux removal from
the flake (the deployed daemon does not use it).

Design divergences from dooboxes/gmux are recorded in
[`docs/adr/0006-divergences.md`](docs/adr/0006-divergences.md).