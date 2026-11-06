# ldmux — Software Specification

> **Status note (2026-09-20, amended 2026-09-27) — these decisions post-date this document.**
>
> - **Runtime: Rust.** The daemon, keeper, CLI and desk-window client are Rust
>   (`crates/`). The Node 26 + TypeScript runtime of
>   [ADR-0009](adr/0009-node-runtime-and-no-pi-extension.md) was replaced at D4
>   (ADR-0018); Node survives only as a dev tool for the mobile tree's `.ts`
>   tests and the two scripts under `scripts/`. Every "Bun" or "Node" below is
>   historical.
> - **Pi RPC is not part of this product** (2026-11-05). §4.5 and the
  `SessionDriver` seam are deleted: ldmux is a shell app, and nothing here drives
  pi semantically. The section existed because an earlier design considered a
  semantic pi UI; that is not something this software does.
- **The pi extension has been removed.** ldmux is not a pi plugin; the
>   `packages/extension` package, the `install-extension` CLI command, and the
>   `register`/`session_end` RPC methods are gone, along with the RPC-only session
>   expiry that existed to serve them. Sessions are created with `new`/`adopt`.
> - **The shell view's orientation is the device's** (§7.1 below is wrong). There is
>   no flip button; the device rotates the shell view and the app only decides whether
>   the keyboard is up. The launcher and its dialogs hold portrait (ADR-0011's
>   amendment). See [ADR-0011](adr/0011-screen-orientation.md). Any claim in this
>   document that Android is landscape-locked is historical.
> - **Remote is a direct tailnet bind, paired by QR code** (added 2026-09-27):
>   ADR-0016 and ADR-0017 supersede ADR-0005's loopback + `tailscale serve`.
> - **The daemon is now Rust, and there is no tmux** (added 2026-10-04): the
>   deployed daemon is `crates/daemon` (`ldmuxd`); a `ldmux-keeper` process owns
>   every PTY and runs libghostty-vt (the same emulator the phone renders), and
>   the daemon auto-spawns it. Every tmux reference below — §5.2's interaction
>   model, §5.3's control-mode registry, §6.4's attachment discipline, the tmux
>   runtime profile in §5.2, §11.1's language note — is **historical**. The
>   session layer, the registry and the terminal engine are the keeper now
>   (`docs/archive/RUST-PORT-PLAN.md`, `docs/archive/RUST-PORT-LOG.md`). The wire protocol (§6),
>   geometry (§7), the scroll model (§7.4), the desktop mirror (§9) and the
>   tailnet/security model (§10) stand, with the keeper in place of tmux.
>
> - **tmux is removed from the software, not just the runtime** (added
>   2026-10-05). The wire/registry field `tmuxName` is `keeperName` and the
>   ping/remote field `tmuxSocket` is `keeperSocket` (the installed phone app
>   must be rebuilt: `SessionInfo.keeperName`). `paneInMode` is gone from
>   `snapshot`/`caps`, the `copymode` message is gone (the client scrolls its own
>   buffer), and `LDMUX_TMUX_NAME`/`LDMUX_TMUX_SOCK` are no longer set. A registry
>   written with the old key is migrated once on load. The key-name vocabulary
>   (`Enter`, `C-a`, `M-f`, `BSpace`, `DC`, ...) is the protocol's own, owned by
>   the daemon's `key_name_to_bytes`. The §3.11 parity gate compares the engine
>   against committed goldens (`crates/daemon/tests/parity/`), not a live tmux.
>   Sections still describing the tmux design are marked **historical** below.
>
> - **Current wire and lifecycle additions** (2026-10-06, branch `fix/audit-hardening`;
>   details in §5.3, §5.5, §6, §9 and `docs/REMOTE.md`): the daemon negotiates
>   `permessage-deflate` (ADR-0019); `hello`/`attach` carry additive `features`
>   (`inplaceResync`, `sessionEnded`), with a `resync: true` snapshot and a
>   `session_ended` message; session records carry the shell's exit status and a
>   closed `endedKind`; ended shells keep their pane 60 s for a reconnecting client;
>   claims persist across a daemon restart; the desk attach client uses a local
>   `0600` attach socket instead of the tailnet. Mesh link security is designed in
>   ADR-0020 and not implemented.
>
> The rest of this document is still the design of record for the transport,
> geometry, scroll and lifecycle decisions; read "Bun"/"Node" as historical, any
> extension reference as historical, and the orientation sections as superseded.
> Sections that still describe the tmux/TypeScript design say so in their heading
> or in a leading note; `docs/STATE.md` is the current position.

**Version:** 0.3 (design; transport decision finalized)
**Date:** 2026-09-13
**Author:** Lincoln Deen (with the pi coding agent)
**Status:** Phases 1–3 implemented (see README Status and `docs/STATE.md`). This document remains the authoritative design; where it describes a *target* API or behaviour that is not built, that is called out inline, and `AGENTS.md` plus the tests are the statement of what exists.

> **Revision notes v0.2 and v0.3 are historical (tmux-era).**
>
> **Revision note (v0.2):** An adversarial architecture review found three blocker
> issues and several high issues in v0.1. This revision fixes them: canonical tmux
> geometry (no per-viewer resize), reason-branched pi session lifecycle, a PTY-free
> transport default, explicit session discovery/adoption scope, a client-owned
> scroll-anchor model, snapshot-then-stream reconnect, a decisive TLS/pairing story,
> and Android specifics. Over-specified infrastructure was trimmed. See §17 (ADRs).
>
> **Revision note (v0.3):** The transport decision (ADR-0001) was settled by
> hands-on experiment: **tmux control mode `-C` runs over plain pipes with no PTY**
> (only `-CC` requires a tty). v1 therefore uses control-mode streaming +
> capture-pane snapshots/fallback. (It was implemented on Bun, then Node 26;
> both are gone — ADR-0018.)
> See [`archive/TRANSPORT.md`](archive/TRANSPORT.md).

---

## 0. TL;DR

`ldmux` is a **keeper-native remote terminal system**:

- A long-lived **daemon** talks to a **keeper** process that owns every PTY and
  runs libghostty-vt per session, and exposes the sessions over the
  **tailnet** via a WebSocket API.
- A **native mobile client**: a KMP + Compose Multiplatform Android app
  (`packages/mobile`) that renders the terminal itself — libghostty-vt behind a
  JNI seam, painted on a Compose `Canvas` — and speaks the protocol directly.
  There is no browser client: it was deleted, so the native app is the only
  renderer, and it is
  no longer what the app hosts (ADR-0007).
- **No pi extension.** ldmux is not a pi plugin; `pi` is simply a program one can
  run in a session (see the status note above and ADR-0009).
- A thin **CLI** and an optional desktop **alacritty window** round it out.
- **The keeper** provides persistence (it outlives the daemon), multi-client
  attach through the daemon's ownership model, and synchronized phone+desktop
  viewing.

The primary design goal is the **mobile terminal UX**. Scroll jumping, session
switching and modifier keys are the things that go wrong there, and the design
answers each one directly. Everything else is kept as small as that goal allows.

---

## 1. Background and intent

> *§1 is the background as of v0.1, when tmux was the intended backbone. The built
> system uses the keeper (ADR-0018).*

### 1.1 What the design is answering

Mobile terminals fail in a few specific ways, and this project exists to answer them:

- a scroll position that jumps when output arrives while the reader is looking back
- session switching that loses where you were
- modifier keys a phone keyboard cannot express
- a browser as the renderer, which is where the interaction is worst

### 1.2 What that decided

- Server-authoritative sessions, with thin clients that hold no session state.
- One terminal renderer behind one client interface, native on the phone.
- Tailnet-first transport, WireGuard-encrypted, no public exposure and no ACME.
- End-to-end pins for every cross-component contract.
- Scroll anchoring as an explicit client state machine rather than a heuristic
  (ADR-0004).
- A stateful per-stream sanitizer, byte to line framing, and an incremental UTF-8
  decoder per pane: the three places a terminal bridge usually corrupts bytes.
- The validated mobile vocabulary: Termux-style extra keys, gestures, jump to
  bottom, and a ceiling on frame rate.

### 1.3 What came from the pre-port investigation

The investigation compared transport options and surveyed session layers, and is
recorded in [`archive/INVESTIGATION.md`](archive/INVESTIGATION.md). Its finding that
control mode needs no PTY held, and its survey of session layers, mattered at the
time. The keeper replaced tmux (ADR-0018), so that material is history rather than
design.

---

## 2. Goals and non-goals

### 2.1 Goals

1. **G1 — keeper-native.** Every session is a PTY owned by the keeper, behind a private Unix socket. (Originally "tmux-native"; tmux was replaced by the keeper, ADR-0018.)
2. **G2 — withdrawn.** The original goal was "pi extension first": ldmux
   available whenever a pi session is active, registering pi sessions. ldmux is
   not a pi plugin (ADR-0009). What survives is the part that was never really
   about pi: the daemon is a long-lived process independent of any client, and
   `pi` can be run inside a session like any other program.
