# ldmux Rust Port Plan — tmux removed, session layer becomes ours

> **Historical plan (2026-10-05 banner).** This is the plan that removed tmux. tmux
> is now removed from the software entirely, not only the runtime: the wire field
> `tmuxName` is `keeperName` (and `tmuxSocket` is `keeperSocket`), `paneInMode` and
> the `copymode` message are gone, `LDMUX_TMUX_NAME`/`LDMUX_TMUX_SOCK` are no longer
> set, and §3.11's parity gate runs against committed goldens with no tmux at test
> time (see §3.11 below and the commit that removed the semantics). Every other
> tmux mention here describes the design being replaced.

**Created:** 2026-09-27
**Revised:** 2026-09-28 — tmux removal added after full investigation (this is the
operative version; the original "keep tmux" plan is superseded); 2026-09-28 audit —
status/consistency findings F1–F9 fixed in this document, record in §3.14
**Status:** plan — D1 resolved 2026-09-27 (one unit, auto-spawned detached keeper,
ssh-agent pattern; §3.2); D2/D3/D4 open (§3.12); defect work D-1/D-2/D-3 root-caused,
**unimplemented** (§3.13); audit 2026-09-28 (§3.14)

---

## 0. The two changes, in one breath

1. **All TypeScript becomes Rust** (daemon, CLI, protocol, tests, build system).
2. **tmux goes away.** The daemon process family now owns the PTYs: a small
   **keeper** process allocates PTYs, supervises the shells, and runs one
   **libghostty-vt** terminal emulator per session (the same engine the mobile
   client already renders with). The desktop live window stops being a tmux client
   and becomes a small **attach client** binary. The wire protocol, the mobile app,
   and every client-visible behaviour stay byte-identical.

tmux is not "replaced by a library". There is **no existing Rust tmux** — the
research found none (Zellij is the only Rust multiplexer and it is a binary, not a
reusable session layer). What exists and is reused:

