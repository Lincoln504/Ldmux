# The remote path on a high-latency link

Written 2026-11-04. This is the audit record for one question: **what in ldmux breaks or
degrades when the Tailscale path is slow** — RTT 300 ms to 2 s, jitter 100 to 800 ms,
occasional 2 to 10 s stalls, 1 to 5% loss, and a link that drops for 30 s and comes back.
It separates what is measured from what is arithmetic on the constants, names the changes in
impact order, and records the decisions that are still open rather than taking them.

The mesh (LoRa) path has its own record in [`MESH-LATENCY.md`](MESH-LATENCY.md). This one is
about the WebSocket path, and about the constants the two share.

## The steady-state budget

One keystroke over the tailnet, phone side:

```
keypress -> TCP/TLS over WireGuard -> ldmuxd connection_loop
  -> keeper RPC over the local Unix socket -> PTY write -> shell
shell echo -> keeper emulator -> daemon output -> coalescer (8 ms cap)
  -> WS frame -> tailnet -> phone
```

Software added latency is **2 to 10 ms**, because output that arrives after a quiet spell is
flushed at once and only what follows within `OUTPUT_FLUSH_MS` is batched
(`crates/daemon/src/remote.rs:152`, and the leading-edge rule in `connection_loop`). There is
no server-side echo delay, deliberately ([`PREDICTIVE-ECHO.md`](PREDICTIVE-ECHO.md)).

So on this path **the budget is the RTT**, and no protocol change can beat it. Perception is a
different question, and the phone already answers it with predicted echo gated on smoothed RTT
(`Predictor.kt`, `FramePacing.kt`). The desktop attach client has **no predictor at all**
(`crates/attach` has no match for `predict`), so a desktop over a slow link pays the full RTT.

What a slow link actually breaks is not the data path but the **repair path**: the liveness
replies, the write deadlines, and the grace that decides how long a shell stays claimed.
Each of those turns *slow* into *disconnected*, and the reconnect then repeats the work that
was slow.

## Every constant on the live path

| Constant | Where | Value | Assumption it encodes | What 1 to 2 s RTT needs |
|---|---|---|---|---|
| `OUTPUT_FLUSH_MS` / `OUTPUT_FLUSH_BYTES` | `remote.rs:152-153` | 8 ms / 4 KiB | batching, not latency | fine |
| `PING_INTERVAL_MS` | `crates/attach/src/lib.rs:234`, `Heartbeat.kt:19` | 4 s | the client speaks faster than the 20 s reap | fine to 8 s RTT |
| `MAX_MISSED_PONGS` / `CLIENT_MISSED_PONGS` | `attach/src/lib.rs:239`, `Heartbeat.kt:26` | 3 | 12 s of silence means death | needs the round trip under 8 s, and no single write over 12 s |
| `CLIENT_PING_SEND_TIMEOUT_MS` | `Heartbeat.kt:37` | 2 s | a hand-in that blocks 2 s means a dead route | fine |
| `read_stall_limit` | `attach/src/lib.rs:253-255` | max(7 x ping, 15 s) = 28 s | a pong every 4 s | fine |
| `SEND_TIMEOUT_MS` (desk writes) | `attach/src/lib.rs:264` | 10 s, whole frame | any local write is fast | keystrokes are small, so fine |
| `WS_IDLE_MS` | `remote.rs:208` | 20 s | client pings every 4 s | fine |
| `WS_SEND_TIMEOUT_MS` | `remote.rs:234` | 10 s **per slice**, since 2026-11-04 | the peer accepts a 64 KiB slice within 10 s | now correct (was per message) |
| `WS_BACKLOG_BYTES` | `remote.rs:206` | 64 KiB | queue length, not time | fine |
| `MAX_PENDING_BYTES` | `remote.rs:150` | 1 MiB | flood between subscribe and snapshot | fine |
| `SNAPSHOT_ALLOWANCE_BYTES` | `remote.rs:159` | 4 MiB | one history page is a few MiB of JSON | needs the write to be slice-bounded, not message-bounded |
| `DRAIN_WAIT_MS` / `Drained` | `remote.rs:170-198` | 10 s **without progress** | slow is not stuck | correct by construction |
| `SCROLLBACK_MIN_INTERVAL_MS` / `MAX_SCROLLBACK_LINES` | `remote.rs:162,242` | 200 ms / 20 000 | a fixed page is a fixed cost | the page is a fixed cost but the link is not |
| `MAX_QUEUED_JSON_BYTES` | `remote.rs:237` | 16 MiB, then `Out::Close` | the peer stopped reading | a slow peer can reach it |
| `CLAIM_GRACE_MS` | `daemon.rs:88` | 3 s | claim to attach is sub-second | too short for the mesh path |
| `RECONNECT_GRACE_MS` / `RELEASE_GRACE_MS` | `daemon.rs:100`, `remote.rs:222` | 15 s | reconnect finishes inside 15 s | holds only to about 1.3 s RTT |
| `CLAIM_TICK_MS` | `daemon.rs:105` | 1 s | fine | fine |
| `RESTORE_GRACE_MS` | `daemon.rs:443` | 60 s | the phone backs off at most 10 s | fine |
| `RETRY_BASE_MS` / `RETRY_MAX_MS` | `LinkTiming.kt:15-16` | 0.4 s to 10 s | after a network change | fine |
| `NETWORK_PROBE_MS` | `LinkTiming.kt:25` | 2.5 s | one way is under 2.5 s | breaks above 2.5 s one way |
| ARQ window / ack delay / first retransmit / cap / attempts | `crates/session/src/arq.rs:131-135` | 8 / 150 ms / 2 s / 30 s / 5 | 2 s is about 10x a 62.5 kHz round trip | unchanged, see `MESH-LATENCY.md` |
| `REFUSED_RETRY_MS` / `REFUSAL_DEADLINE_MS` | `arq.rs:256,265` | 350 ms / 30 s | the radio's transmit-table pace | unchanged |
| `AWAITING_DEADLINE` / `BUDGET_PROBE` / `GATEWAY_TIMEOUT` | `daemon/src/mesh.rs:46,56,64` | 10 s / 60 s / 10 s | the gateway answers promptly | see the ambiguity below |
| `TICK_MS` / `QUIET_TICK_MS` / `QUIET_AFTER_TICKS` / `BURST_GAP` | `mesh_bridge.rs:795,803,826` | 50 / 150 ms / 8 / 1000 ms | interactive echo | fine |

