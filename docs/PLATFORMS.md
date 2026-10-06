# Platforms: what is supported, and how sure we are

Last audited 2026-10-06, on branch `fix/audit-hardening`. Every row says whether it
is **verified** (run, on this machine or the phone, with the evidence named),
**read** (checked against the source, never run on that platform) or **intended**
(a design position nobody has tested). Where a claim is only intended, it says so.

This is the matrix of what exists, not a promise. Anything not listed is unsupported.

## Summary

| Target | Status | Basis |
|---|---|---|
| Linux x86_64, glibc (daemon, keeper, CLI) | **Supported** | Verified: the one machine it is developed and deployed on (Debian 13). |
| Linux aarch64 | **Evaluates, not built** | Read: the flake defines the engine and the workspace for `aarch64-linux` (libghostty-vt is built by nix from the pinned commit, `LDMUX_VT_LIB_DIR`), and the derivations evaluate; nothing has been built or run on aarch64 hardware or under emulation. |
| Linux musl | **Not supported** | Read: the flake builds for glibc targets only; the `nix build` binaries link glibc dynamically and `scripts/install.sh` roots them as a nix GC root. |
| macOS daemon | **Not supported yet** | Read: see "macOS" below. |
| Windows | **Not supported, not planned** | SPEC §2.2. |
| Android client | **Done** | Verified on a device (the phone named in AGENTS.md, Environment facts); suites in `docs/archive/STATE.md` §4 are the older record. |
| iOS client | **Not started** | Nothing is built; `docs/IOS-DEVELOPMENT.md` lists the gating unknowns. |
| LoRa / MeshCore | **Investigated only** | `docs/MESH-TRANSPORT.md`; no code, no hardware measurement. |

## Linux host

**Init system.** The deployed form is a `systemd --user` unit (`deploy/ldmux.service`),
installed by `nix run .#install`. It depends on two systemd properties that are load-bearing
for the product (the unit header explains each): `KillMode=process`, so stopping the
daemon does not kill the keeper and every shell, and `loginctl enable-linger`, so the
user manager (and the keeper in its cgroup) survives logout. **Verified** on the
deployed machine.

Without systemd, run `ldmuxd` under any supervisor. **Intended, not tested.** The
requirements to carry over by hand are the same two: the supervisor must not signal the
keeper when it stops the daemon (the keeper is detached, but it sits in the daemon's
cgroup or process group on most supervisors), and the user session must not be torn down
at logout. The daemon needs `LDMUX_REMOTE_HOST` set to the tailnet address to be
reachable by the phone; the default is loopback.

**Shells.** Any. The keeper spawns `$SHELL` if it is executable, else the passwd entry's
shell, else `/bin/sh` (`pick_shell` in `crates/keeper/src/lib.rs`, unit-tested). The
keeper sets `TERM=xterm-256color` and `COLORTERM=truecolor` in the child. **Read** and
unit-tested; only bash has been used interactively in the deployment.

**Tailscale.** A prerequisite, not shipped. The daemon listens on a tailnet or loopback
address and refuses anything else (ADR-0016). It does not require the `tailscale`
binary at runtime, only an address in `100.64.0.0/10` or `fd7a:115c:a1e0::/48` (or
loopback). Verified on the one tailnet it runs on.

**Toolchain.** `nix develop` provides rustc/cargo (+ Node for the dev-only mobile
tests and scripts, + zbar for the QR tests). The gate is `cargo test --workspace`. A
non-nix build needs a Rust toolchain supporting edition 2024; **not tested**.

## Desktop integration

The portable core needs no desktop at all: sessions, the remote transport, pairing and
the phone work on a headless host. **Verified** (the daemon runs under a user manager;
the phone path does not touch X).

Putting a shell on the machine's own screen is optional. Two parts:

1. **Launching a window** is a direct spawn of `<terminal> -e <abs ldmuxd> attach <id>`
   (`crates/daemon/src/desktop.rs`, `crates/attach`). The terminal is `LDMUX_TERMINAL`,
   default `alacritty`. The daemon requires `DISPLAY` or `WAYLAND_DISPLAY` in its own
   environment and says so when neither is set. **Verified** with alacritty on X11.
