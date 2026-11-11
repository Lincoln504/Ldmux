# The radio path: what is wrong, what to keep, and the order to fix it

This is the plan of record for the public-mesh (`LoRa`) route. It replaces
`docs/RADIO-DESIGN-PROPOSAL.md` as the document that decides work. That proposal's carrier and
timing model were built on two assumptions the firmware does not support (§1.9), and three of its
decisions would introduce new failures: auto-replaying unacknowledged input across a new session
(double execution), a 15-second idle heartbeat (continuous airtime for a mode whose point is that
it needs no connection), and marking an outbound path proven by an inbound flood (§3.2, §3.3).

`docs/RADIO-AUDIT-LOOP.md` stays as the evidence record: 300-odd findings, some wrong, corrected
in place. This document cites only what was verified in source or in the field logs.

Three things are true and shape everything below.

1. **The requirement is a useful remote terminal over a shared public mesh.** "Batch" is a means,
   not a requirement: it exists because a continuous interactive stream is the wrong thing to put
   on a channel other people share. Anything that serves the requirement better than batch does
   should replace it.
2. **The link is intermittent by nature.** A 14–28 hop crossing over repeaters that may be
   holding, dropping, rate-limiting or re-routing is not a connection. Designs that treat
   "session established" as a precondition for the user doing anything have failed on every field
   trip, and will keep failing.
3. **One crossing is expensive.** A record is 490–603 ms of air at the public preset and it is
   repeated at every hop it crosses. A design's cost is measured in crossings, not in messages.

---

## 1. What is actually wrong

Ordered by what it costs the user, with the evidence that establishes it. Everything here was
re-read in source; ledger ids are given so the detail is findable, not as authority.

### 1.1 Data the link "sent" can be silently undeliverable — **blocker**

The radio's receive path and our send path enforce different ceilings.

- App → radio: `[0x41][priority][packet]` inside a 176-byte frame, so **packet ≤ 174**
  (`examples/companion_radio/MyMesh.cpp:2047-2060`, `src/helpers/BaseSerialInterface.h:5`).
- Radio → app: a received packet is pushed only if `len + 3 <= 176`, so **packet ≤ 173**;
  otherwise nothing is emitted at all (`MyMesh.cpp:291-299`, `logRxRaw`).
- A flooded record is `1 header + 1 path byte + hops × hash + 2 hashes + 4 magic + 4 nonce +
  record`, so at the far end `12 + hops × hash + record ≤ 173`.
- Our published flood limit is a flat **162** on both ends (`mesh_serial.rs:461-465`,
  `MeshRecordLimits.kt:34-43`), justified in comments from the *send* budget only.

Consequences, arithmetically exact:

| record | received while |
|---|---|
| 162 B | **never** (174 + 3 > 176 even at zero hops) |
| 143 B | hops ≤ 18 |
| 133 B | hops ≤ 28 |
| 120 B | hops ≤ 41 |

The field log's `record by flood … 133 B` beside a 28-hop session sits exactly one hop from that
cliff. Every larger record leaves the radio, is counted as sent, and is never reported by the
peer's companion. **No amount of retry, timing or carrier tuning fixes a packet the receiver
cannot push.**

### 1.2 A shrinking record limit duplicates and reorders delivered bytes — **blocker**

When the route grows, `Session::poll_with_seq` splits an already-numbered in-flight payload: the
head keeps the sequence number, the tail is pushed to the front of the queue and numbered later
(`session.rs:246-276`, `arq.rs:746-766`). The peer has already received the whole original record
in the ack-lost case, so the tail is delivered **twice**; and because `arq.poll` numbered later
records in the same call, the tail arrives **after** bytes that follow it in the stream.

This is reachable in exactly the situation the mechanism was written for — a route that grows one
repeater — and it corrupts a paste, a command line or a screen patch rather than merely slowing
it. `requeue_remainder`'s stated contract ("same order, same bytes, one extra record") is false.

### 1.3 Three copies of one handshake made three sessions — **blocker, fixed in the working tree**

