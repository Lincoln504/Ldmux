# ldmux — Software Specification

> **Status note (2026-09-14) — two decisions post-date this document.**
>
> - **Runtime: Node 26, not Bun.** The daemon/CLI/protocol run on Node 26 with
>   TypeScript stripped by Node itself (no build step, erasable syntax only).
>   `ws` is the only production dependency. See
>   [ADR-0009](adr/0009-node-runtime-and-no-pi-extension.md).
> - **The pi extension has been removed.** ldmux is not a pi plugin; the
>   `packages/extension` package, the `install-extension` CLI command, and the
>   `register`/`session_end` RPC methods are gone, along with the RPC-only session
>   expiry that existed to serve them. Sessions are created with `new`/`adopt`.
>
> The rest of this document is still the design of record for the transport,
> geometry, scroll and lifecycle decisions; read "Bun" as "the Node runtime" and
> any extension reference as historical.

**Version:** 0.3 (design; transport decision finalized)
**Date:** 2026-09-13
**Author:** Lincoln Deen (with the pi coding agent)
**Status:** Phases 1–3 implemented (see README Status). This document remains the authoritative design; where it describes a *target* API or behaviour that is not built, that is called out inline, and `AGENTS.md` plus the tests are the statement of what exists.

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
> capture-pane snapshots/fallback, still under Bun with no native dependency.
> See [`TRANSPORT.md`](TRANSPORT.md).

---

## 0. TL;DR

`ldmux` is a **tmux-native remote terminal system**:

- A long-lived **daemon** owns a private `tmux` server and exposes it over the
  **tailnet** via a WebSocket API.
- A **pi extension** makes the daemon "always available" while any pi session is
  alive, registers pi sessions, and adds tools/commands.
- A **mobile client**: a native **KMP + Compose Multiplatform** Android app
  (`packages/mobile`, landscape, WebView hosting `packages/web/public`, swipe
  pager) — see `packages/mobile/README.md`. The installable
  landscape-locked **PWA** (`docs/WEB.md`) is the renderer the app hosts, and
  remains usable standalone from a browser.
- A thin **CLI** and an optional desktop **alacritty mirror** round it out.
- **tmux itself** provides persistence, multi-client attach, and synchronized
  phone+desktop viewing.

The primary design goal is to fix the **mobile terminal UX** that made gmux
painful (scroll jumping, session switching, modifier keys) while staying far
simpler than gmux because it is tmux-native and pi-native.

---

## 1. Background and intent

### 1.1 What exists today

- **gmux** (Go, v2.1.0): wraps arbitrary commands in a PTY, auto-starts `gmuxd`,
  serves a Preact/xterm.js dashboard at `127.0.0.1:8790` (optionally via embedded
  tsnet). A bash function wraps interactive `pi` as `gmux -- pi "$@"`. Rendered in
  a **browser**, which is where the mobile UX is worst.
- **dooboxes** (Rust, `~/Documents/dooboxes`): a container-workspace product with
  a tmux-backed session bridge, terminal codec, `alacritty_terminal` core, Tauri
  desktop, and deep prior research on mobile terminal UX/perf and tailnet
  transport. Different product, excellent lessons.
- **Tailscale** is installed and running (`debian.tail496756.ts.net`,
  `100.127.119.54`); the user's Android phone is on the tailnet.

### 1.2 Why not fork gmux

gmux's PTY-runner + daemon + adapter + browser stack is more machinery than
needed. The user wants tmux, a native-ish mobile app, pi-native lifecycle,
Tailscale only, and far less functionality than dooboxes.

### 1.3 What we keep from gmux