3. **G3 — Mobile app, two modes that follow the device.** Landscape: terminal
   ~3/4 and a configurable left/right control panel. Portrait: terminal with a
   compact three-row key bar above the keyboard (a navigation row, a modifier row, and
   an actions row with a 3x2 arrow pad at the trailing edge; amended 2026-10-24). The shell view follows the device's
   rotation and the app owns the keyboard; there is no flip button, and the
   launcher holds portrait (ADR-0011). *(Earlier text said "landscape-only",
   then "chosen by an explicit button"; both are superseded.)*
4. **G4 — Open shells in any directory from the phone** (2026-11-05: there is no
   session kind; a shell is the only kind, and `pi` is a program run inside one),
   optionally mirrored
   live into a desktop `alacritty` window on a chosen i3 workspace.
5. **G5 — Synchronized multi-view is a nice-to-have**, achieved via the
   daemon's multi-client attach; if it complicates anything, degrade gracefully.
6. **G6 — Tailnet only.** Reachable only over Tailscale; token auth in addition to
   WireGuard.
7. **G7 — Robust and simple.** Reconnect/resync, no scroll jumping, clean session
   lifecycle, no leaked processes, observable.
8. **G8 — Best-engineered.** Typed contracts, tests, docs, reproducible builds.
9. **G9 — Light on the network, always.** Stated as a standing ceiling rather
   than an optimisation task: *"It should be light on the network like always.
   That's our goal"* and *"we don't want it to go to like ever that high of a
   frame rate"* (2026-09-14), and *"consistent throughout super light"*
   (2026-09-17). Both directions bind — the frame-rate ceiling is 20 fps, and no
   workload class is exempt, cellular and mesh least of all. Any change that
   raises bytes or frame rate carries the burden of justifying it, not the
   reverse. Measurements live in `docs/WIRE-COST.md`.
10. **G10 — One shared core, two independent clients.** Kotlin Multiplatform with
   Compose Multiplatform, Android and iOS as separate apps over one shared core,
   never one codebase pretending to be two (ADR-0013). Every cross-platform
   decision is made for *both* platforms at the time it is made. iOS is deferred
   as a build, not as a design: the shared half must stay expressible without a
   platform, and that is now enforced by a test rather than by intention
   (`packages/mobile/test/architecture.test.ts`).

### 2.2 Non-goals

- No containers / Docker / devcontainers.
- No hosted relay, billing, Supabase, Centrifugo, Play Integrity.
- No public internet exposure (no Funnel by default).
- No adapter zoo, and no session kinds at all: every session is a shell (2026-11-05).
  `pi` is a program a shell can run, not a kind of session.
- No arbitrary command-wrapping; `ldmux` starts keeper sessions.
- No synchronous live typing requirement.
- No Windows/macOS server support in v1; Linux (this machine) first.

---

## 3. Functional requirements

| ID | Requirement |
|----|-------------|
| FR-1 | Sessions are discoverable and controllable through `ldmux`; there is no session kind, so this is every shell (2026-11-05). |
| FR-2 | **Dropped** — `ldmux` is not a pi extension (ADR-0009). The daemon is still "up and ready" independent of any client, which is what this requirement was for. |
| FR-3 | The backend must be a persistent session layer that outlives clients. (Originally "tmux-based"; now the keeper, ADR-0018.) |
| FR-4 | Remote access is **Tailscale-only**. |
| FR-5 | A **multi-platform mobile app** exists (KMP/Compose preferred). |
| FR-6 | ~~The mobile app is landscape-only.~~ Superseded by ADR-0011: the shell view follows the device's rotation; the launcher holds portrait. |
| FR-7 | Terminal ~3/4 of the screen; control panel 1/4, side (left/right) is a setting. |
| FR-8 | A **clean swipe UI** between open shells. |
| FR-9 | Sessions closed/ended on the host disappear from the mobile live list. |
| FR-10 | From the phone, open new shells **in any directory**. |
| FR-11 | Optionally, a live **`alacritty` window** on a chosen workspace (default "last"/`Mod+0`) attached to the same session. |
| FR-12 | Phone+desktop **synchronized live viewing** desirable but optional; robustness wins. |
| FR-13 | Clean multi-connection handling (multiple devices/sessions). |
| FR-14 | **Stable scrolling** — no jump-to-top while output streams. |
| FR-15 | Usable terminal control keys on mobile (Esc/Tab/Ctrl/Alt/arrows). |
| FR-16 | Robust: reconnect/resync, lifecycle cleanup, no leaks. |
| FR-17 | Best-practice engineering: typed contracts, tests, docs. |

---

## 4. Architecture

### 4.1 Component overview

```
                       ┌─────────────────────────────────────────────┐
                       │                  TAILNET                     │
                       │  (WireGuard; <host>.<tailnet>.ts.net)        │
                       └───────────────┬─────────────────────────────┘
                                       │  ws:// on the tailnet address, token
        ┌──────────────────────────────┴──────────────────────────────┐
        │                                                               │
 ┌──────▼────────┐        ┌───────────────────────────────┐     ┌──────▼───────┐
 │  MOBILE APP   │        │        ldmuxd daemon          │     │  DESKTOP     │
 │ KMP/Compose   │◄──────►│  • pane stream + snapshots    │     │  MIRROR      │
 │ portrait/land.│  WS    │  • session registry (JSON)    │     │ alacritty -e │
 │ libghostty-vt │        │  • WebSocket on the tailnet   │     │ ldmuxd attach│
 │ Compose Canvas│        │  • spawn/kill/list sessions   │     └──────┬───────┘
 │ side panel    │        │  • optional i3 window helper  │            │
 └───────────────┘        └───────┬───────────────────────┘            │
                                  │ keeper protocol (Unix socket)      │
                          ┌───────▼───────────────────────┐           │
                          │  ldmux-keeper                 │◄──────────┘
                          │  session: ldmux-<ulid>        │  (protocol client of the daemon)
                          │    pty: bash | pi             │
                          └───────────────────────────────┘
```

### 4.2 Components

| Component | Language | Role |
|-----------|----------|------|
| **`ldmuxd`** (daemon) | Rust (`crates/daemon`) | Talks to the keeper; pane streaming + snapshots; session registry; WS over tailnet; spawn/kill; optional i3 window helper. |
| **`ldmux`** (CLI) | Rust (`crates/daemon`, `bin/ldmux`) | `ping`, `list`, `desktop`, `new`, `shell`, `run`, `window`, `send`, `key`, `capture`, `snapshot`, `kill`, `close`, `forget`, `adopt`, `mirror`, `live`, `unlive`, `remote`, `code`, `pair` (plus `clipboard-write`). Unix-socket local control. (`attach` is `ldmuxd attach` via `run`/`mirror`; `install-extension` is gone with the extension. Installer and `doctor` work is in progress on this branch and is not specified here.) |
| **Mobile app** | Kotlin Multiplatform + Compose Multiplatform | Portrait (typing, three-row key bar above the keyboard) and landscape (side panel); the launcher holds portrait and the shell view follows the device's rotation, while the app owns the keyboard — no flip button (ADR-0011 and its amendment); native renderer — libghostty-vt over JNI, painted on a Compose `Canvas`; Ktor WebSocket; client-owned pane geometry; predictive echo; tap-to-cursor; foreground service pending. |
| **Desktop mirror / live window** | shell + WM | `i3-msg exec -- <terminal> --class ldmux-live-<id> -e <abs ldmuxd> attach <id>`, then a criteria-targeted `move to workspace number <N>` once the window exists. **Never** `workspace N; exec …` — that moves the user's view. The terminal is `desktop::terminal()` — `LDMUX_TERMINAL`, default `alacritty` — so another emulator is one value rather than one edit per window kind, and `ldmux mirror` is the same argv with `--readonly`. See docs/DESKTOP-INTEGRATION.md; terminal selection and window launching are being reworked on this branch (see docs/PLATFORMS.md), so treat this row as the pre-change description. |

### 4.3 Why the daemon is a separate process

An in-process host cannot be a robust long-lived multi-client server that
outlives any one client. So the daemon is a separate process (spawned detached on
first need and/or installed as a `systemd --user` service) and outlives every
session — required by FR-9/FR-12. Historically the reason was stated as "why the
daemon is separate from the pi extension"; the extension is gone (ADR-0009) and
the separation is unchanged.

**The keeper is not the daemon's child (ADR-0021).** Under systemd the keeper is its
own user unit, `ldmux-keeper.service`, which the daemon starts with
`systemctl --user start` (`LDMUX_KEEPER_UNIT`) and `ldmux.service` `Wants=`. Each shell
the keeper forks is moved into its own transient scope under `ldmux-shells.slice`, so
the daemon unit is charged for the daemon alone and memory is attributed per session; a
job a user daemonized survives the shell being killed, in its scope. A failure to make
the scope never prevents a shell. A keeper restart ends every shell; a daemon restart
touches neither. Without systemd (`LDMUX_KEEPER_UNIT` unset, or `LDMUX_NO_SCOPE=1` for
the scopes) the daemon spawns the keeper detached and shells stay in its cgroup, as
before; there `KillMode=process` on the daemon unit is what keeps them across a restart.

### 4.4 Session model

A **session** is one keeper session (a PTY and its shell) on the keeper socket.

- **Kind:** `shell` (default `$SHELL`) or `pi` (command `pi`, optional args/prompt).
- **Name:** human label; the keeper session name (`keeperName`) is a stable machine id
  `ldmux-<ulid>`.
