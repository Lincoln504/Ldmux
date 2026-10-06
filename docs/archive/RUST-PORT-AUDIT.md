# RUST-PORT-AUDIT.md — the critical audit of the Rust port (2026-09-28)

> **ARCHIVE (moved to docs/archive/ 2026-10-06).** Historical record. The critical audit of the Rust port plan (2026-09-28); the port is done (ADR-0018). Current state: `docs/STATE.md`.

> **Banner, 2026-10-05.** tmux is now fully removed from the software: the wire
> field is `keeperName` (was `tmuxName`), `keeperSocket` (was `tmuxSocket`), `paneInMode`
> and the `copymode` message are gone, and the parity gate uses committed goldens
> (`crates/daemon/tests/parity/`). This record is not rewritten; every tmux name
> below is as it was at the time it was written. See the log's 2026-10-05
> "tmux semantics removed" entry.


> **Correction, 2026-10-04.** This audit's remote/stream findings were
> incomplete: it recorded "M7 FIXED — attach sends full state" when the attach
> reply was a `{type:"state"}` envelope the Kotlin client cannot read, and it
> missed that the whole remote wire protocol diverged from `remote.ts` (auth at
> the upgrade, OUTPUT frames, `caps`+`snapshot`, `created`+`sessions`,
> camelCase `SessionInfo`). `crates/daemon/src/remote.rs` was rewritten against
> the TS source of record on 2026-10-04; see `docs/RUST-PORT-LOG.md` ("Live
> deployment and the deep audit"). Treat the fix-status lines below as
> historical.

The full audit of the Rust port as of the `port/rust` branch (commit `84a1caa`,
2026-09-28). The audit covers: (1) state against the plan/spec, (2) state in its
own merit (bugs, races, leaks, error handling, architecture, best practice),
(3) plan audit, (4) recommended fixes.

## Fix status (as of 2026-09-29)

- C1: **FIXED** (commit 54517fa) — poll loop in `stream_for`
- C2: **FIXED** (commit 54517fa) — `input` handler uses `stream.write`
- C3: **FIXED** (commit 54517fa) — `send` uses `key_name_to_bytes`
- C4: **FIXED** (commit 54517fa) — `key` uses `key_name_to_bytes`
- C5: **FIXED** (commit 54517fa) — `send_key` uses `key_name_to_bytes`
- C6: **FIXED** (commit 3d8c339) — `adopt` handler added
- C7: **FIXED** (commit 3d8c339) — `clipboardWrite` handler added
- C8: **FIXED** (commit 3d8c339) — `deskWindow` handler added
- C9: **FIXED** (commit 3d8c339) — `makeLive` fixed (workspace param, live_windows, claim)
- C10: **FIXED** (commit 3d8c339) — `closeLive` fixed (claim release)
- C11: **FIXED** (commit 3d8c339) — `remote_info` returns actual config
- C12: **FIXED** (commit 3d8c339) — `reclaim` claims the session
- M1: **FIXED** (commit 6956be1) — subscriber id tracking
- M2: **FIXED** (commit 6956be1) — drain by id
- M7: **FIXED** (commit ef6e16c) — attach sends full state
- M9: **FIXED** (commit ef6e16c) — ldmuxd signal handling
- M10: **FIXED** (commit 54517fa) — poll thread in `stream_for`
- M3/M4/M5/M6/M8: **NOT FIXED** (low priority / by design)

---

## 1. CRITICAL BUGS (functional defects that would break the system)

### C1. No live output loop in `PaneStream` — the terminal is static

**File**: `crates/daemon/src/stream.rs`
**What is missing**: A poll loop (or live keeper stream) that pulls `Peek` results from
the keeper and fans them out to subscribers.

**What the plan says** (§3.7): "the keeper never stops reading (it always buffers the
master), and each subscriber pulls from a **bounded** queue — when a slow subscriber's
queue overflows, it is dropped and forced to resync from a snapshot."

