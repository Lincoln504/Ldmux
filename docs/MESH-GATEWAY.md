# Mesh gateway: the carriage contract for LDMUX over MeshCore

**Status: both sides are built and tested, and they have run on devices together.** The daemon
holds the host link, answers the L3 session and bridges the terminal profile; the phone carries
the same session through the Rust ABI and draws the tailnet's own terminal surface. The first
over-the-air session was 2026-10-29 and the standalone phone-radio path followed on 2026-11-02
(`docs/MESH-STANDALONE.md`). Written 2026-10-24 on branch `feat/meshmore-gateway`; status
corrected 2026-11-02. This supersedes the carriage assumptions in
[`MESH-TRANSPORT.md`](MESH-TRANSPORT.md) where the two differ, and it is bound by
[ADR-0020](adr/0020-mesh-link-security.md) wherever it touches security. The
phone-side gateway is superseded by the standalone BLE path; the host gateway still
uses this carriage contract.

`MESH-TRANSPORT.md` asked whether a terminal over MeshCore is plausible and left
the carriage unmeasured. This document answers the carriage, because the
companion-radio API has now been read rather than assumed.

## 1. Who owns what

| Piece | Owner | Language |
|---|---|---|---|
| The phone's radio, the companion-protocol BLE link | LDMUX app (standalone) | Kotlin |
| The host radio, the USB link, the host gateway | MeshMore `meshcore_host` | Dart |
| The end-to-end Noise session, the record layer | LDMUX app ↔ ldmux host | Kotlin (app) and Rust (host) |
| The terminal, PTYs, the keeper, libghostty-vt | ldmux as it already is | Rust |

The 2026-11-01 review of whether LDMUX should own the phone radio is in
[`MESH-RADIO-OWNERSHIP.md`](MESH-RADIO-OWNERSHIP.md); the standalone path now
does. The host still uses MeshMore's gateway, and this document's carriage
contract still describes that host leg.

The gateway is a **blind carrier**. It moves opaque records between the LDMUX
app and the radio. It does not parse them, does not decrypt them, does not
rewrap them, and does not retransmit them. The one thing it adds is an
explicit record length, because the radio's own framing cannot express one
(§3).

The phone side no longer uses this split: LDMUX owns the BLE link and the
MeshCore session directly. The host side still does, because the USB radio is
owned by `meshcore_host`.

## 2. The carriage: two phases, because one of them is much cheaper

**Corrected 2026-10-24 after reading the firmware.** Raw custom is not a
host-only option. The companion radio wired for `PAYLOAD_TYPE_RAW_CUSTOM`
delivers it to the app as `PUSH_CODE_RAW_DATA` (`MyMesh::onRawDataRecv`,
`src/Mesh.cpp:295`), and `createRawData` copies bytes verbatim with no cipher
and no padding (`src/Mesh.cpp:589-603`). So **stock companion firmware at both
ends** can carry it. No firmware fork on either side.

Raw custom is also exact-length, which removes the delimiter problem, and it
carries more. `CMD_SEND_RAW_DATA` is `[25][path_len][path][payload]` inside a
176-byte companion frame, so the datagram is `174 − path_len`; a three-hop path
still allows 171.

The catch is routing: `RAW_CUSTOM` is direct-only in both directions
(`src/Mesh.cpp:292-297` delivers it only when `isRouteDirect()`, and the flood
forward is commented out). A path must exist first, and a path comes from a
flooded exchange.

| | **Phase A — discovery** | **Phase B — established** |
|---|---|---|
| When | no path, or the path went stale | a path is known |
| Uplink | `CMD_SEND_BINARY_REQ`, flooded | `CMD_SEND_RAW_DATA`, direct |
| Downlink | the firmware's flooded `PATH + RESPONSE` return | `PAYLOAD_TYPE_RAW_CUSTOM`, direct |
| Cipher / padding | AES-ECB, zero-padded to 16 B | **none, exact length** |
| Datagram | 143 B | 174 − path_len |
| Record | 142 B (1 B length + record) | **172 B** |
| Application after the 24 B record header | 118 B | **148 B** |

Phase A exists to teach both ends a path, and one exchange does it: a flooded
request makes the responder build a `PATH + RESPONSE` return and flood it back,
which delivers the answer and the reverse path together
(`src/helpers/BaseChatMesh.cpp:286-306`). It is small by nature — the ADR-0020
handshake is 121 B then 50 B — so 118 B of application is enough.

