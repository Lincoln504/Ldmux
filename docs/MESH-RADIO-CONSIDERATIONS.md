# Radio path: every consideration, its state, and what is left

Written 2026-11-10 after a day of field failures from a phone at range. Each line says what the
consideration is, what the code does today, and what remains. Evidence is a file, a measurement, or a
field log line — not an opinion.

## 1. The physical layer

- **Preset must match the mesh.** `910.525 MHz / 62.5 kHz / SF7 / CR5`, one-byte path hashes, both
  ends. The phone logs it on every connect (`mesh radio preset … tx 22 dBm (max 22)`); the host
  prints it in `ldmux mesh status`. A mismatch is silent and indistinguishable from distance — both
  ends now record it.
- **Measured margin at range:** nearest repeaters arrive at **−107 to −124 dBm**, SNR **−6.75 to
  +5.5 dB**, while the same receiver sees **−88 dBm** at home. SF7/62.5 kHz sensitivity is about
  −126 dBm, so a range test sits within a few dB of the floor.
- **Long packets lose first.** A 44-byte request crosses at 2–5 hops; a 133-byte handshake record
  often does not, from the same spot in the same minute. Airtime at 62.5 kHz: ~160 ms vs ~500 ms, so
  three times the exposure to a fade.
- **Antenna and height are the remaining physical levers.** A better antenna is worth 3–6 dB, which
  at this margin is the difference between occasional and routine. Nothing in software creates dB.
- **Regulatory:** the US preset is not duty-cycle limited (EU bands are 0.1–10 %); ldmux does not
  gate airtime on this preset because the allowance is not defined for it.

## 2. Firmware

- Phone radio: **Seeed Wio Tracker L1, MeshCore 1.17.1** (logged on every connect).
- Host radio: Heltec, same family of companion firmware.
- Known firmware faults that matter here: AGC lockup after days of uptime (Wio, cured by
  `set agc.reset.interval 32`), and deaf-after-BLE-reconnect on some ESP32-S3 boards (reflash).
  Neither matches the observed failures, which are directional and repeatable.
- **The host radio used to wedge on every daemon restart** because a CDC-ACM port comes up with DTR
  and RTS asserted, which is the reset side of an ESP32's auto-reset circuit. Fixed; verified by five
  consecutive restarts.
- **`RESP_CODE_SENT` vs `OK`:** firmware 1.17 answers a raw send with `OK`, v1.18 with
  `RESP_CODE_SENT` carrying its own suggested timeout. Both ends accept either and keep the estimate.

## 3. MeshCore's packet rules we must obey

- Header: route (bits 0–1), payload type (2–5), version (6–7). Path byte: hop count (0–5), hash size
  (6–7); hash size 4 is reserved and refused.
- **`RAW_CUSTOM` (0x0F) is direct-only.** The firmware delivers it only on a direct route and never
  forward-routes it, so a session record depends on a path.
- **Floods are forward-routed** by every repeater in range, subject to `flood.max` (64),
  `flood.max.unscoped` (64, often lowered by operators), `flood.max.advert` (8), region scoping and
  loop detection.
- **Duplicate suppression** is `SHA256(payload_type ‖ payload)` in a 160-entry FIFO with no timeout,
  at every repeater and receiver. Every retry therefore needs a fresh nonce — records re-seal, floods
  carry a fresh wrapper nonce.
- **A direct packet's path is consumed in transit**, so a destination cannot learn the return chain
  from a direct packet it receives. Only a flood (or a route request/reply) carries a usable chain.
- **Group data (`GRP_DATA` 0x06) is floodable** and holds 165 bytes, but neither of our clients can
  send it: the phone library has the command as a constant with no builder, and the host crate has
  nothing. It is the upgrade path if a fallback ever needs to carry more than one record.

## 4. Carriers (ADR-0037, ADR-0038)

- **Direct is the default**; flood is escalation caused by evidence and decays on any arrival.
- **One cheap attempt precedes any flood** — a zero-hop direct packet is right for a neighbour.
- **A flood is answered by a flood at once**, sticky until a *direct* record arrives from the peer.
- **A flood replaces the path forcibly**, even over one marked proven, or the link can never return to
  direct.
- **The silence that triggers escalation is the path's own round trip**, six airtimes per hop — not
  the route's staleness bound.
- **Pacing:** a token bucket (4 floods) refills at one per flood round trip at the mesh's measured
  reach. It applies to data only — the bucket starved handshake copies (`1/3` then nothing) until it
  was scoped to established sessions.
- **Capacity:** a flood-carried record holds 162 bytes (the wrapper costs 12); a pathless direct
  record 171. Both ends publish the flood figure when no route is fresh, so the session never builds a
  record its carrier cannot take.

## 5. Session timing (the part still wrong)

- **Handshake is two messages** (Noise IK, stateless transport): msg1 and msg2, so each direction is
  half the odds.
