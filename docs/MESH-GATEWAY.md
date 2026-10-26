# Mesh gateway: the carriage contract for LDMUX over MeshCore

**Status: design only, not implemented.** Written 2026-10-24 on branch
`feat/meshmore-gateway`. This supersedes the carriage assumptions in
[`MESH-TRANSPORT.md`](MESH-TRANSPORT.md) where the two differ, and it is bound by
[ADR-0020](adr/0020-mesh-link-security.md) wherever it touches security. Nothing
here is a plan of record until the open decisions at the bottom are taken.

`MESH-TRANSPORT.md` asked whether a terminal over MeshCore is plausible and left
the carriage unmeasured. This document answers the carriage, because the
companion-radio API has now been read rather than assumed.

## 1. Who owns what

| Piece | Owner | Language |
|---|---|---|
| The radio, the companion-protocol BLE link, the local gateway | MeshMore (the phone app) | Dart |
| The end-to-end Noise session, the record layer | LDMUX app ↔ ldmux host | Kotlin (app) and Rust (host) |
| The terminal, PTYs, the keeper, libghostty-vt | ldmux as it already is | Rust |

The gateway is a **blind carrier**. It moves opaque records between the LDMUX
app and the radio. It does not parse them, does not decrypt them, does not
rewrap them, and does not retransmit them. The one thing it adds is an
explicit record length, because the radio's own framing cannot express one
(§3).

That split is what makes this cheap: the transport-specific knowledge stays in
MeshMore, where the BLE link already lives, and LDMUX gains a socket client
with no radio code at all.

## 2. The two carriages, with real numbers

Both directions ride different MeshCore mechanisms, and the phone's uplink is
much tighter than the host's downlink.

| | phone → host | host → phone |
|---|---|---|
| Mechanism | companion binary request (`CMD_SEND_BINARY_REQ`, `0x32`) | `PAYLOAD_TYPE_RAW_CUSTOM` packet |
| Cipher / padding | AES-ECB, zero-padded to 16 B | none — bytes copied verbatim |
| Datagram budget | 143 B | 184 B packet, 172 B observable at the phone |
| Record ceiling | **142 B** (1 B length + record) | **160 B** |
| Application bytes after the 24 B record header | **118 B** | **136 B** |

Three caps stack on the phone's uplink and the smallest binds: the 176 B
companion frame (`MAX_FRAME_SIZE`), the `sendRequest` guard (163 B), and the
`createDatagram` guard (163 B). A binary request spends 1 B on the opcode and
32 B on the peer public key, leaving 143 B, and one more byte goes to the
record length.

The host side is a full MeshCore node, not a companion, so it reaches the
184 B packet ceiling directly.

**A wider phone uplink exists and is not used yet.** `CMD_SEND_RAW_DATA` (25)
and `CMD_SEND_RAW_PACKET` (65) both bypass the cipher and the padding and reach
about 172 B. Neither can flood: `CMD_SEND_RAW_DATA` rejects a negative path
length with "flood, not supported (yet)", and repeaters do not forward
`PAYLOAD_TYPE_RAW_CUSTOM`. So a flooded first contact still needs the binary
request path. Using raw data for the steady state is a later optimisation, not
a v1 dependency.

## 3. Delimitation, and why the record carries its own length

The binary request path encrypts with AES-ECB and zero-pads the final block to
16 bytes. The receiving side does **not** strip that padding: the decryptor
returns the full block multiple and its own header comment warns that "dest may
contain trailing zero bytes in final block".

So a record cannot be delimited by the datagram length, and stripping trailing
zeros is not safe: one record in 256 ends in a byte the strip would eat. This
is a real corruption rate on every packet, not a theoretical one.

The gateway therefore carries **one byte of record length** as the first byte of
the datagram. It costs one byte of the 143 and it removes the entire class of
problem. The host's direction needs no prefix, because raw custom is copied
verbatim.

A retransmission making the record a different length is not possible: the
record is produced once by the session and only the carrier repeats it, and the
carrier does not repeat it at all (§4).

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

## 5. Discovery: the first exchange floods both ways

A flooded request makes the responder build a path return and flood it back, so
the reply simultaneously delivers the answer and teaches the requester the
return path. The first exchange is therefore flooded **both** ways, not "flood
out, direct back". After that, a path is known and the cheaper direct route
applies.

Two consequences for sizing:

- A flooded reply carries `path_len + path + extra_type` inside the encrypted
  body, so the reply budget **shrinks by the hop count**. Treating the downlink
  as a fixed 160 B is correct only for raw custom, and raw custom needs a known
  path.
- First contact must go through the binary request path, because raw custom
  cannot flood. That is the only reason the binary request path is still
  needed at all.

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
ADR-0020's estimate (Semtech formula, **not measured and not checked against the
Semtech document**), and 136 B is the downlink application payload from §2:

| Preset | s/packet | Records per hour | KB per hour | A 2 KB snapshot needs |
|---|---|---|---|---|
| SF7/500 kHz | 0.08 | 450 | 61.2 | 1 s |
| SF7/125 kHz | 0.30 | 120 | 16.3 | 4 s |
| SF10/250 kHz | 0.90 | 40 | 5.4 | 13 s |
| **SF11/62.5 kHz** | **7.50** | **4.8** | **0.7** | **110 s** |

The last row decides the profile. On MeshCore's longest-range preset, a whole
hour of duty-cycle budget buys **under five packets**, and one terminal snapshot
would cost three hours of it. There is therefore no point in a budget override:
the transfer has to be small by construction.

### 6.2 So the output is bounded at the source

This is not a compromise, it is what the measurements already said. `ls -laR`
is 514,758 bytes and the user sees 24 lines; substituting state for history
turns that into one ~2,000 B snapshot ([`WIRE-COST.md`](WIRE-COST.md) §1). The
mesh profile goes further: **state of now only, never replayed history** (the
`mesh` column in [`MESH-TRANSPORT.md`](MESH-TRANSPORT.md)).

The bound is applied where the output is produced, on the host:

- cap a mesh reply at **1,500 B**, and
- further cap it to what the remaining budget affords:
  `floor(budgetRemaining / airtimePerPacket) * 136` bytes.

On SF7/125 kHz that is the 1,500 B cap, about 11 records and 3.3 s of airtime.
On SF11/62.5 kHz it is roughly 550 B, four records and 30 s, which still holds a
screenful of last lines.

The measured wire costs in [`WIRE-COST.md`](WIRE-COST.md) are why the cap is on
volume and not on frames: one keystroke echo is 9 bytes and `ls -la` is 1,254,
so a frame is not expensive. The bytes are in sending history the user cannot
keep, and the answer is not to send it.

**No raise path in v1.** If a genuine need for a larger transfer appears later,
an owner-only raise is additive. Nothing in the profile requires it now.

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

## 8. What LDMUX has to build

Small, and none of it is radio code.

1. A KMP `MeshMoreGatewayClient`: a socket client, the L2 frame codec, pairing,
   reconnect with backoff, and the credit-window backpressure the gateway
   imposes so a client cannot outrun a one-packet-at-a-time radio.
2. A compact encoding for the L3 control messages the mesh profile actually
   carries. **The existing `crates/protocol` frames cannot cross this link**:
   they are JSON with a 16 MiB payload ceiling. The mesh profile needs its own
   binary encoding of the subset, defined end to end inside the Noise session
   and therefore opaque to the gateway.
3. Mesh-profile values for the three timers in §7.
4. Honest UI for a link that can be throttled, can time out, and cannot promise
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