Phase B is where the throughput lives: **148 B against 118**, with no delimiter
at all.

**A wider Phase A exists and is not used.** `CMD_SEND_RAW_PACKET` (65) injects a
whole serialised packet, bypassing the cipher and the padding, but it cannot
flood, so it does not solve the one problem Phase A exists for.

## 3. Delimitation, and why it is only a Phase A problem

The binary request path encrypts with AES-ECB and zero-pads the final block to
16 bytes. The receiving side does **not** strip that padding: the decryptor
returns the full block multiple and its own header comment warns that "dest may
contain trailing zero bytes in final block".

So a Phase A record cannot be delimited by the datagram length, and stripping
trailing zeros is not safe: one record in 256 ends in a byte the strip would
eat. Phase A therefore carries **one byte of record length**, costing one byte
of the 143 and removing the whole class of problem.

Phase B needs nothing. Raw custom is exact-length, so the datagram length *is*
the record length.

A retransmission changing a record's length is not possible: the record is
produced once by the session, and the carrier does not retransmit (§4).

## 4. ARQ: the session owns it, the gateway must not

ADR-0020 section 3 is unambiguous: "a retransmission is a *new* record with a
new counter; duplicates are removed by the application sequence number, not by
resending ciphertext." Section 5 repeats that "the record layer never
retransmits".

This rules out the obvious design. A blind relay that resends identical
ciphertext reuses the counter, and the receiver's 64-record sliding window
rejects the replay as a duplicate — the retransmission is a silent no-op that
burns airtime. Gateway-side ARQ is only possible if the gateway decrypts and
rewraps, which would destroy the blind-carrier property and put the session
keys on the phone.

Consequences to design around:

- **MeshCore is treated as a lossy, duplicating, reordering transport.** That is
  what ADR-0020 already assumes.
- **Recovery is a new record.** If a record is lost, the session re-sends its
  content under a new counter. That is the session's decision, and it is
  exactly what the replay window expects.
- **The gateway reports send outcomes, never delivery.** It can say the radio
  accepted the packet and whether it routed it as a flood or over a known path,
  and it can report the firmware's timeout estimate. It cannot say the peer
  received it, and it must not claim to. A `delivered` field would be a lie.
- **Duplicate suppression happens once**, in the receiver's counter window.

## 5. Discovery and path refresh

Phase A exists for one reason and should be used for nothing else.

A flooded request makes the responder build a path return and flood it back, so
the reply delivers the answer and teaches the requester the return path at the
same time. The first exchange is flooded **both** ways, not "flood out, direct
back". After it, both ends know a path and Phase B applies.

Two sizing consequences:

- A flooded reply carries `path_len + path + extra_type` inside the encrypted
  body, so its budget shrinks with hop count. Phase A's 118 B is a ceiling, not
  a promise.
- Phase A must be used for the handshake, because Phase B cannot flood. That is
  the whole reason the binary request path survives in the design.

**Paths go stale and fail silently.** The radio learns better paths as traffic
flows and signals it with `PUSH_CODE_PATH_UPDATED` (0x81), which carries only
the peer's public key (`MyMesh.cpp:404-410`), so the app must re-read the contact
to get the new `out_path`. The firmware's own comment warns the app may not be
connected when that push happens, so a re-read on reconnect is also required.

A wrong path does not error: the radio sends into the void. So the receiver of
silence cannot distinguish a lost record from a stale path. The gateway treats
"no response within the session's timeout" as a path failure, issues
`CMD_RESET_PATH` (0x0D) so the next send floods, and returns to Phase A.

That reset is cheap and it is the only remedy the companion API offers, which is
why the timeout is the trigger rather than any radio-level signal.

## 6. Airtime: this is a terminal on a shared band

The mesh profile is designed **as if EU868's 1 % duty cycle applied, even on
US915**. A terminal stream is the one traffic class that could become a
nuisance if unmanaged, so the budget is enforced rather than trusted.

**A budget is per-transmitter and local.** Each end meters its own
transmissions against its own duty cycle. A carrier cannot and must not throttle
the peer's airtime: the phone's radio and the host's radio are separate
transmitters with separate obligations. So ldmux's mesh gateway enforces the
policy on the host's own downlink, and MeshMore enforces it on the phone's
uplink. Neither has authority over the other.

