# Mesh transport (LoRa) — is a terminal over MeshCore plausible?

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

An ldmux frame is a 40-byte header plus 9–25 bytes per cell. An 80x24 grid is 1920
cells, so roughly **17.3 KB per full frame**.

- 17,280 B / 184 B = **94 packets** for one screen.
- At ~0.08 s airtime each (the 5.5 kbps preset) that is ~25 s of airtime, and against
  the 400 ms dwell limit ~38 s wall clock.
- **So a full grid is about 0.03 fps.** It is not a frame rate problem to be tuned
  away. It is off by two orders of magnitude.

What does fit:

- **A line of text**: 40–80 B with the 5 B text overhead — **one packet**, inside every
  preset and inside the companion's 160 B cap.
- **A keystroke echo**: a handful of bytes, one packet.
- A 60 B line at 400 ms dwell gives **~2 lines/s** in the US. One line per second, on a
  link whose latency a user would call usable, is real. It would read like a good BBS.

## Why this is a different transport, not a lower frame rate

`FramePacing` (this repo) paces **rendering** against the measured round trip. It does
not change what is on the wire: a frame is sent when the pane changes, and a frame is
the whole grid. Lowering the cadence, which was done this week and is worth doing,
buys main-thread headroom and battery. It does not make the payload smaller, and mesh
is a payload problem.

A mesh path would need:

1. **A line-delta protocol.** The daemon sends a line, the client paints a line. The
   daemon already has the plain-text path this needs (`capture`), which matters,
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
- **MeshCore's region/preset in this deployment.** `docs/01-PLAN.md` targets US 902–928,
  which is the region that makes this viable. In EU868 the same idea is 1 packet per
  36 s and this document's verdict changes to "no".
- Whether the MeshCore companion API carries arbitrary binary for a custom application,
  or only the message types it defines.
