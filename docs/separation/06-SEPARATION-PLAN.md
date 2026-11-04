# Separating LDMUX from MeshMore

- Written 2026-11-04. This reverses the earlier decision to keep MeshMore's gateway as LDMUX's host transport.
- The goal: LDMUX must not need MeshMore at runtime, at build time, or at release time.

## What separation means, as five tests

1. No shared process. `ldmuxd` opens the serial port itself, and no other repository's daemon has to be running.
2. No shared wire contract. No `MG` frames, no pairing token, no Binder handoff, no port discovery.
3. No shared record format. The record is LDMUX's, carried inside a raw-custom packet.
4. No shared release. A MeshMore release cannot break LDMUX, and a LDMUX release cannot break MeshMore.
5. No shared documentation. Each repository documents its own side of the radio.

## The coupling that exists today

| Coupling | Where | Pinned by |
|---|---|---|
| L2 wire contract v1 | `meshcore_gateway/lib/src/constants.dart:10-90` | version 1, magic `MG`, 7-byte header, 512-byte payload |
| Record ceiling and floor | `constants.dart:44-58` | 172 and 160 bytes |
| Credit window | `constants.dart:38` | 8 unacknowledged records |
| Pre-auth timeout | `constants.dart:33` | 5 seconds |
| Pairing token | `constants.dart:64`, `pairing_payload.dart:10` | 16 bytes, `meshmore://gateway?v=1&host=...&token=<32 hex>` |
| Binder handoff | `GatewayHandoff.kt:62`, `AndroidManifest.xml:14` | permission `com.iotone.meshmore_sns_app.permission.GATEWAY_HANDOFF`, contract version 1 |
| Host daemon | `meshcore_host/bin/meshmore_gateway.dart` | the process LDMUX connects to |
| Firmware pin | `meshcore/README.md:14-22` | `companion-v1.15.0` at `dee3e26` |

- The phone side is already independent: LDMUX's Kotlin talks to its own vendored Java library over BLE.
- The host side is the whole problem, and it is one process and one wire contract.

## The target

- A Rust companion client inside `ldmuxd`, speaking the MeshCore companion protocol over the serial port directly.
- Why Rust and not Dart: `ldmuxd` is already Rust, so this adds no second runtime, no second process, no IPC, and no second release.
- Why not a shared native library: the architecture of record rejected it (`meshmore-sns-architecture.md:280`), and it would couple the two repositories more tightly, not less.

### What the client has to implement

| Piece | Detail | Evidence |
|---|---|---|
| Serial transport | Open by `/dev/serial/by-id/...`, same line settings as the Dart port | `meshcore_host/lib/src/serial_port.dart` |
| Framing | Host to radio `'<' len_lo len_hi payload`, radio to host `'>' len_lo len_hi payload`, little-endian length excluding the three header bytes, 176-byte ceiling, incremental decoder, sticky error on a bad length | `serial_framing.dart:25-30`, `src/helpers/ArduinoSerialInterface.cpp:31,46`, `src/helpers/BaseSerialInterface.h` |
| Command queue | One tracked command in flight, one raw send slot, a deadline per command that clears the slot and reports failure | `MeshcoreSession.java:883-919` is the shape, and it is missing the deadline |
| Raw send | `0x41` only, with the whole packet built by LDMUX | `MyMesh.cpp:1519-1532`, `AndroidRadioLink.kt:311-321` |
| Contacts and paths | Contact sync, path updates, a path cache with a trust rule | `serial_mesh_sender.dart:236-290,402-415` |
| Stats | `CMD_GET_STATS` for `tx_air_secs`, `rx_air_secs`, `noise_floor`, `last_rssi`, `last_snr`, `recv`, `recv_errors` | opcode 56, firmware `MyMesh.cpp` |
| Health | Reboot only on N unanswered sends to a bound peer, never on silence alone | `03-DEFECTS-MESHMORE.md` defect 1 |
| Airtime | Build the estimator from `SelfInfo`, not a constant, with the `de` rule and the 32/16 preamble split, and test it against the known 613 ms value | `gateway_service.dart:207-215`, `Radio.kt:196-211` |
| Retry timeout | Derive the first retry from the preset and the hop count, using the firmware's own formula 500 + ((airtime x 6 + 250) x (hops + 1)) ms, so a retry never fires before an answer could arrive | `08-RESEARCH-INTEGRATION.md` |
| Path codec | Pack and unpack hop count in bits 0 to 5 and hash width minus one in bits 6 and 7, with `0xFF` as no path. Never store the byte raw | firmware `Packet::writePath`, research `protocol-and-firmware.md` |
| Firmware version | Report it in status for diagnostics. **No carrier gate is needed**: the 1.18.0 fix is in the `0x19` handler and both sides read a whole packet through `0x41` (`02-DEFECTS-LDMUX.md` defect 7) | firmware release notes, `08-RESEARCH-INTEGRATION.md` |

