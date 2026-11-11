# The radio path: plan of record

The public-mesh route (`LoRa`) for ldmux. This document decides work; the evidence behind it is
`docs/RADIO-AUDIT-LOOP.md` and the disposition of every entry in it is
`docs/RADIO-AUDIT-DISPOSITION.md`. `docs/RADIO-DESIGN-PROPOSAL.md` is superseded and deleted: its
carrier and timing model rested on firmware behaviour that does not exist (§1.10).

## 0. The requirement, and the model that follows

**A useful remote terminal over a shared public mesh, whose failures are visible and whose airtime
is defensible.**

- The link is intermittent. A 14–28 hop crossing over repeaters that hold, drop, rate-limit and
  re-route is not a connection, and nothing the user does may depend on one being "up".
- A crossing is expensive: 490–603 ms of air per record at the public preset, repeated at every
  hop. Cost is measured in crossings.
- Therefore: **tap to send, tap to receive.** Nothing streams back on its own. A send owns exactly
  one answer, delivered when the shell settles. A second tap while it is still working fetches the
  current state. There is no live mode on the radio.
- The notebook model: the phone is a terminal you keep notes in, the host is the machine. What you
  typed is kept per shell; what came back is kept per shell; a link failure costs a wait, not your
  place.

## 1. What is actually wrong

Each item is verified in source. Severity is justified by what the user loses, not by how odd the
code is. Ledger groups (G1–G10) are in `RADIO-AUDIT-DISPOSITION.md`.

### 1.1 Flooded records above a hop-dependent size are never delivered — **blocker** (G1)

- App → radio: 176-byte frame, `[0x41][priority][packet]`, so packet ≤ 174.
- Radio → app: a received packet is pushed only if `len + 3 ≤ 176`, so packet ≤ 173
  (`examples/companion_radio/MyMesh.cpp:291-299`).
- A flooded record is `12 + hops × hash + record`, so it arrives only while
  `record ≤ 161 − hops × hash` (4 bytes less when scoped).
- We publish a flat **162** (`mesh_serial.rs:461-465`, `MeshRecordLimits.kt:34-43`), derived from
  the send side alone.

| record | arrives while |
|---|---|
| 162 B | never |
| 143 B | hops ≤ 18 |
| 133 B | hops ≤ 28 |
| 120 B | hops ≤ 41 |

The field log's `record by flood … 133 B` on a 28-hop route is this arithmetic. Nothing reports it:
the sender is told "sent", the receiver's companion emits nothing.

### 1.2 A record limit that shrinks mid-flight duplicates and reorders bytes — **blocker** (G2)

`Session::poll_with_seq` splits an already-numbered in-flight record when the route grows: the head
keeps the sequence, the tail goes to the front of the queue and is numbered later
(`session.rs:246-276`, `arq.rs:746-766`). If the whole original had already arrived (ack lost), the
tail is delivered **twice**; and because later records were numbered in the same poll, out of order.
Corrupts a paste or a command line. `requeue_remainder`'s "same order, same bytes" contract is false.

### 1.3 The handshake is not idempotent — **blocker** (G6)

Three copies of one `msg1` built three sessions, three indices and up to nine replies; a captured
`msg1` replayed was an unauthenticated session-kill (because equal timestamps are accepted), two
fsyncs, and three flooded answers. The phone finished on whichever answer arrived first, the host
kept whichever request arrived last, and mismatches read as `handshakes 3` on a dead link.

Fixed in the working tree (uncommitted, unvalidated): an idempotent responder cache keyed on the
request bytes, with a superseded-initiation refusal and a bounded cache that never evicts a
replayable request.

### 1.4 The phone finishes a handshake with any record — **blocker** (G6)

`MeshTermDriver.onRecord` consumes the pending handshake with the first record of any kind and calls
`finish` on it (`:906-916`). A stale transport record or a stranger's packet spends a rung.

### 1.5 The carrier was a latch, and proof came from the wrong direction — **blocker** (G3, G4)

`directWorks` / `peer_via_flood` are cleared by a flood and set only by a direct arrival, so both
ends can wait for a direct record the other never sends. `peer_path_proven` is set by a **flooded**
record — an inbound chain reversed into an outbound guess — and never by a direct arrival; the timer
then takes the shortest inbound hop count, which the journal shows flipping 1↔16 hops in 11 s.

### 1.6 Every flooded record is sent three times — **major** (airtime)

`RECORD_FLOOD_COPIES = 3` on both ends for every flooded record, and `REQUEST_COPIES = 3` for route
requests (`mesh_serial.rs:513,1468`; `AndroidRadioLink.kt:2032`). Three copies of a handshake is the
right trade; three copies of every data record on a shared mesh is the largest avoidable multiplier
in the system.

