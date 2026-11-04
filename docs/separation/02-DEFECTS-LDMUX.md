# Defects in `ldmux`

- Written 2026-11-04 against branch `feat/standalone-radio` at `c96d316`.
- Severity: S1 blocks a user or loses data, S2 wrong behaviour under a real condition, S3 documentation or consistency, S4 hygiene.
- Status: PROVEN means read in the source at that commit. SUSPECTED means inferred without a run.

## S1

### 1. The host reaches its radio only through MeshMore's Dart gateway

- PROVEN. `ldmuxd` opens a loopback TCP connection to a process in another repository, which owns the USB serial port.
- Evidence: `crates/daemon/src/mesh_bridge.rs` (the bridge client), `docs/MESH-GATEWAY.md`, and the coupling table in `06-SEPARATION-PLAN.md`.
- Consequence: LDMUX cannot ship a host build without MeshMore, cannot test the host path without MeshMore's gateway, and inherits MeshMore's exit codes, token handshake and release cadence.
- This is the defect the separation plan exists to remove.

### 2. A lost send result permanently consumes a credit slot

- PROVEN. `awaiting` is incremented on every record write (`crates/daemon/src/mesh.rs:782`) and decremented only when a `SendResult` frame arrives (`mesh.rs:706`).
- There is no timer, no decay and no reset on reconnect. `l2_slots` is computed as `CREDIT_WINDOW.saturating_sub(awaiting)` (`mesh.rs:755,769`), and the send gate is `awaiting < CREDIT_WINDOW` (`mesh.rs:791`).
- Consequence: each lost `SendResult` shrinks the window for the life of the process. After enough losses the bridge stops sending entirely and only a daemon restart recovers it.
- Fix: decay `awaiting` on a deadline, or reset it when the session reconnects, and log the decay so a stuck window is visible.

### 3. A tracked command that is never answered wedges the radio link

- PROVEN. `sendTracked` queues, `pumpTracked` writes one at a time, and `trackedAnswered` clears the in-flight flag (`packages/mobile/vendor/libmeshcore-android/src/main/java/io/iotone/meshcore/android/MeshcoreSession.java:883-919`).
- Nothing clears `trackedInFlight` on a timeout. `isIdle()` therefore stays false, and `AndroidRadioLink` refuses every raw send while a tracked command is outstanding (`AndroidRadioLink.kt:309`).
- Consequence: one lost answer turns the phone's radio link into a link that accepts nothing until the session is torn down.
- Fix: a per-command deadline that calls `trackedAnswered()` and reports a failure, which is what the protocol document requires.

## S2

### 4. Reconnect backoff never returns to its floor

- PROVEN. `backoff` starts at 1 s and doubles to a 30 s cap (`crates/daemon/src/mesh.rs:548,581`). It is never reset after a successful session.
- The comment above the cap says a gateway that comes back should be noticed quickly, which the code does not do.
- Consequence: after one outage the daemon waits 30 s before every reconnect for the rest of its life, so a radio that is switched off and on takes up to 30 s to come back.
- Fix: reset `backoff` to 1 s when a session reaches a bound state.

### 5. The bridge polls a full screen snapshot every 50 ms

- PROVEN. `TICK_MS = 50` (`crates/daemon/src/mesh_bridge.rs:739`), the tick calls `self.host.screen(&session).await` (`mesh_bridge.rs:347`), and `DaemonHost::screen` takes the daemon-wide mutex and awaits an RPC (`mesh_bridge.rs:653`).
- Consequence: 20 full-snapshot acquisitions per second per session, each holding a global lock across an await. The cost scales with the number of sessions, and it is paid whether or not anything changed.
- Fix: push on change, or poll a cheap revision counter and fetch the screen only when it moves.

### 6. Error codes are mapped to the wrong meanings

- PROVEN. `errorFor` maps 3 and 4 to `Busy` and 6 to `TooLong` (`AndroidRadioLink.kt:506-515`).
- The firmware's own codes are 1 unsupported, 2 not found, 3 table full, 4 bad state, 5 file I/O, 6 illegal argument (`examples/companion_radio/MyMesh.cpp`, `ERR_CODE_*`).
- Consequence: a table-full or bad-state refusal is reported to the user as "busy", and an illegal argument is reported as "too long", so the message names the wrong cause.
- Fix: map each code to its own outcome, and keep the numeric code in the log line.

### 7. The `0x19` raw-send branch is only correct at a one-byte path hash

- PROVEN. The phone sends `0x19` when `path.encoded <= 0x3F` and `0x41` above it (`AndroidRadioLink.kt:311-321`).
- The firmware reads the `0x19` length byte as a byte count in its bounds check (`MyMesh.cpp:1519-1521`) and as an encoded value in `Packet::writePath` (`src/Packet.cpp:20-23`). The two readings agree only when the hash size is one byte.
- The branch is therefore correct today and silently wrong the moment a two-byte path hash is selected, which the preset type already allows (`Radio.kt:60-62`).
- Fix: use `0x41` for every send and delete the `0x19` path, which removes the ambiguity instead of relying on the hash size.
- Upstream context, added 2026-11-04 from the field research: MeshCore v1.18.0 (released 2026-10-15) lists a `CMD_SEND_RAW_DATA` multi-byte path fix, and states that raw data over 2- and 3-byte hashes was broken before it. The radios here run `companion-v1.17.1`, so the workaround is a firmware-version boundary and not a permanent rule. Gate the carrier choice on the version the radio reports, and record which side of 1.18 each end is on.