- Child-awareness (env vars + a local endpoint the child reports to), simplified.
- Optional session families / parent-child (phase 2+).
- The hard-won lesson that scroll anchoring must be an explicit client state
  machine (gmux PR #500, `@gmux/scroll-anchor`).
- Tailnet-only remote access.

### 1.4 What we keep from dooboxes

- tmux control mode (`-C` over pipes) as the real-time transport — the
  corrected finding (ADR-0001): `-C` needs **no** PTY, only `-CC` does. Its
  concept of a *stateful per-stream sanitizer* and its diff/scroll lessons are
  kept; its ControlModeClient is not a reference implementation.
- A server-authoritative session model; clients are thin.
- One terminal renderer behind one client interface.
- Tailnet-first, WireGuard-encrypted, no public ACME.
- E2E pins for every cross-component contract.
- The validated mobile UX vocabulary (Termux-style extra keys, gestures,
  jump-to-bottom) and mobile perf constraints.

---

## 2. Goals and non-goals

### 2.1 Goals

1. **G1 — tmux-native.** Every session is a tmux session on a private socket.
2. **G2 — pi extension first.** `ldmux` is available whenever a pi session process
   is active; it registers pi sessions and can spawn new pi/shell sessions.
3. **G3 — Mobile app, landscape-only.** Terminal ~3/4; configurable left/right
   control quarter; horizontal swipe to switch sessions; stable scrolling.
4. **G4 — Open shells/pi in any directory from the phone**, optionally mirrored
   live into a desktop `alacritty` window on a chosen i3 workspace.
5. **G5 — Synchronized multi-view is a nice-to-have**, achieved via tmux
   multi-client attach; if it complicates anything, degrade gracefully.
6. **G6 — Tailnet only.** Reachable only over Tailscale; token auth in addition to
   WireGuard.
7. **G7 — Robust and simple.** Reconnect/resync, no scroll jumping, clean session
   lifecycle, no leaked processes, observable.
8. **G8 — Best-engineered.** Typed contracts, tests, docs, reproducible builds.

### 2.2 Non-goals

- No containers / Docker / devcontainers (that is `dooboxes`).
- No hosted relay, billing, Supabase, Centrifugo, Play Integrity.
- No public internet exposure (no Funnel by default).
- No adapter zoo; only **shell** and **pi** session kinds.
- No arbitrary command-wrapping; `ldmux` starts tmux sessions.
- No synchronous live typing requirement.
- No Windows/macOS server support in v1; Linux (this machine) first.

---

## 3. Functional requirements

| ID | Requirement |
|----|-------------|
| FR-1 | pi sessions are discoverable and controllable through `ldmux` (within a defined scope — see §4.6). |
| FR-2 | `ldmux` runs as a **pi extension** and is "up and ready" whenever a pi session process is active. |
| FR-3 | The backend must be **tmux-based**. |
| FR-4 | Remote access is **Tailscale-only**. |
| FR-5 | A **multi-platform mobile app** exists (KMP/Compose preferred). |
| FR-6 | The mobile app is **landscape-only**. |
| FR-7 | Terminal ~3/4 of the screen; control panel 1/4, side (left/right) is a setting. |
| FR-8 | A **clean swipe UI** between open shells and pi sessions. |
| FR-9 | Sessions closed/ended on the host disappear from the mobile live list. |
| FR-10 | From the phone, open new shells and new pi sessions **in any directory**. |
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
                       │  (WireGuard; debian.tail496756.ts.net)       │
                       └───────────────┬─────────────────────────────┘
                                       │  TLS/WSS (tailscale serve, token)
        ┌──────────────────────────────┴──────────────────────────────┐
        │                                                               │
 ┌──────▼────────┐        ┌───────────────────────────────┐     ┌──────▼───────┐
 │  MOBILE APP   │        │        ldmuxd daemon          │     │  DESKTOP     │
 │ KMP/Compose   │◄──────►│  • pane stream + snapshots    │     │  MIRROR      │
 │ landscape     │  WS    │  • session registry (JSON)    │     │ alacritty -e │
 │ xterm.js in   │        │  • WS+HTTP over tailnet       │     │ tmux attach  │
 │ WebView       │        │  • spawn/kill/list sessions   │     └──────┬───────┘
 │ control 1/4   │        │  • optional i3 mirror helper  │            │
 └───────────────┘        └───────┬───────────────────────┘            │
                                  │ tmux commands + pane stream        │
                          ┌───────▼───────────────────────┐           │
                          │  tmux server (-L ldmux)       │◄──────────┘
                          │  session: ldmux-<ulid>        │  (native multi-client attach)
                          │    pane 0: bash | pi          │
                          └───────────────────────────────┘
                                  ▲
                                  │ session_start (reason-branched)
                          ┌───────┴───────────────────────┐
                          │    pi extension (in-process)  │
                          └───────────────────────────────┘
```

### 4.2 Components

| Component | Language | Role |
|-----------|----------|------|
| **`ldmuxd`** (daemon) | TypeScript on Bun (Node fallback) | Owns the private tmux server; pane streaming + snapshots; session registry; WS+HTTP over tailnet; spawn/kill; optional i3 mirror helper. |
| **`ldmux`** (CLI) | TypeScript (Bun) | `ping`, `list`, `new`, `run`, `send`, `key`, `capture`, `snapshot`, `kill`, `forget`, `adopt`, `mirror`, `remote`, `pair`, `expose`, `install-extension`. Unix-socket local control. (`attach` is `tmux attach` via `run`/`mirror`; there is no `doctor`.) |
| **pi extension** | TypeScript | Ensures `ldmuxd` runs; registers the current pi session; `/ldmux` + `ldmux_*` tools; **reason-branched** `session_shutdown`. |
| **Mobile app** | Kotlin Multiplatform + Compose Multiplatform | Landscape UI; xterm.js WebView renderer; Ktor WebSocket; foreground service; swipe pager; control quarter. |
| **Desktop mirror** | shell + i3 | `i3-msg` + `alacritty -e tmux -L ldmux attach-session [-r] -t <name>`. |

### 4.3 Why the daemon is separate from the pi extension

pi extensions run **in-process**; they cannot host a robust long-lived multi-client
server that outlives a single pi session. So the extension is a
**supervisor/bridge**, and the daemon is a separate process (spawned detached on
first need and/or installed as a `systemd --user` service). The daemon outlives
individual pi sessions — required by FR-9/FR-12.

### 4.4 Session model

A **session** is one tmux session on the private socket `-L ldmux`.

- **Kind:** `shell` (default `$SHELL`) or `pi` (command `pi`, optional args/prompt).
- **Name:** human label; the tmux session name is a stable machine id
  `ldmux-<ulid>` (decoupled because tmux names forbid some characters).
- **cwd:** arbitrary directory (FR-10).
- **Created-by / owner:** which pi session or client created it (families, phase 2).
- **State:** `starting | running | attached | detached | dead`.
- **Persistence:** tmux keeps the pane alive across disconnects; `remain-on-exit`
  lets the daemon observe exit and retain scrollback.

**Pi sessions** are ordinary tmux sessions whose pane runs `pi`. Interactive `pi`
is launched into tmux by the shell wrapper:

```bash
# ~/.bashrc (new)
pi() {
  [[ -n "$LDMUX" || -n "$TMUX" ]] && { command pi "$@"; return; }   # never nest
  case "$1" in -p|--print|--mode|--version|-h|--help|update|install|remove|uninstall)
       command pi "$@"; return;; esac
  ldmux -- pi "$@"
}
```

**Child env contract** (injected by `ldmux -- <cmd>` / `new-session`):

| Var | Meaning |
|-----|---------|
| `LDMUX` | `1` — running under ldmux (nesting guard). |
| `LDMUX_SESSION_ID` | ldmux session id (`<ulid>`). |
| `LDMUX_SOCKET` | daemon Unix socket path for child reporting. |
| `LDMUX_KIND` | `shell` \| `pi`. |
| `LDMUX_TMUX_NAME` | the session's tmux session name (for `tmux -L $LDMUX_TMUX_SOCK`). |
| `LDMUX_TMUX_SOCK` | the private tmux server socket name (`ldmux` by default). |

`LDMUX_DAEMON_TOKEN` is **not** implemented: children reach the daemon through
the Unix socket, whose 0600 permission plus a 0700 state dir is the boundary.

### 4.5 Pi RPC as a complementary channel

pi 0.85.1 exposes `pi --mode rpc` (JSONL over stdin/stdout; `prompt`, `steer`,
`follow_up`, `abort`, `new_session`, `get_state`, `get_entries`, `set_model`,
extension-UI round-trips, streaming events) and an `RpcClient`. This enables
**semantic** control that raw terminal I/O cannot express.

**v1 decision:** terminal-first; RPC is not required. The daemon defines a
`SessionDriver` seam so a future `pi-rpc` driver can expose a semantic pi UI on
mobile (as `pi-mobile`/`pi-remote` do) without disturbing the terminal path.

### 4.6 Session discovery and adoption scope (FR-1)

Honest v1 scope:

1. **ldmux-created sessions** (via `ldmux --`, the wrapper, the extension, the CLI,
   or the mobile app): full terminal control. These are the primary case.
2. **Opt-in adoption** of pre-existing tmux sessions: `ldmux adopt <tmux-session>`
   registers the session and opens a pane stream. Adoption is explicit, never
   automatic.
3. **Bare, non-tmux `pi` processes** (started outside ldmux): **not
   terminal-attachable in v1.** They are only reachable via the §4.5 RPC driver
   (future). This is a stated limitation, not a bug.

The mobile app always shows ldmux sessions; adopted sessions appear after opt-in.
The extension registers the current pi session on `session_start` if it is running
under ldmux (`$LDMUX=1`).

---

## 5. Daemon design (`ldmuxd`)

### 5.1 Responsibilities

1. Own/monitor a private tmux server (`-L ldmux` — a bare socket *name*, so
   tmux places it in its own directory, `${TMUX_TMPDIR:-/tmp}/tmux-<uid>/`;
   ldmux never sets `TMUX_TMPDIR`).
2. Maintain a compact **session registry** reconciled against `tmux list-*`.
3. **Stream** pane output (see ADR-0001) and provide **snapshots**.
4. Accept **input** (`send-keys`).
5. Serve the **API** (Unix socket locally; TLS/WSS on the tailnet).
6. Spawn/kill sessions in arbitrary cwds (FR-10).
7. Optionally launch the desktop mirror (FR-11).
8. Emit **events** for reactive UI (in-protocol; see §5.4).
9. Clean up; never leak processes; never `kill-server` on exit.

### 5.2 tmux interaction model

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
set -g window-size manual
set -g set-clipboard on
set -g focus-events on
set -g remain-on-exit on
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
  lines** (`scrollback` message → snapshot with `history: true`).
- **Fallback:** if the control client dies/stalls (`%exit`, EOF), it is
  **reconnected with bounded backoff** (3 attempts); if control mode still cannot
  be established the stream switches to **`capture-pane` row-diff polling**
  (~250 ms, in-place row rewrites) and retries control mode every 30 s. A
  `%pause` (queued output discarded because we fell behind) is a resync point;
  the pane is resumed with `refresh-client -A <pane>:continue`.

A `PaneStream` interface abstracts this; `pipe-pane` (raw bytes) and a
server-side VT emulator (perfect snapshots) slot in behind it later.

Rationale: `-C` over pipes gives control-mode latency and flow control **without
the native PTY dependency** the v0.1/v0.2 drafts assumed (dooboxes labelled
control mode "PTY-based"; that is a convention, not a requirement for `-C`).
See [`TRANSPORT.md`](TRANSPORT.md) for the experiments and the parser spec.

**Parser requirements (do not repeat dooboxes' bugs):** byte→line framing on
`\n` (never `readline`; U+2028/9 must not split); octal → **bytes** then a
**per-pane incremental UTF-8 decoder** (never `byte as char` or per-chunk
`from_utf8`); correlate `%begin/%end/%error <seq>`; tolerate unknown `%…` lines;
stateful per-stream sanitizer (OSC 52 / DCS / device-status) on by default.

**Multi-pane scope (v1):** one window, one pane per session. The mobile context
menu can split/create windows via `send-keys`/tmux commands, but the client tracks
only the *active* pane; the pane poller follows `display-message -p '#{pane_id}'`.

### 5.3 Session registry

**tmux is authoritative.** v1 keeps a **compact JSON state file**
(`<stateDir>/registry.json`, where `<stateDir>` is `$LDMUX_STATE_DIR`, else
`${XDG_RUNTIME_DIR}/ldmux`, else `/tmp/ldmux`) for metadata tmux does not
hold (kind, title, pi-session id, created-by, last-seen). SQLite is **deferred**
until a proven need (it was over-engineered in v0.1).

Per-session record:

```json
{
  "id": "01a09c54-…", "tmuxName": "ldmux-01a09c54", "kind": "pi",
  "cwd": "/home/ldeen", "title": "ldmux investigation",
  "createdAt": 1789…, "updatedAt": 1789…, "createdBy": "mobile",
  "piSessionId": "01a09c54-…", "status": "running|dead",
  "exitCode": null, "exitedAt": null
}
```

Reconciliation on start and periodically: `tmux list-sessions` → upsert; drop
records for vanished sessions; adopt survivors. Dead-but-retained sessions keep
metadata + scrollback reference for replay.

### 5.4 API surface (v1)

> **Aspirational — not implemented.** The shipped surface is deliberately
> smaller: newline-delimited JSON RPC over a Unix socket
> (`packages/protocol/src/rpc.ts`: ping/list/new/send/capture/snapshot/kill/
> adopt/register/session_end/remote_info), one WebSocket per session at `WS /ws`
> (JSON control + binary `OUTPUT` frames — `docs/REMOTE.md`), and the static web
> client at `/`. No REST layer, no `/api/dirs`, no session creation from a
> client. Scrollback paging **is** implemented, as the `scrollback` WebSocket
> message (a snapshot carrying `history: true`, up to 5,000 lines) rather than
> the REST route below. The table records original intent, not the contract.

Local: Unix socket `$XDG_RUNTIME_DIR/ldmux/ldmuxd.sock`.
Remote: `wss://debian.tail496756.ts.net/…` + `https://…/api/…` (via
`tailscale serve`; see §10).

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/api/health` | liveness + tmux reachable + version + transport mode |
| GET | `/api/sessions` | list (alive first, newest first) |
| POST | `/api/sessions` | create `{kind, cwd, cmd?, title?, mirror?}` |
| GET | `/api/sessions/:id` | detail |
| POST | `/api/sessions/:id/input` | `{data}` or `{keys:[…]}` |
| GET | `/api/sessions/:id/snapshot` | visible screen + state fields |
| GET | `/api/sessions/:id/scrollback?from=&lines=` | paged history text |
| POST | `/api/sessions/:id/mirror` | `{workspace?, readOnly?}` |
| POST | `/api/sessions/:id/kill` | terminate |
| GET | `/api/dirs?path=` | directory browsing (read; constrained to configured roots) |
| WS | `/ws/:id` | terminal channel (see §6) |

Deferred (v0.1 over-specification): SSE `/api/events` (session lifecycle is
delivered on the WS + a lightweight poll), mirror focus/close endpoints (a shell
helper suffices), and owner/families.

### 5.5 Lifecycle and robustness

- **Reconcile on start:** tmux wins; registry is a cache.
- **Crash-only design:** restart rebuilds from tmux + registry JSON.
- **Idempotent startup:** `flock`; refuse to run twice.
- **Graceful shutdown:** stop pollers, flush registry, leave tmux alive.
- **No blocking calls:** every tmux CLI call is async (`Bun.spawn`, both pipes
  drained concurrently) so the daemon's single event loop is never stalled on a
  child process, and each call is bounded: SIGTERM at 15 s, SIGKILL at +2 s, and
  a hard deadline past both, so a wedged tmux cannot pin a request or the
  per-connection handler queue. A bounded call returns exit code `-1`, which is
  reported as *unknown* liveness, never as "no sessions".
- **`remain-on-exit`** + `pane_died` detection: record `exit_code`/`exited_at`,
  keep scrollback, mark `dead`, notify clients, remove from the *live* list.
- **Extension lifecycle (ADR-0003):** the pi extension branches on
  `event.reason`:

  | reason | action |
  |--------|--------|
  | `quit` | mark the pi session ended; never kill tmux; daemon stays. |
  | `reload` | re-register on the next `session_start`; release per-session subscriptions. |
  | `new` / `resume` / `fork` | release subscriptions; on the following `session_start` register the **new** pi session id (the pi→tmux mapping can change within one tmux session's life). |

  The extension **never** stops the daemon or kills tmux from `session_shutdown`.

---

## 6. Wire protocol

### 6.1 Transport and auth

- WebSocket over TLS (`wss://`) provided by **`tailscale serve`** with the daemon
  bound to **loopback** (ADR-0005). This removes bind-address churn (the tailnet IP
  can change) and gives MagicDNS + optional identity headers.
