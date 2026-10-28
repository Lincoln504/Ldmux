# Mesh transport (LoRa) — is a terminal over MeshCore plausible?

Target: **MeshCore** (Meshtastic appears below only as a comparison).
Status: **the L3 session exists on the daemon side (`crates/session`, ADR-0021); the phone side and the terminal encoding do not.** The L2 link beneath it now
is: see `MESH-GATEWAY.md` section 12 and `STATE.md`. This records what is known, what it costs,
and what is still unanswered. It is not a plan of record and nothing reads it at
runtime.

> **Reviewed 2026-10-06.** See "Review (2026-10-06)" at the end: several claims below
> were corrected (security model, the `caps` profile field, timers, regulatory
> wording), and the link-security design is in
> [ADR-0020](adr/0020-mesh-link-security.md).

The question asked was narrow: could ldmux, at a low enough frame rate, be usable over
MeshCore? The short answer is that a **grid** never can be, a **line** can, and the
gap between those two is a different transport rather than a tuning of this one.

## The numbers

MeshCore, all from the official and independent specs:

| Quantity | Value | Where |
|---|---|---|
| Max application payload per packet | **184 B** (`MAX_PACKET_PAYLOAD`) | `docs.meshcore.io/packet_format/`, `meshcore-spec` 01-wire-format |
| Max wire size | 255 B (`MAX_TRANS_UNIT`) | same |
| Wire layout | `header(1) + transport_codes(4,opt) + path_len(1) + path(0–64) + payload(1–184)` | same |
| Smallest possible packet | **3 B** (header + path_len + 1 B payload) | same |
| Text message overhead | **5 B** before any text (4 B timestamp + 1 B type/attempt) | `docs.meshcore.io/payloads/` |
| Companion text command cap | **160 B** (`CMD_SEND_TXT_MSG`) | MeshCore wiki, Companion-Radio-Protocol |
| Companion link | BLE Nordic UART, MTU 23 default / 512 requestable | deepwiki MeshCore 4.1.2 |
| Presets (proposal) | 0.3 kbps (SF11/62.5k) … 21.9 kbps (SF7/500k) | meshcore-dev/MeshCore issue #549 |

Meshtastic, for comparison:

| Quantity | Value | Where |
|---|---|---|
| Max `Data.payload` | **233 B** | `meshtastic/protobufs` mesh.options |
| Practical text limit | 200 B (client-side) | meshtastic.org Apple client docs |
| LongFast airtime | ~200 ms per message | freq51 message calculator |
| Measured latency | <500 ms 1 hop; 500 ms–2 s at 2–3 hops; 2–5 s at 4+ | signal-test-bot (measured *by that bot*) |

Regulatory, and this is the number that decides the question:

- **EU868: 1% duty cycle** — 36 s of transmit per hour. At 354 ms airtime that is
  **~101 packets per hour**, about one every 36 s.
- **US915: no duty cycle** in the LoRaWAN sense. The "400 ms dwell limit plus frequency
  hopping" sometimes quoted with it is a LoRaWAN US915 rule, and nothing read for this
  document says a MeshCore node hops or is held to it; treat it as unverified for
  MeshCore (see Review). This is the region ldmux's own mesh plan targets.

## The arithmetic

**This section was wrong when it was written, and the measurements are what
corrected it.** It assumed a frame carried a cell grid — 40-byte header plus 9-25
bytes per cell, so ~17.3 KB per 80x24 screen, ~94 MeshCore packets, about 0.03 fps.
That is the arithmetic for an *in-memory render structure*, not for traffic.

What actually goes on the wire is the pty byte stream. `crates/protocol/src/frames.rs` defines more frame types, but only
`OUTPUT` is emitted on the remote socket, and measurement agrees — see `WIRE-COST.md`:

| Workload | Frames | Bytes |
|---|---|---|
| one keystroke echo | 1 | **9** |
| `ls -la` | 1 | 1,254 |
| `seq 1 100` | 1 | 477 |
| `ls -laR` | 32 | 514,758 |
| attach (screen replay, escaped JSON) | text | 1,980 |