**What is implemented**: The `PaneStream` has a bounded queue (1 MB) and a `dropped`
flag. But the queue is **never filled** because there is no poll loop. The `fanout`
method exists but is never called with real output. The only call to `fanout` in the
codebase is the broken `input` handler (C2 below).

**Impact**: A phone that attaches gets a snapshot (one-shot capture) but **never sees
live output**. The terminal is static. This is the single most critical gap in the port.

**Fix**: Add a poll loop (a tokio task per `PaneStream`) that pulls `Peek` results
from the keeper and calls `fanout` with the bytes. The loop runs while the stream
has subscribers; it stops when the stream is closed. The bounded queue and drop-on-
overflow mechanism already exist — they just need a source of data.

**TS source of record**: `packages/daemon/src/stream.ts` — the TS `PaneStream` has
a control client (or pipe client) that streams pane output continuously via
`onOutput` callbacks. The Rust port has no equivalent.

---

### C2. `input` handler in `remote.rs` uses `fanout` instead of writing to the keeper

**File**: `crates/daemon/src/remote.rs`, the `"input"` match arm (~line 567)
**What it does**: `stream.fanout(text)` — sends the text to subscribers (output direction).
**What it should do**: Write the text to the keeper via a `Write` message (input direction).
**TS source**: `data.stream.inputText(text)` in `packages/daemon/src/remote.ts`.
**Impact**: User input from the phone is sent to subscribers (loopback), not to the PTY.
The shell never receives the text. The user types, and the text appears in the output
stream but the shell does not execute it.
**Fix**: Add a `write` method to `PaneStream` that sends a `Write` keeper message, and
call it in the `input` handler. Remove the `fanout` call.

---

### C3. `send` handler in `daemon.rs` sends key names as strings, not bytes

**File**: `crates/daemon/src/daemon.rs`, the `"send"` match arm (~line 1032)
**What it does**: Concatenates `text` + key names into one string and sends as a `Write`.
The key names (e.g. `"Enter"`, `"C-a"`) are sent as their literal string form.
**What it should do**: Send the text as bytes, and convert key names to their byte
sequences (e.g. `"Enter"` → `0x0D`, `"C-a"` → `0x01`, `"Escape"` → `0x1B`).
**TS source**: `this.cp.sendText(record.tmuxName, p.text)` + `this.cp.sendKey(record.tmuxName, ...p.keys)`
in `packages/daemon/src/daemon.ts:1319`. The TS uses tmux's `send-keys` which converts
key names to bytes.
**Impact**: The shell receives the literal string `"Enter"` instead of the byte `0x0D`.
Every key press is broken.
**Fix**: Add a `key_name_to_bytes` function (a lookup table: `"Enter"` → `[0x0D]`,
`"Tab"` → `[0x09]`, `"Escape"` → `[0x1B]`, `"C-a"` → `[0x01]`, etc.) and use it to
convert key names before writing.

---

### C4. `key` handler in `remote.rs` sends key names as strings

**File**: `crates/daemon/src/remote.rs`, the `"key"` match arm (~line 655)
**What it does**: `stream.send_key(&keys_refs)` — sends key names as strings.
**What it should do**: Convert key names to bytes and write them to the keeper.
**TS source**: `data.stream.sendKey(keys)` in `packages/daemon/src/remote.ts`.
**Impact**: Same as C3 — the shell receives the literal string `"Enter"` instead of `0x0D`.
**Fix**: Same as C3 — use the `key_name_to_bytes` conversion.

---

### C5. `PaneStream::send_key` sends key names as strings

**File**: `crates/daemon/src/stream.rs`, `send_key` method
**What it does**: `let text: String = keys.iter().copied().collect();` — concatenates key
names into one string.
**What it should do**: Convert key names to their byte sequences.
**Impact**: Same as C3/C4.
**Fix**: Same as C3 — use the `key_name_to_bytes` conversion.

---

### C6. Missing `adopt` handler