- Default **36 s of transmit airtime per hour**, the EU868 1 % equivalent.
- The meter accumulates the firmware's timeout **estimate**. It is not
  measurement, and it cannot see repeater retransmissions or collisions. Call it
  an estimate everywhere it is surfaced.
- When the budget is spent the sender stops sending and reports it. The receiver
  must degrade gracefully: show the throttle and never assume a send succeeded.

### 6.1 The budget in packets, which is the number that matters

36 s per hour sounds generous until it is converted. Airtime per packet is
the Semtech formula for a 176 B packet (CR 4/5, firmware preamble of 32 symbols at
SF8 and below, 16 above, CRC on), recomputed 2026-10-28 and **not measured on air**.
These are generic presets used for scale. The presets people actually deploy are in
the next table:

| Preset | s/packet | Records per hour | KB per hour | A 2 KB snapshot needs |
|---|---|---|---|---|
| SF7/500 kHz | 0.08 | 450 | 66.6 | 1 s |
| SF7/125 kHz | 0.30 | 120 | 17.8 | 4 s |
| SF10/250 kHz | 0.90 | 40 | 5.9 | 13 s |
| **SF11/62.5 kHz** | **7.50** | **4.8** | **0.7** | **110 s** |

148 B is the Phase B application payload. Phase A is smaller and carries only
the handshake.

**Correction 2026-10-28.** No preset in the live MeshCore feed
(`api.meshcore.nz/api/v1/config`, 26 entries) is SF11/62.5 kHz. That row is a
proposal from MeshCore issue #549, not a deployed preset, so "the longest-range
preset" and "cannot complete one" below it were wrong for any real radio. The real
figures for a 176 B packet:

| Deployed preset | ms/packet | Records/hour at 36 s | A 6-record operation |
|---|---|---|---|
| USA/Canada 910.525 / 62.5 kHz / SF7 / CR5 | 613 | 58.7 | about 10 per hour |
| EU/UK Narrow 869.618 / 62.5 kHz / SF8 / CR8 | 1656 | 21.7 | about 3.6 per hour |

Both are computed with the formula above (`radio-config` in MeshMore carries the
same 613 ms). The 36 s budget is the EU868 equivalent. On US915 no duty cycle
applies, so it is a courtesy bound, not a legal one.

**Correction 2026-11-04.** LDMUX does not run either 62.5 kHz row. The deployed
pair was configured to **914.25 MHz / 500 kHz / SF8 / CR5 / 22 dBm** and read back
on the device (`MESH-STANDALONE.md:415`, `HARDWARE-VALIDATION.md:783`). A 176 B
packet on that tuple is **138 ms**, which at 36 s per hour is **261 records per
hour**:

| Deployed LDMUX preset | ms/packet | Records/hour at 36 s | A 6-record operation |
|---|---|---|---|
| 914.25 / 500 kHz / SF8 / CR5 | 138 | 261 | about 43 per hour |

So the 62.5 kHz rows describe the public MeshCore presets, which this link cannot
reach because the bandwidth differs, and the budget conclusion below is right in
direction and about four times high in magnitude: 261 records an hour, not 58.7.
A command with a 5-record reply affords about 43 of them an hour, not 10.

The **deployed** row decides the profile. On the narrowest public preset an hour of
duty-cycle budget buys under five packets and a terminal snapshot would cost three hours of
it, which is where this section's original conclusion came from. **Corrected 2026-11-04:**
that is not this link. At 914.25 / 500 kHz / SF8 the hour buys 261 records, so the binding
constraint is the **1,500 byte packet cap** and the 138 ms airtime, not the meter: a budget
override would still buy nothing, but for a different reason, and the reason is what a
reader needs.

### 6.2 So the output is bounded at the source

This is not a compromise, it is what the measurements already said. `ls -laR`
is 514,758 bytes and the user sees 24 lines; substituting state for history
turns that into one ~2,000 B snapshot ([`WIRE-COST.md`](WIRE-COST.md) §1). The
mesh profile goes further: **state of now only, never replayed history** (the
`mesh` column in [`MESH-TRANSPORT.md`](MESH-TRANSPORT.md)).

The bound is applied where the output is produced, on the host:

- cap a mesh reply at **1,500 B**, and
- further cap it to what the remaining budget affords:
  `floor(budgetRemaining / airtimePerPacket) * 148` bytes.

