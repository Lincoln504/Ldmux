# Agent Instructions — ldmux

ldmux is a remote terminal system: a daemon + CLI + native mobile client,
reachable over Tailscale or — from the Android client — over LoRa with MeshCore
radios (`docs/ARCHITECTURE.md` section 11). The daemon owns the PTYs. A **keeper** process
allocates them and supervises the shells, running one **libghostty-vt** terminal
emulator per session — the same engine the mobile client renders with, so the
snapshot and the client share one cell model.

The implementation is **Rust**, in `crates/`. The keeper is the
session layer (there is no terminal multiplexer), and the TypeScript tree that preceded it was deleted at D4
(the history of that tree is in git; the port's plan and log were two of the
documents consolidated away at `731970f`, and are readable at `c3d7c56`).

| crate | contents |
|---|---|
| `protocol` | frames, messages, rpc, pairing, net, paths, socket |
| `keeper` | PTYs, child supervision + reaping, ghostty-vt per session, keeper socket server |
| `daemon` | registry, ownership, remote WS, streams, desktop + inventory, CLI (`bin/ldmux`), daemon entry (`bin/ldmuxd`) |
| `attach` | the desktop-window client (`ldmuxd attach`) |
| `vt-sys` | hand-written FFI against the vendored libghostty-vt headers |
| `companion` | the host's MeshCore companion radio driver over USB serial |
| `mesh` | the L2 client for MeshCore radios: frames, commands, events (no tokio state) |
| `session` | the L3 session over that link: Noise `IKpsk2`, message transactions, retry ladder, replay window, and the phone's engine behind `ldmux_client_*`. Pure, no I/O |
| `session-ffi` | the C ABI the mobile client calls instead of reimplementing the session in Kotlin |
| `radio-harness` | the pieces the radio tests share (a port, a fake companion radio, the chain that joins two ends); a dev-dependency, never shipped |

`packages/mobile` is the KMP/Compose client and the JNI shim; it is not part of
the port and is kept.

`docs/STATE.md` is the one-page state of the project: what works, what does not, the
public-mesh rule, the feasibility arithmetic, and the open questions. Read it first.
`docs/ARCHITECTURE.md` is the authoritative design: layers, wire, invariants,
security, failure modes, platform support. `docs/RADIO-PLAN.md` is the architectural
redesign and plan of record for the radio path, paired with `docs/RADIO-AUDIT-REVIEW.md`
(the consolidated root-cause, firmware, and integration audit; the intermediate 2026-11-10
scratchpads were consolidated into it and remain readable at `9cf722e`). `docs/adr/` is
the decision record, and `docs/adr/README.md` lists every one with its status.
`docs/RELEASE-AUDIT.md` is the open work, aspect by aspect, and the store material
(with `docs/SECURITY.md`, `docs/PRIVACY.md`, and `docs/AIRTIME.md`). `README.md` is for a
person meeting the project. A change that alters behaviour updates all docs that it touches,
in the same commit.

## Status

- The Rust daemon **is** the deployed daemon (`systemctl --user restart ldmux`
  runs `~/.local/bin/ldmuxd`). Under systemd the keeper is its own unit,
  `ldmux-keeper.service`, started by the daemon (`systemctl --user start`, ADR-0021) and
  every shell lives in its own scope under `ldmux-shells.slice`; both survive a daemon
  restart. Without systemd the daemon auto-spawns the keeper detached (the ssh-agent
  pattern).
- `scripts/test-parallel.py` is the gate (it runs the same binaries `cargo test
  --workspace` does, one process per binary, in a pool). The exact commands are in
  the README's Development section; open items are in `docs/RELEASE-AUDIT.md`.

## Rules

- **Tests are the gate.** `scripts/test-parallel.py`, inside `nix develop` (the QR
  decode tests need `zbarimg`, which only the devShell has). It builds with
  `cargo test --no-run`, reads the executables out of the JSON, and runs them in a
  pool: 48 binaries in about 80 s against 118 s for `cargo test --workspace`,
  which runs them one at a time. `cargo test --workspace` remains the authority on
  what the gate is; the script only changes how the same binaries are scheduled.
  `nix flake check` runs `cargo test --workspace` in the nix sandbox.
- **A test that waits out a fixed timeout is a bug in the test.** Make the deadline
  and the tick configurable through the daemon builder (the `LDMUX_*_GRACE_MS` env
  vars and `with_*` hooks exist for this), then assert the deadline with a short
  budget. A real wait — three seconds proving a keeper came back — stays, and says
  in a comment why it is that long.
- **Lint is clean.** `cargo clippy --workspace --all-targets -- -D warnings`,
  zero warnings: a lint that is tolerated is a lint nobody reads.
- **Tailnet only.** Never bind a public interface; never disable auth.
- **Scroll fidelity is sacred.** Any terminal UI change must preserve the
  `following`/`anchored` model (SPEC §7.4). Output must never move the viewport.
- **Never split engines per client.** One terminal renderer (libghostty-vt)
  behind one seam.
- **The keeper is the PTY owner.** It outlives the daemon (restart survival): under
  systemd because it is its own unit (`ldmux-keeper.service`, ADR-0021), otherwise
  because the daemon spawns it detached, where `KillMode=process` on the daemon unit is
  load-bearing. Linger is load-bearing either way. A shell that cannot get a scope must
  still start (`LDMUX_NO_SCOPE=1` turns scopes off). See the unit headers.
