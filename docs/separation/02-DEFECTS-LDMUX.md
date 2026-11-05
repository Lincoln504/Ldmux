# Defects in `ldmux`

- Written 2026-11-04 at `c96d316`, **re-verified and rewritten at `7b8510b` and the commit after
  it** (the refusal-pace and sequence-number work). Every line citation below was checked against
  the current tree, not carried over: a register with stale numbers is worse than no register,
  because it survives review.
- Severity: S1 blocks a user or loses data, S2 wrong behaviour under a real condition, S3
  documentation or consistency, S4 hygiene.
- Status: FIXED (with the commit or the code that fixed it), OPEN, or WRONG (the claim is no longer
  true). Evidence labels: PROVEN means a code path was read; SUSPECTED means inferred without a run.
- No mobile test suite and no radio was run for this revision. The mobile sources are read, not
  compiled.

## S1

### 1. The host reaches its radio only through MeshMore's Dart gateway — FIXED in code, UNVERIFIED on hardware (2026-11-05)

- Now: `LDMUX_MESH_SERIAL=<device>` makes the daemon own the radio through `crates/companion`
  (serial framing, `APP_START`, `DEVICE_QUERY` at protocol 3, `0x41` raw send, `0x88` receive),
  served on a dedicated thread behind the `Carrier` trait in `crates/mesh`. No gateway process, no
  socket, no token, no Dart. The gateway remains as the alternative carrier for one release.
- Unverified: no hardware pass has been run on the serial carrier yet; the unit and scripted-carrier
  tests are the only evidence. Marked fixed-in-code rather than closed.

- PROVEN. `ldmuxd` opens a loopback TCP connection to a process in another repository, which owns
  the USB serial port (`crates/daemon/src/mesh.rs`, connect and the write loop).
- Evidence: `docs/MESH-GATEWAY.md`, `crates/mesh/src/client.rs` (the frame client the daemon uses),
  and the coupling table in `06-SEPARATION-PLAN.md`.
- Consequence: LDMUX cannot ship a host build without MeshMore, cannot test the host path without
  MeshMore's gateway, and inherits MeshMore's token handshake, exit codes and release cadence.
- This is the defect the separation plan exists to remove, and it is the only S1 left.

## S2

### 2. A lost send result permanently consumes a credit slot — FIXED

- Was: `awaiting` grew on every record write and shrank only on a `SEND_RESULT`, with no timer.
- Now: `AWAITING_DEADLINE = 10 s` (`crates/daemon/src/mesh.rs:46`), a front-of-queue sweep
  (`mesh.rs:824-832`) that releases the slots and names how many in `last_error`, and a
  sequence-keyed removal (`mesh.rs:770-778`).
- Fixed in `d00111c`.

### 3. A tracked command that is never answered wedges the radio link — FIXED

- Was: `trackedInFlight` was cleared only by an answer, so `isIdle()` stayed false and the link
  accepted nothing.
- Now: `expireTrackedLocked` with `TRACKED_DEADLINE_NANOS`
  (`packages/mobile/vendor/libmeshcore-android/src/main/java/io/iotone/meshcore/android/MeshcoreSession.java:569-575`).
- This is one of LDMUX's own additions to the vendored library; see defect 11, which is still open
  because that addition has no upstream tracking.

### 4. Reconnect backoff never returns to its floor — FIXED

- Was: `backoff` doubled to 30 s and was never reset.
- Now: a `bound` latch resets it to 1 s when a session reaches a bound state
  (`crates/daemon/src/mesh.rs:585,593-620,626`).
- Fixed in `d00111c`.

### 5. The bridge polls a full screen snapshot every 50 ms — PARTIAL

- Cadence improved: `TICK_MS = 50` when something moved, `QUIET_TICK_MS = 150` after eight quiet
  ticks (`crates/daemon/src/mesh_bridge.rs:795,803`, interval chosen at `:308-318`). A quiet link
  now takes about 6.7 snapshots a second instead of 20.