- **Gates:** (1) WireGuard/tailnet reachability, and (2) a bearer **token**
  stored plaintext at `<stateDir>/token` with mode `0600` and compared in
  constant time. The optional `tailscale whois` / `Tailscale-User-Login` allow
  list is **not implemented** (documented as a later hardening step): the token
  is the only authentication, so treat it as the credential it is.
- **Pairing:** `ldmux pair` prints the URL, local URL and deep link (no QR: `qrencode` is absent) (deep link
  `ldmux://pair?host=…&port=…&token=…`); manual entry fallback. If MagicDNS fails
  in the app, fall back to the `100.x` address shown at pairing.
- **Framing:** terminal data is **binary**; control/handshake is **JSON text**
  frames. `protocolVersion` is negotiated in the handshake; enforces max frame
  sizes: 16 MiB absolute frame ceiling, 16 KiB of `input` data, 32 keys per
  message, 64 KiB per inbound WebSocket frame, and a 1 MiB cap on the output
  buffered while a client is attaching (exceeding it forces a resync).

### 6.2 Message types (implemented)

This is the shipped set (the source of truth is
`packages/protocol/src/messages.ts`; behaviour is in `docs/REMOTE.md`).
Everything below is a JSON **text** message except the last line.

Client → server: `attach {sessionId}`, `input {data}`, `key {keys}`,
`follow {mode}`, `scrollback {sessionId, lines?}`, `list`, `new {kind?, cwd?, title?}`,
`kill {sessionId}`, `ping {id?}`.

