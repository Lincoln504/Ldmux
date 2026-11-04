# ldmux

A **remote terminal**: a daemon + CLI on the host, and a **native** Kotlin
Multiplatform mobile client, reachable **only over Tailscale**.

`ldmux` is the successor idea to [`gmux`](https://gmux.app) and borrows heavily
from the lessons of the local `dooboxes` project — but it is *not* a fork of
either. It is deliberately simple:

- **The keeper is the single source of truth.** Every session is a PTY owned by a
  `ldmux-keeper` process (under systemd its own unit, `ldmux-keeper.service`, with each
  shell in its own scope; otherwise the daemon auto-spawns it); the daemon talks to it over
  a Unix socket. There is no tmux and no second session layer.
- **Tailnet only.** The host runs Tailscale (`<host>.<tailnet>.ts.net`, `100.x.y.z`);
  no public exposure, no hosted relay.
- **A thin daemon on the host, a smart client.** The viewing
  client owns the pane geometry (ADR-0008) and paints the terminal itself, from a
  native VT emulator (`docs/NATIVE-TERMINAL.md`, ADR-0007), so there is no
  re-rendering of a server-side screenshot and no WebView.
- **Light on the network by construction.** Output is coalesced on the daemon, the
  WebSocket negotiates `permessage-deflate` when the client offers it (7x on a directory
  listing, 3-4x on a screenful; ADR-0019, `docs/WIRE-COST.md`), the client repaints on
  its own clock rather than per packet, and predictive local echo turns itself on when
  the round trip passes ~120 ms and off below ~80 ms (no setting) —
  `docs/PREDICTIVE-ECHO.md`. On a slow link the client also asks for smaller history
  pages and is repaired with a snapshot instead of a reconnect (`docs/WIRE-COST.md`).

> **Status: the Rust daemon is deployed and running.** `crates/daemon`
> (`ldmuxd`) is the live daemon under the systemd user unit; the keeper owns the
> PTYs and runs libghostty-vt; the native mobile client is done and
> device-verified for Android, with iOS not started. The one gate is the Rust
> workspace (`scripts/test-parallel.py`, inside `nix develop`). The
> authoritative design is [`docs/SPEC.md`](docs/SPEC.md); historical records (the
> port, the tmux study, the old state register) are in
> [`docs/archive/`](docs/archive/).

**Current position, open work and known unknowns: [`docs/STATE.md`](docs/STATE.md).**

## Documents

| Doc | Purpose |
|-----|---------|
| [`docs/STATE.md`](docs/STATE.md) | **Where everything stands**: what exists, what is deployed, the gate, open items. Start here. |
| [`docs/SPEC.md`](docs/SPEC.md) | The authoritative software specification (its tmux-era sections are marked historical). |
| [`docs/PLATFORMS.md`](docs/PLATFORMS.md) | What is supported where, and whether each claim is verified or intended. |
| [`docs/REMOTE.md`](docs/REMOTE.md) | Remote WebSocket transport, auth, and framing. |
| [`docs/NATIVE-TERMINAL.md`](docs/NATIVE-TERMINAL.md) | The native terminal engine (libghostty-vt) and the JNI seam. |
| [`docs/WIRE-COST.md`](docs/WIRE-COST.md) | What each interaction actually costs on the wire, measured. |
| [`docs/DESKTOP-INTEGRATION.md`](docs/DESKTOP-INTEGRATION.md) | Putting a shell on the machine's own screen, and the i3 binding. |
| [`docs/IOS-DEVELOPMENT.md`](docs/IOS-DEVELOPMENT.md) | What the iOS build needs, and what is still unknown about it. |
| [`docs/PREDICTIVE-ECHO.md`](docs/PREDICTIVE-ECHO.md) | Perceived latency: what can be predicted, and what must not be. |
| [`docs/MESH-STANDALONE.md`](docs/MESH-STANDALONE.md) | **LoRa / MeshCore, as built**: the phone owns its own radio over BLE, the host radio is reached through a gateway, and this is the record of the protocol, the encryption and the hardware runs. |
| [`docs/MESH-GATEWAY.md`](docs/MESH-GATEWAY.md) | The host half: the gateway that owns a USB radio, and the state file the daemon reads. |
| [`docs/MESH-HOST-RADIO.md`](docs/MESH-HOST-RADIO.md) | Bringing the host radio up: firmware, gateway, environment, and what each failure means. |
| [`docs/MESH-LATENCY.md`](docs/MESH-LATENCY.md) | What a mesh update costs on air, and what the software does about it. |
| [`docs/MESH-TRANSPORT.md`](docs/MESH-TRANSPORT.md) | The original investigation: MeshCore vs Meshtastic, and what was unknown at the time. |
| [`docs/HARDWARE-VALIDATION.md`](docs/HARDWARE-VALIDATION.md) | The radios, the firmware on them, and every measurement taken on real hardware. |
| [`docs/archive/`](docs/archive/) | Historical records: the Rust port plan, log and audit, the tmux study (decided: no tmux, ADR-0018), the pre-port investigation, the old state register. |
| [`docs/adr/`](docs/adr/) | Architecture decision records. |
| [`AGENTS.md`](AGENTS.md) | Instructions for agents working in this repo. |

## Naming

The user's working name for the old idea was "Doombox"/"Dooboxes"; the existing
`~/Documents/dooboxes` project is a different (Rust, container-workspace) product.
`ldmux` is a **new, separate project** — no rename of `dooboxes` is performed.

## Development

The project uses a **Nix flake** for a reproducible toolchain (rustc/cargo for
the workspace, zbar for the QR tests). Node 26 is also in the dev shell, but
only for `nix run .#mobile-test` (the mobile tree's `.ts` tests) and the two scripts
under `scripts/`; the Rust gate does not need it. `packages/mobile` additionally
needs the Android/KMP toolchain (`nix develop .#mobile`). Supported platforms:
[`docs/PLATFORMS.md`](docs/PLATFORMS.md) (today: x86_64 Linux for the host).

```bash
nix develop                  # rustc/cargo, zbarimg (+ node, dev-only); builds the engine once
cargo build --workspace      # build every Rust crate (LDMUX_VT_LIB_DIR is set by the shell)
scripts/test-parallel.py     # the gate: same binaries as `cargo test --workspace`, ~32 s instead of ~118 s
cargo test --workspace       # the same gate, one binary at a time (the authority; use it when a script is suspected)
cargo clippy --workspace --all-targets -- -D warnings   # lint; must be clean
cargo run -p ldmux-daemon --bin ldmuxd                  # the daemon, in the foreground
cargo run -q -p ldmux-daemon --bin ldmux -- list        # the CLI: list | run shell | remote | code | pair
nix build                    # the release binaries in ./result/bin (self-contained, own glibc)
nix flake check              # build + the test gate + clippy + fmt + mobile TS tests, in the sandbox
nix run .#install            # install the nix-built binaries + both units, ldmux and ldmux-keeper (FORCE=1 overwrites them)
nix run .#restart-keeper     # replace the running keeper (ENDS every shell)
nix run .#mobile-test        # the mobile tree's own TypeScript tests (needs node)
nix develop -c node scripts/measure_latency.ts --session <id> --runs 20   # round-trip echo latency
```

`remote` prints host, port, token and whether the daemon is listening; `code` is the
pairing QR code for the phone (ADR-0017); `pair` is the same pairing as an `ldmux://` link.

**A fresh clone builds with nix alone.** The terminal engine (libghostty-vt) is built by
a nix derivation from the single Ghostty commit pinned in
`packages/mobile/native/ghostty.pin`; `nix develop` exports `LDMUX_VT_LIB_DIR` pointing at
it, so plain `cargo` works inside the shell. Outside nix, set `LDMUX_VT_LIB_DIR` to a
directory holding `libghostty-vt.a` (the build script's error says how).

There is no TypeScript daemon any more: the tree was deleted at D4 (2026-10-04,
[ADR-0018](docs/adr/0018-session-layer.md)) together with the previous session
layer. Node remains a dev tool only.

## Status

Implemented and tested (run the gate for the counts; open items are in
[`docs/STATE.md`](docs/STATE.md)):

- `crates/protocol` — one canonical frame codec, the daemon RPC contract, the
  keeper message set, runtime path resolution, the pairing link, and the socket
  client, shared by every consumer.
- `crates/keeper` — the session layer: a PTY per session in its own process
  group, libghostty-vt in-process, the `Spawn`/`Kill`/`Write`/`Resize`/`Read`/
  `Peek`/`Snapshot`/`List`/`Close` server (`Peek` can be bounded, answering base64 chunks).
- `crates/vt-sys` — the FFI surface of libghostty-vt (the vendored `.a`).
- `crates/daemon` — the RPC daemon (Unix socket + pid lockfile), the session
  registry with reconciliation and pruning, ownership with the holder guard and
  the idle sweep, a `PaneStream` fan-out with per-subscriber coalescing, the
  **WebSocket remote transport on the tailnet address with bearer-token auth at
  the HTTP upgrade** (ADR-0016, `docs/REMOTE.md`) over the daemon's own WebSocket frame
  layer with permessage-deflate, a local `0600` attach socket for the desk window,
  persisted claims, ended-session reporting (`session_ended`, exit status), the desktop
  mirror (direct terminal spawn, optional i3/sway/hyprland placement), tailnet
  pairing, and the CLI (`ldmux`).
- `crates/attach` — the desktop desk-window client; it attaches over the daemon's local
  attach socket, so a desk window needs no tailnet.
- `packages/mobile` — native KMP + Compose Multiplatform client (Android first).
  A Compose `Canvas` renderer over a **libghostty-vt** emulator behind a narrow
  JNI seam; client-driven pane resize; **the shell view follows the device's
  rotation and the app owns the keyboard** — portrait is typing with a three-row
  bar, landscape is the side panel, and there is no flip button; the launcher
  holds portrait (ADR-0011); scrollback, cursor rendering, tap-to-position, a spatial
  arrow pad and a `paste` key in both modes, sticky Ctrl/Alt/Shift (so `Shift+Enter`
  is a newline), and predictive local echo at a low, adaptive frame rate. See
  [`docs/NATIVE-TERMINAL.md`](docs/NATIVE-TERMINAL.md) and
  [`packages/mobile/README.md`](packages/mobile/README.md).
- The daemon serves **no** static files. `GET /ws` is the only endpoint; a plain
  GET answers 401/400 so the phone's connection probe can tell a refused token
  from a machine that is off.

Not yet: iOS (not started; needs a macOS host for the xcframework) and the mobile client's
Compose renderer hardening (cursor blink, zoom).

Design divergences from dooboxes/gmux are recorded in
[`docs/adr/0006-divergences.md`](docs/adr/0006-divergences.md).