- **Our ladder:** rungs at `8 s + 2 s/hop`, doubling to 8/16/32 at a neighbour; budget `rung × 8` =
  **64 s at 0 hops**.
- **The firmware's own arithmetic** (`calcDirectTimeoutMillisFor`, and `openhop_core` reproduces it):
  `500 + (6·airtime + 250)·(hops+1)`, clamped **1.5–12 s**, with **3 attempts**, and a multi-hop reply
  window of **120 s** ("observed multi-hop replies can take over a minute").
- **Consequence:** our retry cadence is faster than a five-repeater answer can return, and our budget
  is half the documented worst case. A handshake that the host has already established is abandoned
  by the phone before the answer lands. **This is the next change.**
- **ARQ:** re-seal per retransmission, duplicate is repair not fault, silence bound scales with hops,
  in-flight window `64/hops` clamped 1..8.
- **A flooded record is timed against the mesh's measured reach**, not one hop.

## 6. Discovery

- **Route request:** `REQ` flood, three copies, fresh nonce each, pace 5/10/20 s.
- **Reply:** flood, now repeated three times, each with a fresh nonce, **delayed 500 ms + jitter** so
  it does not collide with the request's own echo (MeshCore's own path return uses 500 ms).
- **Adverts:** the phone announces on every connect and, while linkless, every other discovery
  attempt (at most one a minute). An advert is one-way and teaches the far end where we are — the
  host logged `advert heard from 26f6071f hops=2 rssi=-65dBm` from the field.
- **Adverts are capped at 8 hops** by `flood.max.advert`; requests are not.

## 7. The phone application

- **One radio, one app** (ADR-0036): a lease asks other apps to yield; they disconnect and keep
  running.
- **Two instances of ldmux can lease the radio from each other** — observed 2026-11-10, PID 32486
  losing the radio to PID 8114 mid-handshake. The app should be single-instance.
- **Mode vs route:** the app connects over the **tailnet WebSocket** whenever that is reachable, even
  while the screen shows the radio. Several "it works" observations turned out to be the tailnet. A
  radio test requires the tailnet to be off on the device.
- **Durable log:** rolling `mesh-log.txt` (256 KiB → 128 KiB), wall-clock stamped, protocol lines
  only, plus `mesh-last-failure.log`. This is what made today diagnosable.
- **What the receiver hears** is counted and reported (`mesh heard N packet(s) in 10s · last …`),
  which distinguishes "no coverage" from "deaf radio" — a quiet mesh is silent because repeaters
  advertise every 47 hours, not because nothing is there.

## 8. Observability

- Phone: every route decision, every carrier choice with size and destination byte, every rejected
  record with its reason in words, the radio preset and firmware.
- Host: every request/reply/flooded record with hops and RSSI, every advert heard with key and RSSI,
  every record send with carrier, size and hops, route decisions with their reasons.
- Wanted but not built: per-record ack latency and retransmission counts per attempt, so a marginal
  link can be characterised instead of described.

## 9. Testing

- Unit-level: the two carriers are byte-pinned in Rust and Kotlin; the flood wrapper round-trips; the
  host reads a phone's flooded record through a real receive-log frame.
- Simulation: a 28-repeater lossy route (`crates/session/tests/lossy_multihop.rs`) with deterministic
  loss, which advances to timer deadlines as well as arrivals.
- **Missing:** an end-to-end test that drives the *handshake* across a simulated multi-hop mesh with
  realistic loss and the firmware's timing. That would have caught the budget problem.
- `mesh-research/clones/mcsim` is a C++ MeshCore simulator on this machine and could supply realistic
  propagation and collision behaviour for exactly that test.

## 10. Comparison with other implementations (local clones)

- **`openhop_core`** — the closest reference for a companion: adaptive timeouts from the firmware
  formulas, 3 attempts, a 120 s login/reply window, flood only when `out_path_len` is unknown, and
  scope applied to every originated flood. Our timing should follow it.
- **`meshcore_pi`** (`companionradio.py`) — the maintainers' own Python companion: the same timeout
  arithmetic and retry cadence.
- **`meshcore.js` / `meshcore_client`** — protocol codecs; useful for path encoding and message
  layouts, not for retry policy.
- **`mcsim`** (C++), **`mcsim-rust`/`mcrs`**, **`coresplitter`**, **`filter-firmware`** — firmware and
  simulation references for forwarding rules, flood limits and loop detection.

## Open items, in the order they matter

1. **Handshake timing to the firmware's formula** — cadence and a ≥120 s budget. The phone abandons
   handshakes the host has already established.
2. **Single-instance app** — two copies lease the radio from each other.
3. **Radio mode must not silently use the tailnet** — the mode the operator picks should decide the
   transport, or say plainly which one is carrying.
4. **A simulated multi-hop handshake test** — to stop rediscovering timing regressions in the field.
5. **`GRP_DATA` fallback** — the only carrier that works with no route at all, if 1–4 are not enough.
6. **Per-attempt ack latency in the logs** — to characterise a marginal link numerically.