2. **Placing it on a workspace** is an optional adapter. The only one with a measured
   implementation is **i3** (`i3-msg`, X11, i3 4.24; `docs/DESKTOP-INTEGRATION.md`).
   The source names sway and hyprland as intended adapters; treat those as **intended**.
   GNOME, KDE and macOS have no placement adapter, so a window opens where the window
   manager puts it.

**In progress on this branch** (another change set, not described here): terminal
selection, window launching, the installer and a `doctor` command. This document will
need reconciling with it; the statements above are the pre-change behaviour.

The desktop inventory (the launcher's list of workspaces, windows and pi sessions)
reads `/proc` and queries i3 (`crates/daemon/src/desktop_inventory.rs`); it is X11/i3
plus Linux only.

## macOS daemon

Not supported. A port is plausible but nobody has tried it. What is Linux-only, from
reading the source:

- **`/proc` is read directly** for process facts: the keeper reads `/proc/<pid>/comm`,
  `cwd` and `stat` for each session's foreground process, cwd and child discovery
  (`crates/keeper/src/lib.rs`); the daemon checks liveness by `/proc/<pid>` existing
  (`daemon.rs`, `daemon_lock.rs`) and walks `/proc` for the desktop inventory.
  macOS needs `proc_pidinfo`/`sysctl` equivalents.
- **The libghostty-vt build is nix-on-Linux only** (`flake.nix`, `systems`; `crates/vt-sys/build.rs`
  takes the archive from `LDMUX_VT_LIB_DIR`). Building for macOS needs the engine derivation proven
  on darwin (not evaluated or built) and the systems list extended.
- **systemd** is the deployed supervisor, `loginctl linger` the persistence story; macOS
  would need a launchd agent and its own answer to "does the keeper outlive the daemon".
- **Runtime directory.** `protocol::paths` uses `$XDG_RUNTIME_DIR`, falling back to a
  per-uid `/tmp` directory; the fallback works on macOS, but `XDG_RUNTIME_DIR` is not set
  there.
- The `nix` crate calls used for PTYs and process groups are POSIX and likely portable;
  that is a guess, not a check.

The flake includes `aarch64-darwin` in its `systems`, which only means the dev shell
evaluates; it does not mean the workspace builds there.

## Android

Done. Kotlin Multiplatform + Compose, libghostty-vt through a JNI shim, native renderer,
foreground service, QR pairing. Verified on a physical device over Tailscale
(`packages/mobile/README.md`; ADR-0007, 0011, 0012). Build needs `nix develop .#mobile`
and the Android SDK/NDK under `$HOME/Android`.

## iOS

Not started. Needs a macOS host and Xcode for the engine xcframework, and a decision on
the background-connection model (iOS gives roughly three minutes). The shared Kotlin core
is kept expressible without a platform by `packages/mobile/test/architecture.test.ts`
(ADR-0013), which is the only iOS-related thing that is enforced. See
`docs/IOS-DEVELOPMENT.md`.

## Mesh / LoRa

Investigated, not implemented, and not measured on radios. The target is **MeshCore**;
Meshtastic is a comparison in `docs/MESH-TRANSPORT.md`, which also states what is
unknown (real-hardware throughput, MeshCore latency, whether the companion API carries
arbitrary binary, the regional duty-cycle that decides viability).

## What would move a row from "read" or "intended" to "verified"

- aarch64 Linux: `nix build` and `nix flake check` on real aarch64 hardware (the engine derivation
  is defined for it but unbuilt).
- Non-systemd Linux: run the daemon under runit/OpenRC/s6 on a test host, restart it, and
  confirm the shells survive (the `restart_survival` integration test models this
  against the keeper but not against a supervisor).
- A second shell (zsh, fish): spawn it through the keeper and run the color and
  cursor checks.
- sway/hyprland placement: implement and measure, as i3 was.