- **cwd:** arbitrary directory (FR-10).
- **Created-by / owner:** which pi session or client created it (families, phase 2).
- **State:** `starting | running | attached | detached | dead`.
- **Persistence:** the keeper keeps the pty alive across disconnects. A shell that
  exits ends its session: the record becomes `dead` with the shell's
  exit status (`exitCode`, or `exitSignal` when a signal killed it) and an
  `endedKind`, and a desk window on it closes. The pane (final screen) stays in the
  keeper for 60 s so a reconnecting client can read it (§5.5).

**Pi sessions** are ordinary keeper sessions whose pty runs `pi`. An optional
shell wrapper (an example only; the repo does not install it) can launch
interactive `pi` into a session:

```bash
# ~/.bashrc (new)
pi() {
  [[ -n "$LDMUX" ]] && { command pi "$@"; return; }   # never nest
  case "$1" in -p|--print|--mode|--version|-h|--help|update|install|remove|uninstall)
       command pi "$@"; return;; esac
  ldmux -- pi "$@"
}
```

**Child env contract** (injected by the keeper's `Spawn`, plan §2.6):

| Var | Meaning |
|-----|---------|
| `LDMUX` | `1` — running under ldmux (nesting guard). |
| `LDMUX_SESSION_ID` | ldmux session id (`<ulid>`). |
| `LDMUX_SOCKET` | daemon Unix socket path for child reporting. |
| `LDMUX_KIND` | `shell` \| `pi`. |
| `LDMUX_SESSION_NAME` | the keeper session id (`ldmux-<session id>`). |
| `LDMUX_KEEPER_SOCK` | the keeper's Unix socket path. |
| `TERM` / `COLORTERM` | `xterm-256color` / `truecolor`: what the keeper's emulator is (set by the keeper and stated again in the daemon's spawn env). |
| `SHELL` | the program the session actually exec'd (a `pi`-kind `Spawn.command` leaves it naming the user's shell). |
| `PATH` | **inherited, and floored**: the unit's `PATH` (the user manager's) plus `~/.local/bin`, `/usr/local/bin`, `/usr/bin`, `/bin` if missing. Never replaced — a session shell is not a login shell, so a user's `.profile`/`.zprofile` PATH would otherwise never apply. |

Variables that name the terminal the daemon was started from (`TMUX`, `STY`,
`TERM_PROGRAM`, `KITTY_*`, `VTE_VERSION`, `WEZTERM_*`, `ALACRITTY_*`, `WT_*`, `COLORFGBG`, …)
are **stripped**: a session is its own terminal (2026-10-24).

`LDMUX_DAEMON_TOKEN` is **not** implemented: children reach the daemon through
the Unix socket, whose 0600 permission plus a 0700 state dir is the boundary.

### 4.6 Session discovery and adoption scope (FR-1)

Honest v1 scope:

1. **ldmux-created sessions** (via the CLI, the optional wrapper, or the mobile app): full terminal control. These are the primary case.
2. **Opt-in adoption** of keeper sessions the registry does not know: `ldmux adopt <keeper-session>`
   registers the session and opens a pane stream. Adoption is explicit, never
   automatic.
3. **Bare `pi` processes** (started outside ldmux): **not
   terminal-attachable.** There is no RPC driver and none is planned: ldmux is a
   shell app, and a process started outside it is not one of its sessions. This
   is a stated limitation, not a bug.

The mobile app always shows ldmux sessions; adopted sessions appear after opt-in.

---

## 5. Daemon design (`ldmuxd`)

### 5.1 Responsibilities

