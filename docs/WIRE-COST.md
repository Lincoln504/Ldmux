# What a frame costs on the wire, measured

> **Provenance (2026-10).** The frame-size tables in the first two sections (the
> attach, keystroke, `ls -la`, `seq` and `ls -laR` byte counts, and the snapshot
> envelope breakdown) were taken against the TypeScript daemon. The Rust daemon
> carries the same frame format (`crates/protocol/src/frames.rs`), so those findings
> carry over; the flush constant is now 4 KiB (`OUTPUT_FLUSH_BYTES`,
> `crates/daemon/src/remote.rs`). **Compression is re-measured on the Rust daemon**
> (section 3, from ADR-0019): the Rust daemon negotiates `permessage-deflate` again,
> through its own WebSocket frame layer (`crates/daemon/src/ws.rs`).

Status: **measured, and the conclusion contradicts the assumption this file was
written to test.** Read the first section before reading any other document about
frame sizes, because the premise everyone reaches for — "a grid is ~17 KB, so shrink
the grid" — is **false for this codebase**.

## What the wire actually carries

`crates/protocol/src/frames.rs` defines more frame types, but only `OUTPUT` is emitted
on the remote socket by shipped code (the remaining members are reserved). A frame is an **8-byte header
followed by raw pty bytes**. There is no cell grid on the wire, and there never was —
the client runs its own libghostty and emulates the stream.

Measured against the live test daemon, 80x24 pane, over the tailnet:

| Workload | Frames | Bytes | Largest frame |
|---|---|---|---|
| attach (screen replay, JSON text) | text | **1980** | 1980 |
| one keystroke echo | 1 | **9** | 9 |
| `ls -la` | 1 | **1254** | 1254 |
| `seq 1 100` | 1 | **477** | 477 |
| `ls -laR` | 32 | **514,758** | 18,646 |

So the wire cost is the program's own output plus about 0.05% of framing. **A frame is
not expensive.** There is no "kilobytes per frame" to remove, and any document that
opens with a 17 KB grid is describing an in-memory render structure, not traffic.

## Then where do the bytes go

They go in the **volume of output**, and the lever is not a smaller frame. It is
sending **fewer frames**, and specifically: stopping the transmission of history the
client cannot keep.

### 1. Substituting state for history — the big one

`ls -laR` cost 514,758 bytes. What the user ever *sees* is 24 lines. The client's
scrollback is bounded (1000 lines by design), so almost all of that output is
processed, scrolled past, and dropped — and it **must** be processed, because the
bytes carry the state changes, not just the text.

But the daemon can produce the screen directly: that is what the attach snapshot
already is, rendered from the keeper's emulator. So when a client is behind by more than it
can keep, the correct thing to send is **a snapshot, not the backlog**.

- Measured potential: 514,758 B → **~2,000 B**, one snapshot for that workload. Two
  orders of magnitude, on the worst case measured.
- What it costs: the client's scrollback loses what it skipped. That is honest —
  those lines were never going to be in a 1000-line buffer anyway.
- What it needs: a policy for "this client is too far behind", and the daemon
  already knows the answer (it drops and resyncs today when a socket's queue passes
  `WS_BACKLOG_BYTES`, 64 KiB, in `crates/daemon/src/remote.rs`). The mechanism exists, and a
  client that advertises `inplaceResync` in `hello.features` is now answered with a fresh
  `caps` + `snapshot` (`resync: true`) on the same socket instead of a reconnect; the
  threshold and the policy for a client far behind are what would change.

**This is the answer to "how do we drastically lower it". Not smaller frames —
fewer, and state instead of history.**

### 2. The attach snapshot's envelope, corrected — the escaping is not the cost

**This section claimed a ~6x tax from hex-escaping. That was wrong, and it was never
measured.** Measured now, by attaching a `ws` client to a real daemon and weighing the
message at 120x40 with colour on screen:

| Part | Bytes |
|---|---|
| the whole message on the wire | **1985** |
| the `screen` field's own content | 1483 |
| the envelope (cols, rows, cursor, modes, epoch) | 247 |
| JSON quoting and escaping | 502 (**25%**) |