So the mesh arithmetic changes completely:

- A **keystroke echo is 9 bytes** — it fits MeshCore's 184-byte payload nine times
  over. Interactive traffic is not the problem at all.
- A `seq 1 100` frame is 477 bytes and does **not** fit; it would be split across
  three packets. So a mesh transport needs the daemon's coalescing capped at the
  MTU — `OUTPUT_FLUSH_BYTES` (`crates/daemon/src/remote.rs`) is 4 KiB today, which is 22 MeshCore packets' worth.
- `ls -laR`'s 514,758 bytes are the real cost, and they are the *volume* of output
  rather than the size of a frame. The lever is the one in `WIRE-COST.md`: when a
  client is behind, send it a snapshot of now instead of the backlog. On a mesh that
  is not an optimisation, it is the difference between usable and not.

## Why this is a different transport, not a lower frame rate

`FramePacing` (the mobile client, `packages/mobile`) paces **rendering** against the measured round trip. It does
not change what is on the wire: a frame is sent when the pane changes, and a frame is
the whole grid. Lowering the cadence, which was done this week and is worth doing,
buys main-thread headroom and battery. It does not make the payload smaller, and mesh
is a payload problem.

A mesh path would need:

1. **A line-delta protocol.** The daemon sends a line, the client paints a line. The
   daemon already has the plain-text path this needs (the `capture` RPC in `crates/daemon/src/daemon.rs`), which matters,
   because `snapshot.screen` is **not** plain text — it begins with an escape stream
   and would be pointless to send over a 184-byte link.
2. **Coalescing on the client side**, so a burst of lines becomes one "here are the last
   N lines" message rather than N messages.
3. **No claims on every keystroke.** The ownership and presence machinery would need to
   be amortised, since a claim per packet is unaffordable.

## What is not known

Nothing in these sources answers any of the following, and they are what a decision
would turn on:

- **MeshCore's measured latency.** Its direct routing has retransmit delay 0 and top
  priority, and only repeaters/room servers relay, which should be *better* than
  Meshtastic's managed flooding — but no measurement was found. This is an
  expectation, not a result.
- **Whether real hardware sustains any particular packet rate** in the US without the
  mesh collapsing into collisions. An earlier draft said "~2 packets/s"; that figure
  was not derived from airtime and is withdrawn (see Review). Absence of a duty cycle is
  not the same as capacity.
- **MeshCore's region/preset in this deployment.** the mesh project's own plan (`docs/01-PLAN.md` there, not in this repo; unverified here) targets US 902–928,
  which is the region that makes this viable. In EU868 the same idea is 1 packet per
  36 s and this document's verdict changes to "no".
- Whether the MeshCore companion API carries arbitrary binary for a custom application,
  or only the message types it defines.


---

## Two profiles, because a mesh is not a slow network

The question this answers: should ldmux have a separate mode for LoRa, or should the
fastest possible tailnet mode simply be made as light as it can be? Both, and they are
different things.

### What a LoRa link actually costs

A summary of this kind is worth correcting before it shapes a design, because three of
its claims are LoRaWAN-isms that would rule out work that is legal where this is going.

- **"1% duty cycle, so one packet every 50 seconds."** That is the **EU868** rule.
  **US915 has no duty cycle**; the dwell/hopping wording is addressed in Review. US915
  is what this document already targets, at MeshCore's **184-byte payload ceiling**.
  Adopting the EU figure would abandon a design that is legal here.
- **"Tokenise commands with a shared dictionary, so `systemctl restart nginx` becomes
  three bytes."** Rejected, and this is the important one. A measured keystroke echo on
  this protocol is **9 bytes** ("The arithmetic" above). The command text is not the cost and never was. A
  static dictionary cannot cover an arbitrary shell, and it would be a large amount of
  machinery aimed at the one part of the traffic that is already small.
- **"Strip ANSI and return a status code instead of strings."** Half right and unusable
  as stated. You cannot change what a program prints, and a status code is not a
  terminal. The true version of that instinct is already the rule here: cap the backlog
  and send **now** instead of replaying history.

