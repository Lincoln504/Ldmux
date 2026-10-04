# Agent Instructions — ldmux

ldmux is a remote terminal system: a daemon + CLI + native mobile client,
reachable only over Tailscale. The daemon owns the PTYs. A **keeper** process
allocates them and supervises the shells, running one **libghostty-vt** terminal
emulator per session — the same engine the mobile client renders with, so the
snapshot and the client share one cell model.

The implementation is **Rust**, in `crates/`:

| crate | contents |
|---|---|
| `protocol` | frames, messages (incl. keeper messages), rpc, pairing, net, paths, socket |
| `keeper` | PTYs, child supervision + reaping, ghostty-vt per session, keeper socket server |
| `daemon` | registry, ownership, remote WS, streams, desktop + inventory, CLI (`bin/ldmux`), daemon entry (`bin/ldmuxd`) |
| `attach` | the desktop-window client (`ldmuxd attach`) |
| `vt-sys` | hand-written FFI against the vendored libghostty-vt headers |

The legacy TypeScript tree (`packages/{daemon,protocol,testing}`) and tmux are
**being removed**. Until the switch is complete both trees exist; the deployed
daemon is still the TS one, and tmux is still the live session layer. Never
introduce a second session/PTY layer in either tree.

`docs/SPEC.md` is the authoritative design. `docs/RUST-PORT-PLAN.md` is the
design of the session-layer replacement (the tmux ledger, the keeper topology,
the snapshot-parity gate). `docs/RUST-PORT-LOG.md` is the findings + state
record. `docs/adr/` holds the decision record.

## Status

- **Rust**: the workspace builds and `cargo test --workspace` is green. The
  protocol, keeper, vt-sys, attach, and daemon crates are implemented; the
  daemon covers the lifecycle, control socket, remote WS, streams, desktop, and
  CLI. See `docs/RUST-PORT-LOG.md` for the exact gate and any open item.
- **TS**: `node --run test` + `node --run typecheck` are green. This is the
  deployed daemon.
- **Switch**: `just install` installs the Rust daemon + keeper and the one
  systemd unit; the keeper is auto-spawned by the daemon (the ssh-agent pattern),
  and survives a daemon restart. The TS unit comes back with one `systemctl`
  switch. The registry JSON is format-identical, so no data migrates either way.

## Rules

- **Tests + types are the gate.** Rust: `cargo test --workspace` (inside
  `nix develop`; the QR decode tests need `zbarimg`, which only the devShell
  has). TS (while it lives): `nix develop --command node --run test` and
  `node --run typecheck`. `just test` / `just rust-test` are the shorthands.
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

- **Nix toolchain.** Develop and test inside `nix develop` (cargo + rustc +
  tmux + just come from `flake.nix`). Do not add unpinned system dependencies.
- **Rust**: workspace under `crates/`, no code-generation, std + the plan §0
  allowlist (`tokio`, `tokio-tungstenite`, `serde`, `clap`, `thiserror`, `ring`,
  `uuid`, `nix`, `rand`, `fs2`); edition 2024; tests in-crate (`#[cfg(test)]`)
  or `tests/`; match the style of the surrounding crate.
- **TypeScript (legacy)**: strict, erasable syntax only (`erasableSyntaxOnly`),
  no bundling step, `@ldmux/testing` is the assertion vocabulary.
- **Daemon is a separate process.** Clients talk to it over the Unix socket; the
  daemon owns every long-lived resource.
- **Children are spawned detached, in their own process group, and killed by
  group.** Signalling only the process we know about leaves *its* children
  holding the pipes open.
- **A stub binary must be proven to have run** (e.g. a marker file): a
  missing-socket binary also yields "no sessions", so a timeout test can
  silently pass against the real binary.
- **Never spawn synchronously in a test that also runs a daemon in-process**
  (`spawnSync` blocks the event loop the TS daemon needs).
- **Live tests clean up after themselves.** Kill the scratch keeper / tmux
  server and remove the scratch state dir in the test; a leaked keeper holds
  PTYs and a socket for the life of the machine.
- Conventional commits: `type(scope): summary`.
- Work on branches off `main`; open PRs against `main`.

## Environment facts (observed; re-check before relying)

- Tailscale host `debian.tail496756.ts.net` (`100.127.119.54`); phone
  `motorola-edge-2024`.
- i3 4.24 / X11 (`DISPLAY=:0.0`); workspaces `1..10` (`Mod+0` → `10`).
- The flake pins the devShell toolchain (Node 26 + tmux 3.7b + cargo/rustc, from
  `flake.nix`). The system node/tmux are older and are not used for the tests.
- **Deploy with `just install`**, never by hand-editing `~/.local/bin` or the
  unit: `deploy/` is the source of truth for both.
- iOS builds need a macOS host.
- gh authed as `Lincoln504`.