The 1483 characters are **real screen content**. The pane is 120x40, so 4800 cells, and
the daemon sends up to the last one with anything in it rather than padding out a grid.
Of the 502 bytes of overhead, 43 ESC bytes account for about 215 — six characters each
as `\u001b` — and JSON's own quoting accounts for the rest.

So a binary snapshot saves about **500 bytes, 25%**, not 83%. It happens once per attach,
and a phone resizes the pane to roughly 67x17 before snapshotting, which is a few hundred
bytes of content to begin with. **The lever is small.** It is worth taking when a frame
type is next added for another reason, and it is not worth a protocol change on its own.

### 3. Compression, which needs no protocol change at all (taken)

`permessage-deflate` (RFC 7692) is a WebSocket extension, so it needs no framing work in
the protocol. The Rust daemon negotiates it when the client offers it (ADR-0019): it owns
the server side of the WebSocket frame layer (tungstenite has no deflate and cannot set
RSV1), keeps per-connection context takeover, and sends messages under 96 bytes plain.
OkHttp (the Android client) offers it on every connection; a client that does not offer
it is served uncompressed, byte for byte as before. Measured on the Rust daemon (real
keeper, real shell, loopback, every byte after the `101` counted, 80x24), reproduced by
`cargo test -p ldmux-daemon --test wire_compression -- --nocapture`:

| Workload | plain | deflate | ratio |
|---|---|---|---|
| one keystroke echo | 11 B | 11 B (sent plain) | 1.0x |
| `ls -la --color` (40 lines) | 2,834 B | 673 B | 4.2x |
| `seq 1 100` | 524 B | 207 B | 2.5x |
| `ls -laR /usr/lib` (8,000 lines) | 421,497 B | 59,521 B | 7.1x |
| `yes \| head -c 4000000` (6.0 MB with the tty's CR) | 6,001,794 B | 8,682 B | 691x |
| attach snapshot, screen with content | 2,051 B | 612 B | 3.4x |

The ratio is a property of the output, not a constant: treat about 7x as the figure for a
directory listing and 3-4x for a screenful. The older 14.8x figure (TypeScript daemon,
`ls -laR /usr/bin`, default zlib level) was one corpus and is superseded by this table.

- **Level 3** of the pure-Rust `miniz_oxide` backend (via `flate2`, no C): 7.1x on
  `ls -laR` at 61 MB/s, against 5.6x at 182 MB/s for level 1 and 7.5x at 25 MB/s for
  level 6; the worst 64 KiB message takes 1.3 ms.
- **The 96-byte threshold** is the cold break-even: the first message after a reconnect
  has an empty dictionary, and a 64 B message compresses to 1.08x its size cold. A
  keystroke echo is never compressed.
- **Reconnect is cold.** The context is per connection, so the ratio right after a
  reconnect is the cold figure (a snapshot compresses 3.4x cold) and recovers as output
  flows. Snapshots are measured cold for that reason.
- **Bounded.** A message over 16 KiB is compressed on the blocking pool; the inflater
  refuses a message over the 64 KiB inbound cap (close code 1009).
- Interop with an independent implementation (Python `websockets` 15) was checked
  against the daemon (ADR-0019).

The server-side compressor state is not measured precisely (ADR-0019 estimates a few
hundred KB per connection).


### 4. The coalescing window is sized for a datacentre, not a phone

`OUTPUT_FLUSH_BYTES` was `16 * 1024` when this was measured, which is why the largest
frame above is 18,646 bytes; it is now 4 KiB, and the flush is leading-edge (output after
a quiet spell goes out at once, what follows within the 8 ms window is batched; ADR-0019,
"Output coalescing"). On a lossy link one dropped segment costs 18 KB of retransmission, and
the frame cannot be displayed until all of it arrives.

At 4 KB the framing overhead is 128 frames x 8 bytes = 1 KB out of 514 KB, or 0.2%.
The trade is 0.2% more bytes for a much smaller unit of loss and a shorter time to
first paint. That is worth taking, and it is a constant, not an architecture.

### 5. Volume follows the link (2026-10-24)

Sections 1–4 are about *what* is sent. This is about *how much*, which is the other
half of "responsible on a slow link": a request that is fine at 20 ms is a stall at
200 ms, and the difference is not the frame rate (the pacing already handles that) but
the size of the one message a user is waiting on — a history page.

- **The history page is sized by the link.** A `scrollback` reply is the visible screen
  plus the requested rows, replayed into the client's emulator, and each further pull
  re-sends everything older again, so a pull is quadratic in the number of pages.
  5,000 rows of an 80-column screen is ~400 KB before compression: instant on a fast
  link, a visible stall on a slow one. `FramePacing` now classifies the link
  (`slow`, hysteresis 150/90 ms) and the first page is `1,000` rows on a slow link
  against `5,000` on a fast one, with a `4,000`/`20,000` ceiling for the doubling the
  controller does as the user keeps pulling. The policy is `historyPageLines()` /
  `historyMaxLines()`, tested in `FramePacingTest`.
- **A behind client is repaired in place.** The phone now advertises
  `inplaceResync`, so a socket that falls past `WS_BACKLOG_BYTES` gets `caps` +
  `snapshot` on the same socket rather than a reconnect — section 1's two-order-of-
  magnitude lever, and the event that used to make ownership look like it had changed
  hands (ADR-0010 amendment).
- **A slow link is not a stuck one.** The drain waits for an in-place resync and a
  deferred scrollback page used to have a fixed 10 s deadline; a 24 MB backlog on a
  poor path is minutes of honest work, and closing that connection turned "slow" into
  "reconnect and rebuild". The deadline now runs only while the unsent backlog is not
  falling (`Drained` in `crates/daemon/src/remote.rs`). A link that is draining is
  never declared stuck, and one that is not is still closed in 10 s.

The direction of the repaint interval is the opposite and was already right: a slow
link is painted *less* often (`RTT / 2`, clamped to 5–20 fps), because the frames
would arrive after they were useful anyway. Volume is not like that: fewer frames cost
nothing, but a smaller *page* costs the user scrollback, so it is only cut when the
link cannot deliver it.

## The layer table

| Layer | Measured cost | What can be done | Available win |
|---|---|---|---|
| program output | 514 KB for `ls -laR` | send a snapshot instead of the backlog | **~250x** |
| pty stream as delta | already minimal | nothing — a terminal stream *is* the delta of its own state | — |
| snapshot envelope | 1985 B | binary frame instead of JSON | **25%** of one message per attach |
| compression | 7.1x on `ls -laR`, 4.2x on `ls -la`, 3.4x on a snapshot, 691x on a flood (Rust daemon) | permessage-deflate, negotiated when offered (ADR-0019) | taken |
| frame header | 8 B x 32 = 0.05% | nothing worth doing | — |
| coalescing | was 16 KB units | 4 KB units (done: `OUTPUT_FLUSH_BYTES`) | taken |
| history page | 5,000 rows ≈ 400 KB, quadratic per pull | **1,000 rows** on a slow link (done: `FramePacing.historyPageLines`) | 5x per pull |
| repair after falling behind | close + reconnect + rebuild | **caps + snapshot in place** when the client advertises `inplaceResync` (done on the phone) | section 1 |

## The two things that follow

1. **Do not shrink the frame format.** It is already the minimal description of a
   terminal's change: the program prints what changed, and we forward it. Any
   cell-delta scheme would have to re-derive that, and for scrolling text it would be
   *larger*, not smaller.
2. **Do shrink the backlog.** The inefficiency is sending a client everything that
   happened while it was behind. That is the only place with an order of magnitude in
   it, and it is a policy change rather than a format change.

For the mesh question this is decisive and is why `MESH-TRANSPORT.md` was corrected:
MeshCore's 184-byte payload fits an **interactive** frame with room to spare (a
keystroke echo measured 9 bytes), and cannot fit a 477-byte one, so a mesh transport
needs the flush cap at the MTU and snapshot catch-up — the first and fourth rows of
the table above, both of which are policies rather than formats.

## Latency, hop by hop (2026-11-03)

Bytes and time are different questions, and the second one is the one a user feels. This is the
chain as built, what each link costs, and which term is worth attacking.