Server → client: `caps {protocolVersion, sessionId, cols, rows, snapshotEpoch,
capabilities {controlMode, scrollback}}`, `snapshot {…state fields…, history?}`,
`sessions {sessions[]}` (the answer to `list`/`new`/`kill`), `pong {id?}`,
`error {code, message}`.

Binary: exactly one frame type is emitted — `OUTPUT` (raw pane bytes) — using the
one canonical 8-byte header (`FrameType` in `frames.ts` carries reserved members
for later revisions; no shipped code sends them, and the `flags` byte is 0).

Deliberately **cut** from the v0.1 draft: `HELLO`/`resumeToken`,
`SESSION_STATE`, a server-side `SCROLLBACK` frame, `CLOSE`, and the binary
`INPUT`/`RESIZE` frames (input is JSON; the client never resizes).

`snapshot`/`caps` state fields (fixes v0.1's fidelity gap — the client replays
bracketed-paste, application-cursor and opt-in mouse from them): `cols`, `rows`,
`cursorX`, `cursorY`, `alternateOn`, `paneInMode`, `bracketedPaste`, `mouse`,
`appCursor`, `historySize`, `snapshotEpoch`, sourced from
`tmux display-message -p '#{cursor_x} #{cursor_y} #{alternate_on} #{pane_in_mode} …'`.