- **Still open:** `DaemonHost::screen` takes the daemon-wide mutex and awaits an RPC while holding
  it (`mesh_bridge.rs:709-716`), and `input`, `keys` and `resize` do the same. Every snapshot
  serialises every other user of that lock for the length of a keeper round trip, and the cost
  still scales with the number of sessions.
- Smallest correct fix: stop holding the lock across the await (hold a handle the client serialises
  internally), or skip the snapshot entirely when a cheap revision counter has not moved.

### 6. Error codes are mapped to the wrong meanings — FIXED

- Now (`AndroidRadioLink.kt:515-531`): 1 unsupported, 2 not found, 3 table full, 4 bad state, 5 file
  I/O, 6 illegal argument. The numeric code stays in the log line either way.
- 6 used to be reported as `TooLong`, which surfaced as a "no path" state. It is
  `ERR_CODE_ILLEGAL_ARG`: on `0x41` the only other failure is a full transmit table, so 6 means the
  firmware could not parse the packet we built. Mapping it to a size complaint hid the one failure a
  developer needs to see.

### 7. The `0x19` raw-send branch is only correct at a one-byte path hash — FIXED

- Now every send uses `CMD_SEND_RAW_PACKET` (`0x41`) at every path width
  (`AndroidRadioLink.kt:125-137,474-481`), which carries a complete packet, so the encoded path is
  written once by us and parsed once by the firmware. The `0x19` branch and its two readings are
  gone.
- **Correction to this document's own earlier note:** the field research reports a
  `CMD_SEND_RAW_DATA` multi-byte path fix in firmware 1.18.0, and an earlier revision of this file
  concluded that the carrier choice must therefore be gated on the firmware version. That is wrong
  for `0x41`: the 1.18.0 fix is in the `0x19` handler, and `0x41` parses a whole packet through
  `Packet::readFrom` on both sides of that release. With `0x41` only, there is no version boundary
  and no gate to build. `06-SEPARATION-PLAN.md` has been corrected.

### 8. The ARQ retry is shorter than the carrier's own timeout past one hop — OPEN

- PROVEN. `Config::for_payload` sets `rto_initial_ms: 2_000` (`crates/session/src/arq.rs:132`).
- The firmware's direct-mode timeout is `500 + ((airtime x 6 + 250) x (hops + 1))`. At 500 kHz SF8
  with 137.9 ms of airtime that is 1.6 s for a neighbour, 2.7 s at one hop, 3.7 s at two.
- A second formula in the corpus, `4000 + 2000 x (hops + 1)` plus a 12 s flood timeout, describes
  the push path rather than the raw carrier, but it agrees on the direction: 2 s is short.
- Consequence: on a two-hop path the ARQ retransmits about 1.7 s before the firmware would have
  given up. The cost is one duplicate record per expiry, paid out of the same hourly budget, and a
  duplicate is still a transmission.
- The smoothed-RTT floor (`arq.rs:492`) fixes the steady state after the first ack. The first
  transmission still starts at 2 s.
- Fix, not done: derive the initial interval from the preset and the hop count the path reports, or
  raise the floor to cover the firmware's own timeout. It changes measured device behaviour, so it
  needs a device run and was left open deliberately.

## S3

### 9. Five different radio presets are asserted as the live one — OPEN

- PROVEN. See `05-RADIO-TUPLE.md` for the table. The live radio reports 914.25 MHz / 500 kHz / SF8 /
  CR5 (`docs/MESH-STANDALONE.md:415`, `docs/HARDWARE-VALIDATION.md:783`); the app's compiled default
  is 910.525 / 500 / SF7 (`Radio.kt:64-73`); `docs/MESH-REGULATION-AND-CAPACITY.md:52` argues for
  910.525 at 500 kHz; `docs/MESH-GATEWAY.md:206`, `docs/HARDWARE-VALIDATION.md:215,261,300,349` and
  `meshmore-sns-radio-config.md:53` say 910.525 at 62.5 kHz.