**Tailnet route.** `key → WebSocket → daemon → unix socket → keeper → PTY`, and the echo back the
same way, then the client's own render throttle.

| Link | Cost | Where it is |
|---|---|---|
| client render | 50-200 ms, adaptive on measured RTT, **coalesced rather than delayed** | `FramePacing.kt` |
| tailnet RTT | the network's | - |
| WebSocket frame | 8-byte header plus the bytes; input is one small text frame | `crates/protocol/src/frames.rs` |
| daemon ↔ keeper | one JSON request and reply over a unix socket | `crates/keeper` |
| PTY write | the kernel's | - |

Measured on this box: a whole `ldmux snapshot` invocation — process start, socket connect, RPC,
keeper round trip, JSON decode — is a 4.7-5.9 ms round trip (min/median/max of eight runs). The
local hops are inside that, so they are not the term that matters. What does matter on this route
is the network RTT and the fact that a keystroke is echoed by the *program*, so the round trip is
at least two network legs. That is what predictive echo is for (`docs/PREDICTIVE-ECHO.md`), and
the phone has the setting off by default.

**Mesh route.** `key → BLE → radio → LoRa → radio → USB serial → gateway → TCP JSON → daemon →
unix socket → keeper → PTY`, and the mirror image for the screen. The terms, in order:

1. **Airtime per record**, 0.5-3.5 s depending on the preset. Nothing else in the chain is close.
2. **The bridge's burst gap**, up to 1 s by design (`BURST_GAP`): a screen that keeps changing is
   sent once a second, because a record carrying the present screen is worth more than several
   carrying moments that have already passed.
3. **The phone's input batch**, 100 ms after the last key or 200 ms from the first
   (`MeshInput.IDLE_MS` / `MAX_HOLD_MS`): one record for a burst of typing instead of one per key.
4. **The ARQ's ack delay**, 150 ms, and only when nothing else is going out — an ack rides on the
   next record whenever there is one.
5. **BLE notification spacing**, 60 ms on the ESP32 firmware: at most about 16 records a second
   from the radio, whatever the radio is doing.
6. **The gateway's TCP JSON hop**: loopback, sub-millisecond. Negligible.

## Intermediaries: which ones earn their place

- **The keeper is not a relay.** It owns the PTYs, outlives the daemon so shells survive a daemon
  restart, and runs the terminal emulator the daemon snapshots. Removing it would put PTY ownership
  in the daemon and lose the survival property; its latency cost is local and measured above as
  negligible.
- **The host gateway is a real intermediate.** It owns the USB serial port, speaks the MeshCore
  companion protocol and re-exposes the L2 contract as JSON over loopback. Its *latency* cost is
  negligible, so removing it is not a latency project. It is an operational one: one more process,
  one more language (Dart), one more repository (`meshmore-sns`), one more version to keep in step,
  and one more thing that can be down while the daemon looks healthy. The removal is a Rust
  companion-protocol client inside `crates/mesh` over a serial port: the frame codec already exists
  twice in this tree (`packages/mobile/vendor/libmeshcore` in Java, and the Dart reference it was
  ported from), and `crates/mesh` already contains the L2 contract the daemon speaks, so the work
  is the transport and the command/event mapping rather than the protocol.
- **The bridge is not an intermediary but a translator**, and it is where the mesh latency control
  lives: it decides when a screen update becomes a record.

## What to attack next, in order of latency per unit of risk

1. **Predictive echo on a slow link.** The machinery exists and is off by default; turning it on
   where the measured RTT is high is the only change that hides a whole round trip.
2. **A shorter burst gap for small, interactive updates.** One second is right for a screen that is
   scrolling; it is long for a prompt that printed one line. A gap that adapts to how much of the
   screen changed is the obvious next step, and needs a radio to measure honestly.
3. **The gateway removal, for operations rather than latency.** One process and one language fewer,
   and a daemon that cannot be up while its radio path is a missing service.
4. **Nothing on the tailnet local path.** It is already inside the 5 ms the CLI proves, and the
   remaining cost is the network and the program's own echo.