1. Supervise the keeper (auto-spawned, detached, outlives the daemon).
2. Maintain a compact **session registry** reconciled against the keeper's `List`.
3. **Stream** pane output (§6) and provide **snapshots** (rendered by the keeper's emulator).
4. Accept **input** (`Write` to the keeper).
5. Serve the **API** (Unix socket locally; plain WebSocket on the tailnet address, ADR-0016).
6. Spawn/kill sessions in arbitrary cwds (FR-10).
7. Optionally launch the desktop mirror (FR-11).
8. Emit **events** for reactive UI (in-protocol; see §5.4).
9. Clean up; never leak processes; never kill the keeper on exit.

### 5.2 tmux interaction model — **historical; the session layer is the keeper**

> The deployed daemon has no tmux. `ldmux-keeper` (`crates/keeper`) is the
> session layer: it forks a PTY per session in its own process group, runs the
> shell under it, and runs libghostty-vt in-process to render the grid. The
> daemon talks to the keeper over a Unix socket with the same message set as
> the phone's wire protocol (minus auth): `Spawn`/`Kill`/`Write`/`Resize`/`Read`/
> `Peek`/`Snapshot`/`List`/`Close`. There is no control-mode parser, no
> `capture-pane`, no tmux server, and no `-L` socket. The text below is the
> record of the tmux design this replaced.
>
> What replaced each piece:
>
> | tmux mechanism | now |
> |---|---|
> | tmux server + `-L ldmux` socket | `ldmux-keeper`, one process, detached (ADR-0018) |
> | server option profile (below) | the seven-variable spawn env (§4.4) |
> | control mode `-C` + `%output` parsing | a framed binary protocol we own (`crates/protocol`) |
> | `capture-pane` snapshots | the keeper's libghostty-vt grid |
> | `capture-pane -S/-E` scrollback paging | the emulator's scrollback, paged by the same `snapshot` message |
> | `send-keys` / `refresh-client` | `Write` / `Resize` keeper messages |
> | `window-size manual` (canonical geometry) | the claim hands the size over; `protocol::clamp_pane_size` bounds it |
>
> **Everything between here and §5.3 is that historical tmux design, kept as a record.**

**Canonical geometry (ADR-0002).** tmux has **one window size per window**. If a
viewing client influences sizing, every other client (including the desktop
mirror) reflows. Therefore:

- Set `window-size manual` on the ldmux server and pin a **canonical geometry**
  (default: match the desktop display, e.g. `detect → 120x40`, configurable).
- **The mobile client renders a viewport over the canonical grid** — it may
  scale/pan/pinch, but it **never** resizes the pane.
- Do **not** use `refresh-client -C` to "claim" a phone viewport. (v0.1 was wrong.)
- Independent per-client sizes are only possible with **grouped sessions**
  (`new-session -t`), which is explicitly a different (non-synced) mode; out of
  scope for v1.

**Server option profile** (`ldmux-server.conf`, applied on server start):

```
set -g history-limit 10000                # ~40 MB/pane; 50k costs ~200 MB
set -g default-terminal tmux-256color   # fall back to screen-256color if unsupported
set -g mouse on
set -g status off                        # consistent for phone stream and mirror
set -g aggressive-resize off
set -g set-clipboard on
set -g focus-events on
set -g remain-on-exit off               # a shell that exits ends its session
# NOT global: `window-size manual`. Set globally it crashes tmux 3.5a (Debian 13)
# on the next new-session; it is set per session at creation instead.
# NOT applied: `allow-passthrough on`. Arbitrary escape pass-through is exactly
# what the ADR-0001 sanitizer bounds; re-add only with an explicit policy.
```

**Transport (ADR-0001 — final).** v1 uses **tmux control mode `-C` over pipes**
(no PTY) for live streaming, with **`capture-pane` snapshots** for
connect/reconnect/scrollback and as an automatic fallback.

- **Streaming:** one `tmux -L ldmux -C attach -t <session>` child per *viewed*
  session, spawned with `stdin`/`stdout` **pipes** (proven on tmux 3.5a + Bun
  1.3.9). Parse `%output` / `%extended-output` notifications. Do **not** use
  `-CC` — it requires a tty (`tcgetattr failed` over a pipe); `-C` needs no PTY.
- **Backpressure:** `refresh-client -f pause-after=2` in the control client;
  `%extended-output <pane> <age_ms> : <data>` is parsed as the paused form.
  `%pause` is treated as a resync point (tmux discards a paused pane's queued
  output) and the pane is resumed with `refresh-client -A <pane>:continue`,
  throttled per pane; a client whose WebSocket buffer exceeds 4 MiB is closed
  with `too_far_behind` so it reconnects into a fresh snapshot.
  (Measured: ~1.5 MB/s burst, no stalls, ~0 tmux CPU when idle.)
- **Input:** `send-keys` over the control connection (no process per keystroke).
- **Snapshots (reconnect):** `capture-pane -p -e` (plus `-a` on the alternate
  screen) + `display-message -p` state fields (see §6.2).
- **Scrollback paging:** `capture-pane -S/-E` on demand, **bounded to 5,000
  lines** (`scrollback` message → snapshot with `history: true`). Asked for by
  the act of scrolling to the top of what is loaded — the viewport offset counts
  from the *top* of the loaded region (0 = oldest, `total − len` = live bottom),
  so "at the top" is `offset == 0`. A resize (rotation, font change) replays the
  captured screen into the live engine instead of rebuilding it, because a
  rebuild discards every line the client accumulated since the last snapshot.
- **Fallback:** if the control client dies/stalls (`%exit`, EOF), it is
  **reconnected with bounded backoff** (3 attempts); if control mode still cannot
  be established the stream switches to **`capture-pane` row-diff polling**
  (~250 ms, in-place row rewrites) and retries control mode every 30 s. A
  `%pause` (queued output discarded because we fell behind) is a resync point;
  the pane is resumed with `refresh-client -A <pane>:continue`.

A `PaneStream` interface abstracts this; `pipe-pane` (raw bytes) and a
server-side VT emulator (perfect snapshots) slot in behind it later.

Rationale: `-C` over pipes gives control-mode latency and flow control **without
the native PTY dependency** the v0.1/v0.2 drafts assumed. Calling control mode
PTY-based is a convention, not a requirement for `-C`.
See [`archive/TRANSPORT.md`](archive/TRANSPORT.md) for the experiments and the parser spec.

**Parser requirements (the three places a naive bridge corrupts bytes):** byte→line framing on
`\n` (never `readline`; U+2028/9 must not split); octal → **bytes** then a
**per-pane incremental UTF-8 decoder** (never `byte as char` or per-chunk
`from_utf8`); correlate `%begin/%end/%error <seq>`; tolerate unknown `%…` lines;
stateful per-stream sanitizer (OSC 52 / DCS / device-status) on by default; OSC 52 only
may be re-enabled by the host (`ldmux clipboard-write on`, off by default).

**Multi-pane scope (v1):** one window, one pane per session. The mobile context
menu can split/create windows via `send-keys`/tmux commands, but the client tracks
only the *active* pane; the pane poller follows `display-message -p '#{pane_id}'`.
(Historical: the keeper has one pty per session and no windows or panes to split.)

### 5.3 Session registry

**The keeper is authoritative** for what is running. v1 keeps a **compact JSON
state file** (`<stateDir>/registry.json`, where `<stateDir>` is `$LDMUX_STATE_DIR`,
else `${XDG_RUNTIME_DIR}/ldmux`, else `/tmp/ldmux`) for metadata the keeper does
not hold (kind, title, pi-session id, created-by, last-seen). A file written
before 2026-10-05 stored the keeper name under a different key; `Registry::load`
migrates it once and rewrites the file under `keeperName`. SQLite is **deferred**
until a proven need (it was over-engineered in v0.1).

Per-session record:

```json
{
  "id": "01a09c54-…", "keeperName": "ldmux-01a09c54", "kind": "pi",
  "cwd": "/home/ldeen", "title": "ldmux investigation",
  "createdAt": 1789…, "updatedAt": 1789…, "createdBy": "mobile",
  "piSessionId": "01a09c54-…", "status": "running|dead",
  "exitCode": null, "exitedAt": null, "endedReason": null,
  "exitSignal": null, "endedKind": null
}
```

`exitCode` / `exitSignal` are the shell's own ending as the keeper's `waitpid` saw it
(neither is set for a session someone killed or the keeper lost). `endedKind` is a
closed set: `exited | killed | closed | keeper_lost | window_closed`; it is absent while
the session runs and on records written before it existed. The same three fields and
`endedReason` ride on `SessionInfo` (`crates/protocol/src/rpc.rs`).

Reconciliation on start and periodically: keeper `List` → upsert; drop
records for vanished sessions; adopt survivors. Dead-but-retained sessions keep
metadata + scrollback reference for replay.

**Every ending says why.** `endedReason` is a short phrase written where the
session ends and required by the signature of the one method that ends one, so a
new ending path cannot compile without saying how: `its shell exited`, `the
keeper no longer has it` (it came back without the pane), `its desk window
closed`, `its desk window closed while the daemon was down`, `killed from
<device name>`, `closed from …`. It is carried on `SessionInfo` as
`endedReason`, so `ldmux list` prints it beside `dead`, and the daemon logs
`session <id> ended: <reason>` on the same path. Until 2026-10-05 a record said
`dead` and nothing else and the journal said nothing at all — two shells ended
during an evening of use and the only way to explain either was elimination.

### 5.4 API surface

The shipped surface, and the only one: newline-delimited JSON RPC over a Unix
socket (`crates/protocol/src/rpc.rs`), and one WebSocket per session at `WS /ws`
(JSON control plus binary `OUTPUT` frames, `docs/REMOTE.md`). The daemon serves no
static files and has no REST layer.

Local: Unix socket `$XDG_RUNTIME_DIR/ldmux/ldmuxd.sock`.
Remote: `ws://<tailnet-address>:7625/ws` — the daemon's own tailnet address, no
proxy (ADR-0016; see §10).

A twelve-row REST table stood here until 2026-11-05 and described an API nobody
was going to build: no `/api/health`, no `/api/sessions`, no `/api/dirs`, no SSE.
Every one of its rows is served instead by an RPC method or a WebSocket message, so
the table was a second, contradictory description of the product. Deleted rather
than marked aspirational a second time.

### 5.5 Lifecycle and robustness

- **Reconcile on start:** the keeper wins; registry is a cache.
- **Crash-only design:** restart rebuilds from the keeper + registry JSON.
- **Idempotent startup:** `flock`; refuse to run twice.
- **Graceful shutdown:** stop pollers, flush registry, leave the keeper alive.
- **No blocking calls on the async runtime:** keeper socket I/O that can block
  runs on `spawn_blocking`, and each external command (i3, alacritty) is bounded
  by a timeout, so a wedged child cannot pin a request or the per-connection
  handler queue. A timed-out call is reported as *unknown*, never as "no sessions".
- **A client that floods the daemon waits for its own requests.** The TypeScript
  had an in-process per-connection queue (`data.queue = data.queue.then(...)`) and
  this section recorded, in that era, that a client spamming `list` put roughly two
  hundred real requests ahead of its own *attach*, which then timed out. The Rust
  port has no queue at all: `handle_connection` reads one message, awaits its
  handler, and reads the next, so the socket's own buffer is the queue and every
  message is served in order. The consequence is the same shape and much smaller —
  a client that sends two hundred `list`s waits about one keeper round trip each
  (single-digit milliseconds), not a timed-out attach — and nothing is dropped.
  Nothing here is broken by a slow client. Recorded rather than changed: the
  message that would fix it properly is the one that makes `list` a
  *current-state* request whose queued copies are worthless, and at this cost it
  does not earn its complexity.
- **A shell that exits ends its session**; reconcile marks it
  `dead` (recording its exit status and `endedKind`), releases its claim and window
  record, and broadcasts. The keeper keeps the dead pane, with its final screen, for
  60 s (`DEAD_KEEP_MS`; the keeper's own backstop for an unattended keeper is 15
  minutes), so a client that reconnects in that window can attach, read the final
  screen and receive `session_ended` (a client that advertised `sessionEnded`). After
  that the pane is released and the final scrollback is gone. Dead records are pruned
  after 24 h.
- **Claims persist.** The ownership table is mirrored to `<state>/claims.json` and
  reloaded at start with a 60 s re-claim grace (`RESTORE_GRACE_MS`); a restored claim no
  device re-attaches to is released when the grace ends.
- **The state directory and the token file must be owned by the daemon's uid**; the
  daemon refuses to use one that is not (state dir tightened to `0700`, token file to
  `0600`).
- **The pi extension is gone (ADR-0009).** This section used to carry a table of
  what the extension did on `quit`, `reload`, `new`, `resume` and `fork`. There is
  no extension: ldmux is not a pi plugin, and `pi` runs inside a session like any
  other program. Left as a note rather than deleted so the removed scope stays
  visible.

---

## 6. Wire protocol

### 6.1 Transport and auth

- Plain WebSocket (`ws://`) on the daemon's **tailnet address** (ADR-0016,
  superseding ADR-0005's loopback + `tailscale serve`). WireGuard encrypts the
  link. The daemon listens only on a tailnet (`100.64.0.0/10`,
  `fd7a:115c:a1e0::/48`) or loopback address and answers `403` to any peer
  outside those ranges, before the token is checked (`crates/protocol/src/net.rs`).
- **Gates:** (1) the peer is on the tailnet (or loopback), and (2) a bearer
  **token** stored plaintext at `$LDMUX_TOKEN_FILE` (default `<stateDir>/token`)
  with mode `0600` and compared in constant time; the app sends it as
  `Authorization: Bearer`. There is no per-user identity allow list: the token
  is the only authentication, so treat it as the credential it is.
- **Pairing (ADR-0017):** `ldmux code` draws a QR code of
  `ldmux://pair?v=1&host=<addr:port>&token=…&name=<machine>`; `ldmux pair`
  prints the same link. Both refuse a loopback-only or not-yet-listening daemon.
  The app scans it from Settings; there is no manual host/token entry.
- **Framing:** terminal data is **binary**; control/handshake is **JSON text**
  frames. `protocolVersion` is negotiated in the handshake; enforces max frame
  sizes: 16 MiB absolute frame ceiling, 16 KiB of `input` data, 32 keys per
  message, 64 KiB per inbound WebSocket frame, a 1 MiB cap on the output
  buffered while a client is attaching, and a 64 KiB cap on output queued for a
  socket that is not reading (`WS_BACKLOG_BYTES`; exceeding either sends one
  `resync` error and drops output until the client re-attaches and is sent a
  snapshot; a client that advertised `inplaceResync` is instead sent a fresh `caps`
  and a `snapshot` with `resync: true` on the same socket).
- **Compression:** the daemon owns the server side of the WebSocket frame layer
  (`crates/daemon/src/ws.rs`) and negotiates `permessage-deflate` when the client offers
  it (ADR-0019): per-connection context takeover, level 3, messages under 96 B sent
  plain, one message per frame. Measured: `ls -la` 4.2x, `ls -laR` 7.1x, a flood 691x,
  an attach snapshot 3.4x (`docs/WIRE-COST.md`).
- **Local attach socket:** `<control>-attach.sock` carries the same protocol over a Unix
  socket (`0600`, peer uid checked with `SO_PEERCRED`) for the desk attach client, so a
  desk window needs neither the tailnet nor the token.

### 6.2 Message types (implemented)

This is the shipped set (the source of truth is
`crates/protocol/src/messages.rs`; behaviour is in `docs/REMOTE.md`).
Everything below is a JSON **text** message except the last line.

Client → server: `hello {device, features?}`,
`attach {sessionId, cols?, rows?, takeover?, features?}`,
`resize {cols, rows}`, `input {data}`, `key {keys}`, `mouseclick {col, row}`,
`follow {mode}`, `scrollback {sessionId, lines?}`, `list`,
`new {kind?, cwd?, title?}`, `kill {sessionId}`, `close {sessionId}`, `desktop`,
`makelive {sessionId, workspace}`, `closelive {sessionId}`, `ping {id?}`.

Server → client: `caps {protocolVersion, sessionId, cols, rows, snapshotEpoch,
capabilities {scrollback}}`, `snapshot {…state fields…, history?, resync?}`,
`sessions {sessions[]}` (the answer to `list`/`new`/`kill`/`close`),
`created {sessionId, cwd}`, `owned {sessionId, owner}`, `mousedelivered {col, row}`,
`desktop {inventory}`, `live {sessionId, ok, detail?}`, `pong {id?}`,
`session_ended {sessionId, kind, reason?, exitCode?, exitSignal?}`,
`error {code, message}`.

`features` is an optional list of strings, additive: a client that sends none gets only the
messages it always got. `inplaceResync` — a stale picture is repaired with `caps` +
`snapshot{resync:true}` on the same socket. `sessionEnded` — the end of a session is a
`session_ended` message (and a just-ended session can be attached for its final screen).
`speechChannel` and its `speech` message **were withdrawn on 2026-11-05** with ADR-0024: the
only producer was a pi extension, which a shell app does not have. The feature name is not
advertised and the message is not sent.
Writes (`input`, `key`, `mouseclick`) need a `hello` and the holding device
(`hello_required`, `not_holder`); `new`, `kill`, `close`, `makelive`, `closelive` need a
`hello`; delivery failures are `input_failed`, `kill_failed`, `close_failed`.

Binary: exactly one frame type is emitted — `OUTPUT` (raw pane bytes) — using the
one canonical 8-byte header (`crates/protocol/src/frames.rs` defines further
frame types — hello, caps, input, resize, snapshot, follow, ping, close and so
on — reserved for later revisions; no shipped code sends them on the remote
socket, and the `flags` byte is 0).

Deliberately **cut** from the v0.1 draft: `HELLO`/`resumeToken`,
`SESSION_STATE`, a server-side `SCROLLBACK` frame, `CLOSE`, and the binary
`INPUT`/`RESIZE` frames (input and resize are JSON messages).

`snapshot`/`caps` state fields (fixes v0.1's fidelity gap — the client replays
bracketed-paste, application-cursor and opt-in mouse from them): `cols`, `rows`,
`cursorX`, `cursorY`, `alternateOn`, `bracketedPaste`, `mouse`,
`appCursor`, `historySize`, `snapshotEpoch`, read directly from the keeper's
libghostty-vt state. (`paneInMode`, a tmux copy-mode flag, was removed
2026-10-05.)

### 6.3 Connection lifecycle (snapshot-then-stream)

**Reconnect = snapshot, then stream.** The live stream is the keeper's pane output
(binary `OUTPUT` frames). The v0.1 seq-window replay is **cut**:
the keeper persists, so replaying a bounded frame window adds cache/eviction/desync bugs
for marginal benefit.

1. Client → `hello {device}` (identity; needed before any write or shell-changing
   message, and the carrier of `features`), then `attach {sessionId}` (a connection that
   attaches without a hello is a read-only observer).
2. Server → `caps`, then `snapshot` (declared geometry + state fields).
3. Client repaints from the snapshot, replays the terminal modes, and resumes the
   live `OUTPUT` stream. The server subscribed to the pane *before* capturing the
   snapshot and queues the overlap, so no output is lost in the gap.
4. Client → `input`/`key`.
5. Heartbeats: the client sends an application-level `PING` and the server
   answers `PONG`. Both shipped clients ping every **4 s** (`PING_INTERVAL_MS`
   in `crates/attach`; the same in the app), and the server **closes a
   connection that has said nothing for `idle_ms`** (20 s by default, swept every
   `idle_ms/4`). That is a correction to this section, which said the server
   never drops an idle client: it does, and it should. A socket that dies
   without a FIN — a phone that walks out of range — is otherwise
   indistinguishable from a quiet one, and the connection it left behind holds a
   claim and a pane subscription for the life of the daemon. Five missed beats is
   the margin, and the app must be a **foreground service** so the beat is not
   suspended (Android).

Fidelity: the snapshot is rendered from the keeper's own emulator (the same
libghostty-vt the phone paints with), so the screen and the replayed terminal
modes come from one source. (The tmux-era gap — `capture-pane` could not restore
every VT mode — is closed.)