On SF7/125 kHz that is the 1,500 B cap, about 10 records and 3 s of airtime.
On SF11/62.5 kHz it is roughly 550 B, four records and 30 s, which still holds a
screenful of last lines.

The measured wire costs in [`WIRE-COST.md`](WIRE-COST.md) are why the cap is on
volume and not on frames: one keystroke echo is 9 bytes and `ls -la` is 1,254,
so a frame is not expensive. The bytes are in sending history the user cannot
keep, and the answer is not to send it.

**No raise path in v1.** If a genuine need for a larger transfer appears later,
an owner-only raise is additive. Nothing in the profile requires it now.

## 6.3 What this link is, stated honestly

The arithmetic decides the UX, and the honest description is **not** "a
terminal".

| Preset | Records/hour | A command with a 5-record reply | So |
|---|---|---|---|
| SF7/500 kHz | 450 | 6 records | about 75/hour |
| SF7/125 kHz | 120 | 6 records | about 20/hour |
| SF10/250 kHz | 40 | 6 records | about 6/hour |
| SF11/62.5 kHz (a proposal, not deployed) | 4.8 | 6 records | under 1 per hour |
| USA/Canada SF7/62.5 kHz | 58.7 | 6 records | about 10/hour |
| EU/UK Narrow SF8/62.5 kHz CR8 | 21.7 | 6 records | about 3.6/hour |
| **Deployed: 914.25 / 500 kHz / SF8 / CR5** | **138** | 6 records | **about 43/hour** |

**Corrected 2026-11-04.** The two 62.5 kHz rows were labelled "(deployed)" and are not:
they are public MeshCore presets, and this link cannot reach them because the bandwidth
differs. The deployed row is the third.

On the 62.5 kHz preset a public North American deployment uses, the budget affords
about ten command-and-reply operations an hour. The mesh profile is a **low-rate
command
channel**: send a command, receive the last lines of its output, minutes later.

**Corrected 2026-11-04 for the deployed link.** The pair runs 914.25 / 500 kHz /
SF8 / CR5, where a record is 138 ms and the 36 s budget buys 261 records an hour.
A command with a 5-record reply therefore affords about 43 an hour, and the
"minutes later" above is a property of the narrow public preset, not of this
link.

This agrees with what this repository already concluded. The `mesh` column in
[`MESH-TRANSPORT.md`](MESH-TRANSPORT.md) is "plain lines", "one uplink per
command", "backlog never replayed", and the client's terminal engine is
**unused**. The carriage document does not change that; it makes the numbers
behind it explicit.

What it means for LDMUX's UI:

**Corrected 2026-11-04.** These rules were derived from the narrow public preset. On the
deployed link the implementation does echo a keystroke (one record each way, about 0.3 s of
air plus the bridge tick, `MESH-LATENCY.md` §5), and the budget is **about 43**
command-and-reply operations an hour, not ten. The first, second and third hold for a public
preset only. The last holds either way.

- No live cursor, no per-keystroke echo, no spinner that implies a second (public narrow
  preset only).
- Present the budget as **operations left this hour**, not seconds.
- Timestamp every reply, because "minutes later" is normal (public narrow preset only).
- Never imply delivery. The carrier cannot know, so the UI must not suggest it.

## 7. Timers

The direct-path timer values are tuned for a link whose round trip is tens of
milliseconds. On the mesh they are all wrong.

| Timer | `direct` today | `mesh` |
|---|---|---|
| client ping | 4 s | none, or at least 60 s |
| idle sweep (`WS_IDLE_MS`) | 20 s | at least 5 min, a multiple of the ping |
| claim grace (`CLAIM_GRACE_MS`, `RELEASE_GRACE_MS`) | 3 s | about 60 s |

The gateway adds **no heartbeat of its own**. TCP keepalive plus a status frame
is enough, and L3 already has `PING`/`PONG`.

## 7.1 Contract details the client must honour

Three things the L2 contract carries that are easy to miss and expensive to get
wrong.

**Records carry a sequence number.** `RECORD` is `seq(1) + record` and
`SEND_RESULT` echoes the same `seq`. A gateway queue can hold several records
while one is in flight, so an unattributed refusal would leave the client unable
to tell which of its records failed.

**Capacity is advertised, not assumed.** `BIND_OK` reports the largest uplink
record the peer's path actually allows, because `cmd_send_raw_data` accepts
`174 − path_len`. A long path therefore shrinks the record, and a client sized to
the absolute ceiling would spend airtime on sends the radio refuses. Size to the
advertised number. The floor is 160 B, which still fits a 14-hop path.