The phone sends `msg1` as three floods; the host had no content dedup, so after the timestamp
guard was relaxed each copy built its own Noise responder, its own session index and its own
reply — up to nine flooded answers. The phone finished on whichever arrived first, the host kept
whichever arrived last, and when they disagreed every phone record failed its tag while the
journal read `handshakes 3` (looks healthy).

Fixed by an idempotent responder cache in `mesh_session.rs` (working tree, unvalidated):
identical `msg1` → identical cached `msg2`, no new session, no index advance, no outbox clear; a
replay of a *superseded* initiation is refused rather than replacing the newer session. It also
makes a captured `msg1` unable to destroy a live session, which the `2ea77bb` tolerance had
allowed.

### 1.4 The phone finishes a handshake with any record at all — **blocker**

`MeshTermDriver.onRecord` takes the first record of any kind when a handshake is pending, sets
`handshake = null`, and calls `finish` on it (`MeshTermDriver.kt:906-916`). A leftover transport
record, a stranger's packet or a surplus answer therefore destroys the attempt and spends a rung.
Crossings are seconds long, so late answers are normal, not exceptional.

### 1.5 The carrier was chosen per record from a latch, and proof came from the wrong direction — **blocker**

Both ends keep "we heard a flood, so send floods" flags (`peer_via_flood` in `mesh_serial.rs`,
`directWorks` in `AndroidRadioLink.kt`), cleared only by a direct arrival. One flood on each side
latches both sides into flood forever.

Separately, `peer_path_proven` is set by a **flooded** record (`mesh_serial.rs:2204`) — that
chain is an inbound observation, and the stored path is its reversal, a guess about the outbound
direction — while the direct-arrival branch sets nothing. The timer then takes its hop count from
the shortest inbound path, which the journal shows flipping 1↔16 hops in eleven seconds.

### 1.6 The host stops reading the radio while it answers — **blocker**

Answering a route request sleeps the reader for up to ~17 s (`mesh_serial.rs` reply path with a
`reply_delay` sleep), so handshakes arriving in that window are never read. MeshCore's own clients
enqueue delayed sends and keep receiving. One outbound queue with `not_before` removes the class.

### 1.7 A press is not a transaction — **blocker for the mode**

Verified end to end:

- `sync()` sets `urgent = true` and opens a gate; the pump drains the **live** buffer, so text
  typed after the press leaves with it (`MeshTerminalSource.kt:266`, `MeshTerminalScreen.kt:537-560`).
- `batcher.accepted()` removes a piece when the ARQ **queues** it, not when the peer has it; a
  session death then drops it with nothing said (`MeshInput.kt:300-320`,
  `MeshTermDriver.kt:765-790`).
- `Input` for a shell no longer attached returns silently — no reply, no counter
  (`mesh_bridge.rs:1816-1820`). The phone's `Live` view survives that.
- A paste split across records is applied piece by piece, each newline running on arrival; a loss
  mid-paste leaves half a command in the pane.
- One draft buffer and one mirror per **connection**, not per shell
  (`MeshTerminalScreen.kt:478-482`), so a shell switch or a rebuild takes the draft with it.
- `resumeShell` is declared "the value that must survive a rebuild" and is only ever assigned `""`
  (`MeshTerminalScreen.kt:256, 342, 914, 974`), which makes the resume path and the held-screen
  pause unreachable and lands every rebuild on the launcher.

The wire has no batch identity and no execution answer, so none of this is checkable by any local
test. That is why it shipped green.

### 1.8 Claims outlive the phone; the UI has no bounded truth

A mesh claim is exempt from the release sweep and `engaged()` makes it indisplaceable; the only
thing that releases it needs the session to die, and an idle shell has nothing outstanding. A quiet
shell can stay held for hours at the phone's geometry, refusing a desk window. Every mesh client is
also the same device id (`"mesh-link"`), so two phones silently share one shell.

The screen shows a verdict at 30 s while the ladder continues for 176–372 s, and deletes its only
moving evidence at that moment; the session death reason is computed and then discarded
(`mesh_session.rs` sets `session = None` before any log).

### 1.9 Every flooded record is transmitted three times — **major, airtime**