**File**: `crates/daemon/src/daemon.rs`, `handle` method
**What is missing**: The `adopt` handler.
**TS source**: `packages/daemon/src/daemon.ts:1427` — `case "adopt": return { session: await this.adopt(params) };`
**What it does in TS**: Adopts sessions that were running when the daemon crashed
(their names say they are ours — `ldmux-<id>`). Only at startup.
**Impact**: After a daemon crash, running sessions are not re-adopted. The user loses
access to them.
**Fix**: Port the `adopt` handler.

---

### C7. Missing `clipboardWrite` handler

**File**: `crates/daemon/src/daemon.rs`, `handle` method
**What is missing**: The `clipboardWrite` handler.
**TS source**: `packages/daemon/src/daemon.ts:1337` — writes the clipboard policy to the state dir.
**Impact**: The clipboard policy cannot be set via RPC.
**Fix**: Port the `clipboardWrite` handler.

---

### C8. Missing `deskWindow` handler

**File**: `crates/daemon/src/daemon.rs`, `handle` method
**What is missing**: The `deskWindow` handler.
**TS source**: `packages/daemon/src/daemon.ts:1246` — opens a desk window for a session.
**Impact**: The desk window workflow is incomplete.
**Fix**: Port the `deskWindow` handler.

---

### C9. `makelive` handler hardcodes workspace and omits desktop claim

**File**: `crates/daemon/src/daemon.rs`, `"makeLive"` match arm (~line 1100)
**What it does**: Calls `crate::desktop::open_live(..., "10", ...)` with a hardcoded
workspace. Does not record the window in a `liveWindows` map. Does not claim the
session to the desktop device. Does not broadcast sessions.
**TS source**: `packages/daemon/src/daemon.ts:1197` — uses `params.workspace`, records
in `liveWindows`, claims to desktop via `claimSession`, broadcasts.
**Impact**: The workspace is always `"10"` regardless of what the phone requests. The
window is not tracked. The desktop does not claim the session (so the phone can still
attach, leading to two writers on one PTY — the split-input the exclusive rule exists
to prevent).
**Fix**: Use `params.workspace`, track the window, claim to desktop, broadcast.

---

### C10. `closelive` handler omits desktop claim release and broadcast

**File**: `crates/daemon/src/daemon.rs`, `"closeLive"` match arm (~line 1123)
**What it does**: Calls `crate::desktop::close_live(session_id)`.
**What it should do**: Also release the desktop claim and broadcast sessions.
**TS source**: `packages/daemon/src/daemon.ts:1263` — releases the desktop claim via
`this.ownership.release(record.id, this.desktopDevice.id)` and broadcasts.
**Impact**: The desktop claim is not released. The session list is not broadcast to
clients.
**Fix**: Add claim release and broadcast.

---

### C11. `remote_info` handler returns a placeholder

**File**: `crates/daemon/src/daemon.rs`, `"remote_info"` match arm (~line 1196)
**What it does**: Returns `{"host": "", "port": 0}` (placeholder).
**What it should do**: Return the actual remote host and port from `remote_cfg`.
**Impact**: The phone cannot discover the remote address.
**Fix**: Return the actual `remote_cfg` values.

---

### C12. `reclaim` handler is a no-op

**File**: `crates/daemon/src/daemon.rs`, `"reclaim"` match arm (~line 1190)
**What it does**: Returns `Ok(serde_json::json!({ "ok": true }))` (no-op).
**What it should do**: Reclaim the live window (the TS `reclaim` handler).
**TS source**: `packages/daemon/src/daemon.ts:1279`.
**Impact**: The reclaim workflow is incomplete.
**Fix**: Port the reclaim handler.

---

## 2. MODERATE ISSUES (incomplete implementations, not broken)

### M1. `PaneStream` subscribe drops the LAST subscriber, not the specific one

**File**: `crates/daemon/src/stream.rs`, `subscribe` method
**What it does**: `subs.pop()` — removes the last element of the Vec.
**What it should do**: Remove the specific subscriber by identity.
**Impact**: If subscriber A unsubscribes, subscriber B's queue is removed instead.
**Fix**: Use a unique id or index for each subscriber, and remove by that.