The grace and tick deadlines are fields on the daemon (`with_*` builder hooks plus the
`LDMUX_CLAIM_GRACE_MS`, `LDMUX_RECONNECT_GRACE_MS`, `LDMUX_RESTORE_GRACE_MS` and
`LDMUX_CLAIM_TICK_MS` environment variables), so a test or a deployment on a slow path can
change them without a rebuild.

## What was wrong, and what is now fixed

- **F1, the write deadline was elapsed, not progress-based.** `send_bounded` wrapped the whole
  message in one `timeout(WS_SEND_TIMEOUT_MS)`, so a 4 MiB snapshot page on a link moving
  40 KiB/s was killed exactly like a wedged peer: the daemon closed the connection, the claim
  was released after the release grace, the client reconnected, and the first thing it did was
  ask for the same page. The daemon's maximum message size was silently
  `throughput x 10 s`.
  **Fixed 2026-11-04:** the writer checks the allowance **per 64 KiB slice**
  (`crates/daemon/src/ws.rs`, `send_binary_stalling`), so the deadline now means *nothing was
  accepted for ten seconds* — the same rule as `Drained` ("without movement, not elapsed").
  A slow link finishes, a dead one is still cut off after ten seconds. Three tests pin it: the
  sliced frame is byte-identical to the plain one, a peer that stops taking bytes fails at the
  allowance, and a link that takes 4 KiB every 2 ms delivers 512 KiB that no single deadline
  would have allowed.

- **F2, `RECONNECT_GRACE_MS` is shorter than the client's own reconnect above about 1.3 s
  RTT.** The client's worst case on the heartbeat path is 3 missed pings (12 s) plus
  `RETRY_BASE_MS` (0.4 s) plus two round trips to connect and upgrade: `12.4 + 2R`, which must
  fit in 15 s, so `R <= 1.3 s`. Above that the daemon frees the shell before its own device can
  re-attach, and a desk window takes it.
  **Not fixed.** The constant's comment derives 15 s from the *network-change* path
  (`NETWORK_PROBE_MS` 2.5 s + `RETRY_BASE_MS` 0.4 s), which is 5x more than that path needs and
  2 s short of the heartbeat path at 300 ms RTT. It cannot be right for both paths at a fixed
  value. See the open decision below.

- **F3, a pong can be queued behind a large message.** A ping is answered by queueing a pong on
  the connection's single ordered channel (`remote.rs`, the `"ping"` arm), and the loop is
  inside the write while a frame goes out, so a pong already queued waits. Both clients count
  only pongs as liveness, so a message that occupies the loop for more than 12 s makes a healthy
  client declare the link dead.
  **Partly mitigated, not fixed.** The slice-bounded write means a slow link finishes; it does
  not mean the pong goes out during the write. Closing it needs a protocol change: cap one
  control message (snapshot, scrollback page) at a size that can be written inside the liveness
  window, and let the client ask for the rest with the additive `features` mechanism
  (`REMOTE.md`, "Additive features"). That is a decision, not a bug fix, because it changes the
  wire.

- **F4, no predictive echo on the desktop.** The phone predicts typed text and the desktop does
  not, so the same link feels instant on one and lagged on the other. `crates/attach` has no
  predictor at all.
  **Not started.** The recovery primitive the predictor needs (an exact snapshot on demand, and
  in-place resync) already exists.

