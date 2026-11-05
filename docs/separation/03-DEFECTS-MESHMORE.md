# Defects in `meshmore-sns`

- Written 2026-11-04 against branch `feat/ldmux-gateway` at `5d97625`.
- **Re-verified against the same commit later the same day, after the separation decision.** The
  items marked VERIFIED below were read again in the tree at `5d97625`, not carried over. The ones
  that were not re-read say so; treat them as claims from the first pass.
- **One of these was fixed; the rest are reported, not fixed.** S1 #1 (the deafness watchdog) was
  changed on 2026-11-04 in `meshmore-sns` commit `1e66a8d` after the device run in
  `../MOBILE-UX-AUDIT.md` §2c.7 measured it rebooting a healthy radio every two minutes and dropping
  the phone's link each time. Everything else stands as reported: MeshMore is its own product now
  (`06-SEPARATION-PLAN.md`), and several of the items here are deliberate behaviour with tests behind
  them, so changing them is a product decision rather than a bug fix. What LDMUX can do about each of
  them is stated in the separation plan.
- Severity: S1 blocks a user or loses data, S2 wrong behaviour under a real condition, S3 documentation or consistency, S4 hygiene.

## S1

### 1. The deaf-radio detector restarts the gateway on a quiet but healthy mesh — FIXED 2026-11-04

- Still true at `5d97625`. `radioLooksDeaf` (`serial_mesh_sender.dart:63-69`) consults only
  `_heardAirAt` and the time since the port opened; `_sentSinceHeard` is incremented on every send
  (`:256`) and still read only by `_pathIsSuspect` (`:407`). The gateway reboots and exits 75 on a
  30 s timer (`bin/meshmore_gateway.dart:149-165`), driven by a measurement the comment records
  (350 contact frames in 70 s, one air packet in 25 minutes, rssi -128).
- Deliberate, with tests (`test/serial_mesh_sender_test.dart`). On a private link that is simply
  quiet it reboots a healthy radio every two minutes: the USB device re-enumerates, the daemon
  reports `state down`, and the restart count rises (the observed run reached `NRestarts=901`).
- The fix is a rule change, not a code repair: require N unanswered sends to a bound peer, and treat
  "no adverts" as no evidence.

**Fixed 2026-11-04** (`1e66a8d`, in `meshmore-sns`). The rule is now "a deaf radio is one that was
asked and did not answer": `_lastSentAt` records when a record was last handed to the radio, and
silence before that is idleness rather than deafness. A send older than the window with no air packet
since is still deaf, so the failure the watchdog exists for is still caught. Three tests, two of them
rewritten to the new rule plus "an idle link is not deaf: silence with nothing asked is not
evidence" (39 in the package, all passing).

The measurement that prompted it, from the device run: the gateway exited 75 at `21:24:16`, 2m04s
after opening, having heard nothing because nothing had been sent — the daemon sends no records while
the phone sits at a prompt — and the phone answered "the host did not answer" every time. After the
fix the gateway stayed up for the rest of the session (no exit between `21:28:17` and the end of the
run) against one every ~2 minutes before.

## S2

### 2. `PathMode.flood` does not flood — OPEN, VERIFIED

- Still true at `5d97625`: `_pathForSend` returns an empty path for `PathMode.flood`
  (`serial_mesh_sender.dart:412-414`) and the frame is `0x19 0x00` plus payload
  (`:261`), which the firmware sends as a zero-hop direct.
- The enum's own docstring says every record floods (`serial_mesh_sender.dart:26-33`).
- The real flood carrier is dead code: `MeshCarryCodec.sendBinaryReq` (`mesh_carry.dart:150`) and `lengthPrefixedRecord` (`:171`) appear only in the codec, its tests and a vector file. `GatewayLimits.discoveryRecordMax` (`meshcore_gateway/lib/src/constants.dart:62`) appears only in the constants and one test.
- Consequence: a user who selects flood to reach a peer two hops away gets a send that never leaves the neighbour, and the radio still answers `OK`.

### 3. `sendRawData` writes a byte count where the firmware decodes an encoded value — OPEN, VERIFIED

- Still true at `5d97625`: `mesh_carry.dart:207` writes `path.length` as the length byte and
  `:193-195` refuses a path over `0x7F`. The firmware reads that byte twice, as a byte count and as
  the packed hop-count/hash-width field, so the two agree only at a one-byte hash.
- The firmware reads that byte as a byte count in its bounds check (`MyMesh.cpp:1519-1521`) and as an encoded value in `Packet::writePath` (`src/Packet.cpp:20-23`), where `hash_count = path_len & 63` and `hash_size = (path_len >> 6) + 1`.
- The two readings agree only at a one-byte path hash. At two or three bytes the packet carries the wrong path length and the wrong number of bytes.
- `mesh_carry.dart:193` also refuses a path longer than `0x7F`, which caps a two-byte-hash path at 63 hops.
- Consequence: the host is correct by accident at the default hash size and wrong at any other, and the failure looks like a route that never answers.
- Upstream context, added 2026-11-04 from the field research: MeshCore v1.18.0 (released 2026-10-15) fixed this in firmware, and states that raw data over 2- and 3-byte hashes was broken before it. The radios here run `companion-v1.17.1`, so the host is wrong on this firmware regardless of what any client does, and a firmware upgrade is part of the fix.

