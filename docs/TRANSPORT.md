# ldmux — Transport deep-dive and decision

**Question:** Which mechanism should `ldmuxd` use to stream tmux pane output to
mobile/desktop clients, and with which stack?

**Answer:** **tmux control mode `-C` over plain pipes (no PTY), + `capture-pane`
snapshots/fallback, under Node + TypeScript.** This is ADR-0001 (frozen).

This document records the experiments, the comparison, and the exact parser
contract, so implementation is unambiguous.

---

## 1. The premise everyone gets wrong

dooboxes' `TMUX_SESSION_MANAGEMENT.md` and its `control.rs` call tmux control mode
"PTY-based". That is **convention, not a requirement**. tmux's own man page says
`-CC` disables echo; it does not say `-C` needs a tty.

Hands-on result on this machine (tmux **3.5a**, originally driven from Bun
1.3.9; the transport is now exercised on every run of the suite under Node 26):

```
# A) -C over pipes, no tty — WORKS
$ ( printf 'display-message -p "#{session_name}"\n'; sleep 0.4 ) \
    | timeout 3 tmux -L ldmux-probe -C attach -t s1
%begin 1789335997 282 0
%end 1789335997 282 0
%session-changed $0 s1
%begin 1789335997 286 1
s1
%end 1789335997 286 1
%exit

# B) -CC over pipes — FAILS
$ tmux -L ldmux-probe -CC attach -t s1 < fifo
tcgetattr failed: Inappropriate ioctl for device
```

**Rule: use `-C`. `-CC` needs a tty; `-C` does not.** Over a pipe there is no
outer terminal to echo to, so `-CC`'s no-echo benefit is moot.

Live output over a pipe (no tty), ANSI intact:

```
%output %0 echo HELLO_FROM_PANE\015\012
%output %0 \033[?2004l\015
%output %0 HELLO_FROM_PANE\015\012
%output %0 \033[?2004h\033[01;32m[ ldeen@debian:/tmp$ ]\033[00m
```

Flow control over a pipe, after `refresh-client -f pause-after=2`:

```
$ tmux -L s list-clients -F '#{client_name} #{client_flags}'
client-2169804 attached,focused,control-mode,pause-after=2,UTF-8
# output then uses:
%extended-output %0 0 : echo BUN_CONTROL_OK; seq 1 5\015\012…
```

Canonical geometry: with `set-option -g window-size manual`, the control client
reports `80x` but the window stays `100x30` — **no reflow** of other clients.

## 2. Proven under the real stack (Bun 1.3.9)

A `Bun.spawn(["tmux","-L","ldmux-probe","-C","attach","-t","s1"],
{stdin:"pipe",stdout:"pipe"})` script wrote `refresh-client` + `send-keys`,
flushed, read the stream, and parsed `%extended-output` correctly:

```json
{ "bun": "1.3.9", "ok": true,
  "sample": ["%extended-output %0 0 : echo BUN_CONTROL_OK; seq 1 5\\015\\012…",
             "%extended-output %0 0 : 1\\015\\0122\\015\\012…",
             "%extended-output %0 0 : \\033[?2004h\\033[01;32m[ ldeen@debian:/tmp$ ]\\033[00m "] }
```

Burst performance (`seq 1 30000`): **~383 KB in ~255 ms (~1.5 MB/s), 184
`%extended-output` frames, no stalls**; tmux ~0% CPU when idle. By contrast
`capture-pane -p -e` costs **~3.1 ms per call** (process-spawn overhead dominates),
so polling at 10 Hz is ≈3% core *per session* plus 3 ms latency.

## 3. Option comparison

| | Control mode `-C` (pipe) | `capture-pane` diff poll | `pipe-pane` |
|---|---|---|---|
| Latency | **best** (event-driven) | poll interval (60–120 ms) | best |
| Flow control | **yes** (`pause-after`, `%pause`/`%continue`, `%extended-output`) | none (coalesce/skip) | none |
| PTY / native dep | **none** (proven) | none | none |
| Restart safety | reconnect + snapshot | **best** (stateless) | fiddly |
| Full-screen TUI cost | low (changed bytes) | high (full redraws) | low |
| Topology events | yes (`%layout-change`, …) | no | no |
| Parser complexity | medium | low | low |

**Control mode wins on every axis that matters**, at the cost of a parser we must
write carefully.

## 4. dooboxes lessons (what to copy, what to avoid)

From `apps/session-bridge/src/` and docs:

- **Copy:** `compute_diff` (`bridge/streaming.rs` L114–140) — if `new` starts with
  `last`, emit the UTF-8-boundary-walked suffix; else emit the full capture so the
  client resyncs. Copy the **stateful sanitizer** policy (one instance per stream;
  drop OSC 52 / DCS / device-status).
- **Avoid:** their control-mode client (`tmux/control.rs`) is **dead code** — grep
  finds no consumer; the shipped path is capture-pane diffs. Its parser has real
  bugs: drops whole reads on split UTF-8 (L241–269), `unescape_octal` does
  `byte as char` (L397–431), and `read_output_block` ignores the sequence number
  (L484–542).