### 6.3 Connection lifecycle (snapshot-then-stream)

**Reconnect = snapshot, then stream.** The live stream is tmux control-mode
`%output`/`%extended-output` (ADR-0001). The v0.1 seq-window replay is **cut**:
tmux persists, so replaying a bounded frame window adds cache/eviction/desync bugs
for marginal benefit.

1. Client → `attach {sessionId}` (the handshake is attach-triggered; there is no
   separate hello step).
2. Server → `caps`, then `snapshot` (declared geometry + state fields).
3. Client repaints from the snapshot, replays the terminal modes, and resumes the
   live `OUTPUT` stream. The server subscribed to the pane *before* capturing the
   snapshot and queues the overlap, so no output is lost in the gap.
4. Client → `input`/`key`.
5. Heartbeats: the client sends an application-level `PING` every **20 s** and
   the server answers `PONG` (implemented). The server does **not** track missed
   beats or drop idle clients; a dead socket is detected by the write failing or
   the client's own reconnect loop. The app must be a **foreground service** so
   the heartbeat is not suspended (Android).

Known fidelity gap: `capture-pane` + state fields does not restore every VT mode
(`bracketedPaste` is empty before tmux 3.8, so it reads as false); the optional
server-side VT emulator seam would close it.

### 6.4 tmux attachment discipline (desktop mirror)