### 1.7 A press is not a transaction — **blocker for the mode** (G2, G9)

- `sync()` sets `urgent` and opens a gate; the pump drains the **live** buffer, so text typed after
  the press leaves with it.
- `batcher.accepted()` removes a piece when the ARQ **queues** it, not when the peer has it; a
  session death then drops it silently.
- `Input` for a shell no longer attached returns with no reply and no counter.
- A paste split across records is applied piece by piece, newlines running on arrival.
- One draft and one mirror per **connection**, not per shell; `resumeShell` is only ever `""`, so a
  rebuild lands on the launcher and the resume path is unreachable.

### 1.8 Claims outlive the phone; the UI states facts it does not have — **major** (G7, G9)

A mesh claim is skipped by the release sweep, made indisplaceable by `engaged()`, and released only
when the session dies — but an idle session never dies. Every mesh client is the same device id, so
two phones share one shell. The screen replaces its only moving evidence at 30 s while the ladder
runs for 176–372 s, and a session death sets `session = None` before anything is logged.

### 1.9 The host stops reading the radio while it answers — **major** (G8)

`std::thread::sleep(reply_delay(airtime_ms))` runs inside the push handler, before the arriving
record is delivered upward (`mesh_serial.rs:335`). Handshakes arriving in that window are not read.
MeshCore's own clients queue delayed sends and keep receiving.

### 1.10 The measurements we designed against were not measurements

- `calcRxDelay` returns **0** on stock repeater, companion and room-server builds (`rx_delay_base`
  defaults to 0.0); what delays a flood at each repeater is a randomised retransmit delay
  `nextInt(0, 5 × airtime × tx_delay_factor)`. The proposal's universal 2.70 s/hop, 65.6 s flood
  crossing and 6.7× penalty do not exist.
- `packetScore` is a length-and-SNR heuristic used only to schedule receive processing, not a
  delivery probability, so "0 dB ⇒ coin toss" is an inference.
- The ARQ computes a true smoothed round trip on every ack and exposes it only to tests
  (`arq.rs:475`), so every timer was tuned without the one number that settles it.

## 2. What to keep

1. The renderer split: libghostty-vt per shell on the host, a row mirror on the phone, diffs on the
   wire.
2. Frame compression with the pinned dictionary (1318 B screen → 3 records), extended uplink.
3. Row diffing with a known base and the absolute top line.
4. AEAD and pairing: pinned static keys, persist-dir credentials, the record layout and replay window.
5. Local echo, the draft batcher's windows, scrubbing secrets on background.
6. The tailnet route, the desk client, claims and consented takeover.
7. Airtime accounting and refusal bookkeeping.
8. The durable phone log and the host journal vocabulary.
9. The launcher, `new shell`, history fetch, grid/fit, `Detach`/`restore_grid`.
10. 143-byte record payload, 137-byte input, credit window 8.

## 3. The design

### 3.1 Device (L0)

- One owner, advisory lock as today. `ERR_CODE_UNSUPPORTED_CMD` from a send is **terminal**: this
  radio cannot carry ldmux. Say so once, stop retrying it as busy.
- Re-synchronise the frame parser on a bad length instead of eating the following frames; count
  skipped bytes and publish the count.
- Re-assert the radio clock on every open.
- Command timeouts from the firmware's own budgets (`500 + 16a` flood; `500 + (6a + 250)(h+1)` direct).

### 3.2 Carrier (L1)

- **Chosen per attempt, never per record.** Attempt 1 with a held path builds a direct
  `RAW_CUSTOM` packet; attempt *n* builds a flood. Nothing persists between attempts, so no latch.
- **The budget is the receiver's**: flood `≤ 161 − hops_max × hash − (scoped ? 4 : 0)`; direct
  `≤ min(172 − path, 171)`; floored at the session minimum 108. Before a hop count is known, publish
  **120**. `hops_max` is the largest hop count seen from this peer.
- **One copy per data record and per route request.** Three copies only where a measurement
  justifies it (the 121-byte `msg1` today).
- **A flood arrival means "the peer cannot reach us"**: answer with a direct path teach, throttled,
  never by mirroring the flood.
- **Scope when the radio has one**, and the 4 bytes are in the budget.
- **Nothing is pushed unprompted**; no adverts by default; no heartbeat.
- `GRP_DATA` stays unimplemented until a measurement says the corrected budget is insufficient. It
  would carry 165 B independent of hops, but costs a channel slot on both radios, shares a FIFO that
  evicts channel frames first, has a 2-byte MAC and no sender identity, and no acknowledgement.

