# What a frame costs on the wire, measured

Status: **measured, and the conclusion contradicts the assumption this file was
written to test.** Read the first section before reading any other document about
frame sizes, because the premise everyone reaches for — "a grid is ~17 KB, so shrink
the grid" — is **false for this codebase**.

## What the wire actually carries

`frames.ts:15` says it in so many words: *only `FrameType.Output` is emitted by the
shipped code; the remaining members [are unused]*. A frame is an **8-byte header
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
already is, fetched with `capture-pane`. So when a client is behind by more than it
can keep, the correct thing to send is **a snapshot, not the backlog**.

- Measured potential: 514,758 B → **~2,000 B**, one snapshot for that workload. Two
  orders of magnitude, on the worst case measured.
- What it costs: the client's scrollback loses what it skipped. That is honest —
  those lines were never going to be in a 1000-line buffer anyway.
- What it needs: a policy for "this client is too far behind", and the daemon
  already knows the answer (it drops and resyncs today when the pending buffer
  overflows: `MAX_PENDING_BYTES`). The mechanism exists; the threshold and the
  response are what would change.

**This is the answer to "how do we drastically lower it". Not smaller frames —
fewer, and state instead of history.**

### 2. The attach snapshot is hex-escaped, and that is a 6x tax

The snapshot is JSON text: `{"type":"snapshot",...,"screen":"\u001b[...]"}`. A single
ESC byte becomes the six characters `\u001b`. Measured: **1980 bytes** to carry a
screen whose raw bytes are about 330.

Sending it as a **binary frame** — an 8-byte header plus the raw stream — is about
**340 bytes**. That is an ~83% saving, on every attach, for a new frame type. The
unused `FrameType` members exist for exactly this.

It is a one-time cost per attach, so it is not the big number. It is the cheapest
one, and on a slow link the attach is precisely where it hurts.

### 3. Compression, which needs no protocol change at all

`permessage-deflate` is a WebSocket extension. Terminal output is extremely
repetitive — `-rw-r--r-- 1 ldeen ldeen 4096 Sep 18` repeated down the page — so
bulk output should compress 5-10x.

Two conditions, both real:

- The **client must offer it**. Ktor does not offer `permessage-deflate` by default,
  so enabling it on the daemon alone would activate nothing; the client needs
  configuring too.
- It must be **threshold-gated** (`perMessageDeflate.threshold`), or a 9-byte
  keystroke echo would get inflated by its own deflate header.

### 4. The coalescing window is sized for a datacentre, not a phone

`OUTPUT_FLUSH_BYTES = 16 * 1024`, which is why the largest frame measured above is
18,646 bytes. On a lossy link one dropped segment costs 18 KB of retransmission, and
the frame cannot be displayed until all of it arrives.

At 4 KB the framing overhead is 128 frames x 8 bytes = 1 KB out of 514 KB, or 0.2%.
The trade is 0.2% more bytes for a much smaller unit of loss and a shorter time to
first paint. That is worth taking, and it is a constant, not an architecture.

## The layer table

| Layer | Measured cost | What can be done | Available win |
|---|---|---|---|
| program output | 514 KB for `ls -laR` | send a snapshot instead of the backlog | **~250x** |
| pty stream as delta | already minimal | nothing — a terminal stream *is* the delta of its own state | — |
| snapshot encoding | 1980 B | binary frame instead of escaped JSON | **~6x** on attach |
| compression | not enabled | `permessage-deflate`, threshold-gated | **~5-10x** on bulk |
| frame header | 8 B x 32 = 0.05% | nothing worth doing | — |
| coalescing | 16 KB units | 4 KB units | costs 0.2%, buys loss behaviour |

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