- Consequence: the app's default cannot reach the host's radio, and the regulation document's US
  argument points at numbers the hardware is not using.
- `05-RADIO-TUPLE.md` recommends option C (no compiled default, adopt the radio's own report, refuse
  sends while the ends disagree). Nothing implements it. The host cannot even report its tuple: the
  `ldmux-mesh` crate deliberately carries no radio knowledge (`crates/mesh/src/lib.rs:9-13`) and
  `MeshStatus` has rssi, battery, airtime and records left only (`crates/daemon/src/mesh.rs`).

### 10. The airtime budget is stated four to eight times too pessimistically — CORRECTED

- `docs/MESH-GATEWAY.md:206,255,256` and `docs/MESH-LATENCY.md:10-11` use 613 ms per 176-byte
  packet, which is the 62.5 kHz SF7 figure.
- Recomputed with the full formula and the 32/16 preamble split: a 176-byte packet is **75.3 ms** at
  500 kHz SF7 and **137.9 ms** at 500 kHz SF8. At a 1 percent duty cycle that is 478 and 261 records
  an hour, not 545 and 273. The 62.5 kHz figure of 58.7 stands and is the only measured one.
- **This file previously used 66 and 132 ms**, which `05-RADIO-TUPLE.md` and
  `08-RESEARCH-INTEGRATION.md` corrected. The conclusion is unchanged: the design's "about ten
  command-and-reply operations an hour" and the 200 by 100 screen budget are four to eight times
  more conservative than the running link requires.

### 11. The vendored Java library has no upstream tracking — OPEN

- PROVEN. LDMUX's `packages/mobile/vendor/libmeshcore-android` contains `RawDataFrame`,
  `StatsFrame`, `sendRawData`, `sendRawPacket`, `isIdle`, a tracked queue and
  `expireTrackedLocked` (`MeshcoreSession.java:129,539,569-575,615,883-919`). MeshMore's copy at the
  same firmware pin contains none of them.
- Consequence: a firmware change that alters the raw carrier cannot be picked up by a version bump,
  because there is no version to bump and no diff to review.
- Fix, not done: a `VENDOR.md` naming the upstream commit and every local patch, or upstream the
  additions.

## S4

### 12. The BLE link had no automatic reconnect — WRONG NOW

- The original claim ("a radio that walks out of range and back needs a manual action") is no longer
  true. `run` ends on `LinkDown` (`MeshTermDriver.kt`, the `failed` path) and the screen rebuilds
  with full jitter, 2 s doubling to a 30 s cap, whenever the app is foreground
  (`MeshTerminalScreen.kt:154-170`).
- What remains true is smaller and worth stating: **the supervisor is the screen**, so a page that is
  not on screen is not reconnected, and the `RadioLink` layer itself still has no supervisor.

### 13. The unsupported link cannot be released — WRONG

- The claim was that `meshRadioLink("")` returns `UnsupportedRadioLink` and that calling `release()`
  on it leaks a reference. Both halves are wrong at this commit.
- `meshRadioLink` on Android returns a real `AndroidRadioLink` for an empty address and never
  constructs `UnsupportedRadioLink` (`AndroidRadioLink.kt:567-570`). `UnsupportedRadioLink` exists
  only as the compile-time stand-in for a platform with no radio (`Radio.kt:332-351`), and it is
  never registered with `RadioLinks`, so there is no reference count for its empty `close()` to
  leak (`AndroidRadioLink.kt:579-610`: release closes a link outside the registry rather than
  returning).
- Nothing to fix. It stays in the register, marked wrong, so the next reader does not have to
  repeat the check that produced it.

### 14. Timing instrumentation is on in production — OPEN, an ops item

- PROVEN. The instrumentation is opt-in and gated on the variable being exactly `"1"`
  (`crates/daemon/src/mesh.rs:456,879,884,900`; `mesh_bridge.rs:878-881`).
- The defect was never in the code: `LDMUX_MESH_TIMING=1` is set in the deployed environment. The fix
  is to unset it in the unit, not to change the code.