### 6.4 Desktop window discipline (desktop mirror)

- The desktop window is `ldmuxd attach <id>`, a protocol client of the daemon
  (plan §3.9), not a separate terminal-multiplexer client.
- **Geometry** is handed over by the claim (ADR-0010), so the window and phone
  never fight over the size.
- The mirror defaults to **read-only** (`ldmuxd attach --readonly`) so a desktop "watch"
  pane does not fight the phone for input; "take control" is explicit.
- **Split input** is therefore avoided by default.
- **`shell`/`run` are a desk window in the terminal they were started in.** They
  do not open one: the daemon is told that the window is the attach process
  (`deskWindow {spawn: false, pid}`), so the shell is not a windowless session
  and it ends when that terminal does. The pid is `SessionRecord.livePid`,
  persisted for the same reason `liveWorkspace` is — a stock terminal has no
  window class of ours, so after a restart the kernel's answer is the only one
  there is.
- **A claim is on what the device is watching.** A connection that
  attaches to another shell releases the one it was on, unless another connection
  of the same device is still on it; a socket that closes releases after the
  grace, and a claim with no connection behind it at all is given back after the
  same grace. So a shell that no device is watching is not held — including the
  shell a desk window's claim was taken *for*, when that window never attached:
  it only takes a shell that is free, so an unbacked claim was a notice nobody
  could clear without pressing `r` (2026-10-05).
- **A phone page holds its shell only while the app is on screen.** Arriving at a
  page takes the shell over (`takeover`), and leaving it — another page, the
  launcher, the app going to the background — closes the connection, which
  releases the claim. A page composed but off screen, or an app in the
  background, is not a reader: the shell is safe in the keeper and the page keeps
  its last frame.
- **A displaced window acts on the session list.** If its shell is running and
  nobody holds it, it takes the shell back by itself — the device putting it down
  *is* the consent (ADR-0010). If the list has no running copy of its shell, it
  re-attaches to take the daemon's verdict, which is `error{session_dead}`:
  "This shell has ended.", and the window ends. That last branch was once the TypeScript
  shim's `has-session` check, and its absence was the reported bug: a window whose
  shell had been ended from the phone kept offering a reclaim that could not work.
- **The session list is pushed when it changes.** `new`, `kill`, `close`, a
  release, a window opening or closing, and the maintenance pass all broadcast it
  to every connected client, so a waiting window does not have to poll. (`close`
  from a phone on a shell the desk started is a release, not an end: the shell
  keeps running and is broadcast as unowned.)

---

## 7. Mobile UX specification (the part that usually goes wrong)

### 7.1 Orientation & layout

> **Superseded by [ADR-0011](adr/0011-screen-orientation.md).** The device owns the
> rotation of the shell view and the app owns the keyboard: portrait is typing with
> a three-row bar, landscape is the side panel, and there is no flip button. The
> launcher and its dialogs hold portrait (ADR-0011 amendment).
> Android never ignores it, because nothing is being requested.

- ~~**Landscape-locked** (FR-6).~~ Caveat: Android large-screen APIs may ignore
  `screenOrientation` ≥600 dp (foldables); provide a graceful portrait
  "please rotate" state and still function if the system forces portrait.
- **Control quarter** (FR-7): `controlsSide = left | right`, width 20–35%.
  - Session pager (live activity dot), extra-keys row(s), actions
    (`＋ shell`, `＋ pi`, `↓ jump-to-bottom`, `⌘ keyboard`, `⧉ mirror`, `⋮ settings`).
- One-tap collapse hides controls (terminal full width) behind a slim edge handle.

### 7.2 Session switching (swipe)

- Session switching is a **drawer** action; the only swipe shortcut is a narrow
  left/right **edge strip** (20 px), because a horizontal drag inside the
  terminal belongs to the running program.
  The pager lists sessions; tap to jump. Switching is instant.
