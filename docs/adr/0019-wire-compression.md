# ADR-0019 — The remote WebSocket negotiates permessage-deflate

**Status:** Accepted (2026-10-06)

## Context

The TypeScript daemon negotiated `permessage-deflate` (RFC 7692) on the remote
socket and `docs/ARCHITECTURE.md#9-airtime` measured 14.8x on terminal output with it. The Rust
daemon dropped that silently: tungstenite has no deflate, so the phone's client
(OkHttp, which offers the extension on every connection) has been getting raw
bytes since the port. The cost is real on a phone link: `ls -laR` is ~420 KB of
output, a `yes`-style flood is megabytes per second, and an attach snapshot is
~2 KB every time the app reconnects.

Two facts about the library decide the shape of the fix. tungstenite 0.26 rejects
an inbound frame with RSV1 set (`NonZeroReservedBits`), and it exposes no way to set
RSV1 on an outbound one: `Message` is converted to a frame inside the crate with
all reserved bits clear. There is no hook; "negotiating the extension" cannot be
done on top of it.

## Decision

1. **The daemon owns the server side of the WebSocket frame layer** (`crates/daemon/src/ws.rs`):
   the handshake response, frame parsing (masking, fragmentation, control frames,
   close codes) and frame writing. It is one code path for every listener: the
   tailnet TCP socket and the local Unix attach socket both end in the same
   `serve_upgrade`. tungstenite stays as the *client* (the desk attach window, the
   tests) and for `derive_accept_key`.
2. **permessage-deflate is negotiated only when the client offers it**
   (`ws::negotiate`). Honoured: `client_no_context_takeover`,
   `server_no_context_takeover` (both echoed), `client_max_window_bits` with or
   without a value (a 32 KiB inflater reads any smaller window). Declined:
   `server_max_window_bits` below 15 — the pure-Rust backend's window is fixed, and
   RFC 7692 §7.1.2.1 says a server that cannot honour an offer declines it, so the
   next offer (or no compression) is taken. An unknown parameter declines that
   offer. A client that sends no `Sec-WebSocket-Extensions` is served byte-for-byte
   what it was served before.
3. **Per-connection context takeover is kept** (sync flush, the `00 00 ff ff` tail
   removed). That is where the ratio comes from: terminal output repeats itself
   across messages, and a dictionary that persists for the connection sees it.
4. **One WS message = one frame**, compressed or not. The server never fragments,
   so the Android client's one-frame-per-message assumption holds.
5. **Messages under 96 bytes are sent plain** (RSV1 clear), without touching the
   compressor. The decision is made *before* compressing: a message compressed and
   then discarded would leave the two ends' dictionaries out of step. A plain
   message is in neither dictionary, so mixing is lawful.
6. **Level 3**, via `flate2` with `default-features = false`. This is the one new
   dependency; `AGENTS.md`'s allowlist names it and says why. Two backends are used,
   both pure Rust and neither one C: `rust_backend` (`miniz_oxide`) for the
   permessage-deflate this ADR is about, and `zlib-rs` for the mesh's compressed screen
   frame, because a frame is deflated **against a preset dictionary** and `flate2` exposes
   `set_dictionary` only for zlib-rs and the C zlib. zlib-rs is by the Trifecta Tech
   Foundation, MIT/Apache-2.0, and is listed in `THIRD-PARTY-NOTICES.md`; the alternative
   was a dictionary-free frame, which is ~25 % larger — three records instead of two on a
   public-preset link, i.e. 0.6 s of air per full screen.
7. **Compression cannot stall the connection loop.** A message over 16 KiB (a
   snapshot, a scrollback page) is compressed on the blocking pool (the compressor
   moves in and out; the connection is one task, so nothing else can reach it);
   smaller ones are inline (the worst measured 4 KiB message was 0.8 ms). Output frames are bounded by the keeper
   peek chunk (64 KiB), so a flood is compressed in 64 KiB pieces. The write timeout
   (`WS_SEND_TIMEOUT_MS`) bounds compression and write together.
8. **The inflater is bounded** to the inbound message cap (64 KiB, after
   inflation): a 1 MiB zero-filled message deflates to ~1 KB and is refused with
   close code 1009.

## Measurements

Real daemon, real keeper, real shell, loopback, a raw client that counts every
byte after the `101` (frame headers included). `cargo test -p ldmux-daemon --test
wire_compression -- --nocapture` prints this table and asserts the floors in the
right-hand column (well under what is measured, so they catch the extension turning
off or the dictionary being reset per message, not noise):

