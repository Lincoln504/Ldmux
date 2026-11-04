# Mesh link latency: what it is made of, and what the software can do

Written 2026-11-03. This is the measurement and design record for "make the mesh link feel
responsive". It separates the parts the client cannot change (radio airtime, the companion
firmware's rates) from the parts it can, and it names which of those ldmux has actually
implemented. Numbers are either measured on this project or attributed to a source.

## The budget, in order

1. **Airtime per record.** One screen update or one keystroke is one record, and a record that
   does not fit waits for the next one, so airtime dominates everything the protocol can do.
   **This link is 138 ms per 176-byte packet**: the pair is configured 914.25 MHz / 500 kHz /
   SF8 / CR5 and read back on the device (`MESH-STANDALONE.md:415`), which at a 1% duty cycle
   buys **261 records an hour**. Read every timing below against 138 ms.

   The public MeshCore presets are narrower and much slower: 62.5 kHz at SF7-11, where a
   172-byte frame is about **554 ms** at SF7, roughly 1.8 s at SF10 and 3.5 s at SF11
   (time-on-air arithmetic, not a measurement). This link cannot reach those presets because
   the bandwidth differs. Quote 554 ms only for someone running a public preset, and say which
   preset it belongs to: **corrected 2026-11-04**, because quoting it first made this link look
   four times slower than it is.
2. **The companion BLE link.** MeshCore's ESP32 firmware declares its RX characteristic as
   write-with-response only and spaces TX notifications by 60 ms, so the radio emits at most
   about 16 notifications a second. With a 172-byte frame ceiling that is a hard ~2.4
   records/s from the radio to the phone, whatever the radio is doing. MTU must be at least
   175 for one frame to fit in one ATT write; the app requests 256 and requires 179.
3. **The L3 session's own timers.** `ldmux-session` defaults (`crates/session/src/arq.rs`,
   `Config::for_payload`): window 8, ack delay 150 ms, first retransmit 2 s, **no heartbeat**.
   The window is the L2 credit; eight unacked records at 138 ms each is 1.1 s of air, so a
   burst that fills the window is a burst the user feels — but a short one.
   `heartbeat_ms` is 0 on purpose: silence is only a failure signal while something is
   outstanding, and the ARQ ladder finds a dead peer as soon as there is anything to send,
   while a 3 s heartbeat cost 1200 records an hour to say nothing (the comment on the field).
   An earlier revision of this section said 3 s.
4. **The host's update pacing.** This is the one large, controllable term, and until 2026-11-03
   it was uncapped: the bridge diffed the screen every 50 ms and sent every change. See below.
5. **Round-trip confirmation.** The ARQ ack rides on the next record (or a 150 ms ack timer),
   so an echo costs one record each way: about two carrier-times plus the bridge tick,
   not the multi-second timeout a retransmission would cost.

## What ldmux does about it

- **Burst coalescing (`crates/daemon/src/mesh_bridge.rs`, commit 2d8cf10).** A screen that
  keeps changing is sent at most once a second. The first change after a quiet tick, the echo
  of input, a `Resync`, an accepted `Resize`, a refused send and a phone returning after a
  full window all bypass the wait. This is the biggest software saving available: a short
  frame's preamble and header are a large fraction of its airtime, so one record carrying the
  present screen costs less than several carrying moments that have already passed.
- **One record per update where it fits.** Messages share a record up to
  `RECORD_PAYLOAD` (143 bytes), so a row and its commit are one frame, not two.
- **Cursor row first.** When a frame is split across the window, the row the cursor is on is
  sent first, so the echo appears before the rest of the screen.
- **The window, not "one update in flight".** Reading the screen only when nothing is queued
  cost a round trip per update (measured as a 16 s keystroke-to-echo gap). The bridge now
  keeps filling up to the session window.
- **Client-side scrollback with no airtime.** The host numbers the row at the top of each
  screen (`firstLine`); `MeshHistory` keeps the rows that left it. Scrolling is local and
  costs nothing on the link.
- **Dead-link detection (commit 9acd926).** A ping whose write suspends forever is now
  bounded, so three unanswered beats become an offline state instead of a frozen terminal,
  and the user is returned to the shell list.

## What is deliberately not done

- **No radio-parameter change from the app.** Spreading factor and bandwidth are a network
  decision: every node must match or nothing demodulates. Narrow/Fast presets belong in the
  region preset picker, not in a latency tweak.
- **No automatic path discovery yet.** Raw custom is direct-route only in this firmware, and
  `CMD_RESET_PATH` / `CMD_SEND_PATH_DISCOVERY_REQ` are not implemented in the vendored
  library. A stored path is used as-is and a `NotFound` send falls back to zero-hop once.
- **No retransmission tuning beyond the defaults above.** The 2 s first retransmit is about
  ten times a measured round trip at 62.5 kHz; shortening it further would only add
  duplicates on a busy channel.

## How to measure it

Instrumented points: `LDMUX_MESH_TIMING=1` prints input arrival and reply queueing in the
daemon; the phone logs each attach, handshake and offline transition; the bridge counts
`records_sent`, `updates`, `refused_sends`. A useful measurement is keystroke-to-echo on the
phone, median and p90 over twenty attempts at a known preset, because a mean hides the retry
that made one attempt take ten seconds.

## Sources

LoRa time-on-air arithmetic and the MeshCore companion/BLE rates are from Semtech's airtime
relation and the MeshCore firmware's `SerialBLEInterface.cpp` (ESP32: 60 ms notification
spacing, write-with-response RX; nRF52: 15-30 ms connection interval, slave latency 4). The
MeshCore packet format, path encoding and raw-custom routing rules are from the first-party
packet/payload documents. These are arithmetic and source-level facts, not field
measurements of this link.