`RECORD_FLOOD_COPIES = 3` on both ends (`mesh_serial.rs:513,1468`,
`AndroidRadioLink.kt:2032`), and route requests are `REQUEST_COPIES = 3` as well. A flooded record
is therefore 3× the air per repeater chain — for the handshake (where it is the right trade) **and
for every data record on the flood carrier** (where it is not: a retry is cheaper than a third
copy, and a lost screen is refetched as a snapshot, not repeated). On a shared mesh this is the
largest avoidable airtime multiplier in the system, and it is invisible in the UI.

### 1.10 The measurements we designed against were not measurements

- `calcRxDelay` is **0** on stock repeater, companion and room-server builds (`rx_delay_base`
  default 0.0); what actually delays a flood at each repeater is a randomised retransmit delay
  `nextInt(0, 5 × airtime × tx_delay_factor)`. The proposal's universal 2.70 s/hop hold does not
  exist, so neither does its 65.6 s flood crossing or its 6.7× flood penalty.
- `packetScore` is `clamp((snr − threshold)/10) × (1 − len/256)`, labelled "Approximate", used
  **only** to schedule receive processing. It is not a delivery probability, so "0 dB margin ⇒ a
  coin toss" is an inference, not a fact.
- The ARQ computes a true smoothed round trip on every ack and exposes it only to tests
  (`arq.rs:670-683`, `:475`), so every timer was tuned without the one number that settles it.

---

## 2. What to keep

Real, hard-won, and not to be lost in a rewrite:

1. **The renderer split.** libghostty-vt per shell on the host, a row mirror on the phone, diffs on
   the wire. A repaint is 2–3 records and an echo is 1 because of it.
2. **Frame compression with the pinned dictionary** — 1318 B screen → 3 records. Extend it
   uplink; do not replace it.
3. **Row diffing with a known base** and the absolute top line: already snapshot-shaped.
4. **AEAD + pairing**: pinned static keys, credential files in the persist dir, the record layout
   and its replay window.
5. **Local echo, the draft batcher's windows, and scrubbing secrets on background.**
6. **The tailnet route, the desk attach client, and takeover consent.** Only liveness-based
   expiry is wrong.
7. **Airtime accounting and refusal bookkeeping.** The app does not spam; keep that property.
8. **The durable phone log and the host journal vocabulary** — the only reason any of this was
   diagnosable.
9. **The launcher, `new shell` as its first row, history fetch, grid/fit, and `Detach`/`restore_grid`.**
10. **The honest constants**: 143-byte record payload, 137-byte input, credit window 8.

---

## 3. The design

Four layers, each with one job and a rule that makes the failure classes above unrepresentable.

### 3.1 Device (L0)

- One owner, enforced by the advisory lock we already take; ldmux keeps the radio.
- `ERR_CODE_UNSUPPORTED_CMD` from a send is **terminal**: this radio cannot carry ldmux. Say so once
  and stop retrying it as "busy".
- Re-synchronise the frame parser on a bad length instead of eating ~15 kB of following frames.
- Re-assert the radio clock on every open (advert ages and route questions depend on it).
- Handshake-bearing command timeouts are derived from the firmware's own budgets
  (`500 + 16 × airtime` flood; `500 + (6 × airtime + 250) × (hops + 1)` direct).

### 3.2 Carrier (L1)

- **The carrier is chosen per attempt, never per record.** Attempt 1 with a held path builds a
  direct `RAW_CUSTOM` packet; attempt *n* builds a flood. Nothing persists between attempts, so no
  latch exists to fix.
- **The record budget is the receiver's**: `min(161 − hops_max × hash − scoped×4, 171 − path)` for a
  flood, `172 − path` (capped 171) for a direct packet, floored at the session's 108-byte minimum.
  `hops_max` is the largest hop count observed from this peer, not the shortest inbound path.
  Before any hop count is known, publish **120**.
- **One copy of a data record per attempt**, and one copy of a route request. Three copies only for
the 121-byte `msg1`, where a lost request costs a whole epoch; the copy count becomes a measured
setting, not a constant applied to every flood.
- **A flood arrival means "the peer cannot reach us".** Answer with a direct path teach (one small
  packet, throttled per peer), never by mirroring the flood.