### 4. The host and the phone use different carriers for the same send — OPEN, VERIFIED

- Still true at `5d97625`: the host always sends `0x19` (`serial_mesh_sender.dart:261`); LDMUX's
  phone sends `0x41` for every record (`AndroidRadioLink.kt:125-137,474-481`).
- Consequence: the two ends of the same link exercise different firmware paths, so a defect in one is invisible from the other, and the phone's workaround documents a firmware behaviour the host still gets wrong.

### 5. The host has no reconnect logic

- PROVEN. Every recovery path is an `exit(75)` plus systemd `Restart=on-failure` with `RestartSec=5` and `StartLimitBurst=10` per 300 s (`bin/meshmore_gateway.dart:165,192`, `deploy/meshmore-gateway.service:20-27`).
- Consequence: ten failures in five minutes leave the unit failed and the host radio gone until someone intervenes, and the deaf-radio rule in defect 1 is a reliable way to reach ten.

## Feature gaps, not defects

- These are verified against source in the research corpus and belong to MeshMore's roadmap. They are listed so that the register above is not read as the whole picture.
- `0x3E CMD_SEND_CHANNEL_DATA` has a constant and no encoder, which blocks every channel media format in the field.
- `0x42 CMD_RUN_CLI_COMMAND` is not declared at all.
- The `0x1B` channel datagram decodes and is then dropped, because nothing in the app's dispatch consumes it.
- The "via" byte is parsed and discarded on `0x84` and `0x1B`, so a second radio is invisible to the app.
- The gateway is the only consumer of `MeshCarryCodec`, so the gateway is MeshMore's only route to raw sends and should not be deleted as cleanup.

## S3

### 6. The protocol coverage document is stale by about 21 opcodes

- PROVEN. `meshmore-sns-protocol-coverage.md:37` says 37 of 58 implemented and `:57` says 21 still missing.
- `meshmore-sns-ux-coverage.md:3` says all 58 are complete, and the code agrees: 58 constants (`meshcore/lib/src/codec/constants.dart:170-407`) and 57 encoders (`frame_codec.dart`).
- Consequence: anyone quoting the coverage document understates the library by a third.

### 7. The XR app's own brief says it does not exist

- PROVEN. `meshmore-xr/MeshmoreXR-design-brief.md:8-9` says "proposal for review, no production Android code exists yet".
- The app is version 0.3.0 build 83 (`app/build.gradle.kts:44-47`), with `MainActivity.kt` at 6,718 lines, `MeshLink.kt` at 2,276 lines and about 60 test files.
- Consequence: the documentation understates a shipping product by an entire product.

### 8. The radio preset is recorded as a property when it is a dated observation

- PROVEN. `meshmore-sns-radio-config.md:190,205,317` and `meshmore-sns-architecture.md:647` state 910.525 MHz / 62.5 kHz / SF7 / CR5 as confirmed from the radio.
- Nothing in the repository pins that value. The host prints whatever the radio reports (`bin/meshmore_gateway.dart:130-133`) and the phone builds its estimator from `SelfInfo` (`gateway_service.dart:207-215`).
- Consequence: a reader treats a measurement from one radio on one day as the system's configuration, which is how the five-way preset conflict in `05-RADIO-TUPLE.md` survived.

### 9. Three payload limits are quoted without saying which carrier each belongs to

- PROVEN. 172 bytes for raw custom (`meshmore-sns-architecture-audit.md:57`), 160 bytes for a text message (`meshmore-sns-meshcore-interop.md:451`), 142 bytes for a discovery record (`meshmore-sns-architecture.md:400`).
- All three are correct for their own carrier, and no single document says so.
- Consequence: a reader can take any of them as "the" limit and size a buffer wrong.

### 10. Test counts disagree inside one file — OPEN, VERIFIED

- Still true at `5d97625`: `README.md:10` says 1014, `README.md:98` says 125, `README.md:130` says
  940, `meshmore_sns_app/README.md:40,51` say 1014 and 940, and
  `packages/meshcore_host/README.md:38` says 30.

## S4

### 11. One opcode has a constant and no encoder — OPEN, VERIFIED

- Still true at `5d97625`: `sendChannelDatagram` (`0x3E`) is declared at `constants.dart:273` and
  referenced nowhere else.

### 12. The firmware reference in the docs is behind the tree

- PROVEN. `meshmore-sns-radio-config.md:192` records `v1.17.1-d929643`. The fork's HEAD is `685a869`, three commits further on: `2955792` ("ble: keep one bond record per peer, or the radio can never encrypt again"), `2fc1361` ("ble: report the bond store, and make the prune testable") and `685a869` (a Heltec V4 FEM rail change on every boot that is not a deep-sleep wake). Only the FEM change was noted here before.
- The **bond-store** pair matters to LDMUX: the phone's BLE path and any re-bond after a pairing reset depend on the peripheral keeping one bond per peer, and that fix is not on the radios. This is a firmware deployment item, not a code item.
- The clone is shallow, with the boundary at `81dbae5`, which `mesh-research/notes/topics/upstream-direction.md` §1 identifies as the v1.18.0 tag commit: the objects for 1.18.0 are reachable for a read-only `git show` without fetching, even though HEAD is three commits past 1.17.1.
- Consequence: the document describes what the radios run, which is correct, but a reader comparing it to the tree sees a mismatch with no explanation.