| Layer | Reused component | Why it is the right one |
|---|---|---|
| PTY allocation + winsize + child supervision | `nix::pty` (`openpty`/`posix_openpt`/`grantpt`/`unlockpt`/`forkpty`, `Winsize` = direct TIOCSWINSZ) | The raw POSIX primitive with direct ioctl access; Zellij uses `nix` for exactly this; `portable-pty` (WezTerm's) is the cross-platform alternative and adds no value on Linux-only (its Unix backend is `libc::openpty` — the same call). The `pty` crate is abandoned (2017). |
| Terminal emulation (the hard part) | **libghostty-vt** — the Ghostty VT core | It is **already the client's renderer**. The daemon running the same engine means snapshot ↔ client render share one cell model (wide chars, graphemes, SGR, wrap state). That parity is structural, not coincidental. It is MIT. There is **no official crate**: the distribution is source-tree / vendored-`.a` (the crates.io `libghostty-vt`/`libghostty-vt-sys` are a community wrapper over the same model, and require Zig 0.16.x). **The project builds the static library locally** (`packages/mobile/native/prebuilt/host/lib/libghostty-vt.a`, pinned Ghostty commit `d5eba8d`, Zig 0.16.0) — the artefact and the headers are **not committed** (D2/§5). |
| Everything else | `tokio`, `tokio-tungstenite`, `serde`, `clap`, `thiserror`, `ring`, `uuid`, `nix`, `rand`, `fs2` | As in the original plan. |

The one thing no library gives us is **restart survival** — and it is not a library
problem, it is a process-topology problem (§3.2).

---

## 1. What stays, what goes

### 1.1 What stays unchanged

| Component | Why |
|---|---|
| The wire protocol (frames, messages, RPC) | The mobile app speaks it. The Rust stack emits the same bytes. Field names that said "tmux" (`keeperName`, `keeperSocket`, `tmuxBin`) stay as wire names for compatibility; their meaning becomes "session name", "daemon socket", "(n/a)". |
| The mobile app (Kotlin + Compose + libghostty-vt + JNI + C) | Zero changes. The phone cannot tell the difference — the bytes are the same bytes. |
| The `following`/`anchored` scroll-fidelity model (SPEC §7.4) | It lives in the client's emulator fed by the same byte stream. Unchanged. |
| The ldmux sanitizer | It is ours, not tmux's; it stays between the PTY stream and the clients. |
| The pairing link, the registry JSON, the token auth | Unchanged. |
| i3/sway/EWMH desktop probing | Unchanged (the daemon still spawns `i3-msg`/`xprop`). |
| The systemd linger/`KillMode=process` machinery | Kept — and now doing its original job for our own keeper instead of tmux (§3.2). |

### 1.2 What goes

| Current | Replaced by |
|---|---|
| tmux server (PTYs, sessions, shells) | **The keeper**: `nix::pty` + child supervision + one ghostty-vt per session |
| `tmux -C` control mode (`%output`, `%pause`, `%continue`, octal escaping) | **Gone.** No transport protocol to parse. The keeper socket carries framed bytes. |
| `pipe-pane` FIFO transport | **Gone** (never shipped as the winner; moot now) |
| The 160-line control-mode parser + 134 lines of tests | Deleted |
| 20-line tmux option profile (SPEC §5.2) + its file-serialisation mechanism | Shrinks to: spawn environment (7 vars) + login-shell argv. No profile file, no `source-file`, no `-f`. |
| `window-size manual`/`latest` dance (incl. the 3.5a crash workaround) | Direct `TIOCSWINSZ` on the PTY master; ownership rule unchanged (ADR-0010). |
| `send-keys`/`send-keys -l`/`send-keys -H` | Direct writes to the PTY master fd. |
| `capture-pane -p -e -N [-J] [-S -N -E -]` | The ghostty-vt snapshot renderer, format-parity tested byte-for-byte (§3.11). |
| `display-message` terminal-state format (12 fields) | Direct ghostty-vt API + `/proc/<pid>` (§3.4). |
| `list-clients`/`detach-client`/`@ldmux_*` session options | First-class attach state in the daemon (the desktop window is a protocol client). |
| Desktop shim **shell script** (`liveShimScript`, ~50 lines of sh) | `ldmuxd attach` — a small Rust client process. |
| `list-sessions`/`list-panes`/`has-session` reconciliation | The keeper is the authority (it owns the processes); the registry reconciles against keeper reports. |
| `LDMUX_TMUX_BIN`, `resolveTmuxBin`, `resolveLoginShell`'s tmux coupling, `deployed-tmux` test, tmux in the flake runtime | Deleted (flake keeps tmux only in the transitional A/B window, §9). |
| Node 26, `ws`, `package.json`, `tsconfig` | Rust toolchain, Cargo workspace, as in the original plan. |

---

## 2. The complete tmux ledger — every feature ldmux actually uses

Enumerated from the code (this is the map the removal had to account for). Each row
is a tmux capability ldmux depends on and its replacement.

### 2.1 PTY / session lifecycle — the reason tmux is here

| tmux capability | Used for (file:line) | Replacement |
|---|---|---|
| PTY allocation, one per session | `new-session -d -s -c` (control-plane.ts:688) | Keeper: `openpty` + `posix_openpt`/`grantpt`/`unlockpt` + `forkpty`-equivalent spawn (fork, `setsid`, `chdir(cwd)`, `exec` login shell). |
| **Sessions outlive the daemon** (the load-bearing property) | measured repeatedly; `KillMode=process` + unit comment (ldmux.service §1) | **Keeper process** = the tmux-server analogue (§3.2). Without it, restart kills shells — a measured regression. |
| Shell exit detection | `remain-on-exit off` + `has-session`/`list-sessions` reconciliation (daemon.ts:481,833,1417,1502) | `waitpid`/`waitid` is the authoritative death signal; EIO/EOF on the master is the secondary I/O signal (§3.3). No polling. |
| Session naming, `has-session`, `list-sessions` (name, windows, created, attached, activity) | `listSessions()` (control-plane.ts:583) | Keeper is the registry of its own children: name, pid, created, attached-count (from live attach state). |
| `new-session` env (`-e K=V`) | RPC `new` with env | `execve` env, direct. |
| `kill-session` | `killSession` (control-plane.ts:755) | Kill the child (SIGTERM → grace → SIGKILL, group kill as today's proc.ts does) + close master. |
| `start-server` (implicit) | `ensureServer` | Keeper process start — under systemd's control, deterministic. |

### 2.2 Input

| tmux capability | Used for | Replacement |
|---|---|---|
| `send-keys -l -- <text>` (literal text) | RPC `send.text` (daemon.ts:1322) | Bytes to PTY master. (Today this is a tmux subprocess per call — measured ~3 ms each; now it is a `write(2)`.) |
| `send-keys -- <Key>...` (named keys: C-u, Enter, arrows) | RPC `send.keys` (daemon.ts:1323) | The daemon already knows the bytes for named keys (it builds them for the mouse path today); it writes them. Key-name → bytes table lives in the protocol crate (it already effectively exists in tests). |
| `send-keys -H <hex…>` (raw bytes) | `sendBytes`, the SGR mouse-click synthesis (control-plane.ts:817, 837) | Bytes to PTY master. Mouse synthesis unchanged (same press/release sequences) — it just no longer needs tmux. |
| Prefix-key limitation (prefix cannot be sent) | documented in control-plane.ts | No change in semantics: the prefix was a tmux-client concept; with no client layer, every byte goes to the program. |

### 2.3 Output / screen state

| tmux capability | Used for | Replacement |
|---|---|---|
| Control-mode `%output` (octal-escaped text protocol) | The live stream (control-client.ts) | Keeper socket, framed binary (§3.6). **The 160-line parser and its 134 lines of tests die here.** |
| `pause-after` / `%pause` / `%continue` / `refresh-client -f` | Flow control (control-client.ts:295) | **Improved, not replicated:** the keeper always drains the PTY, so the shell is never paused; slow clients get bounded queues + forced resync (§3.7). |
| `pipe-pane` FIFO | Measured A/B candidate (TMUX-DEPENDENCY §6.1) | Moot. One transport now. |
| `capture-pane -p -e` (SGR screen text) | Snapshots (stream.ts:188: `escapes:true, pad:true`) | ghostty-vt render → same format, byte-parity tested (§3.11). |
| `capture-pane -N` (pad rows to full width) | Snapshot rows must be full-width so the client's blank tail is correct (micro measurement, control-plane.ts:300-320) | Renderer emits full-width rows (cell model knows every cell to the right edge). |
| `capture-pane -J` (join wrapped rows) | Text-mode `ldmux capture` (daemon.ts:1329) | Renderer joins rows whose wrap state is set (ghostty tracks `is_cursor_pending_wrap` / wrap state per row). |
| `capture-pane -S -N -E -` (bounded history) | `snapshotWithHistory` (stream.ts:194), `ldmux capture --history` (HISTORY_CAPTURE_LINES=5000 cap) | ghostty-vt scrollback with `max_scrollback` = the history-limit analogue; paged requests by line count. |
| `display-message -p <format>` — 12 fields: cursor_x/y, alternate_on, pane_in_mode, history_size, pane_width/height, pane_dead, mouse_any_flag, keypad_cursor_flag, bracket_paste_flag, mouse_sgr_flag, pane_current_command | `snapshotState` (control-plane.ts:947-1010) — feeds the snapshot message the client repaints from | ghostty-vt directly: `cursor_x()/cursor_y()`, `active_screen()`, `mode()` (DECBKM/DECTCEM/mouse), `scrollback_rows()`, `cols()/rows()`, child state from `/proc/<pid>` (§3.4). **Strictly more reliable than parsing a format string.** |
| Screen text = tmux's emulator grid | The whole snapshot story: the client replays tmux's view | The keeper's ghostty-vt instance is the new source of truth. Same role, same position in the pipeline. |

### 2.4 Geometry / resize

| tmux capability | Used for | Replacement |
|---|---|---|
| `resize-window -x -y` | RPC `resize` (daemon.ts, via stream.ts) | `TIOCSWINSZ` on the master fd + `Terminal::resize(cols, rows, 0, 0)`. |
| `window-size manual`/`latest` (who decides size) | `setWindowSize` (control-plane.ts:871); desk-window size takeover (daemon.ts:548,610,1073) | Ownership rule unchanged (ADR-0010): size belongs to whoever owns the shell. Implementation: the client-driven `Resize` RPC, and the desktop attach client reporting its real window size on attach/resize (it reads its own pty winsize). The `manual`/`latest` *option* dies; the rule survives. |
| Size clamping (MIN/MAX_PANE_*) | clampSize (control-plane.ts:213-232, used at :875) | Same clamp in the protocol crate (it already is shared). |
| `new-session -x -y` (initial size) | `newSession` with cols/rows | Initial `TIOCSWINSZ` at spawn. |

### 2.5 Client tracking / the desktop live window

| tmux capability | Used for | Replacement |
|---|---|---|
| `attach` (a terminal becomes a client) | Desktop live window (`liveArgv`/shim), desk windows (`runAsDeskWindow`) | **`ldmuxd attach`**: a real client of the daemon protocol, inside the window's terminal. First-class, not inferred. |
| `list-clients -F #{client_tty}` | `listClientTtys` (control-plane.ts:700) — is the live window still attached? | The daemon knows its own connections. No inference. |
| `detach-client -t <tty>` | The handover: phone takes shell → window stops typing (daemon.ts:596) | Daemon closes the attach client's write side / sends `Detached`; the client shows "press a key to reclaim". No tty bookkeeping. |
| `@ldmux_live_tty` / `@ldmux_live_pid` / `@ldmux_held_elsewhere` session options | The window records its own tty+pid; held-elsewhere flag (desktop.ts, daemon.ts:586-612,912-1030) | All three become daemon-side state about an attach connection. The `set-option -t @…`/`show-option`/`unset` RPCs die. |
| `has-session` on the shim's wait loop | Shim decides "ended" vs "taken" (liveShimScript) | Same decision, made by the daemon over the protocol. |
| `destroy-unattached` | Desk-window sessions die when the window closes (control-plane.ts:750) | Session record flag `destroyOnLastAttach`; daemon destroys on last-attach-close. Same rule, no option. |
| Window title from pane (`set-titles`, `set-titles-string #T`) | Desk window title = program title (profile) | Alacritty takes OSC 0/2 titles from the program natively — it was the missing tmux pass-through before. Strictly better. |

### 2.6 Environment / shell selection

| tmux capability | Used for | Replacement |
|---|---|---|
| `default-shell` (login shell from /etc/passwd, not `$SHELL`) | `resolveLoginShell` (control-plane.ts:80-130) | Keeper picks the login shell the same way (same /etc/passwd logic) and execs it as a **login shell** (`argv[0] = "-bash"`/`"-zsh"`). |
| `set-environment -gu BASH_ENV/ENV/PS1/PROMPT_COMMAND/ZDOTDIR` | Profile: strip inherited prompt state | Same strip, in the keeper's `execve` env. |
| `set-environment -g COLORTERM truecolor` / `-g SHELL` | Profile | Same vars in the spawn env. |
| `-e` env injection at creation | `new-session -e` (RPC `new.env`) | `execve` env. |
| `TERM` (`tmux-256color` + `terminal-overrides ,…:Tc`) | Profile: lie about capabilities so programs don't downgrade | Spawn env `TERM=xterm-256color` + `COLORTERM=truecolor` (decision, §3.12-D). |
| `escape-time 0` | Profile: no 500 ms ESC wait | Irrelevant — we are not a client that hedges on ESC; bytes go straight to the program. |
| `TMUX`/`TMUX_PANE` stripping (`childEnv`) | control-plane.ts:250-260 | Nothing to strip: no tmux variables exist. |

### 2.7 Desktop UX (mouse, wheel, copy)

| tmux capability | Used for | Replacement |
|---|---|---|
| `mouse on` + wheel→`scroll-up -X -N 15` bindings (4 bind-key lines) | Desk-window wheel scrolls tmux history | Alacritty's native scrollback for the bytes it received (shell sessions: correct scrollback, better than tmux's); TUI sessions: keybinding in `ldmux-attach` → history-page request → re-render (§3.9). Flagged parity item. |
| `set-clipboard on` + `allow-passthrough on` | Copy-mode selection → OSC 52 → desktop clipboard | Alacritty's native selection + clipboard (it is a real terminal; the passthrough question no longer exists). |
| `focus-events on` | Window focus → pane focus bytes | Alacritty native. |
| `status off`, `aggressive-resize off`, `mode-keys` bindings | Profile cosmetics | Gone. |
| `extended-keys` / `extended-keys-format csi-u` | Modified keys reach programs (pi's warning) | Gone as an option: there is no middle terminal to encode with; the bytes the window terminal produces go straight to the program. (Alacritty's own key encoding is the outer terminal's business.) |

### 2.8 The hidden dependency — the terminal emulator

This is the row the ledger exists to expose: **tmux is a terminal emulator, and
ldmux has been depending on one without it being in the dependency list.**

- The pane bytes are parsed by tmux into a grid: cells with SGR attributes, wrap
  state, alt-screen, cursor, modes. `capture-pane` reads **that grid**, not the raw
  bytes.
- The scrollback (`history-limit 10000`) is tmux's grid history, not a byte log.
- The client runs a **second** emulator (libghostty-vt) on the same bytes. Today
  those are two different engines (tmux's C grid vs ghostty-vt) that must agree —
  and the snapshot-parity risk is exactly the seam between them.
- Replacement: one ghostty-vt instance per session in the keeper. **The daemon's
  emulator and the client's emulator are now the same engine and the same pinned
  commit** — the seam becomes a format question (§3.11), not an engine question.

---

## 3. The replacement architecture

### 3.1 Why a keeper process (and why nothing off-the-shelf gives it to us)

The surveyed prior art (TMUX-DEPENDENCY.md §9, plus the 2026-09-28 research pass)
is unanimous: **every system keeps the PTYs in a long-lived supervisor process and
reattaches over a socket; none passes fds (no SCM_RIGHTS anywhere); and none beats
tmux on "survives its own restart"** — VS Code restarts its pty host up to five
times and loses the sessions each time; WezTerm has no persistence; Zellij's
server is the process. The one measured, free, load-bearing property of the current
system — daemon restart does not touch the shells — can only be rebuilt by giving the
PTYs to a process that the restart does not kill.

Therefore:

- **`ldmux-keeper`** — allocates PTYs, owns the child pids, runs one ghostty-vt per
  session, serves a Unix socket. Small. Boring. Its crash kills the shells — exactly
  as a tmux server crash does today (parity, not regression).
- **`ldmuxd`** — the daemon: registry, ownership, remote WS server, CLI, desktop,
  live-window state. It is now an **ordinary client of the keeper**, over the same
  framing the rest of the system already uses.

### 3.2 Process topology and systemd — restart survival, the parity table

Today's unit is `ldmux.service` (`KillMode=process`, linger required, per the unit's
own header). The keeper design keeps that machinery and retargets it:

| Event | Today (tmux) | Keeper design |
|---|---|---|
| `systemctl --user restart ldmux` (daemon) | Shells survive (measured) | **Shells survive** — keeper unit untouched |
| tmux server crash / keeper crash | Shells die (rare; tmux is stable) | Shells die (parity; the keeper is smaller and Rust) |
| Machine reboot | Shells die (both today) | Shells die (parity) |
| Daemon process crash (OOM/panic) | Shells survive | Shells survive (keeper is a separate unit) |
| User logs out | Shells survive via linger (unit §2) | Same: linger + `KillMode=process`, now with nothing extra to protect |

**Can the state be a file instead of a second process? No — proven, not
asserted**, 2026-09-27 (experiments on this machine + prior art):

- The pty is a kernel object (an fd pair + line discipline + buffers). The
  master has **no `/dev` path**; the only ways a master fd reaches a process
  are fork inheritance or SCM_RIGHTS to a *live* process. Nothing reads it
  from a file.
- Measured: a "restarted" process that reopens `/dev/pts/N` gets a second
  **slave** fd with no data path to the master's read queue — select+read
  over 2–3 s returned 0 bytes against a live writer.
- Measured: closing the master takes the pty down with it — a shell-like
  session leader dies within microseconds of the parent closing its master
  fd, while the identical child runs fine with the master open. (This is
  exactly why a daemon restart kills shells today, and why the keeper
  exists.)
- Prior art is unanimous: **no PTY supervisor anywhere** (tmux, screen,
  dtach, abduco, Zellij, WezTerm mux, VS Code pty host, containerd shim)
  persists PTY state to the filesystem. The persistence ecosystem
  (tmux-resurrect, WezTerm resurrect) saves layout metadata and *re-runs
  commands* — new PTYs, new processes; not recovery.

So the PTY state lives in a process that outlives the daemon. But the
process does not have to be a second *unit*, and it is made maximally thin
and file-discovered:

- **One systemd unit** — the existing `ldmux.service` (`KillMode=process`,
  linger, unchanged). The daemon **auto-spawns a detached keeper** (setsid,
  own session) on first need, and re-spawns if it finds it dead — exactly
  today's tmux-server position: an orphan of the daemon, no unit, discovered
  through its socket.
- This is the canonical **ssh-agent pattern** (researched 2026-09-27): the
  client (the daemon) checks the state dir, finds the keeper via socket +
  pid, and spawns it if absent. Failure modes and their fixes: spawn races
  → a non-blocking `flock` on `<stateDir>/keeper.lock` (the kernel releases
  it on death, unlike a pidfile); stale pidfile → pid read *inside* the lock
  + `kill(pid, 0)` + socket connect as the real probe (the TOCTOU caveat
  stays true; the socket connect is the authoritative liveness test).
- The keeper is **stateless beyond the kernel**: fds, child pids, one
  ghostty-vt per session. Everything durable is already a file in the
  state dir (registry.json, socket, lock, per-session metadata — the auth token excepted, see below) —
  it is, and stays, file-based. Nothing grows there: files are KB-scale and
  bounded by session count. Scrollback is keeper **memory**, bounded by
  `max_scrollback`, and is never written to a file (a file would be a
  duplicate, not a store). The state dir is `$XDG_RUNTIME_DIR/ldmux`
  (tmpfs, 0700, per-boot) — the XDG-correct place for sockets/pids: it
  vanishes on logout, which is correct because the shells vanish with the
  session. (The tmpfs-growth risk the user asked about is real — the
  Podman/XDG_RUNTIME_DIR NO-SPACE case — and does not apply here: no
  growing data, bounded files. The one item that needs reboot survival — the auth token — already lives in durable storage: `deploy/ldmux.service` pins `LDMUX_TOKEN_FILE=%S/ldmux/token` (systemd `StateDirectory`, i.e. `XDG_STATE_HOME`-equivalent) with a comment saying exactly why (a tmpfs token is minted per boot and locks out every paired device). That is the solved precedent for the split; nothing else needs it, because the shells die on reboot anyway.)

**Decision D1 (resolved per user direction, 2026-09-27):** keeper, one
unit, auto-spawned. Single-process (no keeper) is rejected: restart kills
shells — the regression the unit file's first line ("the daemon must
outlive every session") already engineered against. The generic systemd
guidance ("an orphan that outlives the parent is an anti-pattern for one
unit") is overridden here deliberately: the orphan-survival is the
measured, load-bearing behavior of this system, and the topology
reproduces it exactly.

### 3.3 The PTY layer — what the research settled

- **Crate:** `nix` with `openpty` (winsize-at-allocation = TIOCSWINSZ),
  `posix_openpt`/`grantpt`/`unlockpt` for the manual path, `forkpty` for the spawn.
  Zellij is the production reference. `portable-pty` is the fallback if cross-platform
  is ever wanted; it buys nothing on Linux (same `libc::openpty` underneath).
- **Death detection — the one detail everyone gets wrong:** EIO/EOF on the master is
  **not** the death signal. It is a secondary I/O signal (the master may still hold
  buffered bytes). The authoritative signal is `waitpid`/`waitid(WEXITED)`.
  Implementation: the keeper reaps via a dedicated reaper (SIGCHLD → `waitpid(WNOHANG)`
  loop, or the portable-pty-style explicit `wait`); on death it flushes the last
  master bytes, then reports `ChildExited` over the protocol. WezTerm's
  `DeadPendingClose` state (close the PTY **before** marking dead complete, so the
  slave-drop is clean) is the reference ordering.
- **Slave-drop gotcha:** the master reader gets EOF only when the **slave** fd is
  also closed. The keeper drops the slave after the child exits; until then EIO can
  appear on a live master — treat EIO as "start the death path, verify with waitpid".
- **`openpty` failure surface:** `EBUSY` (pty pool exhausted / `dev.pty.max`),
  `ENFILE` (system fd table), `EINTR` (retry), and **`EIO` on the slave open**
  — measured on this machine (2026-09-27): pty number 0 sits in a stale node
  state where `TIOCGPTN` returns 0 while the slave open fails EIO. The
  allocator must treat EIO/EBUSY as "close this master, re-allocate a fresh
  number" (glibc's `openpty` retry loop exists for exactly this; our
  `nix`-level path must replicate it — the naive loop that retries in place
  spins for 32 minutes, as measured).
  `ENFILE` is a capacity refusal; `EINTR` is a retry.
- **Backpressure:** the keeper **always** reads the master (it must — its emulator
  needs the bytes). The kernel pty buffer therefore never fills from the daemon's
  slowness; a flood blocks nothing except memory (§3.7).
- **UTF-8:** the pty layer is opaque bytes; the emulator does grapheme handling.
  Never split a multibyte sequence at the proxy layer (the framing layer already
  never does; the rule is written into the keeper's read path too).
- **SCM_RIGHTS:** researched, rejected. No fd passing needed: the keeper simply
  keeps the fds for as long as the shells live. (It would only matter for
  in-process restart, which the topology does not do.)

### 3.4 The emulator — libghostty-vt, and the exact API map

**What it is:** Ghostty's terminal core, Rust core + C FFI, MIT, published
(`libghostty-vt`, `libghostty-vt-sys`). The project **already vendors the host
prebuilt** (`prebuilt/host/lib/libghostty-vt.a`, headers under `prebuilt/host/include`),
builds it with `packages/mobile/scripts/build-libghostty-vt.sh` (Zig 0.16.0, pinned
Ghostty `d5eba8d`), and has a freshness test (`native-freshness.test.ts`). The daemon
links the same artifact and pins the same commit — one engine, one commit, two
consumers.

**Bindings decision (D2):** hand-written thin `-sys` crate in-repo against the
vendored headers, linking the vendored `.a`. Rationale: the project's dependency
culture is "one production dependency"; the headers and archive are already
committed with the crate (they exist on this machine only today — §5) and
freshness-tested; the C surface is **one generic accessor** — `ghostty_terminal_get` / `get_multi` keyed by the `GhosttyTerminalData` enum (`terminal.h`), not per-field C functions; the -sys binds that one function + the enum, and the ~20 named accessors are our ergonomic wrapper layer over it. The community
wrapper (`libghostty-rs`, Paneflow) is the alternative and is fine, but it imports a
moving third party for a surface we can pin to a file in the repo.

**Thread model:** `Terminal` is `!Send/!Sync` — one per session, owned by one task in
the keeper. `RenderState` is `Send+Sync` — snapshots can be produced under a lock
or by the owning task. No redesign needed; it is one task per session.

**The map** — every field the old `display-message` format fed, now direct. The field names below are `GhosttyTerminalData` enum values (`COLS=1`, `ROWS=2`, `CURSOR_X=3`, `CURSOR_Y=4`, `CURSOR_PENDING_WRAP=5`, `ACTIVE_SCREEN=6`, `MOUSE_TRACKING=11`, `SCROLLBACK_ROWS=15`, …) read through that single accessor — all the fields we need are present in the enum:

| Old field | ghostty-vt source |
|---|---|
| `cursor_x` / `cursor_y` | `cursor_x()` / `cursor_y()` |
| `alternate_on` | `active_screen()` (Primary vs Alternate) |
| `history_size` | `scrollback_rows()` |
| `pane_width` / `pane_height` | `cols()` / `rows()` |
| `pane_dead` | child state (waitpid, §3.3) |
| `mouse_any_flag` / `mouse_sgr_flag` | `is_mouse_tracking()` / `mode()` |
| `keypad_cursor_flag` (app cursor) | `mode()` (DECTCEM) |
| `bracket_paste_flag` | `mode()` (DECBR) |
| `pane_current_command` | `/proc/<pid>/comm` (keeper) |
| (new, free) current working directory | `/proc/<pid>/cwd` — today this comes from `pane_current_path`; same source, same use (launcher's "what is a shell") |
| (new, free) scrollback paging, wrap state, per-cell SGR | `scroll_viewport`, `grid_ref`, `RenderState` snapshot rows/cells |

**Snapshot rendering:** `RenderState::update(&terminal)` → snapshot → row/cell
iterators with per-cell SGR; emit tmux-`capture-pane -e` format: one line per row,
SGR-prefixed, **full-width** (the `-N` semantics — every cell to the right edge with
its attributes), `\n`-joined; history pages via `scroll_viewport`/grid access. If the
engine's formatter output diverges from `capture-pane` format, we emit the format
ourselves from the cell iterators (it is ours to write; the cells are the data).
§3.11 is the test that makes this safe.

> **Amended 2026-10-05.** "Full-width" means *every column is accounted for*, not
> "padded with spaces to the edge". A never-written cell is a space (it used to
> be empty text and was emitted as nothing, so everything right of a gap slid
> left); only the second half of a wide character is empty. Default-style blanks
> at the right edge are not emitted (every replaying client starts a repaint from
> a clear screen), styled blanks are, and each row ends with the style reset.

### 3.5 The keeper protocol

Framing: the existing 8-byte frame header (`protocol::frames`) — unchanged, already
proven over WS and Unix socket. Messages added (all in `protocol`, versioned like the
rest): `Spawn`, `Kill`, `Write` (input bytes), `Resize`, `Read` (attach/stream),
`Snapshot { history_lines }`, `State`, `List`, `ChildInfo { pid }`, `ChildExited`
(keeper→daemon), `Close`. The daemon↔keeper connection is the **same client
protocol as the WS connection**, minus auth — which buys the biggest testing win:
**a fake keeper is a test double for the entire daemon** (today's daemon tests need a
live tmux server; from now on they need a socket that speaks 10 messages).

### 3.6 Data flows (steady state)

```
shell  ──pty slave──> keeper master ──read always──> ghostty-vt (state + scrollback)
                                              │
                                              ├─> framed bytes ──> daemon ──sanitize──> fan-out
                                              │                                        ├─> WS clients (phone)
                                              │                                        └─> attach clients (desktop windows)
input:  client ──RPC──> daemon ──Write──> keeper ──write(master)──> shell
resize: client/attach ──Resize──> daemon ──Resize──> keeper ──TIOCSWINSZ + Terminal::resize
snapshot: client ──RPC──> daemon ──Snapshot──> keeper ──render──> daemon ──RPC result
exit:    waitpid ──> ChildExited ──> daemon: registry status, clients notified, session closed
```

### 3.7 Flow control — improved, not replicated

Today: a slow client makes tmux **pause the pane** (`pause-after=2`); the paused
pane's queued output is **discarded** (measured, control-client.ts comment); the
recovery is a snapshot resync. Tomorrow: the keeper never stops reading, so the
shell is never blocked by any client's slowness; each client has a bounded output
queue; overflow → drop + forced resync (the existing `onReset`/epoch machinery,
unchanged). Strictly better on the measured failure mode (no pane stalls on a bad
link), at a cost of daemon memory under flood — bounded by the same caps, and
`max_scrollback` bounds the grid history (the `history-limit 10000` analogue;
ghostty's scrollback **compression** mode is available if memory matters — measure,
don't guess).

### 3.8 Environment and the dead 20-line profile

The profile becomes a 7-variable spawn env + a login-shell argv:

```
strip:  BASH_ENV  ENV  PS1  PROMPT_COMMAND  ZDOTDIR        (same list, same reason)
set:    COLORTERM=truecolor  SHELL=<login shell>  TERM=xterm-256color
argv:   "-<shell>"  (login shell: the user's prompt arrives via the user's files)
```

Gone: `escape-time`, `remain-on-exit`, `window-size`, `destroy-unattached`,
`mouse`, wheel `bind-key`×4, `status`, `set-titles`, `set-titles-string`,
`aggressive-resize`, `set-clipboard`, `focus-events`, `extended-keys`×2,
`allow-passthrough`, `terminal-overrides`×3, `default-shell`, the profile **file**,
`writeServerProfile`, `ensureServer`, `source-file`, and the 3.5a crash workaround.
Every line of SPEC §5.2 that exists "only to make tmux behave" dies; the doc shrinks
to the spawn-env table.

### 3.9 Desktop windows become protocol clients

**Today:** `alacritty --class ldmux-live-<id> -e /bin/sh <shim.sh>` where the shim
records its tty in `@ldmux_live_tty`, records its pid in `@ldmux_live_pid`, loops
`tmux attach`, waits for a key or `@ldmux_held_elsewhere` clearing, restores stty.
**Tomorrow:** `alacritty --class ldmux-live-<id> -e <path-to> ldmuxd attach <session>`:

- `ldmuxd attach` is alacritty's child, so its stdio **is** alacritty's pty
  (it opens **no** pty pair — a nested one would double the winsize), connects
  to the daemon (attach = claim the shell, ADR-0010 handover), and pipes it ↔
  session bytes.
- Detached (phone took it): the daemon closes our write side / sends `Detached` →
  the attach client prints "this shell is open on another device — press r
  to take it back" and waits (**r** = reclaim; any other byte is
  swallowed, so dictation cannot trigger the reclaim; the daemon sees the re-attach
  while another device holds the shell and takes it back — **the keystroke is the
  consent**, exactly as today).
- Window closed: attach client exits (SIGHUP/EOF on its pty) → daemon sees the
  connection die → "window closed" vs "shell taken" is now **known, not inferred**
  (today: tty-list polling + `@ldmux_live_pid` `/proc` check).
- Size: the attach client reads its window pty's winsize on attach and on
  `TIOCGWINSZ` changes (poll or `SIGWINCH` on its pty) and sends `Resize` —
  implementing the "size belongs to whoever owns the shell" rule at the point of
  ownership.
- Wheel/copy/mouse: alacritty native (§2.7); TUI scrollback via a configurable
  keybinding → history request → re-render (the phone already has the full
  mechanism; the desktop gets a keybinding in front of it).
- **Everything i3-related stays:** window class, `i3-msg exec` argv (the
  separate-arguments gotcha documented in desktop.ts), workspace placement without
  following, `placeLive`/`closeLive`/`findWindowIdByClass`.

`ldmux window`/`run`/`shell` (the desk-window CLI) print/spawn the attach client
instead of the shim. `mirror` (read-only `tmux attach -r`) becomes
`ldmuxd attach <id> --readonly`.

### 3.10 What dies (line ledger)

| File | Lines | Fate |
|---|---|---|
| `probe.ts` (the control-client debug tool; `just probe` dies with it) | 30 | **Deleted** |
| `tmux/control-parser.ts` | 160 | **Deleted** (no protocol to parse) |
| `tmux/octal.ts` | 34 | **Deleted** |
| `tmux/control-client.ts` | 446 | **Deleted** (the control transport) |
| `tmux/pipe-client.ts` | 333 | **Deleted** (the pipe transport) |
| `tmux/control-plane.ts` | 996 | **~1/3 survives** as keeper-protocol client + spawn-env/login-shell helpers; the rest is direct fd work |
| `stream.ts` | 517 | Fan-out/coalescing/epoch **survives** retargeted at the keeper stream; "control"/"pipe" modes collapse to one transport; polling fallback becomes forced-resync (already a concept here) |
| shim script generation (desktop.ts) | ~60 | Replaced by the attach client |
| profile machinery (`writeServerProfile`, `ensureServer`, `tmuxConfigWord`) | ~120 | Deleted |
| tests: control-parser (134), control-client (137), pipe-client (267), control-plane (289), control-client-policy (70), deployed-tmux (75) | ~970 | Replaced by keeper tests + parity goldens |

Net: roughly **1,800 lines of tmux-transport code out** (the `tmux/` directory is 1,969 lines across its five files, minus the surviving ~1/3 of control-plane; plus the profile machinery ~120 and the shim ~60) **and ~970 lines of its tests out**, ~600 in,
and a 2,300-line-ish new surface (keeper + attach client + FFI) — but the new
surface has **no unspecified external protocol, no version-compatibility surface,
and no parser of an unversioned text format**.

### 3.11 Snapshot parity — the #1 risk, with a harness that settles it

The client replays snapshot text into its emulator. Today that text is tmux's;
tomorrow it is our render. The client's bytes-in must produce the same screen.
**As built (2026-10-05): a committed golden corpus, no tmux at test time.** The
harness was first run against a live tmux server (the reference), and its output
is now committed as goldens that run in the normal gate:

1. Corpus (`crates/daemon/tests/parity/NN-name.bin`): basic SGR, every attribute,
   16/256/truecolor, wide chars + emoji + combining marks, wrap, OSC 8, and scroll
   (40 lines into 24 rows). Not yet in the corpus: alt-screen programs, cursor
   positioning, terminal modes, and `-S -N -E -` history paging — those are pinned
   by the keeper/snapshot tests, not by the golden. Overline (SGR 53) is excluded
   because tmux 3.5a mis-serialises it (`ESC[5:3m`).
2. Goldens (`NN-name.capture`): what tmux 3.5a showed for the same bytes,
   `capture-pane -p -e -N`, captured 2026-10-05 by `scripts/regen_parity_golden.sh`
   (the generator; manual, needs tmux, never run by `cargo test`).
3. `crates/daemon/tests/parity.rs::golden_corpus_cells_match_the_reference_grids`
   decodes each golden (`tests/sgrep`) and compares it **cell by cell** with our
   engine's grid. The gate is on the cells, not the SGR bytes (the format is
   ours); the format half is `round_trip_render_then_parse_is_identity`.
   Provenance and regeneration: `crates/daemon/tests/parity/README.md`.

The original plan text (byte-diff against a live tmux, kept for the record):
the harness would be built before any deletion and run against a live tmux server,
byte-diffing `capture-pane -p -e -N` against the keeper render; the cells are the
source of truth and the format is ours.

### 3.12 New risks, honestly ranked

| # | Risk | Severity | Mitigation |
|---|---|---|---|
| R1 | Snapshot byte-parity (§3.11) | High — a wrong snapshot is a wrong screen with no error | The golden harness lands **before** tmux is deletable; the client's engine == the daemon's engine, so a format error is a format error, not an engine divergence |
| R2 | PTY supervision bugs (EIO/waitpid ordering, slave drop) | High — wrong death handling = lost or zombie shells | §3.3 is the spec; integration tests assert every transition (exit, crash, kill, SIGHUP, daemon-restart-reconnect); WezTerm's state ordering is the reference |
| R3 | Keeper crash = shells die (new single point of failure) | Medium — but **parity**: tmux server crash is the same today | The keeper is small, Rust, no user input parsing (the protocol is framed + length-bounded); the daemon auto-respawns it (§3.2 — it is not a unit, so systemd has nothing to restart; D1); accept and document |
| R4 | libghostty-vt C API instability (header says "not stable") | Medium | We already ship one pinned commit for the client; the daemon pins the **same** commit; a bump is a coordinated change with the freshness test; the `-sys` binds ~20 functions |
| R5 | Memory under output flood (keeper always drains) | Medium | Bounded per-client queues + forced resync; `max_scrollback` bounded; compression mode available; the flood test is in CI |
| R6 | Auto-spawned keeper (spawn race, stale pidfile, keeper death invisible to systemd) | Low | flock (non-blocking, kernel-released) + pid-in-lock + `kill(0)` + socket-connect probe (§3.2); the daemon reconciles a dead keeper exactly as it reconciles a dead tmux server today — same code path, same detection (connect fails); the §3.2 parity table is the acceptance test; rollback window keeps the tmux units (§9) |
| R7 | Desktop TUI scroll parity (copy-mode's niche) | Low | Keybinding + history re-render (§3.9); the phone already works; flag for device check |

### 3.13 Defect work — live-window reclaim and phone switching (root-caused; in scope)

Three independent mechanisms, all root-caused 2026-09-27 (deep investigation,
file:line in each item). **As of the 2026-09-28 audit, none of the three fixes is
implemented in the TS tree** — the root causes stand as written; each fix lands
in the current TS system first, and its semantics then port into the Rust
design. D-1 is live in production until then: dictation into a focused desk
window mis-reclaims the shell on any byte.

**D-1 — the reclaim prompt fires on any byte (voice-to-text triggers it).**
The shim's wait loop (`liveShimScript` in `desktop.ts`): `stty -icanon
echo min 0 time 10` + `dd bs=1 count=1` — **any single byte** breaks the
loop and re-attaches, which the daemon reads as consent. Dictation left
running types into the focused window: the bytes are silently swallowed,
one breaks the loop, the window re-attaches, and the phone shows
held-elsewhere — without the user doing it. **Fix:** the reclaim key was a
**control byte — Ctrl+R (0x12)**: never produced by text dictation. **Amended
2026-10-05:** the key is **`r`**, and only `r`, at the user's request. `r` can
appear in dictated text, but the notice names it and it is one key to remember;
an escape in the read (a paste, an Alt chord, a function key) still never
counts, whatever the terminal emulator encodes. (The
"Enter" proposal is the floor, not sufficient: dictation emits Enter/
newlines, and Enter and Ctrl+Enter are indistinguishable on most terminals
— both 0x0D.) Non-trigger bytes are **echoed back** (today they are
swallowed with no feedback), and the prompt names the key. The shim also
**records the consent** (a session option today; a `Reclaim` protocol
message in the Rust design, D-2) so D-2 can tell "pressed the key" from
"window is open" — and the option handshake is verified end-to-end: the daemon sets `@ldmux_held_elsewhere=1` (`daemon.ts:594`) and clears it (`:549`, `:588`, `:612`); the shim waits for the clear (`desktop.ts:231`). *Port:* the attach client inherits the reclaim key + prompt logic; the consent becomes a protocol message.

**D-2 — the daemon force-claims from an idle window (no keystroke involved).**
`reconcileLivePass` (`daemon.ts:900-937`, ticked every ≤1 s by the 2 s
timer / any `list`): "is the window's tty in tmux's client list?" is
treated as "the desktop is using it" — an idle, open, backgrounded window
satisfies it. Chain: pager swipe deactivates a page → `attach.stop()` →
`releaseClaim` drops the phone's claim → the next tick force-claims the
session to the desktop device (`claimSession(id, desktop, true)`, silent,
no signal to the phone) → swipe back → attach refused (`owned`) → the
confirmation screen for a shell **nobody took**. The `justTaken` 3000 ms
grace (`daemon.ts:927`) shows this race class was already patched in one
direction; this is the other, unpatched. `AttachClient.armedOnce` (added
to fix "swipe away and back steals the shell") is why the phone can no
longer silently reclaim — any fix must not reopen that bug. **Fix:**
force-claim only on (a) **recorded consent** (the D-1 key) or (b) **recent
pane activity** (the `lastActivityAt` the daemon already builds from
activity timestamps, `daemon.ts:505-506`) — never on client presence alone. The surrounding comment (`daemon.ts:912-923`) records why the force-claim exists at all — an earlier bug where the next phone to open an unclaimed shell took it silently while the desk was typing; the fix must preserve that case (genuine typing = activity = still force-claims), not delete the path. Consequences, exactly the requested behaviour: idle open window →
no claim → the phone swipes back and re-claims **silently — no screen, no
tap** ("no extra step"); someone genuinely typing in the window →
activity → claim → the phone shows held-elsewhere on return (**the screen
stays for the real case**). The 2 s/1 s/3000 ms magic numbers become named
constants behind the gate. *Port:* the attach client is a first-class
protocol client — no `list-clients` polling at all: the daemon knows its
own connections, the `Reclaim` message replaces the option, and the
activity gate reads keeper pane activity (the emulator already knows).

**D-3 — the pager swipe "wipes side to side" and never lands (phone).**
`App.kt:667-706`: the drag-vs-reset decision latches `sawDrag` from
`collectIsDraggedAsState()` over a `snapshotFlow`. `snapshotFlow`
conflates: under main-thread load — 3–4 sessions open (`beyondViewportPageCount = 2`
keeps neighbours composed), one a `pi` TUI redrawing continuously while it
receives text — the collector can miss the transient `fingerDown=true`
pulse entirely, so a real swipe is misread as a re-measure reset and the
`scrollToPage(target)` branch snaps the pager back: the swipe is visually
real (the pager's own gesture rendering) and then undone — "resetting".
**Fix:** observe the underlying event stream — `interactionSource.interactions`
(`DragInteraction.Start/Stop`) — directly, not a derived boolean over a
conflated flow; secondary mitigation: stop recomposing inactive pages.
Kotlin-side only; no port impact — recorded here so the port's "the phone
is unchanged" claim is accurate about what the phone needs.

### 3.14 The 2026-09-28 audit — what was verified, what was fixed

**Verified against the tree** (the plan's claims hold; the audit's positive
finds):
- The §3.10 line ledger, every count: control-plane 996, control-client 446,
  pipe-client 333, control-parser 160, octal 34, stream 517; tests 134+137+
  267+289+70+75; the `tmux/` directory 1,969 across its five files
  (`screen-diff.ts`, 60, survives per the §4 map).
- ~20 file:line cites: control-plane.ts:688/700/755/871/947-1010,
  daemon.ts:481/833/900-937/927/505/1322-1329, stream.ts:188/194,
  desktop.ts:140/226-236, App.kt:667-706. (One cite was off: `clampSize`,
  corrected in §2.4 to 213-232, used at :875.)
- The §2 ledger rows: the 12-field `display-message` format, the four wheel
  `bind-key` lines, `set-titles`/`#T`, the env-strip list, the `/etc/passwd`
  login-shell logic, `capture-pane` escapes+pad, `HISTORY_CAPTURE_LINES` 5000,
  the alacritty shim argv.
- The §3.4 API map against the vendored `terminal.h`: enum values (COLS=1,
  ROWS=2, CURSOR_X=3, CURSOR_Y=4, CURSOR_PENDING_WRAP=5, ACTIVE_SCREEN=6,
  MOUSE_TRACKING=11, SCROLLBACK_ROWS=15), `ghostty_terminal_get`/
  `get_multi`, the compression modes, and the "not stable" warning R4 rests
  on.
- The D-1/D-2/D-3 root causes and the existing `@ldmux_held_elsewhere`
  handshake (daemon.ts:594 set / :549,:588,:612 clear; desktop.ts:231 waits
  for the clear).
- Gate state at audit time: 347/347 suite, typecheck clean, 198 native
  checks, Kotlin suite green; engine pin `d5eba8d` + Zig 0.16.0 confirmed in
  the devShell.

**Ecosystem, September 2026:** tmux 3.7c latest stable (3.8 RC only — the
code's "3.8+" comment is consistent with that); Rust 1.98.1 (2026-09-03);
Node 26.10.0 (the flake's 26.8.2 is a normal pin; Node 26 enters LTS in
October 2026, so the flake's "the Current line" comment goes stale in ~3
weeks); Zig 0.16.0 is the current stable (the pinned engine build uses it);
`nix` and `portable-pty` both maintained, the Linux-only call stands; no
official libghostty-vt crate (community wrapper, same distribution model).

**Fixed in this revision** (all in this doc, none in the code yet):
- F1 §3.13 "each is fixed in the current TS system" was false — none of
  D-1/D-2/D-3 is implemented (the shim still reclaims on any byte: a live
  production defect; `reconcileLivePass` still claims on presence; App.kt
  still latches `sawDrag`). §3.13 now says "root-caused; unimplemented".
- F2 §3.9 "any key = reclaim" contradicted D-1's Ctrl+R; §3.9 now names
  Ctrl+R.
- F3 R3 "systemd auto-restarts it" contradicted D1 (the keeper is not a
  unit); now "the daemon auto-respawns it".
- F4 §0/D2/§5 claimed the engine headers and host `.a` were vendored/checked
  in; `git ls-files` under `prebuilt/` is empty — they exist on this machine
  only. Corrected; the headers get committed (§5) and §6 gains the engine-pin
  cross-check.
- F5 §9 was missing the highest-mention docs and ADRs (see the §9 list above);
  ADR-0006 §2 and ADR-0010:116 are inverted by the port.
- F6 §6 lacked the keeper-death e2e and the spawn-race test (the daemon-death
  path was covered; the keeper-death/respawn path — the one a restart actually
  exercises — was asserted, not tested). Both added.
- F7 §4 "three bins or thin wrappers" was incoherent with the subcommand list;
  the post-port wrapper layout is now named.
- F8 §3.9 "opens its own pty pair for alacritty" was incoherent (the attach
  client is alacritty's child and inherits its pty); corrected so a fresh
  implementer cannot build a nested pty.
- F9 §0 "published on crates.io" read as an official property; corrected
  (community wrapper).

---

**Flagged decisions for the user:**
- **D1 (RESOLVED 2026-09-27, per user direction):** keeper, one unit,
  auto-spawned detached keeper (ssh-agent pattern) — §3.2. Single-process
  rejected: restart kills shells, a regression the unit already engineered
  against.
- **D2:** in-repo `-sys` (recommended; vendored headers, pinned) vs the community
  `libghostty-rs` wrapper.
- **D3:** pane `TERM`: `xterm-256color`+`COLORTERM=truecolor` (recommended —
  standard, in every terminfo) vs `tmux-256color` (requires the tmux terminfo
  entry to exist on the host — another compatibility surface we are deleting).
- **D4:** the A/B window: how long the tmux flake package + parity CI stay after
  the switch (recommendation: one release cycle).

---

## 4. Project structure

```
crates/
  protocol/            # frames, messages (incl. keeper messages), rpc, pairing, net, paths, socket
  keeper/              # PTYs, children, ghostty-vt per session, keeper socket server
  daemon/             # daemon proper: registry, ownership, remote WS, streams, desktop, CLI
  attach/             # the desktop window client (ldmuxd attach)
  testing/            # expect-style test utilities, fake keeper, tmux-cleanup (transitional)
binaries:              # one cargo package; the wrappers: `ldmux` (CLI) and
                        # `ldmux-terminal` (desk-window launcher, spawns
                        # `alacritty -e ldmuxd attach <id>`) survive
  ldmuxd             # subcommands: daemon | keeper | attach | <cli: list, send, …>
crates/vt-sys/         # hand-written FFI against the vendored headers (D2)
```

The TS→Rust module map (survivors only; §3.10 is the death list):

| TS module | Rust | Note |
|---|---|---|
| `protocol/*` (7 files) | `protocol/*` | + keeper messages |
| `auth.ts`, `qr.ts`, `sanitize.ts`, `screen-diff.ts`, `ownership.ts`, `registry.ts`, `cwd.ts`, `exec.ts`, `proc.ts`, `pairing.ts` | same names | `proc.ts`'s group-kill/detached-spawn survives for the keeper's children and the daemon's i3 spawns |
| `stream.ts` | `daemon/stream.rs` | retargeted §3.6 |
| `remote.ts` | `daemon/remote.rs` | unchanged in substance |
| `desktop.ts`, `desktop-inventory.ts` | `daemon/desktop*.rs` | i3 code survives; shim generation dies |
| `cli.ts` | `daemon/cli.rs` | clap |
| `tmux/control-plane.ts` | `daemon/keeper_client.rs` + `keeper/*` | split per §3.10 |
| `desktop.ts` shim | `attach/` | new |
| (new) | `keeper/{pty,session,emulator,server,proc_reap}.rs`, `vt-sys/` | the new surface |

---

## 5. Build system

**flake.nix** — `runtime = [ rustup ]` (devShell: `just jq git curl sqlite procps zbar cargo rustc rustfmt clippy`), tmux **removed from runtime** (devShell keeps it only during the A/B window, D4). Zig stays (it builds ghostty-vt; the mobile shell already has it). The ghostty build script gains a `host` target that reproduces the prebuilt (the script exists; the host prebuilt is built locally today and **not committed** — the headers get committed, and the flake makes the build reproducible rather than machine-local).

**justfile** — `test: cargo test --workspace -- --test-threads=4`, `typecheck: cargo check --workspace`, `clippy`, `fmt`; `daemon: cargo run -- daemon`; `cli: cargo run -- {{args}}`; `keeper: cargo run -- keeper`; `install` builds the workspace, writes the wrapper + the **one** unit, `systemctl --user daemon-reload`.

**deploy/** — **one unit**: `ldmux.service` (daemon, `Type=simple` or `notify`, `KillMode=process` — the existing file; the keeper is an auto-spawned orphan, §3.2, exactly as today's tmux server is an orphan), wrappers unchanged in shape (`exec <bin> <subcommand>`). The unit header's three-requirement list (KillMode=process, linger, KillUserProcesses) survives verbatim; its "tmux server" prose becomes "keeper".

---

## 6. Testing strategy

**Inherited** (every TS test with a Rust twin; same cases, same assertions — the
original plan's table applies, minus the tmux-transport rows): auth, qr, sanitize,
screen-diff, ownership, registry, pairing, paths, frames, net, socket, remote,
stream (retargeted), desktop-inventory, cli, daemon-lifecycle.

**New, and the ones that carry the risk:**

- **PTY supervision (keeper/pty):** spawn/resize/kill; child exit → last bytes
  flushed → `ChildExited` (ordering asserted); EIO on a live master (kill -9 the
  slave's writer); `openpty` `EINTR` retry; group-kill on session kill; no zombies
  after 100 sessions (property: N spawns, N exits, zero leaked fds — `lsof`
  count).
- **Snapshot parity goldens (§3.11):** the corpus, byte-diffed against a live
  tmux server, CI'd during the A/B window, then frozen as goldens after
  tmux leaves the flake.
- **Fake-keeper daemon suite:** the daemon tested against a fake keeper for the
  full lifecycle (new→list→send→capture→snapshot→kill, ownership, live-window
  handover, daemon-restart-reconnect) — **no live terminal anywhere in CI after
  the port**, which is strictly easier to run than today's live-tmux suite.
- **Restart-survival e2e:** spawn 3 sessions in a scratch keeper; kill the
  daemon; daemon re-attaches; all 3 shells still run; assert per the §3.2 table.
  (This is the product's load-bearing test; it runs on every CI.)
- **Keeper-death e2e (the path a restart actually exercises):** kill the keeper
  with 3 live sessions; assert the shells die (parity, R3), the daemon detects
  the death via the failed connect, respawns an empty keeper under the flock,
  reports every session dead to the clients, and leaves zero zombies / no wedged
  state.
- **Spawn race:** two concurrent first-needs; one wins the flock, the other
  finds the live socket and attaches — no duplicate keeper.
- **Engine-pin cross-check:** host `.a` stamp == Android `.so` stamp ==
  `GHOSTTY_COMMIT` — "one commit, two consumers" enforced by a test rather than
  by discipline (F4).
- **Backpressure:** flood a session at 100 MB/s while a client stalls 12 s;
  assert: zero bytes lost to the shell (it never blocks), client queue bounded,
  resync issued, memory bounded by cap. (Today's TMUX-DEPENDENCY §8 measured the
  tmux equivalent; this is its successor.)
- **Flow of a dead reader:** kill an attach client mid-stream; assert the session
  survives (the pipe-era failure mode, now impossible by construction — the
  test asserts the construction).
- **Desktop attach e2e (headless):** Xvfb + i3 scratch + alacritty-stub that
  speaks the attach pty; assert handover both directions, size-on-attach,
  wheel→history keybinding, close≠detach.
- **Property tests:** frame codec round-trip (existing plan), sanitizer
  (any input → valid output, OSC 52 dropped), protocol message
  (de)serialization round-trip, scrollback paging (page N is a suffix of page
  N+1 at the same viewport).

---

## 7. Parallelization — phases (agent-parallel, ~5 h serial, ~2.5 h with 6 agents)

> **Status, 2026-10-04:** phases 1–7 are implemented and the Rust daemon is
> **deployed live** (systemd user unit, keeper auto-spawned, phone wire contract
> verified against the running server). The table below is the plan as written;
> `docs/RUST-PORT-LOG.md` ("Live deployment and the deep audit", "D4") is the
> record of what was actually done and found.
>
> **D4 is done (2026-10-04).** The TypeScript tree is deleted
> (`packages/{daemon,protocol,testing}`, `package.json`, `tsconfig.json`,
> `node_modules`), tmux is out of `flake.nix` and out of the deployment, tmux is
> gone from the Rust sources, and the retirement is written up: ADR-0018 is the
> decision, and the docs that described the tmux system carry a dated banner
> (§9 of this plan is the checklist that was worked through). The snapshot-parity
> harness is kept `#[ignore]`d as the R1 evidence artifact — the last green run
> is in the log, because with tmux deleted there is nothing left to compare
> against. The **D4 window** this plan recommended as a release cycle was closed
> early, at the user's direction.

Dependency graph: `protocol` → {pure modules ∥ keeper ∥ vt-sys ∥ attach} →
`daemon` → integration/deploy.

| Phase | Agents | Content | Gate |
|---|---|---|---|
| 1 | A1–A4 | `protocol`: frames/messages/rpc/pairing/net/paths/socket **+ keeper messages** | `cargo test -p protocol` |
| 2 | B1–B6 | pure modules: auth, qr, sanitize, screen-diff, ownership, registry/cwd/exec/proc/pairing | per-module tests |
| 3 | C1–C2 ∥ D1–D2 ∥ E1 | C: keeper PTY+supervision+server skeleton; D: `vt-sys` FFI (against vendored headers) + emulator task; E: attach-client skeleton (pty pair + protocol loop) | keeper boots, serves `List`/`Spawn`; FFI links the vendored `.a`; attach boots headless |
| 4 | F1–F2 | keeper: emulator integration (per-session ghostty-vt), snapshot renderer + **§3.11 parity harness run against live tmux**, state fields, scrollback paging | parity report: zero divergences (or a defect list with reproducers) |
| 5 | G1–G3 | daemon: remote (full `onMessage` dispatch, device ownership, session broadcast, sweep), stream (retargeted, §3.7), desktop+inventory, cli, keeper client, registry/ownership rewiring, **main entry point** (`main.ts` 115) | full suite green against the fake keeper (the ported TS tests + the new Rust tests for the daemon's main loop, remote, desktop, CLI, and main entry point — plan §3.5) |
| 6 | H1–H2 | desktop e2e (attach client + i3), restart-survival e2e, backpressure, dead-reader | §6 risk tests green |
| 7 | I1 | build/deploy: flake, justfile, 1 unit + auto-spawned keeper, wrappers; A/B switch + tmux removal from runtime (D4 window starts) | deploy green on the box; phone against Rust daemon |

The A/B is now the **whole-layer** experiment: parity harness (§3.11) + device run
+ restart-survival e2e, then delete the TS tree and the tmux runtime package.

---

## 8. Migration and rollback

1. Build the Rust tree alongside the TS tree (separate state dirs; nothing shared).
2. Run the parity corpus (§3.11) against the live tmux server — **this is the
   evidence that makes deletion safe.**
3. Device run: phone against the Rust daemon/keeper pair (pairing link, full
   session lifecycle, live window, copy/scroll, copy-mode scrollback, slow-link
   run with the existing `scripts/slow-link.ts`).
4. Switch: `just install` installs keeper+daemon; `systemctl --user restart
   ldmux` (one unit); the daemon auto-spawns the keeper on start; the TS unit is disabled, not removed.
5. Rollback: the TS tree + tmux unit come back with one `systemctl` switch; the
   registry JSON is format-identical, so no data migration either way.
6. After the D4 window: delete TS, tmux from runtime, `deployed-tmux` test,
   `LDMUX_TMUX_BIN`, the profile mechanism; SPEC §5.2 shrinks to the spawn-env
   table; TMUX-DEPENDENCY.md closes as "decided, done" with the ledger as the
   record; new ADR: "0018 session layer" — 0017 is taken by qr-pairing (number per the existing ADR sequence —
   the control-mode question in TMUX-DEPENDENCY is superseded, not answered).

---

## 9. Post-port cleanup (was §11, updated)

Delete: `packages/{daemon,protocol,testing}/src+test` (TS), `package.json`,
`tsconfig.json`, `node_modules`, the tmux runtime package, `ab-transport.ts`
(control-vs-pipe is moot; `measure_latency.ts` and `slow-link.ts` survive,
retargeted). Update — per doc, sized by tmux-mention count: `SPEC.md` (84 mentions — §5.2 profile → spawn-env table, §7.4 wording, transport sections: a pass, not a one-liner), `STATE.md` (39 — which status lines go stale at which port phase), `REMOTE.md` (6 — was missing from this list), `README.md` (12), `AGENTS.md` (18 — the file agents read every session), `NATIVE-TERMINAL.md` (2), `TMUX-DEPENDENCY.md` (42 — gets a closing note: decided, done, with the ledger as the record, not silent staleness), and ADRs 0001/0015 marked **superseded**, not deleted (the record of why). Also, per the same count (audit 2026-09-28): `TRANSPORT.md` (19 — the control-mode deep-dive; the doc the port most directly retires), `INVESTIGATION.md` (23 — historical; annotate, don't rewrite), `DESKTOP-INTEGRATION.md` (14), and ADRs 0002/0003/0006/0008/0010/0012/0014 — of which **ADR-0006 §2 and ADR-0010:116 are inverted by the port** (ldmux owns the PTYs and the scrollback grid: mark superseded, not deleted, with the date). **Keep:** the mobile tree, the prebuilt, the freshness test.

**Summary of the shape of the result:**

| | Before | After |
|---|---|---|
| PTY owner | tmux server (C, 20+ y stable) | `ldmux-keeper` (Rust, ~2k loc, ours) |
| Terminal state | tmux's grid (2nd engine) | ghostty-vt — **the client's own engine** |
| Transport | control mode (unspecified text protocol, 160-line parser) ∥ pipe | one framed binary protocol we own |
| Desktop window | tmux client + shim.sh + `@options` + tty polling | protocol client (`ldmuxd attach`) |
| Profile | 20-option file + serialization mechanism | 7-variable spawn env |
| Restart survival | measured (KillMode=process + tmux orphan) | measured-equivalent — 1 unit, auto-spawned detached keeper (ssh-agent pattern; the same topology as today's tmux server) |
| Version-compat surface | tmux 3.5a/3.7b, `-J`/`-N` fallbacks, `LDMUX_TMUX_BIN` | none |
| New single point of failure | tmux server crash | keeper crash (parity, documented) |
| Languages (Linux) | TS+Node+tmux | Rust (+ vendored C engine) |