### 15. The gateway token and radio key sit in `/tmp` — OPEN, an ops item

- PROVEN. `/tmp/gw.json` holds the pairing token and the radio key (`docs/HARDWARE-VALIDATION.md`),
  and it is wiped on reboot, so a reboot silently unpairs the host.
- Fix: a path under `$XDG_STATE_HOME`, mode 0600, so a reboot does not unpair and the file is not
  readable by anything else.

### 16. Nothing guards the record's first byte against the firmware's routing heuristic — FIXED

- LDMUX's record begins with `KIND_TRANSPORT = 0x13` (`crates/session/src/record.rs:32`), so no
  record can match the forks that classify media by first byte (`0x56`, `0x57`, `0x72`, `0x53`).
- Fixed by a test that asserts the first byte, so a future record kind cannot reintroduce the
  collision silently (`crates/session/tests/handshake_and_record.rs`).

### 17. Two tests fail without `zbarimg` — FIXED

- Was: `cargo test --workspace` failed two QR tests with "zbarimg is not on PATH: run the tests
  inside `nix develop`", so a clean checkout could not reach a green suite without an undeclared
  system dependency, and a test that fails for an environment reason teaches people to ignore
  failures.
- Now: both tests call `skip_without_zbar`, which prints `SKIP <what>: zbarimg is not on PATH ...`
  and returns early (`crates/daemon/src/qr.rs`). The skip is loud on stdout and the decode still runs
  wherever the binary exists, which is `nix develop` and CI.
- Verified both ways on this machine: without the binary the two tests skip and the suite is green,
  with it on `PATH` they run and pass.

## New defects found by the documentation comb, 2026-11-04

### 18. `poll` could emit one record more than it had room for — FIXED

- PROVEN. The retransmission step checked only `l2_slots` and did not spend the budget, then the
  new-data loop spent the full budget, so a due retransmission plus a non-empty queue emitted
  `min(records_left, l2_slots) + 1` records against a contract that said `min(...)`.
- Consequence: with a full credit window the daemon marked nine records in flight while the L2
  contract allows eight, and the *next* poll computed `l2_slots = 0`, closing the window for an
  extra round trip.
- Now: the new-data loop is bounded by `out.len() < l2_slots` as well as the budget
  (`arq.rs:568-572`), the retransmission exemption is stated in the doc comment rather than hidden,
  and `l2_slots_bound_new_data_not_only_retransmissions` pins it.
- The old test named for this bound only exercised new data with nothing due, so it could not fail.

### 19. Refusal accounting keyed on two different sequence spaces — FIXED

- PROVEN. The ARQ numbers records with a `u16` incremented only for new data; the daemon numbered
  the records it wrote with its own `u8` counter incremented for every record, acks included. The
  two agree until the first pure ack and drift after every one, so `on_send_refused` looked up a slot
  that did not exist, returned `false`, and the caller discarded the result: the refusal silently
  advanced the retry ladder, which is the defect `d00111c` set out to remove.
- Now: `Session::poll_with_seq` returns each sealed record with `Option<u16>` naming its slot
  (`crates/session/src/session.rs:120-160`), `SessionHost::poll_with_seq` keeps it
  (`crates/daemon/src/mesh_session.rs:254-290`), and `awaiting` stores the mapping
  `(wire_seq, instant, arq_seq)` (`crates/daemon/src/mesh.rs:691,903`) so a `SEND_RESULT` finds the
  right slot (`mesh.rs:770-806`).
- The same API closes the C ABI hole: `ldmux_session_take_record_seq` and
  `ldmux_session_refuse_seq` (`crates/session-ffi/src/lib.rs`), the header
  (`packages/mobile/native/ldmux_session.h`), the JNI wrapper
  (`packages/mobile/native/ldmux_session_jni.c`) and the Kotlin binding
  (`SessionLib.takeRecordSeq`/`refuseSeq`, `MeshSession.pollWithSeq`, `MeshTermDriver.transmit`).