- **The page order is the user's, and a page number is stable** (amended
  2026-10-25). A page is a shell the user opened, in the order they opened it:
  `pageOrder` is a list of ids and `pagesInOrder` maps it through the session list,
  which is *not* an ordering authority — the daemon sorts live sessions by last
  activity, so using it as one made the page at index N a different shell whenever
  any shell printed anything (the screen slid between shells and returned on its
  own). An id a momentarily stale list does not carry keeps its place in the order (it
  is omitted from the pages while it is missing, so later pages sit one earlier, and
  it returns at its own index). Opening from the launcher always lands on page 1. The
  header reads `3 / 5 shells`, and the launcher's tick box shows the page a chosen row
  will become, in the order the rows were chosen.

### 7.3 Input & keys

- Extra-keys bar always visible (FR-15); **sticky modifiers** built — Ctrl, Alt
  and Shift latch for exactly one following key (Shift added 2026-10-24: it is the
  only way the bar can produce `Shift+Enter`, which is a newline in a TUI such as
  `pi`). Every modified key — characters included — is sent as a protocol key name
  (`C-a`, `M-f`, `M-C-S-Left`), and the daemon's `key_name_to_bytes` (now
  `key_name_to_bytes_in_mode`, `crates/daemon/src/stream.rs`) is the one place that
  turns a name into bytes, so the client never computes an encoding. The key-name
  vocabulary is the protocol's own (`Enter`, `Tab`, `BSpace`, `DC`, `IC`,
  `PPage`, `NPage`, `S-Enter`, `C-<letter>`, ...); an unknown name is an error,
  never typed into the shell.
- **The four arrows are a spatial pad, not a row** (2026-10-24): `Up` above `Down`,
  `Left` and `Right` either side of it, in both modes. One composable
  (`ArrowPad`), driven by `ARROW_PAD_ROWS` in `Keys.kt`, so portrait and landscape
  cannot drift.
- **Portrait carries `paste`** (2026-10-24). Copy is the platform's own (a
  selection, then the copy action); paste is the one thing no gesture provides, and
  landscape has had it all along.
- **Modified `Enter`, `Escape` and `Space` use the Kitty keyboard protocol when the
  program has asked for it** (`CSI 13;2u` for `Shift+Enter`, ADR-0022). Without the
  protocol the encoding is the legacy one (Enter = `CR`, Shift collapsed), so an
  old daemon or a program that never asked sees exactly what it always saw.
- **There is no copy-mode.** The client scrolls its own buffer, and a full-screen
  app on the alternate screen has no scrollback to page through. (The tmux-era
  copy-mode toggle, its `copymode` message and the `paneInMode` flag were removed
  2026-10-05; nothing in the client called the toggle.)
- Gestures: two-finger drag → scroll; pinch → font size, **per shell and not
  persisted** — it lives in the composition for as long as that shell is open,
  and the settings dialog's text size is the default a *new* shell starts at
  (amended 2026-09-27: it used to be saved on every pinch, which made one
  shell's zoom re-size every other open one); long-press →
  select/copy; double-tap empty area → `Tab` (optional).
- IME-safe layout via `WindowInsets`; the control quarter does not jump when the
  soft keyboard opens.

### 7.4 Scroll model — the crown jewel

The classic bug: stale `wasAtBottom`/pixel snapshots racing user wheel input
across BSU→ESU→rAF windows (25 wheel-up notches → net −41 lines). Fix: an explicit
mode state machine.

ldmux keeps the **property** and not the mechanism. The anchor is the emulator's
own viewport, read from the frame header on every render, rather than a
client-side line identity:

- The scroll position always comes from libghostty-vt (`viewport_active`,
  `scroll_offset`) through the C shim, so there is one source of truth for where
  the view is and no second line space to reconcile.
- `scrollBy` **asks, then reports what actually moved**, read back from the header
  instead of assuming the request was honoured. On the alternate screen — vim,
  less, htop — the engine refuses, the report is 0, and a drag moves nothing.
- **Only explicit user intent** changes the view. **Output never moves the
  viewport**: measured 2026-09-27 in `packages/mobile/native/test_engine.c`
  (`output does not move a scrolled viewport`), a viewport parked 10 rows back is
  unchanged by 30 further lines of output.
- BSU/ESU (DEC 2026), ED3, alternate-screen enter/exit and resize reflow are the
  emulator's, not byte scanning — the parser is the engine.
- Persistent **"↓ jump to bottom"** affordance while the viewport is away from the
  live bottom, and any text the session sends returns the view to the bottom,
  because typing happens at the prompt.

**Correction (2026-09-27).** This section described a client-owned anchor by
absolute line identity, a `following`/`anchored` mode machine, `FOLLOW {mode}`, and
a `snapshotEpoch` baseline, and ADR-0004 named a `Scroller` module with
`scrollMode`, `anchorFromBottom` and `anchoredTarget`. None of it was built: no such
module exists, the client never sends `FOLLOW`, and the `snapshotEpoch` that `caps`
carries is never read by the client. What is built is the emulator's viewport above,
which delivers the property this section exists for without needing a line identity
to do it. The superseded description is left in ADR-0004 as the record of what was
planned.
- Local scrollback cap: **1,000 lines on mobile** (measured on a browser client that has since been deleted)
  with a configurable max; the keeper's per-session scrollback is bounded by the emulator
  (10,000 lines was the tmux-era figure, ~40 MB/pane) and the `scrollback` message serves older pages on demand.

### 7.5 Session list semantics (FR-9)

- Live list = sessions the keeper still has and status ≠ `dead`.
- A separate, clearly-marked "ended (recent)" section retains scrollback.
- On the `dead` event, the session leaves the live pager with a brief toast +
  "view last output."

---

## 8. Opening sessions from the phone (FR-10)

The `new {kind?, cwd?, title?}` WebSocket message (or `ldmux new`/`run`; there is
no REST layer, §5.4):

- `shell` → keeper `Spawn` with `cwd`, session name `ldmux-<ulid>`, the user's shell.
- `pi` → keeper `Spawn` with `command: ["pi", …]`.
- `cwd` is validated by the daemon; the launcher's folder picker is a client feature.

---

## 9. Desktop mirror (FR-11)

`ldmux mirror <id> [--workspace N] [--rw] [--terminal T]`:

1. Workspace: `--workspace`, else **`10`** (`desktop::DEFAULT_MIRROR_WORKSPACE`).
2. **Opening a window needs no window manager.** The daemon spawns a terminal emulator
   directly (detached, own process group, absolute path, the desktop session's
   environment) running `ldmuxd attach <id> [--readonly]`. The terminal is chosen from
   `LDMUX_TERMINAL_CMD`, `LDMUX_TERMINAL` (or `--terminal`), `$TERMINAL`, then the first
   found on `PATH` of alacritty, kitty, foot, wezterm, gnome-terminal, konsole,
   xfce4-terminal, xterm, x-terminal-emulator, xdg-terminal-exec. A built-in template
   table gives each known emulator its class flag and keeps it in the foreground; an
   unknown one gets `-e {cmd}`. `LDMUX_TERMINAL_CMD` is a free-form command line with
   `{cmd}` (the attach command; appended if absent) and optionally `{class}` (the window
   class / app-id, `ldmux-mirror-<id>` or `ldmux-live-<id>`).
3. **Placing it on a workspace is an optional adapter** (`desktop::Wm`): i3 (`i3-msg`),
   sway (`swaymsg`) or hyprland (`hyprctl`), detected from the session's own
   sockets; the window is found by its class and moved there once the manager reports
   it. **Never** `workspace N; exec …` — that moves the user's view, which is what the
   tmux-era mirror did. Without an adapter (or a terminal that cannot set a class) the
   window is "opened, not placed". i3 is the only adapter measured; sway and hyprland
   are written but untested (`docs/PLATFORMS.md`). The class is per session so two
   mirrors of two shells cannot move each other.
4. Read-only unless `--rw`. The argv is built in one place for the desk window
   (`live`) and the mirror, so the two cannot drift.

Environment: the daemon is a `systemd --user` service whose unit sets no display
variables. It needs `DISPLAY` or `WAYLAND_DISPLAY` (and the WM sockets) in its own
environment, or learns them from the user manager (`systemctl --user
import-environment …`, run once from the session's startup; the unit header lists the
variables); without them `desktop.rs` reports "no graphical session" rather than
guessing. `deploy/bin/ldmux-terminal` is the absolute-path launcher a key binding runs
(it asks `ldmux window`, and falls back to a plain terminal, logged).

---

## 10. Tailscale & security (FR-4)

- **Transport:** the system `tailscaled` (a prerequisite, not shipped). The daemon binds
  this machine's tailnet address directly (`LDMUX_REMOTE_HOST` in
  `~/.config/ldmux/env`, read by the unit's optional `EnvironmentFile`; template
  `deploy/ldmux.env.example`), plain `ws://` under WireGuard (ADR-0016). No
  `tailscale serve`, no Funnel.
- **Gates:** tailnet or loopback peer address (checked per request, `403`
  otherwise) + bearer token. Constant-time comparison.
- **Refuse** to bind anything but a tailnet or loopback address: `ldmuxd` exits
  at startup (`crates/daemon/src/bin/ldmuxd.rs`, rule in `crates/protocol/src/net.rs`),
  and the remote server refuses to start on one.
- Mobile: Android allows cleartext (`usesCleartextTraffic`, needed for `ws://`
  over the tailnet); the app refuses a pairing host that is not tailnet or
  loopback. Token in the app's private SharedPreferences; iOS in Keychain (iOS
  is a **target**, not v1 — see §11.2).

