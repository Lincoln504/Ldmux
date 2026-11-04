# Mesh link latency: what it is made of, and what the software can do

Written 2026-11-03. This is the measurement and design record for "make the mesh link feel
responsive". It separates the parts the client cannot change (radio airtime, the companion
firmware's rates) from the parts it can, and it names which of those ldmux has actually
implemented. Numbers are either measured on this project or attributed to a source.

## The budget, in order

1. **Airtime per record.** MeshCore's second-generation LoRa presets are narrow: 62.5 kHz at
   SF7-11. A full 172-byte companion frame at SF7/62.5 kHz is about 554 ms on air; the same
   frame at SF10 is roughly 1.8 s and at SF11 roughly 3.5 s (time-on-air arithmetic, not a
   measurement). Record size therefore dominates everything the protocol can do: one screen
   update is one record, and a record that does not fit waits for the next one.

   **Corrected 2026-11-04.** This link does not run a 62.5 kHz preset. The pair was configured
   to 914.25 MHz / 500 kHz / SF8 / CR5 and read back on the device (`MESH-STANDALONE.md:415`),
   where the same 176-byte packet is **138 ms**, so 36 s per hour buys **261 records**, not 58.
   The 62.5 kHz figures describe the public MeshCore presets, which this link cannot reach
   because the bandwidth differs.
2. **The companion BLE link.** MeshCore's ESP32 firmware declares its RX characteristic as
   write-with-response only and spaces TX notifications by 60 ms, so the radio emits at most
   about 16 notifications a second. With a 172-byte frame ceiling that is a hard ~2.4
   records/s from the radio to the phone, whatever the radio is doing. MTU must be at least
   175 for one frame to fit in one ATT write; the app requests 256 and requires 179.
3. **The L3 session's own timers.** `ldmux-session` defaults, measured 2026-10-30: window 8,
   ack delay 150 ms, first retransmit 2 s, heartbeat 3 s. The window is the L2 credit; eight
   unacked records at 554 ms each is 4.4 s of air, so a burst that fills the window is a
   burst the user feels.
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
