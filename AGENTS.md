# Agent Instructions — ldmux

ldmux is a remote terminal system: a daemon + CLI + native mobile client,
reachable only over Tailscale. The daemon owns the PTYs. A **keeper** process
allocates them and supervises the shells, running one **libghostty-vt** terminal
emulator per session — the same engine the mobile client renders with, so the
snapshot and the client share one cell model.

The implementation is **Rust**, in `crates/`. tmux is **gone**: the keeper is the
session layer, and the TypeScript tree that preceded it was deleted at D4 (the
history of that tree is in git; `docs/RUST-PORT-PLAN.md` and
`docs/RUST-PORT-LOG.md` are the record of the port).

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
`docs/RUST-PORT-PLAN.md` is the design of the session-layer replacement (the tmux
ledger, the keeper topology, the snapshot-parity gate) and `docs/RUST-PORT-LOG.md`
is the findings + state record.

## Status

- The Rust daemon **is** the deployed daemon (`systemctl --user restart ldmux`
  runs `~/.local/bin/ldmuxd`). The keeper is auto-spawned by the daemon (the
  ssh-agent pattern) and survives a daemon restart. One unit.
- `cargo test --workspace` is the gate. See `docs/RUST-PORT-LOG.md` for the exact
  gate and any open item.

## Rules

- **Tests are the gate.** `cargo test --workspace`, inside `nix develop` (the QR
  decode tests need `zbarimg`, which only the devShell has). `just test` is the
  shorthand.
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

- **Nix toolchain.** Develop and test inside `nix develop` (cargo + rustc + just
  + node and zbar come from `flake.nix`). Do not add unpinned system dependencies.
- **Rust**: workspace under `crates/`, no code-generation, std + the plan §0
  allowlist (`tokio`, `tokio-tungstenite`, `serde`, `clap`, `thiserror`, `ring`,
  `uuid`, `nix`, `rand`, `fs2`); edition 2024; tests in-crate (`#[cfg(test)]`)
  or `tests/`; match the style of the surrounding crate.
- **Daemon is a separate process.** Clients talk to it over the Unix socket; the
  daemon owns every long-lived resource.
- **Children are spawned detached, in their own process group, and killed by
  group.** Signalling only the process we know about leaves *its* children
  holding the pipes open.
- **A stub binary must be proven to have run** (e.g. a marker file): a
  missing-socket binary also yields "no sessions", so a timeout test can
  silently pass against the real binary.
- **Anything a window manager launches must use an absolute path.** `i3-msg exec`
  runs with i3's own environment, which has no `~/.local/bin` on `PATH`; a bare
  program name is `not found` and the window closes instantly. This has bitten
  this project twice.
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
- The flake pins the devShell toolchain (Node 26 + cargo/rustc, from
  `flake.nix`). tmux is no longer in it.
- **Deploy with `just install`**, never by hand-editing `~/.local/bin` or the
  unit: `deploy/` is the source of truth for both.
- iOS builds need a macOS host.
- gh authed as `Lincoln504`.