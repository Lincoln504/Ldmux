# ldmux: state of the project

Written 2026-10-06 on branch `fix/audit-hardening`. This is the short current
position. The design is `docs/SPEC.md` (its tmux-era sections are marked
historical), the decisions are `docs/adr/`, and the records of how the system got
here are in `docs/archive/`. Claims below name how they were checked; "read" means
checked against the source and not run.

## What exists

A remote terminal system reachable only over Tailscale: a daemon, a keeper, a CLI, a
desk-window client, and a native mobile client.

| Part | Where | State |
|---|---|---|
| Wire contract (frames, messages, keeper messages, rpc, pairing, net, paths) | `crates/protocol` | Implemented, unit-tested. |
| Keeper: PTYs, child supervision and reaping, one libghostty-vt per session | `crates/keeper` | Implemented. Auto-spawned by the daemon, outlives it. |
| FFI to libghostty-vt (static archive built by nix from the pinned commit) | `crates/vt-sys` | Implemented. Verified on x86_64 Linux (glibc); aarch64 Linux evaluates, unbuilt. |
| Daemon: registry, ownership, remote WebSocket, streams, desktop + inventory, CLI | `crates/daemon` (`ldmuxd`, `ldmux`) | Implemented and deployed. |
| Desk-window client (`ldmuxd attach`) | `crates/attach` | Implemented. |
| Mobile client (KMP + Compose, JNI shim over libghostty-vt) | `packages/mobile` | Android done and device-verified. iOS not started. |

There is no tmux and no TypeScript daemon: both were removed at D4 (2026-10-04,
ADR-0018). tmux survives only as a dev-time reference in
`scripts/regen_parity_golden.sh`. Node is a dev tool for `nix run .#mobile-test` and two
scripts, not for the Rust gate.

## What is deployed

- `systemctl --user` unit `ldmux` (source `deploy/ldmux.service`) runs
  `~/.local/bin/ldmuxd`; the keeper is the binary beside it, spawned by the daemon.
- Installed with `nix run .#install` (never by hand-editing `~/.local/bin` or the unit).
  The install and terminal-launcher flow is being changed on this branch; see
  `docs/PLATFORMS.md`.
- **A deploy does not replace a running keeper.** The keeper outlives the daemon on
  purpose, so after an install it keeps executing old code until
  `nix run .#restart-keeper`, which ends every shell. The install prints a warning.
- Remote listener: the tailnet address from `LDMUX_REMOTE_HOST`, port 7625, bearer
  token, `GET /ws` only (ADR-0016). Pairing is `ldmux code` (ADR-0017).
- Machine facts (host, tailnet address, phone) live in `AGENTS.md`, not here.

## The gate

```
nix develop
cargo test --workspace                                  # the gate
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
nix run .#mobile-test                                   # node --test packages/mobile/test/*.test.ts (dev-only; needs Node)
nix flake check                                         # all of the above, in the nix sandbox
```

`scripts/android.sh test` and `scripts/android.sh native-test` cover the mobile JVM
logic and the JNI shim and need `nix develop .#mobile`. The authoritative gate
definition is this section plus the rules in `AGENTS.md`. This file deliberately carries no test
counts: they go stale; run the command. The QR tests need `zbarimg`, which only the
dev shell provides.

## Open items

Product and mobile:
- **Clipboard write policy** (`ldmux clipboard-write on|off`, default off): the policy
  and the client's OSC 52 callback exist; a program's copy reaching the Android
  clipboard has not been verified on a device.
- **Tap-to-cursor** and a few **selection** rough edges (auto-scroll on drag,
  Compose renderer hardening: cursor blink, zoom). Detail in `docs/archive/STATE.md` §5.
- **iOS**: not started; needs a macOS host (`docs/IOS-DEVELOPMENT.md`).
- **Mesh/LoRa**: investigated only (`docs/MESH-TRANSPORT.md`); no code, no radio
  measurements.

Daemon and protocol:
- **`permessage-deflate` is not negotiated by the Rust daemon.** The Ktor client offers
  it and the old TypeScript daemon honoured it (14.8x on a large listing); the Rust
  daemon's own HTTP upgrade has no extension handling. Read, not run; see
  `docs/WIRE-COST.md`.
- **The keeper is a single point of failure** by design: if it dies, every shell ends.
- **Exit status is not recorded** when a shell ends, and the final scrollback of an
  ended session is not retained (SPEC §5.5). Endings do carry a reason (`endedReason`).
- **OSC 8 hyperlinks** are not carried by the cell model, so a snapshot drops them.
- **SPEC §5.4** (REST surface) is aspirational and marked so; the shipped surface is the
  Unix-socket RPC plus the WebSocket.
- `snapshotEpoch` is sent in `caps` and never read by the client; `follow` is never sent.

Platforms (see `docs/PLATFORMS.md`):
- Only x86_64 Linux is built and run today; aarch64 Linux evaluates in the flake but is
  unbuilt, and macOS needs the engine derivation proven on darwin plus replacements for
  the `/proc` reads.
- Non-systemd Linux is intended to work, untested.

Repository hygiene:
- Source comments in `crates/*/src/lib.rs`, `crates/daemon/src/ownership.rs`,
  `crates/vt-sys/tests/engine.rs` and `packages/mobile/README.md` cite
  `docs/archive/RUST-PORT-PLAN.md`, `docs/archive/RUST-PORT-LOG.md` and `docs/archive/STATE.md` section
  numbers. Those files moved to `docs/archive/` on this audit; the comments still need
  the new path (code was out of scope for the docs pass).
- In progress on this branch by another change set: desktop launching, terminal
  selection, the installer, `doctor`. Docs describing those are pre-change until
  reconciled.

## Where to look

| Question | Read |
|---|---|
| What is the product and its wire format | `docs/SPEC.md`, `docs/REMOTE.md` |
| Why a decision was made | `docs/adr/` (each ADR has a Status line) |
| The terminal engine and JNI seam | `docs/NATIVE-TERMINAL.md` |
| What a frame costs | `docs/WIRE-COST.md` |
| Desktop windows and i3 | `docs/DESKTOP-INTEGRATION.md` |
| What runs where | `docs/PLATFORMS.md` |
| How the port to Rust went, and the old TS/tmux state | `docs/archive/` |
