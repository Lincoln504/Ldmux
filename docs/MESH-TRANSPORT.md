# Mesh transport (LoRa) — is a terminal over MeshCore plausible?

Target: **MeshCore** (Meshtastic appears below only as a comparison).
Status: **investigated, not implemented.** This records what is known, what it costs,
and what is still unanswered. It is not a plan of record and nothing reads it at
runtime.

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
- **US915: no duty cycle**, a 400 ms dwell limit per transmission instead, plus
  frequency hopping. This is the region ldmux's own mesh plan targets.

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
- **Whether real hardware sustains ~2 packets/s** in the US without the mesh
  collapsing into collisions. Absence of a duty cycle is not the same as capacity.
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
  **US915 has no duty cycle at all**, only a 400 ms dwell limit and a frequency-hopping
  requirement — and US915 is what this document already targets, at MeshCore's **184-byte
  payload ceiling**. Adopting the EU figure would abandon a design that is legal here.
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

Not a build variant. One client binary, one daemon, a profile that would be announced in `caps` (today `caps.capabilities` carries only `scrollback`; no profile field exists).

| | `direct` (tailnet, LAN, cellular) | `mesh` |
|---|---|---|
| payload | raw pty stream, client emulates | plain lines the daemon renders |
| frame | 8-byte header + bytes | one line, capped to the payload MTU |
| keystrokes | per key | **one uplink per command** |
| backlog | full, coalesced at 4 KB | never replayed; state of now only |
| compression | permessage-deflate (14.8x measured on the old TS daemon; **not implemented in the Rust daemon**, see `WIRE-COST.md`) | not worth it at these sizes |
| client engine | libghostty-vt | **not used at all** |
| ownership, `new`, `close`, `list`, security | shared | shared |

The reason `mesh` is a mode and not a flag on the existing path is the last row of the
second column: in mesh mode the client's terminal engine is not merely slower, it is
**unused**, because there is no byte stream for it to emulate. The daemon has to be the
one that owns the screen.

### What does not change

Session model, registry, ownership and claims, the launcher, `new`/`close`/`list`, and
the entire security model. The profile changes the payload's shape and nothing about
what the payload means.

### What this asks of the direct path

Nothing, and that is the point of separating them. `direct` keeps getting the treatments
that its own measurements justify — 4 KB coalescing, threshold-gated deflate, backlog to
snapshot on a stalled client, Nagle off. The mesh profile is not allowed to justify
making `direct` worse, and `direct` is not allowed to justify making mesh mode heavier.
