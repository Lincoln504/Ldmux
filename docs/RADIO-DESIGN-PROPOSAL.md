# The radio path, redesigned

A replacement design for the MeshCore carrier in ldmux, written against the 313 findings in
`docs/RADIO-AUDIT-LOOP.md`. The goal is not to patch the Register's fifteen rows one by one: it is to
choose rules under which those failures cannot occur, and to say plainly what each choice costs.

Three facts from the ledger shape everything below.

1. **One crossing costs 6.9 s direct and 46 s flooded at 14 hops** (A-340's firmware arithmetic), and one
   record's airtime is 490 ms. Anything that adds a round trip adds 7–46 s, so the design's first
   principle is *fewer exchanges*, and its second is *never pay a flood for something a direct packet
   can carry*.
2. **The link runs at about 0 dB of margin** (A-40: readings of −6.75 and −8.5 dB SNR against SF7's
   −7.5 dB limit). A single packet is a coin toss, so reliability must come from *time-diverse* repeats
   and from *short* packets, never from longer chains of round trips.
3. **The ARQ already measures the true round trip on every ack** (`arq.rs:670-683`, smoothed 1/8,
   first-attempt samples preferred) and that number is discarded at every boundary (A-460). Every timer in
   the redesign is derived from it, or from the firmware's own forwarding formula, and never from a
   constant.

## Principles

- **E**vidence over state. A decision is made from something observed, about *this* send or *this* path. No
  global flag decides a carrier.
- **D**erivation over constants. Every deadline is either a measurement (`srtt`), a formula from the
  firmware (`calcRxDelay`, airtime), or a *named policy* whose cost is in the comment.
- **O**ne exchange fewer. New messages are added only when they remove a round trip or make a failure
  visible.
- **V**isibility at the point of decision. Each transmit states the carrier, attempt and timer it chose,
  so the policy is readable from logs rather than inferred (A-390, A-220).
- **B**ounded by the link. Anything that can wait a long time waits a time the route justifies, and every
  wait has an observable progress signal.

---

## 1. Carrier choice: per attempt, never per record

**Decision.** The carrier is a property of *one send attempt*. There is no per-link carrier state.

```rust
enum Route { Unknown, ZeroHop, Hops { path: PeerPath, proven: bool } }

enum Attempt { First, Retry(u8) }

fn carrier_for(route: &Route, attempt: Attempt) -> Carrier
```

- `Attempt::First` on `Route::Hops { .. }` is **direct**. Always, proven or not. A direct attempt is
  cheap (7.8 s of air for a 133 B record at 16 hops) and it is the only thing that can prove a path.
- `Attempt::Retry(n)` on `Hops` is **flood**, built with the path masked for the duration of the build
  only, restored on every exit path. That is the reference's rule (`base_send.py:1292-1346`) and the
  firmware's own (`MyMesh.cpp` masks `out_path_len` to `OUT_PATH_UNKNOWN` for one forced-flood request).
- `Route::Unknown` and `Route::ZeroHop` are distinct values, so "flood" is asked for by exactly one of
  them (A-392). The current four-way collapse (`peer_path: Option` plus a separate `path_stale()`) is what
  made a flood-on-stale rule look reasonable.
- There is no `peer_via_flood`, no `directWorks`, no `direct_silent`. A latch is not a bug to be fixed in
  this design, it is unrepresentable: nothing persists between attempts, so there is no state to be
  absorbing.

**What decays, and on what evidence.** The only link-level fact is *which attempt index the next send
should start at*, and it is reset by exactly one event: **an arrival that proves the direct carrier
works** (see §2). A record arriving by flood does not reset it; a record arriving direct does. That is the
reference's decay rule, and ADR-0038's own sentence ("any record arrives → failures = 0"), with the one
correction the ledger's A-381 demands: a flood arrival is not evidence about direct, it is evidence that
*the peer cannot reach us*.

**What a flood arrival means (A-381).** If a record arrives by flood while we hold a path, the peer has
no usable route back to us. The response is a **repair**, not a mirror: send a path teach to the peer
**direct** along the path we hold, throttled per peer by a 5 s cooldown (`return_path.py:120-136`). The
teach is one 40 B direct packet, 3.7 s at 16 hops, against 43 s of extra cost per record if the path is
not repaired.

**Consequences.**

- A single stale moment no longer sends every subsequent record by flood: the cost is bounded to *this
  send's* retry. A-120, A-340, A-361, A-121 all disappear as classes.
- The phone and the host must agree on attempt identity, because the phone's `LDMUX` retries and the
  host's ARQ retries are different things. The rule: attempt index is a property of the ARQ slot, passed
  down to the carrier. A retransmission of the same slot is `Retry(n)` and floods; a fresh slot is
  `First` and goes direct.
- A flood costs a retry its latency: a lost direct record costs one crossing (6.9 s) before the flood
  attempt starts. That is correct: the alternative is flooding records that were never lost.

**Cost.** One enum and one function, replacing two boolean expressions on each end. The `Carrier` value
must be threaded from the ARQ slot to `Command::Record` on the host and to the tracked-send table on the
phone.

**Ledger entries answered:** A-120 (blocker), A-340 (blocker), A-361, A-380 (blocker), A-381 (blocker),
A-392, A-125, A-126, A-129.

**Unsure.** Whether a phone's first attempt after a handshake that completed over flood should be
`First` (direct) or `Retry` (flood). I choose `First`: a flood handshake says nothing about direct, and
testing it costs 7.8 s of air, whereas assuming it costs a 43 s per record surcharge indefinitely. This
should be settled by measurement, not argument (see §9).

---

## 2. Proof: one event sets it, and only one

**Decision.** `proven` is set by a **record that arrived direct** on that path, and by nothing else. A
flood chain never sets it.

Two facts are kept apart, because conflating them is the shape of A-360:

| fact | learned from | what it may be used for |
|---|---|---|
| **an inbound chain exists** | any arrival: flood chain reversed, advert path, route request | sending a *teach*, and as a lower bound on timing |
| **this path carried our traffic** | a record that arrived **direct**, or an ACK that crossed direct | blocking a shorter candidate; resetting the attempt index |

Today the flag that means "traffic crossed this path" is set only by a flood (A-360) and never by the
direct arrival that proves it (A-370). That inversion is why a working path is displaced by a ghost one
second after it proves itself, and why the peer's own fresh chain cannot displace a reversed guess.

**Also: reversal is a guess about the outbound direction.** The fleet's reference implementation had to
*remove* an earlier revision that taught the reverse of its own outbound path, because asymmetric routes
converted a working route into a dead one (`return_path.py:76-86`). Ledger A-393 records that our
reversals are of *observed* chains, which is the corrected rule. The constraint that follows, and that
must be written into the code: **any future path repair teaches an observed chain or uses a
request/reply round trip; it never reverses our own outbound path.**

**Route ranking (A-383, A-391).** Hops alone is noise at the margin: one copy at −124 dBm/−8.5 dB SNR
currently outranks a six-hop copy at −107 dBm/+5 dB, which is how the adopted path flipped 1↔16 hops in
eleven seconds (A-80's journal). Adopt the reference's two-tier rule: compute `margin = snr −
demod_limit[sf]`; margin ≥ 6 dB ranks by hops, below 6 dB ranks by margin then hops, and below 2 dB is not
a route. The host already captures `(snr, rssi)` per received record (`mesh_serial.rs:1289`, `:2210`,
`:2281`).

**Ledger entries answered:** A-360 (blocker), A-370 (one-line at heart), A-383, A-386, A-391, A-393,
A-364 (with the teach), A-365 (inverted: a flood from a peer that already has a path means *our* teach was
lost, so re-send it, not adopt their chain).

**Unsure.** Whether a direct *record* is sufficient proof or whether proof requires the peer's
acknowledgement of our packet. Upstream uses an ACK. We have both: the ARQ's ack crosses the same path.
I choose "a direct record or a direct ack", because the ARQ's ack is already the stronger of the two and
the phone's `directWorks` already treats an arrival as proof, so the ends agree.

---

## 3. Timing: every deadline derived

**Decision.** One function produces "how long one crossing over this route takes", and everything else is
expressed in it.

```rust
// The honest cost of one crossing: the record's air delivery plus every repeater's hold.
fn crossing(route: &Route, len: usize, airtime_ms: f64, score: f64) -> Duration {
    let hops = route.hops();                                  // never shortened by inference
    hops * (airtime_ms + hold_ms(score, airtime_ms))          // hold from calcRxDelay
}

// Preferred: what the link actually measured.
fn rto(session: &Arq) -> Duration {
    session.smoothed_rtt_ms()                                  // arq.rs:475, first-attempt samples
        .map(Duration::from_millis)
        .unwrap_or_else(|| crossing(...) * 2)                  // one round trip
}
```

`hold_ms(score, air)` is the firmware's own formula (`Dispatcher.cpp:55-57`):
`(10^(0.85 − score) − 1) × air`, clamped 0…32 s. At the margin a 133 B packet scores 0.036, giving
2.70 s per repeater; at home, 0.66 s.

**The number that must never be shortened.** Timing hops come from the *maximum* the route can be, never
from a shorter candidate that merely arrived first: `max(inbound_chain, mesh_reach)`. A-80's 2.94 s first
retransmission against a 43 s crossing is the entire "it handshakes and then nothing works" class, and it
is a one-line inversion.

**Budgets are sums of the same quantity, never independent constants.**

```
attempt_budget = attempts × rto + one_answer_window
answer_window = max(rto, 120 s)        // the reference's own multi-hop reply observation
```

This is the fix for A-511 (launcher expires at 120 s inside a ≥176 s budget), A-101/A-105 (answer
deadlines below the ARQ's own first timer), A-28 (a verdict at 30 s inside a 372 s ladder), and A-62.

**Staleness is a policy, and it is never shorter than one crossing.**

```
path_idle_limit = max(2 × crossing, 60 s)    // 60 s is policy: a quiet route may be idle
```

Named as policy, with its cost in the comment. A-362's defect is that 60 s is *shorter than one round
trip* at 16 hops (102 s), so a path that is actively carrying is declared stale mid-exchange. The two
ends must compute the same figure from the same inputs; today the host uses 60 s and the phone a flat
120 s.

**What is not adopted.** The reference's 12 s per-attempt cap and immediate re-send (`timing.py:24-31`).
A retransmission inside its own crossing is pure waste on a half-duplex shared channel, and at 65 s per
flood crossing it is worse than waste. We keep airtime-first RTO and instead make the budget a sum, so the
numbers cannot disagree (A-388 says exactly this).

**Ledger entries answered:** A-41, A-80 (blocker), A-81, A-82 (the harness must use `hold`, not one
airtime), A-101, A-105, A-362, A-511, A-62, A-63, A-164/A-165, A-168, A-28.

**Unsure.** Whether `srtt` converges on a link where a third of packets are lost. The ARQ's own Karn-like
rule (only first-attempt samples) makes it conservative, which is the right direction. If it proves noisy,
the fallback formula is the floor and `srtt` only ever *raises* the RTO.

---

## 4. Handshake idempotency

**Decision.** A repeated `msg1` is a repeat, not a new session. Two mechanisms, one at each layer.

**(a) An idempotent responder cache in the session host.** Key: `(pairing_id, hash(msg1))`. On a hit
inside the timestamp window, re-send the cached `msg2` and return **without** touching `session`, `index`,
`outbox` or the handshake counter. That is the standard Noise responder answer to a replayed initiation,
and it makes three copies behave like one copy with two free retries of the answer, which is what they
were added for.

**(b) A receive-side hold and dedup, so the copies collapse before any of this.** Hold a flood-carried
record for `calc_rx_delay(len, snr)` (clamped 50 ms…32 s), keep a small content table keyed on
`(payload_type, payload)` — MeshCore's own dedup key — and process only the **best-heard** copy
(`dispatcher.py:634-655`). The reference's ordering, hold then dedup, is what makes "which copy won"
deterministic instead of lucky. With (a) and (b) together, three copies are one handshake delivered by
the best of three.

**What a replay may cost.** After both mechanisms, replaying a captured flood-carried `msg1` costs the
host one cache lookup and one 50 B telemetered reply, and destroys nothing. Today it destroys the live
session, clears the outbox, performs two `fsync`s and queues three flooded replies (A-240).

**The phone's half.** Two changes, both small:
- Only `record[0] == KIND_MSG2` may consume a pending handshake. A transport record from the previous
  session, a stranger's packet, or a surplus answer must not become a fatal handshake failure (A-21).
- On `Crypto`/`Malformed` from `finish`, do not abandon the attempt: re-send the *same* `msg1` (same
  timestamp ⇒ the host's cache hits) and keep the ladder where it is.

**The clock rule stays, with its tolerance named.** A ±5 minute window against the local clock is the real
anti-replay bound; strict monotonic ordering against the last accepted timestamp is not, because it turns
one refusal into a permanent lockout (the `2ea77bb` regression pair A-20/A-240, and A-241's observation
that the floor is unreachable anyway).

**Ledger entries answered:** A-20 (blocker), A-21 (blocker), A-240 (blocker), A-382 (blocker), A-22, A-23
(with §5), A-25, A-26, A-27, A-29, A-30, A-31, A-241, A-246.

---

## 5. Delivery: what happens when a record cannot be sent

**Decision.** Every accepted record reaches exactly one terminal outcome, and the caller learns it.

**One outbound queue with `not_before`.** Delays belong to packets, not to the receive path. A reply that
must be held 500 ms is queued with `not_before: now + 500 ms`; the loop drains due entries between radio
reads and **never sleeps on a queued packet's behalf**. That single structure closes A-61 (the host
sleeping on its own reader for up to 19 s), A-387 (the reference's queued teach with a settle window) and
A-181 (the reader losing delivery time to serial housekeeping), and it costs nothing in airtime.

**Backpressure is a protocol answer, not a growing queue.** The tailnet path already does this
(`WS_BACKLOG_BYTES` → resync) and the radio builds records until its window fills (A-503). The radio
answer: a credit record the receiver sends when its buffer is under pressure, and a sender that pauses on
credit rather than on record count.

**A refusal has a name and reaches the caller.** The tailnet has fifteen close codes and a refusal
vocabulary; the radio has six close reasons and no message (A-508). Every record that cannot be delivered
resolves to a typed reason (`NotDelivered::Congestion | RouteDead | TooLong | RadioRefused | SessionReplaced`),
which the bridge maps to a wire refusal the phone can log and show. Today `Input`/`Key`/`Resize` to an
unattached channel return silently (A-501), which is why the operator's first keystroke after a rebuild
vanishes with no message.

**Session replacement does not silently discard owed bytes.** On a handshake that replaces a live session,
the old ARQ's unacknowledged payloads come back as `Vec<(shell, Bytes)>` and are re-encoded onto the new
session. The reference has an eviction *policy* for exactly this reason (`message_queue.py:9-44`:
"direct messages are never displaced"); ours has none because the queue dies with the session (A-389,
A-23, A-142).

**Ledger entries answered:** A-501, A-503, A-504, A-508, A-509, A-104, A-109, A-23, A-61, A-89, A-142,
A-143, A-387, A-181, A-389.

---

## 6. Claims: a lease the phone renews

**Decision.** A claim is a **lease**, not a record of a connection, and it is the same rule for every
device.

```
claim_lease = max(2 × crossing, 15 s)     // derived from the route; 15 s is the tailnet floor
```

- The lease is renewed by **any record from that device**, and by one tiny `Alive` record per lease
  period when the operator is idle. That record is exempt from the airtime budget (the heartbeat already
  is, `mesh_session.rs:366`) and costs one small packet per 15 s at range, which is 1/20th of one screen.
- `engaged()` becomes "the lease is current". The three graces (`CLAIM_GRACE_MS`, `RECONNECT_GRACE_MS`,
  `RESTORE_GRACE_MS`) collapse into one derived figure, and `mesh_watched` disappears.
- The mesh device id becomes **per link** (`mesh-link:<peer key prefix>`), so two phones are two devices
  and the ordinary consent path works. Today every mesh client is `mesh-link` (`mesh_bridge.rs:2018`) and
  same-id re-claims are idempotent, so two phones share a shell silently and `HeldBy` is unreachable over
  the radio (A-541).
- A shell that exits while attached releases its claim (`Closed{EXITED}` currently removes the attach and
  keeps the claim, A-548).

Why this is the right shape: today a mesh claim is exempt from the release sweep *and* unconditionally
engaged, and the only thing that gives it back is the bridge's own `drop_all`, which needs
`link.established()` to go false, which needs the ARQ to exhaust retries, which needs something to be
outstanding, which an idle shell never has (A-540). So a quiet shell stays held for hours, at the phone's
geometry, refusing a desk window, refusing to be closed. A lease bounds it by construction.

**Ledger entries answered:** A-500 (blocker), A-540 (blocker), A-541, A-542, A-543, A-544, A-545, A-546,
A-548, A-524 (the missing `Detach` becomes harmless), A-205.

**Unsure.** Whether the idle `Alive` record is affordable on a shared mesh. It is one 5 B record per
15 s, which is 0.5 % of one record's air per second, so yes by arithmetic, but it is the only permanent
spend in the design and should be killed first if a repeater operator complains.

---

## 7. The shape of an attach: three exchanges, not four

**Decision.** The floor is **three** round trips, and the list is not one of them.

| # | exchange | carries | why it cannot be cheaper |
|---|---|---|---|
| 1 | handshake | keys | Noise IK, two messages, no way around it |
| 2 | `Attach` | selector | the claim and the grid choice |
| 3 | answer run | `Profile` + marks + first paint, in **one** run | the screen is the thing you asked for |

The list is fetched **on demand only** (the launcher), never on the way to a shell. Two consequences:

- **Resume is the default.** The phone remembers the last attached shell and re-attaches it directly, so
  a drop costs exchange 1 plus exchange 2 plus exchange 3, with no list. Today `resumeShell` is declared
  and never assigned, so every drop lands on the launcher and pays a fourth exchange (A-420, A-426).
- **A resumed run is a diff, not a screen.** Keeping a mirror per shell (A-320, A-327) means exchange 3 is
  one compressed diff, 1 record instead of 3.

**One record for the answer where possible, and a page index always.** The list protocol has only a
`last` flag, so a lost middle page silently omits shells and nothing can detect it (A-324). Every run
carries its page index and total; the receiver assembles and says which pages are missing. That is what
`Marks` already does (`first`/`last`), so the mechanism exists.

**Paging costs are bounded by the run's record count, and every wait is derived from it:**
`wait = pages_remaining × rto + one_answer_window`. A-148's "nothing tells the client how much an answer
costs" is answered by the run's header carrying its record count.

**Ledger entries answered:** A-102 (blocker), A-108, A-101, A-105, A-106, A-420 (blocker), A-421, A-426,
A-100, A-323, A-324, A-320, A-327, A-322, A-329, A-330, A-511, A-148, A-107.

---

## 8. What the phone shows

**Decision.** The screen has one job at range: never leave the operator unable to tell "working" from
"dead", and never invite an action that costs a ladder.

Four rules, each answering a recorded failure:

1. **Local echo, always.** Typed characters appear immediately, faint, until the host's echo replaces
   them. Today the echo machinery exists (`MeshTerminalSource.kt:544-551`) and is wired only to held
   mode, so in live mode the IME is a 1 dp transparent field and nothing appears for 13 s at range
   (A-160, A-161).
2. **A live line, always.** When a wait is long, the line keeps moving (elapsed, attempt n of m, records
   remaining) instead of being replaced by a static verdict. The verdict may *join* it, never replace it
   (A-280: the verdict covers 83–85 % of the attempt).
3. **A verdict only with an action, and the action must not be the expensive one.** "No answer yet, still
   trying, attempt 2 of 3, 41 s" is honest. "Check the channel, then refresh" is advice that restarts the
   ladder from rung one (A-284), and refresh is the most expensive thing available.
4. **The transport is named on every screen and in every log line.** A-423 records that nothing on either
   end says which transport carried a step, which is why a 6 s attach at 17 hops was believed to be the
   radio when it was the tailnet.

**Never show:** a countdown that exceeds the budget that governs it (A-511), "direct" for a zero-hop flood
(A-282), a good-news colour for a stopped link (A-281), or a state with no exit (A-104).

**Ledger entries answered:** A-280, A-281, A-282, A-283, A-284, A-285, A-286, A-287, A-160 (blocker),
A-161, A-162, A-163, A-164, A-166, A-167, A-168, A-170, A-502, A-423, A-205.

---

## 9. What is measured and logged

**Decision.** The link's own measurements are first-class outputs, and each transmit states its decision.

**Surfaced measurements.**

| value | source today | where it goes |
|---|---|---|
| `srtt` (smoothed round trip) | `arq.rs:670-683`, `:475` | status JSON, `PATH DIAG` line, phone status |
| link margin | `snr − demod_limit[sf]`, from `peer_signal` | route ranking, status line as dB of margin |
| SNR distribution | one variable today (A-462) | min/median/count-below-floor per session |
| transmissions per record | not counted (A-467) | per-record attempts histogram |
| airtime, measured | `tx_air_secs` published and never differenced (A-464) | per-session delta, compared with the estimate |

**One line per transmit, at the point of decision** (A-390's shape, A-220's need):

```
record <id> <shell> <n> B attempt <i>/<m> carrier <direct|flood> path <h> hops
  rto <ms> srtt <ms> margin <db> snr <db> rssi <dbm> copies <k>
```

Rate-limited **per attempt**, never per burst, so three copies of one attempt are three lines and a reader
can see them (A-466's defect is that rate-limited logging cannot be counted, applied to the events a
measurement needs).

**Both ends carry the session id on the wire**, so the host's and the phone's logs can be joined (A-231:
the wire already carries an identifier and neither log prints it).

**Every refusal and every death is printed where it happens.** A-220's session death sets
`session = None` before logging and is therefore emitted nowhere; A-221's refused handshake writes
nothing. One line each, at the point of the event, with the reason.

**Ledger entries answered:** A-180 (blocker, with §5), A-220 (blocker), A-221, A-222, A-223, A-224, A-225,
A-226, A-227, A-228, A-229, A-230, A-231, A-232, A-233, A-234, A-460, A-461, A-462, A-463, A-464, A-465,
A-466, A-467, A-468, A-390.

**Unsure.** Whether the volume is acceptable: 18 records per attach is ~36 lines at range, which is fine
on journald but noisy in the phone's 128 KiB durable log, which holds about an hour today (A-521). The
answer is a second, smaller ring for the per-transmit lines and keeping the durable log for decisions.

---

## 10. What is deliberately not built

- **No radio-port lease machinery.** The kernel already arbitrates the port (`flock` exclusive,
  `link.rs:158`), and the collision that motivated a lease was a development inconvenience between two
  bench tools, not a product requirement. Recorded as a decision in the ledger. What *is* fixed: never
  leave a partial frame in the firmware's parser (A-400) and never stop the daemon mid-write (A-482).
- **No `GRP_DATA` fallback.** It would add a second crypto layer under ours to buy floodability we already
  have via `REQ` floods (A-345's reasoning).
- **No rewrite onto MeshCore's own message layer.** It would delete six blocker classes, but it needs a
  companion ABI, a contact identity and a message-oriented terminal, and the audit's own conclusion is
  that the shape is not what fails (A-342, A-340). Keep it as the documented alternative.
- **No 12 s attempt cap, no immediate re-send** (A-388's explicit "do not adopt").
- **No compression-first as the primary fix.** Compression is a multiplier that fixes none of the
  blockers (A-346). It is adopted where it is free: the uplink frame (§11, A-140) and the diff (§7).
- **No single-record stateless mode as a replacement.** It is a *mode* below one crossing, and the design
  keeps the door open by making the exchange count a property of the run, not of the protocol (A-345).

---

## 11. Two additions that are not blockers but are cheap

**Uplink framing (A-140).** The downlink is compressed, the uplink has no frame at all: a 1,640-byte paste
is 12 records plain and 2 deflated. Add `FrameUp`, the twin of `Frame`, gated by a `features` bit as every
other wire change is. One message, one compressor call, 6× fewer records on the largest item of user data.

**Scope as a resolved chain (A-385, A-60).** Resolve the flood scope **per packet** from three sources in
priority order (per-send override, the radio's persisted default, `LDMUX_MESH_SCOPE`), and mark the packet
so a later layer cannot silently re-decide. A-60 is a conditional portability risk rather than tonight's
cause, but a regionalised mesh cannot be served at all today, and the fix is a resolution order rather
than a feature.

---

## Migration order

Each step is independently shippable, and each is justified by the measurement before it. The order is
chosen so that nothing is tuned before it can be measured, and no fix is built on a number that is wrong.

| step | change | makes moot |
|---|---|---|
| **M0** | §9: `srtt` in status and logs, `PATH DIAG` per attempt, margin, transmissions per record, session id on both logs | A-460, A-462, A-464, A-465, A-466, A-467, A-180, A-220, A-221, A-224, A-231 |
| **M1** | §4: idempotent `msg1` cache, receive hold+dedup, phone `KIND_MSG2` gate | A-20, A-21, A-240, A-382, A-22, A-241 |
| **M2** | §1 + §2: `Route`/`Attempt`/`carrier_for`, proof on direct arrival only, teach-not-mirror, margin ranking | A-120, A-340, A-360, A-370, A-380, A-381, A-361, A-365, A-383, A-392, A-125, A-126 |
| **M3** | one copy per data record, airtime-metered pacing that waits instead of dropping, listen gap between copies | A-102, A-122, A-123, A-183, A-386, A-88, A-90 |
| **M4** | §3: `crossing()` and `rto()` from `srtt`, budgets as sums, idle limit ≥ one crossing | A-80, A-81, A-101, A-105, A-362, A-511, A-28, A-62, A-63, A-85, A-164, A-165 |
| **M5** | §5: outbound queue with `not_before`, delivery before housekeeping | A-61, A-181, A-187, A-188, A-190, A-387, A-185 |
| **M6** | §5: typed refusals to the caller, credit-based backpressure, replacement carries owed bytes | A-501, A-503, A-504, A-508, A-509, A-104, A-109, A-23, A-389, A-142 |
| **M7** | §6: claim lease, per-link mesh device id, release on `EXITED` | A-500, A-540, A-541, A-542, A-543, A-544, A-545, A-546, A-548 |
| **M8** | §7: resume by default, three exchanges, page index, per-shell mirror | A-420, A-421, A-426, A-100, A-323, A-324, A-320, A-327, A-107, A-148 |
| **M9** | §8: live line, local echo, transport named, no false verdicts | A-280…A-287, A-160…A-170, A-502, A-423 |
| **M10** | §11: `FrameUp`, per-packet scope | A-140, A-385, A-60 |

**M0 first is not negotiable.** Every subsequent step's correctness is judged by a number that does not
exist today, and the audit spent a day pricing its findings by hand because of it.

**Test seams come with M0 and M2.** The ledger's A-260 is that five of six blockers were untestable as
written: the session loop is private (`mesh.rs:685`), the phone's carrier policy needs an Android
`Context`, and the fake link has no clock (`link.rs:225`). So: a `ScriptedLink` with a virtual clock and
injectable delays and refusals, a `#[cfg(test)]` entry point for `supervise`, and `carrier_for` as a pure
function in `commonMain` beside `routeRefusal`. With those three, M1–M4 get tests that would have caught
the blockers.

---

## What this design does not fix, and cannot

The journey round's finding stands: **after every item above, two things remain true**. A keystroke costs
a round trip (14 s direct, 92 s flooded), and a shell open costs three exchanges (42 s direct). No rule in
this document changes either, because they are the medium.

What the design does is make those numbers *honest and visible*: the operator sees "attempt 2 of 3, 41 s"
rather than a dead screen, the echo appears locally, and the wait is one crossing rather than four
overlapping retries. If character-at-a-time interaction is unacceptable at range, the answer is not in this
protocol at all: it is a mode where the phone composes a line locally and sends it whole (A-426, A-345),
which is a product decision, not a design one.

## The riskiest assumption

That **`srtt` is a good enough estimate of the next crossing** on a link at 0 dB of margin. Every timer in
§3 rests on it, and if the round trip varies by a factor of three sample to sample, a smoothed average is
the wrong statistic and the design needs a *maximum over a window* instead. The ARQ already computes both
candidate quantities (it has `rto = max(rto_initial, srtt)` and a `fast_retx_floor`), so the fix is small,
but which is right is a measurement, not an argument: M0 must log the distribution before M4 uses it.

The second risk is that a per-attempt carrier decision doubles the *number* of transmissions (a lost direct
record costs a direct attempt plus a flooded retry) on a mesh where transmissions are the scarce resource.
That is a deliberate trade: 7.8 s of direct plus 65.6 s of flood on a lost record, against 65.6 s on
*every* record while the latch holds. The measurement at M0 settles whether the direct first attempt
succeeds often enough to be worth it, and the number to watch is attempts-per-record, which is why it is
in M0.