- The desktop mirror uses a **real `tmux attach`** (not the daemon's poller).
- **Canonical geometry is pinned** (ADR-0002), so the mirror and phone share the
  window without reflow fights.
- The mirror defaults to **read-only** (`tmux attach -r`) so a desktop "watch"
  pane does not fight the phone for input; "take control" is explicit.
- **Split input** is therefore avoided by default.

---

## 7. Mobile UX specification (the part gmux got wrong)

### 7.1 Orientation & layout

- **Landscape-locked** (FR-6). Caveat: Android large-screen APIs may ignore
  `screenOrientation` ≥600 dp (foldables); provide a graceful portrait
  "please rotate" state and still function if the system forces portrait.
- **Control quarter** (FR-7): `controlsSide = left | right`, width 20–35%.
  - Session pager (live activity dot), extra-keys row(s), actions
    (`＋ shell`, `＋ pi`, `↓ jump-to-bottom`, `⌘ keyboard`, `⧉ mirror`, `⧉ copy-mode`, `⋮ settings`).
- One-tap collapse hides controls (terminal full width) behind a slim edge handle.

### 7.2 Session switching (swipe)

- Session switching is a **drawer** action; the only swipe shortcut is a narrow
  left/right **edge strip** (20 px), because a horizontal drag inside the
  terminal belongs to the running program (dooboxes `MOBILE_TERMINAL_UX` §5).
  The pager lists sessions; tap to jump. Switching is instant.

### 7.3 Input & keys

- Extra-keys bar always visible (FR-15); **sticky modifiers** (Ctrl/Alt latch for
  the next key). tmux prefix is a first-class button with a context menu
  (split/window/rename/detach). **Copy-mode toggle** is required for scrolling
  full-screen apps (`vim`, `htop`) whose history is not in the xterm.js buffer.
- Gestures: two-finger drag → scroll; pinch → font size (persisted); long-press →
  select/copy; double-tap empty area → `Tab` (optional).
- IME-safe layout via `WindowInsets`; the control quarter does not jump when the
  soft keyboard opens.
- WebView hardening (from mttyd/dooboxes): xterm.js hidden textarea gets
  `inputmode=url` + a `MutationObserver` guard to defeat Gboard autocorrect;
  touch-drag-as-wheel when the TUI enables mouse tracking; WebGL2 → canvas/DOM
  fallback if unavailable.

### 7.4 Scroll model — the crown jewel

gmux's bug (PR #500): stale `wasAtBottom`/pixel snapshots racing user wheel input
across BSU→ESU→rAF windows (25 wheel-up notches → net −41 lines). Fix: an explicit
mode state machine.

ldmux adopts the same explicit model, **owned entirely by the client** (a
server-facing integer line id has no stable meaning across snapshots/reflow/ED3):

- Modes: **`following`** (pinned to bottom) and **`anchored`** (user is reading;
  anchor = absolute line identity + distance-from-bottom).
- **Only explicit user intent** changes the mode. **Output never moves the
  viewport**; it only re-resolves the mode.
- Anchor by **line identity**, not pixels. The server never resolves line ids.
- Handle BSU/ESU (DEC 2026), ED3 (scrollback clear), alternate-screen enter/exit,
  and resize reflow from the parser, not byte scanning. On resize, preserve the
  anchor; reflow per xterm.js semantics.
- On reconnect: take the `SNAPSHOT` as a fresh baseline (`snapshotEpoch`); re-derive
  the anchor from the snapshot header; **if the anchored line is gone, stand down
  to `following` — never guess.**
- Persistent **"↓ jump to bottom"** affordance while anchored (dooboxes requirement).
- Local scrollback cap: **1,000 lines on mobile** (dooboxes mobile perf research; `packages/web/public/app.js`)
  with a configurable max; server-side tmux `history-limit` is **10,000** lines
  (~40 MB/pane) and the `scrollback` message serves older pages on demand.

### 7.5 Session list semantics (FR-9)

- Live list = sessions whose tmux session exists and status ≠ `dead`.
- A separate, clearly-marked "ended (recent)" section retains scrollback.
- On the `dead` event, the session leaves the live pager with a brief toast +
  "view last output."

---

## 8. Opening sessions from the phone (FR-10)

`POST /api/sessions {kind, cwd, cmd?, title?, mirror?}`:

- `shell` → `tmux -L ldmux new-session -d -c <cwd> -s ldmux-<ulid>`.
- `pi` → `tmux -L ldmux new-session -d -c <cwd> -s ldmux-<ulid> pi [--name …] [prompt]`.
- `cmd` → run that command instead (still a shell session).
- `cwd` validated; `GET /api/dirs` powers the picker (recent dirs from registry).

---

## 9. Desktop mirror (FR-11)

`POST /api/sessions/:id/mirror {workspace?, readOnly?}`:

1. Resolve workspace: explicit, else config, else "last". On this machine i3 uses
   `1..10` and `Mod+0 → 10`, so the default is **`10`**; the setting accepts a
   number or `last`.
2. Prefer a `for_window` rule keyed by the mirror window class
   (`ldmux-mirror`) over `sleep`+`move` to avoid races; otherwise
   `i3-msg 'workspace <ws>'` + move.
3. `alacritty --class ldmux-mirror --title "ldmux: <title>" -e \
      tmux -L ldmux attach-session [-r] -t <tmuxName>`
4. Record the container id via `i3-msg -t get_tree` + `jq` for later focus/close
   (a shell helper; no dedicated API in v1).

Environment: a `systemd --user` daemon lacks `DISPLAY`/`XAUTHORITY`; the i3 config
already runs `systemctl --user import-environment DISPLAY XAUTHORITY
XDG_RUNTIME_DIR`, and the helper exports `DISPLAY=:0`/`XAUTHORITY` as fallback.

---

## 10. Tailscale & security (FR-4)

- **Transport:** system `tailscaled` v1.102.3 (already running). The daemon binds
  **loopback**; `tailscale serve --bg --https=443 http://127.0.0.1:<port>` exposes
  it on the tailnet with TLS + MagicDNS (ADR-0005). `tailscale cert` is the
  fallback. **No Funnel** unless explicitly enabled.
- **Gates:** tailnet identity (WireGuard; optional `whois`/identity-header allow
  list) + bearer token. Constant-time comparison.
- **Refuse** to bind a public interface.
- Mobile: Android cleartext scoped to `localhost` + `*.ts.net`; token in
  EncryptedSharedPreferences (Keystore); iOS in Keychain (iOS is a **target**, not
  v1 — see §11.2).
- `/api/dirs` is read-only and constrained to configured roots; the filesystem
  enumeration surface is documented and acceptable for single-user.

---

## 11. Technology stack

### 11.1 Daemon language

**Recommendation: TypeScript on Bun** for v1. Bun ships native `bun:sqlite`,
WebSocket, and Unix sockets, and `bun build --target bun` yields a single binary.
**Transport note (v0.3):** the Bun recommendation is *reinforced* by ADR-0001 —
control mode `-C` runs over pipes and needs **no** native PTY dependency, so the
single-binary/packaging argument holds. (If a PTY were required, e.g. for `-CC`
or `pipe-pane` helpers, this rationale would be revisited.)

Alternatives: Node.js + `ws` + `better-sqlite3` (ubiquitous, but native-module
fragility); Rust + tokio (best throughput, slower iteration); Rust core + napi-rs
(hot-path optimization, more build complexity). A clean module boundary keeps the
`PaneStream` hot path portable to Rust later.

### 11.2 Mobile stack

No mature pure-Compose terminal renderer exists for iOS/Android in 2026. Options:
KMP + Compose + WebView/xterm.js (**recommended v1**); native KMP via Rust
`alacritty_terminal` JNI/UniFFI (phase 3+, behind the `TerminalSurface` seam);
Flutter + `flutter_rust_bridge` (viable fallback); PWA (instant fallback);
libghostty (**not viable** — no mobile target, unstable ABI).

**Recommendation (target, not built): KMP + Compose Multiplatform**, landscape-only,
WebView/xterm.js `TerminalSurface` for v1. **What shipped instead** is the
buildless PWA in `packages/web/public` (`docs/WEB.md`), because this machine has
no gradle/kotlin toolchain; it is the renderer the WebView shell will host. Libraries: **Ktor** (`ktor-client-websockets`; OkHttp on
Android, Darwin on iOS), `kotlinx.serialization`, `ktor-reconnecting-websocket`
(backoff + `StateFlow`), `compose-webview-multiplatform`, foreground service +
`ConnectivityManager`.

**Android implementation requirements (v0.1 gap):**
- Foreground service with a declared `android:foregroundServiceType` and a
  notification channel; runtime **`POST_NOTIFICATIONS`** (Android 14+).
- Partial wake lock during active sessions; network-change-triggered immediate
  reconnect (`ConnectivityManager.NetworkCallback`).
- OEM battery-exemption guidance (Samsung/Xiaomi/OnePlus) in onboarding.
- Orientation lock + portrait fallback (large-screen caveat).
- **iOS is a target, not v1.** v1 ships Android. iOS constraints (no background
  service model, ~3-min WS background, `WKWebView` input) are documented but
  unimplemented.

### 11.3 Rendering engine (client)

- **v1:** xterm.js in a WebView (no fit addon: the canonical grid is
  CSS-scaled, never reflowed — ADR-0002), optional
  `@xterm/addon-webgl` (canvas/DOM fallback). Scroll-anchor implemented as a small
  client addon around the xterm.js viewport.
- **Future:** native renderer behind the same `TerminalSurface` seam.

---

## 12. Project layout

```
ldmux/
├─ README.md · AGENTS.md · Justfile · flake.nix · package.json · tsconfig.json
├─ docs/
│  ├─ SPEC.md · INVESTIGATION.md · TRANSPORT.md · REMOTE.md · WEB.md ·
│  │  NATIVE-TERMINAL.md · PREDICTIVE-ECHO.md
│  └─ adr/            # 0001 transport, 0002 geometry, 0003 lifecycle,
│                     # 0004 scroll, 0005 remote/tls, 0006 divergences,
│                     # 0007 native terminal engine, 0008 mobile UI modes,
│                     # 0009 Node runtime / extension removed
├─ packages/
│  ├─ protocol/       # frames, messages, rpc, socket client, paths
│  ├─ daemon/         # ldmuxd + cli + probe + tmux/, stream.ts, remote.ts
│  ├─ testing/        # the test/expect vocabulary over node:test
│  ├─ web/public/     # buildless web client (core.js + app.js + vendor/)
│  └─ mobile/         # native KMP + Compose Multiplatform client (Android
│                     # first), with the libghostty-vt core behind JNI
└─ (the web client is served by the daemon; the mobile app is native)
```

Binaries: `ldmux` (CLI), `ldmuxd` (daemon).

---

## 13. Phased plan

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
  `kill`, `forget`, `adopt`, `mirror`, `remote`, `pair`, `expose`,
  `install-extension` (all implemented).
- **Geometry probe (ADR-0002):** confirm `window-size manual` behavior and the
  canonical-size default.

**Phase 2 — pi extension.**
- Reason-branched supervision/registration (ADR-0003), `/ldmux`, `ldmux_*` tools.
- Bash `pi()` wrapper → `ldmux -- pi`; env contract; wrapper test matrix.
- E2E: pi sessions appear/update/disappear correctly across `/new`,`/resume`,`quit`.

**Phase 3 — remote + auth.**
- `tailscale serve` + token + pairing (URL/deep link; no QR generator on this
  machine); loopback bind. **DONE.**
- Reconnect (snapshot-then-stream), heartbeats, `resumeToken`.

**Phase 4 — mobile client (Android v1).**
- **Shipped:** the PWA — canonical-grid scaling, extra-keys bar, session drawer +
  edge-strip paging, client-owned scroll state machine + `follow`, `scrollback`
  paging, unread markers, offline app shell.
- **Remaining (target):** KMP scaffold, Ktor WS, foreground service,
  `TerminalSurface` WebView host, copy-mode.
- New-session dialog + directory browser. E2E over Tailscale from the phone.

**Phase 5 — desktop mirror + polish.**
- i3 + alacritty mirror (read-only default), focus/close helper.
- Settings, theming, `install.sh`, `systemd --user` unit, `ldmux doctor`.

---

## 14. Risks and open questions

| ID | Risk / question | Mitigation |
|----|-----------------|------------|
| R1 | Scroll fidelity is hard. | Explicit `following`/`anchored` client state machine from day one; test 30 Hz BSU/ESU + wheel/touch. |
| R2 | Snapshot fidelity (cursor/modes/alt-screen/soft-wrap). | v1: `capture-pane -e` + state fields; document limits; phase-3 optional server-side VT emulator. |
| R3 | tmux version differences (3.5a here). | Conformance tests; feature-detect; probe before freezing ADRs. |
| R4 | Android background kill severs the WS. | Foreground service, wake lock, backoff + jitter, tmux persistence, resync on resume. |
| R5 | WebView WebGL2 availability. | Fallback to canvas/DOM; test on the real device. |
| R6 | tmux socket under `/tmp` / `TMUX_TMPDIR` nesting. | A bare `-L ldmux` name (no `TMUX_TMPDIR` override); unique per-run socket names in tests, and `killTmuxServer` also unlinks the socket file, which `kill-server` leaves behind when the server already exited. |
| R7 | Split input with a desktop mirror. | Mirror read-only by default; explicit "take control". |
| R8 | i3 placement races. | `for_window` rule keyed by `ldmux-mirror`. |
| R9 | Token/identity security. | Loopback bind, `tailscale serve` TLS, token `0600` (plaintext at rest; the state dir is `0700`). No identity allow list yet. |
| R10 | Language choice. | TS/Bun v1 with a `PaneStream` seam to Rust. |
| R11 | pi RPC UI sprawl. | Out of v1; `SessionDriver` seam. |
| R12 | Control-mode parser robustness (octal, split UTF-8, sequence correlation). | Robust parser + tests; capture-pane fallback; tmux owns state. |
| R13 | iOS scope creep. | v1 = Android; iOS is a documented target. |
| Q1 | Wrapper attaches interactively? | Recommend yes (gmux parity). |
| Q2 | ~~serve vs direct bind~~ | **Resolved (ADR-0005): `tailscale serve`.** |
| Q3 | Native renderer timing? | Defer behind `TerminalSurface`. |
| Q4 | Session families? | Phase 2+. |

---

## 15. Acceptance criteria (v1)

- A pi session on the desktop appears in `ldmux ls` and on the phone; assert by
  polling `/api/sessions` after launch (**bounded: 2 s, tested**).
- From the phone: create a shell and a pi session in arbitrary directories; both
  appear and are interactive.
- Streaming output does not move the viewport while the user is scrolled up; a
  "jump to bottom" affordance works.
- Sessions that end on the host disappear from the live pager (retained under
  "ended (recent)").
- The desktop mirror opens a live `alacritty` on the configured workspace and is
  read-only by default.
- Reconnect after an airplane-mode toggle resumes with correct screen and anchor.
- `/new`, `/resume`, `/quit` do **not** cause false session-ended events.
- No leaked tmux servers/processes after `ldmuxd` restart; tmux sessions survive.
- Reached only over Tailscale; token required; nothing bound publicly.

---

## 16. Appendix — key prior art

See [`INVESTIGATION.md`](INVESTIGATION.md) for citations. Summary: gmux (runner/
daemon/web; scroll-anchor PR #500; two-gate Tailscale auth); dooboxes (tmux
control mode over PTY; multi-client attachment model; mobile UX/perf research;
terminal codec; portability lessons); trustmux/ttyd/wetty/gotty/mttyd (tmux+
Tailscale+web; mobile input fixes); Mosh/Blink/Termius/Termux (mobile interaction
patterns); pi-mobile/pi-remote/paseo (WebSocket bridges over `pi --mode rpc`);
tmuxy-core/cdx/tmux-control-mode-js (control-mode parsers); SocketPipe (framing).

---

## 17. Architecture Decision Records

| ADR | Decision |
|-----|----------|
| **ADR-0001** | **Transport:** tmux control mode `-C` over pipes (no PTY) + `capture-pane` snapshots/fallback, behind a `PaneStream` interface. Proven on tmux 3.5a + Bun 1.3.9. |
| **ADR-0002** | **Canonical geometry:** pin `window-size manual` + a canonical grid; clients render viewports and never resize the pane. |
| **ADR-0003** | **Extension lifecycle:** branch on `session_shutdown.reason`; only `quit` ends a session; never tear down the daemon/tmux on shutdown. |
| **ADR-0004** | **Scroll anchor:** client-owned explicit `following`/`anchored` state machine; server never resolves line ids. |
| **ADR-0005** | **TLS/remote:** loopback bind + `tailscale serve --bg`; token + QR pairing; no Funnel. |

Individual ADRs live under `docs/adr/`. ADR-0001 is now **frozen** (proven on this
machine); ADR-0002's geometry probe is **done** (asserted in the daemon tests:
a pinned 120×40 grid under `window-size manual`). ADR-0006 records the deliberate
divergences from dooboxes/gmux.