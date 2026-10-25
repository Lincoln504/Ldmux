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
| Keeper: PTYs, child supervision and reaping, one libghostty-vt per session | `crates/keeper` | Implemented. Its own systemd unit (daemon-spawned without systemd), outlives the daemon; one scope per shell. |
| FFI to libghostty-vt (static archive built by nix from the pinned commit) | `crates/vt-sys` | Implemented. Verified on x86_64 Linux (glibc); aarch64 Linux evaluates, unbuilt. |
| Daemon: registry, ownership, remote WebSocket, streams, desktop + inventory, CLI | `crates/daemon` (`ldmuxd`, `ldmux`) | Implemented and deployed. |
| Desk-window client (`ldmuxd attach`) | `crates/attach` | Implemented. |
| Mobile client (KMP + Compose, JNI shim over libghostty-vt) | `packages/mobile` | Android done and device-verified. Key bar: a 3x2 arrow pad in both modes (no empty cells), `paste` in portrait, sticky `Ctrl`/`Alt`/`Shift`. Pages keep the order they were opened in. iOS not started. |

There is no tmux and no TypeScript daemon: both were removed at D4 (2026-10-04,
ADR-0018). tmux survives only as a dev-time reference in
`scripts/regen_parity_golden.sh`. Node is a dev tool for `nix run .#mobile-test` and two
scripts, not for the Rust gate.

## Mesh transport

Design only, nothing implemented. `docs/MESH-TRANSPORT.md` asks whether a terminal
over MeshCore is plausible. `docs/MESH-GATEWAY.md` answers the carriage: 142 B of
record on the phone's uplink, 160 B on the host's, an explicit record length because
the binary path zero-pads without stripping, and ARQ owned by the ADR-0020 session
rather than by any relay. Latest position: design only.

## What is deployed

- `systemctl --user` units `ldmux` (source `deploy/ldmux.service`, runs
  `~/.local/bin/ldmuxd`) and `ldmux-keeper` (source `deploy/ldmux-keeper.service`, the
  binary beside the daemon; the daemon starts it, ADR-0021). Each shell is a scope under
  `ldmux-shells.slice`. Without `LDMUX_KEEPER_UNIT` the daemon spawns the keeper itself.
- Installed with `nix run .#install` or `scripts/install.sh [BIN_DIR]` (never by
  hand-editing `~/.local/bin` or the unit). The unit is a template (`@BIN_DIR@`) and
  reads an optional `~/.config/ldmux/env` (`LDMUX_REMOTE_HOST`, `LDMUX_REMOTE_PORT`;
  template `deploy/ldmux.env.example`). There is no `just`.
- Desk windows are opened by the daemon (direct terminal spawn, optional i3/sway/hyprland
  placement) and attach over the local `<control>-attach.sock`; `docs/PLATFORMS.md`.
- **A deploy does not replace a running keeper.** The keeper outlives the daemon on
  purpose, so after an install it keeps executing old code until
  `nix run .#restart-keeper` (`systemctl --user restart ldmux-keeper`), which ends every
  shell. Migrating a daemon-spawned keeper to the unit ends every shell once. The install prints a warning.
  A new daemon works against an old keeper (it reads either form of `Peek`) and logs
  once that the keeper predates bounded peeks, after which a flood is served worse.
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
- **Copying multiple lines on the phone inserts a line break between them** (reported
  2026-10-14, not investigated): copied text pasted elsewhere (a chat such as pi) comes
  out double-spaced, so the mobile copy path is the suspect.
- **"The desktop took the shell back" when nobody touched it** — reproduced on the
  device 2026-10-24 and fixed (ADR-0010 amendment): a page socket closing was treated
  as a release after the same 3 s grace an unbacked claim gets, which is exactly the
  phone's own reconnect floor, so a backgrounded app handed the shell to the waiting
  desk window. A dropped link is now a suspicion for 15 s (`RECONNECT_GRACE_MS`), the
  displacement names the holder instead of `null`, the phone clears the notice and
  re-attaches when the `sessions` broadcast shows the shell free, and the desk window's
  own notice wording stops inventing a device. **The running daemon and keeper are the
  old binaries** (see below).