- **Scoped when the radio has a default scope**, and the 4-byte cost is part of the budget.
- **No unsolicited output on the public mesh**: a held shell is not read, and pushed screens are
  opt-in per open.
- **Native binary datagrams (`CMD_SEND_CHANNEL_DATA`)** stay unimplemented until a measurement says
  the corrected budget is insufficient. They would buy 165 B independent of hops, but cost a
  persistent channel slot on both radios, share a FIFO with chat that evicts channel frames first,
  carry a 2-byte MAC and no sender identity, and offer no acknowledgement at all.

### 3.3 Identity and session (L2)

- **Keep Noise IKpsk2 and the sealed record.** The alternative (a long-lived symmetric key derived
  from the PSK) trades per-session forward secrecy for one less round trip and is not worth it.
- **Drop liveness as a precondition.** A session/handshake is an *epoch*: it is established once per
  app start (or rekey), and it is not what a press waits on. A press before the epoch is up is held
  locally and sent when it exists; the UI says "connecting", not "failed".
- **`msg1` is idempotent** (done, §1.3), and the phone finishes a handshake **only** on a `msg2`
  (`KIND_MSG2`), treating anything else as stale.
- **The answer is retried once on its own timer**, then the epoch is re-attempted deliberately —
  no blind ladder per rung.
- **No auto-replay of unacknowledged user input across epochs.** After a replacement, the phone
  keeps the un-applied part of a submit in the draft and says so; it never re-executes silently.

### 3.4 Delivery (L3)

Five messages replace an ordered stream in both directions. Every one is independently retryable
and every retry is safe.

**Phone → host**

- `Submit { id: u16, shell: u8, part: u8, last: bool, body }` — text and/or key names. The host
  assembles by `id` and applies **only on `last`**, so no partial paste ever reaches the pty.
- `Fetch { shell: u8, want: Full | Diff { base: u32 } | History { before: u32, rows: u8 } | List }`
- `Release { shell: u8 }` — the explicit "I am done" that replaces liveness-based claim expiry.

**Host → phone**

- `Snapshot { id: u32, base: Option<u32>, parts: u8, part: u8, body, producing: bool }` — a row set
  or a compressed frame. Applying it twice is the same screen; an incomplete set is discarded; a
  `base` that does not match is refused and answered with `Full` next time. `producing` is the
  honest "the shell is still working", derived from the host's own observed activity.
- `Applied { id: u16, outcome: Ok | Refused(reason) }` — one small message, and it rides on the
  snapshot the submit produced when there is one. **That is the one-press win**: the answer to a
  press *is* the screen.

Semantics, stated honestly:

- **Idempotent delivery, at-most-once execution.** A repeated `Submit` id returns the cached
  `Applied` without touching the pty, in the same daemon process. After a daemon restart the ring
  is gone, so the phone is told `Unknown` and asked to resend rather than being told nothing.
  A crash between the pty write and the ring entry can duplicate one batch; that window is one
  instruction wide and is disclosed, not hidden.
- **A snapshot supersedes output; it never replays.** Losing a screen costs one `Fetch`, never a
  duplicate or a reorder.
- **Ordering is per submit**, not per byte stream: a submit is applied whole, in order, or not at
  all.
- **Backpressure is a reply**, not a silent drop: a `Fetch` the host cannot serve in the budget, a
  `Submit` it cannot queue, and an `Input` for a shell that is gone all answer.

### 3.5 Terminal protocol and UI (L4)

- Keep `Rows`/`Commit`/`Frame`/`FrameDict`, `Profile`, `Sessions` (with page indices), `Marks`,
  `HistoryPage`, `Resize`, `Create`, `Close`, `Takeover`, `MakeLive`, `Created`, `Detach`.
- Add the uplink twin of a frame: a large paste is compressed before it leaves (`MAX_INPUT` stays
  one record per piece, but the pieces are deflated).
