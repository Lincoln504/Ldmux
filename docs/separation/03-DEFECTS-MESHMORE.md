# Defects in `meshmore-sns`

- Written 2026-11-04 against branch `feat/ldmux-gateway` at `5d97625`.
- Severity: S1 blocks a user or loses data, S2 wrong behaviour under a real condition, S3 documentation or consistency, S4 hygiene.

## S1

### 1. The deaf-radio detector restarts the gateway on a quiet but healthy mesh

- PROVEN. `radioLooksDeaf` returns true when no packet has arrived off the air for two minutes, measured from process start (`meshmore_sns_app/packages/meshcore_host/lib/src/serial_mesh_sender.dart:63-70`).
- `_heardAirAt` is written in exactly two places, both inbound frames: `RawDataFrame` (`:431`) and `NewAdvertFrame` (`:438`).
- `_sentSinceHeard` exists and is incremented on every send (`:114,256`), but only `_pathIsSuspect` reads it (`:407`). The deaf check ignores it, and ignores whether any client is bound.
- The gateway then reboots the radio and exits 75 (`bin/meshmore_gateway.dart:148-165`), and systemd restarts it (`deploy/meshmore-gateway.service:20-27`).
- Consequence: on a private two-radio link where nothing is sent for two minutes, the gateway reboots the radio, the USB device re-enumerates, and the daemon reports `state down` with a rising restart count. The observed run reached `NRestarts=901` with a restart every two minutes.
- The behaviour is deliberate and tested (`test/serial_mesh_sender_test.dart:308,317`), so the fix is a change of rule, not a bug fix: require N unanswered sends to a bound peer, and treat "no adverts" as no evidence.

## S2

### 2. `PathMode.flood` does not flood

- PROVEN. `_pathForSend` returns an empty path for `PathMode.flood` (`serial_mesh_sender.dart:412-415`), which becomes `0x19 0x00 <payload>` (`:261`).
- The firmware turns that into `sendDirect(pkt, path, 0)` (`examples/companion_radio/MyMesh.cpp:1524`), which sets `ROUTE_TYPE_DIRECT` with `path_len = 0` (`src/Mesh.cpp:693-695`). That is a zero-hop direct send, one hop to a neighbour.
- The enum's own docstring says every record floods (`serial_mesh_sender.dart:26-33`).
- The real flood carrier is dead code: `MeshCarryCodec.sendBinaryReq` (`mesh_carry.dart:150`) and `lengthPrefixedRecord` (`:171`) appear only in the codec, its tests and a vector file. `GatewayLimits.discoveryRecordMax` (`meshcore_gateway/lib/src/constants.dart:62`) appears only in the constants and one test.
- Consequence: a user who selects flood to reach a peer two hops away gets a send that never leaves the neighbour, and the radio still answers `OK`.

### 3. `sendRawData` writes a byte count where the firmware decodes an encoded value

- PROVEN. The frame writes `path.length` as the length byte (`mesh_carry.dart:205-209`).
- The firmware reads that byte as a byte count in its bounds check (`MyMesh.cpp:1519-1521`) and as an encoded value in `Packet::writePath` (`src/Packet.cpp:20-23`), where `hash_count = path_len & 63` and `hash_size = (path_len >> 6) + 1`.
- The two readings agree only at a one-byte path hash. At two or three bytes the packet carries the wrong path length and the wrong number of bytes.
- `mesh_carry.dart:193` also refuses a path longer than `0x7F`, which caps a two-byte-hash path at 63 hops.
- Consequence: the host is correct by accident at the default hash size and wrong at any other, and the failure looks like a route that never answers.

### 4. The host and the phone use different carriers for the same send

- PROVEN. The host always sends `0x19` (`serial_mesh_sender.dart:261`). The phone sends `0x41` whenever the encoded path exceeds `0x3F` (`AndroidRadioLink.kt:311-321`), with the reason written at `:124-132`.
- Consequence: the two ends of the same link exercise different firmware paths, so a defect in one is invisible from the other, and the phone's workaround documents a firmware behaviour the host still gets wrong.

### 5. The host has no reconnect logic

- PROVEN. Every recovery path is an `exit(75)` plus systemd `Restart=on-failure` with `RestartSec=5` and `StartLimitBurst=10` per 300 s (`bin/meshmore_gateway.dart:165,192`, `deploy/meshmore-gateway.service:20-27`).
- Consequence: ten failures in five minutes leave the unit failed and the host radio gone until someone intervenes, and the deaf-radio rule in defect 1 is a reliable way to reach ten.

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

### 10. Test counts disagree inside one file

- PROVEN. `README.md:8` says 1014 tests. `meshmore_sns_app/README.md:52` says 1014 and `:57` says 940.
- `packages/meshcore_host/README.md:44` says 30 tests, and the three test files contain about 41.

## S4

### 11. One opcode has a constant and no encoder

- PROVEN. `sendChannelDatagram` (`0x3E`) is declared at `constants.dart:273` and referenced nowhere else, which makes "all 58" off by one.

### 12. The firmware reference in the docs is behind the tree

- PROVEN. `meshmore-sns-radio-config.md:192` records `v1.17.1-d929643`. The fork's HEAD is `685a869`, which adds a Heltec V4 FEM rail change.
- Consequence: the document describes what the radios run, which is correct, but a reader comparing it to the tree sees a mismatch with no explanation.
