# Rust port log

The per-phase state/progress/findings record for the port (AGENTS.md PORT
MODE). The user compacts between phases; a fresh session resumes by reading
this file, then the plan §7 table, then the phase's source of record.

**Phase table** (plan §7, serial — no forks/sub-agents, ever):

| # | Status | Gate command | Gate state |
|---|---|---|---|
| 1 protocol | **done** (2026-09-28) | `cargo test -p ldmux-protocol` | **40 passed, 0 failed** (clippy: 8 non-blocking warnings, see Findings) |
| 2 pure modules | **done** (2026-09-28) | `cargo test -p ldmux-daemon` | **88 passed, 0 failed** |
| 3 keeper ∥ vt-sys ∥ attach skeletons | **done** (2026-09-28) | per §7 | keeper **4** + vt-sys **6** + attach **2** + protocol **40** + daemon **88** = **140 passed, 0 failed**; TS **347/347** |
| 4 emulator + parity | not started | parity report: zero divergences | — |
| 5 daemon | not started | full TS-equivalent suite vs keeper | — |
| 6 desktop/restart/backpressure e2e | not started | §6 risk tests green | — |
| 7 build/deploy + D4 | not started | deploy green | — |

## Findings log (durable, dated)

- **2026-09-28, port opened.** Plan committed as operative (16cd5bf);
  ADR-0015 decision superseded; PORT MODE written into the project AGENTS.md
  (local-only, no forks/sub-agents, no live-system touching, serial phases,
  this log as the compact-resume record).
- **2026-09-28, phase 1 opened and closed.** Branch `port/rust` off
  16cd5bf. The `crates/protocol` crate is built and green (40 tests). Toolchain
  note: the flake's rustup is not yet wired (plan §5, a phase-7 build-system
  item); the gate currently runs on the **system** rustup (1.95.0, on PATH
  and reachable inside `nix develop`). The flake `runtime = [ rustup ]` change
  is pending — until then the gate command is `cargo test -p ldmux-protocol`
  (system toolchain). Record it here so a fresh session does not assume the
  flake already provides cargo.

## Phase 1 — protocol (DONE 2026-09-28)

**Scope (plan §4/§7):** the `protocol` crate — frames, messages (incl. the new
keeper messages), rpc, pairing, net, paths, socket — gate `cargo test -p
ldmux-protocol`.

**Completed (all in `crates/protocol`):**
- `frames` — the 8-byte frame codec, byte-exact (type u8, flags u8, reserved
  u16 BE=0, length u32 BE), encode/decode/`decode_json`, oversized-length
  refusal, partial-tail, subslice, property round-trip. Port of
  `packages/protocol/src/frames.ts`.
- `messages` — the full client control union, a serde `#[serde(tag="type",
  rename_all="lowercase")]` enum with `rename_all="camelCase"` per message,
  field-for-field from `messages.ts` (316 lines). The one modelling choice:
  `DesktopRequest`/`DesktopMessage` both use tag `desktop`, so they are one
  `Desktop { inventory: Option<...> }` variant (absent on a request, present on
  the answer) — emitted JSON is identical both ways. Verified by test
  (`desktop_request_has_no_inventory_result_has_one`,
  `snapshot_full_wire_shape_is_camel_case`).
- `keeper` — the new daemon↔keeper messages (plan §3.5): `Spawn, Kill, Write,
  Resize, Read, Snapshot{history_lines}, State, List, ChildInfo{pid},
  ChildExited, Close`. **No TS source of record** — the names are the plan's,
  the field sets are a **provisional spec** (finalised by the keeper phase,
  plan §7 phase 3). `Write.data` is a `String` (UTF-8, as the client's
  `Input.data` is), not raw bytes, so the message stays valid JSON.