- **Per-shell state objects**: draft, mirror, snapshot id, history cache and scroll anchor live
  with the shell, not with the connection. `resumeShell` becomes real.
- **Three truthful states**, never collapsed: *sent* (the radio accepted the record), *applied*
  (`Applied{Ok}`), *carried* (a snapshot came back). "Waiting for the answer" becomes "still
  producing" while the shell works, and always clears.
- **Live mode is an adjacent-mesh affordance** and says so; batch is the range mode. The mode
  choice stays in settings beside the channel.
- **Cost is visible before the press**: records, and whether the route is direct or flood.

---

## 4. Stages

Each stage is shippable, deletes something, and is judged by its own test. Ordering is forced where
marked.

### S0 · Instrument and make the seam testable
- **S0.1** Expose `srtt`, hops, carrier-per-send, attempts, snapshot parts and per-record outcome
  counts in `ldmux mesh status --json` and in one journal line per exchange.
- **S0.2** Publish the corrected flood budget (§3.2) on both ends, floored at 120, and log when a
  record is refused for size instead of falling back silently.
- **S0.3** Count and name every silent drop: unattached `Input`, queue overflow, session death
  reason (log at the point of death), rate-limited lines carry their suppressed count.
- **S0.4** Test seams: a `ScriptedLink` with a virtual clock and injectable delays/refusals in
  `crates/companion`; a `#[cfg(test)]` entry point for the mesh supervisor; the phone's carrier
  policy moved out of `AndroidRadioLink` into a pure function.
- **Deletes:** nothing. **Why first:** every later stage is judged by numbers that do not exist yet.

### S1 · Delivery correctness in the existing stream
- **S1.1** Split records **before** sequence assignment inside `arq.poll`; an in-flight slot that no
  longer fits fails the session with a named `RetransmitTooLong` instead of being split. Remove
  `requeue_remainder` as a general API. *(fixes §1.2)*
- **S1.2** Clear the refusal window only on evidence of acceptance, so `REFUSAL_DEADLINE_MS` is
  reachable.
- **S1.3** Never clear the phone's whole outgoing queue on one refusal; scrub only on a rebuild, and
  say why.
- **S1.4** Do not drop the oldest from an application queue: refuse new work, so no hole is created
  in a byte stream. Do not advance the ack for bytes the application has not accepted.
- **S1.5** Tests: duplicate/reorder/amplifier cases for the split, refusal deadline, queue-flush,
  ack-vs-deliver.
- **Deletes:** the false "same order, same bytes" contract.

### S2 · Handshake
- **S2.1** Land the idempotent responder cache (working tree) with its three tests. *(§1.3)*
- **S2.2** Phone finishes a handshake only on `KIND_MSG2`; anything else is stale and does not
  spend a rung. *(§1.4)*
- **S2.3** Retry the answer once on its own derived timer; replace the blind ladder with one
  deliberate re-attempt and a clear "connecting" state.
- **S2.4** Keep the replay tolerance, and count a superseded-initiation refusal separately from an
  attack so the journal is readable.
- **Deletes:** the ladder that gates the mode, and the permanent-lockout shape.

### S3 · Transactions (Submit / Applied)
- **S3.1** `Submit{id, part, last}` with host-side assembly and atomic apply. *(§1.7)*
- **S3.2** `Applied{id, outcome}`, cached in a small per-device ring; `Unknown` after a restart.
- **S3.3** Phone keeps the submit's bytes until `Applied`, then removes them; the draft is an
  immutable prefix taken at the press, and later typing waits.
- **S3.4** Every refusal answers (unattached shell, displaced shell, queue full, budget).
- **S3.5** Uplink compression for a large submit, gated by a feature bit.
- **Deletes:** "queued = sent", silent drops, and the partial-paste application.

### S4 · Snapshots
- **S4.1** `Fetch`/`Snapshot{id, base, parts, producing}`; paint only complete sets.
- **S4.2** Remove the idle marker and the 4 s quiet timer as the answer boundary; keep the marker as
  a hint. *(R8, P2)*
