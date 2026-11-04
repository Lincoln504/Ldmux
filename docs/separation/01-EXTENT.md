# To what extent LDMUX uses MeshCore

- Written 2026-11-04. Sources: `ldmux` at `c96d316`, `meshmore-sns` at `5d97625`, `meshcore-fw-alerts` at `685a869`.
- The short answer: LDMUX is built on top of MeshCore as a radio and a packet carrier, and around MeshCore's application layer. It does not replace the firmware and cannot without reflashing.

## The layer table

| Layer | Used by LDMUX | Evidence | What it means |
|---|---|---|---|
| Radio firmware | Yes, unmodified | `meshcore-fw-alerts` is a fork of MeshCore, and the radios run `companion-v1.17.1` at `d929643` (`meshmore-sns-radio-config.md:192`) | LDMUX cannot change modulation, timing or the outbound queue without a firmware change |
| Packet and OTA format | Yes | `createRawData` sets `PAYLOAD_TYPE_RAW_CUSTOM` (`src/Mesh.cpp:589-603`), and the record rides inside it | The 176-byte packet, the header, the path encoding and the airtime are MeshCore's |
| Companion protocol | Yes, and extended | `0x19` raw data and `0x41` raw packet are handled at `examples/companion_radio/MyMesh.cpp:1519-1532`, `PUSH_CODE_RAW_DATA` at `0x84` | LDMUX depends on a carrier that upstream does not document |
| MeshCore application layer | No, bypassed | No channel PSK, no channel AES, no advert-derived identity, auto-drain disabled (`MeshcoreSession.java:101,534`) | Channels, DMs, contacts-as-identity and adverts are not LDMUX's trust model |
| Session, ARQ, terminal | No, entirely LDMUX's | `crates/session`, ADR-0020, ADR-0025 | The end-to-end guarantees are LDMUX's, not MeshCore's |

## What is load-bearing

- The firmware, because the raw-custom carrier exists only there. `CMD_SEND_RAW_DATA` and `CMD_SEND_RAW_PACKET` appear nowhere in `docs/companion_protocol.md`, so the only specification is the firmware source plus the patched Java library.
- The packet format, because the record is a `RAW_CUSTOM` payload and the path encoding decides whether a send reaches the peer.
- The companion protocol, because the phone and the host both speak it, and both have patched it.
- The firmware's own airtime governor, because `sendDirect` goes through `sendPacket` and `Dispatcher::checkSend` (`src/Dispatcher.cpp:275`), which applies `_prefs.airtime_factor` (default 1.0, so a 50 percent duty cycle) and a channel-busy check. LDMUX's own 36 s per hour meter sits inside that, so the effective ceiling is the lower of the two.

## What is not load-bearing

- MeshCore's channels, PSKs and channel encryption. LDMUX carries its own Noise session, so a channel would add a second cipher and no security.
- MeshCore's contact identity. LDMUX pairs by its own key material, so a MeshCore contact is only a routing entry.
- MeshCore's adverts as a discovery mechanism. They are used as a liveness signal and as a route source, not as identity.
- MeshCore's text messages. The record is raw custom, so the 160-byte text limit does not apply.

## The consequence that matters

- `writeOKFrame()` fires immediately after `sendDirect` returns (`MyMesh.cpp:1524-1525`), so `OK` means "accepted into the outbound queue", not "handed to the transmitter".
- The queue can then drop the packet on airtime budget or channel busy, and the sender learns nothing.
- This is why LDMUX needs its own acknowledgement above the carrier, and why a send that returns `OK` can still never arrive.

## The structural cost

- On the phone, LDMUX owns the radio: Kotlin `AndroidRadioLink` calls the vendored Java library, which speaks BLE GATT to the companion.
- On the host, LDMUX does not own the radio: `ldmuxd` opens a loopback TCP connection to MeshMore's Dart gateway, which owns the USB serial port.
- So "standalone" is true on the phone and false on the host, and the host path depends on a second repository's process, wire contract, token and release cadence.
- That dependency is the single largest structural cost in the design, and `06-SEPARATION-PLAN.md` is about removing it.

## The divergence that proves the cost

- LDMUX's vendored `libmeshcore-android` contains `RawDataFrame`, `StatsFrame`, `sendRawData`, `sendRawPacket`, `isIdle` and a tracked-command queue (`MeshcoreSession.java:129,539,572,615,883-919`).
- MeshMore's own `libmeshcore-android` contains none of them, and `libmeshcore` has no `RawDataFrame` or `StatsFrame` at all.
- Two libraries at the same firmware pin have silently diverged, and the carrier LDMUX depends on exists only in LDMUX's copy.