- `rpc` — the shared data types (`DeviceInfo`, `SessionOwner`, `SessionRecord`,
  `SessionInfo`, `DesktopInventory` + workspace/window/pi-session, `PaneState`,
  `PingResult`, `NewParams`, `SendParams`), the method-name const set, and
  `DAEMON_PROTOCOL_VERSION`.
- `pairing` — `pairing_link`/`parse_pairing_link`/`split_host`, a byte-faithful
  `encodeURIComponent` + `URLSearchParams`-style decode (no `url` dep). Reads
  the **shared** TS fixture
  `packages/protocol/test/fixtures/pairing-links.json` (all three sides read it).
- `net` — `classify_address`/`is_tailnet_or_loopback` with a local IPv4/IPv6
  parser (no `node:net` BlockList); the TS test's classification table, ported
  1:1.
- `paths` — `state_dir`/`socket`/`tmux_socket` (the `tmux_socket` is marked
  **transitional** — the tmux server leaves with the port, §8/§9).
- `socket` — the newline-JSON Unix-socket `call` client (tokio), port of
  `socket-client.ts`: one call == one connection == one request/response line,
  timeout, error/refused/closed handling.

**Pending inside the phase:** none — phase complete. (See Open decisions.)

**Gate:** `cargo test -p ldmux-protocol` → **40 passed, 0 failed** (system
rustup 1.95.0, inside `nix develop`). TS gate re-run green: `node --run test`
347/347, `node --run typecheck` clean (the TS tree is untouched by this
phase).

**Open decisions / notes (for the next session):**
- **Typed client:** the Rust `socket::call` (generic over params/result) **is** the
  typed client; the TS's per-method `daemonCall` table is not mirrored as
  per-method wrapper functions (that surface is a CLI/daemon concern, phase 5).
  The method names live in `rpc::method`. Flag if a phase-5 consumer wants the
  per-method wrappers — they are a thin addition.
- **Clippy (non-blocking, per AGENTS):** 8 warnings — 3× manual ascii-range check
  (`pairing::is_unreserved`), 1× manual `!RangeInclusive::contains`
  (`pairing`), 4× collapsible `if`-in-`match` (`pairing` decode). Cosmetic; fix in
  a later pass or leave.
- **Edition 2024** requires rust ≥1.85; the system toolchain is 1.95.0, the
  flake's rustup (when wired) will be ≥ that. No action until the flake change.

## Phase 2 — pure modules (DONE 2026-09-28)

**Scope (plan §7):** the pure, dependency-free daemon modules, each a 1:1
port of its named `packages/daemon/src/…` file with its TS test ported 1:1.
Crate: `crates/daemon` (package `ldmux-daemon`), depends on `ldmux-protocol`.
Gate `cargo test -p ldmux-daemon` → **88 passed, 0 failed** (system rustup
1.95.0, inside `nix develop`). Clippy on the daemon crate: **0 warnings**.

**Modules (all in `crates/daemon/src/`):**
- `auth` (10 tests) — `load_or_create_token`/`token_matches`; 0600 token file,
  constant-time compare, the legacy-path move/adopt rule. Port of `auth.ts`.
- `qr` (5 tests) — full ISO/IEC 18004 encoder (byte mode, v1–40, L/M/Q/H):
  GF(256)/Reed–Solomon, block interleave, placement, 8-mask penalty, plus
  `render_qr_terminal` (xterm-256 half-block, explicit dark/light) and
  `render_qr_pbm`. Port of `qr.ts`. **The tables are the load-bearing part** —
  see Findings. Correctness is pinned by decoding every size back with
  `zbarimg` (the 160-code sweep), not by comparing bits to a second encoder.
- `sanitize` (16 tests) — the stateful pane-byte sanitizer (OSC 52, DCS/APC,
  DA/DSR) FSM. Port of `sanitize.ts`.
- `screen_diff` (10 tests) — "what changed on screen" from two captures,
  `UpdateKind` enum. Port of `tmux/screen-diff.ts`.