- **S4.3** Page indices for runs (list, marks, history) so a lost middle page is detectable.
- **S4.4** Held shells cost nothing until a `Fetch`.
- **Deletes:** the "press show again" class and the freeze-as-listener contract.

### S5 · Carrier
- **S5.1** Per-attempt carrier; delete `peer_via_flood` and `directWorks` as persistent state.
- **S5.2** Proof: only a direct record or a direct ack marks a path proven; a flood chain teaches a
  teach, never a timer.
- **S5.3** Time by `srtt`, floored by the firmware's own formulas; never by the shortest inbound
  path, never by `calcRxDelay`.
- **S5.4** Queue outbound sends with `not_before`; the reader never sleeps. *(§1.6)*
- **S5.5** One data copy per attempt, three only for `msg1`; scope when the radio has one.
- **Deletes:** the latch, the 162 cliff, and the blocked reader.

### S6 · Claims and per-shell state
- **S6.1** Claims released by `Release`, `Detach`, shell exit, or consented takeover. No liveness
  expiry, no heartbeat.
- **S6.2** Per-link device id, so two phones are two devices and `HeldBy` works over radio.
- **S6.3** Per-shell draft/mirror/snapshot-id/history/scroll state; `resumeShell` real; a drop
  costs a pause, not the user's place.
- **Deletes:** hours-long quiet claims and the launcher bounce.

### S7 · Lifecycle and device
- **S7.1** Wake-lock lifetime follows the service, not the notification text (patch ready in a
  worktree; review, then land).
- **S7.2** Switching route must not tear down a live radio epoch silently; if it must, say so and
  do not restart the ladder from rung one.
- **S7.3** Off-screen release sends `Detach`.
- **S7.4** Frame-parser resync; clock restored on open; `UNSUPPORTED_CMD` terminal.
- **Deletes:** the silent 30–55 minute dead zone, and the "wedged radio" class that was really a
  truncated frame.

### S8 · UX
- **S8.1** One press: `Submit` + implicit `Fetch`; the result is the screen. No second press ever.
- **S8.2** Local echo everywhere the draft is visible; held text never leaves the process.
- **S8.3** Honest vocabulary: sent / applied / carried / producing / not delivered after N tries.
- **S8.4** Cost before the press (records, carrier), state on screen, transport named in the log.
- **S8.5** Scrolling: up/down fetch a page, typing returns to the bottom, buttons only, controls
  grey out rather than disappear, batch controls absent in live mode.
- **Deletes:** "connection dropped" in a mode that has no connection.

### S9 · Retire and document
- **S9.1** Remove the ARQ stream machinery that S3–S5 replaced (ordering, window, cumulative ack),
  keeping parts+retry for bulk only.
- **S9.2** Update `ARCHITECTURE.md`, `AIRTIME.md`, `RELEASE-AUDIT.md §18`, the ADRs (new: carrier
  per attempt; identified transactions; snapshots), and this plan — in the same commits.
- **S9.3** Delete the superseded proposal document or mark it historical.

**Forced order:** S0 before S5 (or carrier work is unmeasurable); S1 before S3 (transaction retries
must not ride a duplicating stream); S2 before S3 (a press must not need a handshake); S3 before S4
(snapshots need ids); S4 before S6/S8 (UI must stop inferring).

---

## 4a. Integration surfaces (what else these stages touch)

- **Protocol crate** (`mesh_term.rs`): `Submit`, `Applied`, `Fetch`, `Snapshot`, `Release`; page
  indices on `Sessions`, `Marks`, `HistoryPage`; the uplink frame. One version bump, and an old
  phone is refused with `Closed { VERSION }` as today. Pre-release, both ends deploy together, so
  no feature-bit scaffolding is kept beyond what already exists.
- **Keeper**: unchanged. The bridge buffers a `Submit` in memory and calls `host.input(session,
  bytes)` once on `last`, so a batch is one write to the pty and the pty's own line discipline
  sees whole lines.
- **Daemon ownership**: `Release` maps to the claim table; a `Submit` for a shell handle that no
  longer exists answers `Applied { Refused(NotAttached) }` and the phone keeps the draft. The
  shell handle is per attach, and a submit in flight across a re-attach is refused rather than
  applied to whatever now holds that number.