---

## 11. Technology stack

### 11.1 Daemon language

**Rust** (edition 2024; direct dependencies `tokio`, `tokio-tungstenite`, `serde`,
`serde_json`, `futures-util`, `uuid`, `nix`, `rand`, and `flate2` with the pure-Rust
backend for permessage-deflate, ADR-0019; the list is in `AGENTS.md`). The v0.x recommendation was TypeScript on
Bun, then Node 26 (ADR-0009); the Rust port replaced it and the TypeScript tree
was deleted at D4 (ADR-0018). The session layer is `crates/keeper`, with the
terminal emulator libghostty-vt reached through `crates/vt-sys`.

### 11.2 Mobile stack

**KMP + Compose Multiplatform**, native renderer (§11.3), Android shipped and
device-verified, iOS not started (`docs/IOS-DEVELOPMENT.md`; the C core builds for
aarch64 macOS and Linux, the iOS target needs the Apple SDK). The options this
section once weighed (WebView/xterm.js, Flutter, PWA, `alacritty_terminal`) are
history; the buildless PWA that shipped first was deleted. Libraries: **Ktor**
(`ktor-client-websockets`), `kotlinx.serialization`, foreground service +
`ConnectivityManager`.

**Android implementation (built):**
- **Foreground service**, declared `specialUse` — a long-lived *interactive*
  connection, where `dataSync` carries a 6h/24h cap (Android 15) that a shell left
  open would exceed. Notification channel + `Disconnect` action; runtime
  **`POST_NOTIFICATIONS`**, asked for at the first connection rather than at
  launch, and denial is not fatal.
- **Partial wake lock** while the service runs, released the moment it stops.
- **`ConnectivityManager.NetworkCallback`** cuts the retry backoff short — only for
  a *validated* network, because waking the loop during a handover is how an app
  gets throttled.
- Reconnect backoff is 400 ms doubling to 10 s. A fixed 2 s made a daemon that
  restarted in a second feel broken and one down for an hour get asked 1800 times.
- **Orientation follows the device** for the shell view; the launcher holds
  portrait (ADR-0011).
- **Remaining:** OEM battery-exemption guidance (Samsung/Xiaomi/OnePlus) in
  onboarding; the tunnel/VPN path is manual.
- **iOS is not started.** iOS constraints (no background service model, ~3-min WS
  background) are documented but unimplemented.

The service is keyed on a terminal being **open**, not on being **connected**, and
that distinction is the whole feature: backgrounding drops the socket, which would
clear `connected`, which would stop the service that exists to keep the process
alive to reconnect. Measured as a deadlock before it was fixed.

### 11.3 Rendering engine (client)

- **Cell model, frame version 1.** The native engine's frame (`ldmux_terminal.h`,
  mirrored by `TerminalFrame.kt`; `LDMUX_FRAME_VERSION` is 1) is additive: per-cell flags
  now carry the underline style (single, double, curly, dotted, dashed; bit 2 still means
  "underlined" for every style, so an older reader draws a single underline), the
  wide-glyph head flag, and a hyperlink flag, with an extension block for underline
  colour and OSC 8 URIs. **Known gap:** the Android painter does not draw the new
  attributes yet (underline style and colour, hyperlinks, wide glyphs; it still draws a
  plain underline from bit 2), and the daemon's
  snapshot replay (`crates/vt-sys/src/snapshot.rs`) does not carry hyperlinks, so a
  reattach drops them.

- **Shipped (v1):** native libghostty-vt behind JNI, rendered on a Compose
  Canvas (ADR-0007, `packages/mobile/native/`). This section used to list "v1:
  xterm.js in a WebView... Future: native renderer" — the native renderer is
  what shipped, and `packages/web` (the xterm.js/WebView client) was deleted.
- **Superseded:** xterm.js in a WebView (no fit addon: the canonical grid was
  CSS-scaled, never reflowed — ADR-0002), optional `@xterm/addon-webgl`
  (canvas/DOM fallback), with a scroll-anchor addon around the xterm.js
  viewport. Kept here as a record of the v0 design, not as current behavior.

---

## 12. Project layout

```
ldmux/
├─ README.md · AGENTS.md · scripts/ · flake.nix · Cargo.toml
├─ crates/
│  ├─ protocol/       # frames, messages, keeper messages, rpc, pairing, net,
│  │                  # paths, socket — the wire contract, one implementation
│  ├─ keeper/         # the session layer: PTYs, child supervision + reaping,
│  │                  # one libghostty-vt per session, the keeper socket server
│  ├─ daemon/         # registry, ownership, remote WS, streams, desktop +
│  │                  # inventory, the CLI (`bin/ldmux`), `bin/ldmuxd`
│  ├─ attach/         # the desk-window client (`ldmuxd attach`)
│  └─ vt-sys/         # hand-written FFI against the vendored libghostty-vt
├─ deploy/            # the systemd user unit template, ldmux.env.example, the
│                     # `ldmux-terminal` launcher
├─ scripts/           # install.sh, restart-keeper.sh, android.sh (build/test),
│                     # measure_latency.ts, slow-link.ts (dev tools, need Node),
│                     # regen_parity_golden.sh (needs tmux; regenerates goldens)
├─ docs/
│  ├─ SPEC.md · STATE.md · PLATFORMS.md · REMOTE.md · NATIVE-TERMINAL.md ·
│  │  PREDICTIVE-ECHO.md · MESH-TRANSPORT.md · WIRE-COST.md · DESKTOP-INTEGRATION.md ·
│  │  IOS-DEVELOPMENT.md
│  ├─ archive/        # historical records: the port plan/log/audit, the tmux study,
│  │                  # TRANSPORT.md, the pre-port investigation, the old STATE.md
│  └─ adr/            # the decision record; 0018 is the session layer
└─ packages/
   └─ mobile/         # native KMP + Compose Multiplatform client (Android
                      # first), with the libghostty-vt core behind JNI
```

The TypeScript implementation (`packages/{daemon,protocol,testing}`, and tmux as
the session layer) was deleted at D4 — see ADR-0018 and
`docs/archive/RUST-PORT-LOG.md`. It is readable at git commit `6b3efb8`.

Binaries: `ldmux` (CLI), `ldmuxd` (daemon), `ldmux-keeper` (auto-spawned).

---

## 13. Phased plan — **historical**

> This is the plan as written against tmux and TypeScript. It was executed, and
> then the implementation it describes was replaced: the phases ran as the Rust
> port (`docs/archive/RUST-PORT-PLAN.md`, `docs/archive/RUST-PORT-LOG.md`), and the tmux/TS tree
> was deleted at D4 (ADR-0018). Kept as the record of how the product was built;
> `packages/**` paths below no longer exist.
> (§§13–17 still use tmux-era wording; they are the historical record.)

**Phase 0 — spec (this doc).**

**Phase 1 — local core.**
- `packages/protocol`: frame/message types + tests.
- `ldmuxd`: tmux control plane (`list`/`new`/`send-keys`/`capture-pane`/
  `display-message`), the control-mode `PaneStream` (parser + `%output`/
  `%extended-output` + flow control) with the capture-pane snapshot/fallback path,
  registry JSON, Unix-socket API, lifecycle/`remain-on-exit`.
- **Transport (ADR-0001):** DONE — `-C` over pipes proven on tmux 3.5a + Bun 1.3.9.
  Remaining: implement the robust parser (framing, octal→bytes, per-pane
  incremental UTF-8, `%begin/%end` correlation) and a burst/`%pause` test.
- `ldmux` CLI: `ping`, `list`, `new`, `run`, `send`, `key`, `capture`, `snapshot`,
  `kill`, `close`, `forget`, `adopt`, `mirror`, `remote`, `code`, `pair` (all implemented;
  later verbs are listed in §4.2).
- **Geometry probe (ADR-0002):** confirm `window-size manual` behavior and the
  canonical-size default.

**Phase 2 — pi extension.** Dropped: the extension was removed (ADR-0009) and
ldmux is not a pi plugin.

**Phase 3 — remote + auth.**
- Direct tailnet bind + token (ADR-0016) and QR pairing via `ldmux code`
  (ADR-0017). **DONE.**
- Reconnect (snapshot-then-stream), heartbeats, `resumeToken`.

**Phase 4 — mobile client (Android v1).**
- **Shipped:** the PWA — canonical-grid scaling, extra-keys bar, session drawer +
  edge-strip paging, client-owned scroll state machine + `follow`, `scrollback`
  paging, unread markers, offline app shell. Since superseded by the native
  KMP/Compose client (ADR-0007): KMP scaffold, `AttachClient`, the
  libghostty-vt/Compose Canvas renderer (not a `TerminalSurface` WebView host —
  that plan changed, see ADR-0007), and the new-shell dialog
  (`NewShellModal`, `Modal.kt`) are all in place. This section used to list
  these under "Remaining (target)".
- **Both of the items this note called open are built.** The foreground service
  is `BackgroundSession.android.kt` (declared `specialUse`, with a notification
  and a `Disconnect` action, a partial wake lock, and a `NetworkCallback` that
  cuts the retry backoff short), and the directory browser is the folder picker in
  the launcher's new-shell dialog. E2E over Tailscale from the phone has since
  been run repeatedly (the 2026-10-05 measurements in
  `docs/archive/RUST-PORT-LOG.md`).