### Where it goes

- A new crate, `crates/companion`, with `transport.rs`, `framing.rs`, `session.rs`, `codec.rs` and `health.rs`.
- `crates/mesh` keeps the L2 session and the record format, and gains a `MeshSender` trait with two implementations: the serial client and, for one release, the existing gateway bridge.
- `crates/daemon/src/mesh_bridge.rs` becomes the gateway implementation of that trait, then is deleted.

## Phases

### P0, freeze the carrier, no new code

- Use `0x41` for every raw send in both repositories and delete the `0x19` path.
- Fix the error-code mapping in `AndroidRadioLink.kt:506-515`.
- Add the tracked-command deadline in `MeshcoreSession.java`.
- Reset the reconnect backoff on a bound session, and decay `awaiting`.
- This phase is worth doing even if the separation never happens, because it removes the ambiguity that produced the host and phone divergence.

### P1, the Rust client behind a trait

- Implement `crates/companion` against recorded frames, not a live radio.
- Reuse the Dart vector sets as conformance fixtures, and add a test that decodes every vector the phone's Kotlin also decodes.
- No daemon change yet, so the existing path keeps working.

### P2, cut over

- `ldmuxd` prefers the serial client and falls back to the gateway only when a flag is set.
- Run both against the same radio on the bench and compare the record stream.
- The fallback exists for one release and is then removed.

### P3, delete

- `crates/daemon/src/mesh_bridge.rs` and the gateway client in `crates/mesh`.
- The token handshake, `/tmp/gw.json`, and the port discovery.
- The Binder handoff client on the phone, if it exists only for the gateway.
- Every MeshMore reference in `docs/MESH-GATEWAY.md`, `docs/MESH-HOST-RADIO.md` and `docs/STATE.md`.

### P4, decide the tuple

- Take the decision in `05-RADIO-TUPLE.md`, then make the documentation state one tuple per document with the date it was observed.

## The firmware boundary

- **The carrier does not depend on the firmware version, and an earlier revision of this plan was
  wrong to say so.** The 1.18.0 raw-data fix is in the `CMD_SEND_RAW_DATA` (`0x19`) handler, which
  reads the path-length byte two ways. LDMUX's client sends `CMD_SEND_RAW_PACKET` (`0x41`) for every
  record, and that opcode hands the whole packet to `Packet::readFrom` on both sides of 1.18.0. So
  `0x41` is correct on the firmware in the field and on newer firmware, and no version gate is
  needed for the raw path.
- The version is still worth reading and showing, for a different reason: it tells an operator which
  firmware each end runs when a symptom looks like a firmware bug, and a mixed pair is a real
  configuration rather than a fault. That is diagnostics, not a branch in the send path.
- What *does* change across 1.18.0 is the multi-interface companion (BLE and WiFi at once). LDMUX's
  single-central assumption stays true for BLE, and a host reaching the same node over TCP is a
  second path into one radio, not a second radio.

## What stays shared

- The firmware, which is a binary on the radio and not a repository dependency.
- The companion protocol, which is a firmware interface and not MeshMore's property.
- Nothing else.

## What MeshMore keeps

- Its gateway, its host daemon and its Binder handoff stay, because they serve MeshMore's own app and any third-party client.
- LDMUX simply stops being one of those clients, which is a smaller change for MeshMore than for LDMUX.

## Risks

- The raw-custom carrier is undocumented upstream, so the firmware commit is the specification. Mitigate by pinning the firmware and keeping the vectors as the executable spec.
- Two codecs remain, Kotlin on the phone and Rust on the host. Mitigate with shared JSON vectors and a conformance test, because a shared native library was already rejected.
- The airtime governor exists twice, LDMUX's meter and the firmware's 50 percent budget. Decide which one is the policy and document the other as a backstop.
- The serial port is a single-owner resource, so the cutover must not run both clients against one radio at the same time.