**Only malformed framing closes the connection.** An unknown frame type is
delivered and ignored, so the frame set can grow without a flag day.

## 8. What LDMUX has to build

Small, and none of it is radio code.

1. A KMP `MeshMoreGatewayClient`: a socket client, the L2 frame codec, pairing,
   reconnect with backoff, and the credit-window backpressure the gateway
   imposes so a client cannot outrun a one-packet-at-a-time radio.
2. **Done 2026-10-28 (ADR-0025), host and phone; neither has run on a radio or a device.** A compact encoding for the L3
   control messages the mesh profile actually carries. It is `crates/protocol/src/mesh_term.rs`
   (plain-text row diffs) with the shell bridge in `crates/daemon/src/mesh_bridge.rs`; the phone's
   port (`MeshTerm.kt`), attach driver (`MeshTermDriver`) and screen (`MeshTerminalScreen`) were built the same day. **2026-10-29:** the profile can list shells (`List`/`Sessions`, ADR-0025) and the route is reachable from Settings: a mesh pairing that does not depend on the tailnet pairing, a scanner for the gateway code, and a picker that cannot save a route that will not run. **2026-10-30:** profile version 2 (ADR-0025): named keys (`Key`) encoded by the host for the pane's own mode, and the attached shell named in `Profile`. Original text: **The existing `crates/protocol` frames cannot cross this link**:
   they are JSON with a 16 MiB payload ceiling. The mesh profile needs its own
   binary encoding of the subset, defined end to end inside the Noise session
   and therefore opaque to the gateway.
3. Mesh-profile values for the three timers in §7.
4. **Built 2026-10-29 (no device run):** the mesh section of Settings and the reduced screen, which names the absent features, the send budget in records and the not-working states, and says the route needs MeshMore running on the same phone. Original text: Honest UI for a link that can be throttled, can time out, and cannot promise
   delivery.

The mockup header in `MESH-TRANSPORT.md` (line mode, chunking, a byte-stream
fallback for pagers) still stands. This document changes the carriage, not the
terminal model.

## 9. Decisions taken

Settled on the MeshMore side, 2026-10-24. Recorded here because they bind this
profile.

1. **iOS is tailnet only.** LDMUX on iOS gets no MeshCore support and no
   gateway. The two-app gateway is Android-only. A suspended iOS app cannot
   serve a loopback listener anyway, so this is agreement with the platform
   rather than a restriction.
2. **Contact provisioning is a user-visible step in MeshMore's UI**, not a
   gateway write. The gateway keeps performing no companion writes, and `BIND`
   fails cleanly when the host is not yet a contact.
3. **Airtime is per-transmitter and local** (§6). No `BUDGET` frame and no
   cross-end throttle, because a carrier has no authority over the peer's
   transmissions.
4. **No budget raise in v1**, because the output is bounded at the source
   (§6.2).
5. **Unknown L2 frame types are ignored**; only malformed framing and a bad
   version close the connection, so the frame set can grow without a flag day.
6. **Crypto capabilities stay inside the session.** The ML-KEM stage is
   negotiated end to end, never in the L2 handshake, because a blind gateway
   cannot know or attest the host's capabilities.

## 10. Provenance

Every number above was read, not assumed:

- `MAX_FRAME_SIZE 176`: `src/helpers/BaseSerialInterface.h:5`, and the write and
  receive guards in `src/helpers/ArduinoSerialInterface.cpp:25-28,60-66`.
- Binary request layout and the 143 B budget:
  `examples/companion_radio/MyMesh.cpp:1678-1684`.
- The `sendRequest` and `createDatagram` caps: `src/helpers/BaseChatMesh.cpp:633`
  and `src/Mesh.cpp:487-489`.
- AES-ECB with zero padding and no strip on receipt: `src/Utils.cpp:62-113`
  (`src/Utils.h:41` carries the warning).
- Raw custom copied verbatim: `src/Mesh.cpp:589-603`. Raw custom not forwarded
  by repeaters: `src/Mesh.cpp:292-297`.
- `CMD_SEND_RAW_DATA` rejecting flood: `MyMesh.cpp:1519-1534`.
- Flooded request answered by a flooded path return:
  `src/helpers/BaseChatMesh.cpp:286-306`.