### 3.3 Epoch (L2)

- Keep Noise IKpsk2 and the sealed record; keep forward secrecy per epoch.
- **An epoch is not a connection.** It is established once per app run (or rekey) and is never a
  precondition for the user. A press before it exists is queued and sent when it does; the UI says
  "connecting".
- `msg1` is idempotent; the phone finishes only on `KIND_MSG2`; a stale record does not spend an
  attempt; the answer is retried on its own derived timer.
- No auto-replay of user input across epochs. A send that never reached the host stays queued and
  is shown as queued.
- The link stays warm while the app is in use (foreground service included), so a press normally
  pays no handshake.

### 3.4 Exchanges (L3)

**Phone → host**

- `Send { id: u16, shell: u8, part: u8, last: bool, body }` — text and key names. The host assembles
  by `id` and applies **once on `last`**, so no partial paste reaches the pty.
- `Fetch { shell: u8, want: Full | Diff { base } | History { before, rows } | List }`
- `Release { shell: u8 }` — the explicit "I am done", replacing liveness-based claim expiry.

**Host → phone**

- `Snapshot { id: u32, base: Option<u32>, parts: u8, part: u8, body, producing: bool }` — idempotent;
  applying twice is the same screen; an incomplete set is discarded; a `base` mismatch is answered
  with `Full` next time.
- `Applied { id: u16, outcome: Ok | Refused(reason) }`, riding on the snapshot the send produced.

**Rules**

- **One answer per send, on settle.** The host pushes exactly one snapshot when the shell has been
  quiet long enough to call it finished. If it never settles, nothing is pushed; the answer stays
  owed and a tap fetches the current state.
- **Sends queue in order**; each owns its settle-answer. **Fetches coalesce** — a newer fetch
  supersedes a pending one, because its purpose is "show me the current state".
- **Idempotent delivery, at-most-once execution.** A repeated `Send` id returns the cached
  `Applied` without touching the pty. After a daemon restart the ring is gone: the phone is told
  `Unknown` and shows the text as unsent rather than retrying silently. The one-instruction window
  between the pty write and the ring entry is disclosed, not hidden.
- **Backpressure answers.** A send the host cannot queue, a fetch it cannot serve, and a send for a
  shell that is gone all reply.
- **Applied on `last` only**, so a paste is one pty write and the line discipline sees whole lines.

### 3.5 Interaction (L4)

- **Two controls, not one.** `send` sends and is dimmed with an empty draft; `refresh` fetches and
  never sends. The ambiguity of one control that meant "flush the hold" in one state and "ask again"
  in another is the source of the "press show again" history, and with a settle-answer there is no
  longer a reason to merge them.
- **After a send** the bar says `sent · shell working`; it clears when the answer lands. If the
  shell never settles it keeps saying so, and `refresh` returns the current screen.
- **After a drop inside one app run**, the app returns to the shell it was in, paints the last known
  screen marked `stale`, and refreshes. After an app restart it opens on the shell list.
- **Switching shells** paints that shell's last known screen instantly, marked `stale`, and fetches.
- **Drafts are per shell** and cleared when the app goes to the background; queued sends clear with
  them, because they are part of the draft.
- **The list shows only the existing done/attention marks.** No extra "new output" dot.
- **No cost display, no cap per press.** The protocol keeps airtime honest; the screen does not
  lecture.
- **Notifications**: one per shell when an answer lands while the app is off screen, **without
  terminal text**, cleared by opening the shell. Marks keep their own separate, off-by-default
  notification; the two are different facts (see §3.7).
- **Screens are truthful**: `sent` (radio accepted), `applied` (`Applied{Ok}`), `carried` (a snapshot
  came back), `stale`, `queued`, `producing`, `not delivered`.
- **Held views disappear.** With nothing pushed unprompted, `Hold` is retired.

### 3.6 Terminal protocol

Keep `Rows`/`Commit`/`Frame`/`FrameDict`, `Profile`, `Sessions` (with page indices), `Marks`,
`HistoryPage`, `Resize`, `Create`, `Close`, `Takeover`, `MakeLive`, `Created`, `Detach`. Add the
uplink frame (a large send is deflated before it leaves). Give runs page indices so a lost middle
page is detectable.

### 3.7 Marks, notifications and the agent skill

Two different facts, deliberately not merged:

- **A mark** is the shell raising its hand: `ldmux buzz --kind done|attention`, set by an agent or a
  script. It is row state in the launcher, delivered on the tailnet by presence and on the radio
  with `Sessions`/`Attach`. Its notification is optional and off by default, and it is tailnet-only
  in practice because the radio cannot push an idle phone.
