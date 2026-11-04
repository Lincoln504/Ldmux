# What the field research adds, and what it changes here

- Written 2026-11-04, after the research corpus at `/home/ldeen/Documents/mesh-research/notes/` was written by the parallel track between 10:50Z and 15:01Z.
- That corpus is 20 topic notes, 6 repo notes, a synthesis and a gap register, built from 14 cloned upstream repositories and the MeshCore release blog.
- This document records only what changes a conclusion in `01` to `07`, plus the new facts that matter to LDMUX. It does not restate the corpus.

## What it confirms

- The firmware carries no radio preset. The corpus reaches this from the preset documentation and from the two-profile radio specification, which is the same conclusion `05-RADIO-TUPLE.md` reaches from the code.
- The airtime formula and the 32/16 preamble split are as `RadioAirtime` implements them (`packages/mobile/composeApp/src/commonMain/kotlin/dev/ldmux/mobile/mesh/Radio.kt:196-211`). The corpus gives the formula in full, and substituting a 176-byte packet at 62.5 kHz and SF7 reproduces the 613 ms in `meshmore-sns-radio-config.md:53` exactly, which validates both.
- The raw-custom carrier is undocumented upstream. The corpus confirms that `PAYLOAD_TYPE_RAW_CUSTOM` has no defined format, in one line of the payload reference.

## What it corrects

### The airtime arithmetic in `05-RADIO-TUPLE.md` was too high

- Recomputed with the full formula, a 176-byte packet costs 75.3 ms at 500 kHz SF7 and 137.9 ms at 500 kHz SF8, not the 66 and 132 the earlier pass used.
- At a 1 percent duty cycle that is 478 records per hour at SF7 and 261 at SF8, not 545 and 273.
- The 62.5 kHz figure of 58.7 stands, and it is the only one of the three that came from a measurement rather than an estimate.
- The conclusion does not change: the documents are still four to eight times too pessimistic for the running link.

### The `path_len` bug is upstream, and upstream fixed it in 1.18.0

- MeshCore v1.18.0, released 2026-10-15, lists a `CMD_SEND_RAW_DATA` multi-byte path fix: raw data over 2- and 3-byte path hashes was broken before it.
- The corpus states the rule plainly: any raw-data feature must gate on firmware at or above 1.18.0.
- The radios here run `companion-v1.17.1` at `d929643`, so they are below the fix, which is why the phone's `0x41` workaround is correct today.
- What this changes: the `0x41`-only rule is a workaround for a firmware version, not a permanent design. The plan should gate on the firmware version and record which side of 1.18 each radio is on, because the carrier choice legitimately differs.

## What is new and matters to LDMUX

### The ARQ retry is shorter than the firmware's own timeout past one hop

- This is a new defect, found by putting the corpus's timeout formula against `crates/session/src/arq.rs:128`.
- The firmware's direct-mode timeout is 500 + ((airtime x 6 + 250) x (hops + 1)) ms. At 500 kHz SF8 with 137.9 ms of airtime, a two-hop path gives 500 + (1077 x 3) = 3,731 ms, and a three-hop path gives 4,808 ms.
- `Config::for_payload` sets `rto_initial_ms: 2_000` (`arq.rs:128`), so on anything past a direct neighbour the first retry fires before the answer could arrive.
- The cost is one duplicate record per expiry, which at a 36 s per hour budget is real airtime, and the comment's own justification (about 10 times the round trip at 62.5 kHz) holds only for the neighbour case.
- Fix: derive `rto_initial_ms` from the preset and the hop count the path reports, or raise it to cover the firmware's own timeout, and keep the doubling.

### The session record's first byte is already safe from the firmware's routing heuristic

- Some firmware forks classify a raw-custom packet as media by its first byte alone, routing `0x56`, `0x57`, `0x72` and `0x53` to a second frequency the node is not listening on.
- LDMUX's record begins with `KIND_TRANSPORT = 0x13` (`crates/session/src/record.rs:32`), so no LDMUX record can ever match that heuristic.
- Fix, cheap: a test that asserts the first byte is not in that set, so a future record kind cannot introduce the collision silently.