**Phase 5 — desktop mirror + polish.**
- i3 + alacritty mirror (read-only default), focus/close helper.
- Settings, theming, `install.sh`, `ldmux doctor`. The `systemd --user` unit
  (`deploy/ldmux.service`) and `nix run .#install` exist.

---

## 14. Risks and open questions

| ID | Risk / question | Mitigation |
|----|-----------------|------------|
| R1 | Scroll fidelity is hard. | ~~Explicit `following`/`anchored` client state machine from day one~~; **built as the emulator's own viewport, with the property tested** (§7.4 correction, 2026-09-27). |
| R2 | Snapshot fidelity (cursor/modes/alt-screen/soft-wrap). | **Closed**: the snapshot is rendered from the keeper's libghostty-vt; gated by the §3.11 golden parity corpus. (Tmux-era mitigation was `capture-pane -e` + state fields.) |
| R3 | ~~tmux version differences.~~ Moot: no tmux. | Replaced by the keeper as the single point of failure (plan R3). |
| R4 | Android background kill severs the WS. | Foreground service, wake lock, backoff + jitter, keeper persistence, resync on resume. |
| R5 | ~~WebView WebGL2 availability.~~ Moot: the shipped renderer is native Compose Canvas over libghostty-vt, not a WebView (ADR-0007) — this row is kept to record that the risk was real for the design it was written against, not because it still applies. | ~~Fallback to canvas/DOM; test on the real device.~~ |
| R6 | ~~tmux socket under `/tmp` / `TMUX_TMPDIR` nesting.~~ Moot: no tmux. | Tests use unique per-run keeper socket dirs and reap their keeper (`tests/common` `Reaper`). |
| R7 | Split input with a desktop mirror. | Mirror read-only by default; explicit "take control". |
| R8 | i3 placement races. | **Corrected 2026-10-05.** `for_window` is not an IPC command at all (`docs/DESKTOP-INTEGRATION.md`, measured), so the mitigation that stands is `exec` and then a criteria-targeted move, polling `i3-msg -t get_tree` for the window's class — `ldmux-live-<id>` for a desk window, `ldmux-mirror-<id>` for a mirror. This is what §9 and `crates/daemon/src/desktop.rs` do. |
| R9 | Token/identity security. | Tailnet-or-loopback bind and peer check (ADR-0016), WireGuard encryption, token `0600` (plaintext at rest; its directory is `0700`). No identity allow list or per-device tokens yet. |
| R10 | Language choice. | **Closed**: Rust (ADR-0018). The TS/Bun v1 was ported and deleted. |
| R11 | ~~pi RPC UI sprawl.~~ Moot: there is no pi RPC surface. Removed 2026-11-05 (§4.5 deleted, `SessionDriver` seam dropped) because the product is a shell app: a terminal, three ways to reach it, and nothing Pi-specific. |
| R12 | ~~Control-mode parser robustness.~~ Moot: the keeper protocol is our own framed JSON/binary. | Split-UTF-8 carry (`protocol::utf8::Utf8Carry`) and frame tests. |
| R13 | iOS scope creep. | v1 = Android; iOS is a documented target. |
| Q1 | Wrapper attaches interactively? | Recommend yes: it is what a wrapper is for. |
| Q2 | ~~serve vs direct bind~~ | **Resolved (ADR-0016): direct tailnet bind**, superseding ADR-0005's `tailscale serve`. |
| Q3 | Native renderer timing? | Defer behind `TerminalSurface`. |
| Q4 | Session families? | Phase 2+. |

---

## 15. Acceptance criteria (v1)

- A session started on the desktop appears in `ldmux list` and on the phone;
  asserted by polling the daemon's `list` after launch (bounded, tested).
- From the phone: create a shell and a pi session in arbitrary directories; both
  appear and are interactive.
- Streaming output does not move the viewport while the user is scrolled up; a
  "jump to bottom" affordance works.
- Sessions that end on the host disappear from the live pager (retained under
  "ended (recent)").
- The desktop mirror opens a live `alacritty` on the configured workspace and is
  read-only by default.
- Reconnect after an airplane-mode toggle resumes with correct screen and anchor.
- No leaked processes after `ldmuxd` restart; the keeper and its sessions survive.
- Reached only over Tailscale; token required; nothing bound publicly.

---

## 16. Appendix — key prior art

> *Historical: prior art that informed the tmux-era design.*

See [`archive/INVESTIGATION.md`](archive/INVESTIGATION.md) for citations. Summary: PTY
runners with web dashboards and two-gate Tailscale auth; tmux control mode over a
PTY with a multi-client attachment model, mobile UX and performance research, a
terminal codec and portability lessons; trustmux/ttyd/wetty/gotty/mttyd (tmux+
Tailscale+web; mobile input fixes); Mosh/Blink/Termius/Termux (mobile interaction
patterns); pi-mobile/pi-remote/paseo (WebSocket bridges over `pi --mode rpc`);
tmuxy-core/cdx/tmux-control-mode-js (control-mode parsers); SocketPipe (framing).

---

## 17. Architecture Decision Records

> *ADR-0001 to ADR-0003 below record tmux-era decisions; ADR-0018 is the session layer that replaced them.*

| ADR | Decision |
|-----|----------|
| **ADR-0001** | ~~**Transport:** tmux control mode `-C` over pipes (no PTY) + `capture-pane` snapshots/fallback, behind a `PaneStream` interface.~~ **Superseded** by ADR-0018. |
| **ADR-0002** | ~~**Canonical geometry:** pin `window-size manual` + a canonical grid; clients render viewports and never resize the pane.~~ **Superseded** by ADR-0018 (the keeper is the canonical geometry). |
| **ADR-0003** | ~~**Extension lifecycle:** branch on `session_shutdown.reason`; only `quit` ends a session; never tear down the daemon/tmux on shutdown.~~ **Superseded** (the extension is gone, ADR-0009). |
| **ADR-0004** | **Scroll anchor:** ~~client-owned explicit `following`/`anchored` state machine; server never resolves line ids~~ — **as built, the anchor is the emulator's viewport and `scrollBy` reports the movement it measured** (see ADR-0004's correction). |
| **ADR-0005** | ~~**TLS/remote:** loopback bind + `tailscale serve --bg`; token + QR pairing; no Funnel.~~ **Superseded** by ADR-0016 and ADR-0017. |
| **ADR-0016** | **Direct tailnet bind:** listen only on the tailnet address or loopback, `403` for any other peer, plain `ws://` under WireGuard, bearer token. |
| **ADR-0017** | **QR pairing:** `ldmux code` shows `ldmux://pair?v=1&host=…&token=…&name=…` as a QR code; the app scans it from Settings; no manual host/token entry. |
| **ADR-0018** | **Session layer:** `crates/keeper` owns the PTYs and runs libghostty-vt; supersedes ADR-0001, ADR-0015 and ADR-0006 §2; tmux and TypeScript removed. |
| **ADR-0020** | **Mesh link security (Proposed):** end-to-end Noise IKpsk2 session (X25519, ChaCha20-Poly1305, 256-bit PSK from the QR; optional ML-KEM stage) inside MeshCore, which is treated as an untrusted transport. Design only. |
| **ADR-0021** | **Keeper as a unit, shells as scopes:** `ldmux-keeper.service` runs the keeper; the daemon starts it with `systemctl --user start`; each shell is moved into its own transient scope under `ldmux-shells.slice` (never blocking the shell on failure); detached-spawn fallback without systemd. Amends ADR-0015/0018's restart-survival mechanism. |
| **ADR-0022** | **The terminal answers its own queries:** the keeper's emulator answers DA/DSR, DECRQM, XTVERSION, kitty-keyboard and window-size queries so they never reach the client, and modified Enter uses the Kitty keyboard protocol (`CSI 13;2u` for `Shift+Enter`). |
| **ADR-0023** | **The L3 session's crypto dependencies:** `snow` (Noise `IKpsk2_25519_ChaChaPoly_SHA256`) plus `hmac`, `sha2` and `zeroize` in `crates/session` only, with `default-features = false` so the C-backed `ring` stays out. No hand-rolled cryptography. |
| **ADR-0025** | **The mesh terminal profile:** inside the L3 session the host sends the keeper's screen as plain-text rows, then only changed rows, in messages of at most 133 bytes (one record each); the phone paints on commit; scrollback, colour, hyperlinks, mouse, images, resize and clipboard are declared unsupported in `Profile`. Host side built; phone side not. |
| **ADR-0024** | ~~A pi extension returns, for voice only.~~ **Withdrawn 2026-11-05:** ldmux is a shell app; the extension, the `speech` RPC and the phone chain are deleted. |

ADR-0006 to ADR-0015 (divergences, native engine, UI modes, the Node runtime,
exclusive ownership, orientation, background/sleep, cross-platform seams, text
selection, the tmux session-layer survey) are summarised by their own status
lines under `docs/adr/`; every ADR carries a `Status:` line directly under its title.

Individual ADRs live under `docs/adr/`. ADR-0001 to ADR-0003 and ADR-0015 are
superseded and kept as history; ADR-0006 records the deliberate divergences from
the prior art.