- Firmware-chosen tags and the single outstanding request:
  `src/helpers/BaseChatMesh.cpp:638`, `MyMesh.cpp:1688-1689`.
- Timeout estimates: `src/helpers/BaseChatMesh.cpp:645-653`.

Firmware read at `companion-v1.17.1` (`d929643`). The companion protocol
document is the same one `crates/protocol` is written against for the direct
path; nothing here changes the direct path.

## Addendum, 2026-10-28: the radio does not queue downlink raw data

`onRawDataRecv` pushes `PUSH_CODE_RAW_DATA` (0x84) only `if (_serial->isConnected())`; otherwise it logs and drops it (`MyMesh.cpp:811-816` in `companion-v1.17.1`). Unlike channel data (`onChannelDataRecv` calls `addToOfflineQueue`), raw custom is **not** held for a reconnecting app. Consequences for this contract:

- A Phase B record that arrives while the phone's BLE link is down is lost, silently.
- The gateway cannot detect or replay it. The session layer's own retransmission (ADR-0020: a new record with a new counter) is the only recovery, which is what section 4 already assumes. Do not describe `RECV` as reliable downlink.
- `PUSH_CODE_RAW_DATA` has no sender identity (`[84][snr][rssi][0xFF][payload]`), so binding a peer remains the gateway's job.

Also: the gateway's airtime estimate is only correct for the radio preset it is configured for. The flat 4 ms/byte default fits 62.5 kHz SF7 (613 ms per 176 B). EU/UK Narrow is 1656 ms, 2.4x higher. The app must build the estimator from the radio's real parameters (`AirtimeEstimator.lora`).

## 11. Implementation status, 2026-10-28

> **Superseded 2026-11-02.** The phone no longer uses MeshMore's loopback gateway. This list
> records the earlier Kotlin implementation; the phone now owns BLE directly. Current record:
> [`MESH-STANDALONE.md`](MESH-STANDALONE.md).

**Built (LDMUX mobile, Kotlin, `packages/mobile/composeApp`, package `io.ldmux.mobile.mesh`):**

- `MeshGatewayFrame.kt`: L2 frame codec, typed builders, total readers, an incremental
  decoder with a sticky error and no resync. Pinned byte for byte to the Dart reference
  (`meshcore_gateway`) by `MeshGatewayFrameTest`, whose vectors were generated by running
  the Dart codec, not typed from this document.
- `MeshGatewayClient.kt`: HELLO, PAIR, BIND, sequenced `RECORD`, `SEND_RESULT` per seq,
  `RECV` flow, `STATUS`. One read loop, one write mutex, one idempotent teardown, every
  pending send failed on exit, never reconnects by itself, never retransmits.
- `MeshSocket.kt` and `JavaMeshSocket.kt`: the pipe, and its `java.net.Socket` form.
- `MeshRoute.kt`: `tailnet | mesh` resolution. A saved mesh route on iOS is
  `Unusable(NotOnThisPlatform)`, never a quiet switch to the tailnet. Mesh capabilities
  carry no live cursor, no keystroke echo, no streaming.
- `MeshTokenStore.kt` and `KeystoreMeshTokenStore.kt`: the pairing token under an Android
  Keystore AES-GCM key, in its own preferences file. Not exercised by JVM tests.

**Not built, on purpose:**

- **Superseded 2026-10-28 (the route is now wired as far as it can honestly go):** `LdmuxConfig.route`
  is persisted, `LdmuxConfig.endpoint` is null for a non-tailnet route so neither `LdmuxClient` nor
  `AttachClient` opens anything, and the launcher shows `LinkState.RouteUnavailable` with the
  reason. **Superseded later the same day:** `MESH_TERMINAL_BUILT` is now `true` and `App()` shows the reduced mesh terminal for a resolvable mesh route; a route that cannot run still opens nothing. The original line follows.
- The mesh route is **not wired into the launcher or the terminal surface**. Those drive the
  daemon WebSocket and the terminal engine. Nothing exists on the other end of a mesh route
  yet (the compact L3 encoding in section 8 item 2 and the daemon listener below), so a UI
  entry would be a switch with nothing behind it.