What is right in it: block-based rather than character-at-a-time, raw P2P with a small
ARQ rather than LoRaWAN, and that SSH or telnet over LoRa is not a thing.

### The two profiles

Not a build variant. One client binary, one daemon, and a profile that does not exist
on the wire today: `caps.capabilities` carries only `scrollback`, so no `caps` profile
field is claimed here. If a profile is ever announced, that is a new protocol field to
be designed, and over the mesh the profile is implied by the gateway (see Review), not
negotiated through `caps`.

| | `direct` (tailnet, LAN, cellular) | `mesh` |
|---|---|---|
| payload | raw pty stream, client emulates | plain lines the daemon renders |
| frame | 8-byte header + bytes | one line, capped to the payload MTU |
| keystrokes | per key | **one uplink per command** |
| backlog | full, coalesced at 4 KB | never replayed; state of now only |
| compression | permessage-deflate (14.8x measured on the old TS daemon; **not implemented in the Rust daemon**, see `WIRE-COST.md`) | not worth it at these sizes |
| client engine | libghostty-vt | **not used at all** |
| ownership, `new`, `close`, `list` | shared | shared (through the gateway) |
| link security | WireGuard (tailnet) + bearer token | **not shared**: end-to-end Noise session, [ADR-0020](adr/0020-mesh-link-security.md) |

The reason `mesh` is a mode and not a flag on the existing path is the last row of the
second column: in mesh mode the client's terminal engine is not merely slower, it is
**unused**, because there is no byte stream for it to emulate. The daemon has to be the
one that owns the screen.

### What does not change

Session model, registry, ownership and claims, the launcher, and `new`/`close`/`list`.
The profile changes the payload's shape and nothing about what the payload means.

**The security model does not carry over.** An earlier version of this section said it
did. It cannot: the WebSocket path's confidentiality and peer authentication come from
the tailnet (WireGuard, ADR-0016) and the bearer token only selects among tailnet peers;
a radio link has neither, and the mesh and its repeaters see every packet. MeshCore's
own AES-128-ECB layer with a 2-byte MAC is not a substitute. The mesh path has its own
end-to-end security design in ADR-0020.

### What this asks of the direct path

Nothing, and that is the point of separating them. `direct` keeps getting the treatments
that its own measurements justify — 4 KB coalescing, threshold-gated deflate, backlog to
snapshot on a stalled client, Nagle off. The mesh profile is not allowed to justify
making `direct` worse, and `direct` is not allowed to justify making mesh mode heavier.

---

## Carriage (2026-10-24)

The carriage questions this document left open are answered in
[`MESH-GATEWAY.md`](MESH-GATEWAY.md): the companion binary request gives 143 B
of application data (142 B of record) and raw custom gives 160 B, the binary
path zero-pads and does not strip, and ARQ belongs to the end-to-end session
because the record layer never retransmits. The terminal model below still
stands; only the carriage changed.

## Review (2026-10-06)

Conclusions of a design review. Nothing in *this* document is implemented: the L2 link
and its daemon surface are, but the L3 session that would carry keystrokes is not.

1. **Line mode is not a general terminal.** The `mesh` profile as written (plain lines,
   one uplink per command) ignores programs that use the alternate screen (editors,
   pagers, `top`) and readline behaviour including tab completion and history editing,
   which need the byte stream and an emulator. The profile suits command-and-output
   work only; whether it falls back to a byte stream for those programs is undecided.
2. **The mesh transport is a separate gateway process**, not a mode on the WebSocket
   path. It bridges the daemon's Unix control socket (AGENTS.md: the daemon is a separate
   process and clients talk to it over that socket) to MeshCore, speaks the ADR-0020 session on the
   radio side, and holds exactly a paired phone's authority. This is why the profile is
   implied by the gateway rather than negotiated in `caps`, and why the `direct` path
   needs no change.
3. **The security model is not shared** with the WebSocket path (corrected above): see
   ADR-0020.