### 8. The ARQ retry is shorter than the carrier's own timeout past one hop

- PROVEN, found 2026-11-04 by putting the firmware's timeout formula against `crates/session/src/arq.rs:128`.
- The firmware's direct-mode timeout is 500 + ((airtime x 6 + 250) x (hops + 1)) ms. At 500 kHz SF8 with 137.9 ms of airtime, two hops give 3,731 ms and three hops give 4,808 ms.
- `Config::for_payload` sets `rto_initial_ms: 2_000`, so on any path past a direct neighbour the first retry fires before an answer could arrive.
- The cost is one duplicate record per expiry. At a 36 s per hour budget that is real airtime, and a duplicate is still a transmission.
- Fix: derive `rto_initial_ms` from the preset and the hop count, or raise it to cover the firmware's own timeout, and keep the doubling.

## S3

### 9. Five different radio presets are asserted as the live one

- PROVEN. See `05-RADIO-TUPLE.md` for the full table.
- The live radio reports 914.25 MHz / 500 kHz / SF8 / CR5 (`docs/MESH-STANDALONE.md:415`, `docs/HARDWARE-VALIDATION.md:783`).
- The app's compiled default is 910.525 MHz / 500 kHz / SF7 / CR5 (`Radio.kt:66-73`).
- `docs/MESH-REGULATION-AND-CAPACITY.md:52` argues for 910.525 at 500 kHz.
- `docs/MESH-GATEWAY.md:206`, `docs/HARDWARE-VALIDATION.md:215,261,300,349` and `meshmore-sns-radio-config.md:53` all say 910.525 at 62.5 kHz.
- Consequence: the app's default cannot reach the host's radio, and the regulation document's US argument points at numbers the hardware is not using.

### 10. The airtime budget is stated four to eight times too pessimistically

- PROVEN arithmetic. `docs/MESH-GATEWAY.md:206,255,256` and `docs/MESH-LATENCY.md:10-11` use 613 ms per 176-byte packet, which is the 62.5 kHz SF7 figure.
- The running link is 500 kHz, where the same record is about 66 ms at SF7 (`docs/MESH-SCROLLBACK.md:110`) and about 132 ms at SF8.
- At a 1 percent duty cycle that is about 545 records per hour at SF7 and about 273 at SF8, against the 58.7 the documents use.
- Consequence: the design's "about 10 command-and-reply operations an hour" and the 200 by 100 screen budget are far more conservative than the link requires, which has already shaped the UI and the scrollback policy.

### 11. The vendored Java library has no upstream tracking

- PROVEN. LDMUX's `packages/mobile/vendor/libmeshcore-android` contains `RawDataFrame`, `StatsFrame`, `sendRawData`, `sendRawPacket`, `isIdle` and a tracked queue (`MeshcoreSession.java:129,539,572,615,883-919`).
- MeshMore's copy at the same firmware pin contains none of them.
- Consequence: a firmware change that alters the raw carrier cannot be picked up by a version bump, because there is no version to bump and no diff to review.

## S4

### 12. The BLE link has no automatic reconnect

- SUSPECTED. The only reconnect logic found is inside the preset rollback path (`AndroidRadioLink.kt:370-398`). No supervisor re-establishes a dropped GATT connection.
- Consequence: a radio that walks out of range and back needs a manual action.

### 13. The unsupported link cannot be released

- PROVEN. `meshRadioLink("")` returns `UnsupportedRadioLink`, whose `close()` is empty (`Radio.kt:330-355`).
- Consequence: a caller that holds it and calls `release()` leaves the reference count where it was, so the real link is never closed.

### 14. Timing instrumentation is on in production

- PROVEN. `LDMUX_MESH_TIMING=1` is set in the production environment, and it prints per-record timing lines (`crates/daemon/src/mesh.rs:419,736,759,764,778`).
- Consequence: log volume and a small per-record cost in the path that is being measured.

### 15. The gateway token and radio key sit in `/tmp`

- PROVEN. `/tmp/gw.json` holds the pairing token and the radio key, and it is wiped on reboot.
- Consequence: a reboot silently unpairs the host, and the file is world-readable by default unless the writer sets a mode.

### 16. Nothing guards the record's first byte against the firmware's routing heuristic

- PROVEN and currently harmless. Some firmware forks classify a raw-custom packet as media by its first byte alone and route `0x56`, `0x57`, `0x72` and `0x53` to a second frequency the node is not listening on.
- LDMUX's record begins with `KIND_TRANSPORT = 0x13` (`crates/session/src/record.rs:32`), so no LDMUX record can match that heuristic today.
- Consequence: a future record kind, or a future framing that puts application data first, could silently send every record to a frequency nobody is monitoring, and the radio would still answer `OK`.
- Fix: one test asserting the first byte is outside that set.

### 17. Two tests fail without `zbarimg`

- PROVEN. `cargo test --workspace` gives 314 passed and 2 failed, both for the missing binary.
- Consequence: a clean checkout cannot reach a green suite without an undeclared system dependency.