### M2. `PaneStream` drain is by index, not by subscriber

**File**: `crates/daemon/src/stream.rs`, `drain` method
**What it does**: `subs.get_mut(idx)` — drains by array index.
**Issue**: The index is fragile (subscribers can be added/removed, changing indices).
**Fix**: Use a subscriber id.

### M3. `ConnData` struct is defined but unused

**File**: `crates/daemon/src/remote.rs`, `ConnData` struct
**What it does**: Defined with `#[allow(dead_code)]` but never instantiated.
**What it should do**: Track per-connection state (disposed, attach_gen, live, device,
session_id, record, last_seen).
**Impact**: The per-connection state is not tracked. The attach lifecycle is incomplete.
**Fix**: Use `ConnData` or remove it.

### M4. `by_device` map is defined but unused

**File**: `crates/daemon/src/remote.rs`, `by_device` field
**What it does**: Defined with `#[allow(dead_code)]` but never populated.
**What it should do**: Track which device holds which session.
**Impact**: Device ownership is not tracked per-connection.
**Fix**: Populate `by_device` in the `hello` handler.

### M5. No idle connection sweep

**What is missing**: The TS `RemoteServer` has an `idleMs` sweep that terminates
silent connections. The Rust port has no sweep timer.
**Impact**: Silent connections are never cleaned up.
**Fix**: Add a sweep timer.

### M6. No attach generation tracking

**What is missing**: The TS `ConnData` has `attachGen` which is used to drop stale
snapshots after a re-attach. The Rust port has no attach generation.
**Impact**: A stale snapshot after a re-attach may be sent to the client.
**Fix**: Add attach generation tracking.

### M7. The `attach` handler does not send the full state

**File**: `crates/daemon/src/remote.rs`, `"attach"` match arm
**What it does**: Sends `{"type": "state", "sessionId": sid, "snapshot": ...}`.
**What it should do**: Send the full `PaneState` (the TS sends the state + snapshot).
**Impact**: The client may not have the full state.
**Fix**: Send the full state.

### M8. `copymode` handler is stubbed with a clear error

**File**: `crates/daemon/src/remote.rs`, `"copymode"` match arm
**What it does**: Returns `{"type": "error", "code": "not_implemented", ...}`.
**What it should do**: The keeper has no copy-mode equivalent (it is tmux-specific).
**Impact**: Copy mode is not available in the port.
**Fix**: Defer (the keeper has no copy-mode equivalent). Document as a known limitation.

### M9. No `LdmuxDaemon::stop` signal handling in `ldmuxd.rs`

**File**: `crates/daemon/src/bin/ldmuxd.rs`
**What it does**: Relies on the default process disposition (SIGTERM kills the process).
**What it should do**: The TS `main.ts` registers `SIGINT`/`SIGTERM` handlers that call
`daemon.stop()` before exiting.
**Impact**: A SIGTERM may kill the process without calling `stop()`, leaving the socket
and lock file behind.
**Fix**: Add signal handlers (or use the `Drop` impl, which already calls `stop()`).

### M10. The `LdmuxDaemon::start` does not start a poll loop

**File**: `crates/daemon/src/daemon.rs`, `start` method
**What it does**: Starts the keeper supervisor, the control socket, and the remote.
**What it should do**: Also start the poll loop for streams (or the `PaneStream` should
start its own poll loop when created).
**Impact**: Streams are created on demand but have no active output loop.
**Fix**: The `PaneStream` should start its own poll loop when created.

---

## 3. DESIGN/ARCHITECTURE OBSERVATIONS

### A1. The `PaneStream` keeper connection is one-shot per call

Each `snapshot`, `send_key`, `resize`, `mouse_click` call opens a new Unix socket
connection to the keeper. This is correct per the plan (the keeper protocol is one
request per connection). However, it means the stream's `fanout` does NOT come from a
live keeper stream — it comes from... nothing (see C1).