- **An answer notification** is the result of a press you made. It is not a mark, carries no text,
  and is suppressed while the app is on screen.

The `contrib/skills/ldmux-shell` skill stays: it is the only way a shell can say "turn finished" or
"waiting for you" without producing output, and it also names the shell. It is not extended to
answer notifications, and answer notifications do not fire for marks. On the radio a mark is only
timely at attach, and that is correct: it costs no airtime.

## 4. Decisions taken, with the owner's answers (2026-11-11)

| question | decision |
|---|---|
| one press, shell still working | one answer when it settles; nothing automatic if it never does; a tap fetches the state |
| after a drop | same app run: back to the shell, marked stale; after an app restart: the shell list |
| unsent text | per shell; cleared on background, including queued sends |
| live typing on radio | removed |
| never-settling command | nothing pushed until a tap |
| answer size | the finished screen only; older output via history |
| notification | yes, per shell, without terminal text |
| cost display | none |
| idle link | kept warm while the app is in use |
| second press | sends queue in order; fetches coalesce |
| switch shell | last known screen, marked stale, then refresh |
| list state | only the existing done/attention marks |

## 5. Stages

Every stage ships alone, deletes something, and is measured by its own test. Groups in brackets are
the ledger mechanisms it closes.

### S0 · Instrument and make the seam testable *(G10)*
- **S0.1** Expose `srtt`, hops, carrier per send, attempts and per-record outcome counts in
  `ldmux mesh status --json` and one journal line per exchange.
- **S0.2** Publish the corrected flood budget on both ends, floored at 120; log a size refusal
  instead of falling back silently. *(G1)*
- **S0.3** Name every silent drop: unattached input, queue overflow, session death reason, and the
  suppressed count of any rate-limited line.
- **S0.4** Seams: `ScriptedLink` with a virtual clock in `crates/companion`; a test entry point for
  the supervisor; the phone's carrier decision moved into a pure function.
- **Deletes:** nothing. **First** because every later stage is judged by numbers that do not exist.

### S1 · Delivery correctness *(G2)*
- **S1.1** Split records **before** sequence assignment; an in-flight record that no longer fits
  fails the session with a named `RetransmitTooLong`. Remove `requeue_remainder`.
- **S1.2** Clear the refusal window only on evidence of acceptance, so `REFUSAL_DEADLINE_MS` is
  reachable.
- **S1.3** Never clear the phone's whole outgoing queue on one refusal.
- **S1.4** Never drop the oldest from an application queue; refuse new work. Do not advance the ack
  for bytes the application has not accepted.
- **S1.5** Tests: duplicate, reorder and amplifier cases; refusal deadline; queue flush; ack vs
  deliver.

### S2 · Handshake *(G6)*
- **S2.1** Land the idempotent responder cache with its tests.
- **S2.2** Finish only on `KIND_MSG2`; stale records do not spend an attempt.
- **S2.3** One deliberate re-attempt on a derived timer; no blind ladder; "connecting" is a state,
  not a failure.
- **S2.4** Count a superseded initiation separately from an attack.
- **Deletes:** the ladder that gates the mode.

### S3 · Exchanges *(G2, G9)*
- **S3.1** `Send{id, part, last}` with host assembly and one atomic apply.
- **S3.2** `Applied{id, outcome}` with a per-device ring; `Unknown` after a restart.
- **S3.3** The phone keeps a send until `Applied`; the draft is an immutable prefix taken at the
  press; later typing waits.
- **S3.4** Every refusal answers.
- **S3.5** Uplink compression for a large send.
- **Deletes:** "queued = sent", silent drops, partial-paste application.

### S4 · Snapshots *(G9)*
- **S4.1** `Fetch`/`Snapshot{id, base, parts, producing}`; paint only complete sets.
- **S4.2** One settle-answer per send; retire the idle marker as an answer boundary and retire
  `Hold`.
- **S4.3** Page indices on every run (list, marks, history, snapshot parts).
- **S4.4** History and older output on demand only.
- **Deletes:** the "press show again" class, the 4 s quiet guess, freeze-as-listener.

### S5 · Carrier *(G3, G4, G8)*
- **S5.1** Per-attempt carrier; delete `peer_via_flood` and `directWorks`.
- **S5.2** Proof only from a direct record or a direct ack; a flood chain teaches a teach.
- **S5.3** Time by `srtt`, floored by the firmware formulas; never the shortest inbound path, never
  `calcRxDelay`.