- `ownership` (16 tests) — who is looking at a session (ADR-0010): claim /
  re-claim / takeover / release / retain, `seizing_closes_the_desk_window`.
  Port of `ownership.ts`.
- `registry` (10 tests) — the durable session index: typed `load` (drops a
  record that does not shape into `SessionRecord`), atomic `save`, two-pass
  `prune`, `reconcile`. Port of `registry.ts`.
- `cwd` (8 tests) — `resolve_cwd`/`home_dir`; `None` covers absent/null/
  non-string. Port of `cwd.ts`. `$HOME` read at call time; a clear panic when
  unset (divergence from the TS passwd fallback — untestable edge, logged).
- `exec` (7 tests) — `run`: async, both pipes drained concurrently (a full pipe
  deadlocks otherwise), bounded; SIGTERM at the deadline then SIGKILL after the
  grace (a SIGTERM alone is not a bound). Port of `exec.ts`.
- `proc` (0 tests — exercised through `exec`) — `spawn_captured` (tokio +
  `setsid` pre_exec, own process group), `kill_group` (negative-pid group kill
  with single-pid fallback), `wait_drained` (wait **while** draining both pipes
  concurrently — the ordering is the point). Port of `proc.ts`.
- `pairing` (6 tests) — `pairing_for`/`ListenInfo`/`PairingProblem` for the
  address this daemon listens on. Port of `pairing.ts` (the *daemon-side*
  pairing, distinct from the protocol-crate `pairing` done in phase 1).

**Pending inside the phase:** none — phase complete.

**Gate:** `cargo test -p ldmux-daemon` → **88 passed, 0 failed**. TS gate re-run green: `node --run test` 347/347,
`node --run typecheck` clean (the TS tree is untouched by this phase).

**Findings / decisions (this phase):**
- **The QR ECC tables were transcribed by hand and drifted** (a 45-element row
  where the source has 40). Regenerated **both** `ECC_CODEWORDS_PER_BLOCK` and
  `ECC_BLOCKS` programmatically from `packages/daemon/src/qr.ts` (the TS
  source-of-record), asserting each of the 8 rows is exactly 41 elements. The
  index-0 sentinel is `-1` in TS (unrepresentable in `u32`); it is `0` here and
  never read (indexing starts at version 1). This is the one place a silent
  transcription error would have shipped a mis-encoded QR that still "passed"
  its own render test — the `zbarimg` decode sweep is what catches it.
- **`with_ecc` block-length direction.** TS: short blocks get
  `shortLen - eccLen`, the *later* blocks get `+1` (`i < shortBlocks ? 0 : 1`).
  The first draft had the ternary backwards; a 1-line fix, but it is exactly the
  kind of error the per-version decode sweep exists to catch.
- **`draw_codewords` zig-zag underflow.** TS `right -= 2` is fine at
  `right = 1` (JS: `1 - 2 = -1`, loop ends). Rust `u32` underflowed. Guarded
  with `if right <= 2 { break; }`. Same class in `alignment`:
  `pos.len() - 1` for version 1 (no alignment patterns) → `saturating_sub`.
- **`run` must drain while it waits.** The child cannot exit until its pipes
  are drained, so `wait().await` *then* `drain()` deadlocks the moment a stream
  exceeds the kernel buffer (the 1 MiB test). `Spawned::wait_drained` drives
  the two pipe reads and the exit wait in one `tokio::join!`.
- **`proc` group-kill uses a negative pid** (`kill(-pid)`), the child being in
  its own session from `setsid`; the single-pid fallback covers a reaped pid.
  The `pre_exec` closure returns `Result<(), io::Error>` in tokio 1.53 (not an
  `i32`), so `setsid`'s `Result` is mapped, not returned.
- **Divergences (untestable edges, logged):** `cwd::home_dir` reads `$HOME`
  and panics with a clear message when unset (TS has a passwd fallback);
  `sanitize`'s overflow guard is byte-based (Rust) vs UTF-16-unit-based (TS) —
  stricter for multibyte; the pairing name trim counts Rust chars vs JS UTF-16
  code units (same count for the inputs that matter).