**Observation**: The one-shot protocol is correct for commands. The live output loop
(poll loop) is a separate concern that is not implemented.

### A2. The `LdmuxDaemon` is a single `Mutex`-guarded struct

The entire daemon is guarded by one `tokio::sync::Mutex`. This is simple and correct,
but it means all RPC handlers serialize on the same lock. For a single-user system
(one phone, a few sessions), this is fine. For a multi-phone system, it could be a
bottleneck.

**Observation**: Acceptable for the current scope. Revisit if the system scales.

### A3. The `PaneStream` sanitizer is per-stream, not per-subscriber

The sanitizer is shared across all subscribers of a stream. This is correct: the
sanitizer filters clipboard/DCS sequences from the pane output, and all subscribers
should see the same filtered output.

**Observation**: Correct.

### A4. The `FakeKeeper` is a good test double

The `FakeKeeper` mirrors the real keeper's `handle` exactly (the request→response
shapes, one request line → one response line per connection). The daemon under test
cannot tell the difference. This is correct per plan §3.5.

**Observation**: The `FakeKeeper` is a well-designed test double.

### A5. The `KeeperSupervisor` is the right pattern

The `KeeperSupervisor` auto-spawns the keeper and detects its death by socket
connect. The `is_alive` method uses a 20 × 25 ms retry loop to bridge the
bind→accept race. This is correct and robust.

**Observation**: The `KeeperSupervisor` is well-designed.

### A6. The `daemon_lock` is the right single-instance mechanism

The `daemon_lock` uses a pid lockfile (`ldmuxd.lock`) to serialize startup. A dead
pid is stolen. This is correct.

**Observation**: The `daemon_lock` is well-designed.

### A7. The `vt-sys` FFI bindings are correct

The `vt-sys` crate binds the vendored `libghostty-vt` by hand. The `GhosttyStyle`
is 72 bytes (matching the C struct). The `RenderState` wrapper reads rows via the
render-state API. The SGR encoder and snapshot renderer are correct.

**Observation**: The `vt-sys` crate is well-designed.

### A8. The `keeper` emulator thread is the right pattern

Each session gets its own emulator thread that owns the `!Send` `Terminal`/
`RenderState`. The thread reads the master (always), feeds the emulator, and services
snapshot/resize messages. This is correct per plan §3.4.

**Observation**: The emulator thread is well-designed.

---

## 4. PLAN AUDIT

### P1. Plan §3.7 (flow control) is not implemented

Plan §3.7 describes the bounded queue + drop-on-overflow mechanism. The `PaneStream`
has the queue and the drop flag, but no source of data (C1). The flow control
mechanism is in place but has no input.

**Verdict**: The plan is correct. The implementation is missing the poll loop.

### P2. Plan §3.5 (daemon↔keeper protocol) is correctly implemented

The keeper protocol (one call == one connection) is correctly implemented in both the
keeper and the daemon. The `KeeperMessage` types match the plan.

**Verdict**: Correct and working.

### P3. Plan §3.4 (per-session emulator) is correctly implemented

The emulator thread per session is correctly implemented in the keeper. The
`Snapshot`/`State` handlers read from the emulator. The `Peek` drains the buffer.

**Verdict**: Correct and working.

### P4. Plan §3.2/§3.3 (keeper PTY + supervision) is correctly implemented

