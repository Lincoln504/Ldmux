# Agent Instructions — ldmux

ldmux is a remote terminal system: a daemon + CLI + native mobile client,
reachable only over Tailscale. The daemon owns the PTYs. A **keeper** process
allocates them and supervises the shells, running one **libghostty-vt** terminal
emulator per session — the same engine the mobile client renders with, so the
snapshot and the client share one cell model.

The implementation is **Rust**, in `crates/`. The keeper is the
session layer (there is no terminal multiplexer), and the TypeScript tree that preceded it was deleted at D4 (the
history of that tree is in git; `docs/archive/RUST-PORT-PLAN.md` and
`docs/archive/RUST-PORT-LOG.md` are the record of the port).

| crate | contents |
|---|---|
| `protocol` | frames, messages, rpc, pairing, net, paths, socket |
| `keeper` | PTYs, child supervision + reaping, ghostty-vt per session, keeper socket server |
| `daemon` | registry, ownership, remote WS, streams, desktop + inventory, CLI (`bin/ldmux`), daemon entry (`bin/ldmuxd`) |
| `attach` | the desktop-window client (`ldmuxd attach`) |
| `vt-sys` | hand-written FFI against the vendored libghostty-vt headers |

`packages/mobile` is the KMP/Compose client and the JNI shim; it is not part of
the port and is kept.

`docs/SPEC.md` is the authoritative design. `docs/adr/` is the decision record.
`docs/STATE.md` is the current position and `docs/PLATFORMS.md` the support matrix.
`docs/archive/RUST-PORT-PLAN.md` (the keeper topology, the snapshot-parity gate)
and `docs/archive/RUST-PORT-LOG.md` (findings) are historical records of the port.

## Status

- The Rust daemon **is** the deployed daemon (`systemctl --user restart ldmux`
  runs `~/.local/bin/ldmuxd`). The keeper is auto-spawned by the daemon (the
  ssh-agent pattern) and survives a daemon restart. One unit.
- `cargo test --workspace` is the gate. The exact commands are in the README's
  Development section; open items are in `docs/STATE.md`.

## Rules

- **Tests are the gate.** `cargo test --workspace`, inside `nix develop` (the QR
  decode tests need `zbarimg`, which only the devShell has). `nix flake check`
  runs the same gate in the nix sandbox.
- **Lint is clean.** `cargo clippy --workspace --all-targets -- -D warnings`,
  zero warnings: a lint that is tolerated is a lint nobody reads.
- **Tailnet only.** Never bind a public interface; never disable auth.
- **Scroll fidelity is sacred.** Any terminal UI change must preserve the
  `following`/`anchored` model (SPEC §7.4). Output must never move the viewport.
- **Never split engines per client.** One terminal renderer (libghostty-vt)
  behind one seam.
- **The keeper is the PTY owner.** The daemon spawns it detached; it outlives the
  daemon (restart survival). `KillMode=process` + linger are load-bearing — see
  the unit header.
- **Pin cross-component contracts with e2e tests in the same commit.**
- Keep the design honest: update `docs/SPEC.md` (and add ADRs under
  `docs/adr/`) when decisions change, and grep the docs tree plus README/AGENTS
  for the old wording in the same commit.

## Conventions

- **Nix toolchain.** Develop and test inside `nix develop` (cargo + rustc
  + zbar come from `flake.nix`; Node 26 is there too, dev-only, for `nix run .#mobile-test`
  and `scripts/`). Do not add unpinned system dependencies.
- **Rust**: workspace under `crates/`, no code-generation, std + this
  direct-dependency allowlist: `tokio`, `tokio-tungstenite`, `serde`, `serde_json`,
  `futures-util`, `uuid`, `nix`, `rand`, and `flate2` with the pure-Rust `miniz_oxide`
  backend (permessage-deflate, ADR-0019; no C, no system zlib). `deny.toml` bans TLS
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
  `hello`/`attach` (`inplaceResync`, `sessionEnded`); a client that says nothing gets
  what it always got. A new keeper field must work against a running old keeper (the
  keeper outlives the daemon and is replaced only by ending every shell).
- **Windows are launched by the daemon, not the window manager.** The daemon spawns the
  terminal emulator directly (template table, `LDMUX_TERMINAL_CMD` with `{cmd}`/`{class}`);
  i3/sway/hyprland placement is an optional adapter.
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