- **The daemon has no mesh listener.** `grep -rni mesh crates/*/src` finds nothing outside
  `vt-sys`. There is also **no `ldmux status` command**: the CLI usage line is
  `ping|list|desktop|new|...` (`crates/daemon/src/bin/ldmux.rs:159`). The `mesh.status` and
  `mesh.enable` RPCs and "surfaced in `ldmux status`" come from a MeshMore recommendation
  (`meshmore-sns-private-mesh-investigation.md:403`), not from code here.

Enabling the daemon side would plug in at:

| Piece | Where |
|---|---|
| New RPC methods (`mesh.status`, `mesh.enable`) | the `match method` in `crates/daemon/src/daemon.rs:1664`, next to `ping` at `:1665` |
| A status verb in the CLI | `crates/daemon/src/bin/ldmux.rs:159` (usage) and its dispatch |
| Compact binary encoding of the mesh subset | new module in `crates/protocol`. The existing frames are JSON with a 16 MiB ceiling (`crates/protocol/src/frames.rs:45,95-105`) |
| Mesh timers (ping, idle, claim grace) | `WS_IDLE_MS` `crates/daemon/src/remote.rs:208`, `CLAIM_GRACE_MS` `crates/daemon/src/daemon.rs:88`, client ping `crates/attach/src/lib.rs:171` |

No status field was added: a field reading "not implemented" would be noise, and a real one
needs the listener.

**Contract facts the client honours.** `SEND_RESULT` is what the radio did, not delivery.
`RECV` is lossy: raw data is dropped while no app is connected (`MyMesh.cpp:811-816`). A
raw send is answered by OK/ERR, so `SentFlood` can only come from Phase A. The reference
server advertises `statusPush` only and does not enforce the credit window of section 8
item 1, so the client has no credit logic. `STATUS` uses `rssi == -128` and
`battery == 0xFF` as "unknown" (`constants.dart`), mapped to null here.

## 12. Host side, 2026-10-28: the radio owner now exists, the daemon client does not

The gateway process beside `ldmuxd` is no longer a diagram. It lives in the
MeshMore repo, because it is the MeshMore side of the boundary:

`meshmore_sns_app/packages/meshcore_host/`

- `serial_framing.dart`: the firmware's USB-serial framing, `'<' len_lo len_hi`
  host to radio and `'>' len_lo len_hi` radio to host, 176-byte ceiling
  (`src/helpers/ArduinoSerialInterface.cpp:19-70`,
  `src/helpers/BaseSerialInterface.h:5`). Unlike BLE, serial is a byte stream, so
  the decoder is incremental and keeps state across reads.
- `serial_transport.dart`: a `MeshcoreTransport` over that framing.
- `serial_mesh_sender.dart`: the host twin of the phone's `MeshSender`. Same
  contract, same L1 codec. It adds one thing BLE did not need: an explicit
  one-at-a-time queue, because the firmware's raw-data reply is a bare
  `writeOKFrame` with no correlation tag, so two sends in flight cannot be told
  apart.
- `bin/meshmore_gateway.dart`: the process. Opens the port (`stty` configures
  the TTY, `dart:io` moves the bytes), handshakes, builds the governor from the
  radio's **actual** preset read out of `SELF_INFO`, then starts the same L2
  `Gateway` the phone runs.

### The firmware constraint this exposed

The env that is flashed on the phone-side radio, `heltec_v4_companion_radio_ble`,
does **not** define `ENABLE_USB_INTERFACE`
(`variants/heltec_v4/platformio.ini:202-225`). Only
`heltec_v4_companion_radio_usb` does (`:182-190`). One Heltec cannot be both a
BLE companion and a USB companion under these envs.

So the two-radio fleet maps cleanly:

| Radio | Firmware env | Attached by | Owner |
|---|---|---|---|
| phone-side | `heltec_v4_companion_radio_ble` | BLE | MeshMore app |
| host-side | `heltec_v4_companion_radio_usb` | USB serial | `meshcore_host` |

The host radio must be reflashed. Until it is, this process correctly refuses to
start with a message saying why, rather than hanging.

Verified on a real HELTEC-BASE on 2026-10-28: the USB image answers, reports
`v1.17.1-silent`, 4366 mV, 350/350 contacts and `pathHashMode 0`, and answers a
64-byte zero-hop raw frame with `OK`. The BLE image answers nothing at all on
the same port, which is the `ENABLE_USB_INTERFACE` difference above.

### What LDMUX still owes