4. **US915 wording.** "400 ms dwell plus hopping" is a LoRaWAN US915 rule, not
   something established for MeshCore, and is withdrawn as a constraint. The "~2
   packets/s" figure was not derived. A real figure needs airtime per preset: as an
   illustration, computed here with the Semtech time-on-air formula (coding rate 4/5,
   8-symbol preamble, explicit header, CRC; **not measured, formula recalled from
   memory**), a 189-byte-on-air packet takes about 0.08 s at SF7/500 kHz, 0.3 s at
   SF7/125 kHz, 0.9 s at SF10/250 kHz and 7.5 s at SF11/62.5 kHz; the rate follows from
   the preset and is measured, not assumed.
5. **Packet and companion limits.** 184 B is the *packet* payload ceiling; the companion
   text command is capped at **160 B**, and a text message carries 5 B of
   timestamp/type overhead (whether that 5 B counts against the 160 is unmeasured).
   Raw/custom packets (type `0x0F`) have no MeshCore cipher; whether the companion API
   can send them is unmeasured.
   **Corrected 2026-10-24 (read, not assumed):** the companion API *can* send raw
   custom, two ways. `CMD_SEND_RAW_DATA` (25) is `[25][path_len][path][payload]`,
   direct-route only, and the firmware rejects a negative path length with "flood, not
   supported (yet)". `CMD_SEND_RAW_PACKET` (65) takes a whole serialised packet. Both
   bypass the cipher and the padding, so both are exact-length, capped by the 176 B
   companion frame rather than by the 184 B packet. Separately, the **binary request**
   path (`CMD_SEND_BINARY_REQ`, 50) gives 143 B of application payload and zero-pads
   to 16 B without stripping on receipt. Numbers and sources: `MESH-GATEWAY.md` §2–3. Per-packet budget after the ADR-0020 record layer (24 B
   of overhead): **160** application bytes over raw/custom, 136 over text with binary,
   96 over text with Base64.
6. **ARQ, ordering and de-duplication are unspecified.** MeshCore flood routing
   duplicates and reorders; acknowledgement, fragmentation and retransmission need a
   design of their own, above the authenticated record (seam named in ADR-0020 section 5).
   **Answered 2026-10-24:** ADR-0020 already settles it. The record layer never
   retransmits, a retransmission is a new record with a new counter, and duplicates are
   removed by the receiver's sliding window. Gateway-side ARQ is therefore impossible for
   a blind relay: resending identical ciphertext reuses the counter and the window drops
   it as a replay. `MESH-GATEWAY.md` §4.
7. **Timers are over budget.** The direct-path values are tuned for a link whose round
   trip is tens of milliseconds. Each needs a per-profile value:

| Timer | Where | `direct` (today) | `mesh` (proposed; judgement call, to be measured) |
|---|---|---|---|
| client ping | mobile client | 4 s | none or at least 60 s; every ping is a packet of airtime, and a round trip can exceed 8 s |
| idle sweep | `WS_IDLE_MS`, `crates/daemon/src/remote.rs` | 20 s | at least 5 minutes, and a multiple of the ping interval |
| claim grace (`CLAIM_GRACE_MS`, also `RELEASE_GRACE_MS`) | `crates/daemon/src/daemon.rs`, `remote.rs` | 3 s | about 60 s: a reconnect needs a handshake (ADR-0020) and a slow round trip before the claim is backed again; the ADR-0010 amendment's logic stays |

8. **Unmeasured.** MeshCore latency and throughput on the target hardware and preset; the
   regional preset in use; raw/custom support and repeater forwarding; binary in
   companion text; the 160 B cap versus the 5 B overhead; addressing for raw packets;
   real airtime; the ML-KEM stage's cost on the chosen preset.
9. **Security design:** [ADR-0020](adr/0020-mesh-link-security.md) (Noise IKpsk2 end-to-end
   inside MeshCore, which is treated as an untrusted transport). The 0.3 kbps to 21.9 kbps
   preset range in the table above is from a proposal issue, not a shipped setting.