- **F5, `NETWORK_PROBE_MS` is a one-way budget.** A network change concludes "offline" after
  2.5 s, so above 2.5 s one way a working link is reported offline.
  **Not changed.** It should scale with the smoothed RTT the client already tracks.

## Protocol changes, in impact order

| # | Change | Impact | Effort | Note |
|---|---|---|---|---|
| P1 | Slice-bounded write stall allowance | done | small | the fix above |
| P2 | Cap one control message and let the client page the rest | high | medium | needs an additive feature and a client that asks for it |
| P3 | Derive the claim and release grace from measured RTT | high | small | the daemon can time ping arrivals without new protocol |
| P4 | Predictive echo in `crates/attach` | high for the desktop | medium | port `Predictor.kt` and `FramePacing.kt` |
| P5 | Scale `NETWORK_PROBE_MS` with smoothed RTT | low | small | one constant and its test |
| P6 | Cap scrollback lines by measured throughput rather than a fixed 20 000 | low | small | a page is served in `SCROLLBACK_MIN_INTERVAL_MS` steps today |
| P7 | Selective or gap acknowledgement in the ARQ | medium | large | a new record kind, negotiated in the handshake, because a set reserved flag bit makes an old peer drop the record (`arq.rs:35`) |
| P8 | Add an RTT variance term to `rto` | medium | small | `arq.rs:505-515` uses `max(initial, srtt)` with no dispersion term |

## What must not change

- **The exact full snapshot on demand, and in-place resync.** It is what makes prediction safe
  and a mispredict recoverable in one round trip. Diffs would trade one round trip for state.
- **The additive `features` gate.** A client that says nothing keeps getting exactly the old
  stream. This is the only safe way to add P2.
- **One WebSocket message is one frame, never fragmented.** Chunking, when it comes, is at the
  message level.
- **The refusal semantics on the radio path.** A refusal is "not yet", retried at 350 ms with a
  30 s bound, never a failure (`arq.rs:256,265`).
- **Re-sealing each transmission.** MeshCore dedupes by packet hash at every receiver and
  repeater, so a byte-identical retransmission is dropped and the session dies with both radios
  healthy (`arq.rs:18-23`).
- **No server-side echo delay.** It is an SSP property in mosh, and adding one here would slow
  every client to help none.

## What to measure next

1. **A latent and throttled link in the harness, no radios.** The daemon's sockets are
   `Box<dyn AsyncRead/Write>` (`remote.rs:52-55`), so a wrapper that delays each direction by
   R/2 and caps bytes per second can be inserted with no protocol change. Then, with the raw
   WebSocket client in `crates/daemon/tests/common/wsraw.rs`:
   - ask for a full 20 000-line scrollback at R = 2 s and 128 KiB/s and assert the page arrives
     and the connection survives,
   - record the **pong arrival gap** during that transfer (the direct test of F3),
   - count reconnects per scrollback request (the livelock detector).
2. **The claim-grace boundary.** Drive a reconnect at R of 0.3, 1.3 and 2.0 s and assert the
   claim is still held. Today the expected result is pass, pass, fail exactly at the F2
   arithmetic. The builder hooks make this a fast test.
3. **ARQ loss injection.** `arq.rs` is a pure state machine with time as an argument: drop every
   Nth ack in a unit test and measure records per second delivered at 1% and 5% loss.
4. **On the radio**, keystroke-to-echo median and p90 over 20 attempts, as `MESH-LATENCY.md`
   prescribes. Not run: the gateway holds the port.

## Open decisions

- **What the 15 s reconnect grace is for.** If it covers the heartbeat path it is 2 s short at
  300 ms RTT and fails outright above 1.3 s, so it needs to be `max(15, 12.4 + 2 x srtt)` or
  similar. If it covers only the network-change path it is five times the need and holds a shell
  from other devices for no reason. The two answers point in opposite directions, so this is a
  product decision, not a constant to tune.
- **Whether a control message may be split.** P2 is the only fix for a pong waiting behind a
  large page, and it changes the wire. The alternative is to cap page size, which makes a slow
  link slower but keeps one message per frame.
- **Whether `AWAITING_DEADLINE` (mesh, 10 s) or MeshMore's `linkTimeout` (3 s) is right.** Both
  carry the same comment, that the firmware answers as soon as `sendDirect` returns, and they
  differ by 3.3x (`mesh.rs:41-42` against
  `meshmore_sns_app/packages/meshcore_gateway/lib/src/serial_mesh_sender.dart:88-91`). Neither
  cites a measurement of how long the firmware actually takes.
- **`MAX_QUEUED_JSON_BYTES` closes a connection that is slow rather than dead.** 16 MiB of queued
  control JSON is reachable by several scrollback pages on a slow link, and the close is
  indistinguishable from a failure at the client. Either the page cap comes first (P2) or this
  becomes a backpressure answer instead of a close.