- **Pin cross-component contracts with e2e tests in the same commit.**
- Keep the design honest: update `docs/ARCHITECTURE.md` and `docs/RELEASE-AUDIT.md`
  (and add ADRs under `docs/adr/`) when decisions change, and grep the docs tree plus
  README/AGENTS for the old wording in the same commit.

## Conventions

- **Nix toolchain.** Develop and test inside `nix develop` (cargo + rustc
  + zbar come from `flake.nix`; Node 26 is there too, dev-only, for `nix run .#mobile-test`
  and `scripts/`). Do not add unpinned system dependencies.
- **Rust**: workspace under `crates/`, no code-generation, std + this
  direct-dependency allowlist: `tokio`, `tokio-tungstenite`, `serde`, `serde_json`,
  `futures-util`, `uuid`, `nix`, `rand`, and `flate2` with the pure-Rust `miniz_oxide`
  backend (permessage-deflate, ADR-0019; no C, no system zlib), and, in `crates/session` only,
  `snow`, `hmac`, `sha2`, `zeroize` (ADR-0023; no C, no `ring`; `crates/session-ffi` adds none of its own and is the only crate that exposes the session over a C ABI). `deny.toml` bans TLS
  stacks and HTTP clients. Adding a dependency needs an ADR line; edition 2024; tests in-crate (`#[cfg(test)]`)
  or `tests/`; match the style of the surrounding crate.
- **Daemon is a separate process.** Clients talk to it over the Unix socket; the
  daemon owns every long-lived resource. The desk attach client reaches the daemon over
  its local attach socket (`<control>-attach.sock`, `0600`, peer uid checked), not the
  tailnet; the phone uses the tailnet WebSocket. Both end in the same connection loop.
- **The daemon owns the WebSocket server frame layer** (`crates/daemon/src/ws.rs`), so
  `permessage-deflate` can be negotiated; tungstenite is the client side only.
- **Ownership claims persist** in `<state>/claims.json` (60 s restore grace); the state
  directory and the token file must be owned by the daemon's uid.
- **Wire changes are additive.** New messages and fields are gated by `features` in
  `hello`/`attach` (`inplaceResync`); a client that says nothing gets what it always
  got. A new keeper field must work against a running old keeper (the
  keeper outlives the daemon and is replaced only by ending every shell).
- **A window is spawned only when nobody is at a terminal.** `ldmux attach` runs the desk
  client in the terminal you already have; the phone's send-to-desktop is the only spawn,
  in the terminal you configured — `$TERMINAL`, then `xdg-terminal-exec`, then the
  desktop's own setting, then an emulator already running, then the known list. `ldmux
  terminal` prints that answer and the wrapper asks it rather than copying the chain. There are
  no `LDMUX_TERMINAL*` variables. i3/sway/hyprland placement is an optional adapter.
- **The unit reads an optional `~/.config/ldmux/env`** (`LDMUX_REMOTE_HOST`,
  `LDMUX_REMOTE_PORT`); nothing machine-specific goes in `deploy/ldmux.service`.
- **A fresh clone builds with nix alone.** `nix develop` builds libghostty-vt from the
  commit pinned in `packages/mobile/native/ghostty.pin` and exports `LDMUX_VT_LIB_DIR`;
  outside nix, point that variable at a directory holding `libghostty-vt.a`. There is
  no `just`: use cargo, `nix build`, `nix run .#install` (`scripts/install.sh`),
  `nix run .#restart-keeper` (`scripts/restart-keeper.sh`, ends every shell) and
  `scripts/android.sh`.
- **One resolver for every path.** The daemon, the CLI and the attach client all
  resolve the socket through `protocol::paths`. A hand-rolled fallback (the CLI
  had one: `$HOME/.ldmux/ldmuxd.sock`) looks somewhere the daemon never binds and
  every command fails with "No such file or directory".
- **Children are spawned detached, in their own process group, and killed by
  group.** Signalling only the process we know about leaves *its* children
  holding the pipes open.
- **A stub binary must be proven to have run** (e.g. a marker file): a
  missing-socket binary also yields "no sessions", so a timeout test can
  silently pass against the real binary.
- **Anything a window manager or the user manager launches must use an absolute
  path.** Their environment has no `~/.local/bin` on `PATH`; a bare program name is
  `not found` and the window closes instantly. This has bitten this project twice.
- **Live tests clean up after themselves.** Kill the scratch keeper and remove
  the scratch state dir in the test; a leaked keeper holds PTYs and a socket for
  the life of the machine. Never `pkill -f` a path that could match the running
  daemon's keeper — use an explicit pid or an exact `-x` name.
- Conventional commits: `type(scope): summary`.
- Work on branches off `main`; open PRs against `main`.

## Environment facts (observed; re-check before relying)

- Tailscale host `debian.tail496756.ts.net` (`100.127.119.54`); phone
  `motorola-edge-2024`.
- i3 4.24 / X11 (`DISPLAY=:0.0`); workspaces `1..10` (`Mod+0` → `10`).
- The flake pins the devShell toolchain (cargo/rustc, plus a second pin for dev-only
  Node 26, from `flake.nix`).
- **Deploy with `nix run .#install`** (or `scripts/install.sh <bindir>`), never by hand-editing `~/.local/bin` or the
  unit: `deploy/` is the source of truth for both.
- iOS builds need a macOS host.
- gh authed as `Lincoln504`.