- **Clippy:** the daemon crate is **clean** (0 warnings). The `rand::rng()`
  rename (was the deprecated `thread_rng`) was taken; no new runtime deps were
  added beyond the allowlist (`serde`, `serde_json`, `tokio`, `uuid`, `nix`,
  `rand` — `rand`/`nix` are used only by `auth`/`proc`/`exec`).

## Phase 3 — keeper ∥ vt-sys ∥ attach skeletons (DONE 2026-09-28)

**Scope (plan §7, workstreams C ∥ D ∥ E):** the keeper (PTY + supervision +
server skeleton), `vt-sys` (FFI against the vendored headers), and the
attach-client skeleton (pty pair + protocol loop). Gate (plan §7): "keeper
boots, serves `List`/`Spawn`; FFI links the vendored `.a`; attach boots
headless." **All three met.** New crate: `crates/keeper`, `crates/vt-sys`
(done in the prior vt-sys commit), `crates/attach`; protocol gains
`Peek`/`PeekResult` (the attach's stream pull).

**Gate:** full workspace `cargo test --workspace` → **140 passed, 0 failed**
(attach 2, daemon 88, keeper 4, protocol 40, vt-sys 6). TS gate re-run green:
`node --run test` 347/347, `node --run typecheck` clean (the TS tree is
untouched by this phase). Clippy on the new crates (`keeper`, `attach`,
`protocol`): **0 warnings**. (The pre-existing Phase-2 `daemon` crate still
carries its clippy warnings — out of scope here; the daemon gate is tests,
not clippy.)

**Keeper (`crates/keeper`, package `ldmux-keeper`, lib `keeper`, bin
`ldmux-keeper`)** — the tmux-server position (plan §3). It owns a PTY per
session, spawns the shell into it, supervises it, **always reads the master**
(the kernel pty buffer never fills from a slow reader), and serves the keeper
protocol over a Unix socket (one tagged `KeeperMessage` per line, one request
line → one response line, one call == one connection). State is one
`Mutex<HashMap<id, Session>>`; the reader is a `spawn_blocking` task that
reads the master into a per-session `VecDeque` buffer.
- `spawn_session` — `openpty` → `try_clone` the slave ×3 (the child's 0/1/2),
  `setsid` pre_exec (own session, so group-kill takes the tree), slave as the
  child's stdio, parent drops its slave copy. The reader task reads the master
  into `Session.out`.
- `group_kill` — `kill(-pid, SIGTERM)`, 50 ms grace, `kill(-pid, SIGKILL)`.
- `reap` — `waitpid(-1, WNOHANG)` loop (the authoritative death signal, not the
  master EOF); `StillAlive` breaks, `Exited`/`Signaled` marks the session dead.
- handlers: `Spawn`, `Kill`, `Write` (raw `write` to the master), `Resize`
  (`TIOCSWINSZ` on the master), `Read`/`Snapshot` (→ `State`), `List`, `Peek`
  (drain the buffer), `Close`.
- `serve` — `UnixListener` accept loop, one `serve_conn` task per connection
  (read one JSON line, `handle`, write one JSON line).
- tests (4): boot + empty `List`; `Spawn` a real `/bin/sh`, `List`, `Write`
  `printf`, prove the keeper reads the master (buffered > 0), `Read` (alive),
  `Kill` (dead, list empty); `Resize` (state 120×40) + `Kill`; unknown session
  → error answer.
- **The `resize` test hung until it `Kill`ed its session** — see Findings (the
  reader thread wedged in `read(pty)` blocks the runtime drop on exit).

**vt-sys (`crates/vt-sys`)** — done in the prior commit (`727114a`): `build.rs`
links the vendored `libghostty-vt.a` (`static=ghostty-vt`, `-lm -lpthread`);
`src/ffi.rs` is the `extern "C"` binding (`ghostty_terminal_new/free/set/
vt_write/resize/get/get_multi`, the `GhosttyResult`/`GhosttyTerminalData`/
`GhosttyTerminalOption` constants, the opaque `GhosttyTerminal`); `src/safe.rs`
is a `Terminal` (owns the C handle, `!Send`/`!Sync`, `Drop` frees it, the
accessors + `write`/`resize`); `tests/engine.rs` (6 tests) proves the FFI links
and the round-trip works. The vendored `.a` (20 MB, x86_64-linux-gnu) is at
`packages/mobile/native/prebuilt/host/lib/`.

**Attach (`crates/attach`, package `ldmux-attach`, lib `attach`, bin
`ldmux-attach`)** — the desktop window client skeleton (plan §3.9). In
production it is `alacritty -e ldmuxd attach <id>` (its stdio **is**
alacritty's pty — no nested pty); here it is the skeleton: two fds for its
local terminal (where it reads user input, where it writes session output),
connects to the keeper, and pipes. `Attach::connect` does the `Read`
handshake (→ `State`, errors if the session is dead/unknown); `send_input`
(→ `Write`); `read_output` (→ `Peek`, the stream pull); `write_output` (write
to the local terminal); `read_input` (non-blocking poll of the local
terminal). Tests (2): boot headless + pipe a session's bytes (send
`printf 'ldmux-hi\n'`, poll the output until it contains `ldmux-hi`); reject
an unknown session.

**Protocol addition (`Peek`/`PeekResult`):** the attach's stream direction —
drain the session's buffered output bytes. `Peek { session_id }` →
`PeekResult { session_id, data: Vec<u8> }`. The keeper always reads the master
into the buffer, so a peek is a bounded, non-blocking pull (the skeleton's
encoding is a JSON byte array; a framed binary stream replaces it when the
stream goes live).

**Findings / decisions (this phase):**
- **A keeper/attach test that spawns a session must `Kill` it at the end.**
  The reader task does a *blocking* `read(pty_master)`; while a session is
  alive the master has no data (the shell is idle), so the read blocks, and a
  `#[tokio::test]` cannot drop its runtime while a blocking-pool thread is
  wedged in that syscall — the process hangs on exit (it is *not* a hang in
  the assertions; the assertions pass). The fix is to `Kill` the session in
  the test (the shell exits, the master gets EOF, the reader returns). The
  same rule applies to any future keeper/attach test that leaves a session
  live. (Caught by a `/proc/<pid>/task/*/syscall` dump: the wedged thread was
  `read(fd, 4096)`.)
- **`nix::pty::openpty` returns an `OpenptyResult` struct, not a tuple** —
  `let opened = openpty(...)?; let master = opened.master; let slave =
  opened.slave;` (a `(master, slave)` destructure fails).
- **`Stdio` takes the pty slave by value** (`From<OwnedFd>`), so the child's
  0/1/2 are each a `try_clone` of the slave; the parent's copy is dropped
  after spawn. (`&OwnedFd`/`&BorrowedFd` do **not** satisfy `Into<Stdio>`.)
- **`OwnedFd`/`BorrowedFd` fd access:** `OwnedFd` has no inherent
  `as_fd()->c_int`; use `AsRawFd::as_raw_fd()` for the raw number (the
  `TIOCSWINSZ` ioctl, the raw `read`/`write`), and `AsFd::as_fd()` for a
  `&BorrowedFd`. Raw `libc::read`/`write` are used for the master (the std
  `Read`/`Write` on `&BorrowedFd` did not method-resolve).
- **`waitpid(-1, WNOHANG)` is the non-blocking reap** — `nix::sys::wait::
  waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG))` returns
  `StillAlive` immediately (no child to reap); it does **not** block (the
  earlier "blocking waitpid" reading was the wedged *reader* `read`, not the
  `reap`).
- **`Peek` drains via `iter().copied().collect::<Vec<u8>>()`** (a `VecDeque`
  does not `Into<Vec>` directly).
- **Clippy:** the new crates are clean (0 warnings). The pre-existing
  Phase-1 `protocol` warnings (`pairing`, `frames`, `messages`) were also
  fixed this pass, so `ldmux-protocol` is now **0 warnings** too.
- **No live system touched.** The keeper/attach run in-process against a
  scratch socket + a scratch PTY; no live daemon, no `just install`, no
  `systemctl`, no device.

## Phase 4 — emulator + parity (OPENED 2026-09-28)

**Scope (plan §7):** keeper emulator integration (per-session ghostty-vt), the
snapshot renderer (emit tmux-`capture-pane -e` format), the state fields,
scrollback paging, and the §3.11 parity harness run against a (scratch) tmux
server. Gate: **parity report: zero divergences** (or a defect list with
reproducers).

**D2 finding (amends the plan's D2 decision, 2026-09-28):** the D2 decision
said "the C surface is one generic accessor — `ghostty_terminal_get` /
`get_multi` keyed by the `GhosttyTerminalData` enum." That is **true for the
state fields** (cursor, colors, active-screen, mouse — all scalar `get`
values), but it **under-binds the snapshot renderer**. The grid/cell access is
a **separate** API in `render.h` (`ghostty_render_state_update` → a
`GhosttyRenderState`, then a row iterator → a row-cells iterator, each a
`new`/`next`/`get`/`free` group over opaque handles), **not** a key in
`GhosttyTerminalData`. So the `-sys` binds the terminal's `get`/`get_multi`
*and* the render-state row/cell accessors. The cells are still the source of
truth; the renderer emits the `capture-pane` format itself (plan §3.4 /
§3.11.3). The cell's `GhosttyStyle` (fg/bg/underline colors as tagged unions,
bold/italic/faint/blink/inverse/invisible/strikethrough/overline, underline
kind) is what the SGR encoder consumes.

**Location decision:** the SGR encoder (style → SGR sequence) and the renderer
(cells → `capture-pane` format) are **pure functions**; they live in the
`daemon` crate (`crates/daemon/src/sgr.rs`, `crates/daemon/src/snapshot.rs`),
take plain-Rust cell structs (not the C types), and are testable without the
C library. The keeper holds the C render state, iterates the cells, and hands
the extracted cell data to the renderer.

**Render-state FFI milestone (2026-09-28, committed):** the `-sys` now binds
`render.h` (`GhosttyRenderState` + the row/cell iterators +
`ROW_CELLS_DATA_STYLE`/`_GRAPHEMES_UTF8`), and the `RenderState` wrapper
(`safe.rs`) iterates a row into one plain `Cell` per column. **Key finding:
the C `GhosttyStyle` is 72 bytes** (unpacked, 8-byte aligned — the colour
value is an 8-aligned union, so `GhosttyStyleColor` is 16 bytes: tag @0,
value @8), **not** 56. The C `ROW_CELLS_DATA_STYLE` `@alignCast`s its `out`
pointer, so it needs an **8-byte-aligned 72-byte buffer** — a packed Rust
mirror (56) is UB (heap corruption). We allocate the C size (72) and read
raw bytes at the measured offsets (fg @8, bg @24, underline @40, the eight
attributes @56..64, underline kind @64). **The attributes ARE reflected** in
`ROW_CELLS_DATA_STYLE` (a bold/italic/inverse/strikethrough/underline cell
reads back correctly); an earlier "attribute gap" was a misread of bytes
44–51 (the `underline_color`), not the attributes. A cell with no explicit
colour gets a sentinel tag (neither PALETTE=1 nor RGB=2), mapped to `None`.
Gate: `cargo test -p ldmux-vt-sys` → 8 pass (was 6).