- **Wire `crates/mesh` into the daemon.** The client now exists
  (`ldmux-mesh`: frame codec, decoder, loopback client, 19 tests, clippy clean,
  tokio its only dependency). What remains is the daemon side: a listener task
  that owns the connection, the compact L3 encoding, and an RPC/CLI surface.
- The mobile route is still not wired into the launcher (section 11). **Superseded 2026-10-28:** see the entry above; it is wired to the config, the transport choke point and the link state, and resolves to `TerminalNotBuilt`.

**Two pairing ceremonies are by design.** LDMUX issues its own tailnet grant, so
it works with no MeshMore installed; MeshMore issues the mesh grant because it
owns the radio. They are different secrets for different reach, and neither can
replace the other. The asymmetry to fix is on this side: the daemon token, which
grants shell control, sits in plain preferences while the mesh token is in the
Android Keystore.

**Device identity does not belong in L2.** The gateway is two independent hops,
each terminated by a blind carrier, so an identity placed in the L2 handshake
would be visible to MeshMore and would still not reach the daemon that keys
ownership. It goes in the end-to-end session handshake instead, which LDMUX owns.

**Record contents are ours, not the carrier's.** The gateway used to export
`applicationBytesPerRecord = 148` (172 − a 24-byte header and tag from ADR-0020).
That made a MeshMore package and a MeshMore test depend on this repo's session
layout. It is removed: the carrier promises a 172-byte record arrives intact, and
148 = 172 − 24 is arithmetic for the session layer here.

Verified end to end on 2026-10-28, real radio, USB serial:
`ldmuxd` (this repo) → `crates/mesh` → MeshMore host gateway → HELTEC-BASE.
`ldmux mesh status` printed `state ready`, `capacity 172 B per record`,
`budget 58 records left this hour`, `radio connected`. The 58 is the governor's
own arithmetic: 36 s of airtime per hour divided by a 176-byte frame at
612 ms plus the 2-byte on-air overhead.

Note what is *not* owed: the daemon must never speak MeshCore. It talks L2 to
this process, which owns the radio.

## 13. The L3 session, daemon side, 2026-10-28

The boundary did not move: the gateway still carries opaque records and the session is
LDMUX's. What changed is that the daemon now consumes `RECV`. With `LDMUX_MESH_PSK` set, a
record starting `0x11` is a handshake message 1 and `0x13` is a transport record
(`crates/daemon/src/mesh_session.rs`); anything else is dropped with a reason in
`mesh.status`'s `session.lastError`. Without it the old behaviour holds (dropped, visibly).

Facts a gateway implementer can rely on:

- Every record the daemon hands over, handshake reply and retransmission included, is
  charged against the last `opsLeftThisHour` it saw, and an unreported budget is zero.
  `crates/daemon/tests/mesh_session.rs` pins it (and was mutation-checked: replacing the
  budget with 255 fails `the_daemon_never_transmits_past_the_records_the_gateway_says_remain`).
- The daemon keeps at most `CREDIT_WINDOW` (8) records awaiting `SEND_RESULT`.
- A reconnect to the gateway means a new handshake. The session never outlives the L2 socket.
- The carrier still exports only `RECORD_MAX`. Payload is 144 bytes per record at 172, and
  that figure is in `ldmux_session::arq`, not here.

## Configuring the host without copying keys (2026-10-29)

The firmware's raw-data push carries no sender key: `PUSH_CODE_RAW_DATA` is
`[0x84][snr][rssi][0xFF][payload]` (`examples/companion_radio/MyMesh.cpp:800-811`).
So the host must be told which radio it talks to exactly once, and a 64-hex public
key is the wrong way to say it — the radio already knows its contacts by name.

Host side, one word and one file:

```
meshmore_gateway --port /dev/ttyACM1 --peer-name HELTEC-BASE --state <state.json>
ldmuxd            # with LDMUX_MESH_STATE=<state.json> and LDMUX_MESH_PSK=<64 hex>
```

The gateway resolves the name against the radio's own contact table (refusing with
the names it does hold when the name is unknown), and writes `gatewayAddr`,
`pairingToken`, `radioKey` (its own) and `peerKey` into the state file, 0600 and
merged so a token rotation keeps them. The daemon reads address, token and peer
from that file; `LDMUX_MESH_SELF` comes from the same file's `radioKey` when it is
not set. Explicit environment variables still win key by key, so a port can be
moved without rewriting the file.

Nothing about radio parameters crosses that boundary: the file holds addresses and
public keys, which is what section 13 permits.