- **Two routes, one phone**: the tailnet client is untouched. The mesh client is the only one that
  gains `Submit`/`Fetch`; the shared parts are the terminal codec, the mirror and the UI, which is
  where the user-visible behaviour must match.
- **Claims**: the daemon's `mesh_watched` set disappears; claims come from `Release`, `Detach`, shell
  exit and consented takeover.
- **Docs to update in the same commits**: `ARCHITECTURE.md` §2, §5–§9, §11; `AIRTIME.md` (the
  3-copy multiplier and the corrected budget); `RELEASE-AUDIT.md` §18; new ADRs for carrier per
  attempt, identified transactions and snapshots; and the ADRs that describe the session as a live
  connection (0029, 0032, 0037, 0038) marked superseded where the plan changes them.

---

## 4b. How each stage is proven without a field trip

Existing seams first, new ones only where a defect has no home:

- **`crates/session/tests/lossy_multihop.rs`** already drives the session over a lossy, multi-hop
  model. Extend it to the transaction layer: loss, duplication, reordering, a route that shrinks or
  grows mid-flight, and a replayed `msg1`. This is where S1/S2/S3's correctness lives.
- **`ScriptedLink` with a virtual clock** in `crates/companion` (S0.4) for the carrier: delayed
  sends, refusals, a radio that stops answering, a partial frame, a burst of frames.
- **Pure carrier function** on the phone (S0.4) so `directWorks`/`floodWanted` become a table test
  rather than a BLE experiment.
- **Bench with two radios at minimum power** (both on the host's desk) answers what only hardware
  can: does `0x41` exist on this firmware, what is the real per-record airtime, does a native
  group datagram arrive, and does the wakelock survive an hour. That is one afternoon, not a range
  trip.
- **A field trip is only for reach**: how far the mesh carries, how many hops, and which carrier
  wins at that distance. The instrumented log (S0) turns it into a table instead of an anecdote.

---

## 4c. Compatibility and rollback

- Every stage keeps the host and phone speakable at the same commit; a mismatch is refused with a
  version reason, never a half-working link.
- The radio path can always be turned off by removing `LDMUX_MESH_SERIAL`, which leaves the tailnet
  route and the desk client untouched. That is the rollback for any stage that misbehaves in the
  field.
- The keeper and its shells are never part of a radio change: a radio failure cannot cost a shell.

---

## 5. Decisions taken, and the questions that are not mine to settle

Taken, on the evidence above:

- Keep Noise; drop liveness as a gate (epoch, not connection).
- Keep `RAW_CUSTOM` direct + `REQ` flood with the corrected budget; **do not** build GRP_DATA until a
  measurement says so.
- Identified submits and idempotent snapshots; **no auto-replay of user input across epochs**.
- At-most-once execution per daemon lifetime, with `Unknown` after a restart rather than a silent
  retry.
- Claims end by action, never by heartbeat.
- Live mode is declared an adjacent-mesh affordance.

Open, and it needs a decision before S8:

- **The airtime policy for one press.** The user's own sentences are "latency above airtime" and
  "airtime minimised at all costs", which conflict whenever a second copy would cut latency. The
  proposal that satisfies both: a **hard cap per press** (records, shown before the press) with
  latency optimised inside it. That needs a number: 8 records per press, 12, or "whatever the
  screen needs, capped at one full frame".

---

## 6. What would make this plan wrong

- If `hops_max` on the real route is small (≤6), the flood budget fix matters less and GRP_DATA is
  not worth its cost.
- If the mesh never carries a direct packet on the route in use, the direct-first rule costs a
  crossing per attempt and flood-first is correct; S5.1 makes that a policy change, not a rewrite.
- If `srtt` proves too noisy at the margin, the floor becomes the firmware formula and `srtt` only
  ever raises the timer.
- If a daemon crash between pty write and the applied-ring entry proves to matter in practice, the
  fix is an execution journal written *before* the write with an `Unknown` state on recovery — more
  machinery than the current evidence justifies.
