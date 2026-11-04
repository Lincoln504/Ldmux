# Sources and protocol facts

- Written 2026-11-04. Every fact here was read in a local tree at the commit named, unless it is marked EXTERNAL.
- EXTERNAL means it came from web research and was not independently verified against a local source. Treat those as leads.

## Firmware, `meshcore-fw-alerts` at `685a869`

| Fact | Evidence |
|---|---|
| Raw custom is a payload type, not a channel | `src/Mesh.cpp:589-603`, `createRawData` sets `PAYLOAD_TYPE_RAW_CUSTOM` |
| `CMD_SEND_RAW_DATA` reads a path length byte, then a path, then at least 4 payload bytes | `examples/companion_radio/MyMesh.cpp:1519-1521` |
| The same byte is a byte count in the bounds check and an encoded value in `writePath` | `MyMesh.cpp:1519-1521` against `src/Packet.cpp:20-23` |
| A negative path length is refused as unsupported, with the comment "flood, not supported (yet)" | `MyMesh.cpp:1529-1531` |
| `sendDirect` sets `ROUTE_TYPE_DIRECT` and copies the path | `src/Mesh.cpp:693-695` |
| Flood is `0x01` and direct is `0x02` | `src/Packet.h:15-16` |
| `OK` is written immediately after `sendDirect` returns, so it means queued, not transmitted | `MyMesh.cpp:1524-1525` |
| The outbound queue is gated by an airtime budget and a channel-busy check | `src/Dispatcher.cpp:275` |
| The airtime budget factor defaults to 1.0, which is a 50 percent duty cycle | `examples/companion_radio/MyMesh.cpp:257-259` |
| CAD is disabled in the companion build | `MyMesh.cpp:261-266`, `src/Dispatcher.h:170-171` |
| The dispatcher re-applies the CAD setting on every noise-floor recalibration | `src/Dispatcher.cpp:68-69` |
| CAD fail maximum duration is 4000 ms | `src/Dispatcher.cpp:62-64` |
| Serial framing is `'<' len_lo len_hi payload` to the radio and `'>' len_lo len_hi payload` from it | `src/helpers/ArduinoSerialInterface.cpp:31,46`, `src/helpers/BaseSerialInterface.h` |
| The firmware frame ceiling is 176 bytes | `src/helpers/BaseSerialInterface.h` |

## Protocol, `meshmore-sns` at `5d97625`

| Fact | Evidence |
|---|---|
| The Dart library is pinned to `companion-v1.15.0` at `dee3e26` | `meshcore/lib/src/codec/constants.dart:22-24` |
| 58 command constants, 57 encoders | `constants.dart:170-407`, `frame_codec.dart:38-769` |
| `sendChannelDatagram` at `0x3E` has a constant and no encoder | `constants.dart:273` |
| Inbound decode is total, with unknown opcodes becoming `UnsupportedFrame` | `frame_codec.dart:776`, `inbound.dart:425` |
| `PUSH_CODE_RAW_DATA` is `0x84` | `frame_codec.dart:1015` |
| The gateway contract is version 1 with a 7-byte header and a 512-byte payload ceiling | `meshcore_gateway/lib/src/constants.dart:10-30` |
| The gateway is loopback only, one client at a time, constant-time token compare | `server.dart:17-20,39`, `pairing_store.dart:80-82` |
| Port 41731 is a preference and the real port must be read after start | `server.dart:44-56` |
| The airtime estimator is built from `SelfInfo` on both ends | `gateway_service.dart:207-215`, `bin/meshmore_gateway.dart:88-93` |

## LDMUX, `ldmux` at `c96d316`

| Fact | Evidence |
|---|---|
| The app's compiled default is 910.525 / 500 / SF7 / CR5 | `packages/mobile/composeApp/src/commonMain/kotlin/dev/ldmux/mobile/mesh/Radio.kt:66-73` |
| The phone sends `0x41` when the encoded path exceeds `0x3F` | `AndroidRadioLink.kt:311-321` |
| The reason is written down: the firmware treats the encoded byte as a byte count | `AndroidRadioLink.kt:124-132` |
| Error codes are mapped 3 and 4 to busy, 6 to too long | `AndroidRadioLink.kt:506-515` |
| The tracked queue has no deadline | `MeshcoreSession.java:883-919` |
| Auto-drain defaults to true and is settable | `MeshcoreSession.java:101,534` |
| The bridge polls every 50 ms and takes the daemon-wide mutex across an await | `crates/daemon/src/mesh_bridge.rs:739,347,653` |
| `awaiting` is decremented only by a `SendResult` frame | `crates/daemon/src/mesh.rs:706,782` |
| The reconnect backoff is never reset | `crates/daemon/src/mesh.rs:548,581` |
| Timing instrumentation is gated on `LDMUX_MESH_TIMING=1` | `crates/daemon/src/mesh.rs:419,736,759,764,778` |

## External, not independently verified

- MeshCore's companion protocol document does not describe `CMD_SEND_RAW_DATA`, `CMD_SEND_RAW_PACKET` or `PUSH_CODE_RAW_DATA`. The only specification is the firmware source and the patched client libraries. This matches the local reading of `docs/companion_protocol.md` in the firmware tree, which contains no raw-send section.
- Community clients exist in several languages, including a JavaScript and a Python implementation, and a CLI. Their handling of radio configuration is reported to be "read the radio, do not assume a preset", which is the same conclusion `05-RADIO-TUPLE.md` reaches from the local evidence. EXTERNAL, and worth confirming against a specific client before citing it in a design document.
- MeshCore's own preset list is published as a configuration catalogue rather than a firmware constant, which is why no firmware build carries a default tuple. EXTERNAL, consistent with `meshmore-sns-radio-config.md:10-24`.

## What was not verified

- No radio was driven during this pass, so every airtime figure is arithmetic from a cited time-on-air, not a measurement.
- The Kotlin and Android suites were not run.
- The firmware tree is ahead of the build the radios run, so a fact read at `685a869` may not hold at `d929643`. The raw-send and path-encoding code was checked in both places where it mattered.