- **S5.4** Outbound queue with `not_before`; the reader never sleeps.
- **S5.5** One data copy per attempt; three only where measured; scope when available.
- **Deletes:** the latch, the 162 cliff, the blocked reader.

### S6 · Claims and per-shell state *(G7)*
- **S6.1** Claims end on `Release`, `Detach`, shell exit or consented takeover. No heartbeat, no
  liveness expiry; a stale claim is displaceable by consent and the phone learns on its next
  exchange.
- **S6.2** Per-link device id.
- **S6.3** Per-shell draft, mirror, snapshot id, history cache, scroll anchor.
- **Deletes:** hours-long quiet claims, the launcher bounce, the dead `resumeShell`.

### S7 · Lifecycle and device
- **S7.1** Wake-lock lifetime follows the service, not the notification text (patch ready in a
  worktree; review, then land).
- **S7.2** A route switch must not silently tear down a warm epoch; if it must, say so once.
- **S7.3** Off-screen release sends `Detach`.
- **S7.4** Parser resync; clock on open; `UNSUPPORTED_CMD` terminal.
- **Deletes:** the off-screen dead zone and the "wedged radio" class that was a truncated frame.

### S8 · Interaction *(G9)*
- **S8.1** Two controls: `send` and `refresh`; sends queue, fetches coalesce.
- **S8.2** Local echo of the draft; per-shell drafts; cleared on background.
- **S8.3** The vocabulary of §3.5, including `stale`, `queued` and `producing`.
- **S8.4** Answer notification without text; marks unchanged and separate.
- **S8.5** Remove live typing and its controls from the radio; keep the tailnet's stream, labelled.
- **S8.6** Scrolling: a page per tap, typing returns to the bottom, buttons only, controls grey out
  rather than disappear.

### S9 · Retire and document
- **S9.1** Remove the ordered-stream machinery S3–S5 replaced (window, cumulative ack, rekey as a
  gate), keeping parts and retry for bulk.
- **S9.2** Update `ARCHITECTURE.md` (§2, §5–§9, §11a), `AIRTIME.md`, `RELEASE-AUDIT.md` §18, the
  ADRs (new: carrier per attempt, exchanges, snapshots; superseded: 0029, 0032, 0037, 0038), and
  this plan, in the same commits.
- **S9.3** Keep `RADIO-AUDIT-LOOP.md` and `RADIO-AUDIT-DISPOSITION.md` as labelled audit records,
  not design.

**Forced order:** S0 before S5; S1 before S3; S2 before S3; S3 before S4; S4 before S6/S8.

## 6. Integration surfaces

- **Protocol** (`mesh_term.rs`): `Send`, `Applied`, `Fetch`, `Snapshot`, `Release`; page indices;
  uplink frame. One version bump; an old phone is refused with `Closed { VERSION }`. Pre-release,
  both ends deploy together.
- **Keeper**: unchanged. The bridge buffers a send in memory and writes once on `last`.
- **Ownership**: `Release` maps to the claim table; a send for a vanished handle answers
  `Refused(NotAttached)` and the phone keeps the text.
- **Two routes, one phone**: the tailnet client keeps its stream; the mesh gains explicit exchanges;
  both share the codec, mirror and UI states.
- **Rollback**: remove `LDMUX_MESH_SERIAL` and the daemon never opens the port; the tailnet and the
  desk client are unaffected. A radio change can never cost a shell.

## 7. How it is proven without a field trip

- `crates/session/tests/lossy_multihop.rs` extended to the exchange layer: loss, duplication,
  reordering, a route that grows or shrinks mid-flight, a replayed `msg1`.
- `ScriptedLink` plus a virtual clock for the carrier: delayed sends, refusals, a silent radio, a
  partial frame, a burst of frames.
- A pure carrier function on the phone, so the policy is a table test.
- Bench with both radios at minimum power: does `0x41` exist on this firmware, real airtime, does a
  group datagram arrive, does the wakelock survive an hour. One afternoon.
- A field trip is only for reach: how far, how many hops, which carrier wins. S0's log turns it into
  a table.

## 8. What would make this plan wrong

- If `hops_max` on the real route is small, the budget fix matters less and `GRP_DATA` is not worth
  its cost.
- If the route in use never carries a direct packet, flood-first is correct and S5.1 becomes a
  policy change, not a rewrite.
- If `srtt` is too noisy at the margin, the firmware floor leads and `srtt` only raises the timer.
- If the pty-write/ring-entry window proves to matter in practice, the answer is an execution
  journal written before the write, with `Unknown` on recovery. Not justified yet.