| Workload (80x24) | plain | deflate | ratio | floor |
|---|---|---|---|---|
| one keystroke echo | 11 B | 11 B (sent plain) | 1.0x | not larger |
| `ls -la --color` (40 lines) | 2,834 B | 673 B | 4.2x | 3.0x |
| `seq 1 100` | 524 B | 207 B | 2.5x | 2.0x |
| `ls -laR /usr/lib` (8,000 lines) | 421,497 B | 59,521 B | 7.1x | 5.0x |
| `yes \| head -c 4000000` (6.0 MB with the tty's CR) | 6,001,794 B | 8,682 B | 691x | 100x |
| attach snapshot, screen with content | 2,051 B | 612 B | 3.4x | 2.5x |

The old 14.8x was one corpus (`ls -laR` of a different tree, default zlib level);
the ratio is a property of the output, so treat 7x as the figure for a directory
listing and 3-4x for a screenful, not as a constant.

**Level and CPU** (release build, this machine, one thread; `cargo test -p
ldmux-daemon --lib --release compression_cost_report -- --ignored --nocapture`):

| data, 64 KiB messages | level 1 | level 3 | level 6 |
|---|---|---|---|
| `ls -laR` ratio | 5.6x | **7.1x** | 7.5x |
| `ls -laR` throughput | 182 MB/s | **61 MB/s** | 25 MB/s |
| `yes` ratio | 200x | 829x | 829x |
| `yes` throughput | 1189 MB/s | 204 MB/s | 179 MB/s |
| worst 64 KiB message (`ls` / `yes`) | 0.45 / 0.16 ms | **1.3 / 0.75 ms** | 5.0 / 1.8 ms |

Level 3 buys 27% fewer bytes than level 1 for a third of the throughput, and level
6 buys 6% more for another 2.4x CPU. A phone link is the scarce resource and a
shell's real output is KB/s, so 3. Under a `yes` flood the compressor does 200 MB/s
against a keeper that produces less than that through its emulator, and the 1.3 ms
worst message is bounded work per message on the blocking pool.

**The threshold** (mean wire size of consecutive 16.1024-byte fragments of `ls`
output, as a fraction of plain; same script, `threshold_report`): in a *warm* stream
compression wins at every size on repetitive text (0.52 at 16 B, 0.18 at 1 KB), but
the first message after a reconnect has an empty dictionary: **cold** it is 1.05 at
16 B, 1.08 at 64 B, 0.87 at 96 B, 0.62 at 192 B and 0.28 at 1 KB. Break-even is
~90 B, so the threshold is 96, and a keystroke echo (a handful of bytes that carry no
repetition at all) is never compressed. A deflate sync flush on its own adds bytes;
this is the cost the threshold avoids.

**Reconnect.** The context is per connection, so it is lost whenever the socket
is; the ratio right after a reconnect is the *cold* column above (an attach
snapshot, the first thing sent, compresses 3.4x cold) and recovers as output
flows. The phone reconnects often, which is why the numbers for snapshots are
measured cold.

**Interop.** An independent implementation (Python `websockets` 15, zlib) negotiated
the extension against the daemon, read compressed output, and sent compressed
messages with its own context takeover, all accepted. (`serve_for_interop` in
`wire_compression.rs` serves a daemon for any such client.)

## Output coalescing

Output is coalesced into one frame per 8 ms window (`OUTPUT_FLUSH_MS`) or 4 KiB
(`OUTPUT_FLUSH_BYTES`). That window was a trailing timer: a keystroke's echo waited
up to 8 ms behind it, which is latency for no saving (a lone small frame has nothing
to coalesce with). It is now a **leading-edge** flush: output arriving after a quiet
spell goes out at once, and what follows within the window is batched. A burst
still batches (the flood arrives as 94 frames for 6 MB, bounded by the 64 KiB
peek chunk, not by a timer), and echo never waits for the batch timer. Mosh caps
updates at about half the RTT with a floor near 20 ms because it has to pace
screen diffs over UDP; here the unit is a byte stream over TCP and 8 ms is
already below any phone RTT, so the interval was not changed.

## Alternatives

- **App-level zstd frames with a flag.** Needs a new frame type, a flag in `hello`,
  a zstd implementation on Android and iOS and in Rust (a C binding, which the
  dependency rule excludes), and per-client version matching. permessage-deflate
  is negotiated by the transport the clients already use, and OkHttp, URLSession and
  browsers offer it with no code change. Streaming zstd does compress better (Discord
  reported ≈40% less client bandwidth moving its gateway from streaming zlib
  to streaming zstd, a figure from their write-up, not measured here), and it is
  the upgrade path if deflate's ratio is ever the bottleneck.
- **Stateless per-message compression** (`*_no_context_takeover`). Supported when
  a client asks, but not the default: it throws away the dictionary, which is the
  gain. A published brotli comparison puts stateless at 72.5% saved against 83.5%
  for streaming (as reported by the source, not reproduced here).
- **Trained or preset dictionaries.** A 16 KB trained dictionary beat stateless in
  the same comparison (80.5%) but not streaming, and it needs the client and the
  server to agree on the dictionary's version for ever. No dictionary machinery.
- **Screen-state diffing** (what Mosh does: send the difference between two screen
  states instead of the bytes that produced them). The only approach that suits a
  very low-bandwidth, lossy link such as the mesh (packets of ~184 bytes), and not
  this WebSocket: here the client runs its own libghostty and needs the byte
  stream, and the wire-cost finding stands (the lever is fewer frames and state
  instead of history, not smaller frames). Recorded as the design for a future
  mesh bridge (ADR-0020); not implemented.

## Consequences

- The daemon speaks WebSocket itself. The risk is the frame layer's correctness,
  which is covered by the codec tests (masking, fragmentation interleaved with
  pings, oversize frames refused on the header, bad UTF-8, RSV1 without
  negotiation, an inflation bomb), the existing client tests (all tungstenite
  clients, unchanged and passing), and the independent-client run above.
- A compressor holds a few hundred KB of state per connection (64 connections at most; not measured precisely).
- The wire contract for existing clients is unchanged. `docs/ARCHITECTURE.md#9-airtime`'s
  provenance note and `docs/ARCHITECTURE.md#4-the-wire` should say the Rust daemon negotiates
  permessage-deflate again, with the table above in place of the TypeScript numbers.