- **Avoid:** three incompatible wire framings in the tree
  (`TERMINAL_CODEC_DESIGN.md` §0). Define **one** canonical frame in
  `packages/protocol` with conformance tests.

## 5. Parser contract (must-have)

Byte stream from control mode (LF-delimited records):

1. Frame on `\n` only. Strip a trailing `\r`. **Never** use Node `readline`
   (it splits on U+2028/U+2029) or a generic line reader.
2. Records starting with `%` are notifications; otherwise a command response line.
3. `%begin <ts> <seq> <flags>` … `%end|%error … <seq>` — correlate by `<seq>`
   (a FIFO queue of pending commands). Notifications never appear inside a block.
4. `%output %<pane> <data>` and `%extended-output %<pane> <age_ms> : <data>`:
   decode `\NNN` octal into **bytes** (control chars + backslash), then feed a
   **per-pane incremental UTF-8 decoder** (chars can straddle records).
5. Handle `%pause` / `%continue`. tmux **discards** a paused pane's queued
   output, so a `%pause` is a resync point: subscribers are told to re-snapshot
   (throttled) and the pane is resumed with `refresh-client -A <pane>:continue`
   on a coalescing timer (a dropped resume would freeze the pane). Backpressure
   for a slow *client* is the remote layer closing a socket whose buffer exceeds
   4 MiB (`too_far_behind`); reading is never stopped in-process.
6. `%exit`/EOF → run the capture-pane fallback; restart the control client with
   backoff.
7. Tolerate unknown `%…` lines (forward compatibility).

**Two gotchas found while implementing this (do not reintroduce):**

- **Attach preamble:** tmux emits an unsolicited, empty `%begin/%end` block
  immediately on attach, *before* `%session-changed`. If you register a command
  waiter before it arrives, the response FIFO desyncs (commands resolve with
  empty output, and a later `%error` is misattributed). **Wait for
  `%session-changed` before issuing the first command** (or drop the first
  response block). The client in `packages/daemon` does this.
- **Byte-level octal:** tmux leaves bytes ≥ 0x80 **raw** (only < 0x20 and `\` are
  escaped). Octal-decode at the **byte** level. Decoding a JS string and doing
  `charCodeAt(i) & 0xff` destroys astral characters (😀 → surrogates → `=` + NUL).
  `packages/daemon/src/tmux/octal.ts` decodes `Uint8Array` → `Uint8Array` for
  this reason, with a regression test.

## 6. Snapshot & fallback

- **Snapshot:** `capture-pane -p -e -t %<pane> -S -<n>` plus state fields from
  `display-message -p` (`cursor_x`, `cursor_y`, `alternate_on`, `pane_in_mode`,
  `bracketed_paste`, `mouse`, `app_cursor`, `history_size`, `pane_width`,
  `pane_height`). Carry `snapshotEpoch` (ADR-0004).
- **Field availability:** `bracketed_paste_flag` and `pane_private_modes` are
  tmux **3.8+**; on older builds (e.g. 3.7b in the devShell, 3.5a system) those
  query fields are empty. Treat missing as `false`; the remaining fields
  (`cursor_x/y`, `alternate_on`, `pane_in_mode`, `history_size`, dimensions) are
  available. Full mode fidelity remains the R2 phase-3 seam.
- **Alternate screen:** use `capture-pane -a` when `alternate_on` is true (or
  unconditionally, since `-a` falls back to the normal screen).
- **Fallback:** `capture-pane` diff polling at a fixed ~250 ms (no idle backoff;
it stops when the stream has no subscribers and retries control mode every
30 s) whenever the control client is unavailable. Diffs are **row-addressed**
(`CSI <row>;1H` + `CSI K`) rather than whole-screen repaints, so polling does
not push duplicate screens into the client's scrollback.

## 7. Stack decision

**Bun + TypeScript.** No native modules are required (control mode over pipes,
`bun:sqlite` if/persisted state is added, native WebSocket/Unix sockets,
single-binary via `bun build --target bun`). Rust/tokio remains an escape hatch
behind `PaneStream` if throughput ever demands it.

## 8. Remaining Phase-1 spike items

- ~~Sustained backpressure: force a slow reader and confirm `%pause` fires and
  the pane is protected (only a short burst was measured).~~ Done:
  `packages/daemon/test/stream.test.ts` ("a real tmux %pause resyncs
  subscribers and then resumes the pane") sets `pauseAfter: 0` and drives a
  300KB burst through a real tmux, so `%pause` firing and the pane resuming
  are both exercised for real now, not just measured on a short burst.
- Many concurrent control clients (scaling test) — still open; no test found
  for this.
- `pipe-pane` FIFO restart/continuity (only if revisited) — still open; no
  test found for this.
- Confirm `%extended-output` vs `%output` switching is handled across
  `pause-after` enable/disable — partially covered:
  `packages/daemon/test/control-parser.test.ts` tests `%extended-output`
  decoding (including a malformed one) but the switching-across-`pause-after`
  case specifically was not found.