### A packed `path_len` byte must be unpacked, never stored raw

- `path_len` packs hop count in bits 0 to 5 and hash size minus one in bits 6 and 7, so `0x45` is five hops of two-byte hashes, not 69 hops.
- The corpus records a shipped client that stored it raw and displayed "69 hops".
- LDMUX stores a `RadioPath` with an `encoded` field (`AndroidRadioLink.kt:311`), which is the right shape. The risk is at the display and trust layers, where a raw byte read as a count is wrong by a factor of the hash width.

### Multi-interface companion firmware breaks the single-central assumption

- v1.18.0 lets one node serve BLE and WiFi at the same time. USB is excluded from BLE plus WiFi builds.
- The architecture of record assumes the radio's BLE link is single-central (`meshmore-sns-architecture.md:27`). That remains true for BLE, and it is no longer the only door.
- Consequence for LDMUX: the phone's BLE-only assumption is still correct for the phone, but a host could reach the same node over TCP, which is a second path into the same radio and not a second radio.

### The ceiling table, which the repository has never stated in one place

| Ceiling | Bytes | Derivation |
|---|---|---|
| OTA packet payload | 184 | `MAX_PACKET_PAYLOAD` |
| Group datagram application data | 165 | 184 minus 16 cipher padding minus 3 inner header |
| Companion frame | 176 | `MAX_FRAME_SIZE` |
| Text message payload | 160 | protocol |
| Binary request app data | 143 | 176 minus 1 opcode minus 32 public key |
| App blob in one group datagram | 163 | 172 minus 9 response header |
| LDMUX record | 172 | `RECORD_MAX` |
| LDMUX application bytes per record | 148 | 172 minus 6 clear, 2 inner, 16 tag |

- The corpus's point is that 165 and 163 are both correct for their layer and differ by two bytes, which is enough to push a payload into a second packet.
- LDMUX's 148 is derived the same way and should be stated in the docs as a derivation, not just a constant.

## What the corpus says about the MeshMore side

### The gateway is the only consumer of the binary carriage layer

- `MeshCarryCodec` is used in three places, all gateway or host code (`mesh_sender_adapter.dart:185`, `serial_mesh_sender.dart:261`, `hardware_smoke.dart:137`).
- The main app's message path never touches it.
- This weakens the case for deleting MeshMore's gateway outright, in a way that does not apply to LDMUX: the gateway is not dead code, it is MeshMore's only route to raw sends.
- For the separation that changes nothing, because LDMUX stops being a client. It does mean MeshMore should not delete the gateway as cleanup.

### Verified gaps in MeshMore, already itemised there

- `0x3E CMD_SEND_CHANNEL_DATA` has a constant and no encoder, which blocks every channel media format in the field.
- `0x42 CMD_RUN_CLI_COMMAND` is not even declared.
- The `0x1B` channel datagram decodes and is then dropped by the app.
- The "via" byte is parsed and discarded on `0x84` and `0x1B`.
- These are feature gaps rather than defects, and they belong to MeshMore's roadmap. They are listed here only so the fix register is not read as complete.

## What to adopt in the Rust companion client

- The full airtime formula, with the `de` rule and the 32/16 preamble split, implemented once and tested against the 613 ms known value.
- The firmware's timeout formula, so the session's retry is not shorter than the carrier's own.
- A `path_len` codec that packs and unpacks hop count and hash width, with the `0xFF` no-path sentinel.
- A firmware version gate, because the carrier choice differs across the 1.18.0 boundary.
- The first-byte guard test.
- The reasoning behind the corpus's warning list: do not add a software CRC on LoRa, do not build security on channel authenticity, and do not assume a bridge means mesh to internet.

## Where the corpus is weak for this purpose

- It is written for MeshMore's product decisions, not for LDMUX's transport. It has no LDMUX code review and no opinion on the separation.
- Several of its own items are marked open, including the iOS BLE loss investigation and the 1,501-line companion frame reference, so the frame layouts it quotes should still be checked against the firmware before they are relied on.
- Nothing in it was measured on the LDMUX pair. Every number that matters to this repository was recomputed or re-read from local source.