- **Tap-to-cursor** and a few **selection** rough edges (auto-scroll on drag,
  Compose renderer hardening: cursor blink, zoom). Detail in `docs/archive/STATE.md` §5.
  The session-switching failures (scroll/tap/selection doing nothing after a switch)
  are fixed; see `docs/MOBILE-STATE-AUDIT.md`.
- **Live voice mode** (hands-free conversation with a coding agent): planned only,
  in `docs/VOICE-MODE.md`. Nothing implemented; it needs an ADR and an amendment
  to ADR-0009 before any code.
- **Multi-client pane geometry** (confirmed on a device 2026-10-06): the phone set a
  session's shared pane to 48x17 while the desk window's own tty was 158x82, so the
  desk window rendered thin. One pane has one size, so two clients of different sizes
  cannot both be right (ADR-0008 consequences). A desk window now re-asserts its own
  size when a snapshot disagrees with its tty (`crates/attach`), so the holder wins;
  the general per-client viewport question is still open (`docs/MOBILE-STATE-AUDIT.md` §3).
- **A terminal reply never re-enters the emulator.** An answer goes into the pty
  master (the program's input), and a program that echoes what it reads turns it back
  into output — which the emulator would answer again, for ever (measured 2026-10-25
  at ~60 replies a second through a pane running `cat`). The keeper drops the
  parameterised reply form (`CSI ? <flags> u`) from the output stream; the bare query
  and the push are untouched (`without_kitty_replies`, `crates/keeper/src/emulator.rs`).
- **A reply to a terminal query is dropped rather than queued.** The keeper's
  write-back writes from the emulator thread, and the pty master is blocking, so a
  program that asks and does not read (a `cat` fed binary) could have wedged the
  thread that also drains the master — output would stop and the session would read
  as hung. The callback asks `poll(POLLOUT, 0)` first and drops the reply when the
  queue is full (`crates/keeper/src/emulator.rs`). Reachable only since the keeper
  started answering queries at all.
- **A history page is sized when the user asks, not when the page attaches**, and it
  is never smaller than what is already loaded. A reply *replaces* the scrollback, so
  a slow-link cap (4,000 rows) applied to a fast-link page (5,000 rows already held)
  would have shortened the history and then looked like "the keeper has no more", and
  the client would have stopped asking for good. `maybeExtendHistory` in
  `TerminalController.kt` now treats a cap at or below what is held as exhausted.
- **A deliberate leave costs the other device up to 15 s.** The reclaim grace is one
  number for every socket close, because there is no detach message on the wire: a
  phone returning to the launcher and a phone whose link died look the same to the
  daemon. The consequence is that a waiting desk window reclaims a shell the phone
  put down after up to `RECONNECT_GRACE_MS`, where it used to be 3 s. Fixing it
  properly needs a client "I am leaving" message (the client closes its socket
  without one), and that is a protocol change, not a constant.
- **The mobile client advertises `inplaceResync`** (2026-10-24): a client that falls
  behind is now repaired with `caps` + `snapshot` on the same socket instead of a
  reconnect, which is the two-orders-of-magnitude lever in `docs/WIRE-COST.md` and the
  event that used to make ownership look like it had changed hands. Not advertised:
  `sessionEnded` (the ending still arrives as a `resync`/`session_dead`), which needs a
  device check before it is claimed.
- **iOS**: not started; needs a macOS host (`docs/IOS-DEVELOPMENT.md`).
- **Mesh/LoRa**: investigated only (`docs/MESH-TRANSPORT.md`); no code, no radio
  measurements.

Daemon and protocol:
- **The keeper is a single point of failure** by design: if it dies, every shell ends.
  Its `sessions` table and output buffers are now poison-tolerant, so a panic while one is
  locked no longer disables every later request, and its reaper task now survives a
  panicking pass, so a shell that exits while nobody calls the keeper cannot be left
  unreaped for the life of the process. **The running keeper is still the
  old binary** until it is replaced (which ends every shell): these fixes are on disk only.
- **Desk windows opened before 2026-10-06 22:07 run the old attach client** (no
  `inplaceResync`, no size re-assert). They stay correct against the fixed daemon; a window
  opened from now on takes the new path.
- **Endings are recorded** (`exitCode`, `exitSignal`, `endedKind`, `endedReason`; the
  `session_ended` message). The final screen of an ended shell is kept for 60 s only
  (SPEC §5.5); the final scrollback is not retained after that.
- **SPEC §5.4** (REST surface) is aspirational and marked so; the shipped surface is the
  Unix-socket RPC plus the WebSocket.
- `snapshotEpoch` is sent in `caps` and never read by the client; `follow` is never sent.

Shell view: pages are the user's, numbered 1..N (2026-10-25):
- **A page keeps the order it was opened in.** `pageOrder` is a list of session ids
  (`pagesInOrder`, `packages/mobile/.../PageOrder.kt`), not a set filtered through the
  session list. The session list is the daemon's, sorted **last activity first**
  (`sessions.sort_by` in `crates/daemon/src/daemon.rs`), so the page at index N used
  to become a different shell every time any shell printed anything: with several
  shells open the screen slid between shells and came back on its own, and typing in
  one shell was enough to trigger it (reported 2026-10-25). Measured on the host
  2026-10-25: with eight live shells, `ldmux send d8a750218a 'echo x'` moved that row
  from position 8 to position 1 in `ldmux list` — a page's index is a different shell
  after one keystroke. An id a momentarily stale list does not carry keeps its place in
  the order rather than being dropped and re-appended (it is omitted from the pages
  while it is missing, so the pages after it sit one earlier, and it returns at its
  own index).
- **The number is the page number, in both places.** The header reads
  `3 / 5 shells`, and the launcher's tick box shows the page a chosen row will become
  (`ordinalIn`), in the order the rows were ticked — so the row you ticked first is
  page 1, which is the page the pager will show first.
- **No empty cells in the key bar.** `ARROW_PAD_ROWS` is `[Up]` over
  `[Left, Down, Right]`: `Up` is one full-width key centred over `Down`. The first
  version was `[null, Up, null]` over the row of three, which drew the same cross and
  left two blank cells where a thumb goes (reported 2026-10-25). The list has no
  `null` in it, so the layout cannot grow one back.

Desk window liveness (2026-10-25):
- **A window cannot freeze silently any more.** One was found frozen and small — pane
  46x22 (a phone's grid) in a 158x40 window, no claim, no notice, and zero read
  syscalls in twenty seconds — after the phone that had used that shell died. The
  keepalive was a `select!` branch and the loop was stuck elsewhere, so nothing was
  pinged and nothing reconnected. The attach client now bounds its receive wait
  (`read_stall_limit`) and every send (`SEND_TIMEOUT_MS`), and retries a refused size
  re-assert (`REASSERT_RETRY`). Measurements and the full account:
  `docs/DESKTOP-INTEGRATION.md` §"A window that goes quiet".
- **Known gap**: the write to the window's own pty is still unbounded. A terminal
  that stops taking output holds the client there — not this incident (the terminal
  repainted on the next snapshot), and not guessed at.

Shell environment (2026-10-24):
- **`PATH` is inherited and floored, never replaced.** The units used to pin
  `PATH=%h/.local/bin:/usr/local/bin:/usr/bin:/bin`; every session inherits the unit's
  environment, and the session shell is deliberately not a login shell, so a user whose
  `PATH` lives in `.profile`/`.zprofile` lost every tool outside those four directories
  inside ldmux. The units no longer set `PATH`; the keeper appends `~/.local/bin` and
  the conventional system directories to whatever it inherits (`ensure_path_floor`).
- **The launching terminal's identity is stripped** from a session (`TMUX`, `STY`,
  `TERM_PROGRAM`, `KITTY_*`, `VTE_VERSION`, `WEZTERM_*`, `ALACRITTY_*`, `WT_*`, …): a
  session is its own terminal, and a `TMUX` pointing at a socket it does not belong to
  makes its own `tmux` clients misbehave.
- **`SHELL` names the program that was exec'd** for a shell session (a `Spawn.command`
  such as `pi` leaves `SHELL` naming the user's shell).
- **`IUTF8` is set on the session pty.** Without it the line discipline erased one byte
  per backspace in canonical mode, so a non-ASCII character took two to four deletes
  and left half of one behind.
- **A non-UTF-8 environment variable no longer refuses every spawn.** `std::env::vars()`
  panics on one; the keeper and the desk-window launcher now use `vars_os()` and drop
  what the JSON protocol cannot carry, saying so once.
- **Scrollback caps are configurable**: `LDMUX_SCROLLBACK_LINES` and
  `LDMUX_SCROLLBACK_BYTES` in `~/.config/ldmux/env` override the 10,000-line / 40 MiB
  defaults.

Platforms (see `docs/PLATFORMS.md`):
- Only x86_64 Linux is built and run today; aarch64 Linux evaluates in the flake but is
  unbuilt, and macOS needs the engine derivation proven on darwin plus replacements for
  the `/proc` reads.
- Non-systemd Linux is intended to work, untested.

## Known gaps / follow-ups

- **Painter for the new cell attributes.** The cell model is at frame version 1
  (underline style and colour, hyperlinks, wide-glyph flags); the Android painter does not
  draw them yet.
- **Hyperlinks in the Rust snapshot replay** (`crates/vt-sys/src/snapshot.rs`): a
  reattach drops OSC 8 links that a live stream would have set.
- **SAFETY comments for the 113 `unsafe` sites** (attach, daemon, keeper, vt-sys), then
  enable clippy's `undocumented_unsafe_blocks` in `[workspace.lints.clippy]`
  (`Cargo.toml` explains why it is off).
- **The Kitty keyboard protocol is implemented, and the deployed keeper predates it**
  (ADR-0022). The keeper's emulator now answers the terminal queries a program asks
  (DA1, DSR/CPR, DECRQM, XTVERSION, the Kitty query, mode reports) through its
  write-back; the daemon encodes a modified `Enter`/`Escape`/`Space` as `CSI u` when
  the program has the protocol on; the phone latches `Shift` and sends `S-Enter`. End
  to end in `crates/daemon/tests/kitty_keys.rs`. **Until the keeper is replaced**
  (`nix run .#restart-keeper`, which ends every shell) `pi` on the phone still sees a
  terminal without the protocol and `Shift+Enter` still submits; the phone can fall
  back to the `Ctrl` latch then `j`, which `pi` also binds to a newline.
- **`App.kt` is about 2,000 lines** and wants splitting.
- **Unused selection bindings in the native engine** (`ldmux_terminal.c` / the JNI shim).
- **Real-device verification of scroll-anchor replay**: a clear-screen in the replayed
  history may push content into scrollback; checked in tests, not on a phone.
- **Keeper replacement is needed to get bounded peeks** on a machine whose keeper predates
  them; it ends every shell, so it waits for a convenient moment.
- **Mesh work** (ADR-0020, `docs/MESH-TRANSPORT.md`) is designed and unimplemented.
- **A tailnet IP appears in test fixtures** (`crates/daemon/src/pairing.rs` and others).
- **The git `user.name` is misconfigured** as `user.email`; commits carry that name.

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