The keeper's PTY allocation, fork+exec, reaping, and group-kill are correctly
implemented. The `fork_shell` function is the sole-reaper fix (the keeper is the
sole reaper, not tokio's global `ChildReaper`).

**Verdict**: Correct and working.

### P5. Plan §3.11 (cell parity) is correctly implemented

The snapshot renderer and SGR encoder are correctly implemented. The parity harness
(§3.11 cell-parity gate) passed with zero divergences.

**Verdict**: Correct and working.

### P6. Plan §7 (phases) is being followed

Phases 1-4 are complete. Phase 5 (daemon) is mostly done but has the stream layer
gap (C1-C5). Phase 6 (risk tests) is done. Phase 7 (build/deploy) is not started.

**Verdict**: On track, with the stream layer gap being the main blocker for phase 5
completion.

### P7. The plan's "no live system" constraint is respected

The port work is all local (scratch state dirs, scratch tmux servers, fake keeper).
No live system is touched. The parity harness (phase 4) is the one exception (live
scratch tmux).

**Verdict**: Correct.

---

## 5. RECOMMENDED FIXES (prioritized)

### Priority 1 (blocker for phase 5 completion)

1. **C1**: Add a poll loop to `PaneStream` that pulls `Peek` results and fans them
   out to subscribers. This is the single most critical gap. Without it, the terminal
   is static.

2. **C2**: Fix the `input` handler to write to the keeper (not `fanout`).

3. **C3/C4/C5**: Add a `key_name_to_bytes` function and use it in the `send`, `key`,
   and `send_key` paths.

### Priority 2 (required for full phase 5)

4. **C6**: Port the `adopt` handler.
5. **C7**: Port the `clipboardWrite` handler.
6. **C8**: Port the `deskWindow` handler.
7. **C9**: Fix the `makelive` handler (workspace, liveWindows, claim, broadcast).
8. **C10**: Fix the `closelive` handler (claim release, broadcast).
9. **C11**: Fix the `remote_info` handler (return actual values).
10. **C12**: Port the `reclaim` handler.

### Priority 3 (incomplete but not broken)

11. **M1/M2**: Fix `PaneStream` subscribe/drain to use subscriber ids.
12. **M3/M4**: Use `ConnData` and `by_device` (or remove them).
13. **M5**: Add an idle connection sweep.
14. **M6**: Add attach generation tracking.
15. **M7**: Send the full state in the `attach` handler.
16. **M9**: Add signal handling in `ldmuxd.rs` (or rely on `Drop`).

### Priority 4 (defer)

17. **M8**: `copymode` — defer (the keeper has no copy-mode equivalent). Document as
    a known limitation.

---

## 6. WHAT IS WORKING WELL

- **Protocol crate**: The frame codec, messages, RPC, pairing, net, paths, socket —
  all correct and tested.
- **Keeper**: PTY allocation, fork+exec (sole reaper), reaping, group-kill, emulator
  thread, snapshot/state handlers — all correct and tested.
- **vt-sys**: FFI bindings, render state, SGR encoder, snapshot renderer — all correct
  and tested.
- **Attach**: Skeleton (connect, read_input, send_input, read_output, write_output,
  pump_output) — correct for the headless gate.
- **Daemon lifecycle**: `start`, `stop`, `reconcile`, `adopt_own_sessions`,
  `create_session`, `kill_session`, `close_session`, `forget_session` — all correct
  and tested.
- **Control socket**: The Unix socket server (one line per request) — correct and tested.
- **Remote WS**: The WebSocket server (token auth, 16 message types) — the handlers are
  present, but the input/key/stream paths are broken (C1-C5).
- **Fake keeper**: A well-designed test double that mirrors the real keeper.
- **Keeper supervisor**: Auto-spawn, death detection, respawn — correct and tested.
- **Risk tests**: Backpressure, dead-reader, spawn-race, restart-survival, keeper-death
  — all pass.
- **Desktop e2e**: Xvfb + i3, functions don't panic — pass.

---

## 7. SUMMARY

The Rust port is in good shape for the **core infrastructure** (phases 1-4: protocol,
pure modules, keeper/vt-sys/attach, emulator + parity). The **daemon** (phase 5) is
mostly done but has a **critical gap in the stream layer**: no poll loop (C1), broken
input/key handlers (C2-C5), and missing desktop/adopt/reclaim handlers (C6-C12).

The **single most critical fix** is C1 (the poll loop). Without it, the terminal is
static — the phone attaches, gets a snapshot, but never sees live output. This is the
blocker for phase 5 completion.

After C1-C5 are fixed, the remaining work (C6-C12, M1-M9) is a straightforward port of
the TS handlers. Phase 7 (build/deploy) is not started.