- Tests: `a_sealed_record_keeps_the_arq_sequence_that_names_it`,
  `refusing_a_sealed_record_by_its_sequence_retries_at_the_radio_pace`,
  `a_refused_record_is_named_by_its_sequence_and_retried_at_the_radio_pace`.

### 20. The phone had no refusal hook at all — FIXED in code, UNVERIFIED on a device

- PROVEN (read, not run). On the phone a `Busy`, a local `Budget` refusal or a local "not idle"
  failure was swallowed by the driver's `else -> {}` and counted as an ARQ attempt; five of them
  ended the session and forced a handshake that costs more records than the burst did.
- Now the driver gives the attempt back for every outcome that means "never reached the air":
  `ErrBusy`, `ErrNotFound`, `ErrTooLong`, `ErrBudget`, `LocalTimeout`
  (`MeshTermDriver.kt`, `transmit`), by the ARQ number carried with the record.
- The mobile build was not compiled here (no Gradle, no Android SDK on this machine). The C side was
  syntax-checked against the NDK headers for `aarch64-linux-android29`; the Kotlin was written to
  keep every existing fake compiling, through default methods on `SessionPort`.

### 21. A refusal run had no wall-clock bound — FIXED

- A refusal gives the attempt back, so a radio that never takes the record never reaches
  `max_attempts`: the session stayed alive and retried every 350 ms for as long as the process ran.
  That is a wedged firmware, a wrong preset or a packet the firmware will never parse, presented as
  silence.
- Now a slot's refusal run starts at its first refusal and fails the session with
  `Failure::RadioRefused` after `REFUSAL_DEADLINE_MS = 30_000` (`arq.rs:265,555-575`). The daemon
  names the cause in `last_error` (`mesh_session.rs`), and the ABI's status bit is set for any
  terminal failure.
- Test: `a_refusal_that_outlives_the_deadline_fails_the_session`.

### 22. There is no pacer: a screen update leaves as a burst of up to eight — OPEN

- The firmware's transmit table fills under burst sending, measured at about 350 ms per fragment by
  meshcore-sar (`companion-wire-layouts.md:246-256`). LDMUX discovers that pace by being refused, one
  `ERR_CODE_TABLE_FULL` per excess record per burst. Since defect 21 landed a refusal costs nothing
  but time, so the recovery is sound; the burst is still refused rather than paced.
- Fix, not done: when `poll` would emit more than one record, emit one and let the daemon's existing
  "go round again now" loop (`mesh.rs:913`) send the next after `REFUSED_RETRY_MS`. That turns a
  refused burst into a paced trickle without a new timer.

### 23. `Stats::sent` counts control records and retransmissions — OPEN, naming

- `stats.sent` is incremented for every record emitted, retransmissions and acks included
  (`arq.rs`), while `retransmits` counts them separately. The name invites `sent - retransmits` as
  "new data", which is wrong by however many acks went out.
- Fix: rename to `emitted`, or add `new_data`. Cosmetic, but the counters are shown in
  `mesh_watch`.

### 24. No randomised initial retry — OPEN, low priority

- `rto()` is deterministic, so two ends that lose the same frame retry in lockstep. CoAP's
  `ACK_RANDOM_FACTOR` of 1.5 is the cheapest anti-collision device in the corpus's table
  (`retries-and-session-state.md:74-86`), and the phone's socket layer already carries the same
  lesson (`MeshTerminalScreen.kt:154-160` uses full jitter and says why).
- On a private two-radio channel there is almost no contention to spread, and jitter makes the ARQ's
  timing tests non-deterministic unless the source is injected. Left open deliberately.

## Score

- 11 fixed (2, 3, 4, 6, 7, 16, 17, 18, 19, 20, 21), one partial (5), three corrected or withdrawn
  (10, 12, 13), one open at S1 (1), and the rest open by choice or because they are deployment
  facts rather than code.
- Every open item above names its smallest correct fix and the file it lives in.
