# Radio path audit ledger (append-only)

Started 2026-11-10. Purpose: identify **everything** that makes the radio path fail, stall or waste
airtime at range, with evidence, so an advanced model can act on it later. **No code changes are made
during this audit** — the only file written is this one.

## Register — blockers, ordered by what the fix buys

Read this first. Each line names the entry, the defect in one clause, and the smallest change that closes it.
Entries are in the Ledger below with full evidence.

1. **A-340 + A-380** — we flood when a path is *stale*; the reference floods only when the path is *unknown*, and escalates **per attempt** by masking the path for that one send alone (`base_send.py:1292-1346`). A flooded record costs ~20x a direct one at 16 hops. Corrected by A-441: do **not** delete the per-attempt escalation, replace the persisted rule with a per-attempt one.
2. **A-80 as corrected by A-442 + A-370 + A-460** — `proven` *is* set at range, but by a **flooded** record whose chain we merely reverse (a guess about the outbound direction), and it is **never** set by a direct arrival, which is the only real proof. Meanwhile the timer is priced from the shortest inbound hop count (1 while the chain is 16). → set `proven` on a direct arrival only, time by the measured `srtt` the ARQ already computes (`arq.rs:670-683`, unreachable outside tests), never by a shortened hop count.
3. **A-20 + A-240** — three identical `msg1` copies create three sessions, and a captured `msg1` replayed is an unauthenticated session-kill that also amplifies transmissions. → idempotent `msg1` cache: identical bytes in, identical cached `msg2` out, no session replacement, no index advance, no outbox clear.
4. **A-60 as corrected by A-445** — the "5 hops" figure comes from `mcrs` (a third-party Rust rewrite), not the shipped C++ firmware, whose `flood_max_unscoped` defaults to 0 and which this mesh demonstrably forwards at 5–12 hops. A **conditional portability risk**, not the observed cause: set a scope before relying on a regionalised mesh, but it is not why tonight failed.
5. **A-120** — the two sticky-flood flags latch: each end waits for a direct record the other never sends. That is the mirroring ADR-0038 rejects. → decay on *any* arrival, plus one deliberate cheap direct probe per N records.
6. **A-102 + A-108** — an attach answer is 18–27 transmissions, the credit window hands over eight records at once, and the ARQ's 41 s timer expires before one crossing completes. → one-record attach answer, window sized to the crossing.
7. **A-180** — the silence watchdog cannot fire: `Status` is self-generated every 500 ms and refreshes `last_carrier` without touching the radio. → count only successful radio commands.
8. **A-61** — the host sleeps on the thread that reads the radio while it answers a route request (up to ~17 s), so handshakes arriving then are never read. → enqueue the answer, keep reading.
9. **A-200** — changing the route persists a config field, tears the radio link down at GATT level and restarts the ladder from rung one. → keep the link alive across route changes, or say plainly that switching costs a full ladder.
10. **A-220** — a session death sets `session = None` before logging, so "retries exhausted" is emitted nowhere. → log at the point of death.
11. **A-280** — the UI deletes its only moving evidence at 30 s and stays inert for the remaining 83–85 % of the attempt. → keep the live line and say "still trying, attempt n of m".
12. **A-160** — in live mode a keystroke is invisible until the host echoes (13 s direct, ~64 s flooded); local echo is wired only to held mode. → drive `echoHeld` from live mode.
13. **A-140** — the downlink is compressed, the uplink is not: a 1.6 KB paste is 12 records plain against 2 deflated. → a phone→host framed message, gated by a feature bit.
14. **A-320 + A-327** — every shell switch destroys the mirror and the difference buffer: five shells is 20 records and five round trips, 5–7 minutes at 14 hops. → keep a mirror per shell, or accept one and say so.
15. **A-260** — five of the six blockers were *untestable as written*: the session loop is private, the carrier policy needs an Android `Context`, and the fake link has no clock. → scripted link with a virtual clock; pure carrier function; test entry point for the supervisor.

## Numbers every later round should reuse

- Airtime (910.525 / 62.5 kHz / SF7 / CR5, preamble 32): 42 B 223.7 ms · 121 B 459.3 ms · 133 B 490.0 ms · 172 B 602.6 ms.
- Repeater hold per hop: 0.66 s at home, **2.70 s at the margin** for a 133 B packet — 4.1× worse than the timers we ship.
- Crossing at 16 hops: **9.8 s direct**, **65.6 s flooded** (a flood is deferred at every repeater; a direct packet is not).
- Throughput optimum ≈50 B PHY payload (85.2 s/kB) versus 143 B (112.1 s/kB): large records cost 24 % more at the margin.
- Framing (real dictionary, deflate 9): 46×38 screen 1318 B → 10 records unframed, **3 framed**; 38-row history 1458 B → 11 unframed, **2 framed**.

## Severity

- `blocker` — the link cannot work, or works only by luck.
- `major` — works sometimes; wastes airtime or time badly; user sees a hang.
- `minor` — inefficiency or rough edge with no functional loss.
- `question` — needs a measurement or research before it can be graded.

## Entry schema

```
### A-<n> · <aspect> · <severity>
- Evidence: <file:line | commit | journal line | phone log line | doc | clone path:line>
- Finding: <what is actually true>
- Why it hurts at range: <causal chain to airtime, latency or a dead UI>
- Proposed fix (NOT applied): <one paragraph naming the file/constant it would touch>
- Open questions: <what a later round should check>
- Investigator: <id> · <time>
```

## Queue

| # | aspect | state |
|---|---|---|
| 1 | physical layer: preset, SF/BW/CR, sensitivity, airtime per record | done |
| 2 | MeshCore forwarding: flood limits, scoping, rate limits, advert caps, dedup | done |
| 3 | handshake: IK, sizes, timestamps, replay tolerance, pairing lifecycle | done |
| 4 | carrier selection: direct/flood evidence rules, oscillation, cost | pending |
| 5 | session: ARQ, RTO vs hops, window, fragmentation, ordering | in progress |
| 6 | attach flow: list/profile answer, one-record fit, waits, UI states | in progress |
| 7 | batch mode: repaint size, compression, record ceiling | pending |
| 8 | live mode: keystroke latency, echo, cursor, resync | pending |
| 9 | paging, grid protocol, multi-shell | pending |
| 10 | phone lifecycle: instances, radio lease, mode vs transport, wakelocks | pending |
| 11 | host side: serial handling, scheduling, backpressure, restarts | pending |
| 12 | observability: what cannot be seen, refusal reasons, latency | pending |
| 13 | UX expectations at range: what the user is told and when | pending |
| 14 | security: replay, PSK, pinning, downgrade | pending |
| 15 | tests and simulation: untested paths, what mcsim could prove | pending |
| 16 | docs correctness: every claim against the code | pending |
| 17 | comparison with openhop_core / meshcore-pi behaviour | pending |
| 18 | alternatives: single-message terminal, compression-first, adaptive preset | pending |

## Ledger


### A-20 · handshake: three identical `msg1` copies each build a *different* session · blocker

- Evidence: `crates/daemon/src/mesh_session.rs:286` (`let index = self.next_index;`), `:319-333` (a session is installed, `outbox.clear()`, `next_index` advanced, `handshakes += 1`, per accepted message); `crates/session/src/handshake.rs:295-299` (`accept()` builds a **fresh responder** each call, so each reply carries a new ephemeral); `crates/session-ffi` test `a_full_143_byte_payload…` plus `packages/mobile/.../AndroidRadioLink.kt:1026-1053` (the phone sends the **same** `record` bytes three times, differing only in the LDMR wrapper nonce); commit `2ea77bb` (equal timestamps are now accepted, so copies 2 and 3 are no longer refused as `Replay`).
- Finding: the phone emits one `msg1` as three floods. The host has no content dedup on handshake records (`crates/daemon/src/mesh_serial.rs:2207-2221` sends **every** flooded record to `Event::Recv`). Before `2ea77bb`, copies 2–3 were rejected as `Replay`, so exactly one session existed (but only one 50-byte reply ever went out, and if *that* was lost the rung was spent). After `2ea77bb`, each copy is accepted: **three sessions, three different Noise ephemerals, three different session indices, and up to nine flooded `msg2`s** (three copies each, `mesh_serial.rs:1468`). The phone finishes with whichever `msg2` arrives first (`MeshTermDriver.kt:909-916`), the host ends on whichever `msg1` arrived last. When they differ, every record the phone sends is rejected — `record[1] != index` (`crates/session/src/record.rs:180`) → `WrongIndex`, or the tag fails → `BadTag`.
- Why it hurts at range: at range the three copies arrive seconds apart, so each reply is transmitted before the next copy lands — the host therefore emits three *different* answers and the phone and host agree on the session only when the arrival order happens to line up, i.e. roughly one attempt in three. That is the "it handshakes and then nothing works" loop, and it is the worst possible failure shape because the host's own journal says `session replaced`/`handshakes 3` (looks healthy) while the phone's records are discarded.
- Proposed fix (NOT applied): make `msg1` handling **idempotent**. Keep the last accepted `msg1` (its bytes, or a hash) with its cached `reply` and, if the same bytes arrive again inside the timestamp window, re-send that cached reply and return without touching `session`, `index`, `outbox` or `handshakes`. That is the standard Noise-responder answer to a replayed initiation and it makes the three copies behave exactly like one copy with two free retries of the answer — which is what they were added for. Second half: the phone must also accept that the host may re-send an identical `msg2` (it already guards stale ones, `MeshTermDriver.kt:956-965`).
- Open questions: whether the reply cache should be per pairing or per (pairing, timestamp); whether a cached reply older than the window must be dropped; and whether the same reasoning applies to a re-`msg1` from a genuinely *new* attempt (new ephemeral ⇒ different bytes ⇒ cache miss, so no interference).
- Investigator: handshake round · 2026-11-10

### A-21 · handshake: *any* record finishes a pending handshake, and a failure destroys it · blocker

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTermDriver.kt:909-916` — `val h = handshake; if (h != null) { handshake = null; val s = try { h.finish(record, …) } catch (e) { failed(e.state, …) } }`. The only shape check is inside `finish` (`crates/session/src/handshake.rs:206-208`: length and `KIND_MSG2`), and any mismatch becomes `failed(…)`, which abandons the handshake and restarts recovery (`MeshTermDriver.kt:765-800`).
- Finding: the pending handshake is consumed by **the first record of any kind** that arrives, before anything checks whether it is a plausible answer. A `0x13` transport record left over from the previous session, a surplus `msg2` from an earlier attempt, or a record that is not ours at all all reach `finish()` and are converted into a fatal handshake failure. The code's own comment at `MeshTermDriver.kt:330-334` records the same symptom from 2026-11-01 ("the late record was handed to the *new* handshake and failed to decode"), but the dispatch was never changed — only the retry interval was.
- Why it hurts at range: crossings take seconds, so late answers are normal rather than exceptional. One stale record therefore costs a whole rung — 8 s, 16 s, 32 s, and now a two-minute answer window — and it is indistinguishable on the screen from the host not answering. It also silently converts "the host answered a moment too late" into "the mesh is broken", which then feeds route blame (`Blame.RouteUnlessDirect`).
- Proposed fix (NOT applied): two changes, both cheap. (1) Before consuming `handshake`, require `record[0] == KIND_MSG2` (0x12) — a transport record or a stranger's packet must fall through to the session path or be ignored, not finish the handshake. (2) Keep the consumed attempt recoverable: on `Crypto`/`Malformed` from `finish`, do not call `failed`; treat it as a stale answer, re-send the *same* `msg1` (no new ephemeral, so a cached reply hits — see A-20) and keep the ladder where it is.
- Open questions: whether keeping two pending handshakes (current and previous) for a few seconds is better than re-sending; and whether `finish` should be attempted on a *copy* of the handshake state so a failed attempt does not consume the Noise state.
- Investigator: handshake round · 2026-11-10

### A-22 · handshake: the reply has no retransmission of its own · major

- Evidence: `crates/daemon/src/mesh_session.rs:380-393` — a queued reply is polled **once** (`outbox.pop_front()`), stamped `seq: None`, and `on_send_refused` (`:438-447`) can only report a refusal because there is no slot to give back. The only recovery is the phone re-asking, which re-enters `on_handshake` and (A-20) makes a new session.
- Finding: the handshake answer is control traffic sent once per accepted message, never retried by the host, and its loss is invisible on the host side (`reply sent ok=false` is logged in the carrier but not retried). The three flooded copies (`mesh_serial.rs:1468`) are the only redundancy, and they are three attempts at the *air*, not three attempts at the *radio* — a `Rejected(busy)` on all three ends the rung.
- Why it hurts at range: the answer is the half of the exchange that has to travel back over the chain that just dropped the request's echo. Losing it costs a full ladder rung (8→16→32 s) plus a session replacement. Measured tonight: the host logged `record 50 B sent by flood over 0 hop(s)` at 22:13:26 and the phone was still handshaking minutes later.
- Proposed fix (NOT applied): hold the reply in the outbox until either an arrival from the peer proves it landed (any record with a session index the reply created, or a `msg1` that hits the A-20 cache) or a deadline derived from the route expires, and re-poll it on that deadline — mirroring the ARQ's own rule rather than inventing a second one.
- Open questions: how to tell "the reply landed" when the phone's next message is a re-ask (which looks the same as a lost reply); whether the deadline should be `direct_round_trip(hops, airtime)` or the flood interval.
- Investigator: handshake round · 2026-11-10

### A-23 · handshake: replacing a session silently discards the bytes already owed · major

- Evidence: `crates/daemon/src/mesh_session.rs:329` (`self.session = Some(session)`) — the previous `Session`, which owns the ARQ with every unacknowledged record, is dropped. `outbox.clear()` on the same page (`:330`) drops any queued control record. No caller is told; `SessionHost::send` (`:343-356`) has already returned `Ok`.
- Finding: a handshake that arrives while data is in flight destroys that data. The application layer is not notified, so from the phone's point of view the attach it asked for simply never comes back — which is precisely the "asking host for shell, counts to 100, resets" shape reported tonight, and equally "a paste that half-applies".
- Why it hurts at range: at range handshake replacements are frequent (A-20 makes them three per attempt), so a link that is *working* still loses whole answers. Every lost answer costs a re-ask plus, often, a rebuild.
- Proposed fix (NOT applied): on replacement, hand the old session's outstanding bytes back to the caller as a `Vec<(shell, bytes)>` (the session already exposes enough to enumerate them, or the ARQ can be asked for its unacked queue) so the daemon can re-send them on the new session, or at minimum log one line naming how many bytes were lost. Do not silently discard.
- Open questions: whether re-sending across a key change is safe for the attach flow's idempotency (an attach is a request, so probably yes); whether the phone should be told explicitly (`Closed(SESSION_REPLACED)`) so it stops waiting.
- Investigator: handshake round · 2026-11-10

### A-24 · handshake: the ladder still assumes a record cannot cross repeaters · major

- Evidence: `MeshTermDriver.kt:1093-1121` — when `gateway.routeHops == null || !gateway.routeFresh`, the rung sends a route request instead of the handshake, and only **every third** rung ("`blindAsks % 3 == 0`") sends the handshake at all. The rationale in the comment is "a session record is a direct packet and a direct packet crosses no repeater". Since `ADR-0037`/`ce0e600` that premise is false: `AndroidRadioLink.kt:1010-1055` puts the handshake record on the **flood** carrier whenever `!directWorks`, and the field log shows `record by flood 1/3: Accepted · 133 B · dest 97` on every rung.
- Finding: with no route — the normal state at range — two rungs in three spend their airtime on a route request and send no handshake, and the rung interval is reset to the first rung (`handshakeTries = 0`, `MeshTermDriver.kt:1106`), so the ladder never climbs while it is blind. The behaviour is correct for a direct-only world and wrong for the current design.
- Why it hurts at range: a flooded handshake *can* cross 14 hops (measured: host `flooded record 121 B from a 0 hop chain`, and earlier `flooded record 40 B from a 14 hop chain`). Skipping it to ask for a route that may never be answered spends three 48-byte floods to avoid one 121-byte record that would have worked.
- Proposed fix (NOT applied): gate the blind branch on the carrier, not on the route — ask for a route on one rung and send the handshake (flood) on the next, with the handshake never skipped, since it no longer needs a path. Or simply invert the ratio while `floodCarrierUsable` is true.
- Open questions: what the right ratio is when the flood carrier itself is failing (`mesh heard nothing` for minutes); whether the route request should be dropped entirely once adverts are being heard.
- Investigator: handshake round · 2026-11-10

### A-25 · handshake: every refusal reason lives only on the host · major

- Evidence: `crates/daemon/src/mesh_session.rs:505-514` (`describe`) → `:295,303,338` (`last_error`) → `crates/daemon/src/bin/ldmux.rs:419` (`handshakesRefused` in status). There is **no** wire message carrying a refusal reason; the phone's state machine only knows that no answer arrived (`MeshTermView.Handshaking` / `NoAnswer`).
- Finding: `Stale`, `Replay`, `PinMismatch`, `BadFilter` and `Crypto` are five different faults with five different cures, and the operator sees only "handshaking". Tonight's field report — "it just turned out again on the handshake" — is exactly this: the host had a specific, nameable reason in its journal and the phone had nothing.
- Why it hurts at range: at range there is no laptop next to the phone, so the only diagnostic is the phone's durable log. A reason that exists only in the host's journal is a reason the operator cannot act on.
- Proposed fix (NOT applied): a small unsealed control record (`LDNO` + reason code + pairing id) the host may send on any refusal, best-effort, once per refusal per window; the phone logs it (`mesh host refused: <reason>`) and shows it in place of the generic sentence. It carries no secret, so it can be unsealed like the route reply.
- Open questions: whether an unsealed refusal is an oracle for an attacker probing PSKs (it would reveal that a filter tag matched, but only to someone who already knows the PSK — the `BadFilter` case must stay silent); and whether the reason should be rate-limited to one per minute.
- Investigator: handshake round · 2026-11-10

### A-26 · handshake: the ±5-minute clock window is a hard dependency with no recovery path · major

- Evidence: `crates/session/src/handshake.rs:313-315` (`if now_ms.abs_diff(timestamp_ms) > self.window_ms { return Err(Stale) }`), `:66` (`TIMESTAMP_WINDOW_MS = 5 * 60 * 1000`); the phone supplies `Clock.System.now()` (`Radio.kt:643-647`). `describe` names the cure in words the operator never sees (A-25): "is the phone's clock right?".
- Finding: the handshake requires the phone's wall clock to be within five minutes of the host's. A phone used radio-only for days, booted without network time, or with a manually-set clock fails **every** attempt with `Stale`, forever, and neither end says so. There is no fallback: no way to negotiate, no way for the host to answer with "your clock is off by X", and `clear_pin` does not help.
- Why it hurts at range: the radio-only user is precisely the user who has been offline long enough for the clock to be untrustworthy, and the failure mode is total and silent. This is the classic tension between an anti-replay timestamp and an intermittently-connected peer; WireGuard solves it with a counter that starts at 0 per handshake and accepts an initiation only after a valid cookie, not with wall-clock ordering.
- Proposed fix (NOT applied): two parts. (1) Make `Stale` recoverable: when the host computes a large `abs_diff`, include the host's own `now_ms` in the refusal record (A-25) so the phone can log the offset and, if it is the phone that is wrong, offer to correct it; (2) consider replacing the timestamp with a per-pairing monotonic *attempt counter* negotiated once (the host keeps `last_counter`), pushing the clock dependency into pairing time only.
- Open questions: whether `snow`'s Noise state can be reused across a rejected attempt (it cannot, so a re-ask is unavoidable); whether Android's `System.currentTimeMillis()` is trustworthy after a long offline period (it is RTC-backed, but drift and manual changes are real).
- Investigator: handshake round · 2026-11-10

### A-27 · handshake: the replay floor outlives the pairing it belongs to · major

- Evidence: `crates/daemon/src/mesh_session.rs:531-537` — `clear_pin()` writes `pinned = None` and **keeps `last_timestamp_ms`**, with the comment "Clear the pinned phone key while keeping the replay timestamp." `write_pairing`/`parse_pairing` (`:657-680`) persist the floor to `mesh-pairing.json` and `current_pairing()` reloads it on every L2 connection (`:539-553`).
- Finding: a new pairing grant — a new phone, a factory reset, a restored backup — inherits the *old* device's replay floor. A new phone whose clock is behind the old one by more than `HANDSHAKE_REPLAY_TOLERANCE_MS` (10 minutes, `handshake.rs:69`) is refused as `Replay` until its clock passes the old floor. With a *new PSK*, the honest handshake is refused on a rule that protects against a replay of a message encrypted under a key that no longer exists.
- Why it hurts at range: re-pairing is the cure the app offers when the link is dead. If the cure itself is blocked by stale state, the user has no path forward that does not involve walking to the host.
- Proposed fix (NOT applied): reset `last_timestamp_ms` to 0 when the PSK changes or when `clear_pin` is called as part of a *new pairing grant* (as opposed to a pin-only clear), or bind the floor to the pairing identity (PSK hash) so a new pairing starts with an empty floor.
- Open questions: whether a pin-only clear can be distinguished from a re-pair at the call sites (`crates/daemon/src/mesh.rs:562-570`, `:872`); whether an attacker could use a re-pair to replay an old `msg1` (they would need the old PSK, which a re-pair has revoked).
- Investigator: handshake round · 2026-11-10

### A-28 · handshake: the screen calls it "no answer" at 30 s while the ladder keeps going for minutes · minor (UX, but it produces airtime)

- Evidence: `MeshTermDriver.kt:345` (`handshakeVerdictMs = 30_000`) used at `:2353-2357` to pick `MeshTermView.NoAnswer`; the actual budget is `handshakeWaitMs(hops)` = `rung × 7 + max(rung, 120_000)` (`:2596-2623`), i.e. ≥190 s at a neighbour and much longer on a route.
- Finding: the UI declares failure roughly six times earlier than the driver gives up. The operator sees "no answer", presses something, and a fresh ladder starts — the airtime is spent twice and the second ladder competes with the first.
- Why it hurts at range: at range the first answer genuinely can take two minutes (that is why the budget was raised in `499de61`), so the verdict is now *always* premature, and it trains the user to intervene at the worst moment.
- Proposed fix (NOT applied): derive the verdict from the same budget the ladder uses, or split the sentence: "no answer yet after 30 s — still trying (attempt 2 of 3)". The count already exists (`handshakeNo`, `attemptsThisLink`).
- Open questions: whether an explicit "stop trying" affordance is better than a verdict the user is expected to ignore.
- Investigator: handshake round · 2026-11-10

### A-29 · handshake: copy spacing is derived from a 48-byte packet, not from the record being sent · minor

- Evidence: `AndroidRadioLink.kt:2005-2008` — `requestCopyDelayMs` = `airtime(48 bytes) × 1.5`, clamped 400–3000 ms; applied at `:1028` to every flooded record, including the 133-byte handshake (whose own airtime at this preset is ~560 ms).
- Finding: the three handshake copies are spaced ~400–420 ms apart while each occupies ~560 ms of air; on a half-duplex radio they are therefore queued rather than separated, and the "time diversity" the copies exist for is partly lost — three transmissions in a row behave like one longer transmission, exposed to the same fade.
- Why it hurts at range: the whole point of three copies is time diversity against a margin fade. Spacing them by less than their own airtime buys little more than repeating a frame.
- Proposed fix (NOT applied): size the delay from the record actually being sent — `airtime(record.len()) × 1.5` — which also naturally spaces the shorter route requests as they are today.
- Open questions: whether the firmware's own `txdelay` already handles this at the repeater and whether phone-side spacing should be larger still (`2×` airtime) to clear the echo of its own transmission.
- Investigator: handshake round · 2026-11-10

### A-30 · handshake: two counters and one sentence say the wrong thing · minor

- Evidence: `mesh_session.rs:333` — `handshakes += 1` on every *accepted message*, which with three copies is three per attempt; `MeshTermDriver.kt:711` prints `handshake n/3` from a clamped copy counter; `mesh_session.rs:319-326` prints `session replaced after {lived}s (hops=H, {last_error})` where `last_error` may be a *refusal of a later attempt*, not the reason this session died.
- Finding: the journal's three most-read lines are all ambiguous. `handshakes 3` can mean one phone attempt or three; `session replaced after 5443s (…, handshake refused: replayed…)` was measured tonight and reads as "the old session died of a replay" when the refusal was of the *new* attempt; `handshake 4/3` (already fixed in `d4a20e3`) was the visible tip of the same counter confusion.
- Why it hurts at range: every investigation so far — including this one — has had to reverse-engineer these lines. Misleading instrumentation is a bug in a system that can only be debugged from logs.
- Proposed fix (NOT applied): count *attempts* separately from *accepted messages* (both are useful); print the replacement reason as its own field captured at the moment the old session died, or print `previous session ended: <reason>` as a distinct line; and print the copy number alongside the accept so a reader can see three copies of one attempt.
- Open questions: whether the phone's `handshake n/3` should count ladder rungs rather than copies, for symmetry with the host.
- Investigator: handshake round · 2026-11-10

### A-31 · handshake: two `fsync`s happen before the answer is queued, once per copy · minor

- Evidence: `mesh_session.rs:692-708` (`write_private`: write tmp → `sync_all` → `set_permissions` → `rename` → open dir → `sync_all`), called from `on_handshake` at `:303` **before** `Session::with_airtime` and before the reply is queued (`:331`).
- Finding: each accepted `msg1` performs a durable write of the pairing file (two fsyncs, one of them on the directory) on the critical path between receiving the request and queueing the answer. With three copies that is three durable writes per attempt. The refusal-on-persist-failure policy is right; the placement in the latency path is not.
- Why it hurts at range: it adds disk latency to the one exchange where every millisecond of the answer window is contested, and it makes the answer's timing depend on unrelated disk activity. On a loaded or networked filesystem this is milliseconds to tens of milliseconds; it is never zero.
- Proposed fix (NOT applied): queue the reply first and persist asynchronously, with the *policy* unchanged — if the persist fails, tear the session down immediately (the security property is "no session without a recorded floor", which is still honoured because the teardown happens before any record is accepted under it).
- Open questions: whether the teardown-after-queue can be raced by an incoming transport record (it can, within the same poll); whether `O_DSYNC` on a single file write is cheaper than the tmp+rename dance here.
- Investigator: handshake round · 2026-11-10

### A-32 · handshake: one pairing per host · question

- Evidence: `mesh_session.rs:287-291` — `accept(std::slice::from_ref(&self.pairing), …)`, a slice of exactly one; `crates/daemon/src/mesh_bridge.rs:2017` — "carries one phone key, and a second phone means a new pairing".
- Finding: a second phone cannot be paired without displacing the first, and the displacement is a new PSK plus a `clear_pin` (which keeps the replay floor — A-27). Two ldmux phones in range of one host therefore either take turns or thrash.
- Why it hurts at range: unknown whether this matters — it depends on whether the operator ever carries two devices, or tests with a second handset. Flagged as a question rather than a fault, because single-pairing is a deliberate simplification and the ledger should not manufacture scope.
- Proposed fix (NOT applied): if it ever matters, `Responder::accept` already takes a slice, so the change is a small table (one entry per pairing, each with its own floor and pin) plus a pairing id in the refusal record (A-25).
- Open questions: whether a second phone is a real use case for this project, and what the QR flow would look like for two.
- Investigator: handshake round · 2026-11-10

### A-33 · handshake: the session index is 1 byte and reused every 255 handshakes · question

- Evidence: `mesh_session.rs:106` (`next_index: u8`), `:136` (`1`), `:332` (`wrapping_add(1).max(1)`); `crates/session/src/record.rs:180` (index is the lookup key).
- Finding: after 255 accepted handshakes the index wraps and a stale record from an earlier session with the same index becomes a candidate for the live session — where it fails on the counter/replay window if the counters disagree badly enough, and on the tag otherwise. With A-20 making three sessions per attempt, 85 attempts wrap the index.
- Why it hurts at range: probably harmless (the tag and the replay window are the real gates), but it is an unnecessary coupling between "which session" and "a one-byte field", and it is exactly the kind of thing that produces a once-a-day mystery.
- Proposed fix (NOT applied): leave the wire field at 1 byte (it is a lookup hint, not a security boundary) but stop reusing it: hold the *live* index until the session is replaced and refuse a record whose index matches a session that is gone but whose counter is lower than the new session's first counter. Or simply never wrap within a connection: derive it from a per-daemon counter and let the tag do its job.
- Open questions: whether any stale-record scenario survives the tag; whether 255 is reachable in a week of aggressive retries (it is, at ~85 attempts).
- Investigator: handshake round · 2026-11-10

### A-34 · handshake: the cost of one rung, in airtime · minor (arithmetic for later rounds)

- Evidence: `handshake.rs:50-51` (`MSG1_LEN 121`, `MSG2_LEN 50`); `mesh_serial.rs:465` (`LDMUX_FLOOD_RECORD_MAX = 162`, wrapper costs 12, so 121+12 = 133 B on the air, matching the phone's `record by flood … 133 B`); `RECORD_FLOOD_COPIES = 3` on both sides (`AndroidRadioLink.kt:2032`, `mesh_serial.rs:1468`).
- Finding: one handshake rung at this preset costs, on the air, roughly `3 × airtime(133 B) + 3 × airtime(62 B)` ≈ `3 × 560 ms + 3 × 300 ms` ≈ **2.6 s** of transmission before any route request, and with A-20's three sessions it can reach **~8 s** plus three route-request bursts. On a shared city mesh that is a large, repeated, and (for two thirds of it) redundant spend.
- Why it hurts at range: airtime is the scarce resource; the current design spends it on duplicates that produce *conflicting* state (A-20) rather than on duplicates that produce reliability.
- Proposed fix (NOT applied): none of its own — this entry exists so later rounds can price changes against a number. The fix is A-20 (make the duplicates idempotent, at which point 3 copies of `msg1` cost 1.7 s and 1 reply costs 0.3 s).
- Open questions: measured airtime per record on the actual radio (the phone records `recordAirtime`; the host does not), so this estimate can be replaced with a measurement.
- Investigator: handshake round · 2026-11-10

---


## Round summary (deepseek-flash, handshake)

- **A-20 is a blocker and it is a regression from `2ea77bb`**: three identical `msg1` copies now create three sessions, three ephemerals and up to nine flooded replies.
- A-24: the blind ladder still assumes a record cannot cross a repeater, which stopped being true with the flood carrier.
- Undetermined: whether tonight the phone consumed a stale record (A-21) or never received the replies; whether `snow` allows a failed `finish` to be retried; measured host-side per-record airtime.


### A-40 · physical: the link runs at about 0 dB of link margin, and SF7's floor is −7.5 dB SNR · major

- Evidence: `docs/MESH-RADIO-CONSIDERATIONS.md:13-15` (range readings **−107 to −124 dBm, SNR −6.75 to +5.5 dB**); `filter-firmware/src/helpers/radiolib/RadioLibWrappers.cpp:310-317` (`snr_threshold[]`: `-7.5, -10, -12.5, -15, -17.5, -20`, "SF7 needs at least -7.5 dB SNR"); `meshcoreng/src/helpers/radiolib/RadioLibWrappers.cpp:223-224` and `mcsim/MeshCore/src/helpers/radiolib/RadioLibWrappers.cpp:174-175` (same table, independently); `mcsim/crates/mcsim-link/src/predict.rs:112` (`snr_thresholds: [-7.5, -10.0, …]`), `:637` (`let link_margin = median_snr - snr_threshold;`), `:208-215` (`Excellent >10`, `Good >5`, `Marginal >0`, else `Unreliable`); host journal, 14 days: RSSI distribution peaks −84/−85 dBm with only 7 readings below −100, i.e. the host's own receivers are healthy and the range readings are the exception.
- Finding: at SF7 the demodulation floor is **−7.5 dB SNR**. The field readings are −6.75 (margin **+0.75 dB**) and −8.5 (margin **−1.0 dB**). By `mcsim`'s own classification that is `Marginal` at best and `Unreliable` at worst. Every protocol behaviour downstream — copies, retries, ladders, timers — is operating in the regime where a single packet is a coin toss, and no scheduling change moves that.
- Why it hurts at range: this is the baseline that makes every other finding worse. A retry policy tuned for a healthy link multiplies airtime without multiplying success; a policy tuned for this regime must assume most packets are lost and make each one as cheap and as short as possible.
- Proposed fix (NOT applied): none in radio code — the preset belongs to the mesh. What ldmux *should* do is compute this number and act on it: `link_margin = last_snr_db − snr_threshold[sf]` already exists in `crates/companion/src/codec.rs:1142` (`last_snr_db`) with the threshold table absent; adding the table costs nothing and gives every later decision a grading.
- Open questions: whether the −8.5 dB reading is a packet that passed CRC (possible at ~1 dB below the 1 %-PER point for a short packet) or a receive-log entry emitted before CRC validation — `MyMesh.cpp` `logRxRaw` ordering should be checked before treating that reading as a delivered packet.
- Investigator: physical round · 2026-11-11

### A-41 · physical: MeshCore delays **every flood at every repeater** by up to 32 s, so a crossing takes 45–58 s while our rung interval is 8–36 s · blocker

- Evidence: `filter-firmware/src/Dispatcher.cpp:55-57` (`int Dispatcher::calcRxDelay(float score, uint32_t air_time) { return (int)((pow(10, 0.85f - score) - 1.0) * air_time); }`); `:193-204` (`score = _radio->packetScore(_radio->getLastSNR(), len); air_time = _radio->getEstAirtimeFor(len);`); `:240-252` (`if (pkt->isRouteFlood()) { int _delay = calcRxDelay(score, air_time); … _mgr->queueInbound(pkt, futureMillis(_delay)); }`); `:11` (`#define MAX_RX_DELAY_MILLIS 32000`); `RadioLibWrappers.cpp:235-237` (`getTimeOnAir(len_bytes) / 1000`, i.e. **milliseconds**); `packages/mobile/…/MeshTermDriver.kt:2643-2646` (`answerDeadlineMs = baseMs + hops * 2_000L`).
- Finding: a flood is not forwarded on arrival. Each repeater computes a score from the SNR it measured and holds the packet for `(10^(0.85−score) − 1) × airtime`, capped at 32 s. At the measured margin the numbers are: a **133-byte** handshake record (airtime 490.0 ms, score 0.036) is held **2.70 s per repeater**; a **174-byte** full record (613 ms, score 0.024) is held **3.49 s**; a **48-byte** request (244 ms, score 0.061) is held **1.26 s**. Over a 14-repeater chain that is **44.7 s**, **57.5 s** and **21.0 s** of pure forwarding delay — before any packet even arrives. Our per-hop budget is a flat **2000 ms**, and the phone's first handshake rung with no route is `8 s` (`handshakeRetryFor(null) = 8000 + 0`, `MeshTermDriver.kt:2593`).
- Why it hurts at range: the rung interval is **shorter than one crossing**, so with no route the phone emits a fresh handshake every 8 s while a single flood is still walking the mesh. Six handshakes are in flight per crossing. Each carries a **new ephemeral**, so whichever answer finally arrives is, by construction, an answer to a handshake that has already been abandoned — which is exactly the shape `A-20`/`A-21` describe, and it is generated here in the timing rather than in the dispatch.
- Proposed fix (NOT applied): derive the per-hop term from the firmware's own function instead of a flat constant: `per_hop = airtime(next_record) + calc_rx_delay(score, airtime(next_record))`, with `score` from `packetScore(last_snr_db, len)`. Then `rung` and `answerDeadlineMs` are both the real crossing time. A flat average of ~3.2 s per hop at the margin is the correct order for a 133-byte record; 2.0 s is not.
- Open questions: whether a repeater's own `txdelay`/`calcRxDelay` is ever bypassed for high-priority routes (`ACTION_RETRANSMIT` at `Dispatcher.cpp:266-272` carries its own `_delay`, which may overlap the inbound delay and must not be double-counted); and whether the 32 s cap is reached in practice on this mesh.
- Investigator: physical round · 2026-11-11

### A-42 · physical: packet length is a first-class reliability term in MeshCore's own model, and every ldmux packet is built to the maximum · major

- Evidence: `filter-firmware/src/helpers/radiolib/RadioLibWrappers.cpp:319-328` (`success_rate_based_on_snr = (snr - threshold)/10.0; collision_penalty = 1 - (packet_len / 256.0); return clamp(success × collision_penalty)`) with `MeshCore.h:23` (`#define MAX_TRANS_UNIT 255`); `crates/mesh/src/constants.rs:24` (`RECORD_MAX = 172`), `packages/mobile/…/MeshRecordLimits.kt:34` (`MAX = 172`), `crates/protocol/src/mesh_term.rs:184` (`RECORD_PAYLOAD = 143`).
- Finding: the firmware scores a packet as `(snr − threshold)/10 × (1 − len/256)` — **length halves the success estimate across the legal range**. Computed at SNR −6.75 dB: 48 B → **0.061**, 133 B → **0.036**, 174 B → **0.024**. Even at a comfortable SNR (+5.75 dB) a full record is capped at **0.424** while a 42-byte one reaches **1.000**. ldmux builds data records to `RECORD_MAX` (172 → 174 on the air) and the handshake to 133, i.e. at the worst end of the term.
- Why it hurts at range: at the margin, the difference between a 48-byte and a 174-byte packet is a factor of **2.5 in per-attempt success at identical cost per attempt**, so the choice of record size is a bigger lever than the choice of retry count.
- Proposed fix (NOT applied): treat record length as a tunable on the marginal link rather than a constant — a `record_limit` derived from measured margin, the same way it is already derived from the path (`crates/daemon/src/mesh_serial.rs:3134` `record_limit(path)`), so a link at 0 dB margin hands the session ~100 bytes and a bench link gets the full 172.
- Open questions: the `collision_penalty` term models **collision**, not fade, so its applicability to a fade-limited link is an approximation — `mcsim` (`crates/mcsim-link`, ITM + DEM) is the instrument that could replace the heuristic with a real propagation model before this is relied on.
- Investigator: physical round · 2026-11-11

### A-43 · physical: the measured SNR is reported and never used, and the phone cannot display it · major

- Evidence: `crates/companion/src/codec.rs:1142-1143` (`pub fn last_snr_db(&self) -> f32`), `:1118` (`last_snr_quarter_db`); `crates/daemon/src/mesh_serial.rs:2209-2210`, `:2280-2281` (SNR/RSSI taken from every `0x88` log frame into `peer_signal`); `packages/mobile/…/MeshCarrier.kt:55-58` (`data class MeshStatus(… val rssiDbm: Int?, …)` — **no SNR, no noise floor**); `packages/mobile/…/MeshRadioSection.kt:657-664` (the "signal" line prints `rssi dBm` and volts only); `crates/daemon/src/mesh.rs:372` (`"noiseFloorDbm": self.noise_floor_dbm` — host JSON has it, the phone's model does not).
- Finding: SNR is the single quantity that says whether the link can work, it is measured on every received packet and on both ends, and **nothing branches on it**. Copies are always three, the reply delay is fixed, the rung interval ignores it, and the phone's own status struct has no field for it — so the operator cannot see it either.
- Why it hurts at range: every decision that should adapt (copies, retry cadence, record size, whether to tell the user "out of range") is instead a constant chosen for the worst case, which means a good link pays the marginal link's airtime and a dead link gets the same treatment as a marginal one.
- Proposed fix (NOT applied): add `snrDb` and `noiseFloorDbm` to `MeshStatus` (`MeshCarrier.kt:52-66`) and to the status JSON the phone parses; then gate `RECORD_FLOOD_COPIES` and the rung interval on `link_margin = snr − snr_threshold[sf]` — one copy above +5 dB, three at the margin, and a plain statement to the user below 0 dB.
- Open questions: how stale the last SNR is when no packet has arrived for minutes (a link with nothing heard has no margin reading at all, which is itself the answer).
- Investigator: physical round · 2026-11-11

### A-44 · physical: below the demodulation floor the firmware's own model says "no chance", and ldmux keeps retrying without saying so · major

- Evidence: `filter-firmware/src/helpers/radiolib/RadioLibWrappers.cpp:322` (`if (snr < snr_threshold[sf - 7]) return 0.0f; // Below threshold, no chance of success`); field readings `docs/MESH-RADIO-CONSIDERATIONS.md:13` (SNR to −6.75 dB and the phone's own log recording `-8.5 dB` at a 34-hop flood).
- Finding: at −8.5 dB SNR every length scores **0.000** in MeshCore's model. ldmux has no such gate: it schedules handshakes, route requests and floods on the same cadence whether the last heard packet was at +5 dB or −8.5 dB, and it reports "handshaking" in both cases.
- Why it hurts at range: a link that is physically out of range and a link that is marginal are indistinguishable in behaviour and in the UI, so the operator keeps waiting for something that will not happen, and the mesh pays for the retries. The honest message — "you are below the noise margin for this preset" — is available from data ldmux already has.
- Proposed fix (NOT applied): when `link_margin < 0` for a full rung interval, surface it (`MeshTermView` should gain a state distinct from `NoAnswer`) and reduce the cadence to the minimum that would still notice a return; keep retrying, but say what is happening.
- Open questions: whether a below-threshold reading should suppress the handshake entirely for a period, given that a moving user crosses back into coverage and would then be silent.
- Investigator: physical round · 2026-11-11

### A-45 · physical: a full record's airtime reference is 172 bytes but the packet on the air is 174 · minor

- Evidence: `crates/daemon/src/mesh_serial.rs:1585-1590` and `:1742` (`time_on_air_ms(…, RECORD_MAX)`); `crates/mesh/src/constants.rs:24` (`RECORD_MAX = 172`); `packages/mobile/…/AndroidRadioLink.kt:2035` (`rawPacketFor` builds `ByteArray(2 + path.bytes.size + record.size)` — header + path byte + path + record); `crates/companion/src/codec.rs:167` (`PACKET_MAX = 174`).
- Finding: the record is 172 bytes but the **PHY payload** the airtime formula is given should be `2 + path + record`. With no path that is 173, with a one-byte path 174. `time_on_air_ms(…, 172)` = 602.6 ms where the true worst case is 612.9 ms, so every figure derived from "one record's airtime" — the budget, `direct_round_trip`, `reply_delay`, `floodIntervalMs` — is low by **1.7 %**.
- Why it hurts at range: the error is small and in the safe direction for pacing, but it compounds into the hourly allowance and into every per-hop product at thirty-odd hops, and it makes the doc's 602.6 ms slightly wrong for the record it claims to price.
- Proposed fix (NOT applied): pass `RECORD_MAX + 2 + path_bytes` (or the actual record being sent) rather than `RECORD_MAX`, in `mesh_serial.rs:1585`, `:1742` and `MeshTerminalScreen.kt:619`.
- Open questions: none — this is arithmetic.
- Investigator: physical round · 2026-11-11

### A-46 · physical: the airtime meter over-counts every record by 1–3 bytes · minor

- Evidence: `packages/mobile/…/AndroidRadioLink.kt:1046` (`recordAirtime(packet.size + 3)` on the flood path, where `packet` is already the complete MeshCore packet); `:1105` (`recordAirtime(record.size + 2 + path.bytes.size + 1)` on the direct path); `Radio.kt:488-499` (`timeOnAirMs`).
- Finding: both metering sites add bytes that are not on the air. `packet.size` from `RadioRoute.recordFlood` already includes the header, path byte, hashes, marker and nonce; the `+3` is pure over-count. The direct path adds `+1` beyond `record + header + pathlen + path`. On the US preset the meter is display-only (`RadioAirtime.budgetMsPerHour` returns `null`, `Radio.kt:480-486`), so nothing is wrongly refused — but the published figure is wrong.
- Why it hurts at range: on a European sub-band preset this meter *does* gate sends (`remainingAirtimeMs`, `AndroidRadioLink.kt:1712-1727`), and there the over-count shortens the useful hour slightly on every record. It also means the reported `airtime` and any future "records left" disagree with the radio's own counter.
- Proposed fix (NOT applied): price exactly what the radio is handed — `packet.size` for the flood, `2 + path.bytes.size + record.size` for the direct — at both sites.
- Open questions: whether the `+3`/`+1` were meant to account for the LoRa CRC and preamble; the formula already includes both (`+16.0` for CRC in `Radio.kt:495`, and the preamble term at `:498`), so if that was the intent it is a double count.
- Investigator: physical round · 2026-11-11

### A-47 · physical: the reply delay ignores the measured score, and at the margin the firmware's own formula waits longer than we do · minor

- Evidence: `crates/daemon/src/mesh_serial.rs:524-529` (`reply_delay`: `500 + (nonce % (5 × 0.5 × airtime_ms))`); `crates/daemon/src/mesh_serial.rs:335` (`std::thread::sleep(reply_delay(airtime_ms))`); `filter-firmware/src/Dispatcher.cpp:55-57` (`calcRxDelay(score, air_time)`).
- Finding: our delay is `500 … 2006 ms` at this preset, independent of how marginal the packet was. The firmware's own rule is `(10^(0.85 − score) − 1) × airtime`, which at the measured margin gives **2703 ms** for a 133-byte request (score 0.036) and **1258 ms** for a 48-byte one (score 0.061), and **0 ms** for a strong short packet. So at the margin we are **~35 % too eager** and on a good link we are **500 ms too slow**.
- Why it hurts at range: the delay exists to let the request's echo and every still-forwarding repeater clear. Being 700 ms short of the firmware's own figure at the margin is exactly the collision this was written to avoid — the fix in `f413baf` moved in the right direction but stopped at a fixed factor rather than the scoring function its author was citing.
- Proposed fix (NOT applied): `reply_delay = calc_rx_delay(packet_score(last_snr_db, reply_len), airtime(reply_len))` with a floor, reused for both the host's reply and the phone's copy spacing (`requestCopyDelayMs`, which is currently `airtime(48) × 1.5` for every record — the subject of ledger `A-29`).
- Open questions: whether a host that answers a flood should model its own delay on the **request's** score (what the firmware does) or on the **reply's** expected score at the far end; the firmware uses the received packet's score, which is the measurement available.
- Investigator: physical round · 2026-11-11

### A-48 · physical: the radio's own transmit airtime is parsed and never compared with our estimate · minor

- Evidence: `crates/companion/src/codec.rs:1119-1123` (`pub tx_air_secs: u32`, `pub rx_air_secs: u32` — "The radio's own transmit airtime since boot"), `:1174-1175` (parsed); `crates/daemon/src/mesh.rs:978-979` (`s.radio_tx_secs = Some(reading.tx_air_secs)`); `crates/daemon/src/mesh_serial.rs:2531` (`tx_airtime_tenths_this_hour: …` — our own estimate); `crates/companion/src/codec.rs:90` asserts the purpose: "so `mesh status` says whose number each one is" and "the daemon's own meter has something to be checked against".
- Finding: the radio reports cumulative TX airtime in seconds as ground truth, the daemon computes its own estimate from record counts, and **the two are never compared** — `radio_tx_secs` is stored in status and read by no decision or display. The comment states the check exists; it does not.
- Why it hurts at range: an estimate that is never reconciled drifts silently. Tonight's findings A-45 and A-46 are exactly the sort of error a single comparison against the radio's counter would surface in one line ("radio says 41.2 s, we say 44.1 s").
- Proposed fix (NOT applied): publish both in `mesh status` (JSON already has `radioTxSecs` and `txAirtimeTenths`), and log one line when they diverge by more than a few percent over a session.
- Open questions: whether `tx_air_secs` counts only transmitted packets or also preambles/retries the firmware did internally (a firmware retry would appear in the radio's count and not ours).
- Investigator: physical round · 2026-11-11

### A-49 · physical: the two airtime implementations agree, constant for constant — recorded so later rounds need not re-audit it · minor

- Evidence: `crates/companion/src/codec.rs:1054-1079` (`time_on_air_ms`) vs `packages/mobile/…/Radio.kt:488-499` (`RadioAirtime.timeOnAirMs`): identical `tsym`, `de = symbol ≥ 16 ms`, `preamble = 32/16 by SF ≤ 8`, `numerator = 8·len − 4·SF + 28 + 16`, `denominator = 4·(SF − 2·de)`, `(preamble + 4.25 + payload_symbols) × tsym`. Ground truth: `filter-firmware/src/helpers/radiolib/RadioLibWrappers.h:128` (`preambleLengthForSF` → `sf <= 8 ? 32 : 16`); `meshcoreng/lib/RadioLib/src/modules/SX126x/SX126x_commands.cpp:184-200` (`ldroAuto` computes `symbolLength = 2^sf / bw_khz` and enables LDRO at `>= 16.0`, with `ldroAuto = true` by default at `SX126x.h:883`); `RadioLibWrappers.cpp:235-237` (firmware prices with RadioLib's own `getTimeOnAir`).
- Finding: **no fault.** The Kotlin and Rust implementations are byte-identical in structure, both match RadioLib's LoRa airtime formula, both use the firmware's own preamble rule, and both use RadioLib's exact LDRO threshold. Computed at 910.525/62.5/SF7/CR5: 42 B → 223.7 ms, 121 B → 459.3 ms, 133 B → 490.0 ms, 172 B → 602.6 ms, 174 B → 612.9 ms. Airtime grows with payload and with spreading factor as the crate's own test asserts (`codec.rs:1762`).
- Why it hurts at range: it does not. This entry exists only so a later round does not spend its budget re-deriving the formula, and so the corrections above (A-45, A-46) are made against a known-good function rather than a suspect one.
- Proposed fix (NOT applied): none.
- Open questions: whether the firmware's `getTimeOnAir` adds the sync word / header bytes in a way the datasheet formula omits — the two agree within rounding on every length tested, so the residual is under a millisecond.
- Investigator: physical round · 2026-11-11

### A-50 · physical: at the margin, ~50-byte records deliver the same bytes in ~24 % less air than 143-byte ones · major (design)

- Evidence: model and constants as A-42 (`RadioLibWrappers.cpp:319-328`, `RECORD_PAYLOAD = 143`, 100.9 ms fixed cost per packet: `time_on_air_ms(…, 0)`); computed from those.
- Finding: minimizing `packets × (1/score) × airtime` for 1000 bytes at SNR −6.75 dB gives a shallow optimum around a **50-byte PHY payload (85.2 s)**, against **112.1 s at a 143-byte payload (PHY 145)** and **108.9 s at 22 bytes**. So the current "pack it to the maximum" packing is ~**24 % more air** than a mid-sized one at the margin, and the very small packets are penalised by the 100.9 ms fixed cost.
- Why it hurts at range: every design decision that packs to `RECORD_PAYLOAD` (a repaint, a list page, a paste) is choosing the worst point of this curve for a marginal link, and the cost is paid in retries rather than in messages.
- Proposed fix (NOT applied): none yet — this is a *question to measure*, because the `collision_penalty` term is MeshCore's heuristic for collisions (A-42's open question) and a fade-limited channel may weight length differently. The concrete next step is to run the same computation in `mcsim`'s link model and see whether the optimum survives.
- Open questions: the shape of the curve under a real fade/ITM model; whether the session's 143-byte packing could be made adaptive on `link_margin` without a wire change (the record limit is already negotiated: `record_limit(path)`, `mesh_serial.rs:3134`).
- Investigator: physical round · 2026-11-11

### A-51 · physical: the two directions are not symmetric, and nothing measures which one is failing · question

- Evidence: host journal 2026-11-10/11: the host hears **other** nodes' adverts at −63 to −88 dBm (`route advert heard from 7aba4e02 hops=6 rssi=-73dBm`, `9aa06686 hops=8 rssi=-63dBm`, `eacc4434 hops=9 rssi=-87dBm`) while the phone at range recorded −107 to −124 dBm; the host logged the phone's `route request … hops=1` and `hops=16` in the same window (`journalctl … | grep 26f6071f`), and once `route flooded record 121 B from a 0 hop chain` and once `from a 14 hop chain`.
- Finding: the host's receiver is demonstrably healthy on other traffic at the same time the phone is at the floor — so the asymmetry is the phone's position, not the host's antenna. But ldmux reports signal as one number per end and never states *which direction* is failing, even though uplink (phone TX → host RX) and downlink (host TX → phone RX) are separate channels with separate margins.
- Why it hurts at range: the fix differs by direction — an uplink-limited link argues for more copies of the phone's records, a downlink-limited one for more copies of the host's — and the current symmetric policy spends both equally.
- Proposed fix (NOT applied): record, per direction, the SNR of the last record actually received (`peer_signal` on the host already carries it for the phone's floods, `mesh_serial.rs:2209`), and publish both in `mesh status` so "which way is broken" is answerable without a laptop.
- Open questions: how to attribute an SNR reading to a direction when a flood crossed N repeaters (the reading is of the last hop, not the whole path).
- Investigator: physical round · 2026-11-11

---


## Round summary (deepseek-flash, physical layer)

- **A-40**: the field link runs at ~**0 dB margin** (SF7 needs −7.5 dB SNR; readings were −6.75 and −8.5). Single packets are coin tosses; no scheduling change moves that.
- **A-41 is the headline**: MeshCore holds every flood at **every** repeater for `calcRxDelay(score, airtime)` — measured **2.7 s per repeater** for a 133 B record at the margin, so **14 hops ≈ 44.7 s** — while our per-hop budget is 2 s and the no-route rung is 8 s. The phone therefore re-handshakes six times per crossing, manufacturing the session mismatch in A-20/A-21.
- Airtime (910.525/62.5/SF7/CR5, preamble 32): 42 B 223.7 ms · 121 B 459.3 ms · 133 B 490.0 ms · 172 B 602.6 ms. Throughput optimum ≈50 B PHY payload (85.2 s/kB) vs 143 B (112.1 s/kB) — a 24 
## Round summary (deepseek-flash, physical layer)

- **A-40**: the field link runs at about **0 dB of margin** (SF7 needs −7.5 dB SNR; readings were −6.75 and −8.5). Single packets are coin tosses; no scheduling change moves that.
- **A-41 is the headline**: MeshCore holds every flood at **every** repeater for `calcRxDelay(score, airtime)` — measured **2.7 s per repeater** for a 133 B record at the margin, so **14 hops ≈ 44.7 s** — while our per-hop budget is 2 s and the no-route rung is 8 s. The phone therefore re-handshakes six times per crossing, manufacturing the session mismatch in A-20/A-21.
- Airtime (910.525 / 62.5 kHz / SF7 / CR5, preamble 32): 42 B 223.7 ms · 121 B 459.3 ms · 133 B 490.0 ms · 172 B 602.6 ms. Throughput optimum is about a 50 B payload (85.2 s per kB) against 143 B (112.1 s per kB) — a 24 percent penalty for large records at the margin.
- Undetermined: whether the −8.5 dB readings are CRC-valid; whether `ACTION_RETRANSMIT` stacks on `calcRxDelay`; host-side measured airtime (`tx_air_secs` exists and is never read).

### A-60 · conformance: unscoped floods are capped at 5 hops by default, and denied outright on a regional mesh · blocker

- Evidence: `mcrs/profiles/defaults.conf:21-22` (`flood.max.unscoped=5`, `flood.max.advert=3`); `mcrs/CONFIG.md:137-138` (same defaults, documented); `mcrs/repeater/src/app/regions.rs:152-158` — a `RouteType::Flood` (unscoped) is forwarded **only** `if self.wildcard().allows_flood()`, while `RouteType::TransportFlood` goes to `match_transport_region`; `mcrs/repeater/src/app/mod.rs:1404` (`Drop("unknown or denied flood region")`) and `:1419-1424` (`Advert && hop_count >= max_advert_hops` → `DoNotForward`; `unscoped && hop_count >= max_unscoped_hops` → `DoNotForward`); our runtime state: `ldmux mesh status --json` reports `scope: None`; `crates/daemon/src/mesh_serial.rs:652-699` (`ScopeChoice` resolves env pin → radio default → none, and stays unscoped deliberately); measured local mesh: journal 23:03–23:18 shows adverts arriving at **hops 5–12** at −63…−87 dBm.
- Finding: on any mesh configured the way the reference repeater ships, our traffic dies at **5 hops** (requests/records) and **3 hops** (adverts), because both are unscoped. Scoping wrongly is strictly worse: a flood whose transport code names a region the repeater does not carry is `Drop`ped at the **first** repeater (`mod.rs:1404`), not merely capped. Our local mesh is permissive (adverts survive to 12 hops), which is exactly why this looks like "works at home, fails at range".
- Why it hurts at range: the user's link is measured at **14–17 hops** (`route request from 26f6071f hops=16`, journal 22:20:44; `flooded record 121 B from a 0 hop chain` and earlier at 14). Against a default or regionalised mesh that distance is unreachable by either carrier, and the failure is silent — the packet is `DoNotForward`ed and nothing reports it.
- Proposed fix (NOT applied): make the scope a **first-class, discoverable setting** rather than an optional env pin — on both ends, read the radio's default scope (already implemented), and when it is absent, offer the operator the region list rather than silently staying unscoped. The phone already learns a scope from overheard scoped floods (`AndroidRadioLink.kt:639-660`); the host has the same machinery (`identify_scope`, `KNOWN_SCOPE_TAGS` at `crates/companion/src/codec.rs:394`) but no way to see that unscoped is costing it hops. A `question` for a later round: can we *detect* the cap empirically (e.g. our flood is heard by the peer at fewer hops than our advert's advertised reach) and say so?
- Open questions: whether the operator's mesh is regional (only they can answer); whether `flood.max.unscoped` is commonly raised in practice on city meshes; whether a scoped flood needs a wildcard region configured to also be forwarded by repeaters that only carry the named region.
- Investigator: conformance round · 2026-11-10

### A-61 · conformance: the host answers a route request by blocking its own receive loop for up to ~19 s · blocker

- Evidence: `crates/daemon/src/mesh_serial.rs:334` — `std::thread::sleep(reply_delay(airtime_ms))` inside `send_route_reply`, whose own comment says "This runs inside the push handler, before the record that arrived is delivered upward"; that function is called from `serve_loop` at `:2072`; `:487-506` — `send_retrying` does `SEND_ROUNDS = 3` attempts, each with `raw_send_deadline(900.0)` = 4 900 ms, plus `300·(1<<round)` ms and `200·(1<<round)` ms sleeps; `:529-534` (`reply_delay` jitter up to `5 × 0.5 × airtime` ≈ 1.5 s at this preset).
- Finding: answering one route request costs the daemon **0.5–1.8 s of sleep plus up to 3 × 4.9 s of send-with-deadline**, all on the thread that reads the radio. Worst case ≈ 17 s of `send_retrying` plus the reply delay, with no reception at all during it. The reference does the opposite: a repeater **enqueues** a forward with a delay (`mcrs/repeater/src/app/mod.rs:1310-1312`, `context.enqueue_forward(encoded, delay_ms, signature)`) and keeps receiving; MeshCore firmware's companion send is likewise queued, not blocking.
- Why it hurts at range: at range the phone is sending request copies **and** handshake records interleaved. While the host is composing and retrying one reply, every packet the radio pushes is unread; the radio's own buffer is small, so the handshake record — the one thing that would establish the link — is dropped rather than delayed. This is a plausible direct cause of "the host says `session waiting` while the phone says it is sending".
- Proposed fix (NOT applied): make the reply **asynchronous** — queue it on a timer (the same `reply_delay`) and let the receive loop continue; the outbox/pending-send structure already exists in `Companion`. Failing that, at minimum cap the blocking to the single send attempt and drop the retry-on-refusal for a *flooded reply* (a refused flood is better re-asked by the phone than waited out by the host).
- Open questions: how large the radio's RX buffer actually is (does a dropped read lose the packet or just delay it); whether `Event::Recv` delivery order matters to the session layer; whether the same blocking pattern exists on the phone side (`answerRequest`, `AndroidRadioLink.kt:1466`).
- Investigator: conformance round · 2026-11-10

### A-62 · conformance: our first handshake rung (8 s) is shorter than a 14-hop flood's forward window · major

- Evidence: `mcrs/repeater/src/app/mod.rs:1509-1518` — `flood_forwarding_delay_ms` = `airtime × (12 − tier) / 4`, jittered over `[0, window]`, so a **tier 0** repeater forwards a flood at a delay uniform in `[0, 3 × airtime]`; `mcrs/CONFIG.md:48-51` (tiers 0/2/5/10 ≈ 0.6, 0.5, 0.35, 0.1 windows, "CAD still checks for a busy channel"); `mcsim/crates/mcsim-model/src/properties/definitions.rs:211-245` — firmware **flood ack timeout 30 s**, `flood_attempts_no_path = 3`, `direct_ack_timeout_per_hop_s = 5.0`; ours: `packages/mobile/composeApp/src/commonMain/.../MeshTermDriver.kt:2643-2646` (`answerDeadlineMs = base + hops × 2000`) and `:2612` (`HANDSHAKE_FIRST_RETRY_MS = 8_000`), with `hops = null → 0` when there is no route.
- Finding: a flooded record at 14 hops needs, per hop, its own airtime plus a forward delay averaging 1.5 × airtime at tier 0 — ≈ 14 × (0.49 + 0.74) ≈ **17 s** one way for a 133-byte record at this preset, before the answer's return. Our ladder's first rung with no route is **8 s**, and the reference waits **30 s** for a single flood acknowledgement.
- Why it hurts at range: every rung retransmits while the previous copy is still being walked across the mesh. The retransmission competes with its own predecessor's forwards (CAD defers, or the two collide), so copies 2 and 3 of a rung are *less* independent than they look — the time diversity the copies exist for is partly cancelled.
- Proposed fix (NOT applied): derive the first rung from the mesh's **measured reach** where the route is unknown (`meshReach()`/`maxFloodHops` already exists and is already used by `floodIntervalMs` and `sessionHopsFor`), not from `hops = 0`. The reference figure for a flooded exchange is 30 s flat; ours should be at least `reach × airtime × 2.5` when `hops` is null.
- Open questions: whether tier 0 is the common tier on real meshes (mcrs ships it as default); whether the same `null → 0` flattening appears in the host's own answer waits.
- Investigator: conformance round · 2026-11-10

### A-63 · conformance: we wait 4 s for a route answer that needs ~14 s to cross · major

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1456-1457` (`replyWaitMs() = ROUTE_REPLY_WAIT_MS + ROUTE_REPLY_PER_HOP_MS × (advertPath ?: contactPath)?.hops() ?: 0`), `:2051-2052` (`4_000L`, `2_000L`); `:1545` (`meshReach() = maxFloodHops`, tracked at `:582`); the request copies at `:1683-1700` are floods with no path at all.
- Finding: the wait for a route answer is derived from the route **in hand** — and when the phone is asking, it has none, so the term is zero and the wait is the neighbour figure, **4 s**. The comment states the intent ("on a route the request itself already needs `2 × hops × airtime` to arrive and come back"), but the case that matters — no route — falls through to the flat 4 s. `maxFloodHops` already holds the right number and is not consulted here.
- Why it hurts at range: combined with A-62's forward window, a 14-hop request/reply exchange takes ~14 s. We stop listening at 4 s, conclude "no answer", send two more copies (multiplying the air), and then fall back to the free cache reads and a blind handshake. The answer does arrive — 10 s after we stopped caring — and is discarded as stale.
- Proposed fix (NOT applied): `replyWaitMs()` should use `(path?.hops() ?: meshReach())`, the same fallback `sessionHopsFor(routeHops, meshReach)` already uses for record timing.
- Open questions: whether a 30 s flat floor (the firmware's flood ack figure) is simpler and safer than the per-hop term; whether the phone's `ROUTE_REPLY_WAIT_MS` was chosen to bound UI latency rather than airtime.
- Investigator: conformance round · 2026-11-10

### A-64 · conformance: one route discovery costs ~84 repeater transmissions, ~4× the reference's duty · major

- Evidence: `AndroidRadioLink.kt:1683-1700` — `REQUEST_COPIES = 3` loop, and **the nonce is rebuilt per copy** (`RadioRoute.routeRequest(...)` inside the loop, comment: "The nonce is fresh per copy"); `crates/daemon/src/mesh_serial.rs:2070-2080` — the host answers each distinct nonce once (`first_time_question`), so **three requests produce three flooded replies**; `crates/daemon/src/mesh_serial.rs:302-320` (the reply is a flood, `build_route_reply_flood_in_scope`); airtime at this preset from `crates/companion/src/codec.rs:1054-1080` — 60 B = 275 ms, 74 B = 316 ms, 133 B = 490 ms.
- Finding: one "ask for a route" is **6 flooded packets** (3 up, 3 back), each re-flooded by every repeater in range. At 14 hops that is ≈ 6 × 14 ≈ **84 repeater transmissions ≈ 25 s of aggregate mesh airtime** (per-hop airtime grows 275→330 ms as the path grows), compressed into a few seconds. The reference policy (`definitions.rs:228-245`) is **3 flood attempts, one packet each, 30 s apart** — ≈ 42 transmissions spread over 90 s.
- Why it hurts at range: we are 2× the total transmissions at ~6× the instantaneous rate, on other people's repeaters, every 5/10/20 s while linkless (`ensureRoute`, `AndroidRadioLink.kt:1650+`). On a busy city mesh that is the profile a repeater's duty-cycle limit and CAD backoff exist to suppress, and it makes our own copies collide with each other.
- Proposed fix (NOT applied): drop `REQUEST_COPIES` to 1 and let the *time* diversity come from the ladder (which already exists), or keep 3 copies but make copies 2 and 3 **byte-identical** so repeaters dedupe them (see A-65 — deliberately not doing this is the reason the nonce is fresh). The cheapest correct shape is 1 request per rung, spaced by the reference's 30 s-class interval.
- Open questions: whether the host's 3 answers are useful at all when the phone stops listening at 4 s (A-63); whether `ANSWERED_NONCES` should instead answer only the *first* nonce of a burst.
- Investigator: conformance round · 2026-11-10

### A-65 · conformance: our copies are spaced 400 ms, inside every repeater's forward window · major

- Evidence: `AndroidRadioLink.kt:2005-2008` — `requestCopyDelayMs(preset) = airtime(48 bytes) × 1.5`, clamped `400…3000 ms`; applied at `:1028` to the **133-byte** handshake record and at `:1684` to the request; `mcrs/repeater/src/app/mod.rs:1509-1518` (forward delay up to `3 × airtime`), `:99-101` (`SEEN_PACKET_TTL_MS = 15_000`); `mcrs/CONFIG.md:48-51`.
- Finding: the inter-copy gap is derived from a **48-byte** packet (~244 ms → ~366 ms, clamped to 400) while the record actually being repeated is 133 B (490 ms) or 162 B (572 ms). Meanwhile a repeater that heard copy 1 is still inside its forward window (up to 3 × airtime ≈ 1.5 s for a 133 B record) and will transmit copy 1 *during* copy 2's airtime; CAD makes it defer, so the two copies are heard by disjoint sets of neighbours rather than by the same set twice.
- Why it hurts at range: the three copies exist for time diversity against a fade. Spacing them below both their own airtime **and** the mesh's forward window reduces them to one longer transmission with extra air.
- Proposed fix (NOT applied): derive the gap from the record being sent **and** the mesh's forward window — `airtime(record.len()) × 1.5` as a floor, and ideally beyond `3 × airtime` so the far end has finished forwarding before the next copy starts.
- Open questions: whether the firmware's own `txdelay` already produces this spacing at the repeater such that phone-side spacing is redundant; the measured gap between the first and last of our three copies arriving at the host (the journal shows one line per burst, so this needs a new log).
- Investigator: conformance round · 2026-11-10

### A-66 · conformance: an env-pinned scope desynchronises our adverts from our records · minor

- Evidence: `crates/daemon/src/mesh_serial.rs:621-699` — `ScopeChoice` takes `LDMUX_MESH_SCOPE` first, then the radio's default; adverts are sent with `CMD_SEND_SELF_ADVERT` (`mesh_serial.rs:3075`, `SEND_SELF_ADVERT` at `crates/companion/src/codec.rs:113`), so the **firmware** builds the packet and scopes it from `prefs.default_scope_key` only (`openhop_core/src/openhop_core/companion/base_config.py:371-380`, "bypassing both the transient send_scope override and the send_unscoped flag"); the phone does the same (`AndroidRadioLink.kt:1636`, `s.sendSelfAdvert(true)`).
- Finding: our records/replies are scoped by *our* resolution; adverts are scoped by the *radio's* default. When the two disagree — the env pin set, the radio's default empty — adverts go unscoped while records go scoped. On a mesh with `flood.max.advert = 3` an unscoped advert dies at 3 hops while a scoped record crosses the region.
- Why it hurts at range: adverts are one of the two ways the peer learns a path (the other is the travel path of a flooded record, which the host does use — `route from a flooded record hops=N`). Losing the advert half costs route convergence, which keeps us on the expensive flood carrier longer.
- Proposed fix (NOT applied): when the env pin disagrees with the radio's default, **write the scope to the radio** (the machinery exists: `build_default_flood_scope`, `crates/companion/src/codec.rs:363`), so adverts and records agree; or refuse the env pin with a warning that adverts cannot follow it.
- Open questions: whether writing the radio's default scope is acceptable for a shared host radio; whether the phone should do the same when it adopts a scope by hearsay.
- Investigator: conformance round · 2026-11-10

### A-67 · conformance: 1-byte path hashes with long paths are a collision lottery against loop detection · major

- Evidence: phone reports "path hashes 1 B" (field log, 2026-11-10); `mcrs/repeater/src/app/mod.rs:74-87` — `detects_loop` thresholds: `(Minimal, One)` = 4 matches, `(Minimal, Two)` = 2, `(Moderate, One)` = 2, else 1; `mcrs/repeater/src/app/mod.rs:1415-1417` (loop detection runs **before** hop limits and drops); `mcrs/CONFIG.md:140` (`set path.hash.mode` default `2` = **3-byte** hashes for this repeater's own paths); our path is 14–17 hops at range (journal 22:20:44, `hops=16`).
- Finding: a 1-byte hash gives 256 possible values; over a 14-hop path the chance that some hop's hash equals a given repeater's own hash is ≈ 14/256 ≈ 5.5 %. Under `minimal` (threshold 4) that is tolerated, but under `moderate` (threshold 2) or on a 2-byte path (threshold 2) a single collision is not, and a repeater that detects a "loop" **drops** the packet. Across ~14 independent repeaters the chance that at least one drops it is high enough to matter. With 2-byte hashes the same arithmetic gives ≈ 0.02 %.
- Why it hurts at range: long paths are exactly where collisions accumulate, so the failure appears only at range and only on the subset of repeaters configured `moderate`/`strict` — intermittent, unexplainable loss on a link whose signal is adequate.
- Proposed fix (NOT applied): evaluate 2-byte path hashes when the route is long — `preset.pathHashSize` is already a knob and `record_limit(path)` (`mesh_serial.rs:542-548`) already shrinks the record to fit the path, so the cost is ~14 bytes of payload on a 14-hop route in exchange for removing a probabilistic drop.
- Open questions: which loop-detection mode the local repeaters run (their protocol logs may say); whether the firmware's phase-1 reordering makes collisions worse than uniform; whether a 2-byte path is accepted by 1-byte-originating peers (MeshCore v1.14 forwarding is width-agnostic, per `crates/companion/src/codec.rs:514-517`).
- Investigator: conformance round · 2026-11-10

### A-68 · conformance: the payload version bits are never checked · minor

- Evidence: `crates/companion/src/codec.rs:520-527` — `parse_packet` takes `route = header & 0x03` and `payload_type = (header >> 2) & 0x0F` and never reads bits 6–7; the reference validates them: `openhop_core/src/openhop_core/protocol/packet.py:492-495` (`Unsupported packet version` raises), and `mcsim/crates/meshcore-packet/src/lib.rs:282-298` derives **hash size and MAC size from the version** (`V1` → 1/2, "future versions use 2-byte hashes").
- Finding: a packet with a non-zero payload version is parsed as if it were v1 — the hash size comes from the path byte (`encoded >> 6`), which is a v1 interpretation. The reference either refuses it or interprets it under v2 rules. We will misparse it and, worse, answer it.
- Why it hurts at range: nothing today, because no v2 packets exist. It is a forward-compatibility fault on a shared network whose other nodes are newer or older than us, and it would surface as inexplicable garbage rather than a clean drop.
- Proposed fix (NOT applied): read the version in `parse_packet` and return `None` for any value other than 0 (v1), as `openhop_core` does; note it in the `PacketView` so a later round can add v2 parsing in one place.
- Open questions: whether any firmware in the field emits a non-zero version for `REQ`/`RAW_CUSTOM`; whether the phone's `RadioRoute` parser has the same omission.
- Investigator: conformance round · 2026-11-10

### A-69 · conformance: airtime arithmetic and reply-scope match the reference — sound, with one documented divergence · minor

- Evidence: ours `crates/companion/src/codec.rs:1054-1080` vs `openhop_core/src/openhop_core/protocol/packet_utils.py:31-93` ("matching RadioLib's chip drivers"): identical numerator (`8·PL + 16`, i.e. `8PL − 4SF + 28 + 16`), identical denominator (`4·(SF − 2·DE)` with LDRO at ≥16 ms symbols), identical coding-rate convention (denominator 5–8 as the symbol multiplier), identical `0 → 1`, `0xFFFF → 0xFFFE` remap in `transport_code` (`codec.rs:323-336` vs `mcrs/repeater/src/app/regions.rs:214-233`), and `PayloadKind::Request => 0x00` (`mcrs/protocol/src/payload_kind.rs:41`) matching the payload type we hash.
  Reply scope: `crates/daemon/src/mesh_serial.rs:371-380` returns `None` when the frame carries no transport codes, which is exactly `chooseReplyScope`'s "unscoped flood → REPLY_SCOPE_NONE" (`openhop_core/src/openhop_core/protocol/region_map.py:149-165`).
- Finding: the parts of the protocol that are hardest to get right are right: time-on-air, the transport-code derivation, the request/reply scope mirroring. The one divergence is the preamble: we use 32 symbols below SF8 (`codec.rs:1063-1069`, citing `preambleLengthForSF`), the reference defaults to 8 (`packet_utils.py:51`). Ours is the more conservative figure; if it is wrong it inflates every airtime-derived timing by ≈160 ms per packet at this preset, which matters only for A-62/A-63's per-hop budgets.
- Why it hurts at range: it does not, today — this entry exists so later rounds do not "fix" a formula that already agrees with the reference.
- Proposed fix (NOT applied): none. Verify the 32-symbol preamble claim against firmware source before changing any timing that depends on it.
- Open questions: whether `preambleLengthForSF` really returns 32 at SF7, or 8 with a longer sync word (the two are easy to conflate, and the difference is 50 ms of air per packet at 62.5 kHz).
- Investigator: conformance round · 2026-11-10

### A-70 · conformance: listen-before-talk is the firmware's, and one of our two deadlines knows it while the other does not · minor

- Evidence: `openhop_core/src/openhop_core/hardware/usb_radio.py:126-151` — LBT defaults `lbt_max_wait_seconds = 4.0`, `lbt_retry_interval_ms = 200`, "bounded in TIME rather than attempts"; `mcrs/repeater/src/app/mod.rs:105-108` (`CAD_MAX_ATTEMPTS: 16`, `CAD_BUSY_BACKOFF_AIRTIME_DIVISOR: 2`, `CAD_BUSY_BACKOFF_SYMBOLS: 4`); ours: `crates/daemon/src/mesh_serial.rs:531-534` (`raw_send_deadline = 4_000 + airtime`, minimum 2 s — numerically the LBT budget, but with **no comment saying so**), and `AndroidRadioLink.kt:2064` (`SEND_TIMEOUT_MS = 10_000`), against `REQUEST_ANSWER_MS = 2_000` (`AndroidRadioLink.kt:2011`) for a route request's own send-ack.
- Finding: the radio, not the client, does listen-before-talk, and it may defer a transmission by up to 4 s. The host's send deadline accounts for that exactly (4 s + airtime). The phone's route-request loop waits only `REQUEST_ANSWER_MS = 2 000` ms for the same acknowledgement, which is **below the firmware's own LBT budget** — on a busy channel the radio is still waiting for a quiet slot when the phone has already given up and moved on.
- Why it hurts at range: at range the channel is busier (every repeater in a 25 km radius is forwarding), so LBT defers more often, so the 2 s route-request deadline is most likely to expire exactly where the link is hardest — and the phone counts a deferred send as a failed one.
- Proposed fix (NOT applied): raise `REQUEST_ANSWER_MS` to the firmware's own LBT budget plus one airtime (the host's `raw_send_deadline` is the model), and comment `raw_send_deadline`'s `4_000.0` as the LBT bound so the next reader does not "simplify" it.
- Open questions: whether `RESP_CODE_SENT`/`OK` is emitted before or after LBT (before would make 2 s adequate); how often the journal shows `route reply not sent` / `radio refused a raw send: err 3` on a busy mesh.
- Investigator: conformance round · 2026-11-10

---


## Round summary (deepseek-flash, conformance)

- **A-60 is a blocker and it is the shape of the whole complaint**: reference MeshCore ships `flood.max.unscoped=5` and `flood.max.advert=3`, and a regionalised repeater drops an unscoped flood at the **first** hop. Our traffic is unscoped, so on a default or regional mesh our range ceiling is **5 hops** while the measured link needs **14–17**. Our local mesh is permissive, which is exactly why it "works at home".
- Consequence: the protocol can be perfect and still not reach a regionalised mesh. Scope is a configuration/allocation problem, not a timing one, and it needs to be resolved before any further timing work.

### A-100 · attach: the list page buffer survives a session death, so runs merge · major
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTermDriver.kt:471` (`private var pages = ArrayList<SessionEntry>()`), `:2136` (`if (pages.isEmpty())` decides a run's meaning), `:2145` (`pages = ArrayList()` — the only clear, on a run's `last` page), `:765-806` (`failed()` clears `outgoing`, `handshake`, `session`, and calls `forgetMarkPage()` — but never `pages`), `:1445`/`:2114` (comments assert "a completed run empties it").
- Finding: the shells page buffer is cleared **only** when a run completes with a `last` page. A run interrupted by a session death (or an ARQ failure that ends the session) leaves its pages in the buffer forever, and the next run's pages are appended to them; `distinctBy { it.id }.take(MAX_SESSIONS)` then commits a list that is half an old run and half a new one. Mark pages get exactly the reset that shell pages lack (`forgetMarkPage()` at `:805`).
- Why it hurts at range: session replacement is common at range (a handshake arrives mid-transfer, A-20/A-23; ARQ gives up after five attempts). Each replacement therefore risks a launcher showing a stale, partial or duplicated row set — and the user's cure (refresh) starts yet another run appending to the same stale buffer.
- Proposed fix (NOT applied): clear `pages` in `failed()` and in `onGatewayState` for any non-bound state, exactly as `forgetMarkPage()` is called there; alternatively tag each page with the session index it arrived under and drop pages from an older one.
- Open questions: whether the host's `reply_upto` can retransmit the tail of a run across a session replacement at all (A-23 says the bytes are dropped), and whether `pagesAsked` should also be reset with the buffer.
- Investigator: attach round · 2026-11-11

### A-101 · attach: the list and attach deadlines are shorter than the route's own retransmission timer · major
- Evidence: `MeshTermDriver.kt:2643-2646` (`answerDeadlineMs = base + hops * 2_000`), `:2654` (`LIST_ANSWER_MS = 8_000`), `:2663` (`ANSWER_RETRY_MS = 6_000`); `crates/session/src/arq.rs:187` (`scaled = airtime * 6.0 * hops`), `:140` (`rto_initial_ms: 2_000`); measured per-hop flood hold from the physical round (A-41): **2.7 s** for a 133 B record.
- Finding: three different per-hop figures govern the same crossing. The ARQ times a first retransmission at **6 airtimes per hop** (2.94 s/hop at 490 ms), the phone's answer deadlines assume **2.0 s/hop**, and the measured repeater hold is **2.7 s/hop**. At 14 hops the phone waits `8 + 28 = 36 s` (list) and `6 + 28 = 34 s` (profile) for answers whose one-way delivery the ARQ itself prices at 41 s and whose round trip (record out, ack back) is ~76 s.
- Why it hurts at range: the phone re-asks before the answer can possibly have completed a round trip, so each re-ask starts a **second full answer run** whose records compete with the first. `LIST_ASKS`/`ANSWER_ASKS` = 3 makes that up to three overlapping runs — the multiplying airtime the user sees as a shell that never opens.
- Proposed fix (NOT applied): derive the answer deadlines from the same expression the ARQ uses (6 airtimes × hops, floored at the interactive 6 s), and make it at least twice that for a *paged* answer, since its ack returns over the same path. One function, one figure, used by both.
- Open questions: whether the phone knows `airtime_ms` at all times (`gateway.airtimeMs` exists for the radio), and whether the honest deadline should instead be "no page has arrived for X" (a progress deadline) rather than "the whole answer has not arrived by X".
- Investigator: attach round · 2026-11-11

### A-102 · attach: three flood copies per record, multiplied by the credit window, deafen the sender · blocker at range
- Evidence: `crates/daemon/src/mesh_serial.rs:513` (`RECORD_FLOOD_COPIES = 3`), `:1468` (the copies are sent back-to-back inside one command, no gap and no listen between them), `:2845` (`companion.next_push(Duration::from_millis(1))` — a 1 ms receive window between commands); `crates/daemon/src/mesh_session.rs:383-405` (`poll` returns up to `l2_slots` records), `crates/daemon/src/mesh.rs:1183` (`l2_slots = CREDIT_WINDOW - used`), `crates/mesh/src/constants.rs:14` (`CREDIT_WINDOW = 8`).
- Finding: when the path is not fresh, the host emits **every** record three times (`build_record_flood_in_scope` per copy, fresh nonce each), and the ARQ may hand over up to eight records per poll. One list answer of five records is therefore up to **fifteen consecutive transmissions** — about 7.4 s of continuous TX at 490 ms — during which the radio is half-duplex and the loop's receive window is 1 ms. The acknowledgements that would advance the window cannot be heard, so the ARQ's timer expires and the same records go again, three copies each.
- Why it hurts at range: the answer to a request deafens the host for the exact window in which the phone is acknowledging it. This is self-inflicted retransmission, and it scales with the answer's size — which is why opening a shell costs minutes while a single-record exchange costs seconds.
- Proposed fix (NOT applied): (a) one copy per record for *data*, with the ARQ's retransmission as the retry (copies exist to survive a fade on a control exchange, not to triplicate a stream), or (b) at minimum a listen gap of one airtime between copies, and a send budget that counts **copies** rather than records (see A-103).
- Open questions: whether the phone's acks are recorded as arriving at all during a burst (host journal has no per-copy arrival line — `route_log_rare` collapses the burst), and whether `RECORD_FLOOD_COPIES` was ever measured rather than reasoned (git log shows `fa4f4a7`, `c8c5d3b`, neither with a field number).
- Investigator: attach round · 2026-11-11

### A-103 · attach: the host's airtime meter counts one transmission for three copies · major
- Evidence: `mesh_serial.rs:1508` (`sent = sent.saturating_add(1)` — once per record, after up to three flood copies), `:2524-2532` (`status()` computes `tenths = sent * airtime_ms / 100` and `ops_left` from `left(budget, sent)`), `:2515-2519` (`left`).
- Finding: the budget and the airtime meter are both driven by a counter incremented once per *record*, while the radio transmits the record up to three times when flooding. At range — when flooding is the normal carrier — the host reports and charges **one third** of the air it actually spends, and on a duty-cycle band (`airtime_budget_ms`, EU presets) it would overspend the lawful allowance by the same factor.
- Why it hurts at range: the one number an operator can use to judge "is this link expensive" is wrong by the exact factor that matters at range, and the safety property for regulated presets (the budget never spends more than the allowance) silently fails while flooding.
- Proposed fix (NOT applied): increment `sent` per **transmission** — inside the copy loop — or add a separate `copies` count and make `status()` and `left()` use `transmissions = sent * copies`. Keep the record count too; both are useful.
- Open questions: whether the phone's `AirtimePolicy` has the same defect (it meters `ownedChannel` airtime; if it charges per record, its countdown is also a third), and whether the persisted window (`save_window`) should store transmissions.
- Investigator: attach round · 2026-11-11

### A-104 · attach: once the asks are spent, the screen waits for ever with a counter that never resets · major
- Evidence: `MeshTermDriver.kt:1197-1212` (the list retry: after `listAsks >= LIST_ASKS` it only sets `listingForHeader = false`; no state change, no verdict), `:1159-1176` (the same shape for an attach), `:371-372`/`:640` (`attempt` advances **only** in `connect()`, i.e. per handshake rung), `:2360` (`mirror.profile == null && Pick.Listing -> MeshTermView.Listing`), `MeshTerminalScreen.kt:1471` and `:1674-1697` (`WaitingLine` counts seconds keyed on `attemptNo`).
- Finding: when the list or the profile never arrives and the three asks are spent, the driver leaves the view in `Listing`/`ProfileMissing` for ever. Nothing changes on screen and `attempt` does not advance, because the session is still up — so the seconds counter climbs without limit while nothing is being asked. The operator's report ("it counted to 100 and then went back to 0") is this state, plus an eventual handshake rung resetting `attempt`.
- Why it hurts at range: the failure is indistinguishable from "still working", and the only cure is the refresh square, which the screen does not name while waiting. `NoAnswer` has a verdict and a sentence; a stalled *answer* has neither.
- Proposed fix (NOT applied): give the spent-asks state its own view (e.g. `NoAnswer`'s sibling: "the host has not answered the list in N tries — check the channel, then refresh"), and key `WaitingLine` on the ask count as well as `attempt`, so the counter reflects the retries that are actually happening.
- Open questions: whether `attempt` should advance per ask (it currently means "handshake rung"), and whether the UI should offer the retry directly rather than pointing at a square.
- Investigator: attach round · 2026-11-11

### A-105 · attach: the profile deadline is below one round trip on any real route · major
- Evidence: `MeshTermDriver.kt:2663` (`ANSWER_RETRY_MS = 6_000`), `:2643-2646` (adds 2 s/hop), `:1159` (used for the attach retry), `crates/session/src/arq.rs:187-190` (the ARQ's own first timer: 6 airtimes/hop).
- Finding: a `Profile` is one record out and one ack back, so its honest budget is a round trip — the ARQ prices that at 6 airtimes per hop, and the firmware's forwarding adds a hold per repeater (A-41). The phone instead waits `6 s + 2 s/hop`, which at 14 hops is 34 s: under the ARQ's own 41 s first retransmission, and far under a real round trip. The retry then fires while the first profile is still crossing, and up to three attaches are in flight.
- Why it hurts at range: each premature retry is an `Attach` record (three floods, a `claim`/`release` pair on the host, and a second `Profile` + marks record), so a slow open costs three times the air and leaves the host churning claims.
- Proposed fix (NOT applied): size the profile/created deadline from the same per-hop expression as A-101 (and from `gateway.airtimeMs`), so one function answers "how long may a one-record answer take over this route".
- Open questions: whether a retried `Attach` should be suppressed when an arriving `Commit`/`Rows` proves the session is alive (any inbound record is evidence the link carries).
- Investigator: attach round · 2026-11-11

### A-106 · attach: a retried attach makes the host release and re-claim the shell · minor
- Evidence: `crates/daemon/src/mesh_bridge.rs:1413` (`self.host.claim(one, takeover)`), `:1436-1439` (`if let Some(old) = self.shells.remove(&shell) && old.session != **one { self.release(&old.session).await }`), `:1540-1560` (the `Profile` and the marks are re-sent on every attach).
- Finding: each retried `Attach` re-enters `attach_with`, which releases the previous claim and claims again, and re-sends `Profile` + marks. Nothing is idempotent about it: the host does real work (two keeper round trips) and spends a record per retry.
- Why it hurts at range: retries are expected at range (A-105 makes them frequent), so the cheap case becomes a claim churn, and a desk window watching the same shell sees it released and taken again.
- Proposed fix (NOT applied): when the attach names the shell already attached on the same channel and the phone's version is unchanged, answer from the existing `Attached` (re-send `Profile` + a fresh `Commit` for a full frame) without releasing and re-claiming.
- Open questions: whether the re-release is load-bearing for the "reconnect that raced its own Detach" case the comment cites (it is, for a *different* shell; the same shell should be a no-op).
- Investigator: attach round · 2026-11-11

### A-107 · attach: a run that begins while the phone is listing is assumed to answer the list · minor
- Evidence: `MeshTermDriver.kt:2139` (`pagesAsked = listingForHeader || pick == Pick.Listing`).
- Finding: the meaning of a page run is decided from UI state at the run's first page. A `Sessions` the host pushes on its own (a shell ended or created by another client, `mesh_bridge.rs:2126` comment) that happens to begin while the phone is on the launcher is classified as the answer to the phone's list — and the `else` branch at `:2172-2186` then attaches a shell, moves `pick`, or empties the launcher, all on the strength of a pushed page.
- Why it hurts at range: at range a pushed list arrives seconds after the ask, so the two are easily confused; the phone can attach a shell the user never picked, which costs an attach record, a profile and a first screen (~6–9 records, A-108's arithmetic).
- Proposed fix (NOT applied): carry the ask's identity in the request/answer (a one-byte list nonce echoed in the page run's first flag byte), or treat a pushed run as authoritative for `shells` and never as a reason to attach.
- Open questions: whether the host can tell a pushed list from an asked-for one today (it cannot — `Message::Sessions` has no request token beyond `last`).
- Investigator: attach round · 2026-11-11

### A-108 · attach: what an open costs on the air, and why it is measured in minutes · major (arithmetic)
- Evidence: `crates/protocol/src/mesh_term.rs:180` (`MAX_ROWS_BODY = 134`), `:184` (`RECORD_PAYLOAD = 143`), `:1424-1470` (`session_pages` packs entries up to `MAX_ROWS_BODY`), `mesh_bridge.rs:858-866` (frames are compressed as one frame before splitting), `mesh_serial.rs:1468` (×3 flood copies); field state: 13 shells (`sessions parsed=13 attachable=13`, phone log 2026-11-10 20:45).
- Finding: an entry costs `2 + id + label + 1 + cwd + 1 + 1 + 1 + shell` — about 56 B for a 10-byte id, an 11-byte label and a 25-byte path — so 134 B holds **two** entries: **7 pages** for thirteen shells uncompressed, and roughly **3–4 records** once the frame compressor runs over the repeated ids and paths. An open is therefore list (3–4) + attach `Profile`/marks (1) + first paint (2–4 compressed) ≈ **6–9 records**, which as floods is **18–27 transmissions ≈ 9–13 s of air**. Each flood crossing 14 repeaters at the measured 2.7 s/hop is ~38 s one way, so with the credit window the wall clock is two crossings plus serialisation — **80–120 s with no losses** — and the ARQ's 41 s timer (A-101) fires before that round trip completes, so expect at least one full retransmission pass: **36–54 transmissions, 18–26 s of air**, and the host deaf while it sends (A-102).
- Why it hurts at range: this is the number behind "asking host for shell" running for minutes. It is not one bug; it is three copies × (list + profile + paint) × (one retransmission pass) on a link whose own budget assumes single transmissions.
- Proposed fix (NOT applied): none of its own; it prices A-102 (drop data copies), A-101 (one honest deadline) and the list-size work already in the tree (`MAX_CWD`, `MAX_LABEL` cuts, compression). Shrinking `MAX_CWD` from 60 to 40 alone moves a long-path entry under a third of a page in many cases.
- Open questions: the compressor's real ratio on a thirteen-entry list with distinct paths (the comment's 1.7×–8× figures are for screen text, not for a list), and whether a list could be sent as a *dictionary* of repeated path prefixes (client-side already has `MeshFrameDictionary`).
- Investigator: attach round · 2026-11-11

### A-109 · attach: a queued attach arms its retry even when the queue refused it · minor
- Evidence: `MeshTermDriver.kt:2221-2224` (`attachAskedAt = clock()` before the queue call), `:2236-2249` (`queue(...)` then `requestedFit` guarded on `QueueOutcome.Queued`).
- Finding: `attachAskedAt` is set unconditionally, but the record may never have been queued (`QueueOutcome.Full`). The retry then fires and asks again — which is benign — but `attachAsks` is consumed by attempts that were never transmitted, so the three allowed asks can be spent without a single `Attach` reaching the air.
- Why it hurts at range: the queue fills exactly when a large answer is still in flight (a list arriving, a paint pending), which is the moment an attach is most likely to be pressed — so at range the asks are spent on nothing and the state ends in A-104's permanent wait.
- Proposed fix (NOT applied): arm the retry only when the outcome is `Queued`, and let a refused queue leave the ask unspent (the tick already re-queues when there is something to send).
- Open questions: whether `QueueOutcome.Full` should surface as its own sentence ("the link is busy sending the list") rather than silent re-queueing.
- Investigator: attach round · 2026-11-11

### A-110 · attach: the radio attach has never been exercised end to end, so this whole aspect is unmeasured · question
- Evidence: host journal 2026-11-10 22:13–22:22 (the radio-only window): `advert heard from 26f6071f hops=0 rssi=-32dBm`, `route request … hops=0`, `route reply sent ok=true`, `flooded record 121 B from a 0 hop chain`, `session replaced … handshake refused: replayed or out-of-order timestamp`, `record 50 B sent by flood over 0 hop(s)` — and **no** `record … sent` line of a list-sized run. The only completed attach that evening (20:44–20:47) coincides with `remote: connection from 100.120.129.96:40718` at 20:45:32, i.e. the tailnet.
- Finding: every successful "shell opened" this audit can find was carried by the tailnet WebSocket, not the radio. The radio path has never delivered a list, a profile or a paint in the recorded journals; the field failures stop at the handshake. So the entries above are derived from code and arithmetic, not from a radio trace.
- Why it hurts at range: without one recorded radio attach there is no ground truth for any of these numbers — the per-record airtime, the crossing delay, the loss rate per record, and whether the retransmission pass actually happens.
- Proposed fix (NOT applied): instrument one open end to end — a single line per record sent and per record acknowledged, with the sequence number and the elapsed since first send — and capture one attach over the radio. That trace would settle A-101, A-102 and A-108 at once.
- Open questions: whether `route_log_rare` (which collapses repeated carrier lines) would hide that trace even if it ran, and where the trace should go so it survives a phone in a pocket (`files/mesh-log.txt` on the phone, journal on the host).
- Investigator: attach round · 2026-11-11

---


**Worst finding:** `A-102` combined with `A-108` — a pathless record goes out three times, the credit window lets eight records be handed over at once, and the receive window between them is 1 ms, so a list answer is 18–27 transmissions that deafen the host while the phone is acknowledging them; the ARQ's own timer (41 s at 14 hops) then expires before even one round trip completes (A-101), so the run repeats. That is minutes per open, by construction, not by bad luck.

**Arithmetic for an attach answer at 14 hops:** 6–9 records (list 3–4, profile+marks 1, first paint 2–4) → **18–27 flood transmissions ≈ 9–13 s of air**; crossing ~38 s one way at 2.7 s/hop, two crossings plus serialisation ≈ **80–120 s**; with the one retransmission pass the 41 s timer forces: **36–54 transmissions, 18–26 s of air**, and up to three overlapping runs if the phone's 34–36 s deadlines fire first.

### A-80 · session: the retransmission timer follows the *shortest inbound* path, which can be a ghost · blocker

- Evidence: `crates/daemon/src/mesh_serial.rs:404-436` (`prefer_route`: `if proven { return false } … candidate.hops < current.hops`), `:2036` (`max_flood_hops` learns only from route requests), `:2192-2204` (a flooded record sets `peer_path_proven = true` with *its own* chain), `:1877-1881` (`hops_now = peer_path…map_or(1, |p| p.hops)`); `crates/session/src/arq.rs:187-191` (`rto_initial = 6 × airtime × hops`); journal 2026-11-10: `22:20:33 route request from 26f6071f hops=1 (adopted)`, `22:20:44 … hops=16 (adopted)`, `22:21:20 … hops=1 (adopted)`, `22:21:42 … hops=1 (adopted)`.
- Finding: at range no record ever crosses **direct**, so `proven` is never set by the carrier that matters and every request burst re-adopts whatever path is shortest *inbound* — the journal flips between 1 and 16 hops within 11 s, and ends at 1. The session is then told `hops=1`, so `rto_initial = 6 × 0.490 s = 2.94 s` while one crossing of the real chain is ≈43 s (`calcRxDelay` arithmetic, A-41).
- Why it hurts at range: the first retransmission fires at 4.6 % of the true round trip, so every record is transmitted 15–20 times before its first copy can land; five attempts are exhausted in ≈12 s and the session dies — which is exactly "it handshakes and then nothing works". The shortest inbound path is also not evidence about the outbound one, and the code's own comment (`:427-430`) says so — the guard just does not apply while nothing is proven.
- Proposed fix (NOT applied): split the two quantities. The path used to *send* may stay the shortest that carries, but the path used to *time* must never be shortened by an inbound copy: take the maximum hop count seen from the peer's own traffic in the last stale window (the flood reach), and let the peer's handshake carry its own `meshReach`, which the phone already computes (`sessionHopsFor`, `MeshTermDriver.kt:2588`). Then `proven` should also be set by "this route has been carrying records and being acked", not only by a direct arrival.
- Open questions: whether the 1-hop copy is a genuine long single link or a path-byte artefact; whether the host should simply refuse a candidate shorter than half the current one absent any ack evidence.
- Investigator: session round · 2026-11-11

### A-81 · session: the first retransmission is designed below the real round trip at the margin · major

- Evidence: `crates/session/src/arq.rs:187` (`let scaled = (airtime * 6.0 * hops).round()`), `:191`; `mesh-research/clones/filter-firmware/src/Dispatcher.cpp:55` (`calcRxDelay = (pow(10, 0.85f - score) - 1.0) * air_time`), used at `:243`; score values from round 1 (48 B 0.061, 133 B 0.036).
- Finding: the timer budgets **one airtime per hop for the whole round trip**. Measured per-hop hold at the margin is `(10^(0.85−0.036) − 1) = 5.52` airtimes for a 133 B record and `5.15` for a 48 B ack, so a real round trip is ≈11 airtimes per hop, not 6. At hops=16 the timer is 47 s against a 63 s round trip — 74 % of the way there, i.e. every record retransmitted with nothing lost at all.
- Why it hurts at range: a duplicate transmitted before the original has crossed is not redundancy, it is collision on a half-duplex channel, and it spends the air the timer exists to save. This is the single largest avoidable airtime cost on a working link.
- Proposed fix (NOT applied): derive the first timeout from the repeater's own hold arithmetic rather than a flat factor — `rto_initial = hops × (hold(score_record) + hold(score_ack))`, with `hold` the firmware formula and the score from the last SNR. Keep MeshCore's `6 × airtime × hops` only as the floor for a healthy link where the margin is large.
- Open questions: whether the score used should be the last packet's SNR or a smoothed value; whether `calcRxDelay` is applied by every repeater in the chain or only by some firmware.
- Investigator: session round · 2026-11-11

### A-82 · session: the simulated air is five times faster than the real air, so timing regressions pass the tests · major

- Evidence: `crates/session/tests/lossy_multihop.rs:51-53` (`one_way_ms: hops * airtime_ms`), used at `:165` and `:206` with `603`; the two tests assert record counts and ordering, not elapsed time.
- Finding: the harness models one airtime of delay per repeater. The firmware's own hold is up to 5.5 airtimes at the margin, so the simulation is optimistic by that factor in every timing assertion. A timer that is 2× too short passes here and fails in the field.
- Why it hurts at range: every timing bug found in the field (2 s/poll bursts, the 30 s cap, the 6-airtime floor) was invisible to this harness, and there is no test that would have caught A-80 or A-81.
- Proposed fix (NOT applied): parameterise the harness by `(airtime, score)` and derive the per-hop delay from `calcRxDelay`; add a case at score 0.036 over 16 hops and assert that no record is retransmitted while its first copy is still in flight, and that total elapsed time stays within a stated bound.
- Open questions: whether a repeater's forward delay applies per hop compounded or sampled once per packet.
- Investigator: session round · 2026-11-11

### A-83 · session: the window is far below the bandwidth-delay product, so the ceiling is ~8 bytes a second · major

- Evidence: `crates/session/src/arq.rs:227-228` (`credit = (64.0 / hops).clamp(1.0, 8.0)`), `:53` (`MAX_WINDOW = CREDIT_WINDOW`), `crates/mesh/src/constants.rs:14` (`CREDIT_WINDOW = 8`), `crates/session/src/record.rs:24-30` (overhead 24 B), flood limit 162 B (`mesh_serial.rs:465`) ⇒ 133 B payload.
- Finding: at 16 hops the window is 4 records; with a 63 s round trip that is `4 × 133 / 63 ≈ 8.4 B/s`. A 600-byte body (`crates/protocol/src/mesh_term.rs:170`, `MAX_BODY = 600`) is five records, i.e. two windows ≈ 2 minutes. The comment justifies the division by chain occupancy, but the effect is that the link runs at a few percent of its own capacity and every exchange costs whole round trips.
- Why it hurts at range: at range every UI action — attach, list, page, paint — is a multiple of 63 s, and no timeout in the app is sized for that. This is the arithmetic behind "counts to 100 and resets".
- Proposed fix (NOT applied): none in the window itself (the L2 credit of 8 is the hard ceiling, and a shared mesh should not take more), but the UX and the protocol must be designed against ~8 B/s: fewer bytes per exchange (compress, one-record attach answer), and every wait derived from `ceil(bytes/133) × RTT`.
- Open questions: measured throughput at range from the phone's own counters; whether the credit window could be raised to 16 for a proven private route.
- Investigator: session round · 2026-11-11

### A-84 · session: recovery from a burst of loss is serialised, one record per timeout · major

- Evidence: `crates/session/src/arq.rs:820-880` (a single `front_due`, at most one retransmission per poll), `:665-690` (`fast_due` only ever set on `inflight.front_mut()`), no selective acknowledgement anywhere.
- Finding: only the oldest unacknowledged record can be retransmitted, and only one per poll. With window 4 and three consecutive records lost — which is what a fade does — recovery costs three full timeouts, i.e. ≈3 × 47 s at 16 hops.
- Why it hurts at range: bursts of loss are the normal case at the margin, so the ARQ's worst case is the field's typical case, and every extra timeout is also a window that cannot advance.
- Proposed fix (NOT applied): let a poll emit up to `l2_slots` retransmissions (ordering by sequence), and mark every slot at or below the peer's acked point + 1 as `fast_due` when a non-advancing ack proves a gap, rather than only the front.
- Open questions: whether MeshCore's mesh layer can even carry four records back to back from one node without the transmit table refusing (see A-89).
- Investigator: session round · 2026-11-11

### A-85 · session: the phone refuses to follow the route while anything is pending, which is always · major

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTermDriver.kt:883-890` (`followRoute`: `if (hops == sessionHops) return; if (health.pending > 0) return`).
- Finding: the timer is re-scaled only when the queue is empty. During any transfer — the exact time the route matters — a change from 1 to 16 hops never reaches the session, so the phone keeps a neighbour's timer on a long route. This is the failure `followRoute` was written to prevent.
- Why it hurts at range: a phone that walks out of range re-handshakes, gets the right hops, then loses them at the first packet in flight, and every record after that is retransmitted far too early.
- Proposed fix (NOT applied): apply the change to *new* slots only (per-slot timing in the ARQ), or update when hop count differs by more than one and nothing was sent for one RTO; the host already updates mid-session via `Event::Limit` (`mesh.rs:982`), so the asymmetry is unintended.
- Open questions: whether the guard exists to avoid re-splitting records mid-flight (that is `set_record_limit`, a different call).
- Investigator: session round · 2026-11-11

### A-86 · session: a flood's own chain is taken as proof of the timing distance · major

- Evidence: `crates/daemon/src/mesh_serial.rs:2192-2204` (`peer_path_proven = true`, candidate = the flood's chain), `:1877-1881` (`hops_now` from that path).
- Finding: a flood's path is how many repeaters it crossed **toward us**. Treating it as proof that the return route is the same length is optimistic on an asymmetric mesh, and it is the same mistake A-80 describes from the other side.
- Why it hurts at range: if the return chain is longer, the timer is short by exactly the asymmetry, and the window is sized for a chain half the length — silent, and only visible as duplicates and stalls.
- Proposed fix (NOT applied): keep the flood's chain as a *lower bound* and use `max(path, max_flood_hops)` for timing, which is what the phone already does (`sessionHopsFor`). The host is the less careful of the two ends here, which is the opposite of what one would expect.
- Open questions: how often the two directions differ in this mesh — an advert heard at 6 hops whose own path back is 16 would settle it.
- Investigator: session round · 2026-11-11

### A-87 · session: a zero-hop path publishes `hops = 0` and is then silently clamped · minor

- Evidence: `crates/daemon/src/mesh_serial.rs:1879` (`peer_path.as_ref().map_or(1, |p| p.hops)` — the `1` is the *absence* case, a present path of 0 passes through), `crates/session/src/session.rs:123` (`self.hops = hops.max(1)`).
- Finding: "no path" and "a path of zero hops" are conflated at the publish site, and the clamp happens two layers away.
- Why it hurts at range: harmless in effect, but it means a log line cannot distinguish a neighbour from an unknown, and the timing hops are then decided by a clamp rather than by evidence.
- Proposed fix (NOT applied): clamp at the publish site and log `hops=0 (direct)` distinctly.
- Investigator: session round · 2026-11-11

### A-88 · session: retransmissions are exempt from the duty-cycle budget, so the software can overspend the meter it publishes · major

- Evidence: `crates/session/src/arq.rs:34-39` (module docs: retransmissions, acks and the close are not charged), `crates/daemon/src/mesh.rs:795-808` (`poll_budget`, the 1 % countdown), `mesh.rs:1041` (`records_left = status.ops_left_this_hour`).
- Finding: only new data is charged. With A-81 causing a retransmission on nearly every record, the operator's 1 % countdown can be exceeded by a factor of two, and the radio then refuses (the firmware's own limiter is a 50 % duty cycle, per the same comment) — after `REFUSAL_DEADLINE_MS` the session dies as `RadioRefused`.
- Why it hurts at range: the failure arrives exactly when the link is worst, and it is self-inflicted: the software reports a budget it is not honouring.
- Proposed fix (NOT applied): charge retransmissions against the same counter (the exemption was introduced so a spent hour would not kill a live exchange — that can be served by a small reserve instead), and print the spend including them.
- Open questions: whether any operator-facing number currently claims compliance.
- Investigator: session round · 2026-11-11

### A-89 · session: a refusal rewinds the attempt counter, so an unbuildable record is hammered for 30 s · minor

- Evidence: `crates/session/src/arq.rs:705-730` (`on_refused`: `attempts = attempts.saturating_sub(1).max(1)`), `:333-343` (`REFUSAL_DEADLINE_MS = 30_000`), `:869-878` (deadline check inside `poll`).
- Finding: every refusal is treated as "not yet", so `too long` — a permanent, local fault — is retried until the wall-clock deadline. The carrier already distinguishes refusal kinds (`resizeToLink`, `ff625f3`), but the ARQ does not.
- Why it hurts at range: a route that grows mid-flight makes a record too long, and instead of one resize the session spends 30 s of poll cycles and then fails with a name that blames the radio.
- Proposed fix (NOT applied): pass the refusal kind into `on_refused`; `TooLong` re-sizes and re-offers at once, `Busy` keeps the 350 ms pace.
- Investigator: session round · 2026-11-11

### A-90 · session: three flooded copies are one ARQ transmission, so the ARQ's own accounting is wrong · minor

- Evidence: `crates/daemon/src/mesh_serial.rs:513` (`RECORD_FLOOD_COPIES = 3`), `:1468`; `crates/session/src/arq.rs` `Slot.attempts` increments once per `poll` decision.
- Finding: a record sent by the flood carrier occupies up to 3 × 490 ms of air, but the ARQ records one attempt and one record against the budget, and its RTO starts from the first copy's send time rather than from the last copy's.
- Why it hurts at range: the timer is up to two copy-spacings early on the flood carrier, and the spend reports 1 where the air took 3.
- Proposed fix (NOT applied): have the carrier report copies per send and let the ARQ stamp `sent_at` at the last copy and charge the copies to the meter.
- Investigator: session round · 2026-11-11

### A-91 · session: the parts that are correct, so a later pass does not "fix" them · question

- Evidence: `arq.rs:1-45` (module rules), `:629-700` (`apply_ack`, Karn's rule, non-advancing ack as evidence), `:540-560` (duplicates mark an ack owed, not a reply each), `:268-275` (`mark` defeating MeshCore's packet-hash dedup), `:559-575` (`resize_pending` merges losslessly), `:748-760` (`requeue_remainder`).
- Finding: cumulative acks, one-mark-per-transmission, Karn's rule, the refused-vs-lost distinction, the lossless re-split after a record-size change, and refusing rather than growing a queue are all correct and well argued. The failure at range is in *which numbers feed the timers*, not in the mechanism.
- Why it hurts at range: nothing directly — this entry exists so the register does not turn into a rewrite of sound code, which is the cheapest way to lose the properties above.
- Proposed fix (NOT applied): none. Every change above (A-80…A-90) should be made inside these rules, not by replacing them.
- Investigator: session round · 2026-11-11

---

109:**Worst finding:** A-80 — at range nothing is ever "proven", so `prefer_route` keeps adopting the shortest *inbound* copy; the journal shows the host flipping 1 ↔ 16 hops inside 11 s and settling on 1, which sets the first retransmission to 2.94 s against a 43 s crossing. Every record is then duplicated ~15 times and five attempts are exhausted in ~12 s, so the session dies while both radios are healthy. It is the same class as A-20: a rule that is right when the evidence is trustworthy,

### A-120 · carrier: the two "sticky flood" flags form the mutual latch ADR-0038 rejects · blocker
- Evidence: `AndroidRadioLink.kt:1013` (`floodWanted = (!directWorks || directSilent) && floodTokens > 0`), `:531` (`directWorks = true` only on a **direct** arrival), `:594` (`directWorks = false` on **any flood** arrival); `mesh_serial.rs:1285`/`:2208`/`:2278` (`peer_via_flood` set by a flood, cleared only by a direct arrival), `:1413-1425` (`want_flood` includes it); ADR-0038 `docs/adr/0038-carrier-selection-escalate-on-silence.md` ("**any** record arrives → failures = 0 → **direct**", "Rejected: mirroring the carrier — positive feedback").
- Finding: ADR-0038 says decay happens on *any* arrival; the code implements decay on a *direct* arrival only, on both ends. Once either end has heard one flood, it refuses to send direct, so the other end never hears direct, so neither flag is ever cleared. The latch is absorbing. It is escapable in exactly two ways: the phone's bucket running dry (`floodTokens == 0` falls through to the direct path at `:1057`), or a zero-hop host direct that happens to reach.
- Why it hurts at range: at 14–17 hops a zero-hop direct packet reaches only neighbours, so the only escape is a bucket-dry direct send — one attempt per ~3 records — and it must itself cross the chain. Until it does, **every** record on both sides rides the flood carrier with three copies each. This is the mirroring loop the ADR rejects, deduced from the two flags rather than stated anywhere.
- Proposed fix (NOT applied): make the decay rule what the ADR says — *any* arrival from the peer clears the escalation — and keep one bounded exception: a periodic cheap direct probe (one record per N records, not per bucket exhaustion) so the path carrier is re-tested deliberately rather than by accident.
- Open questions: was the intent that `directWorks` mean "the path in hand has carried", i.e. it should also be cleared when the path *changes* (a new path is unproven by definition)? 
- Investigator: carrier round · 2026-11-11

### A-121 · carrier: `6838508`'s session gate was removed by `ce0e600`, and the comment still claims it · major
- Evidence: `git log -S "!sessionUp && directSilent"` → `6838508 fix(mesh): carrier silence is not evidence while a session is up`; `git show ce0e600` → `- val floodWanted = !sessionUp && directSilent && floodTokens > 0` / `+ val floodWanted = (!directWorks || directSilent) && floodTokens > 0`; the surviving comment at `AndroidRadioLink.kt:993-1012` still says "**Only while there is no session.** Carrier silence is not evidence of failure when the peer simply has nothing to say … measured 2026-11-10 a healthy link sent a 43-byte record as a flood for that reason".
- Finding: the gate that commit removed was the subject of an earlier fix, and the comment describing it was left in place, so the code and its own explanation now disagree. A healthy session floods its data whenever the last inbound happened to arrive by flood.
- Why it hurts at range: it removes the only thing that limited flood traffic during a live session, and it makes the source lie to the next reader — the same class of fault as the `handshake 4/3` counter.
- Proposed fix (NOT applied): decide which rule is intended. If the session gate is wanted back, restore it as `(!sessionUp || !directWorks)`-style logic with the bucket; if not, delete the stale comment. Leaving both is the worst option.
- Open questions: whether the three-copies-per-record traffic measured on 2026-11-10 at 22:18–22:22 (every rung `1/3, 2/3, 3/3`) is entirely pre-session, or whether data records also flooded.
- Investigator: carrier round · 2026-11-11

### A-122 · carrier: the flood carrier's steady-state throughput is one record per ~58 s, and the host has no bucket at all · blocker
- Evidence: `AndroidRadioLink.kt:2022` (`FLOOD_BURST = 4`), `:298-301` (`floodIntervalMs = 2 × reach × airtime(172)`, clamp 5–90 s), `:1027`/`:1038` (up to `RECORD_FLOOD_COPIES` tokens spent per record); `mesh_serial.rs` has **no** token bucket (grep for `token|bucket|FLOOD_BURST` returns only unrelated matches), only `budget_per_hour`, which is `None` on US frequencies (`:3036`).
- Finding: at a measured reach of 16 hops the refill is 2 × 16 × 602.6 ms = **19.3 s per token**, and a record costs three tokens, so sustained flood throughput is **one 143-byte record per 58 s ≈ 2.5 B/s**. A 2 KB repaint (14 records) is therefore **≈13.5 minutes** on the flood carrier. The host side is unbounded by comparison: it emits three flood copies per inbound flooded record (`:1466-1485`) with no pacing whatsoever.
- Why it hurts at range: this is the arithmetic behind "it just counts and resets". It also makes the two ends' airtime discipline asymmetric — the phone is throttled to 2.5 B/s while the host can transmit freely, so the shared mesh sees the host's side of the conversation at ~3× the phone's rate.
- Proposed fix (NOT applied): give the host the same bucket (it is the end with no limiter), and price the bucket per **byte** rather than per record so a short handshake is not charged like a full screen.
- Open questions: whether `floodIntervalMs` should use the record actually being sent rather than a fixed 172 bytes (it currently under-charges short records and over-charges none).
- Investigator: carrier round · 2026-11-11

### A-123 · carrier: the host's three flood copies go out back to back, so they buy no time diversity · major
- Evidence: `mesh_serial.rs:1468-1484` — `for _ in 0..RECORD_FLOOD_COPIES { … send_retrying(|d| companion.send_packet_within(&packet, d)) }` with no delay between iterations; contrast the phone's `:1028` `if (copy > 0) delay(requestCopyDelayMs(currentPreset()))`, and `:1425`'s own comment that the firmware acknowledges a send **before** it transmits.
- Finding: the host's three copies are queued into the radio's transmit queue immediately, so they occupy the channel contiguously. At ~0.49 s of air per 133-byte copy that is ~1.5 s of continuous transmission — one fade longer than 1.5 s kills all three. The phone spaces its copies; the host does not.
- Why it hurts at range: three copies exist for time diversity against a margin fade (A-40 measured margin ≈ 0 dB). Unexpanded, they are 3× the airtime for roughly 1× the reliability — the worst trade available.
- Proposed fix (NOT applied): space the host's copies by one airtime of the packet being sent (the same derivation the phone uses), and make it a shared helper so the two ends cannot drift.
- Open questions: whether `send_packet_within` returns before or after the firmware's own `txdelay`, which decides whether a host-side delay would even separate them.
- Investigator: carrier round · 2026-11-11

### A-124 · carrier: `send_retrying` retransmits identical bytes, which repeater dedup swallows · major
- Evidence: `mesh_serial.rs:487-506` — three rounds, each calling `attempt(deadline)` with the **same** `packet`; called at `:1479` inside the flood copy loop; the dedup rule (`SHA256(payload_type ‖ payload)`, 160-entry FIFO) is recorded in `docs/MESH-RADIO-CONSIDERATIONS.md` and is why every flood copy uses a fresh `route_nonce()` (`:1471`).
- Finding: the code knows a repeat needs a fresh nonce — the copy loop does exactly that — but `send_retrying`, which fires on `Disabled` and `Timeout`, resends the identical packet. For `Disabled` (never transmitted) that is right; for `Timeout` (possibly transmitted) the retry is silently dropped by the first repeater that already saw it.
- Why it hurts at range: `Timeout` is the common case on a crowded channel, so the retry ladder's most-used branch is the one that cannot work. It burns the deadline (4 s + airtime per round) without buying air.
- Proposed fix (NOT applied): rebuild the packet with a fresh nonce for each `Timeout` retry (only the `Disabled` case may reuse it), or drop the `Timeout` branch entirely and let the ARQ/copies provide redundancy.
- Open questions: whether the radio's `Timeout` ever means "not transmitted" in practice, which would justify keeping one identical retry.
- Investigator: carrier round · 2026-11-11

### A-125 · carrier: adopting a route commits the link to direct for ~58 s, which is most of the attach window · major
- Evidence: `mesh_serial.rs:2029-2033` (adoption sets `peer_path_heard = Some(Instant::now())`), `:1388-1392` (`path_stale` false until `route_stale(hops)`), `:1401-1408` (`direct_silent` needs `direct_round_trip(hops, airtime)` = 6 × airtime × hops), `:536-541`; journal `22:20:44 route request from 26f6071f hops=16 (adopted)`.
- Finding: adoption marks the path fresh, so the next sends go **direct**, and the verdict "this direct chain did not carry" is not available until 6 × 0.603 s × 16 = **57.8 s** later. With the phone's attach wait of ~100 s (`handshakeWaitMs`/attach budget), only one escalation fits inside a single attempt.
- Why it hurts at range: it converts a route-repair success into a minute of silence on a chain that may be 16 repeaters long. The user sees "asking host for shell" counting down exactly once, which is the reported symptom.
- Proposed fix (NOT applied): judge direct silence against the **carrier actually used last** and against the mesh's measured reach rather than the adopted hop count, or allow one flood in parallel with the first direct attempt when the path is long and unproven.
- Open questions: whether `direct_round_trip` should include the repeater hold delay measured in A-41 (≈2.7 s/hop), which would make the verdict *later*, not earlier — i.e. whether the real fix is instead to flood first when `hops` exceeds some threshold.
- Investigator: carrier round · 2026-11-11

### A-126 · carrier: path churn every request burst — a 16-hop chain displaced a 1-hop chain 11 s later · major
- Evidence: `mesh_serial.rs:2014-2026` (`dead = !previous_burst`, `REQUEST_BURST_MS = 10 s`), `:404-434` (`prefer_route`: `dead || stale → return true`, bypassing both the proven-route protection and the shortest-wins rule); journal `22:20:33 hops=1 (adopted)`, `22:20:44 hops=16 (adopted)`, `22:21:20 hops=1 (adopted)`, `22:21:42 hops=1 (adopted)`.
- Finding: eleven seconds apart, the adopted path moved from 1 hop to 16 and back. Because `dead` is true for the first copy of every burst, *any* candidate wins at that moment, so the path is effectively whichever copy arrives first after the 10-second window.
- Why it hurts at range: each replacement changes both the carrier decision and the silence verdict (A-125). The link's idea of its own route flaps on a timescale shorter than one exchange.
- Proposed fix (NOT applied): keep `dead` only as "the route in hand has been silent for its own round trip", not "a burst window expired", so a burst cannot displace a healthy path on recency alone.
- Open questions: whether the 1-hop and 16-hop arrivals are the same physical route (a 1-hop path via one repeater that both ends hear) — the RSSI/hop pairs in the journal suggest two distinct chains and it is not decidable from the log.
- Investigator: carrier round · 2026-11-11

### A-127 · carrier: `route_stale` hardcodes a 603 ms airtime · minor
- Evidence: `mesh_serial.rs:166-169` (`Duration::from_millis(2 * 603 * hops.max(1))`), against `:1110` (`airtime_ms = self_info.time_on_air_ms(RECORD_MAX)`) which is the derived value used everywhere else.
- Finding: the only flat airtime in the radio path. At the public preset it coincides with the derived value (602.6 ms); at a slow preset (SF12, ≈7.3 s) it under-states staleness by ~12× and at a fast one it over-states it.
- Why it hurts at range: a route judged stale too late keeps a dead chain in use; too early and a working route is replaced needlessly. Both are carrier decisions driven by a constant that ignores the preset the link is actually running.
- Proposed fix (NOT applied): pass `settings.airtime_ms` into `route_stale`, as `direct_round_trip` already does.
- Open questions: whether the 60 s floor should also be derived (`2 × hops × airtime` with no floor) for meshes slower than the public preset.
- Investigator: carrier round · 2026-11-11

### A-128 · carrier: `heardByFlood` is written and never read · minor
- Evidence: `AndroidRadioLink.kt:233` (declaration), `:537` (`= false` on a direct arrival), `:593` (`= true` on a flood arrival); no other reference in the file.
- Finding: dead state left by the abandoned "reply on the carrier you were heard on" rule that ADR-0038 rejects.
- Why it hurts at range: it is an invitation to reintroduce the loop — the next reader sees a flag named exactly like the rejected policy and may wire it up.
- Proposed fix (NOT applied): delete it.
- Open questions: none.
- Investigator: carrier round · 2026-11-11

### A-129 · carrier: carrier decisions are logged at most once a minute, so a session's carrier history is invisible · major
- Evidence: `mesh_serial.rs:217-222` (`route_log_rare`: one line per 60 s per slot), used for the carrier line (`:1496-1505`), the flooded-record line (`:2174`) and the route-from-flood line (`:2197`); `journalctl --user -u ldmux --since "2026-11-10 18:00"` contains exactly **two** carrier lines for the whole evening (`sent by flood over 1 hop`, `sent by flood over 0 hop`).
- Finding: the single most important fact about a radio session — which carrier each record rode — is rate-limited to one line a minute across the entire evening.
- Why it hurts at range: every conclusion in this audit about carrier behaviour has had to be derived from code rather than measured, because the measurement does not exist. The phone has per-record carrier lines (`record by flood 1/3 …`); the host does not.
- Proposed fix (NOT applied): per-record carrier at trace level (`LDMUX_MESH_TRACE`), plus a **counter** (`records_by_flood`, `records_by_direct`, bytes each) published in `ldmux mesh status` and logged once a session — the summary is what a field report needs.
- Open questions: whether a counter is enough or whether the phone should also publish its carrier counts over the session so both ends' views can be compared.
- Investigator: carrier round · 2026-11-11

### A-130 · carrier: `directWorks` survives reconnects because links are cached per address · minor
- Evidence: `AndroidRadioLink.kt:2085-2102` (`RadioLinks.acquire` returns the cached link and only calls `updateHostKey`), `:237` (`directWorks` is instance state, never reset on connect/disconnect).
- Finding: a new connection inherits the previous connection's opinion of the direct carrier. A stale `true` sends the first records direct with no path (they die silently); a stale `false` keeps a fresh link on the flood carrier.
- Why it hurts at range: it adds a second, invisible source of carrier choice that depends on session history rather than on the link in front of you.
- Proposed fix (NOT applied): reset `directWorks`, `lastDirectAt`, `lastInboundAt` and the bucket in `updateHostKey`/on a new session, so a link starts with no opinion.
- Open questions: whether `RadioLinks` caching is needed for a reason that would preclude resetting on `updateHostKey`.
- Investigator: carrier round · 2026-11-11

### A-131 · carrier: the aggregate airtime one exchange costs on a 14-hop flood · question (arithmetic)
- Evidence: `mesh_serial.rs:513` (`RECORD_FLOOD_COPIES = 3`), `:1468` (3 copies), `AndroidRadioLink.kt:2026` (3 copies), airtime ≈0.49 s per 133-byte transmission at the public preset, 14 repeaters from `docs/MESH-RADIO-CONSIDERATIONS.md` and the journal's 14–17 hop readings.
- Finding: one record each way by flood = 6 origin transmissions × 15 nodes ≈ 90 transmissions × 0.49 s ≈ **44 s of aggregate channel time** for a single request/response pair. A handshake rung (3 × msg1 + 3 × msg2) is ~88 s of channel time before any data.
- Why it hurts at range: on a shared city mesh this is the number that matters, and it is not computed or logged anywhere.
- Proposed fix (NOT applied): none of its own — this exists so later rounds and any future change can be priced. The reducible parts are A-123 (no spacing), A-124 (dead retries) and the copy count itself.
- Open questions: the measured value, which needs the host's `tx_air_secs` (`codec.rs:1120`, currently never read) surfaced in the journal.
- Investigator: carrier round · 2026-11-11

---

101:**Worst finding:** A-120 — the two sticky flags (`directWorks` on the phone, `peer_via_flood` on the host) make flooding self-sustaining, because the only thing that clears either is a direct record and neither end will send one while its flag is set. ADR-0038's decay rule ("any arrival") was implemented as "a direct arrival", which is precisely the mirroring loop the ADR rejected.

### A-140 · batch: a screen is compressed, the *uplink* is not — a paste has no frame at all · major

- Evidence: `crates/protocol/src/mesh_term.rs:552` and `:565` (`Frame`/`FrameDict` are "Host to phone only"); there is no phone→host counterpart in the message enum (`mesh_term.rs:337-520`); `mesh_term.rs:191` (`MAX_INPUT = 137`, one record); `packages/mobile/.../mesh/MeshInput.kt:56` (`maxBytes = MeshTerm.MAX_INPUT`); `MeshInput.kt:248` (`CAPACITY = 65_536`); compression exists only in `crates/daemon/src/mesh_bridge.rs:739-830` (`pack_frame`/`frame_run`, applied to a reply on the host side). Measured with the real `FRAME_DICTIONARY` (extracted from `mesh_term.rs:586`) and raw deflate at level 9: a 1,640-byte pasted command line repeated CLIs → **12 records plain, 2 records deflated** (197 B); a 480-byte Python snippet → **4 plain, 2 deflated**; 2,000 bytes of incompressible ids → **14 plain, 10 deflated**.
- Finding: every byte the operator sends travels as raw `Input` at 137 B per record, while every byte the host sends goes through a compressor that takes a screen from 923 B to 508 B. The asymmetry is accidental — the frame message was built for screens — but on this link the uplink is where the largest single item of user data lives (a paste).
- Why it hurts at range: a 1,640-byte paste is 12 records × ~582 ms of air (166 B on air at 62.5 kHz/SF7) = **7 s of air direct, 21 s as three flood copies**; with the credit window of 4 at 16 hops (`crates/session/src/arq.rs:227`) that is 3 windows ≈ **4 minutes of wall clock** at a 76–86 s round trip, and every keystroke typed behind it waits in the same FIFO. The same paste compressed is 2 records, **1.2 s of air, one window**.
- Proposed fix (NOT applied): add the phone→host twin of `Frame` (a `FrameUp`, deflated with a fixed phone-side dictionary, gated by a `features` bit in `Attach` as every other wire change is), and have `MeshInputBatcher` emit one framed piece per pump cycle instead of 137-byte raw pieces. The host's decoder already inflates frames (`mesh_term.rs:1360`), so the receiving half exists; only the direction is missing.
- Open questions: whether a framed uplink should stay ≤ one record (to keep a keystroke's echo prompt) with framing used only for pieces over ~300 B; the dictionary for the uplink direction (command vocabulary, not screen vocabulary) and whether it can be reused as-is; whether the phone's deflate on a 1,640-byte buffer costs any measurable time on the UI thread (it should not, but it is on the input path).
- Investigator: batch round · 2026-11-11

### A-141 · batch: a frame is atomic across ticks, so a screen that needs more than one window paints nothing until the last tick · major

- Evidence: `crates/protocol/src/mesh_term.rs:1903-1904` (`let commit = out.len() + take < room && take == packed_len; self.frame_open = !commit;`) with the comment at `:1893-1900` explaining that a partial frame deliberately carries no `Commit`; `packages/mobile/.../mesh/MeshTerm.kt:970` ("Rows are held until their `Commit` and painted only then") and `:1019-1041` (`hasPending` accumulates `Rows` until a `Commit` of the same epoch, and only then does `lines = next` and `return true`); `crates/daemon/src/mesh_bridge.rs:2499` (`FRAME_MESSAGES = 12`) and `:1072-1080` (`room` in messages, `break` when zero). Measured inner-stream sizes with the shipped packer rules: a realistic 46×17 shell screen is 812 B → 122 B after deflate → **1 record**; a box-drawn 46×17 TUI 836 B → 80 B → **1 record**; 46×17 of incompressible ids 866 B → 616 B → **5 records**; 80×24 arithmetic: 24 rows × 83 B = one row per `Rows` message, so Profile + 24 `Rows` + Commit = **26 messages → 3 ticks** at `FRAME_MESSAGES = 12`.
- Finding: an ordinary screen is now *one* record, which is excellent. But the tail is not: whenever a frame needs more records than the credit window allows (5 > 4 at 16 hops) or more messages than one tick (`80×24`), the phone is given no `Commit` until every tick has landed, so it draws the **old** screen until the last record arrives. The commit is deliberately withheld so the cursor never runs ahead of the text, and that reasoning is sound — but the effect at range is all-or-nothing painting.
- Why it hurts at range: with a 4-record window and a 76–86 s round trip, a 5-record screen paints after **2 round trips (2.5–3 min)** and a desk-sized one after **3 (4 min)**, during which the operator sees a frozen screen with no indication that anything is arriving. A per-tick `Commit` (with the cursor from that tick's snapshot, which is already computed) would show the first third after one round trip and the rest as it lands.
- Proposed fix (NOT applied): in `Differ::update_within`, emit a `Commit` after each tick's rows when the frame is *partial*, carrying the cursor of the target screen and a flag saying "more rows follow in this epoch" (a spare bit in the `Commit` flags byte, version-gated). Keep the current all-or-nothing behaviour for a geometry change, where a half-painted pane is genuinely wrong.
- Open questions: whether a partial paint with the target cursor looks worse than a frozen screen on a phone (it may for a TUI that redraws its own frame); whether the phone should dim or mark a partially painted screen; and whether `FRAME_MESSAGES = 12` should instead be derived from the window so a frame that fits one window is always one tick.
- Investigator: batch round · 2026-11-11

### A-142 · batch: a paste has no integrity boundary, so a session death leaves half a command in the pane · major

- Evidence: `packages/mobile/.../mesh/MeshTermDriver.kt:779` (`outgoing.clear()` inside `failed()`, with the comment "Held messages go too: an `Attach` or an `Input` built for a session that ended is not for the next one"); `crates/daemon/src/mesh_bridge.rs:1864-1868` (every `Input` record is written to the pty as it arrives via `paste_bytes`); `mesh_bridge.rs:442-457` (`paste_bytes` decides bracketed wrapping **per record**). Field context: `docs/RADIO-AUDIT-LOOP.md` A-20 and A-23 (session replacement is common at range and drops unacknowledged bytes), A-108 (a 13-shell open already costs minutes).
- Finding: a multi-record paste is written to the pty record by record, and the phone's queue for the remainder is discarded the moment the session ends. Nothing on the wire marks "this paste is complete", and nothing tells the operator that a paste was cut. On a pane with bracketed paste the shell holds the text unexecuted, which is lucky rather than designed; on a pane without it, `paste_bytes` folds `\n` to `\r`, so each delivered line was already a command and a truncated paste runs a truncated command.
- Why it hurts at range: a 12-record paste spans 2–4 minutes at 16 hops, and that is exactly the window in which a session is replaced (A-20, A-23). The failure is silent, arrives as a partially executed command, and the operator's only evidence is a `mesh drop:` line in a log they are not reading.
- Proposed fix (NOT applied): give the input path a paste envelope — one `Input` message carrying `paste: begin|chunk|end` (or a length prefix and a closing `Input{paste: done}`) so the host can buffer the pieces and write them in one call, execute nothing until `end`, and log `paste incomplete: 7 of 12 pieces` when a session ends mid-paste. The phone then also knows what it never delivered and can say so on screen.
- Open questions: how much pty-side buffering the host may hold (a 65 KiB paste is fine in memory, but the write must stay one call for bracketed paste to behave); whether a partially received paste should be discarded rather than written; and whether the same envelope should cover `Key` runs.
- Investigator: batch round · 2026-11-11

### A-143 · batch: the paste queue is a fixed 64 KiB with no relation to what the link can carry · major

- Evidence: `packages/mobile/.../mesh/MeshInput.kt:248` (`const val CAPACITY = 65_536`), `:56` (`maxBytes = MeshTerm.MAX_INPUT`, i.e. one record per piece), `:235-237` (`IDLE_MS = 100`, `MAX_HOLD_MS = 200`); `packages/mobile/.../Paste.kt:13` (`INPUT_CHUNK_CHARS = 4000` for the tailnet route) and `MeshInput.kt:365-378` (`paste` accepts anything under capacity and refuses wholesale otherwise); the pump is FIFO, so nothing behind the paste is sent first (`MeshTermDriver.kt:1247` documents "a paste is the caller's to split and to rate-limit").
- Finding: the only limit on a paste is memory — 65,536 bytes, which is **478 records** at 137 B. The integrator that exists (`CAPACITY`) was sized for a route where 64 KiB is a blink; on the radio it is a queue that takes 478 × 582 ms of air ≈ **4.6 minutes direct** and, as three flood copies at range, ≈ **14 minutes** — during which every keystroke, every `Enter` and every `C-c` typed behind it waits in order.
- Why it hurts at range: an accepted paste that cannot drain is worse than a refused one. The user sees their paste "take", then a link that appears dead for minutes, and an interrupt that cannot be delivered because it is behind 400 records of clipboard.
- Proposed fix (NOT applied): derive the accepted paste budget from the link — `records_we_can_drain_in(t) = window × t / rtt` — and refuse (or offer to send the first N bytes with an explicit "paste truncated to N bytes here") when the clipboard exceeds it. `Attached::airtime`/`airtime_ms()` and the session's hop count are already available on both ends, so the number is derivable rather than a new constant. Separately, let an urgent piece (a line end or ETX) jump the paste queue rather than sit behind it.
- Open questions: whether the phone's own queue can be sequenced so `Key` messages bypass pending `Input` text (the ARQ keeps order, so this needs a second stream or a priority lane, which is a bigger change than a constant); what the right budget is for a 10-minute drain that a phone in a pocket can survive.
- Investigator: batch round · 2026-11-11

### A-144 · batch: the compression dictionary is one developer's vocabulary, home directory included · minor

- Evidence: `crates/protocol/src/mesh_term.rs:586` (`FRAME_DICTIONARY`, 945 bytes as shipped) contains `/home/ldeen/Documents/`, `/home/ldeen/`, `crates/`, `src/`, `tests/`, `cargo`, `clippy`, `rustc`, `journalctl`, `mesh`, `airtime`. Measured against that exact dictionary, raw deflate level 9: this project's build log **602 → 508 B (15.6 % smaller, 5 → 4 records)**; another user's Python/Node project (`pytest`, `acme-api`, `/Users/ana/work/...`) **461 → 434 B (5.9 %, still 4 records)**; 2,020 B of incompressible ids **1,396 → 1,394 B (0.1 %)**.
- Finding: the dictionary works, and its gain is real but uneven — roughly a tenth of the screen's airtime goes to a vocabulary that matches this repository and not other people's. Its contents also leak the author's home path into a wire contract, and `meshh`-style project nouns (`crates/`, `clippy`) are in the dictionary of a *terminal* link that most users will point at a Python or Node project.
- Why it hurts at range: the gain is 0.6 s of air per full screen for the wrong user where it is 1.2 s for the right one, and that is the difference between 4 and 5 records — which, per A-141, is the difference between one window and two at 16 hops, i.e. between a screen painted after one round trip and one painted after two. So a small percentage here crosses a threshold, not a gradient.
- Proposed fix (NOT applied): rebuild the dictionary from what the link actually carries — generic command vocabulary, common path prefixes, and the words the *protocol itself* puts on the wire — and drop the personal paths. The measurement harness for it is cheap: the host already logs screens; deflate a few hundred of them against candidate dictionaries and keep the one that minimises records, not bytes.
- Open questions: whether the dictionary could be per-pairing and negotiated at pairing time (a 945-byte table exchanged once over the tailnet would make the gain match the user); whether `zstd`-style shared dictionaries would be better than deflate with a preset; how a rebuilt dictionary interacts with the "byte-identical on both ends" requirement (it is versioned, so a bump is the mechanism).
- Investigator: batch round · 2026-11-11

### A-145 · batch: `MAX_SEGMENT` counts bytes, so a row of wide glyphs is fragmented into extra patches · minor

- Evidence: `crates/protocol/src/mesh_term.rs:208` (`MAX_SEGMENT = 120`), `:1619-1634` (`segments()` splits a row at 120 **bytes** on character boundaries), `:2029-2036` (each segment becomes its own `Patch` with `append: true`); `MAX_ROWS_BODY = 134` (`:180`) packs patches into a `Rows` body. Measured: a 46-column row of `─` is 138 bytes → **2 patches** instead of 1; 17 such rows = 34 patches, where the packer would otherwise fit 2 patches per `Rows` body.
- Finding: a box-drawn or CJK screen fragments every row into two patches, each carrying 3 bytes of patch header, and the extra patches push the `Rows` packing over `MAX_ROWS_BODY` sooner — so a TUI costs a record or two more than the same amount of ASCII. The code is honest about this (`clamp_row`'s comment explains why bytes and not characters), and it is a correctness-over-efficiency trade, but the cost is unmeasured and it lands on exactly the screens that are already hard to paint.
- Why it hurts at range: one extra record is 0.6 s of air and, per A-141, can be the record that pushes a frame from one window to two.
- Proposed fix (NOT applied): none needed for correctness; if the cost matters, let `segments()` split at a **character-count** limit that still guarantees the byte bound (i.e. stop earlier on wide glyphs so two 46-column rows can share a body), or raise `MAX_SEGMENT`'s budget to allow one full row of 3-byte glyphs (a 46-col row of `─` is 138 B; 138 + 3 = 141 > 134, so `MAX_ROWS_BODY` would have to move too, which it cannot without a protocol change).
- Open questions: measured record cost of a real TUI screen at 46×17 against the same screen in ASCII; whether the phone's own grid being ≤ 120 columns makes the byte bound a non-issue in practice.
- Investigator: batch round · 2026-11-11

### A-146 · batch: the burst pacing constant is derived from airtime, but the drain rate is one window per round trip — most of that mismatch is absorbed, and here is why · question

- Evidence: `crates/daemon/src/mesh_bridge.rs:2510` (`BURST_GAP = 1 s`) and `:2506-2513` (`burst_gap()` = 2 × airtime clamped 300 ms–1 s), `:2536-2541` (`echo_gap()` = one airtime clamped 200 ms–1 s); against `:1072-1080` (`room = PENDING_LIMIT - pending`, and `break` when `room == 0`), `PENDING_LIMIT = 8` (`:2488`), and the session credit `(64.0 / hops).clamp(1, 8)` = **4 at 16 hops** (`crates/session/src/arq.rs:227`) → one window per round trip, ~47–58 s by the session's own model and 76–86 s by the repeater-hold arithmetic (A-41).
- Finding: pacing at 1 frame/s looks two orders of magnitude faster than the link can drain, which would mean a queue of stale frames — but it does not, because the loop breaks as soon as the window is full (`:1077-1080`) and only rebuilds when there is room, and a refused send keeps exactly the unsent tail and re-offers it first (`:1166-1190`). So the screen is always built *fresh* at send time and nothing stale is queued. **This part is sound**, and I record it so a later pass does not "fix" it into a real queue.
- Why it hurts at range: it does not, except in one sense — the coalescing window (~1 s) is far below the drain period (~50 s), so a continuously changing screen is rebuilt many times between sends and every rebuild is discarded. That is wasted CPU, not airtime, and it means the *rate* of frames is set by the window, not by the constant; anyone tuning `BURST_GAP` for range behaviour would be tuning the wrong knob.
- Proposed fix (NOT applied): none. The only change worth considering is to make `burst_gap()` respect the drain period (a frame is not worth building if the last one has not been acknowledged), which would save the discarded rebuilds without changing what reaches the phone.
- Open questions: whether those rebuilds cost measurable CPU on the host with five attached shells; whether `stalled` (set whenever a window fills) should be reported in `mesh status`, since it is the honest measure of "the link is the constraint right now".
- Investigator: batch round · 2026-11-11

### A-147 · batch: `MAX_BODY` caps a compressed frame at 5 records, so a big frame is split into several frames, each paying its own header · minor

- Evidence: `crates/protocol/src/mesh_term.rs:170` (`MAX_BODY = 600`), `:663` (`FRAME_MAX_INFLATED = 8_192`), `:600` (`FRAME_COMPRESS_MIN = 200`); `crates/daemon/src/mesh_bridge.rs:776-821` (`frame_run` splits the run in halves until each frame encodes and is smaller than plain). Measured: 46×17 incompressible → inner 866 B → deflated 616 B, which exceeds 600 and therefore becomes **two frames** of ~308 B each (3 records each, 6 total) rather than one 5-record message — and the split loses a few bytes to each frame's own header.
- Finding: the recursion is correct and defensive (it also checks that the frame is smaller than what it replaces, which is why incompressible text does not regress), but a frame that lands just over 600 B pays a whole extra message header and a worse ratio after being halved. The phone's stream decoder already handles a frame in several records, so the 600-byte ceiling is a *message* bound rather than a *record* bound, and the frame path is the one place where a bigger body would be free.
- Why it hurts at range: one extra record is 0.6 s of air, and the pathological case (a screen of hashes, ids, base64 or a dense table) is precisely the one already fighting the window.
- Proposed fix (NOT applied): let `Frame`/`FrameDict` bodies exceed `MAX_BODY` up to a frame-specific bound (the decoder already reconstructs from `inner.len()` and `FRAME_MAX_INFLATED`), raising the ceiling to what one window can carry. That needs a version-gated decoder change on the phone (`MeshTerm.kt` body-length check) and is a wire change, so it belongs with the same change that adds A-140's uplink frame.
- Open questions: whether a 1,200-byte frame body makes the phone's decode of a partially arrived record harder (it should not — the ARQ delivers whole records in order); the true worst-case inflated size for a 120×50 pane (`FRAME_MAX_INFLATED` 8,192 may itself be the binding limit there).
- Investigator: batch round · 2026-11-11

### A-148 · batch: nothing on the wire tells the client how much an answer costs, so every wait is a guess · question

- Evidence: `crates/protocol/src/mesh_term.rs:359-385` (`Profile`, `Rows`, `Commit` carry no count of what follows; `Commit` has 5 flag/cursor bytes and a 4-byte `first_line`), `:486-517` (`WantHistory` states how many rows it wants, `HistoryPage` says `last`); the client's waits are constants derived from hops (`MeshTermDriver.kt:2596-2623`, `handshakeWaitMs`), and the attach/list waits guess from the request alone (A-104, A-105).
- Finding: the host knows, at the moment it answers, exactly how many records the answer will take (it has just built and compressed the messages and can count `ceil(len/143)` per message). It never says so. The client therefore sizes every deadline from the route, not from the work: a one-record list and a nine-record list get the same wait, so the small one waits too long and the big one gives up early.
- Why it hurts at range: the failures this audit has recorded are almost all "the client stopped waiting before the answer could arrive" (A-101, A-104, A-105). A cost figure on the first record of an answer turns every one of those into `wait = cost × rtt`, which is arithmetic the client can actually do.
- Proposed fix (NOT applied): add a `pendingRecords: u8` (or a `records` field on `Profile`/`Sessions`/`HistoryPage.first`) so the first message of a multi-message answer states its total, version-gated like every other wire change; the client then computes its deadline as `records / window × rtt` instead of using a constant. `Commit`'s flags byte has room for a "more follows" bit as the cheap half of the same idea (see A-141).
- Open questions: whether the count is knowable before compression for a `Rows` run (it is, after `pack_frame`); whether stating it is worth a byte on every answer or should ride only answers above one record.
- Investigator: batch round · 2026-11-11

### A-149 · batch: shells are served in `HashMap` order and the loop breaks when the window fills, so a busy shell can starve the others · major

- Evidence: `crates/daemon/src/mesh_bridge.rs:1021` (`let ids: Vec<u8> = self.shells.keys().copied().collect();` — `HashMap` iteration order, stable per map but unrelated to attachment order), `:1072-1080` (`room` computed per shell and `if room == 0 { self.stalled = true; break; }` — the remaining shells are skipped entirely for this tick), `:1265-1270` (a send fills the window and the next shell recomputes `room = PENDING_LIMIT - pending`); `PENDING_LIMIT = 8` (`:2488`) against a credit window of 4 at 16 hops (`arq.rs:227`).
- Finding: when a shell whose id comes first in the map has a frame large enough to fill the window (5–8 records — an incompressible screen, a scrolling TUI, or a `Hold` release), it takes the whole window and the loop `break`s, so every shell after it in map order is skipped. On the next tick the same shell is examined first and refills. With five shells open (the measured field state: `sessions parsed=13 attachable=13`, five attached) and one of them printing, the others can go unserved for as long as that one keeps producing — including the shell the operator is typing in.
- Why it hurts at range: the window is the whole bandwidth at 16 hops, so whoever gets it first occupies the link for a full round trip (47–86 s). A keystroke's echo for shell B queued behind shell A's repaint is the "keys go missing" report, and at range it is not packet loss but starvation.
- Proposed fix (NOT applied): round-robin the starting point — iterate from a stored cursor that advances each tick — and give each shell a per-tick share of the window (e.g. `room` for shell *i* is at least 1 when others are waiting) so one busy shell cannot take the whole window every tick. The echo path already has priority inside a shell (`a.echo`), so the same idea is being applied one level too low.
- Open questions: whether the fairness should be per shell or per *attachment* (the operator's attached shell arguably outranks four background ones); whether `stalled` should be per shell so the UI can say which one is waiting; and whether the fix is measurable in the journal (a per-shell "records sent / skipped" counter would settle it).
- Investigator: batch round · 2026-11-11

---

95:**Worst finding:** A-140 and A-143 together — the uplink has no compression at all while the downlink is compressed, so a 1,640-byte paste is 12 records / 7 s of air (21 s as flood copies, 3 windows ≈ 4 minutes) where deflate makes it 2 records / 1.2 s; and the queue that accepts it is a fixed 64 KiB regardless of the link, i.e. 478 records ≈ 14 minutes of air that also block every keystr

### A-160 · live: typing is invisible — no local echo, no pending indicator, in live mode · blocker

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:629` — `if (source.publicChannel) source.echoHeld(handle.echoText) else source.echoHeld("")`; the bar that would show held text is built only in held mode, `MeshTerminalScreen.kt:1068` (`if (config.meshHoldsInput) … else null`); the IME's buffer is a 1 dp, fully transparent field, `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/TerminalSurface.android.kt:891-899` (`BasicTextField`, `size(1.dp)`, `alpha(0f)`); the echo machinery itself exists and is safe — `MeshTerminalSource.kt:544-551` (`echoHeld` rebuilds the host's last screen then writes the held text on top), guarded by `MeshScreenBytes.kt:20` (the cursor travels in `Commit`).
- Finding: on a private tuple in live mode the phone shows **nothing at all** for a keystroke until the host's echo arrives. The typed characters go into an invisible IME buffer; the local echo is wired only to the held/public-channel mode; the pending label is rendered only by `PublicChannelBar`, which does not exist in live mode. The tailnet's predictive echo (`Predictor.kt`, `FramePacing.predict`) is not used by the mesh path at all (`grep FramePacing|predict` over `mesh/` → no hits).
- Why it hurts at range: at range the echo takes 13 s on a working direct route and ~64 s by flood (A-164, A-165 arithmetic below). A person typing into a screen that does not change for that long concludes the app is broken and starts pressing things — which is exactly the reported behaviour, and it also costs airtime, because pressing again doubles the work.
- Proposed fix (NOT applied): drive `echoHeld` from live mode too, with the text that is **sent but not yet echoed** rather than the batcher's pending text (see A-163), and draw it faint as the held echo already does. The existing rule — rebuild from the host's screen each time, autowrap off, nothing on an alternate screen — already makes it safe; the only new piece is the "sent, unacknowledged" set.
- Open questions: whether a faint echo is honest enough for a password prompt (the tailnet predictor stops predicting after the echo stops, which this should copy); whether the line should be marked as provisional after N seconds in flight.
- Investigator: live round · 2026-11-11

### A-161 · live: the phone has no per-keystroke state, so it cannot echo, count or confess · major

- Evidence: `MeshTermDriver.kt:1002-1009` (`quietForMs` needs `pending > 0` and measures from `maxOf(lastInboundAt, lastCarriedAt)` — a single link-wide timestamp); `MeshSession.kt:278-286` (`SessionHealth` = failed, peerClosed, needsRekey, pending, dropped — nothing per record); `crates/session/src/arq.rs:475-477` (`smoothed_rtt_ms()` is measured) vs `crates/session-ffi/src/lib.rs` exports (no `rtt` accessor anywhere in the list, lines 171-737).
- Finding: the session measures a smoothed round trip and the daemon can see per-record acknowledgement, but **none of it reaches the phone**. There is no notion of "this text was sent at T and has not been echoed", and no measured round trip to compare the promise against. The user is told "link quiet · N s" only when something is unacknowledged, which brings A-162 and A-168 with it.
- Why it hurts at range: every honest sentence ("sent 12 s ago", "the mesh has not passed this yet"), every local echo, and every adaptive window needs the same missing fact. Without it, the UI can only say "live" and be wrong.
- Proposed fix (NOT applied): export `ldmux_session_rtt_ms` (and per-record send/ack timestamps, or at least the age of the oldest unacknowledged record) through `session-ffi`, surface it in `SessionHealth`, and let the driver keep a bounded list of sent-but-unseen input.
- Open questions: whether the ARQ's `srtt` is meaningful when the route changes underneath it (A-80 suggests it currently is not); how many sent-but-unseen inputs should be remembered.
- Investigator: live round · 2026-11-11

### A-162 · live: the one warning that the link is not carrying is suppressed by the user's own typing · major

- Evidence: `MeshTermDriver.kt:1002-1009` (`quietForMs` returns null when `pending <= 0`, and measures from `maxOf(lastInboundAt, lastCarriedAt)`); `lastCarriedAt` is set on every successful hand-off to the radio (`MeshTermDriver.kt:2274-2276`, `:988`); `MeshTerminalScreen.kt:180` (`QUIET_NOTICE_MS = 10_000`), rendered at `MeshTerminalScreen.kt:1359` (`Notice("link quiet · ${quietSecs} s")`).
- Finding: the quiet notice starts its clock at the last *successful send*, and every keystroke updates it. A person typing continuously into a link that is delivering nothing therefore never sees the notice: the timer is restarted by each send, which is the very activity that fails to arrive.
- Why it hurts at range: this is the failure mode at range — records are accepted by the local radio (send OK) and lost on the mesh. The one indicator that exists is precisely disabled by the behaviour it should be warning about.
- Proposed fix (NOT applied): measure quiet from the last record **acknowledged by the peer** (or from the oldest unacknowledged record's send time), not from the last local send.
- Open questions: whether the local radio's `OK` should count as "carried" at all; on a store-and-forward mesh it means "queued", not "delivered".
- Investigator: live round · 2026-11-11

### A-163 · live: echo and burst pacing — and every in-flight window — are hop-blind · blocker

- Evidence: `crates/daemon/src/mesh_bridge.rs:518-522` (`echo_gap` = one **airtime**, clamped 200 ms..1 s), `:507-514` (`burst_gap` = two airtimes, clamped 300 ms..1 s), `:2488` (`PENDING_LIMIT = 8` records), `:2499` (`FRAME_MESSAGES = 12`), and `grep -n hops crates/daemon/src/mesh_bridge.rs` → **no hits**: the bridge never learns the route length. The only quantity it is given is `Link::airtime_ms()` (`mesh_bridge.rs:212-214`), which is one record's air on **one** hop.
- Finding: on a 14-hop route a record occupies `14 × 602.6 ms ≈ 8.4 s` of mesh time, while the bridge paces a fresh echo every **0.49 s** and a burst every 1 s, and allows **8** records in flight. The pacing and the window are sized for a link where flight time ≈ airtime, which is true at one hop and false everywhere else.
- Why it hurts at range: typing for 4 s produces ~8 echo frames, each needing 8.4 s of air, so the queue saturates at `PENDING_LIMIT` immediately and every frame after the first carries a screen that is up to a minute stale by the time it lands. The phone paints old news; the mesh spends ~67 s of airtime to deliver 4 seconds of typing.
- Proposed fix (NOT applied): give the bridge the hop count (it already reaches the daemon through the session's own figure, `mesh_serial.rs` knows `peer_path.hops`), and express `echo_gap`, `burst_gap` and `PENDING_LIMIT` in **flight time** — `hops × airtime` — rather than in airtime alone. On a 14-hop route that is one echo per 8.4 s and a window of one or two records.
- Open questions: whether `PENDING_LIMIT` should be 1 at range (the differ then always sends the newest state, which is what a terminal wants) — this is the same question as A-102/A-108 from the attach round, from the other end.
- Investigator: live round · 2026-11-11

### A-164 · live: keystroke-to-echo arithmetic at range, and why floods are the wrong carrier for typing · blocker

- Evidence: MeshCore's forwarding delay, `~/Documents/mesh-research/clones/filter-firmware/src/Dispatcher.cpp:55-57` — `calcRxDelay(score, air_time) = (10^(0.85 − score) − 1) × air_time` — applied **only to floods** (`:243-252`), while a direct packet is processed immediately (`:254-256`); score from `filter-firmware/src/helpers/radiolib/RadioLibWrappers.cpp:319-328`; our airtime `Radio.kt:493-498` (preamble 32 at SF7); the phone's own flood carrier `AndroidRadioLink.kt:1013` (A-120's latch), `:1026-1053` (three copies), `:298-301` (`floodIntervalMs`).
- Finding: at SNR −6.75 dB a 42 B record scores 0.063 and is held **1.15 s per repeater**, a 172 B frame scores 0.025 and is held **3.43 s per repeater**; below the −7.5 dB SF7 threshold the score is 0 and every flood costs the maximum **6.08 × airtime** per hop. Summed over a 14-hop route:
  - **direct (a working path):** 3.1 s up + 8.4 s down + 0.6 s batcher + 0.5 s echo gap ≈ **13 s** from keypress to visible echo;
  - **flood:** ~16 s up + ~48 s down ≈ **64 s**, per visible character.
- Why it hurts at range: the phone's carrier rule prefers flood until a record arrives *direct* (A-120), so at range the default carrier for a keystroke is the one that is delayed seconds per hop. Typing over a flood at range cannot be faster than about a minute per character — no window, timer or copy count changes that, only switching carrier does.
- Proposed fix (NOT applied): input should be the one record class that insists on the direct carrier, and when there is no direct route the UI should say so rather than accept keystrokes into a minute-long queue (see A-160/A-165). Also: `calcRxDelay` belongs in our own arithmetic — the phone already models airtime (`Radio.kt:455-498`) and does not model delay.
- Open questions: whether repeated copies of a flood are *each* held by a repeater (they are distinct packets, so probably) — which would make three copies cost three times the forwarding delay, not three times the airtime.
- Investigator: live round · 2026-11-11

### A-165 · live: the typing windows are set in airtimes (0.6 s) when the round trip is 13–64 s · minor (but it is the visible "inconsistent, not slow" feel)

- Evidence: `MeshInput.kt:246-250` (`pace(airtimeMs)` → `idleMs = airtime.coerceIn(IDLE_MS, 900)`, `maxHoldMs = max(idleMs, MAX_HOLD_MS)`), constants at `MeshInput.kt:236-241`; applied from the radio's report at `MeshTerminalScreen.kt:613-626` (`batcher.pace(RadioAirtime.timeOnAirMs(preset, MeshTerm.RECORD_PAYLOAD + 29))`).
- Finding: the coalescing window exists so a pause does not cost a record. At range a record costs ≥ 13 s of round trip, so coalescing 0.6 s of typing saves **nothing** while making the first character arrive 0.6 s later than it could; and the corresponding `MAX_HOLD_MS` ceiling means continuous typing is flushed every 0.6 s into a link whose queue already holds 8 records.
- Why it hurts at range: the windows are calibrated in the one unit that is not the bottleneck. On a link where flight time dominates, the right window is "the queue is empty and the last echo has landed", not a fraction of an airtime.
- Proposed fix (NOT applied): make the flush condition "the session has room and the previous echo has been seen", with the airtime window only as a floor for a link that is fast.
- Open questions: whether a typist at range would rather have coalescing of several seconds (fewer, larger records) — that is a UX choice worth a measurement.
- Investigator: live round · 2026-11-11

### A-166 · live: deleting a long line costs one record per 16 backspaces · major

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/InputDelta.kt:44-45` — `val deleted = previous.length - shared; if (deleted > 0) ops += InputOp.Keys(List(deleted) { "BSpace" })`, no run-length encoding; `MeshInput.kt:150-165` coalesces names into one `Run.Keys` up to `MAX_KEYS`; `crates/protocol/src/mesh_term.rs:160` (`MAX_KEYS = 16`); the key table has no repeat form (`MeshInput.kt:520-530`, `keyGlyph` shows `‹Name›` only).
- Finding: clearing a 300-character line at the IME (select all, delete) emits 300 named `BSpace` keys, packed 16 per record: **19 records**. Each `Key` message is `grep -c` … a full record, and on the flood carrier each is three transmissions.
- Why it hurts at range: 19 records × 8.4 s of mesh air ≈ **2.6 minutes** of airtime, plus the session's window, to express something the shell would accept as one byte — and the shell's line discipline already has `C-u`/`C-w` for exactly this, which the key bar can already send as a named key (`MeshInput.kt:520-530`).
- Proposed fix (NOT applied): a bounded repeat form for `BSpace` (e.g. `BSpace×12` as one name, decoded by the host into N deletes), or map a bulk deletion to the pane's own kill-line key. Sending fewer, larger messages is the same rule the rest of the protocol already follows.
- Open questions: whether a run of backspaces could be collapsed at the host instead (it sees the same 300 keys), which needs no protocol change and no version gate.
- Investigator: live round · 2026-11-11

### A-167 · live: an arrow or control key gives no feedback at all and costs a full record each · minor

- Evidence: `MeshInput.kt:503-508` (`sendKey` → `pushKey` → `appendKey`), `MeshInput.kt:412-420` (`echoText` renders only `Enter`, `Tab`, `BSpace`; everything else renders as nothing, with the reason spelled out), `MeshInput.kt:487-500` (`heldSummary` counts keys as "N keys").
- Finding: named keys are deliberately never echoed (correct — their effect is the pane's), but they also carry no in-flight marker, so a `C-c` on a stalled link is completely invisible: nothing is drawn, nothing is counted on screen in live mode (the summary lives in the held-mode bar), and the interrupt may take 13–64 s to arrive.
- Why it hurts at range: an interrupt that appears to do nothing invites the user to press it again, and each press is another record on a shared channel — the exact behaviour the batcher's `urgent` rule exists to prevent on the sending side but does not address afterwards.
- Proposed fix (NOT applied): show "C-c sent ⟨time⟩" (or a small pending chip) for named keys until the next screen arrives; the count machinery already exists in `summarize`/`readable`.
- Open questions: whether an interrupt should ever be sent twice on a silent link (the ARQ already retries; a second press adds a second record for nothing).
- Investigator: live round · 2026-11-11

### A-168 · live: the one latency number the user is given is 4–8× optimistic at range · minor

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/PublicChannelBar.kt:271-277` — `exchangeMs = 2 × hops × airtime(172)`, used by the held-mode bar (`MeshTerminalScreen.kt:1074-1079`).
- Finding: the estimate counts one record each way and **no repeater hold**. On a 14-hop route it says 16.9 s; the same exchange by flood is ~64 s because `calcRxDelay` adds 1.2–3.7 s per hop (A-164). It is also shown only in held mode, so in live mode the user has no expectation set at all.
- Why it hurts at range: the number is the only statement the app makes about how long the link takes, and it is wrong by a factor of four in exactly the regime where the user is deciding whether the link is broken.
- Proposed fix (NOT applied): `2 × hops × (airtime + rxDelay(score, airtime))` with the score derived from the last measured SNR (the radio reports it, `codec.rs:1142`), and say which carrier the estimate assumes.
- Open questions: whether SNR is available often enough on the phone to make this stable (it arrives with `Status`).
- Investigator: live round · 2026-11-11

### A-169 · live: IME composition and autocorrect reach the shell as keystroke-by-keystroke edits · question

- Evidence: `InputDelta.kt:35-69` (`inputOpsFor` turns whole-value changes into backspaces + text, with no notion of an IME composing region) and `TerminalSurface.android.kt:891-900` (the field is read as a plain value).
- Finding: a keyboard that *extends* its composition (the common case) produces the right result by accident — each intermediate value is a prefix, so only the newly added characters are sent. A keyboard that *rewrites* its composition (autocorrect, transliteration, swipe) produces backspaces plus replacement text, which is correct but costs one `Keys` record plus one `Text` record per rewrite.
- Why it hurts at range: on a 0.6 s-per-hop link a single autocorrect correction can cost more airtime than the sentence did. It is not wrong, only expensive — hence a question rather than a fault.
- Proposed fix (NOT applied): if it proves common, hold input while the IME reports a composing region (`TextFieldValue.composition`), which this path does not read.
- Open questions: how often rewriting keyboards are used on this device; whether Compose exposes composition on the hidden field at all.
- Investigator: live round · 2026-11-11

### A-170 · live: a full-screen program is declared "large and slow" but nothing changes behaviour · minor

- Evidence: `MeshTerminalScreen.kt:1364-1365` (`live?.screen?.alternate == true → Notice("full-screen program: updates large and slow")`), while the echo path skips the local echo on an alternate screen (`MeshTerminalSource.kt:~590`, "not on a full-screen program") and the bridge keeps the same `echo_gap`/`burst_gap`.
- Finding: in `vim`/`less` the app tells the user the truth in words and then does nothing differently: no local echo (correct), no different pacing, no smaller frames. The notice is therefore a permanent apology rather than a decision.
- Why it hurts at range: a full-screen program repaints whole rows, so its frames are the largest; at 14 hops each is 8.4 s of air and the same 1 s burst gap applies, which is the worst case for the queue (A-163).
- Proposed fix (NOT applied): make the notice a real policy switch — on an alternate screen, coalesce to flight time and prefer cursor-only updates.
- Open questions: whether the differ already minimises an alternate-screen frame to changed rows (it appears to).
- Investigator: live round · 2026-11-11

---

106:**Worst finding:** A-160 with A-163/A-164 — in live mode the phone shows nothing for a keystroke until the host's echo comes back, and that echo takes **~13 s on a working direct route and ~64 s by flood at 14 hops**, because MeshCore holds every flood for `calcRxDelay` seconds per repeater (`filter-firmware/src/Dispatcher.cpp:241-256`) while our pacing and in-flight windows are sized in single-hop airtime and 

### A-180 · host: the silence watchdog can never fire, because the heartbeat is self-generated · blocker
- Evidence: `crates/daemon/src/mesh.rs:966-967` (`event = carrier.recv() => { last_carrier = Instant::now(); }` — **any** event); `crates/daemon/src/mesh.rs:1115-1119` (the watchdog: `h.snapshot().pending > 0 && last_carrier.elapsed() >= silence`); `crates/daemon/src/mesh_serial.rs:1809-1812` (`Event::Status` published every `STATUS_REFRESH` = 500 ms, `:59`); `crates/daemon/src/mesh_serial.rs:2524-2535` (`status()` is computed from `settings`/`budget`/`sent` — **no radio I/O at all**).
- Finding: `CARRIER_SILENCE` was added so that a radio that stops answering ends the connection and `supervise` reopens it. But `last_carrier` is refreshed by every event, and the radio thread manufactures a `Status` event from its own clock every 500 ms without touching the radio. On a radio that has stopped answering (the DTR/bootloader wedge, a stalled UART), the loop's own cycle is: handle `Command::Record` (up to ~46 s of failed sends) → publish `Status` → handle the next record. So the longest gap between `Status` events is ~46 s, against a `silence` of `max(75 s, 3 × first_rto)` = 75–123 s. The watchdog cannot elapse, so the recovery path it exists for is unreachable and the snapshot keeps reading `ready` while every send is refused.
- Why it hurts at range: this is the exact failure that has needed a hand restart repeatedly (`no reply to 0x05`), and the backstop written to fix it is defeated by a heartbeat that proves nothing about the radio. At range the wedge is more likely (rf interference does not cause it, but repeated daemon restarts and USB re-enumeration do), and the user's only symptom is a phone that will not handshake.
- Proposed fix (NOT applied): separate "the thread is alive" from "the radio is answering". Either publish the status with a sequence that only advances after a *successful* radio command, or (smaller) refresh `last_carrier` only on `Event::Recv`/`Event::SendResult`/`Event::Signal`, never on `Event::Status`, and let `supervise` treat a `Status`-only stream as silence after the bound.
- Open questions: whether `Event::SendResult` with a refusal should count as liveness (it proves the thread, not the radio); whether the phone's own reconnect cadence would mask the difference in field logs.
- Investigator: host round · 2026-11-11

### A-181 · host: one thread serves the port for both directions, so every serial command trades directly against the receive path · major
- Evidence: `crates/daemon/src/mesh_serial.rs:1356` (the loop takes a command with `recv_timeout(COMMAND_WAIT = 10 ms)` *before* reading), `:1919-1920` (`companion.next_push(Duration::from_millis(1))` — one frame per pass, after the command was handled); `:335` (`std::thread::sleep(reply_delay(...))` = 500 ms + up to 5 × 0.5 × airtime ≈ **1.75 s at this tuple**, inside `send_route_reply`, which is called from the push path at `:2075`); `:2259-2264` (`refresh_peer_route` — one `CMD_GET_ADVERT_PATH`, `ROUTE_READ_TIMEOUT` 1.5 s, `crates/companion/src/session.rs:37` — **before** the record is handed to the session at `:2284`); `:1702-1704` (`app_start`, 5 s timeout, `crates/companion/src/session.rs:153`, every `PARAM_WATCH` = 60 s); `:1781` (`radio_stats`, 1.5 s, every 20 s). `Companion::has_pending` exists for exactly this ordering (`crates/companion/src/session.rs:477-479`) and is used in **one** place (`:1781`).
- Finding: received frames are not lost during these calls — `Companion::request`'s `pump()` keeps draining the port into `pending` (`session.rs:655-680`) — but nothing is *delivered* to the session until the loop reaches `next_push`. So each inbound record carries a delivery delay of: up to 1.75 s (a flood reply's own echo wait), up to 1.5 s (a route re-read triggered by the previous record, floored at 2 s by `ROUTE_REASK_FLOOR`), up to 5 s every minute (`app_start`), and up to ~46 s if the radio stops answering and three flood copies each burn three `send_retrying` rounds (`:487-503`, deadline `raw_send_deadline` = 4 s + airtime, `:531-534`).
- Why it hurts at range: at range the ACL is the ack *and* the next thing the session is waiting for, and every one of these delays is longer than an ARQ round trip at a neighbour (2.9 s) — so the host's own housekeeping looks exactly like packet loss to the ARQ, which then retransmits into a channel the host was not listening to. The delays are additive with A-61's reply sleep and A-41's repeater holds.
- Proposed fix (NOT applied): give the receive side a priority lane — drain `pending` to `Event::Recv` before every serial command, i.e. call `has_pending`-style flushing at the top of the loop and inside `refresh_peer_route`'s caller; and move `refresh_peer_route` to *after* the record is delivered (it is a cache read, its answer only matters for the next send).
- Open questions: whether `pending` can be flushed to the event channel from inside `Companion` (it owns the queue) or whether the loop should call `next_push` in a tight inner loop whenever `has_pending()`; whether `app_start` could be `SET`-less and cheaper.
- Investigator: host round · 2026-11-11

### A-182 · host: the remembered peer radio key becomes a permanent routing pin that no command can clear · blocker
- Evidence: `crates/daemon/src/mesh_serial.rs:1133-1136` (`peer_key` loaded from `mesh-peer` at open), `:2054` (`peer_radio = pinned.or(Some(req.peer_key))` — the **old** key), `:2043-2052` ("request from … ignored for routing: the peer is pinned"), `:2218-2235` (a route hint from a different key is also not adopted), `crates/daemon/src/mesh.rs:564-571` (`clear_pin` clears the *session* pin via `files.clear_pin()`), `crates/daemon/src/mesh_session.rs:533-537`; `ldmux mesh` has only `preset` and `status` (`crates/daemon/src/bin/ldmux.rs:1118-1140`), and `ldmux code`/`ldmux pair` print links without touching it.
- Finding: `<state>/mesh-peer` was added so a restarted daemon can address a flood (RELEASE-AUDIT row 6.24d). It is now treated as a security-grade pin: a request from any other radio key is answered *to the pinned key*, and a new phone's key is never adopted — for the life of the process and across restarts. `clear_pin`, the one recovery the app offers, does not touch it, and no CLI does.
- Why it hurts at range: swapping a phone, factory-resetting one, or restoring a backup to new hardware changes the radio key. The host then floods every answer to a radio that is not on the air, says one line per minute, and the phone shows "handshaking" for ever. The cure is deleting a file under the state directory by hand, which nothing documents as a cure.
- Proposed fix (NOT applied): separate the two facts — keep `mesh-peer` as a *hint* (used only while it agrees with traffic) and let a handshake from a different key displace it, since the session pin (the PSK and the pinned static key) is the actual security boundary; or, if the pinning is deliberate, give it a name and a command (`ldmux mesh forget-peer`) and say so in the refusal line.
- Open questions: whether the same `clear_pin` path issues the new PSK at the same time (a re-pair revokes the old key, which would make auto-displacement safe); whether two-key operation is wanted at all (see the ledger's A-32).

### A-183 · host: a three-copy flood is charged to the airtime meter as one record · major
- Evidence: `crates/daemon/src/mesh_serial.rs:1468-1495` (the burst: `for _ in 0..RECORD_FLOOD_COPIES` with a fresh wrapper nonce each, and `RECORD_FLOOD_COPIES = 3` at `:513`); `:1510` (`sent = sent.saturating_add(1)` — **once** per `Command::Record`, whatever the carrier did); `:2524-2535` (`tx_airtime_tenths_this_hour` is `sent × airtime_ms`).
- Finding: on the flood carrier the host transmits three full packets and counts one. On a metered band this under-reports the hour's real airtime by 3×, and the countdown that holds the session back (`Status.ops_left_this_hour`) is derived from the same number, so a link running entirely on floods spends up to three times the allowance the meter believes it has.
- Why it hurts at range: at range the flood carrier is the only one carrying (per the ledger's A-120), so the mis-count is the *normal* case rather than an edge one — and it defeats exactly the policy the daemon was built to honour (1 % inside 868.0-868.6, a tenth of 10 % inside 869.4-869.65: `:722-744`).
- Proposed fix (NOT applied): charge per transmitted packet, not per record — increment `sent` inside the burst loop per successful copy, or add the copies as a separate term to `status()`.
- Open questions: whether the phone's own meter counts copies (it has the same three-copy rule); and whether the *wrapper* bytes should be charged too, since `airtime_ms` is `time_on_air_ms(RECORD_MAX)` for a bare record.

### A-184 · host: the companion client re-sends a *mutating* TX command while the radio is silent, so one record can go on the air many times · major
- Evidence: `crates/companion/src/session.rs:29` (`RESEND_INTERVAL = 600 ms`), `:563-566` and `:646-649` (`if !heard_anything && last_send.elapsed() >= RESEND_INTERVAL { self.link.write_all(&framed)?; }` inside `request_any`/`request`), `:508-511` (the whole command frame is what is written); `heard_anything` is set by *any* bytes at all including console text (`:515`, `:655-680`); applied to `SEND_RAW_PACKET` (`:286-320`), `SEND_SELF_ADVERT` (`:207-215`).
- Finding: the resend rule is aimed at a lost *command frame*, but it cannot distinguish "the firmware never got the command" from "the firmware got it and is not answering". Each resend of `CMD_SEND_RAW_PACKET` is a fresh queue-and-transmit request. With a 4.9 s deadline this is up to 8 resends per `send_retrying` round, × 3 rounds, × 3 flood copies — up to 72 raw-send requests for one record if the port stays silent.
- Why it hurts at range: it is the shape of failure where the host amplifies its own airtime exactly when the channel is worst. It only bites while the port is silent, which is also the state in which the firmware *may* have already transmitted the first copy — the one combination that turns a wedged-but-alive radio into a packet storm, and the one the 1 % policy assumes cannot happen.
- Proposed fix (NOT applied): never resend a command that mutates the radio's transmit queue. Restrict `RESEND_INTERVAL` to idempotent reads (`APP_START`, `DEVICE_QUERY`, `GET_STATS`, `GET_ADVERT_PATH`), and let a lost `SEND_RAW_PACKET` be reported as a refusal for the caller's ladder to pace — which is what A-1..A-8 of the mesh already does.
- Open questions: whether firmware ≥ 1.18's `RESP_CODE_SENT` makes the distinction cheap (it is emitted per accepted send); whether `heard_anything` should count only framed pushes rather than console bytes.

### A-185 · host: the send path has no backpressure, and the timeout that pretends to bound it is dead code · major
- Evidence: `crates/daemon/src/mesh_serial.rs:44` (`use std::sync::mpsc::{… channel}` — unbounded), `:797` (`let (command_tx, command_rx) = channel()`), `:882-892` (`send_record` is `async fn` whose whole body is `commands.send(...)` — non-blocking), `crates/daemon/src/mesh.rs:1227-1230` (`tokio::time::timeout(CARRIER_TIMEOUT, carrier.send_record(…))`), `crates/daemon/src/mesh.rs:62` (`CARRIER_TIMEOUT = 30 s`), `crates/daemon/src/mesh.rs:1242` (the drain loop `continue`s while `awaiting.len() < CREDIT_WINDOW`).
- Finding: `carrier.send_record` returns as soon as the record is *queued*, so the 30-second timeout around it can never fire and offers no protection; the only feedback is the lagging `SendResult`. Queue depth is bounded in practice by `CREDIT_WINDOW` (8) because `awaiting` only drains on `SendResult` — but the *latency* is not bounded at all, and `AWAITING_DEADLINE` (10 s, `mesh.rs:48`) frees the slots before a slow radio has finished, which lets the ARQ retransmit records that are still sitting in the queue.
- Why it hurts at range: a record may be retransmitted up to five times (the ARQ's ladder) before the radio has transmitted its first copy, and each retransmission is charged airtime. The host's own belief about "what is in flight" and the radio's queue are two different things, with a 10-second assumption between them.
- Proposed fix (NOT applied): make `send_record` complete on the radio's acceptance (`SendResult`) rather than on queueing — i.e. hand back a future — or have the carrier expose its queue depth so `l2_slots` reflects it. Then `AWAITING_DEADLINE` can be derived from airtime × hops rather than a flat 10 s.
- Open questions: whether the firmware's `CORE_STATS` outbound depth (already parsed, `session.rs:191-196`) is a usable meter; whether the 10 s `AWAITING_DEADLINE` is reachable at 14 hops by design.

### A-186 · host: a host restart is silent to the phone, which then burns a whole session-timeout of airtime discovering it · major
- Evidence: `crates/daemon/src/mesh_session.rs:194-201` (`Some(ldmux_session::record::KIND_TRANSPORT) => { let Some(session) = self.session.as_mut() else { self.last_error = Some("transport record with no session".to_owned()); return; } }`); no message is sent on that path; `crates/daemon/src/mesh.rs:830-845` (a new `SessionHost` per L2 connection, so every reconnect/reconnect-after-restart loses the session); `crates/daemon/src/mesh_serial.rs:2030-2070` (the host answers route *requests*, which the phone only sends on its own ladder).
- Finding: after a daemon restart (or any reconnect of the serial link, which `supervise` performs on error, `mesh.rs:684-738`) the host has no session, and the phone has one. The phone's transport records arrive, fail to open, and are counted locally as "transport record with no session"; nothing goes back on the air. The phone's ARQ retransmits until its own silence bound ends the session and it handshakes again — `first_rto × 5` at 14 hops is a couple of minutes of transmissions into a host that is already listening and merely needs a handshake.
- Why it hurts at range: the recovery is entirely one-directional. Every daemon restart, every reconnect, every `restart-keeper` costs the phone minutes of airtime before the link returns, and the operator sees only "the shell stopped".
- Proposed fix (NOT applied): answer an unopenable transport record with the cheapest possible hint the peer already understands — a sealed record is impossible without a session, so this has to be an unsealed control packet in the same family as the route hint (`LDRH`, `:203`), sent once per window, meaning "no session here; handshake". The phone already has to handle a hint it did not ask for.
- Open questions: whether that hint is a downgrade oracle (it reveals "a host is here" to anyone who heard the phone's flood — arguably already public via the route reply); what rate limits it needs (`ROUTE_REPLY_FLOOR` is the existing precedent).

### A-187 · host: a flood answer sleeps the reader on the air's behalf, and the port goes dark for the whole sleep · major (cross-reference: ledger A-61)
- Evidence: `crates/daemon/src/mesh_serial.rs:524-529` (`reply_delay` = 500 ms + `entropy % (5 × 0.5 × airtime)`); `:335` (`std::thread::sleep(reply_delay(airtime_ms))`); called from the push path at `:2075-2090` on every route request copy that passes `first_time_question` and `ROUTE_REPLY_FLOOR` (600 ms).
- Finding: quantifying A-61 rather than restating it. At the field tuple the sleep is 500 ms → 1.745 s, and it happens **on the only thread that reads the port**. During it, nothing is drained from the serial device; the firmware's companion output for received packets goes into `pending` only via `pump()`, which is not running. Whether the bytes survive depends on the firmware's USB CDC buffer, which is not something this code can see or size.
- Why it hurts at range: at range the phone sends three request copies per rung and three handshake copies, so the host can be asleep for the majority of a rung — and the records that arrive in those windows are exactly the ones the reply is being timed against.
- Proposed fix (NOT applied): the same fix A-61 proposes (enqueue the delayed reply and keep reading) — this entry exists to record the *loss* mechanism and its size, so a fix that keeps the sleep but moves it elsewhere is not mistaken for a fix.
- Open questions: the ESP32-S3 CDC-ACM RX buffer depth and the firmware's behaviour when it fills (drop the oldest? refuse to read?), which decides whether the dark window costs a packet or only a delay.

### A-188 · host: the parameter watch blocks the reader for up to five seconds every minute · minor
- Evidence: `crates/daemon/src/mesh_serial.rs:1702-1704` (`if last_params.elapsed() >= PARAM_WATCH { … companion.app_start("ldmux") … }`, `PARAM_WATCH = 60 s` at `:67`); `crates/companion/src/session.rs:148-153` (`app_start` waits `SELF_INFO` for **5 s**, with `RESEND_INTERVAL` resends); `:1750-1760` (on a changed tuple it additionally calls `device_query()`, `session.rs:163-170`, and `ScopeChoice::resolve`, which issues `get_default_flood_scope`).
- Finding: on a *quiet* channel (nothing heard for 600 ms) the app-start command is written up to 9 times over its 5 s window. The path is only taken once a minute, and it is followed by a `device_query` (another 5 s) whenever the tuple changed.
- Why it hurts at range: it is a serial-only cost with no airtime, so it is minor — but it lands on the same thread as A-181 and it is the longest single block that happens *routinely* rather than as a failure. A burst of phone copies arriving inside that minute can be delivered up to 5–10 s late, which is longer than the ARQ's first retransmission at a neighbour.
- Proposed fix (NOT applied): skip the watch while `has_pending()` is true (the pattern already used for `radio_stats` at `:1781`), and move the `device_query` follow-up to the next idle pass.
- Open questions: whether `app_start` is even needed on the watch path, since `SELF_INFO` is what is wanted — a cheaper command may exist.

### A-189 · host: the carrier diagnostic added for this investigation is rate-limited to one line a minute, and shares one stamp across unrelated decisions · major (observability)
- Evidence: `crates/daemon/src/mesh_serial.rs:217-221` (`route_log_rare` — one per 60 s), `:1560-1567` (the `record N B sent by {carrier} over H hop(s)` line uses `last_carrier_log`), `:2168-2173` and `:2186-2191` (the flood-record lines use *other* stamps). Measured: `journalctl --user -u ldmux | grep -c "sent by"` = **2**, over a day containing many sessions and hundreds of records.
- Finding: the one line that says which carrier a record took — the line this audit needs in order to tell "the answer left by flood" from "the answer never left" — is emitted at most once a minute, and its stamp is shared by direct and flood decisions alike. Meanwhile `route_log` (unconditional) writes `route request from … hops=N (adopted)` for every burst, so the journal is full of the least informative line and nearly empty of the most informative one.
- Why it hurts at range: every field investigation so far has had to infer the carrier from secondary evidence (the phone's `record by flood … 133 B dest 97`), because the host's own record of it was suppressed. A diagnosis that cannot be made from the logs is the reason this ledger exists.
- Proposed fix (NOT applied): keep the minute-rate for *repetition of the same message* (that is what it was for) but make the state *change* unconditional — log when the carrier changes, when the hop count changes, and every N records with a counter, so a burst shows as `record 50 B sent by flood over 3 hops (1 of 3 copies, 41 this minute)`.
- Open questions: whether the journal can carry a per-record line at all on a busy day (`eprintln!` to the journal socket is a synchronous write on the radio thread).

### A-190 · host: three async methods block a tokio worker for up to ten seconds · minor
- Evidence: `crates/daemon/src/mesh_serial.rs:831-848`, `:849-864`, `:865-880` — `async fn set_preset` / `set_path_width` / `set_tx_power`, each `match answer.recv_timeout(Duration::from_secs(10))` on a `std::sync::mpsc::Receiver`, with no `spawn_blocking` and no `block_in_place`. Contrast `SerialCarrier::drop`, which does use `block_in_place` (`:908-920`).
- Finding: `recv_timeout` is a blocking call inside a future polled on a runtime worker. `tokio::runtime::Runtime::new()` (`crates/daemon/src/bin/ldmuxd.rs:227`) makes it a multi-thread runtime, so one stalled worker costs a thread — but every other task scheduled on that worker (the control socket, the WebSocket server for the tailnet route, the bridge) waits behind it.
- Why it hurts at range: retunes are rare, so this is not an at-range failure; it is listed because it is the kind of thing that turns a rare retune into an unexplained whole-daemon hiccup at the same moment the radio path is being reconfigured.
- Proposed fix (NOT applied): wrap the wait in `tokio::task::block_in_place`, as `Drop` already does, or make the reply channel `tokio::sync::oneshot`.
- Open questions: whether any caller can reach these while a session is established (a preset change mid-session does).

### A-191 · host: `route_stale` hardcodes a 603 ms airtime while every neighbouring calculation uses the radio's measured one · minor
- Evidence: `crates/daemon/src/mesh_serial.rs:166-169` (`route_stale` = `max(60 s, 2 × 603 × hops)`); `:531-534` (`raw_send_deadline` uses a flat 4 s); `:536-540` (`direct_round_trip` uses `settings.airtime_ms`); `:854-858` (the radio's own `time_on_air_ms(RECORD_MAX)` is read at open into `settings.airtime_ms`).
- Finding: `603` is the airtime of a full record at 62.5 kHz/SF7 — correct for the field tuple and wrong for every other one. At 500 kHz the same record is ~70 ms, so a 10-hop route is called stale after 60 s instead of after 1.4 s; at SF10 it is longer than the radio's real airtime, so the route is called dead while its first exchange is still crossing. Meanwhile the same file computes `direct_round_trip` from the measured figure, so two functions disagree about the same quantity by up to 8×.
- Why it hurts at range: at the field tuple it is right, which is why it has never hurt *yet*; the danger is a tuple change (a regional mesh at a different SF) silently making route staleness wrong in either direction, with the disagreement invisible because both numbers look plausible.
- Proposed fix (NOT applied): pass `airtime_ms` in, as `direct_round_trip` already does, and derive the constant from the measured value.
- Open questions: whether the 2-airtimes-per-hop factor should be the firmware's `calcRxDelay` (which the ledger's A-41 measured at up to 2.7 s per hop for a 133 B record, i.e. not 2 airtimes at the margin).

### A-192 · host: the flood wrapper's "fresh nonce" is a clock field, not entropy · major (conditional)
- Evidence: `crates/daemon/src/mesh_serial.rs:450-456` (`route_nonce()` = `subsec_nanos().to_le_bytes()`), used per flood copy at `:1470` and for the reply jitter at `:524-529` (`let entropy = u64::from(u32::from_le_bytes(route_nonce()))`); the code's own requirement at `:1460-1462` ("a fresh wrapper nonce each time, since an identical repeat is swallowed by the first repeater's duplicate filter"); `crates/companion/…` dedup basis is `SHA256(payload_type ‖ payload)`.
- Finding: the requirement the design *depends on* is "each copy must be a different packet". The implementation takes a nanosecond counter modulo one second. Two records sent exactly one second apart are byte-identical, so every repeater's duplicate filter drops the second one and the host believes it sent a retry. Consecutive copies inside a burst differ only by the serial round trip, so the burst works — but the invariance is accidental, and the "entropy" feeding the reply jitter is a deterministic function of wall-clock sub-second time, so two responders deciding in the same millisecond pick the same offset.
- Why it hurts at range: the collision case (a retry at ~1.000 s) is exactly the cadence a flood retransmission has, and its symptom is a retry that never leaves the first repeater — invisible on both ends, and indistinguishable from a lost packet, which is the hardest failure this ledger has.
- Proposed fix (NOT applied): use a random source (`rand` is in the dependency allowlist) or a monotonic send counter, and keep the per-copy freshness a property rather than a likelihood.
- Open questions: whether a repeater's duplicate cache is keyed on the whole payload for floods as well as directs (the code says it is); whether a 4-byte counter that is not the clock would leak anything (it would not — it is inside a sealed record for records, and a wrapper field for floods).

### A-193 · host: the event channel couples the radio thread's reading to the consumer, at a capacity that a stall can fill · question
- Evidence: `crates/daemon/src/mesh_serial.rs:798` (`async_mpsc::channel(64)`), `:1539`, `:1545`, `:1809-1812`, `:2212`, `:2284` and eleven more `blocking_send` sites in `serve_loop`; the consumer is `crates/daemon/src/mesh.rs:965-1110`.
- Finding: `blocking_send` on a bounded channel means the radio thread stops reading the port when 64 events are queued. At two `Status` a second plus one `Recv` per packet, that is about 30 s of consumer stall. The longest consumer stall I can find is `set_preset`'s blocking `recv_timeout` (10 s, A-190) — under the bound — but the margin is not large, and the failure it produces would be silent packet loss rather than a delay.
- Why it hurts at range: unverified whether it is reachable; listed so a later round can measure event-queue depth under load rather than assume.
- Proposed fix (NOT applied): none proposed until measured. If it is reachable, the shape is a separate channel for `Recv` (which must not be dropped) and a *last-value-wins* slot for `Status`/`Signal` (which may be dropped freely).
- Open questions: actual worst-case consumer stall; whether `Status`/`Signal` could be `try_send` with the last value kept.

### A-194 · host: the open timeout cannot cancel the open, so a timed-out attempt leaves its own thread holding the port · minor
- Evidence: `crates/daemon/src/mesh.rs:765-773` (`tokio::time::timeout(CARRIER_TIMEOUT, SerialCarrier::open(…))`), `crates/daemon/src/mesh_serial.rs:786-792` (`spawn_blocking(|| prepare(…))`), `:1064-1071` (`prepare` takes `lock_exclusive` first), `:900-928` (`Drop` waits `CLOSE_WAIT` = 250 ms then detaches); `crates/companion/src/link.rs:166-176` (`flock … LOCK_EXCL_NB`).
- Finding: `prepare` runs on a `spawn_blocking` task that a futures timeout cannot cancel. If the 30 s bound fires (a radio answering nothing: `app_start` 5 s + `device_query` 5 s + scope read 1.5 s + resends inside each), the task keeps the `flock`. The next attempt then fails with `"{device}: another process holds this radio"` — which is this process's own previous thread, and reads to an operator as "some other program has my radio".
- Why it hurts at range: it is a recoverable condition reported as an unrecoverable-looking one, and the backoff doubles each time (no `bound`, `mesh.rs:731-738`), so a wedged radio looks like a permanently contended device. The comment at `mesh.rs:55-61` shows this exact misdiagnosis has already cost one investigation.
- Proposed fix (NOT applied): say which process and pid holds it when the lock is ours (compare against this process's own attempt), or make the open non-cancellable by construction (no `timeout`, since `prepare`'s own commands are already bounded) and let the backoff do the bounding.
- Open questions: the largest reachable `prepare` duration, which decides whether the 30 s bound can fire at all (the comment estimates 16.5 s; the resend rule could extend each command).

---

**What is sound, for the record:** the serial open path (no DTR/RTS transition, `HUPCL` cleared, `O_CLOEXEC` + `flock`) is careful and well explained (`link.rs:84-176`); a hung-up port is a named error rather than silence (`link.rs:196-215`, with a real pty test); the write bound is real (`WRITE_TIMEOUT`, tested against a writer that never accepts a byte); the reconnect backoff resets only on a completed open (`mesh.rs:684-738`); the hourly allowance is persisted across restarts and is correctly *absent* on an open band, matching the phone (`mesh_serial.rs:722-744` vs `Radio.kt:474-485`); adverts are off unless `LDMUX_MESH_ADVERT_SECS` is set, so the host is not spending airtime on broadcasts (`:101-109`); per-burst route convergence is correct — after the first copy of a burst adopts, `proven = true` refuses later candidates, which is the fix for the "last, longest copy won" failure (`prefer_route`, `:404-436`).

114:**Worst finding:** A-180. The one mechanism the host has for noticing "the radio has stopped answering" can never fire, because the radio thread manufactures a status heartbeat from its own clock and `last_carrier` counts it as liveness. Everything else on this list is slower or spendier; this one is why a wedged radio needs a hand restart.

### A-200 · app: changing the route destroys the radio session, and nothing logs that it happened · blocker

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/App.kt:79` (`val mesh = remember(config.connectionKey, credentialsVersion) { meshScreenFor(config) }`), `:120`/`:140` (`if (mesh != null) MeshTerminalScreen(...) else AppContent(...)`); `meshScreenFor` returns null for any non-mesh route (`mesh/MeshTermHost.kt:83`); `MeshTerminalScreen.kt:257` (`val conn = remember(plan, attempt) { … }`), `:384-391` (`onDispose { conn.close() }`); `Radio.kt:683-696` (`close()` → `link.release()`); `AndroidRadioLink.kt:1231-1235` (`close()` → `RadioLinks.close(this)`) → `:2120-2126` → `closeNow()` → `closeTransport()` (GATT disconnect); `MeshTermDriver.kt:621-633` (`start()` → `connect()`: a connection always begins a handshake); `Link.kt:114-118` (`ConfigStore.update` persists and sets, and logs nothing).
- Finding: the route is a persisted config field, and `config.connectionKey` includes it (`Config.kt:224`). So changing the route rebuilds the remembered mesh plan, disposes `MeshTerminalScreen`, and closes the radio link at GATT level. Returning rebuilds the plan and starts a *fresh* handshake ladder plus a fresh attach. Nothing in the durable log records the route change, so the file interleaves both transports with no marker — the 18:37 field window reads as "the app used the tailnet while showing the radio" when in fact the operator had switched the route and the radio had been torn down.
- Why it hurts at range: the cheapest way to look at the app is the most expensive thing to do to the radio. At a neighbour a ladder is ≥190 s (`MeshTermDriver.kt:2596-2623`) and at 14 hops ≈372 s; an attach is A-100…A-108. The operator's own reported habit — "I keep switching back to see what you're doing" — is therefore, by construction, the action that guarantees "it never finishes handshaking". It also makes every previous "it worked / it didn't" observation ambiguous, because the evidence for which transport was live does not exist.
- Proposed fix (NOT applied): own the connection above the route branch — a process-scoped holder keyed on the radio + pairing, with the screen merely attaching to it — so choosing the tailnet hides the mesh surface instead of destroying the link; and write one `route -> tailnet|mesh` line from `ConfigStore.update` whenever the route changes, so a field log can be read.
- Open questions: whether keeping a bound BLE link while the tailnet surface is drawn is acceptable battery-wise at range (the FGS already holds a partial wakelock while a shell is wanted); whether the mesh driver's timers behave while its surface is not composed.
- Investigator: app round · 2026-11-11

### A-201 · app: the 45-second release grace is a tailnet figure, and at range it costs a whole re-handshake · major

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/BackgroundSession.android.kt:225` (`actual val releaseGraceMs: Long = 45_000L`); `AppLifecycle.kt:63` (`graceMs: Long = BackgroundSession.releaseGraceMs`); `mesh/MeshTerminalScreen.kt:379-388` (`AppLifecycle.setReleaseHandler { released = true; BackgroundSession.setActive(false, ""); conn.close() }` — no route-specific grace); `MainActivity.kt:125-128` (`onStop` → `AppLifecycle.set(false)`); no `KEEP_SCREEN_ON` anywhere in `packages/mobile/composeApp/src` (grep: zero hits).
- Finding: the mesh route keeps the platform-wide 45 s grace. When the screen goes off, `onStop` starts it, and 45 s later the connection is closed — a GATT disconnect (A-200's path) followed, on return, by a fresh handshake and attach. The system screen timeout on the tested device is 30 s, so *waiting for a slow link is enough to destroy it*: the operator sets the phone down to let the ladder run, and the ladder's own duration triggers the teardown.
- Why it hurts at range: a re-handshake is minutes and an attach is minutes (A-41, A-100…A-108), so a single screen timeout can consume more airtime than the transfer that was being waited for. It also repeatedly triggers the BLE disconnect/reconnect that ADR-0036 records as leaving some 1.17.1 boards deaf and mute (MeshCore #3582) — a lottery ticket per glance.
- Proposed fix (NOT applied): derive the grace from the route (the reason the mesh route exists is that a reconnect is expensive), or hold the connection while a shell is *claimed* and only stop painting; and set `FLAG_KEEP_SCREEN_ON` while a mesh terminal is `Live` with a shell claimed, so the one screen whose cost is minutes does not time out on its own.
- Open questions: whether Android's screen-off reliably calls `onStop` on the target device (it usually does, but the field log would settle it); whether keeping the screen on is acceptable for a battery-first radio user, or should be a setting.
- Investigator: app round · 2026-11-11

### A-202 · app: ldmux never yields the radio, so the lease only works in one direction · blocker

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/RadioLease.kt:62-99` (`acquire`) registers a receiver for `ACTION_GRANTED` **only**, sends `ACTION_WANTED`, and gives up after `ANSWER_MS = 3_000` (`:52`); `:103-112` (`release`) sends `ACTION_FREE` and logs; no receiver for `ACTION_WANTED` exists anywhere (`grep -rn "RADIO_WANTED\|ACTION_WANTED" packages/mobile/composeApp/src` matches only the send site), no manifest `<receiver>` in `AndroidManifest.xml:57-125`, and no persisted `yielded` state (`grep -rn "yielded"`: zero hits). ADR-0036 requires the opposite: rule 1 (the holder disconnects and answers `GRANTED`), rule 2 (its ladder is suppressed while yielded), rule 5 (`yielded` persists across process death), and "both apps declare their receivers".
- Finding: ldmux implements only the *requester* half of the contract it wrote. When Meshmore asks (it retries every 30 s, measured: `DISCONNECTED … reason=22` three times in a minute, ADR-0036 Context), ldmux answers nothing, holds its GATT link, and Meshmore — applying rule 4, "absence means free" — connects as a second central. That is precisely the eviction pattern the ADR was written for, and from the operator's side it is "handshake, fail, handshake, fail, sometimes the link dropped".
- Why it hurts at range: the radio is only usable while both apps honour the lease, and the side that fails to honour it is the one under test. Every Meshmore retry during a range test can kill a live session mid-ladder, and nothing on the ldmux screen says the radio was taken.
- Proposed fix (NOT applied): implement the holder half — a manifest `<receiver>` for `ACTION_WANTED` (exported, `setPackage`-addressed), persist `yielded` (`AppPrefs`), on request disconnect the transport, answer `GRANTED`, suppress the rebuild ladder while yielded, and clear `yielded` when the operator acts. Add a unit test for the state machine, since the lease currently has none (`grep RadioLease …androidUnitTest`: zero hits).
- Open questions: whether Meshmore actually broadcasts `WANTED` with `setPackage("io.ldmux.mobile")` (if it broadcasts implicitly, a manifest receiver would not receive it on Android 8+); and whether a yielded ldmux should keep its foreground service up (yes, per rule 1).
- Investigator: app round · 2026-11-11

### A-203 · app: the ten-minute give-up is unreachable, so a dead link retries for ever · major

- Evidence: `mesh/MeshTerminalScreen.kt:273` (`var downSince by remember(conn) { mutableStateOf<ValueTimeMark?>(null) }`), `:324` (`val since = downSince ?: markNow().also { downSince = it }`), `:332-345` (compute `downMs`, then `delay(...)`, then `attempt += 1`), `:257` (`conn = remember(plan, attempt)`), `:461` (`downSince = null` on every healthy view), `:206` (`LINK_GIVE_UP_MS = 10 * 60_000L`), and the comment at `:198-205` claiming "Ten minutes is a walk that turned round and came back … the service is released and the battery stops paying".
- Finding: `downSince` is remembered **keyed on `conn`**, and every rebuild creates a new `conn`. So the time-since-down is recomputed from zero on each rebuild and can never reach ten minutes; the give-up block is dead code. (The backoff `rebuilds` counter is keyed on `plan` and does persist, which is why the ladder keeps climbing while the give-up never fires.)
- Why it hurts at range: a link that is genuinely gone (out of range for good, wrong channel) rebuilds for ever — each rebuild being a BLE reconnect plus a fresh handshake ladder — while the phone sits in a pocket with a foreground service and a partial wakelock. That is both a battery defect and an airtime one: an unattended phone keeps spending the mesh's airtime on handshakes nobody is waiting for.
- Proposed fix (NOT applied): key `downSince` on `plan` (or hold it in a `rememberSaveable`/holder object) so it survives rebuilds, and clear it only from the healthy-view branch at `:454-465`.
- Open questions: whether the give-up should also be measured from the last *successful* link rather than the last failure, so a flapping link is not given up on between good moments.
- Investigator: app round · 2026-11-11

### A-204 · app: the rebuild backoff's documented ceiling is wrong by an order of magnitude · minor

- Evidence: `mesh/MeshTerminalScreen.kt:265-268` (comment: "on the same backoff: two seconds doubling to thirty, and back to two as soon as the link is live"), `:194-195` (`rebuildCapMs(rebuilds) = (2_000L shl rebuilds.coerceIn(0, 7)).coerceAtMost(300_000L)`), `androidUnitTest/.../MeshReconnectTest.kt:19-30` (asserts 256 000 at index 7 and 8, i.e. the cap is five minutes, not thirty seconds).
- Finding: the code and its test cap the backoff at **300 s**; the comment — and the sentence an operator would repeat — says thirty seconds.
- Why it hurts at range: after a run of failures the app can wait five minutes before its next rebuild, which is indistinguishable from "the app needs a refresh" (the exact complaint the ladder exists to remove) and is longer than the give-up it is supposed to sit under.
- Proposed fix (NOT applied): decide which figure is intended. If thirty seconds, cap `rebuildCapMs` at 30 000; if five minutes, correct the comment. Either way the number must appear once.
- Open questions: which one the 2026-10-31 field observation actually supported.
- Investigator: app round · 2026-11-11

### A-205 · app: the phone's durable log cannot say which transport was live · major

- Evidence: `AppLog.android.kt:14-21` (every `appLog` goes to `AppLogBuffer` **and** `AppLogFile` unconditionally), `AppLogFile.kt:26-55` (line = stamp + message; no tag, no route), `Link.kt:114-118` (`ConfigStore.update` logs nothing on a route change), `LdmuxClient.kt:531` (`appLog("connecting to ${endpoint.host}")` — a tailnet line written into the same file), `mesh/MeshTermDriver.kt` mesh lines in the same file.
- Finding: the one artefact a range failure leaves behind mixes the two transports with no marker. The 18:37 window — `connecting to 100.127.119.54:7625`, `sessions parsed=13 attachable=13`, `page …: connected, attaching (take over)` interleaved with mesh lines — was read during this audit as "the app used the tailnet while showing the radio"; the code makes that impossible (A-200), so it was a route switch nobody recorded.
- Why it hurts at range: the phone's file is the only field record (logcat rolls in minutes). A record that cannot say which link produced a line cannot answer the first question a reader has, and it cost this audit a wrong hypothesis that had already been entered as fact.
- Proposed fix (NOT applied): `appLog("route -> ${route.name}")` from `ConfigStore.update` on change, and one startup line naming the resolved route; nothing else in the format needs to change.
- Open questions: whether the tailnet's attach-level lines (`page …: attached`) should carry a `[tailnet]` prefix or whether the single route-change line is enough for a reader.
- Investigator: app round · 2026-11-11

### A-206 · app: the two routes are exclusive by construction, so the radio pays for the app being looked at · major (design)

- Evidence: `App.kt:120-142` (`if (mesh != null) MeshTerminalScreen(...) else AppContent(...)` — one branch or the other, nothing shared), `MeshTermScreen.kt:257` (connection remembered on the plan), `Config.kt:190` (`endpoint` is null unless `route == Tailnet`, `:202`), `MeshTermHost.kt:83` (no plan unless the route is mesh).
- Finding: there is no way for the mesh link to stay bound while the tailnet surface is drawn, and no way for the tailnet client to keep its socket while the mesh surface is drawn. Each route's connection is owned by a composable that only exists while that route is chosen.
- Why it hurts at range: the operator's natural behaviour — glance at the app, switch to the familiar route to see session state, come back — is exactly the sequence that resets the radio link (A-200). Making the radio survive a glance is the difference between "I can watch it work" and "every time I look, it starts over".
- Proposed fix (NOT applied): a route-independent holder for each link (a small `remember` outside the branch, or an app-scoped object), with the surface subscribing to it; then a route change selects which surface is *drawn*, not which link is *alive*. If that is too large a change, the minimum is a confirmation before leaving a live mesh link, and A-205's logging so the cost is at least visible.
- Open questions: whether two live links at once (tailnet socket + BLE) is acceptable on this device; whether the mesh driver's tick loop depends on being composed (it does not — it is a coroutine in the connection object).
- Investigator: app round · 2026-11-11

### A-207 · app: a refused lease still connects, so two apps can still fight over the radio · minor

- Evidence: `RadioLease.kt:56-61` (comment: "A refusal is logged and treated as 'not granted' — the connect still proceeds, because a link that is attempted and fails names its own reason"), `:96` (`withTimeoutOrNull(ANSWER_MS) { answer.await() } ?: false`), consumed at `AndroidRadioLink.kt:786-790` (`if (RadioLease.acquire(context)) appLog("granted by another app")` — no other use of the result).
- Finding: the return value is used only for a log line. A counterpart that answers "no" or answers nothing is treated identically, and the connect proceeds into a link the firmware will drop later.
- Why it hurts at range: a contended radio produces a link that works for seconds and then vanishes — the worst diagnostic shape — while the screen says nothing about contention. The operator reads it as a range failure.
- Proposed fix (NOT applied): on an explicit refusal, say so (`the radio is in use by <app>`) and retry after a short delay rather than connecting; keep "absence means free" for silence only.
- Open questions: whether Meshmore sends an explicit refusal at all, or only silence (which would make this unreachable in practice).
- Investigator: app round · 2026-11-11

### A-208 · app: two ldmux processes at once is structurally impossible — the field's two PIDs were sequential · question

- Evidence: `AndroidManifest.xml:77-99` (`android:launchMode="singleTask"`, `configChanges` covering orientation/size/density/font scale/locale, so recreation on rotation is prevented); no `android:process` on the activity or the service (`:100-125`); `BackgroundSession.android.kt:107,112` (`START_NOT_STICKY` — the service does not resurrect the process); `AndroidRadioLink.kt:2085-2130` (`RadioLinks` is an in-process refcounted registry keyed by address).
- Finding: the app cannot have two live activity instances, and the service cannot start a second process. The two PIDs recorded on 2026-11-10 (32486, 8114) are therefore almost certainly an older process's buffered logcat lines next to a newer process's — logcat interleaves by timestamp, not by process lifetime. Recent runs each showed a single PID (14826, 17182), consistent with that.
- Why it hurts at range: nothing here — but if it *were* real it would mean two holders of one radio, so the question is worth closing rather than carrying as a suspicion in every later report.
- Proposed fix (NOT applied): none; if it recurs, capture `adb shell dumpsys activity processes | grep io.ldmux` and `dumpsys bluetooth_manager | grep -c com.iotone` at the moment of the sighting, which names the holder directly.
- Open questions: whether the duplicate sighting came from `am force-stop` + `am start` performed by the investigation itself (it did, at 18:25).
- Investigator: app round · 2026-11-11

### A-209 · app: every connect and rebuild pays up to three seconds waiting for the lease · minor

- Evidence: `RadioLease.kt:52` (`ANSWER_MS = 3_000L`), `:96` (awaits the full window), `AndroidRadioLink.kt:786-790` (inside `connect()`, i.e. on every connection attempt and every rebuild).
- Finding: the requester waits the whole three seconds whenever the counterpart is installed but silent.
- Why it hurts at range: with a dead link rebuilding for ever (A-203) and a screen-off teardown per glance (A-201), this is three seconds added to each of dozens of attempts; it is also three seconds during which the radio is unclaimed by either app.
- Proposed fix (NOT applied): if a previous `acquire` in this process was answered or silent while no `FREE` has been sent, ask once and proceed without waiting on the retry, or make the wait configurable and short after the first exchange.
- Open questions: whether the counterpart's answer is fast enough in practice that the window is never fully waited out (the field log's `granted by another app` at 18:25:34 suggests an answer does arrive).
- Investigator: app round · 2026-11-11

---

95:**Worst finding:** A-200. Changing the route is a config write, and a config write destroys the radio link — GATT disconnect, then a fresh handshake and attach on return — with no log line saying it happened. The operator's own way of watching the work is the thing that guarantees the radio never finishes. A-202 is the close second: the app implements only the requester half of a lease contract it wrote, so Meshmore's 30-second retry can e

### A-220 · observability: the host's session death is computed and then thrown away · blocker

- Evidence: `crates/daemon/src/mesh_session.rs:411-431` — `poll` sets `self.last_error = Some("the phone stopped acknowledging: retries exhausted" | "the radio kept refusing a record …" | "the send queue filled …")` and then `self.session = None`, with no log call; `crates/daemon/src/mesh_session.rs:319` — the only line that ever prints that reason is gated on `if self.session.is_some()`; `crates/daemon/src/mesh_session.rs:330` — the same function then does `self.last_error = None` on success. The whole file contains exactly three `eprintln!` calls (`:235`, `:264`, `:321`), none of them in these paths. Contrast the phone, which logs `mesh drop: SessionFailed · three unanswered handshakes`.
- Finding: a session that dies **is set to `None` first**. When the phone re-handshakes and succeeds, `self.session.is_some()` is false, so `session replaced after Ns (…, reason)` does not print, and `last_error` is immediately cleared. The death reason is therefore emitted **nowhere at all** — not in the journal, and only in `mesh status` if someone happens to poll in the window between the death and the next handshake. The only line an investigation can reach (`session replaced after 5443s (hops=1, handshake refused: replayed or out-of-order timestamp)`, 2026-11-10 22:13:26) exists *only for the case where the old session was still alive when the new handshake arrived*.
- Why it hurts at range: at range the common death is retries-exhausted, which is exactly the silent path. Tonight's sequence — host `SESSION ESTABLISHED`, then nothing, then a phone that keeps handshaking — is the shape of a session dying with no trace, and it cost hours.
- Proposed fix (NOT applied): log unconditionally in `poll` at the moment `session = None` is taken, one line per death, naming the `Failure` and how many records were outstanding (`session.arq()` can be asked). Then relax `:319` so the *replacement* line always prints, with the previous session's reason captured at death rather than read from the mutable `last_error`.
- Open questions: whether the phone's own `SessionFailed` reason already arrives as a `Closed` record (it appears to; `Inbound::Closed` at `:212` sets a reason and is also silent) and so could be correlated for free once both are logged.

### A-221 · observability: a refused handshake writes nothing, so a locked-out peer looks like an idle one · blocker

- Evidence: `crates/daemon/src/mesh_session.rs:336-338` — the `Err(e)` arm of `on_handshake` does `self.refused += 1; self.last_error = Some(format!("session refused: {e:?}"))` and nothing else; there is no `eprintln!` anywhere in that arm (the file's only three are `:235`, `:264`, `:321`). `handshakes_refused` is published (`crates/daemon/src/mesh_session.rs:492`, `:79`) and printed by the CLI (`crates/daemon/src/bin/ldmux.rs:419`) — but only when someone runs the CLI.
- Finding: `Stale`, `Replay`, `PinMismatch`, `BadFilter` and `Crypto` are five faults with five different cures, and a run of them produces **zero journal lines** unless a session was established at some point (A-220). On 2026-11-10 the host refused every handshake for roughly an hour after 22:13:26 and the journal contains not one refusal line for that hour — only `route request from 26f6071f hops=N (adopted)`, which reads like everything is fine.
- Why it hurts at range: the operator's only instrument at range is the journal on the host and the app's log on the phone. A fault whose entire evidence lives in a counter nobody polls is indistinguishable from silence, and silence is the one thing a radio investigation is already drowning in.
- Proposed fix (NOT applied): log the first refusal of each distinct reason, then every 8th (`is_power_of_two` on `refused`), as `mesh_session` already does for dropped records (`:232-241`) — the same precedent, the same vocabulary. One line, a handful per hour.
- Open questions: whether a refusal should also count toward the link's own health so `supervise` stops reporting `ready` while a peer is being refused (see A-180 in the ledger, which found `state` stays `ready` regardless).

### A-222 · observability: `route_log_rare` suppresses *changes*, not repeats · major

- Evidence: `crates/daemon/src/mesh_serial.rs:217-221` — `if last.is_none_or(|t| t.elapsed() >= Duration::from_secs(60))`; the four call sites are `:1496` (`record {n} B sent by {carrier} over {hops} hop(s)`), `:2046` (`request from … ignored for routing: the peer is pinned`), `:2173` (`flooded record {n} B from a {h} hop chain`), `:2196` (`route from a flooded record hops={h}`). Each site has one `Option<Instant>`; none remembers what it last said.
- Finding: the limiter is purely temporal. If a message differs from the last one in every field — carrier flipped direct↔flood, record size changed, hop count changed — it is still dropped if 60 s have not passed. The two lines that would answer "is this link on flood or direct, and how far is the route now" are therefore suppressed precisely at the transitions they exist to show, and the journal instead shows an old value that reads as current.
- Why it hurts at range: tonight's central question was which carrier carried the handshake and when it changed. `record 133 B sent by flood over 0 hop(s)` appearing once a minute cannot answer it, and A-120 (the sticky-flood latch) was invisible for that reason among others.
- Proposed fix (NOT applied): make the limiter content-aware — log whenever the message differs from the last emitted one, and rate-limit only an *identical* repeat. A `(String, Instant)` pair per site instead of `Option<Instant>`; the "at most once a minute" property survives for a steady link and dies where it was always wrong.
- Open questions: whether a change-detecting limiter must also cap burstiness when a value oscillates every record (a direct/flood flap); a minimum interval per *distinct* message would cover it.

### A-223 · observability: "sent by flood" is printed before the send is attempted · major

- Evidence: `crates/daemon/src/mesh_serial.rs:1456-1493` builds `let attempt: Result<SendOutcome, SessionError> = match flooded { … }`; `:1496-1504` calls `route_log_rare(… "record {n} B sent by {carrier} over {hops} hop(s)")`; `:1505` is `let outcome = match attempt {`. The flood branch sets `sent_ok = true` if **any** of `RECORD_FLOOD_COPIES` copies was accepted and reports `Ok(SendOutcome::SentFlood)` regardless of how many did (`:1485-1489`).
- Finding: the line asserts "sent" from the *intent* — the value of `attempt` is not consulted, and cannot be, because it is matched afterwards. A send that ends in `Err` is logged as `sent by flood`. A burst where copy 1 was accepted and copies 2–3 were refused is also logged as `sent`, with no statement of how many of three reached the air.
- Why it hurts at range: copy count is the whole redundancy argument at the margin (A-34 in the ledger prices a rung at ~2.6 s of transmission on the assumption that three copies went out). A line that says three went when one did makes the arithmetic and the log disagree, and the log is what an investigation trusts.
- Proposed fix (NOT applied): move the line after `let outcome = …` and print the outcome and the accepted-copy count: `record {n} B over {hops} hop(s): {outcome} ({accepted}/{copies} copies)`. The flood branch already knows `sent_ok`; it need only count acceptances instead of folding them into a bool.
- Open questions: whether `send_retrying`'s internal retries should also be counted here, since a "refused" copy that succeeded on its second try is neither.

### A-224 · observability: the carrier decision and its reasons are nowhere — not in the journal, not in `mesh status` · major

- Evidence: `crates/daemon/src/mesh_serial.rs:1218` (`peer_path_proven`), `:1285` (`peer_via_flood`), `:1388` (`path_stale`), `:1401` (`direct_silent`), `:1408-1417` (`want_flood` composed from all of them) — none is carried into an `Event`; `crates/daemon/src/mesh.rs:265-322` (`MeshSnapshot`) has `peer_hops`, `last_error`, `session`, and no carrier field; `to_json` (`:363-412`) likewise. The only trace of a decision is the rate-limited outcome line (A-223).
- Finding: four booleans decide whether a record rides flood or direct, and the operator can see none of them. There is no "we are flooding because the peer's last record came by flood" line, and no `carrier` field in `mesh status`. ADR-0038's entire subject — escalation, decay, stickiness — is unobservable.
- Why it hurts at range: the latch in A-120 (both ends refusing direct until a direct record arrives) is a two-flag feedback loop, and a single published `carrier` plus the flag that caused it would have shown it in one `ldmux mesh status`. Instead it took a full audit round and a code read.
- Proposed fix (NOT applied): publish `carrier: "direct" | "flood"` and `carrierReason: "no-path" | "path-stale" | "direct-silent" | "peer-flooded" | "proven"` in `MeshSnapshot`/`to_json` from the same booleans, and log a line on **change** (which A-222's content-aware limiter would give for free).
- Open questions: whether the phone should publish the same two fields so the two ends can be compared at a glance; the phone has the symmetric flags (`directWorks`, `directSilent`, `floodTokens`).

### A-225 · observability: retransmissions are counted but never said · major

- Evidence: `crates/daemon/src/mesh_session.rs:53-56` (`retransmits`, `fast_retransmits`), `:78-80` (published as `retransmits`/`fastRetransmits`); `crates/daemon/src/bin/ldmux.rs:420-432` prints them, including the gloss "…of them pulled early by the peer's own ack". A grep for `eprintln` in `mesh_session.rs` returns three lines (`:235`, `:264`, `:321`) and none concerns retransmission.
- Finding: the one counter that would have exposed a retransmission storm is published only on demand, and the journal never mentions it. A record retransmitted 15–20 times (the arithmetic in A-80 of the ledger: RTO ~2.9 s against a ~43 s crossing) leaves no mark in the journal at all — the first visible symptom is the session dying silently (A-220).
- Why it hurts at range: retransmission is the dominant airtime consumer on a marginal link, and it is the subject of half the ledger's findings (A-80, A-102, A-108). All of those were found by reading code, because the logs could not show them.
- Proposed fix (NOT applied): log at the same power-of-two cadence already used for dropped records (`:232-241`): `mesh retransmits {n} so far ({fast} pulled early)`. Two lines an hour on a healthy link, seven on a bad one.
- Open questions: whether per-record (`seq N retransmitted k times`) is affordable; at the observed rates it is a handful of lines per session and would pinpoint which record is stuck.

### A-226 · observability: the host cannot distinguish "no coverage" from "the radio stopped listening" · major

- Evidence: the phone has exactly the line for this and says why: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1560-1588` — `reportHeard()` logs `mesh heard N packet(s) in {forSecs}s · last …` / `mesh heard nothing in {forSecs}s`, with the doc comment "**The one line that tells 'nobody covers this street' from 'this radio stopped listening'**". The host's receive path (`crates/daemon/src/mesh_serial.rs:1740-1910`) logs adverts (`:2124`) and its own traffic, and has no equivalent counter or periodic report.
- Finding: the host logs only what it *recognises* — adverts from any node, and records addressed to it. Everything else that arrives is parsed and dropped without a tally, so a host whose radio has gone deaf and a host in a dead spot produce identical journals. The phone solved this and the host did not.
- Why it hurts at range: A-180 in the ledger found that the host's silence watchdog can never fire because `Status` events are self-generated; a "heard N packets" line is the direct measurement that watchdog was trying to infer. It would also separate the two host-side failure modes that dominated tonight: radio wedged, and nobody within range.
- Proposed fix (NOT applied): count every `PUSH_CODE_LOG_RX_DATA` frame received, regardless of whether it was for us, and log it on the same cadence the phone uses (`reportHeard`), plus include it in `MeshSnapshot` so `mesh status` shows a live number.
- Open questions: whether the count should be split by payload type (the phone's `describeHeard` already names type and hop count) so "eight adverts, no floods" is visible at a glance.

### A-227 · observability: the phone logs every flooded copy and no direct send at all · major

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1047-1052` logs each flooded copy (`mesh record by flood {copy+1}/{RECORD_FLOOD_COPIES}: {result} · {size} B · dest {n}`); the direct path at `:1058-1107` logs nothing on success and nothing on an ordinary `Failed` result — the only lines it can emit are `mesh path not found; retrying zero-hop` (`:1086`), `errorFor`'s `radio refused a raw send: err {code}` (`:1895`), and the transport-level timeout at `:1108-1115` which does not log at all (A-228).
- Finding: the log is loud exactly where the design says the link should not be (flood) and silent exactly where it should be (direct). When the phone finally gets a proven path and switches to the cheap carrier, its durable log goes quiet and there is no way to tell a working direct link from a stuck one.
- Why it hurts at range: the durable log (`AppLogFile`, `files/mesh-log.txt`) is the only field record, and at range the interesting transition is flood → direct. Today that transition can only be inferred from the absence of flood lines, which is not evidence.
- Proposed fix (NOT applied): log the direct outcome with the same shape as the flood copies, on change rather than per record to stay cheap: `mesh record {n} B direct over {h} hop(s): {result}`, rate-limited the way A-222 proposes.
- Open questions: whether a per-record direct line is affordable at the observed send rate (it is: the log rolls at 256 KiB and direct sends are one line each).

### A-228 · observability: the phone's send timeout closes the transport without a word · major

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1108-1115` — `catch (e: TimeoutCancellationException) { _state.value = MeshLinkState.RadioUnavailable; closeTransport(); RadioSendResult.Failed }`, with no `appLog` between. The comment explains the design ("A timeout therefore ends the transport; the caller's rebuild is the only safe next step") but nothing records that it happened.
- Finding: a radio that stops answering the companion protocol — the failure this project has hit repeatedly, and the one the host's own watchdog was written for (A-180) — produces a state change and a closed transport on the phone with **no log line**. The `composeApp` state machine may say something later, but the event itself, with its timing, is lost.
- Why it hurts at range: correlating a phone transport death against the host's journal is how a wedge gets identified. Without a stamped line at the moment of the timeout, the two logs cannot be lined up, and the phone's log shows an unexplained gap.
- Proposed fix (NOT applied): one line before the close — `mesh radio stopped answering (send timed out after {SEND_TIMEOUT_MS} ms); rebuilding` — matching the vocabulary the host uses for `no reply to 0x05 within 5.0s`.
- Open questions: whether `closeTransport()` should log its own reason (it is also called from other paths) so every transport death is accounted for in one place.

### A-229 · observability: `x3` states the attempt count, not the number of copies that went · minor

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1684-1707` — the loop `for (copy in 0 until REQUEST_COPIES)` breaks on the first non-`Accepted` (`:1702`), then logs `"mesh route request (… ) x$REQUEST_COPIES: ${result ?: Failed}"`. `REQUEST_COPIES` is the constant, not the number sent.
- Finding: `x3: Failed` is printed when only the **first** copy was attempted and failed; `x3: Accepted` is printed only when all three were accepted, so the success case is honest and the failure case overstates the attempt. The field logs of 2026-11-10 contain many `x3: Accepted` (honest) and the ambiguity never bit — but the same pattern in the flooded-record loop is bounded differently (`break` on failure there too, but with a per-copy line, A-227).
- Why it hurts at range: route requests are the most frequent airtime spend while blind (three copies every 5–20 s per the ladder), and "how many actually went" is the question that decides whether to widen the spacing.
- Proposed fix (NOT applied): count accepted copies in the loop and print that number — `x{sent}/{REQUEST_COPIES}` — which costs one integer.
- Open questions: none material; this is a one-line change with no design content.

### A-230 · observability: `lastError` in `mesh status` can be hours old and reads as current · minor

- Evidence: `crates/daemon/src/mesh.rs:318` — the field's doc comment says "The last failure, if the link is down or recovering"; it is cleared only at connection open (`:831`), at a successful retune (`:923`), and at the top of the supervise loop (`:692`). During a long-lived link it is written by transient refusals (`:1052`, `:1073`, `:1079`), by a session send refusal (`:1163`), and by a downlink record with no session (`:1054`) — and never cleared afterwards.
- Finding: one transient `the radio refused record N (busy); its retry was given back` persists in `mesh status` and in `to_json`'s `lastError` for the rest of the link's life, presented beside a `state: ready`. The doc comment describes a field that does not exist.
- Why it hurts at range: a refusal is the ordinary state of a busy channel at range (the firmware defers sends, `:1451-1457`), so the field will almost always be populated, and an operator reading `lastError` learns nothing about whether the link is currently healthy.
- Proposed fix (NOT applied): either clear `last_error` on the next successful record send/receive (making it "the last failure, since the last success"), or publish `lastErrorAt` with it so its age is visible. The first is cheaper and matches the doc comment.
- Open questions: whether clearing on success would hide a failure that repeats every record but never consecutively.

### A-231 · observability: the two ends' logs cannot be joined, though the wire carries an identifier · major

- Evidence: the flood wrapper is `[dest hash][source hash][LDMR][4-byte nonce][record]` (`crates/companion/src/codec.rs:646-648`, built at `:663-668`); the parser returns only `record` and `path` (`:692-697`), so the nonce at `payload[6..10]` is checked for length and discarded (`:705-721`). On the host the answering side keeps the nonce in `answered_nonces` (`crates/daemon/src/mesh_serial.rs:1266`, `:2068`) but never prints it; on the phone the nonce is in scope where the copy line is written (`AndroidRadioLink.kt:1030-1051`) and is not printed either.
- Finding: a single flooded record carries a 4-byte nonce that both ends already handle, and neither log contains it. During a field test there is therefore no way to match "the phone sent copy 2 at 22:18:34" to "the host read a flooded record at 22:18:41" — the two logs can only be aligned by eye against wall-clock and record size, which is precisely what this investigation has been doing.
- Why it hurts at range: every crossing takes seconds and every attempt has three copies; without an identifier, "which copy arrived" and "did this attempt's answer land" are unanswerable, and those two questions are the core of the whole range problem.
- Proposed fix (NOT applied): return the nonce from `parse_record_flood` (`FloodedRecord { nonce: [u8;4], … }`) and print it in the two log lines that already exist, as `n={hex4}`. No wire change, no privacy question (it is a random 4-byte value already on the air).
- Open questions: whether the same treatment should apply to the route request's nonce (`req.nonce`, `mesh_serial.rs:2068`) so a request burst can be matched to its answer as well.

### A-232 · observability: no airtime figure anywhere is measured · major

- Evidence: the phone's `recordAirtime` (`AndroidRadioLink.kt:1728-1734`) is a no-op unless `budgetMode == RadioBudgetMode.Metered`, and even then it adds `RadioAirtime.timeOnAirMs(currentPreset(), otaBytes)` — an estimate from the preset, not a measurement. The host receives the radio's own counters from `GET_STATS` every `STATS_WATCH` = 20 s (`mesh_serial.rs:70`, `:1779-1811`) into `tx_air_secs`/`rx_air_secs` (`:1799-1800`), publishes them as `radioTxSecs`/`radioRxSecs` (`mesh.rs:375-376`), and never compares them with the per-record estimate (`status()` computes `tx_airtime_tenths_this_hour` from `settings.airtime_ms`, `mesh_serial.rs:2530-2545`).
- Finding: the system has both an estimate and a ground truth and never subtracts them. Every airtime figure the project reasons from — the ledger's `490 ms` per 133-byte record, the `2.6 s` per rung, the budget arithmetic — is arithmetic over a preset. The one measurement that could confirm or refute it is already fetched every 20 s and only displayed.
- Why it hurts at range: the whole design is airtime-first, and its central constant is unverified. Two consecutive `mesh status` readings of `radioTxSecs` across a known number of records would settle it in a minute.
- Proposed fix (NOT applied): log the delta — after each `Stats` event, `mesh airtime {tx}s tx / {rx}s rx, expected {n} × {airtime} ms` — or at minimum print `radioTxSecs` beside `txAirtimeTenths` so the two disagree visibly.
- Open questions: whether `tx_air_secs` counts only this radio's transmissions or all time the radio is keyed, and whether it accumulates across a firmware reset (it is `since boot`).

### A-233 · observability: a failed radio-stats read is silently skipped, freezing the only live signal · major

- Evidence: `crates/daemon/src/mesh_serial.rs:1781-1811` — `if !companion.has_pending() && last_stats.is_none_or(…) { last_stats = Some(Instant::now()); if let Ok(stats) = companion.radio_stats() { … } }`. The `Err` case has no branch at all: no line, no counter, no state change. `last_stats` is stamped *before* the attempt, so a failing read also does not retry sooner.
- Finding: `GET_STATS` is the only command whose answer proves the radio is still answering *anything*, and its failure is discarded. `snrQuarterDb`, `radioRssi`, `noiseFloorDbm`, `radioTxSecs` and `radioRxSecs` therefore freeze at their last good values while the snapshot keeps reporting `ready` — the same false-healthy shape A-180 found for the `Status` heartbeat, in a second place.
- Why it hurts at range: signal readings are how a range test is judged; a frozen reading presented as live is worse than no reading, because it looks like the link is still there when the radio has stopped talking.
- Proposed fix (NOT applied): on `Err`, count the consecutive failures and log at the power-of-two cadence already used elsewhere; after N consecutive failures, publish a `radioUnresponsive` state or clear the signal fields so nothing stale is shown.
- Open questions: whether `has_pending()` can mask the failure indefinitely (a pending tracked command suppresses the stats read entirely, so a stuck tracked command would also freeze the readings).

### A-234 · observability: the per-packet view — the only one that shows paths — is off by default and only reachable by editing a file · minor (question)

- Evidence: `crates/daemon/src/mesh_serial.rs:224-226` (`trace_on()` reads `LDMUX_MESH_TRACE`) and `:228-264` (`trace_rx` prints route, type, hops, width, length, SNR, RSSI, and an advert's key prefix); called at `:1840` and `:1899-1900`. The variable is supplier-settable through `EnvironmentFile=-%h/.config/ldmux/env` (`deploy/ldmux.service:97`), which requires an edit plus `systemctl --user daemon-reload`/restart.
- Finding: `trace_rx` is the only instrumentation that shows each received packet's shape — including the path hop count the receive log carries — and it is gated behind a file edit and a restart of the process that owns the radio. During a live field test, turning it on means ending the experiment.
- Why it hurts at range: the two readings that matter most (how far a flood really travelled, and what the radio heard in a window) are available only when the measurement cannot be taken.
- Proposed fix (NOT applied): make the trace a runtime switch the CLI can set (`ldmux mesh trace on --for 60s`), or at minimum include a rolling "last 20 packets heard" list in `mesh status` — the same information, sampled, without a restart.
- Open questions: whether the trace's volume (every packet, on a shared city mesh) makes a default of "off" correct and only the *reachability* wrong.

---

125:**The single line I would add first:** A-220's death line — one `eprintln!` at `mesh_session.rs:430` naming the `Failure` and the outstanding record count. Tonight's most expensive silence is a session that dies without saying why, and it is one line in the one place that already computes the answer.
127:**The line I would fix first:** A-223's `record {n} B sent by {carrier}` — it currently asserts an outcome it has not yet observed, and it is the line an investigation leans on to decide which carrier is working.
129:**What I could not determine:** (1) whether `Event::Signal` failures (A-233) ever occur in practice — the journal has no `GET_STATS` failure line to search for, which is itself the finding; (2) whether `tx_air_secs` is comparable across a firmware reset, so A-232's calibration is a 20-second experiment nobody has run rather than a guaranteed one; (3) whether the phone's `

### A-240 · security: a replayed `msg1` is an unauthenticated session-kill and a transmission amplifier · blocker

- Evidence: `crates/session/src/handshake.rs:316-333` (an equal timestamp, or one up to `HANDSHAKE_REPLAY_TOLERANCE_MS` behind, is now accepted); `crates/daemon/src/mesh_session.rs:194` (every `KIND_MSG1` goes straight to `on_handshake` — no dedup, no throttle); `:285-333` (`on_handshake` persists the pairing, **replaces** `self.session` at `:329`, `outbox.clear()` at `:330`, advances `next_index` at `:332`); `:692-708` (`write_private`: tmp, `sync_all`, rename, **directory** `sync_all`); `crates/daemon/src/mesh_serial.rs:2190-2221` (every flooded copy of a record reaches the session layer); `:513` and `:1468` (the answer is flooded **three times**); `docs/SECURITY.md` radios section — a flood is "a broadcast, so every node in range repeats and sees" it, which is exactly how the handshake rides at range (`record by flood 1/3, 2/3, 3/3 … 133 B` in the 2026-11-10 field log).
- Finding: an attacker within range captures one 121-byte `msg1` (a flood, heard by everyone) and replays the identical bytes. The host accepts it, **destroys the live session and its unacknowledged outbox**, performs two durable writes of the pairing file, and queues a reply that is transmitted three times as a flood. That can be repeated for as long as the captured timestamp stays inside the ±5 minute window (`TIMESTAMP_WINDOW_MS`, `handshake.rs:71`) — i.e. for five minutes per capture, and a new capture is available on every rung. Before `2ea77bb` an equal timestamp was refused as `Replay`, so a capture could not be replayed at all: this is a regression introduced by the availability fix, and it is the security half of the ledger's A-20.
- Why it hurts at range: at range the handshake *is* a flood, so the attacker needs no PSK and no position — only to be in range once. Each replay costs the attacker one small transmission and costs the mesh three flooded answers plus a full ladder restart (~2.6 s of air per rung, A-34), and the link can never stay up. It also makes the host transmit on a stranger's schedule, which is the amplification property `docs/SECURITY.md` claims the rate limits prevent ("so a peer cannot use either as an amplifier") — that claim is true for route requests and false for handshakes.
- Proposed fix (NOT applied): make `msg1` handling idempotent — keep `(msg1 bytes → reply, index)` per pairing and, on a repeat inside the window, re-send the cached reply and return **without** touching `session`, `outbox`, `next_index` or `handshakes`. That is both A-20's fix and this one: a replay becomes a duplicate answer instead of a session kill, and it separates the two questions "may I answer twice?" from "may I build a second session?". Add a floor on *distinct* accepted handshakes (one per pairing per N seconds) so a replay cannot be used as a free transmit trigger even when it is answered from cache.
- Open questions: whether the cached reply should be re-flooded at all (it still spends airtime an attacker chooses) or answered once and then dropped as a duplicate; whether the cache is keyed on bytes or on `(pairing, timestamp)`; whether the phone must treat a repeated identical `msg2` as stale (the ledger's A-20 says it already does).
- Investigator: security round · 2026-11-11

### A-241 · security: the replay floor is unreachable, so replay protection is only the ±5 minute window · major

- Evidence: `crates/session/src/handshake.rs:68` (`HANDSHAKE_REPLAY_TOLERANCE_MS = 10 min`), `:71` (`TIMESTAMP_WINDOW_MS = 5 min`), `:313-315` (window check), `:316-333` (floor check), `crates/daemon/src/mesh_session.rs:301` (`last_timestamp_ms = accepted.timestamp_ms`).
- Finding: `last_timestamp_ms` can only ever lie within 5 minutes of the host clock — it was accepted under that window — and a candidate timestamp must also lie within 5 minutes, so `last − ts ≤ 10 min` always. The floor refuses only when `last − ts > 10 min`. **The `Replay` branch cannot fire.** The only bound on a replayed handshake is the ±5 minute wall-clock window, which is exactly the bound A-240 exploits.
- Why it hurts at range: at range the link is retried for minutes and clocks are adjusted between attempts, so a 5-minute window is the whole defence — and the code reads as though a monotonic floor also protects it, which is how an audit (and this one's own A-26 note) came to write "the timestamp floor" as if it were two mechanisms.
- Proposed fix (NOT applied): pick one. Either make the floor able to act (a per-pairing counter that must strictly increase, carried beside the timestamp in the handshake payload — it is 8 bytes today and has room), or accept the 5-minute window as the only bound and delete the unreachable branch so no later reader counts protection that does not exist.
- Open questions: whether a 4-byte attempt counter can ride the existing payload without changing `MSG1_LEN` (it cannot — the payload is fixed at 8 bytes — so this is a wire change gated by `features`, or a re-purpose of the timestamp's low bits).
- Investigator: security round · 2026-11-11

### A-242 · security: a **direct** route reply is unauthenticated, and can steer the host's path · major

- Evidence: `crates/daemon/src/mesh_serial.rs:355-362` (`route_reply` = `parse_route_reply`, which checks the magic only), `:2241-2249` (accepted from **any** direct packet; `prefer_route` takes the shortest *unproven* path); `crates/companion/src/codec.rs:920-926` (magic only); contrast `codec.rs:894-916` (`parse_route_reply_flood` verifies the responder's full 32-byte key against `from_key`) and `mesh_serial.rs:2088-2100` (the flooded branch passes the pinned peer key).
- Finding: the flooded reply is bound to the pinned peer radio key; the direct reply is bound to nothing. Anyone who can send this radio a direct packet can inject a reply whose path is as short as it likes — a zero-hop path is well-formed — and `prefer_route` prefers it over the real 14–17 hop chain, after which every direct record is addressed to a neighbour broadcast that cannot reach the phone. The journal then prints the plausible `reply received hops=0`.
- Why it hurts at range: at range the flood carrier is the one that works, so the attack is not "the link dies" but "the cheap carrier never runs" — the expensive carrier permanently, with a log line that looks like a successful route. Cost to the attacker: one small direct packet, repeatable.
- Proposed fix (NOT applied): accept a direct reply only when it names the pinned peer radio key (the flooded parser already does this) or only while a request this end actually sent is outstanding — the reply echoes the request nonce (`codec.rs:742`). Either is a field comparison, not a protocol change.
- Open questions: whether any legitimate use exists for a reply from an unpinned sender (none is identifiable — the only peer is the paired phone).
- Investigator: security round · 2026-11-11

### A-243 · security: the routing peer is learned from unauthenticated traffic and persisted · major

- Evidence: `crates/daemon/src/mesh_serial.rs:2040-2065` (a route request adopts `settings.peer_key` when nothing is pinned, then `save_peer`s it), `:2230-2237` (a route **hint** does the same, unconditionally), `:1133` (loaded at start), `:2098` (the pinned radio key is what a flooded reply must match); `crates/daemon/src/mesh.rs:877` (the only clear is the manual `reset_pin` path); `crates/daemon/src/mesh_session.rs` pairing `pinned` is a **different** key (the Noise static).
- Finding: the host's notion of "the peer radio" — the key it addresses direct records to and the key it filters replies by — is learned from unauthenticated floods and written to disk, and is never checked against the authenticated Noise static key that the handshake pins. Two independent pins with one word between them: one authenticated, one not.
- Why it hurts at range: after a host restart (or on a fresh install where `peer_record` is absent) a stranger's request arriving first is remembered, the host addresses direct records to the stranger's radio, and the real phone's requests are refused for routing at `:2049-2058` ("the peer is pinned"). Nothing is disclosed — records stay sealed — but the direct carrier is aimed at the wrong radio and the phone pays the flood carrier for it, indefinitely, with the cure buried in a CLI flag.
- Proposed fix (NOT applied): never adopt a radio key from traffic that has not been authenticated; learn it at pairing (the QR can carry both keys) or from a sealed handshake record, and otherwise only while no pairing exists at all. Make the manual clear reset both pins.
- Open questions: whether binding radio key ↔ Noise static at pairing time is enough, or whether a roaming radio change (a replaced radio module) needs a re-pair anyway.
- Investigator: security round · 2026-11-11

### A-244 · security: `peerKey` in status means the Noise pin, not the routing peer · minor

- Evidence: `crates/daemon/src/mesh.rs:505-510` (`peer_key: setup.files.pairing.pinned.map(hex)`); `crates/daemon/src/mesh_serial.rs:956` (`peer_key: Option<[u8; 32]>` — a **radio** key), `:2232` (adopted from a hint).
- Finding: `ldmux mesh status` reports the Noise static key under the name `peerKey`, while routing uses a different key that may be a stranger's (A-243). One name, two concepts — the same class of defect the ledger already records for `handshakes` and `session replaced`.
- Why it hurts at range: the operator's only view of "which phone is pinned" shows a key that is not the one deciding where records go, so A-243 is invisible from status and a mis-aimed direct carrier reads as healthy.
- Proposed fix (NOT applied): report both, named for what they are (`noisePeer`, `radioPeer`).
- Open questions: none.
- Investigator: security round · 2026-11-11

### A-245 · security: the flood wrapper is unauthenticated by design; injected records cost one AEAD open each, with no rate bound · minor

- Evidence: `crates/companion/src/codec.rs:705-724` (`parse_record_flood`: destination hash byte, `LDMR` magic, nonce — **no MAC**), `:644` (`RECORD_FLOOD_MAGIC`); `crates/daemon/src/mesh_serial.rs:2452-2457`; `crates/daemon/src/mesh_session.rs:196-206` (transport kind → `session.receive`); `crates/session/src/record.rs:170-190` (tag checked after index and window).
- Finding: the wrapper is a routing envelope, not a seal, and the code treats it that way — the inner record's tag is the gate. The cost of an injected record is one ChaChaPoly open plus a receive-log parse, and nothing bounds how often an attacker may impose that.
- Why it hurts at range: the host must keep receiving while it answers; on a shared channel a receiver kept busy by injected garbage is a receiver that misses the phone's real records. The cryptographic cost is small and is *not* the amplification risk — the amplification risk is A-240.
- Proposed fix (NOT applied): none urgent. If it ever matters, count failed opens per window, cap them, and log the cap so the behaviour is visible rather than silent.
- Open questions: whether the firmware applies any rate limiting to flooded `REQ`/`RAW` reception before the companion is involved (`mcsim`/`mcrs` would answer this).
- Investigator: security round · 2026-11-11

### A-246 · security: `msg2` has no cheap-drop, so the phone performs two X25519 per injected 50-byte packet · minor

- Evidence: `crates/session/src/handshake.rs:186-196` (the initiator's `msg1` carries a 16-byte filter tag, checked before any Diffie-Hellman), `:209-211` (`Initiator::finish` checks only `len == MSG2_LEN` and `msg2[0] == KIND_MSG2` before `read_message`); Noise IK processes msg2 as `e, ee, se`, so the initiator performs two X25519 operations per attempt.
- Finding: the cheap-drop protection is one-directional. The host rejects a stranger's `msg1` for one HMAC per pairing; the phone cannot reject a stranger's `msg2` without two scalar multiplications. Anyone can flood well-formed 50-byte `0x12` packets and make the phone compute.
- Why it hurts at range: the phone is the end with a battery and no mains, and at range it is also the end that must stay listening. It compounds the ledger's A-21, where *any* record consumes a pending handshake: an injected `0x12` during a pending handshake both costs the DHs and destroys the attempt.
- Proposed fix (NOT applied): give `msg2` a filter tag of its own (`HMAC(psk, kind ‖ msg2)`, 16 bytes, which `MSG2_LEN` has no room for unless the record budget grows — so test it as a `features`-gated 66-byte message), or require the record to arrive with the session index the host just assigned (still forgeable) — the filter tag is the only cheap one. Fix A-21 first either way, so an injected record cannot consume a handshake.
- Open questions: whether two X25519 on a mid-range Android phone is measurable in battery terms over a flooding attack (a hundred packets is ~200 scalar mults, milliseconds — so this is a cheap attack but a small effect; graded minor for that reason).
- Investigator: security round · 2026-11-11

### A-247 · security: the public-channel rationale describes a disclosure the transport prevents · minor

- Evidence: `docs/adr/0026-public-channel-mode.md` ("The problem": "everything typed is broadcast to everyone in range … a password typed into a prompt … each keystroke is a frame on the air"); `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/PublicChannelHold.kt:11-12` (same claim); `docs/SECURITY.md`, radios section ("an observer learns that two radios are talking, and how much, but not what"); `crates/session/src/record.rs:1-25` (the application bytes are inside the AEAD).
- Finding: two documents disagree, and the one a user would read for reassurance — ADR-0026 — is the pessimistic and incorrect one. The text is sealed; what a public channel exposes is volume, timing, the peer's identity and the fact that a radio is talking. `PublicChannelHold` itself is a UI hold (`:9-30`), so the mode's real function is airtime and composition, not disclosure avoidance.
- Why it hurts: a security claim that is wrong in either direction is a defect. Here it justifies a mode on grounds that do not exist, and would equally justify removing a feature that is safe — and a user told "every byte is broadcast" may avoid a radio channel that is in fact confidential.
- Proposed fix (NOT applied): restate ADR-0026's "The problem" in `SECURITY.md`'s terms (volume and timing, not content), and state plainly that "public channel" is a composition mode over the same sealed session.
- Open questions: whether "public channel" was ever intended as a MeshCore group channel — the code says it is not, and ADR-0037 rejects `GRP_DATA`.
- Investigator: security round · 2026-11-11

### A-248 · security: a refusal-reason record must not become an oracle — guidance for the ledger's A-25 · question

- Evidence: `crates/session/src/handshake.rs:75-89` (`HandshakeError`: `Malformed`, `BadFilter`, `Stale`, `Replay`, `PinMismatch`, `Crypto`), `crates/daemon/src/mesh_session.rs:293-295` (`refused += 1`, `last_error`, no wire message); ledger A-25 proposes sending the reason to the phone.
- Finding: A-25's proposal is right for the operator and carries two obligations that are easy to miss. `BadFilter` must stay **silent**: answering it tells an attacker whether a guessed PSK's filter tag matched, converting a 2^−128 check into an online oracle for the existence of a pairing. And the phone must treat the reason as **display only**, because the record is unauthenticated and anyone in range can forge "your host refused: pin mismatch". Everything else (`Stale`, `Replay`, `PinMismatch`, `Crypto`) is learnable by anyone already holding the PSK, so sending those is safe.
- Why it hurts at range: at range the phone's durable log is the only diagnostic, so A-25 is worth doing — but done carelessly it hands an attacker a PSK oracle and a spoofing surface, which is worse than the current silence.
- Proposed fix (guidance, not applied): emit reasons only for the codes a PSK holder could derive locally; never for `BadFilter`; rate-limit one per pairing per minute; the phone logs it and shows it, and never acts on it.
- Open questions: A-26's follow-on proposal — sending the host's clock with `Stale` — is a timing disclosure to everyone in range; probably acceptable, but it should be a stated decision rather than a side effect.
- Investigator: security round · 2026-11-11

### A-249 · security: the advert's identity and possible location are the radio's, and ldmux can neither see nor state them · question

- Evidence: `crates/companion/src/session.rs:199-223` (`send_self_advert` — the **firmware** builds the packet); `crates/companion/src/codec.rs:1038-1039` (advert info carries `adv_lat`/`adv_lon`), `:1245-1246` (decoded as micro-degrees); `crates/daemon/src/mesh_serial.rs:2105-2130` (the host logs every advert it *hears*); `docs/SECURITY.md` ("publishes its node name and public key to the mesh").
- Finding: ldmux cannot read or set the advert's name, and the advert format carries a latitude and longitude that the firmware fills from the radio's own configuration. `SECURITY.md` mentions the name and the key but not the location field. The host logs every advert received and says nothing about what its own publishes.
- Why it hurts at range: an operator on a public city mesh is broadcasting an identity they configured in a different app, and the one document that describes the exposure omits the field that is most sensitive. The mesh at range is a public place, which is exactly where this matters.
- Proposed fix (NOT applied): log once at start what this radio's advert carries (name length, whether a location is present) so the operator can see it; add one sentence to `SECURITY.md` naming the location field and saying it comes from the radio's configuration, not from ldmux.
- Open questions: whether the host's advert leaves the neighbourhood at all, given `flood.max.advert` defaults to 3 (the ledger's A-60 — our adverts were received at 5–12 hops locally, so this mesh is permissive); whether the phone app can read the advertising radio's own advert content through the companion API, which would let ldmux show it.
- Investigator: security round · 2026-11-11

### A-250 · security: what is genuinely sound, recorded so a later round does not re-litigate it · minor (no defect)

- Evidence: `crates/session/src/handshake.rs:24-47` (the audit comment: two cipherstates, nonce = record counter, rekey by a fresh handshake rather than `Rekey()`, constant-time filter compare, prologue binding); `:157-170` (`filter_key`/`filter_mac` — `HMAC(psk, label)`, so the PSK never leaves Noise); `crates/session/src/record.rs:20-30`, `:152-190` (full 16-byte tag, `u32::MAX` refused, replay window committed only after the tag verifies); `crates/session/src/session.rs:94` (counter starts at 0 per session), `:187` (rekey bounds); `crates/daemon/src/mesh_session.rs:692-708` (`0600`, tmp+rename, directory fsync); `docs/SECURITY.md` secrets section.
- Finding: the payload protections are right and match the Noise specification's own rules: separate keys per direction on a half-duplex link, a nonce that cannot repeat because a retransmission is re-sealed with a new counter, a 128-bit tag never truncated, a replay window a forgery cannot advance, a domain-separated filter key, the suite bound into the prologue so a rollback is a handshake failure, forward secrecy per session, rekey by fresh handshake, and credentials `0600` outside the tmpfs runtime dir.
- Why it matters that this is written down: four rounds of this audit have found availability and state-machine defects around the crypto while the crypto itself held. A later reader should not relax the record layout, the filter derivation, the prologue or the rekey rule believing they are untested judgements.
- Proposed fix: none. Recorded as a finding so it is not re-opened without new evidence.
- Open questions: none beyond A-246 (`msg2` has no cheap-drop) and A-241 (the floor is dead), which are recorded separately.
- Investigator: security round · 2026-11-11

---

**Judgement on the existing entries, as asked.** A-20 is right about the availability consequence and **incomplete**: it misses that the same duplicate-acceptance turns a captured handshake into a replayable session-kill and a transmit trigger (A-240). A-26 is right that the clock dependency is a hard failure, but the fix as landed leaves the replay floor **arithmetically unreachable** (A-241) — the audit's framing of "the timestamp floor" no longer describes the code. A-27 is correct as recorded: a stale floor cannot protect a new PSK, and an attacker replaying an old `msg1` after re-pairing would need the PSK that the re-pair revoked.

108:**Genuinely sound:** the record layer and the handshake's cryptographic construction (A-250) — separate directional keys, nonce-from-counter with re-sealing per retransmission, a full tag, a post-tag replay commit, a domain-separated filter, prologue-bound suite, fresh-handshake rekey, and `0600` persisted secrets.
110:**Also sound, worth noting:** route requests are bounded rather than free — `ROUTE_REPLY_FLOOR = 600 ms`, `answered_nonces` (16), `FLOOD_BURST = 4`, and the pinned-peer refusal at `mesh_serial.rs:2040-2065` mean a request flood cannot be used as an amplifier without the PSK. That is the property `SECURITY.md` claims, and it holds for requests — it is handshakes where it does not.
112:**What I could not determine:** (1) whether a replayed `msg1` re-derives identical keys — it does **not**, because the responder contributes a fresh ephemeral in Noise IK, so the consequence is availability and airtime, never record injection (this is why A-240 is graded a session-kill rather than an integrity break); (2) whether the local repeaters enforce any pre-companion rate limit on fl

The short answer to "would any test have caught these": **two were unreachable, two were unmeasurable, two were tested at the wrong layer or pinned as intent.** None was a missing assertion in an otherwise-covered path.

### A-260 · tests: five of the six blockers live where no harness can stand · blocker
- Evidence: `crates/daemon/src/mesh.rs:685` — `async fn supervise(...)` is private, and the whole `mesh.rs` test module is config/snapshot only (`:1739`, `:1755`, `:1787`, `:1803`, `:1815`, `:1852`); `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:65` — `class AndroidRadioLink(private val context: Context, …)`, and no file under `androidUnitTest/` references it, while *pure* functions in the same source set (`RadioRoute.kt`) are tested (`RadioRouteTest.kt:152`, `:274`), so the barrier is the `Context`, not the source set; `crates/companion/src/link.rs:225` — `MemoryLink` is two byte buffers with no clock, no refusal and no reordering.
- Finding: the three harnesses that exist cannot express what the blockers are about. The daemon's session loop is uninstantiable (private `supervise`), the phone's carrier policy is inside a BLE adapter (needs `Context`), and the only fake carrier has no time. A-120 and A-180 are therefore not *untested* — they are *untestable as written*; A-61 and A-102 are testable but have no harness that can see a transmission.
- Why it hurts at range: every range-only defect in this audit is a timing-and-state defect. The topology has no place to put either, so the only detector is a field trip.
- Proposed fix (NOT applied): three small seams, in cost order. (1) A `ScriptedLink` in `crates/companion` with a virtual clock and injectable delays/refusals, plus `pub(crate)` on `supervise` (or a `#[cfg(test)]` entry point), which makes the loop testable. (2) Move the phone's carrier decision out of `AndroidRadioLink` into a pure `commonMain` function, exactly as `routeRefusal` already is for routes — the adapter keeps I/O only. (3) Make the harnesses count transmissions, not records (A-264).
- Open questions: whether `MeshRuntime`'s `wake` handle is enough to drive `supervise` deterministically, or whether the loop needs a clock trait; how much of `AndroidRadioLink` is pure enough to move without touching BLE.
- Investigator: tests round · 2026-11-11

### A-261 · tests: the multi-hop simulation's physics is three to five times too optimistic · major
- Evidence: `crates/session/tests/lossy_multihop.rs:44` — `one_way_ms: hops * airtime_ms`, stated as the model at `:3-4` ("each holding it for up to an airtime") and `:156` ("Twenty-eight hops is the chain measured on 2026-11-10"); against A-41's measured `calcRxDelay` of ≈2.7 s per hop for a 133 B record where the airtime is 0.49 s.
- Finding: the only multi-hop harness models a repeater as propagating within one airtime. At 28 hops that is 16.9 s one-way where the firmware's own hold makes it roughly 60–115 s. Both of its tests (`:162` "2 KB across twenty-eight lossy hops", `:203` "a clean route costs about one record per chunk") pass with timers that are wrong on the real link, and it feeds the session the *true* hop count (`:117-118`) rather than the one the carrier would publish.
- Why it hurts at range: this is the harness whose docstring says it exists because "the timers are the part no amount of reading settles". It cannot catch A-41 or A-80, because one is an error in *how long a hop takes* and the other in *which hop count the session is told* — neither of which it models.
- Proposed fix (NOT applied): add a per-hop hold to `Air::new` (`hold_ms`, taken from a `calc_rx_delay(score, airtime)` table — `mesh-research/clones/mcrs` and `mcsim` both carry one), and add one test that sets `hops` to what the carrier's shortest-inbound rule would publish (1) while the air is a 16-hop route. That second test reproduces A-80 with no radio at all.
- Open questions: whether the score→delay table is stable enough across firmware versions to pin in a test, or should be a parameter.
- Investigator: tests round · 2026-11-11

### A-262 · tests: the daemon's connection loop has no test whatsoever · blocker
- Evidence: `crates/daemon/src/mesh.rs:967` (`last_carrier = Instant::now()` on any event), `:1118` (the watchdog), `:1123-1140` (credit expiry), `:685` (`supervise`); grep for "stopped answering" across `crates/daemon/src` and `crates/daemon/tests` finds the code and no test.
- Finding: nothing drives `supervise`. The silence watchdog, the credit-expiry sweep, the carrier-event handling and the reconnection decision are entirely unverified. A-180 (a `Status` every 500 ms that never touches the radio defeats the watchdog) could not fail a test because no test runs the loop.
- Why it hurts at range: the loop *is* the range behaviour — it decides when a dead carriage is replaced. Its only current test is the user restarting the daemon by hand.
- Proposed fix (NOT applied): same seam as A-260 item 1; the first test should be "a carrier that publishes status but answers nothing ends the connection within `silence`", which fails today by A-180.
- Investigator: tests round · 2026-11-11

### A-263 · tests: the phone's carrier policy is not referenced by any test, and its fix shipped with none · blocker
- Evidence: `git show --stat ce0e600` → one file, `AndroidRadioLink.kt`, +13/−1, no test; grep for `directWorks`, `floodTokens`, `floodWanted` across `androidUnitTest/` and `commonTest/` → zero hits; the policy itself at `AndroidRadioLink.kt:1013` (`floodWanted = (!directWorks || directSilent) && floodTokens > 0`), `:2022` (`FLOOD_BURST = 4`), `:2032` (`RECORD_FLOOD_COPIES = 3`).
- Finding: A-120's latch is a two-flag feedback rule with four state transitions and no test exercises any of them. The commit that introduced it added no test, and (A-260) could not have: the rule is inside an adapter that needs a `Context`.
- Why it hurts at range: this is the rule that decides which carrier every byte takes. It was changed twice in one evening (`ce0e600`, then the bucket gating at `:987`), both times unverified, and the second change is what starved the handshake copies.
- Proposed fix (NOT applied): extract to a pure function (`fun floodWanted(directWorks: Boolean, directSilent: Boolean, tokens: Int): Boolean`) plus a small state object, and test the latch: "one flood heard, then silence ⇒ direct is never attempted again" would have failed A-120 immediately.
- Investigator: tests round · 2026-11-11

### A-264 · tests: no harness counts transmissions, so the cost findings are outside what tests can see · major
- Evidence: `crates/daemon/src/mesh_serial.rs:513` `const RECORD_FLOOD_COPIES: u32 = 3` and `AndroidRadioLink.kt:2032` `const val RECORD_FLOOD_COPIES = 3` — two independent literals, referenced by no test (grep) and absent from the cross-language fixture that pins the record limits (`MeshTermCodecTest.kt:139-150` pins `recordFloor`, `recordMax`, `creditWindow`); the existing cost assertions count *records handed to a link* (`crates/daemon/tests/mesh_paint.rs:123` `records <= 4`; `crates/daemon/src/mesh_bridge.rs:4708` `records <= 3`).
- Finding: the quantity that matters at range — transmissions on the air — is measured by nothing. Records are the link's input; airtime is copies × retries × records. A-102/A-108 (an attach costing 18–27 transmissions, A-23's replacements) cannot fail a test even in principle, and the two ends' copy counts could drift apart silently.
- Why it hurts at range: every blocker in this audit is a multiplier on the same transmission count. A gate that cannot see the multiplier cannot see the regression.
- Proposed fix (NOT applied): give the test link a counter for `send` calls per record (the `CountingLink` in `crates/daemon/tests/airtime.rs:38` already does this) and make one attach's transmission count an ordinary assertion; pin `RECORD_FLOOD_COPIES` and `FLOOD_BURST` in the cross-language fixture beside the record limits.
- Open questions: whether `records_left`/`l2_slots` already imply the copy multiplier at the session boundary, which would let the assertion live in Rust alone.
- Investigator: tests round · 2026-11-11

### A-265 · tests: the only airtime measurement is excluded from the gate · major
- Evidence: `crates/daemon/tests/airtime.rs:239` `#[ignore = "measurement, not a gate: run with --ignored --nocapture"]`, and the module doc at `:3` telling a reader to run it by hand; `scripts/test-parallel.py` runs the default set only (`-- --ignored` is documented at `:20` as "the slow tier").
- Finding: the project's single instrument for the thing this audit is about runs only when someone remembers, and nothing records its output between runs, so there is no history to regress against.
- Why it hurts at range: cost is the constraint. An ungated measurement is a number nobody looks at until the link is already failing.
- Proposed fix (NOT applied): keep it `#[ignore]`d, but have it write a JSON baseline into the repo and add a cheap default-set test that fails when a recorded workload's records/airtime exceed the baseline by a margin. That converts a measurement into a ratchet without adding slow tests to the gate.
- Investigator: tests round · 2026-11-11

### A-266 · tests: the Kotlin suite is in no automated gate, and there is no CI · major
- Evidence: `scripts/test-parallel.py:2-10` (Rust binaries only); `flake.nix:242` (`cargo test --workspace`); `flake.nix:276-289` — `mobile-test` runs `node --test packages/mobile/test/*.test.ts` only; no `.github` directory exists (despite `flake.nix:246` saying such tests "run in the dev-shell gate and in CI").
- Finding: 72 `MeshTermDriverTest` cases plus the cross-language constant fixture are not run by `nix flake check` and not run by any CI. The phone half of the protocol — where A-21 and A-100 live — is verified only when a person types `./gradlew testDebugUnitTest`.
- Why it hurts at range: the failure shape of tonight was phone-side (data discarded after a host-side success). A gate that only exercises the host cannot see it.
- Proposed fix (NOT applied): add a `checks.mobile-unit` that runs `:composeApp:testDebugUnitTest` offline in the devshell toolchain (the deps are already vendored: `nix develop ..#mobile --command ./gradlew --offline` works), or at minimum document the two commands as one script so the gate cannot be half-run.
- Investigator: tests round · 2026-11-11

### A-267 · tests: two tests *pin* blockers as intended behaviour · major
- Evidence: `packages/mobile/composeApp/src/androidUnitTest/kotlin/io/ldmux/mobile/mesh/MeshTermDriverTest.kt:1380` — `a handshake record that cannot be finished ends the connection`, body `d.onRecord(byteArrayOf(7))` while a handshake is pending, asserting `LinkDown`; `crates/daemon/src/mesh_serial.rs` prefer_route test asserting `prefer_route(Some(&path(4)), Some(now), false, false, &path(2))` — "later, shorter copy" — introduced by `a80523c` ("the two ends agreed on nothing, because neither kept the shortest path").
- Finding: A-21 and A-80's routing half are not coverage gaps; they are requirements with tests. A one-byte record finishing a pending handshake is asserted as correct, and the shortest inbound copy displacing the route in hand is asserted as correct. Any fix must argue with these tests rather than add coverage around them.
- Why it hurts at range: both rules are defensible for a neighbour and wrong for a 16-hop link — the first converts a late answer into a dead link, the second hands the timer a hop count of 1. A test that encodes the wrong regime is worse than no test, because it makes the fix look like a regression.
- Proposed fix (NOT applied): change each test to state the range case explicitly — a record whose kind is not `0x12` while a handshake is pending is ignored, and a shorter inbound path may change *where we send* but not *how long we wait*.
- Investigator: tests round · 2026-11-11

### A-268 · tests: the A-20 fix was tested one layer below where the defect lives · major
- Evidence: `git show --stat 2ea77bb` → `crates/session/src/handshake.rs`, `crates/session/tests/handshake_and_record.rs` (+23), `crates/session-ffi/tests/abi.rs` (+9); the consequence lives in `crates/daemon/src/mesh_session.rs:283-334`; every `on_record` call in its tests uses a distinct timestamp (`:751`, `:848`, `:887`, `:937`) or two different ones (`:976`, `:983`), and none sends the same bytes twice.
- Finding: the commit proved the responder *accepts* a duplicate, and never asked what the host does when it accepts three. The new assertion's own wording — "an equal timestamp is a duplicate copy of the same attempt" — assumed an idempotence that did not exist, so the test documented the defect as if it were the fix.
- Why it hurts at range: this is the most instructive failure in the audit. A test at the wrong layer does not merely miss the bug, it certifies it.
- Proposed fix (NOT applied): `on_record(same msg1)` twice must leave `handshakes == 1`, `next_index` unchanged and `outbox.len() == 1`; that one test fails today and would have blocked `2ea77bb`.
- Investigator: tests round · 2026-11-11

### A-269 · tests: no test connects route choice to the retransmission timer · major
- Evidence: `Session::set_hops` is called from tests only at `crates/session/tests/lossy_multihop.rs:117-118` with the true hop count; in production it is fed by `crates/daemon/src/mesh_serial.rs:1877-1890` (`hops_now` from the chosen `peer_path`) into `crates/daemon/src/mesh_session.rs:177`; the test that names the status path (`mesh_serial.rs:3191`, `the_budget_is_republished_while_the_link_is_idle`) asserts only `statuses >= 2` (`:3232`) and never `hops`.
- Finding: the seam where A-80 happens — chosen path → `Limit.hops` → `set_hops` → `rto_initial` — is untested on both sides. Each half is tested alone: `prefer_route` by unit assertions, `rto_initial` by `lossy_multihop` given the true count.
- Why it hurts at range: every composed rule in this system fails at the seam, and the seams are exactly what the tests do not cross.
- Proposed fix (NOT applied): a `mesh_session` test — `set_hops(1)` then assert `first_rto_ms()` equals `6 × airtime`, and a mesh_serial test that publishes a 16-hop flood record and asserts the resulting `Limit.hops` is not 1. The pair makes the seam visible.
- Investigator: tests round · 2026-11-11

### A-270 · tests: `reply_delay` and the blocking send path have no test at all · minor
- Evidence: `crates/daemon/src/mesh_serial.rs:524` (`fn reply_delay`), used at `:335` (`std::thread::sleep(reply_delay(airtime_ms))`) inside the push handler; `send_retrying` at `:490-505` sleeps `200 × 2^round` ms up to three rounds; grep for `reply_delay` across `crates/` finds those two lines and no test; `git show --stat f413baf` → `mesh_serial.rs` + `RELEASE-AUDIT.md`, no test.
- Finding: a delay deliberately inserted into the thread that also reads the radio has no unit test for its bound and no assertion that the loop stays responsive while it sleeps; the retry ladder adds up to 1.4 s of additional sleeps plus send deadlines in the same thread.
- Why it hurts at range: A-61 is exactly "we sleep where the reference implementation enqueues". The test that should exist is not about the delay's value but about arrivals during it.
- Proposed fix (NOT applied): a `ScriptedLink` test that pushes a request, then a second record 100 ms later, and asserts the second record is *read* before the reply is sent — i.e. that the answer is queued, not slept on.
- Investigator: tests round · 2026-11-11

### A-271 · tests: nothing tests a full attach, or a replacement while one is in flight · major
- Evidence: `crates/daemon/tests/mesh_paint.rs:123` asserts `records <= 4` for a first paint; there is no test that drives list → profile → grid → first frame as one sequence, and `MeshTermDriverTest` has no case where a handshake completes (or a session is replaced) while a `ListRequest` is outstanding; A-100's buffer (`MeshTermDriver.kt:471`, cleared only at `:2145`) is asserted nowhere.
- Finding: the failure the user reported — "asking host for shell", counts to 100, resets to 0 — has no test shape. Nothing asserts what happens to application state in flight when the session beneath it is replaced (`mesh_session.rs:329-330`), and nothing asserts that a repeated run's pages cannot merge with an abandoned one.
- Why it hurts at range: this is the user-visible symptom of the most common range event (session replacement), and it is entirely unrepresented in the suite.
- Proposed fix (NOT applied): two tests — (1) host: replace the session mid-list, assert the outstanding answer is reported rather than dropped silently; (2) phone: start a list run, fail the session, start another, assert the committed list is the new run's alone.
- Investigator: tests round · 2026-11-11

### A-272 · tests: worth adopting from `mcsim` — two pieces, not the simulator · question
- Evidence: `mesh-research/clones/mcsim/crates/mcsim-link/src/predict.rs:112` (SNR thresholds per spreading factor), `:637` (`let link_margin = median_snr - snr_threshold`), `:208-215` (Excellent/Good/Marginal/Unreliable bands); `mcrs` and `mcsim` both implement the repeater hold delay.
- Finding: the two things this project's tests lack — a reception-probability model and a per-hop hold — are both small, both already written in a clone here, and neither requires the simulator's air interface. Adopting the whole of `mcsim` is a much larger claim than the evidence supports.
- Why it hurts at range: with those two pieces, the existing `lossy_multihop` becomes able to fail on A-41 and A-80; without them it stays a harness that validates timers against physics five times kinder than the field.
- Proposed fix (NOT applied): copy the SNR→reception table and the `calc_rx_delay` formula into the test harness as two pure functions, with a comment naming the clone and the firmware function. No new dependency.
- Open questions: whether the reception model should drive loss probabilistically (flaky tests) or as a deterministic score threshold (preferable); how to keep the harness fast.
- Investigator: tests round · 2026-11-11

---

102:**The single test to write first:** `on_record(same msg1 bytes) twice leaves one session` — it fails today, it is four lines, it sits at the layer where A-20 lives, and it is the defect currently costing every field trial.
104:**Is simulation with `mcsim`'s model worth the effort:** yes, but only two of its pieces (A-272). The existing `lossy_multihop` is the right harness with the wrong physics; adopting `mcsim` wholesale would be a new project, and the host loop (A-180, A-220) is a bigger hole than the physics is.
106:**Could not determine:** (1) whether `supervise` can be driven deterministically without a clock trait, since it awaits `tokio::select!` over a timer and a `Notify` (`mesh.rs:1103-1106`) and the timer is wall-clock; (2) whether `AndroidRadioLink`'s carrier state can be moved to `commonMain` without pulling BLE types with it, since `RadioLink`'s interface is already common (`commonMain/.../Radi

### A-280 · ux: the verdict replaces the only moving evidence, and the screen goes dead-still for ~85 % of the attempt · major

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:1454-1476` — `waiting = when (v) { … MeshTermView.NoAnswer -> null … }`, so the `WaitingLine` (the seconds, the tries, the route-request count) is drawn **only** for `Handshaking`/`ProfileMissing`/`Listing`; `MeshBody.kt:33-35` then draws two static sentences; `MeshTermDriver.kt:2348-2357` — `NoAnswer` is chosen when `clock() - handshakeBeganAt > answerDeadlineMs(handshakeVerdictMs = 30_000, gateway.routeHops)`; `MeshTermDriver.kt:2612-2622` — the actual give-up is `handshakeWaitMs(hops) = rung × 7 + max(rung, 120_000)` with `rung = answerDeadlineMs(8_000, hops)`. At `hops = 0` that is 176 s; at `hops = 14`, 372 s. So the static verdict covers **83–85 %** of the attempt. `MeshTermDriver.kt:892` (`hopsNow() = gateway.routeHops`) means the verdict's own hop term is the *shortest inbound* path that A-80 shows can read 1 while the chain is 16, which makes the verdict earlier still.
- Finding: for the first thirty-odd seconds the launcher says `handshaking · 12 s · 3 tries · 4 route requests, no answer from the mesh`, then it replaces that with `no answer from host / check the host is on the same channel, then refresh` and **nothing on the screen changes again** — no seconds, no counter, no evidence — while the driver keeps re-asking for minutes. The one part of the UI that proved the app was still working is deleted at the exact moment the wait gets long.
- Why it hurts at range: this is the reported "it just counted to 100 and then went back to 0". A range user cannot distinguish a link that is grinding through its ladder from one that has died, so they press refresh (A-284) or switch routes (A-200), and both restart the ladder from rung one. The airtime spent so far is discarded, and the retry competes with the one already running.
- Proposed fix (NOT applied): keep the `WaitingLine` in `NoAnswer` and change its wording instead of removing it — e.g. `still trying · 47 s · 4 tries · 6 route requests, no answer from the mesh` — so the verdict adds a sentence rather than replacing the evidence. Derive the verdict from `handshakeWaitMs(hops)` (or a fraction of it) rather than a separate `handshakeVerdictMs`, so the sentence and the give-up cannot disagree.
- Open questions: whether `NoAnswer` should carry the elapsed total (`attemptsThisLink`) as well, so a user watching for ten minutes sees one number that never resets; whether the verdict should be per-link rather than per-connection.
- Investigator: ux round · 2026-11-11

### A-281 · ux: the one warning that the link stopped carrying is painted in the good-news colour · minor

- Evidence: `MeshTerminalScreen.kt:1354-1360` — `quietSecs > 0L -> Notice("link quiet · ${quietSecs} s", ok = true)`; `Controls.kt:179-186` — `Notice(text, ok)` renders `color = if (notice.ok) Muted else Warn`; `Controls.kt:171-178` — the type's own doctrine: *"A status line that cannot tell good news from bad is worse than no line: the user learns to distrust the colour, and the one failure that matters looks like the last success."* The same `ok = true` is used for `waiting for send budget` and `full-screen program: updates large and slow`, both of which are benign; `ok = false` is used for `too much typed: wait for send`.
- Finding: `link quiet · 45 s` is the app's statement that a session is live and the peer has stopped answering — the most serious thing that notice line ever says — and it is drawn in the muted colour reserved for information. Every other notice in that block is correctly weighted, so this single flag is an inversion, not the convention.
- Why it hurts at range: this is the line that should tell a user "the link is not carrying, stop typing". At range the link also goes quiet legitimately for tens of seconds, so the line appears often; drawn as muted information it reads as a status quirk, and the type's own argument says exactly why that trains the reader to ignore it.
- Proposed fix (NOT applied): `ok = false` for `quietSecs > 0L`. If the intent is to distinguish "quiet but owed" from "refused", give `Notice` a third weight rather than reusing the success flag.
- Open questions: whether `QUIET_NOTICE_MS` and `QUIET_POLL_MS` (both used at `:736-743`) are scaled to the route; at 14 hops a legitimate gap is longer than the notice threshold, which would make a correct weight produce a permanent false alarm.
- Investigator: ux round · 2026-11-11

### A-282 · ux: the screen says "direct" for a zero-hop flood · major

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/PublicChannelBar.kt:79` — `val where = hops?.let { if (it <= 0) "direct" else "$it hop${…}" }`; `PublicChannelBar.kt:271-277` — `exchangeMs` returns null when `h <= 0`, so the price of an exchange is also withheld in the same state; `AndroidRadioLink.kt` flood carrier (`record by flood 1/3 … dest 97` in the 2026-11-10 field log) and the host's own line `route flooded record 121 B from a 0 hop chain` (journal, 2026-11-10 22:13:26) show that **hops = 0 with a flood carrier is a measured state**, not a hypothetical.
- Finding: `hops` is a path length, and the carrier is a separate fact. A flooded record that a neighbour relayed with no path bytes reports `hops = 0`, and the bar prints `direct` — asserting the cheap carrier at the exact moment the record provably rode the flood. The same bar then omits `~Ns/exchange`, because that figure is suppressed for `h <= 0`.
- Why it hurts at range: the bar exists so the operator can tell what the link is doing, and this is the one place it can name the wrong carrier. A user who reads `direct · answered 2 s ago` concludes the path works and wonders why typing still takes a minute; the truth is that every record is going out three times as a flood.
- Proposed fix (NOT applied): pass the carrier alongside the hop count (the driver already knows — the flood paths in `AndroidRadioLink.kt` and `mesh_serial.rs` both set a flag) and print `flood · 0 hops` when the last inbound was a flood; keep `direct` for a record that actually arrived with a consumed path.
- Open questions: whether the phone has the carrier of the *last inbound* at all, or only of the last outbound; if only outbound, the line should say `sending by flood` rather than make a claim about the inbound.
- Investigator: ux round · 2026-11-11

### A-283 · ux: the one actionable number at range is three taps away, and SNR is never surfaced · major

- Evidence: `MeshCarrier.kt:58` — `val rssiDbm: Int?` is in the status the terminal screen already collects (`MeshTerminalScreen.kt:445` reads `radioStatus?.opsLeftThisHour` from the same object); `MeshRadioSection.kt:657-665` is the **only** place it is drawn (`listOfNotNull(rssi?.let { "$it dBm" }, volts)`), and that is the Settings screen; `MeshRadioSection.kt:631` — the "-128 dBm" sentinel handling also lives only there. `docs/MESH-RADIO-CONSIDERATIONS.md:13-15` records the field readings: **−107 to −124 dBm, SNR −6.75 to +5.5 dB**, against an SF7 floor of −7.5 dB SNR — i.e. the link was operating within ~1 dB of not existing.
- Finding: the terminal screen holds `radioStatus` and draws the airtime fraction from it while waiting, but never the signal. The single number that says "you are at the edge, this is not a bug" is visible only by leaving the shell and opening Settings, and SNR — the number that actually predicts decodability at this spreading factor — is not plumbed to the UI anywhere.
- Why it hurts at range: every field session in this audit ended in the same wrong conclusion ("the app is broken"), when the measured truth was a link at ~0 dB margin where one packet is a coin toss. One line of the data the app already has would have said so, and it costs no airtime.
- Proposed fix (NOT applied): put the radio's `rssiDbm` (and SNR, if the companion status carries it) on the waiting line beside `linkEffort()`, e.g. `handshaking · 40 s · 3 tries · −121 dBm`. Read it from the status object the screen already collects; no new radio work.
- Open questions: whether the companion's status exposes SNR per reading or only per packet (A-40 notes `last_snr_db` exists host-side in `crates/companion/src/codec.rs:1142`); whether the phone's firmware reports the sentinel for "not measured" on that field too, which needs the same handling as `MeshRadioSection.kt:631`.
- Investigator: ux round · 2026-11-11

### A-284 · ux: the verdict's advice is wrong at range, and its action is the most expensive one available · major

- Evidence: `MeshBody.kt:32-35` — `MeshTermView.NoAnswer -> MeshBody("no answer from host", "check the host is on the same channel, then refresh")`; `MeshTerminalScreen.kt:967-979` — the refresh square's `else -> attempt += 1`, and `attempt` keys the connection (`:262-264`), so a refresh **replaces** the link and session; `MeshTerminalScreen.kt:324-343` — the rebuild clears `resumeShell` and forces `showList = true`; every measured failure in this ledger has a different cause: A-80 (retransmission timer 2.9 s against a 43 s round trip), A-221 (a refused handshake writes nothing), A-20 (three `msg1` copies make three sessions), A-41 (repeater hold ≈2.7 s per hop).
- Finding: at range the host is on the same channel in every failure this audit could measure — the app told the user to check a thing that was already correct — and the only action it offers (`refresh`) tears down a link that may be one crossing away from working, discards the accumulated ladder, and starts again at rung one. The sentence is a diagnosis of the wrong cause paired with the most costly available action.
- Why it hurts at range: the user follows the instruction, because it is the only instruction, and the cost is a full teardown plus a fresh ladder (≥176 s at a neighbour, ≈372 s at 14 hops) plus an attach (A-100…A-108). The app's own advice is therefore the surest way to guarantee "it never finishes handshaking".
- Proposed fix (NOT applied): say what is actually true and cheap — `no answer yet · 4 tries · 7 route requests` plus one line explaining that the mesh is slow rather than broken — and make the refresh square's action state-dependent: re-ask the handshake *on the existing connection* where the driver is still retrying, and reserve the teardown for the state where the driver has given up or the transport is dead (`transportDead`, `MeshTermDriver.kt:391-405`).
- Open questions: whether a "re-ask now, without rebuilding" action is available on the driver's API (the ladder's own rung is), which would make the cheap action real rather than advisory; whether A-221's refusal reason could be surfaced here once it exists, so the sentence is specific.
- Investigator: ux round · 2026-11-11

### A-285 · ux: three comments in one file describe three different behaviours for a drop, and the code takes the most expensive one · major

- Evidence: `MeshTerminalScreen.kt:253-256` — *"A rebuilt connection comes back to the same shell. The attach carries an id, so a resume is one record instead of a fresh list and a tap: a drop costs the user a pause, not their place."*; `:324-329` — *"A rebuilt connection lands on the shell list, not in the shell it was in"*, immediately followed by `showList = true; resumeShell = ""` at `:341-343`; `:376-379` — *"Closing here wakes `run`, and the effect above rebuilds on return, re-attaching `resumeShell`, so the user gets their place back"*; `:1017` — `holding` requires `resumeShell.isNotEmpty()`, so the held-screen path cannot run after a rebuild either.
- Finding: the resume id is cleared by the automatic rebuild at `:342`, so the two comments that promise "your place back" describe a path that no longer exists, and the actual behaviour is the one at `:324`: after any transport death the user lands on the launcher. Which of these is intended is not stated anywhere; all three are asserted as fact in the same file.
- Why it hurts at range: transport death is the normal event at range, not the exception. Each one costs a list request (A-100…A-108: 18–27 transmissions for a list answer), a tap, and an attach — the three most expensive things the app does, spent to restore a place it had already recorded.
- Proposed fix (NOT applied): decide the behaviour and make the comments agree. If landing on the list is intended (the argument at `:324` is sound — the shell may be gone), delete the promise at `:253` and `:378`; if the pause-and-resume is intended, do not clear `resumeShell` in the rebuild and let the attach's `NOT_FOUND`/`held` answers fall back to the list, which is a single record and already handled.
- Open questions: whether a resume attach to a shell the host has lost costs less than a list request plus an attach (i.e. whether the list landing is cheaper as well as safer); whether `heldScreen` (`:1017`) can be kept across a rebuild so the pause still shows the last picture, as `:1000-1008` claims for a drop inside the driver.
- Investigator: ux round · 2026-11-11

### A-286 · ux: the notification says "linked to desktop over radio" in states that are not linked · minor

- Evidence: `MeshTerminalScreen.kt:420-430` — `val wanted = when (view) { Idle, HostClosed -> false; else -> !disconnected }` and `val text = when (view) { LinkDown, Handshaking -> "reconnecting to desktop over radio"; else -> "linked to desktop over radio" }`.
- Finding: `NoAnswer`, `ProfileMissing`, `Listing`, `Choose` and `NoShells` all fall through to `"linked to desktop over radio"` while `wanted` is true, so the persistent notification claims a link during the very states that exist because no link was established. `NoAnswer` is the clearest: the screen says no answer came, the notification says linked.
- Why it hurts at range: the notification is what the user sees with the app backgrounded, which at range is most of the time (the app is held in a pocket while walking). It is the one surface that can tell them the truth without waking the screen, and in these states it tells them the opposite.
- Proposed fix (NOT applied): derive the words from the same three-way split the screen uses — reconnecting / not answered yet / linked — and keep `wanted` as it is, since holding the service during a wait is deliberate (`:410-419`).
- Open questions: whether the notification's text is refreshed often enough to be worth the extra states, or whether it should only claim "linked" once a `Commit` has been applied (`Live.painted`).
- Investigator: ux round · 2026-11-11

### A-287 · ux: nothing in the app can stop the ladder · minor

- Evidence: `MeshTerminalScreen.kt:922-930` — the only disconnect path is `DisconnectRequests.requests`, which is the **notification's** action; `:407` `var disconnected` exists solely for that collection; no control on the mesh screen sets it (the header's square is refresh, `:967-979`; the body's actions are `Takeover` for `HeldBy` only).
- Finding: a link that is spending airtime on a ladder the user no longer wants can only be stopped from the notification shade. In the app the nearest control is `refresh`, which is the opposite of stopping.
- Why it hurts at range: with the budget now 176–372 s per link plus rebuilds, a user who realises the host is off has no in-app way to stop the radio working; the app keeps transmitting, which on a shared mesh is other people's airtime as well as theirs.
- Proposed fix (NOT applied): give the launcher's wait state one action — `stop trying` — wired to the same `disconnected = true` the notification's Disconnect already sets, so both surfaces converge on one state.
- Open questions: whether `disconnected` should also cancel the pending handshake in the driver rather than only releasing the service (the connection is closed, so probably yes).
- Investigator: ux round · 2026-11-11

---

77:**The three sentences I would change first:** (1) `"no answer from host"` / `"check the host is on the same channel, then refresh"` (`MeshBody.kt:33-35`) — the wrong cause and the most expensive cure, and it is the sentence a range user sees for most of every attempt; (2) `"link quiet · ${quietSecs} s"` with `ok = true` (`MeshTerminalScreen.kt:1359-1360`) — the one warning of a dead link, 
79:**What I could not determine:** (1) whether the phone's `MeshStatus` carries the *inbound* carrier or only the outbound, which decides whether A-282 can be fixed in the bar alone; (2) whether the companion status exposes SNR per reading on the phone side, so A-283's second half may be a proto change; (3) whether `heldScreen` actually survives the rebuild in practice — the code path at `:1017`
### A-300 · docs: ADR-0038 says decay is "on any arrival"; code decays only on a direct arrival · blocker

- Evidence: `docs/adr/0038-carrier-selection-escalate-on-silence.md:27` (`| **any** record arrives | something works | failures = 0 → **direct** |`), `:22-24` ("flood is an escalation … decays the moment that evidence changes"), `:57` (`Mirroring the carrier — positive feedback` listed under **Rejected**), `docs/adr/README.md:41` ("decayed by any arrival"); code `crates/daemon/src/mesh_serial.rs:2208` (`peer_via_flood = true` on a flood), `:2278` (`peer_via_flood = false` **only** in the direct-arrival branch), `:1413-1417` (`want_flood = … || peer_via_flood`); phone `AndroidRadioLink.kt:531` (`directWorks = true` only on a direct record), `:594` (`directWorks = false` on any flood), `:1013` (`floodWanted = (!directWorks || directSilent)`).
- Finding: the decay the ADR defines happens on a **direct** arrival only, on both ends. A flood arrival sets the latch and nothing but a direct arrival clears it — so the "positive feedback loop" the ADR explicitly rejects as an alternative is the implemented behaviour. The ADR's own `Why this converges` section ("flood → the flood's travel chain becomes the new candidate and is adopted forcibly → failures reset → **direct** on the new candidate") describes a convergence path that does not exist in the code.
- Why it hurts at range: the next engineer reads ADR-0038, believes the link returns to direct after one flood, and looks for the bug elsewhere; meanwhile every record on both ends rides the flood carrier with three copies each, indefinitely, on a mesh that measures −107…−124 dBm.
- Proposed fix (NOT applied): the code should implement the ADR — a flood arrival resets the escalation (the reversed chain is already adopted as the candidate at `mesh_serial.rs:2183-2199`), with one bounded exception (a periodic deliberate direct probe) recorded as a new ADR line. If instead the latch is intended, ADR-0038, `README.md:41` and `ARCHITECTURE.md:649` must be rewritten to say so and the `Mirroring the carrier` rejection removed.
- Open questions: which of the two is intended — the ledger's A-120 grades the latch a blocker, so the docs are probably right and the code wrong.
- Investigator: docs round · 2026-11-11

### A-301 · docs: ADR-0037 and ARCHITECTURE still describe "flood when the route is not fresh" · blocker

- Evidence: `docs/adr/0037-two-carriers-direct-by-default-flood-when-there-is-no-route.md:37` ("**When each is used.** The phone floods a record when its route is not fresh. The host, with no path, sends the first reply as a plain zero-hop direct packet … and escalates to a flood only if the peer is still re-handshaking after that. Any record that arrives resets the escalation"), and again at `:62`; `docs/ARCHITECTURE.md:649-652` ("The carrier is chosen per send — direct when the route is fresh, flood when it is not — and the host escalates rather than starting there: a pathless zero-hop reply is cheap and correct for a neighbour, so the flood is used only after that fails to be answered"); code `mesh_serial.rs:1413-1418` (`want_flood` also set by `path_stale`, `direct_silent`, `answer_flood_with_flood`, `peer_via_flood`), `:1425` (`flood_now = answer_flood_with_flood || (want_flood && pathless_directs > 0)`), `AndroidRadioLink.kt:1013`.
- Finding: three separate claims in these two documents are false of the code: (a) the phone does not test route freshness at all — it tests `directWorks`/`directSilent`; (b) the host does not send a zero-hop direct reply first when the peer's record arrived by flood — `answer_flood_with_flood` makes the very next reply a flood; (c) "any record that arrives resets the escalation" is false for floods (A-300).
- Why it hurts at range: ARCHITECTURE is the authoritative design; a reader debugging "why is everything flooded" is told by this paragraph that flooding requires a stale route, and the real cause (`peer_via_flood`) is never mentioned in any document outside the ledger.
- Proposed fix (NOT applied): rewrite ARCHITECTURE:641-652 and ADR-0037:37 to state the four escalation causes and the zero-hop-first exception as the code has them, or change the code to match. Also record the flood-reach hop count that the session is timed against when pathless (`mesh_serial.rs:1873-1876`), which no design document mentions at all.
- Open questions: whether `path_stale` should be an escalation cause now that `direct_silent` exists — the ADR explicitly rejects "flooding by staleness alone".
- Investigator: docs round · 2026-11-11

### A-302 · docs: ADR-0038 says floods are paced by a token bucket; the host has no bucket · major

- Evidence: `docs/adr/0038…:42` ("**Floods are paced by a token bucket** (four back to back, refilled at one per flood round trip), because a batch is thirteen records and a terminal must not become a broadcast"); `grep -n "bucket\|throttle\|pace\|tokens" crates/daemon/src/mesh_serial.rs` → only an unrelated comment at `:493`; the bucket exists solely in `AndroidRadioLink.kt:274-288, 987, 2022` (`FLOOD_BURST = 4`).
- Finding: the pacing decision is documented as a property of the carrier rule, and only one end implements it. The host floods without limit, and each flooded record is **three copies** (`mesh_serial.rs:513`, `:1468`), with `peer_via_flood` (A-300) making that permanent at range.
- Why it hurts at range: the document that says "a terminal must not become a broadcast" is cited as evidence the design is safe, while the end that talks to the mesh most (the host answering a phone that re-handshakes) has no bound at all.
- Proposed fix (NOT applied): either add the bucket to the host's flood path (same `FLOOD_BURST`, refill from the measured flood round trip) or state in ADR-0038 that the host relies on the ladder and the reply floor instead, and why that is enough.
- Open questions: whether the host's `last_reply` floor (600 ms, A-303) plus `answer_flood_with_flood` can out-pace the phone's bucket in practice.
- Investigator: docs round · 2026-11-11

### A-303 · docs: "one reply per 5 s" is now 600 ms, and no request floor of 10 s exists · major

- Evidence: `docs/SECURITY.md:47` ("Both ends rate limit (one reply per 5 s, one request per 10 s) so a peer cannot use either as an amplifier"); `docs/ARCHITECTURE.md:736` ("the host answers at most one per 5 s"); code `mesh_serial.rs:180` (`const ROUTE_REPLY_FLOOR: Duration = Duration::from_millis(600);`), and `git log -L180,180:crates/daemon/src/mesh_serial.rs` → `db4623b` replaced `from_secs(5)` with `from_millis(600)` and touched no document; the phone still answers at 5 s (`AndroidRadioLink.kt:1984` `REQUEST_ANSWER_FLOOR_MS = 5_000L`), so "both ends" is false in both directions; the only 10 s constant is `REQUEST_BURST_MS = 10 s` (`mesh_serial.rs:77`), which coalesces one request's copies and is not a request rate limit — the host re-asks at `ROUTE_REASK_FLOOR = 2 s` (`:142`) and the phone at `ROUTE_REQUEST_FLOOR_MS = 5 s` (`AndroidRadioLink.kt:1990`).
- Finding: the documented amplifier bound is **8× tighter than the code on the host**, and the documented request bound does not exist. The asymmetry (host 600 ms, phone 5 s) is documented nowhere.
- Why it hurts at range: this is the one paragraph a reader consults to decide whether an unauthenticated flood can be turned into an amplifier, and it is wrong in the unsafe direction — a spoofed request stream makes the host emit one flooded reply per 600 ms, and with the three-copy behaviour of A-304 potentially more.
- Proposed fix (NOT applied): restore 5 s on the host or document 600 ms as a deliberate cost with its justification; delete "one request per 10 s" or name the constant that implements it.
- Open questions: whether `first_time_question`'s nonce memory (`mesh_serial.rs:182-186`) caps the rate for a *repeating* attacker, which is the realistic case.
- Investigator: docs round · 2026-11-11

### A-304 · docs: row 6.228 claims the host's flooded reply is repeated three times; it is one packet · major

- Evidence: `docs/RELEASE-AUDIT.md:312` (row 6.228: "FIXED, three copies with a fresh wrapper nonce each, on the phone's flooded records **and on the host's flooded replies**"); `git show c8c5d3b -- crates/daemon/src/mesh_serial.rs` added `RECORD_FLOOD_COPIES = 3` and the copy loop to the **record** send only (`mesh_serial.rs:513`, `:1468`); the reply path `send_route_reply` (`:290-355`) still builds one packet and sends it through `send_retrying(|d| companion.send_packet_within(&packet, d))`, which retries **the same bytes** — and the function's own comment says an identical repeat is swallowed by MeshCore's duplicate filter, so a retry after a lost ack teaches the phone nothing.
- Finding: the row records as fixed the half of the asymmetry that was not fixed. The reply — the packet that has never been heard at range — remains a single transmission with at most a same-bytes retry.
- Why it hurts at range: an investigator reading the audit concludes the answer-half of discovery is covered and looks elsewhere; the measured field evidence ("`route reply sent ok=true` and the phone never sees it") is consistent with a single lost copy.
- Proposed fix (NOT applied): make `send_route_reply` loop `RECORD_FLOOD_COPIES` times with `route_nonce()` re-stamped (the reply already echoes the request nonce, so the wrapper nonce is what must vary), or correct row 6.228 to say the reply was left at one copy and why.
- Open questions: `send_retrying`'s rounds (`SEND_ROUNDS = 3`) fire only on `Disabled`/`Timeout`, so on a busy radio that is three identical packets — does the first repeater's dedup drop copies two and three?
- Investigator: docs round · 2026-11-11

### A-305 · docs: three rows of the "every timing figure" table are stale · major

- Evidence: `docs/RELEASE-AUDIT.md:722` (`| handshakeWaitMs | retry << 3 | …`) vs `MeshTermDriver.kt:2612-2623` (`ladder = rung * 7`, budget `= ladder + max(rung, HANDSHAKE_ANSWER_WINDOW_MS)` with `HANDSHAKE_ANSWER_WINDOW_MS = 120_000`, i.e. 176 s at a neighbour, changed in `499de61`); `:729` (`| ROUTE_REPLY_FLOOR | 5 s |`) vs `mesh_serial.rs:180` (600 ms, `db4623b`); `:703` (R13, "a raw send waits at most 2 s") vs `mesh_serial.rs:494` (`raw_send_deadline(900.0) + 300 ms << round` → 4.9 s + 0.3/0.6/1.2 s, and `raw_send_deadline` is `max(2 s, 4000 + airtime)`).
- Finding: the table is presented as the register of every derived number with its basis, and three of its rows describe numbers the code no longer uses — the handshake budget by 2.75×, the reply floor by 8×, the raw-send hold by 2.5× (and up to 8.5× with retry rounds).
- Why it hurts at range: this table is what a reviewer checks when asking "is anything a flat constant?" — the flat-constant audit is being performed against stale rows.
- Proposed fix (NOT applied): regenerate the table from the constants, and add the rows that are missing (the answer window, the flood-reach hop count, `direct_round_trip`).
- Open questions: whether `499de61` and `db4623b` should have failed a doc gate; no test reads these numbers.
- Investigator: docs round · 2026-11-11

### A-306 · docs: §18's "Deployed" table names a four-day-old commit · major

- Evidence: `docs/RELEASE-AUDIT.md:638` ("## 18. State and risks at `548759f` (2026-11-10)") and `:644` (`| Desktop daemon | 838f101, ldmuxd 838f101 (protocol 1) …|`); the deployed binary reports `ldmux 2ea77bb (protocol 1)` (`ldmux --version`), HEAD is `5523167`, and the phone's APK was rebuilt at 22:07 after that.
- Finding: the section's stated purpose is "what is deployed, what was actually exercised on the air, what was not, and every risk that remains", and its first table is wrong by ~100 commits; its `Verified on the air` and `Not verified` lists are from the same era (e.g. "A session establishes with one accepted handshake" and "Not verified: a full session over more than one repeater since the `TooLong` fix" are both superseded by 2026-11-10's runs, and the current blockers — duplicate `msg1` sessions, the latch, the timer following the shortest inbound path — appear nowhere).
- Why it hurts at range: this is the document a person reads cold before a field trip, and it would send them to test the wrong things with a build they do not have.
- Proposed fix (NOT applied): rewrite §18's header, `Deployed` table and the two verification lists from the current tree, and move the historical section under a "superseded" heading.
- Open questions: whether §18 should be regenerated automatically from `ldmux --version` plus the ledger's open blockers, so it cannot drift again.
- Investigator: docs round · 2026-11-11

### A-307 · docs: the per-hop delay model is wrong in both directions · major

- Evidence: `docs/ARCHITECTURE.md:775-778` ("each repeater holds a packet for a random delay of up to two airtimes, so the session's first retry is `6 x airtime x hops`"), `:368-370` (same factor), `docs/RELEASE-AUDIT.md:721` (`handshakeRetryFor` basis "the firmware's forward delay is up to 2 airtimes a hop"); firmware `mesh-research/clones/filter-firmware/src/Dispatcher.cpp:241-252` (`if (pkt->isRouteFlood()) { int _delay = calcRxDelay(score, air_time); … } else { n_recv_direct++; processRecvPacket(pkt); }`), `:55-57` (`calcRxDelay = (10^(0.85 − score) − 1) × air_time`), `Mesh.cpp:17-21` (`getRetransmitDelay = rng(0,5) × (airtime × 52/50)/2`, i.e. 0–2 airtimes), `Mesh.cpp:22-24` (`getDirectRetransmitDelay` → **0**).
- Finding: the firmware has two different per-hop costs and the documents describe a third. A **flood** is held at each repeater for `calcRxDelay` — 5.5 airtimes at the measured score of 0.036 for a 133 B packet, i.e. ≈2.7 s, not 0.98 s — before it is even queued, and then re-transmitted after `getRetransmitDelay` (0–2 airtimes). A **direct** packet gets no retransmit delay at all. So "up to two airtimes a hop" understates a flood hop by roughly 3× and overstates a direct hop.
- Why it hurts at range: every derived figure that names this premise (`handshakeRetryFor` 2 s/hop, `answerDeadlineMs` 2 s/hop, the ARQ's `6 × airtime × hops`) is justified by a number the firmware contradicts, and at range the carrier in use is the flood — the expensive case.
- Proposed fix (NOT applied): restate the premise as two figures — direct: `≈1 airtime per hop`; flood: `calcRxDelay(score, airtime) + getRetransmitDelay()`, with the measured 5.5 airtimes at a marginal score — and re-derive the hop-scaled deadlines from the flood figure whenever the flood carrier is in use.
- Open questions: whether `ACTION_RETRANSMIT_DELAYED`'s own `_delay` (`Dispatcher.cpp:266-272`) stacks on `calcRxDelay` or replaces it, which decides whether ≈5.5 or ≈7.5 airtimes is the flood figure.
- Investigator: docs round · 2026-11-11

### A-308 · docs: AIRTIME's "first timeout is `(window + 2) × airtime`" is a superseded design · major

- Evidence: `docs/AIRTIME.md:158-161` ("**A retransmission timer has to cover the burst it started.** The ARQ hands a whole window to the radio at once, so the first timeout is `(window + 2) × airtime` — 6 s at 62.5 kHz"); code `crates/session/src/arq.rs:181-187` ("The burst is no longer a reason to stretch this, because a slot's timer starts at its own air slot … `let scaled = (airtime * 6.0 * hops).round()`"), and `ARCHITECTURE.md:363-367` states the slot-based rule — contradicting AIRTIME in the same document set.
- Finding: AIRTIME documents a rule the code removed on purpose, and it is the only document that explains why the timer is what it is; ARCHITECTURE's version is correct. Two documents in one set give different formulas for the same constant.
- Why it hurts at range: someone auditing "is the retransmission timer long enough at 14 hops" gets 6 s from AIRTIME and 50 s from the code, and would `fix` a non-bug or miss the real one (A-80: the hop count itself is wrong).
- Proposed fix (NOT applied): delete the sentence, or restate it as "the burst is covered by per-slot timer start, not by stretching the timeout", citing `arq.rs:181`.
- Open questions: none — this is a plain stale paragraph.
- Investigator: docs round · 2026-11-11

### A-309 · docs: `DIRECT_SEND_PERHOP_FACTOR` is cited to the wrong file, and its formula loses two terms · major

- Evidence: `docs/AIRTIME.md:153-155` ("`DIRECT_SEND_PERHOP_FACTOR` in the firmware's `Dispatcher.cpp` is 6.0 airtimes per hop (its flood factor is 16.0)"); `docs/ARCHITECTURE.md:368-369` (`(Dispatcher.cpp)`); `crates/session/src/arq.rs:167` (same citation); firmware: the constants live in `filter-firmware/examples/companion_radio/MyMesh.cpp:109-110`, not `Dispatcher.cpp`, and the budget is `MyMesh.cpp:891-896`: `500 + ((airtime × 6.0 + 250) × (hops + 1))`, with the flood form `500 + 16.0 × airtime` (`:888-890`).
- Finding: the citation is wrong, and "six airtimes per hop" is only the leading term of the firmware's own budget: it omits a 500 ms base, a 250 ms per-hop constant, and the `+1` hop. At a neighbour the firmware waits **8.2 s** where ldmux waits 3.6 s; at 14 hops **58.5 s** where ldmux waits 50.7 s. The firmware also treats this as a *send timeout* (when to declare the send unanswered), not as a physical forward delay — which is what the documents call it.
- Why it hurts at range: the one figure the design leans on for "we are at least as patient as MeshCore" is 2.3× shorter than MeshCore at the hop count where the link is hardest, and the document asserts parity.
- Proposed fix (NOT applied): correct the citation to `MyMesh.cpp:110` and restate the figure as `500 + (6·airtime + 250)·(hops+1)`, noting that it is a timeout and not a delay.
- Open questions: whether `arq.rs:187`'s `6.0 × airtime × hops` should adopt the full formula (the ledger's A-80 says the hop count is the bigger bug).
- Investigator: docs round · 2026-11-11

### A-310 · docs: a constant that does not exist, with a third of its real value · major

- Evidence: `docs/ARCHITECTURE.md:322` ("On the phone, 20 s with something outstanding ends the session (`SILENCE_FAILURE_MS`)"); no such symbol exists (`grep -rn "SILENCE_FAILURE_MS" packages/` → nothing); the function is `silenceFailureMs(hops)` at `MeshTermDriver.kt:2473-2479`, `min(60_000 + 3_600 × hops, 300_000)` — 60 s at a neighbour, 110 s at 14 hops.
- Finding: the sentence names a symbol a reader can search for and fail to find, and understates the wait by 3×.
- Why it hurts at range: "how long before the phone gives up" is the first question in every field report, and this table answers it wrongly; it also hides the fact that the bound now scales with the route.
- Proposed fix (NOT applied): replace with `silenceFailureMs(hops)` = `min(60 s + 3.6 s a hop, 300 s)`, matching row `:717`-adjacent table entries.
- Open questions: none.
- Investigator: docs round · 2026-11-11

### A-311 · docs: the carriage-silence backstop is documented as working, and it cannot fire · major

- Evidence: `docs/ARCHITECTURE.md:323` ("75 seconds with a record outstanding ends the connection, so `supervise` can build another"), `docs/RELEASE-AUDIT.md:730` (`ps carrier silence | max(75 s, 3 × rto) | Derived`); code `crates/daemon/src/mesh.rs:1111-1120` (correct: `CARRIER_SILENCE.max(3 × first_rto)`) but `:966-967` (`last_carrier = Instant::now()` on **any** event) together with `mesh_serial.rs:1809-1812` (`Event::Status` every `STATUS_REFRESH` = 500 ms, computed from internal counters only).
- Finding: the value the docs describe is implemented, and the mechanism that uses it is defeated by a self-generated heartbeat that proves nothing about the radio — the ledger's A-180. The documents state the outcome ("ends the connection") as if it happened.
- Why it hurts at range: an operator whose radio wedged is told by the docs that the daemon recovers by itself, so the manual restart that actually fixes it (five of them on 2026-11-10) is treated as a mystery rather than the expected behaviour.
- Proposed fix (NOT applied): fix the watchdog (A-180), and until then add a sentence to `:323` saying the backstop depends on a status refresh that itself proves only that the thread is alive.
- Open questions: whether `Event::Status` should carry a "last successful radio command" sequence so silence is measurable at all.
- Investigator: docs round · 2026-11-11

### A-312 · docs: "the phone's ladder doubles from 5 s to a 40 s cap"; the cap is 20 s · minor

- Evidence: `docs/ARCHITECTURE.md:735` ("the phone's ladder doubles from 5 s to a 40 s cap with ±25 % jitter"); code `AndroidRadioLink.kt:1508-1513` (`requestIntervalFor`: `step = 1 << asks.coerceIn(0, 2)`, so 5/10/20 s), and the 40 s figure survives only in the unused `requestIntervalMsOld` (`:1516-1522`, `@Suppress("unused")`).
- Finding: the documented ceiling is double the implemented one. The phone asks the mesh twice as often as the design says.
- Why it hurts at range: request floods are the cheapest discovery packet but they are still every repeater's airtime; a document that says "40 s at the edge of coverage" hides that the code reaches 20 s and that a dead function kept a 40 s design alive.
- Proposed fix (NOT applied): correct the sentence, and delete `requestIntervalMsOld` so the design cannot be read off dead code.
- Open questions: whether 20 s was the intended ceiling (the comment in `requestIntervalMsOld` argues for 40 s at the margin).
- Investigator: docs round · 2026-11-11

### A-313 · docs: the advertised record limit at range is 162, not `min(171, 172 − path)` · major

- Evidence: `docs/ARCHITECTURE.md:329` and `:538` ("the limit is `min(171, 172 − path)`", "The limit advertised to the session is therefore `min(171, 172 − path)`"); code `mesh_serial.rs:542-548` (`record_limit` ✓ matches) but `:1318-1322` and `:1869-1876`: when no path is fresh the published limit is `flood_record_limit()` = **162** (`:461-465`), and the hop count published with it is the flood reach, not the path; `docs/RELEASE-AUDIT.md:299` (6.215) and `:302` (6.218) record this as fixed.
- Finding: the authoritative design document states the ceiling formula for the case that no longer governs at range. With no fresh route — the normal state for the user this system exists for — the session is sized to 162, not 171, and no design document says so outside the audit rows.
- Why it hurts at range: a reader sizing a change (a screen chunk, a paste limit) to 171 will produce records the flood builder refuses, which is exactly row 6.215's field failure re-introduced by following the doc.
- Proposed fix (NOT applied): state both limits in §11 — `min(171, 172 − path)` on a fresh path, `162` when the record will ride the flood — and name the constant.
- Open questions: whether the 12-byte wrapper cost should be a named constant shared with the Kotlin side rather than `162` derived twice.
- Investigator: docs round · 2026-11-11

### A-314 · docs: ADR-0037's capacity arithmetic is stale, and its conclusion is false · minor

- Evidence: `docs/adr/0037…:62` ("Record capacity on the flood carrier is *larger* than on a long path (`172 − 10` versus `172 − path bytes`), so a record that fits a direct path always fits the flood"); code `crates/companion/src/codec.rs:167` (`PACKET_MAX = 174`), `mesh_serial.rs:463-465` ("the 12-byte wrapper (header, path byte, two hashes, magic, nonce)" → 162), `:542-548` (`record_limit(None) = 171`).
- Finding: both subtrahends are stale (`172` was the old fixed record max, 10 was an earlier wrapper width), and the conclusion does not hold: 162 < 171, so a record that fits a **short** direct path does not fit the flood. Only paths longer than about ten hops make the flood the larger carrier.
- Why it hurts at range: the sentence is the ADR's justification for not needing a size check when switching carriers; row 6.215 exists precisely because that conclusion was false in the field.
- Proposed fix (NOT applied): rewrite as `174 − 12 = 162` versus `min(171, 174 − 2 − path)`, and state that a carrier switch can shrink the record and must be announced (which `Event::Limit` does).
- Open questions: none.
- Investigator: docs round · 2026-11-11

### A-315 · docs: the handshake's clock rule is documented without its tolerance · major

- Evidence: `docs/SECURITY.md:70-71` ("The host pins the phone's static key on the first accepted handshake, and the handshake timestamp is bounded to a 5 minute window against a stored floor"); code `crates/session/src/handshake.rs:66-71` (`TIMESTAMP_WINDOW_MS = 5 min`) **and** `:69` (`HANDSHAKE_REPLAY_TOLERANCE_MS = 10 min`), with the floor test at `:316-333` (`timestamp + 10 min < last → Replay`).
- Finding: the document describes a strict 5-minute rule; the code accepts any timestamp within ±5 minutes of the host clock **and** up to 10 minutes behind the stored floor — so the floor check can never fire (the ledger's own A-240 analysis reaches the same conclusion independently). The security section therefore advertises a bound that does not exist.
- Why it hurts at range: this is the paragraph a reviewer reads to decide whether a captured handshake can be replayed; the answer it gives is wrong, and the actual bound (the ±5-minute window alone) is what an attacker exploits.
- Proposed fix (NOT applied): state the real rule — "within ±5 minutes of the host's clock, and never more than ten minutes behind the last accepted timestamp; a duplicate inside that is a retry of the same attempt and is idempotent" — once the ledger's A-20 cache makes duplicates idempotent.
- Open questions: whether the tolerance should be dropped entirely once `msg1` handling is idempotent, since equality would no longer need special handling.
- Investigator: docs round · 2026-11-11

### A-316 · docs: the phone's advert cadence is documented as three minutes and is sixty seconds; "off by default" is host-only · minor

- Evidence: `docs/SECURITY.md:48-50` ("The phone's radio also advertises while it is trying to reach the host, once when the link binds and then at most every three minutes while nothing has been heard"); code `AndroidRadioLink.kt:2041` (`ADVERT_INTERVAL_MS = 60_000L`), `:864` (advert at bind), `:1632-1642` (`maybeAdvertise` on the same 60 s); `docs/ARCHITECTURE.md:425-427` ("An advert is a maintenance broadcast, not the way a peer finds us …, and it is **off by default**") — true of the host (`mesh_serial.rs:101-107`, default 0) and false of the phone; `docs/AIRTIME.md:150-152` states the host case as if universal; and `ARCHITECTURE.md:719` contradicts `:425` by adopting a route from an advert "the moment it arrives".
- Finding: three documents give three different pictures of who advertises how often. The phone advertises **three times as often** as the security document states, and the design document's "not the way a peer finds us" sits beside a paragraph that uses adverts as a route source.
- Why it hurts at range: an advert is a flood every node in range repeats, and the security section is where its cost is accounted for; the numbers there are off by 3×, and "off by default" invites the assumption that a radio-only phone is quiet when it is the opposite.
- Proposed fix (NOT applied): state the phone's 60 s cadence in SECURITY.md, scope ARCHITECTURE's "off by default" to the host, and reconcile `:425` with `:719`.
- Open questions: whether 60 s is intended given `flood.max.advert = 3` (A-60's finding that adverts die at eight hops).
- Investigator: docs round · 2026-11-11

### A-317 · docs: `MESH-RADIO-CONSIDERATIONS.md` describes the budget and the fix it was written to ask for · minor

- Evidence: `docs/MESH-RADIO-CONSIDERATIONS.md:75-76` ("**Our ladder:** rungs at `8 s + 2 s/hop` … budget `rung × 8` = **64 s at 0 hops**") and `:146` ("1. **Handshake timing to the firmware's formula** … The phone abandons handshakes the host has already established"); code `MeshTermDriver.kt:2612-2631` (budget `rung × 7 + max(rung, 120_000)` = 176 s at a neighbour, `499de61`).
- Finding: the document's own headline defect is fixed in the code and still listed as the top open item, and the budget it states is 2.75× the real one. Written 2026-11-10 and stale within a day.
- Why it hurts at range: it is the document a newcomer reads first for "everything about this path"; it now teaches the pre-`499de61` arithmetic and hides that the answer window exists.
- Proposed fix (NOT applied): update §§5 and the open-items list to the shipped figures, and add the A-20/A-301/A-307/A-313 items from this ledger so the summary matches reality.
- Open questions: whether this document should be folded into ARCHITECTURE §11 rather than maintained separately (it overlaps heavily and drifts fastest).
- Investigator: docs round · 2026-11-11

### A-318 · docs: "asked again with fresh keys at 8, 16 and 32 seconds" is not what the ladder does · major

- Evidence: `docs/ARCHITECTURE.md:321` (failure table: "a handshake gets no answer | it is asked again with fresh keys at 8, 16 and 32 seconds, then the connection is rebuilt"); code `MeshTermDriver.kt:1093-1121`: while the route is unknown, **only every third rung** sends a handshake (`if (blindAsks % 3 == 0)`), the other rungs send a route request and reset the ladder (`handshakeTries = 0`), and `handshakeWaitMs` is 176 s at a neighbour rather than the 56 s the three rungs imply.
- Finding: the table describes the neighbour case as if it were universal; at range, two rungs in three spend their airtime on a route request and the connection is rebuilt after 176 s, not ~56 s. The code comment that justifies it ("A session record is a direct packet, and a direct packet crosses no repeater") has been false since ADR-0037.
- Why it hurts at range: this row is the canonical answer to "why does it sit on handshaking", and the real answer (skipped handshakes plus a 176 s budget) is in neither document.
- Proposed fix (NOT applied): replace the row with the two cases — a route in hand (8/16/32 s) and blind (every third rung handshakes, the rest ask for a route, budget 176 s + the flood reach).
- Open questions: whether the blind ratio should be inverted now that a handshake record can ride a flood (the ledger's A-24).
- Investigator: docs round · 2026-11-11

---

**Most misleading sentence in the docs today** (and the reason it is first): `docs/adr/0038-carrier-selection-escalate-on-silence.md:27` — "`| **any** record arrives | something works | failures = 0 → **direct** |`". It is the sentence a reader would use to conclude the link cannot latch onto the flood carrier, and the latch is the ledger's A-120 blocker.

**Could not determine:** whether `db4623b`'s 600 ms floor and the missing host token bucket are deliberate costs recorded in a commit message I did not read closely enough to grade as policy rather than drift (both are stated as decisions nowhere in `docs/`); whether `ACTION_RETRANSMIT_DELAYED`'s `_delay` stacks on `calcRxDelay` (decides 5.5 vs 7.5 airtimes per flood hop in A-307); whether `requestIntervalFor`'s 20 s ceiling or the dead 40 s is the intended design (A-312); and whether any doc number is machine-checked anywhere — `grep` found no test reading `docs/`, so every one of these drifts passed the gate.
Implemented: a read-only documentation-vs-code audit of the radio path — **19 entries (A-300…A-318)** delivered above in the requested ledger schema, each with doc line **and** contradicting code line, severity, range impact, proposed fix, and open questions.

Changed files: none — the brief was read-only, no ledger writes, no builds, no adb. `git status --short` is empty.

Validation: every entry was checked against both sides. The load-bearing verifications: `mesh_serial.rs:2208/2278` (decay only on a direct arrival) against `adr/0038:27`; `git log -L180,180:crates/daemon/src/mesh_serial.rs` showing `db4623b` changed 5 s → 600 ms with no doc edit; `git show c8c5d3b` showing `RECORD_FLOOD_COPIES` reached only the record path, not `send_route_reply`; `filter-firmware/examples/companion_radio/MyMesh.cpp:110,891-896` giving the real `DIRECT_SEND_PERHOP_FACTOR` file and the full `500 + (6a + 250)(h+1)` formula; `filter-firmware/src/Dispatcher.cpp:241-252` + `Mesh.cpp:17-24` showing floods are delayed and direct packets are not; `ldmux --version` → `2ea77bb` against §18's `838f101`.

Open risks/questions: the four ungraded items are listed at the end of the report (whether 600 ms and the missing host bucket are policy or drift; whether `ACTION_RETRANSMIT_DELAYED`'s delay stacks on `calcRxDelay`; whether 20 s or the dead 40 s is the intended request ceiling; and that no test reads `docs/`, which is why all nineteen drifts passed the gate).

Recommended next step: the parent appends these to `docs/RADIO-AUDIT-LOOP.md`, then acts on the two that change behaviour on their own — A-300/A-301 (make the docs and the code agree about carrier decay, which is the ledger's A-120) and A-304 (the host's flooded reply is one packet, not three, so row 6.228 is closed prematurely).

```acceptance-report
{
  "criteriaSatisfied": [
    {
      "id": "criterion-1",
      "status": "satisfied",
      "evidence": "Delivered exactly the assigned aspect (docs vs code) as 19 schema-conformant entries A-300..A-318; made no edits, builds, service restarts, adb calls, commits, or ledger writes, per the brief's hard rules. git status --short returned 0 lines."
    },
    {
      "id": "criterion-2",
      "status": "satisfied",
      "evidence": "Every claim carries both sides: e.g. adr/0038:27 (any arrival -> direct) vs mesh_serial.rs:2208/2278 and AndroidRadioLink.kt:531/594; RELEASE-AUDIT:722 (handshakeWaitMs retry<<3) vs MeshTermDriver.kt:2612-2623; AIRTIME:154 vs filter-firmware/examples/companion_radio/MyMesh.cpp:110,891-896; git log -L180,180 proves the db4623b doc drift; git show c8c5d3b proves row 6.228's reply claim never reached send_route_reply."
    }
  ],
  "changedFiles": [],
  "testsAddedOrUpdated": [],
  "commandsRun": [
    {
      "command": "git log -L180,180:crates/daemon/src/mesh_serial.rs --oneline",
      "result": "passed",
      "summary": "Shows db4623b replacing ROUTE_REPLY_FLOOR from_secs(5) with from_millis(600); no doc touched"
    },
    {
      "command": "git show c8c5d3b -- crates/daemon/src/mesh_serial.rs",
      "result": "passed",
      "summary": "RECORD_FLOOD_COPIES added to the record send path only; send_route_reply still sends one packet"
    },
    {
      "command": "grep -rn 'DIRECT_SEND_PERHOP_FACTOR|calcRxDelay|getRetransmitDelay|getDirectRetransmitDelay' ~/Documents/mesh-research/clones/filter-firmware",
      "result": "passed",
      "summary": "Constant is in examples/companion_radio/MyMesh.cpp:110; direct retransmit delay is 0; flood delay is score-dependent calcRxDelay"
    },
    {
      "command": "ldmux --version; git log --oneline -1",
      "result": "passed",
      "summary": "Deployed 2ea77bb, HEAD 5523167, against RELEASE-AUDIT §18's claimed 838f101"
    },
    {
      "command": "git status --short | wc -l",
      "result": "passed",
      "summary": "0 — read-only audit left the tree untouched"
    }
  ],
  "validationOutput": [
    "A-300: ADR-0038:27 says decay on any arrival; mesh_serial.rs:2278 clears peer_via_flood only in the direct branch, AndroidRadioLink.kt:531/594 mirrors it -> documented rule not implemented (blocker)",
    "A-303: SECURITY.md:47 and ARCHITECTURE.md:736 say one reply per 5 s; mesh_serial.rs:180 is 600 ms (db4623b) and no 10 s request floor exists -> amplifier bound 8x weaker than documented",
    "A-304: RELEASE-AUDIT:312 claims three fresh-nonce copies on the host's flooded replies; send_route_reply:290-355 sends one packet via send_retrying, which reuses identical bytes",
    "A-307: ARCHITECTURE:775-778 basis 'up to two airtimes a hop'; firmware calcRxDelay gives 5.5 airtimes for a 133 B packet at the measured score, and direct retransmit delay is 0",
    "A-309: AIRTIME:154 and ARCHITECTURE:369 cite Dispatcher.cpp; the factor is MyMesh.cpp:110 and the budget is 500+(6a+250)(h+1) — 8.2 s at 1 hop where ldmux waits 3.6 s",
    "A-313: ARCHITECTURE:329/538 give min(171,172-path); publish path at 162 when no route is fresh (mesh_serial.rs:461-465,1318-1322), as rows 6.215/6.218 already record",
    "A-306: RELEASE-AUDIT:644 'Deployed 838f101' vs ldmux --version 2ea77bb; §18 verification lists are from the same stale era"
  ],
  "residualRisks": [
    "Whether db4623b's 600 ms reply floor and the host's missing token bucket are deliberate policy or drift: neither is stated as a decision anywhere in docs/, so A-302/A-303 could be mis-graded",
    "A-307's flood per-hop figure (5.5 vs 7.5 airtimes) depends on whether ACTION_RETRANSMIT_DELAYED's own _delay stacks on calcRxDelay — not resolved from the firmware read",
    "A-312 cannot say whether 20 s or the dead 40 s is the intended request ceiling; the dead function keeps a rival design readable",
    "No test reads docs/, so all nineteen drifts passed the gate and will recur unless a doc gate or generated table is added"
  ],
  "noStagedFiles": true,
  "diffSummary": "No diff: read-only investigation. The deliverable is the 19 ledger entries in the final response, written for the parent to append.",
  "reviewFindings": [
    "blocker: docs/adr/0038-carrier-selection-escalate-on-silence.md:27 - documented decay on any arrival; code decays only on a direct arrival (mesh_serial.rs:2208/2278, AndroidRadioLink.kt:531/594), which is the ledger's A-120 latch",
    "blocker: docs/ARCHITECTURE.md:649-652 and docs/adr/0037:37 - describe flood-on-stale-route and zero-hop-first; code escalates on direct_silent/peer_via_flood/answer_flood_with_flood (mesh_serial.rs:1413-1425)",
    "major: docs/SECURITY.md:47 and ARCHITECTURE.md:736 - claim one reply per 5 s; code is 600 ms (mesh_serial.rs:180) and no 10 s request floor exists",
    "major: docs/RELEASE-AUDIT.md:312 - row 6.228 claims three fresh-nonce copies on the host's flooded replies; send_route_reply sends one packet with same-bytes retries",
    "major: docs/ARCHITECTURE.md:775-778, 368-370, AIRTIME.md:154, arq.rs:167 - the per-hop delay premise and the DIRECT_SEND_PERHOP_FACTOR citation are both wrong versus the firmware",
    "major: docs/ARCHITECTURE.md:321-323 - handshake ladder row, a nonexistent SILENCE_FAILURE_MS, and a carriage-silence backstop that cannot fire (ledger A-180)",
    "major: docs/RELEASE-AUDIT.md:638-646 - §18's Deployed table names 838f101 while 2ea77bb is deployed; its verification lists predate the current blockers",
    "major: docs/ARCHITECTURE.md:329,538 - the published record limit at range is 162, not min(171,172-path)",
    "major: docs/SECURITY.md:70-71 - the handshake clock rule is documented without the 10-minute replay tolerance, which makes the floor check unreachable",
    "minor: docs/adr/0037:62 capacity arithmetic (172-10) is stale and its 'fits direct fits flood' conclusion is false below ~10 hops",
    "minor: docs/ARCHITECTURE.md:735 forty-second request cap versus requestIntervalFor's 20 s, with the rival design living in dead code",
    "minor: docs/SECURITY.md:48-50 three-minute phone advert versus ADVERT_INTERVAL_MS 60 s, and ARCHITECTURE's host-only 'off by default' stated as universal",
    "minor: docs/MESH-RADIO-CONSIDERATIONS.md:75-76,146 describe the pre-499de61 64 s budget and list a fixed item as open"
  ],
  "manualNotes": "How to read the nineteen entries: A-300/A-301 are the same defect seen in three documents and are the documentation half of the ledger's A-120 blocker; A-303, A-304, A-306 and A-315 are claims that are false in the unsafe or premature-confidence direction; A-307 and A-309 are the two places where a firmware number was transcribed into the docs with the wrong file, the wrong formula, or the wrong physical meaning. Nothing was appended to docs/RADIO-AUDIT-LOOP.md because the brief reserved that file to the parent."
}
```
### A-340 · shape: the reference client floods only when the path is *unknown*; we flood whenever it is *stale*, and that rule alone is 6.7× latency and 3× air · blocker

- Evidence: `mesh-research/clones/openhop_core/src/openhop_core/companion/base_send.py:262` and `:340` (`is_flood=contact.out_path_len < 0` — flood only when no out-path is known), `:194` (an explicit direct send after a path is learned); `crates/daemon/src/mesh_serial.rs:1413-1417` (`want_flood = peer_path.is_none() || path_stale || direct_silent || answer_flood_with_flood || peer_via_flood`), `:1388-1390` (`path_stale` = nothing heard for `route_stale(hops)`); `packages/mobile/…/AndroidRadioLink.kt:1013` (`floodWanted = (!directWorks || directSilent) && floodTokens > 0`); `mesh-research/clones/filter-firmware/src/Dispatcher.cpp:241-252` (a flood is deferred by `calcRxDelay` at **every** repeater; a direct packet is processed immediately); `Dispatcher.cpp:55-57` (the formula), `RadioLibWrappers.cpp:319-328` (the score), `MeshCore.h:20` (`MAX_PACKET_PAYLOAD 184`).
- Finding: upstream's rule is one line — a path, once learned, is used until it is *absent*, because a MeshCore direct path is **stateless**: the repeater forwards on the path bytes in the packet and keeps no route table (`Mesh.cpp:58`), so a learned chain stays valid as long as the topology does. Our rule instead re-floods whenever the path has been quiet, and `A-80` shows the path being re-adopted from the *shortest inbound* copy, so at range it is stale almost always. The price of that difference, computed from the firmware's own functions over 16 hops (airtime from the ledger's table: 172 B 602.6 ms, 133 B 490.0 ms): **direct 172 B = 9.8 s** one way (16 × 0.613, no hold), **flood 172 B = 65.6 s** (16 × (0.613 + 3.49)); **direct 133 B = 7.8 s**, **flood 133 B = 51.0 s**. Add three copies on the flood (`mesh_serial.rs:513`, `AndroidRadioLink.kt:2032`) and one flooded record costs ~**20×** a direct one in airtime-plus-latency.
- Why it hurts at range: this is the structural cost that no session-layer change touches. Every finding in the ledger about timing (A-41, A-80, A-81, A-83, A-105, A-164) is downstream of *not having a path*, and the single highest-leverage change available is to hold one. It also reframes the range problem: the mesh is not slow because the protocol is wrong, it is slow because we navigate by flood while the mesh's own client navigates by path.
- Proposed fix (NOT applied): invert the flood condition to upstream's — flood when there is **no** path (or when the path has produced nothing across a full round trip priced by A-81), not merely when it is quiet; and treat "a path exists" as a durable fact refreshed by *any* inbound packet's chain (which `mesh_serial.rs:2192-2204` already extracts), so a quiet link does not discard a working chain. Note the real constraint: `discovered from a REQ flood` is the only discovery channel that survives the advert hop cap (A-349).
- Open questions: how a genuinely dead path (repeater moved, chain broken) is distinguished from a quiet one without spending a flood — the honest answer is "a direct send that produces nothing across one full priced round trip", which is `direct_silent`, so the gate is the *pricing* of that round trip (A-81), not the rule.
- Investigator: shape round · 2026-11-11

### A-341 · shape: flood's per-hop hold is 4.1× longer at the margin than at home, so one set of timers cannot serve both — and ours are the home ones · major

- Evidence: `filter-firmware/src/helpers/radiolib/RadioLibWrappers.cpp:319-328` (score), `:310-317` (`snr_threshold[]`, SF7 −7.5); `filter-firmware/src/Dispatcher.cpp:55-57` (`(pow(10, 0.85f - score) - 1.0) * air_time`), `:241-252`; ledger A-40 (field SNR −6.75 and −8.5 dB), and the host journal's home readings at −84/−85 dBm ≈ +5.75 dB SNR (A-40's own distribution).
- Finding: the same 133-byte flood, at the same 16 hops, is held **2.70 s per repeater** at the margin (score 0.036) and **0.66 s per repeater** at home (score 0.480, since `(5.75+7.5)/10 = 1.325` clamps to 1.0 then ×0.48). One-way crossings: **51.0 s at the margin vs 18.3 s at home** for the same packet, i.e. a **2.8× spread in latency that is invisible to the code**. Every ldmux timer is a constant plus `hops × 2 s` (`MeshTermDriver.kt:2643-2646`, `answerDeadlineMs`) or `6 × airtime × hops` (`arq.rs:187`), so the same number is 3.6× too generous at home and 2.9× too tight at the margin.
- Why it hurts at range: this is "worked at home, failed at range" in one line — not two different bugs, the *same* protocol running against a latency that differs 2.8× purely from signal strength. Any home test therefore validates a timer that cannot be right in the field, and any field tuning invalidates the home test.
- Proposed fix (NOT applied): derive every hop-priced quantity from the measured score, not from a constant — `hold(score, airtime)` from `Dispatcher.cpp:55-57`, with `score` from the last SNR the radio reported (`codec.rs:1142` already exposes `last_snr_db`, ledger A-43 shows nothing branches on it). Then home and range get different timers from the same code, which is the only way one build serves both.
- Open questions: whether the score a repeater computes is visible to either end (it is not — this is a model, not a measurement), so the score must be computed from *our* measured SNR as an estimate of the chain's.
- Investigator: shape round · 2026-11-11

### A-342 · shape: MeshCore's own message layer would delete six blocker classes by construction, at the cost of a companion ABI, a contact trust model, and a 160-byte ceiling · major

- Evidence: `filter-firmware/src/Packet.h:21` (`PAYLOAD_TYPE_TXT_MSG 0x02`, "enc data: timestamp, text"), `Mesh.cpp:131-145` (dest/src hashes + MAC + AES body; **first packet wins** on flood), `helpers/BaseChatMesh.h:8` (`MAX_TEXT_LEN = 10 * CIPHER_BLOCK_SIZE` = **160 bytes**), `:77`/`:159` (`composeMsgPacket`, `sendMessage`), `BaseChatMesh.cpp:457-496` (`attempt` written into the payload, `expected_ack = SHA256(timestamp‖attempt‖text, pub_key)`, flood iff `out_path_len == OUT_PATH_UNKNOWN`, timeout from `calcFloodTimeoutMillisFor`/`calcDirectTimeoutMillisFor`); `crates/companion/src/codec.rs:71-76` (our companion crate speaks **only** `SEND_RAW_PACKET` 0x41; no `0x02`).
- Finding: this layer already provides, in firmware: authenticated per-attempt ACKs, dedup-safe retries (`attempt` is part of the packet), flood-until-a-path-exists then direct, a path return from the first flood (`Mesh.cpp:176`, `createPathReturn(&src_hash, secret, …, 0, …)` — the 0 is the 500 ms path-return delay the ledger already cites), end-to-end encryption with the peer's shared secret, and timeouts priced by the firmware's own formulas. It would make **A-20** (no handshake at all), **A-41/A-80/A-81** (no timers of ours), **A-102/A-123** (one copy, not three), **A-124** (retries are distinct packets), **A-180/A-220/A-240** (no session to wedge, kill, or fail silently) vanish *by construction* rather than by fix.
- Why it hurts at range: it is the difference between repairing six independent failure modes and deleting them — but the costs are real and mine, not hypothetical: (i) the companion crate must add `CMD_SEND_TXT_MSG` (0x02) and drain the firmware's message queue via `SYNC_NEXT_MESSAGE` (0x0A, already known to `codec.rs`), which replaces our push/frame receive model with a **poll-and-drain** whose queue depth is unknown — a burst answer could overflow it; (ii) the encryption key is the **contact shared secret from the advert exchange**, so pairing becomes "become each other's contact" instead of a QR PSK, a trust-model change that touches `docs/SECURITY.md` and the pairing code; (iii) 160 bytes of text per message against our 600-byte bodies means our own fragmentation and ordering come back for any answer bigger than one message — the layer deletes the ACK/retry/path machinery, not the framing.
- Proposed fix (NOT applied): not a proposal to adopt — a proposal to *keep the option open*, because the message bodies are already transport-agnostic (`crates/protocol/src/mesh_term.rs:336-520` is pure data). Concretely: finish the nine cheap structural fixes in A-344/A-345/A-340, and only if they leave a link that still cannot attach should this be attempted, behind a `features` bit as every other wire change is.
- Open questions: the firmware's message-queue depth on v1.17.1 (decides whether a 5-message answer survives at range); whether a companion can be *both* a contact and a PSK peer without duplicating the pairing story; and whether the phone's vendored `libmeshcore` already exposes a text-send entry point we could reuse instead of writing a new command path.
- Investigator: shape round · 2026-11-11

### A-343 · shape: the firmware makes every retry a *distinct packet*; ours resends identical bytes, which repeater dedup swallows · major

- Evidence: `filter-firmware/src/helpers/BaseChatMesh.cpp:457-476` (`temp[4] = (attempt & 3)` folded into the payload, and `expected_ack` derived from `timestamp‖attempt‖text`, so attempt 2 of a message is a different packet and a different ACK); `crates/daemon/src/mesh_serial.rs:487-506` (`send_retrying`: three rounds calling `attempt(deadline)` with the **same** `packet`), called inside the flood copy loop at `:1479`; the dedup rule recorded in `docs/MESH-RADIO-CONSIDERATIONS.md` (`SHA256(payload_type‖payload)`, 160-entry FIFO) and used at `Mesh.cpp:141` (`!_tables->wasSeen(pkt)`).
- Finding: our design understands that a repeat needs a fresh nonce — the flood copy loop rebuilds the wrapper with a fresh `route_nonce()` (`mesh_serial.rs:1471`) — but `send_retrying` predates that understanding and resends the identical packet. For `Disabled` (never transmitted) that is correct; for `Timeout` (possibly transmitted) the retry is dropped by the first repeater that already saw it, so the retry is silent airtime. The firmware solves the same problem structurally, by making the attempt number part of the authenticated payload.
- Why it hurts at range: retries-after-timeout are the normal case at the margin, so the one recovery path that must work is the one that silently does nothing — and it *looks* like it worked (a send confirmation comes back from the local radio).
- Proposed fix (NOT applied): carry an attempt counter in the clear record header (there is already a 6-byte clear header, `record.rs:22`) and require it to differ on retry, or rebuild the flood wrapper per retry as the copy loop does. The second is smaller and keeps the wire unchanged.
- Open questions: whether `send_packet_within` returns after the radio has *transmitted* or after it has *queued* (decides whether a `Timeout` retry can ever be a genuine duplicate at the radio).
- Investigator: shape round · 2026-11-11

### A-344 · shape: at the margin the unit of cost is the *answer's bytes*, not the round trips — and a thirteen-shell list is 4–13 records of pure framing · blocker

- Evidence: `crates/protocol/src/mesh_term.rs:171-186` (`MAX_BODY 600`, `MAX_ROWS_BODY 134`, `RECORD_PAYLOAD 143`), `:191-197` (`MAX_INPUT 137`), `MAX_SESSIONS` cap; `crates/daemon/src/mesh_bridge.rs:645-660` (`marks_pages`: an entry is an id plus two bytes and a dozen already overflows one message), the same paging for `Sessions`; `crates/session/src/arq.rs:227-228` (`credit = (64/hops).clamp(1,8)` → **4** at 16 hops); `MeshTermDriver.kt:1449-1460` (list → attach → profile → frame are **sequential asks**, each waiting for its own answer).
- Finding: an entry is `2 + 40 (id) + 24 (label) + 61 (cwd) + flags` — 60 to 129 bytes — against a 134-byte page ceiling, so a thirteen-shell host sends **roughly seven to thirteen zero-filler `Sessions` records** (≈780–1677 B of list). At 16 hops that is: 13 × 0.613 = **8 s of air direct**, 24 s as three-copy floods, and with a credit window of **4** that is `ceil(13/4) = 4` windows — i.e. **4 round trips ≈ 4 × 131 s ≈ 8.7 minutes** on the flood carrier, for a list of names. The answer's *bytes*, not its round trips, set that cost: the attach sequence is four asks (`Attach`, `Sessions`, `Profile`, `Frame`), but each ask is one record while its answers are 1–13 records, so compressing the answers is worth more than collapsing the asks.
- Why it hurts at range: this is the arithmetic behind "counts to 100 and then went back to 0" (A-104, A-280): the one step the user watches is the longest, and it is long because a shell list is sent as raw fixed-width text fields when it is 90 % repetitive (ids, labels, and a `cwd` that is usually the same directory repeated). `FrameDict` already proves the technique on the other message family (`mesh_term.rs:447`, `mesh_bridge.rs:739-830`).
- Proposed fix (NOT applied): give `Sessions`/`Marks` the dictionary treatment `Frame` already has (a run-level dictionary of the repeated `cwd` prefixes and label stems, referenced by index), and cap a page at one window's worth rather than one record's worth. Measured basis for expecting a win: A-140's own figures — a 923-byte screen → 508 (1.8×), a 1,640-byte repetitive command → 197 (8×); a 1,677-byte list of similar `cwd`s sits at the high end of that range.
- Open questions: the real compression ratio needs a measurement on the phone's actual shell list rather than a guess; and whether a page bigger than one record (allowed by `MAX_BODY`) would ever help given the window is 4 records — it would, if the pages were sent as one pipelined burst.
- Investigator: shape round · 2026-11-11

### A-345 · shape: "single-record stateless, no ARQ" is a *mode* of this design, not a replacement — and it is the right mode below one round trip · major

- Evidence: `crates/protocol/src/mesh_term.rs:336-520` (every exchange is already an independent message with its own tag and length), `:170-171` (`MAX_BODY 600`, and the comment records that a message larger than one record travels across records "which the ARQ and both decoders already do"); `crates/session/src/arq.rs:187-235` (the whole timer/window apparatus); `crates/session/src/record.rs:22-30` (per-record AEAD: 6 clear + 2 inner + 16 tag, so a record is independently openable once keys exist).
- Finding: the request/response traffic is already stateless in shape — `Attach`, `Profile`, `Resize`, `WantHistory`, `Input` are one-record asks whose loss is repaired by re-asking, and the driver already re-asks with its own deadlines (`MeshTermDriver.kt:1159-1212`). The ARQ adds nothing for those except a second, competing retry loop (A-105 shows the phone's deadline below the ARQ's timer, so both fire). Dropping ARQ in favour of idempotent re-ask would delete **A-80** (no timer to misprice), **A-102** (no window burst), **A-83**'s multi-window ceiling, and the whole `SessionFailed` family (A-180/A-220) — for custom bodies of one to three records. It cannot serve a 600-byte body or a repaint of fourteen records, where duplicate suppression and ordering are genuine work.
- Why it hurts at range: every range failure in the ledger is a *retry policy* failure, and we run two retry policies (the phone's deadlines, the ARQ's timers) that disagree. One policy, priced from A-341's score model, is strictly better than two — and the simplest way to have one is to make most traffic ARQ-free.
- Proposed fix (NOT applied): keep the session for keys and for bulk (repaint, history), and select "no-ARQ, idempotent re-ask" per message by a flag in the record header for asks that are one record and safe to duplicate. The phone's existing deadlines then become *the* retry policy for those messages, and the ARQ stops competing with them.
- Open questions: whether a duplicated `Input` record is safe to re-ask (it is not — it types twice), so the mode can only apply to idempotent asks; and how many messages fall in the idempotent set once enumerated.
- Investigator: shape round · 2026-11-11

### A-346 · shape: compression is a multiplier that fixes none of the blockers, and the uplink half of it is missing · minor

- Evidence: ledger A-140 (measured: 1,640 B paste 12 records → 2 deflated; 923 B screen → 508; 480 B snippet 4 → 2); `crates/protocol/src/mesh_term.rs:552`, `:565` (`Frame`/`FrameDict` host→phone **only**); `crates/daemon/src/mesh_bridge.rs:739-830` (the only compressor); `crates/session/src/record.rs:24-30` (24 bytes of overhead per record, so fewer records is also a smaller framing fraction).
- Finding: compression changes bytes per interaction by 1.8–8× and changes **no** failure mode: a records' worth of handshake is still 2.6 s of air (A-34), the timer is still priced wrong (A-80/A-81), the flood is still 6.7× more latent than direct (A-340). It is therefore a multiplier on whatever else is fixed, and the cheapest one available — but adopting it *instead of* the structural fixes would shorten a path that is still the wrong path.
- Why it hurts at range: it hurts only by being absent; there is no cost.
- Proposed fix (NOT applied): as A-140 says — a phone→host `FrameUp` behind a `features` bit — plus the list compression in A-344, which is the largest single answer on the link.
- Open questions: whether deflating keystrokes is ever worth it (a keystroke is 1–3 bytes; it is not).
- Investigator: shape round · 2026-11-11

### A-347 · shape: the "poor man's mode" (full-text screen, no diffs, no grid) is *worse* on the dimension that matters · minor

- Evidence: `crates/protocol/src/mesh_term.rs:439-452` (`Frame`/`FrameDict`) and `:373-383` (`Rows`/`Commit`, the diff path); `MAX_HISTORY_ROWS 64` (`:517`); A-140's measured 508 B for a full compressed screen; `MeshTermDriver`'s `RowMirror` (the diff mirror).
- Finding: the proposal is to drop the diff and send a rendered text screen every time. Costed: a compressed full screen is ~508 B ≈ **4 records** at 133 B payload; a one-line edit as a diff is **1 record** (`Rows` for that row + `Commit`). So the "simple" mode multiplies the most frequent interaction by four to save the least frequent one (scroll history). It also loses attributes and cursor-per-cell, which `Commit` carries (`MeshScreenBytes.kt:20`, per A-160's evidence).
- Why it hurts at range: typing is the interaction a terminal exists for, and at range it is the one already worst served (A-160: a keystroke echo takes 13–64 s). Making the echo four times more expensive is the wrong trade.
- Proposed fix (NOT applied): reject as a replacement. What is worth taking from it is the *idea* of a fixed, predictable frame size — which the compressed `FrameDict` already delivers.
- Open questions: none material.
- Investigator: shape round · 2026-11-11

### A-348 · shape: the ledger's own round-trip figures disagree, so every alternative above is priced against a number that needs one canonical derivation · question

- Evidence: this entry is self-referential by design. A-41 states "over a 14-repeater chain that is **44.7 s**", but 2.70 s × 14 = 37.8 s and 2.70 × 16 = 43.2 s — so the figure is a **16**-hop chain, mislabelled. A-81 then states "at hops=16 the timer is 47 s against a **63 s** round trip" — while `6 airtimes/hop` at 16 hops is 47 s and `11 airtimes/hop` is 86 s, and A-41's own per-hop hold implies 2 × 51 s = 102 s. Three different round-trip figures for the same route appear in the ledger: **63 s, 86 s, 102 s**.
- Finding: each round derived its own numbers from the firmware formula and none stated its crossing model (is a crossing `hops × (air + hold)`, or `hops × hold`, or does the origin's own airtime count?). The direction of every conclusion survives, but the magnitudes differ by up to 1.6×, and an implementer sizing a timer from this ledger would pick a number at random.
- Why it hurts at range: every fix proposed in the ledger is a timeout or a deadline, and a timeout derived from an ambiguous figure is the class of bug that started this audit (A-41/A-80/A-81 are all "priced from the wrong quantity"). The ledger should not add another.
- Proposed fix (NOT applied): add one canonical derivation to the ledger — state the crossing model explicitly (`crossing = Σ over hops of (air + hold(score, len))`, `hold` capped at 32 s by `MAX_RX_DELAY_MILLIS`, `score` from `packetScoreInt`), and recompute the three entries from it. The numbers used above in A-340/A-341 follow that model and are stated with their inputs so they can be checked.
- Open questions: whether a repeater's `ACTION_RETRANSMIT` adds its own delay on top of `calcRxDelay` (A-41's own open question), which would raise every crossing figure by up to another airtime per hop.
- Investigator: shape round · 2026-11-11

### A-349 · shape: path-first is not portable — an advert dies at `flood.max.advert` (default 3 hops), so a 16-hop path can only be learned from a request flood · major

- Evidence: `mcrs/CONFIG.md:138` (`set flood.max.advert`, default **3**), `:137` (defaults block: `flood.max.unscoped=5`), `mcrs/profiles/defaults.conf:21-22`; ledger A-60 (measured local mesh is permissive: adverts to 12 hops, requests to 16); `crates/companion/src/codec.rs:113` (`SEND_SELF_ADVERT`, documented as "how the peer learns a path to us"), `:122` (`GET_ADVERT_PATH`, "the only route a companion can learn on stock firmware"); `packages/mobile/…/MeshTermDriver.kt` advert cadence (one per other discovery attempt, per the ledger's `b13c9ba`).
- Finding: our path-discovery story rests on adverts, and adverts are the **most hop-limited packet type in the ecosystem** — 3 hops by default on a conforming repeater, against a link that needs 14–17. On such a mesh the advert cache is empty at range by policy, and the only discovery that can work is the request/reply flood whose *reply* carries the return chain (which is what our route request already exploits, per ADR-0035). So "path-first" (A-340) is correct but its **discovery channel must be the request flood, not adverts**, and the advert should be understood as a neighbour-only affair.
- Why it hurts at range: if the next fix is "keep a path", and the path is assumed to come from adverts, it will work on this operator's permissive mesh and fail on any mesh built from reference defaults — the same "works at home" trap one level up.
- Proposed fix (NOT applied): make the route request the documented and primary discovery channel at range, with the advert as a zero-hop/near-field optimisation; and make the phone's discovery cadence scale with the measured round trip rather than a minute.
- Open questions: whether the local repeaters are `filter-firmware` (no unscoped cap) or `mcrs` (default 5) — one `set` line from an operator settles which policy applies, and it decides whether A-60 is latent or live here.
- Investigator: shape round · 2026-11-11

### A-320 · paging: every shell switch destroys the screen, so five shells cost five repaints · major ·
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTermDriver.kt:1374` (`open()` — `mirror = RowMirror()`), `:1408` (`takeover`), `:1698` (`releaseShell`); `MeshTerminalScreen.kt:1168`/`:1399` (`onShow = selectShell(id)`, and `selectShell` is `open` at `:1485`); `crates/daemon/src/mesh_bridge.rs:1488` (`differ: Differ::for_attach(version, one)` — a fresh differ per attach). Measured with the real `FRAME_DICTIONARY` (extracted from `crates/protocol/src/mesh_term.rs:586`) and raw deflate on a realistic 46x38 screen (`LDMR` rows + commit): **1318 B raw = 10 records unframed, 294 B with the dictionary = 3 records**.
- Finding: the phone keeps exactly one `RowMirror` and the host keeps exactly one `Differ` per channel, both reset on attach. There is no per-shell screen cache anywhere, so paging the header through five shells re-attaches each one and re-sends a full screen every time: 5 × (1 `Attach` up + ~3 records down).
- Why it hurts at range: at 14 hops the ARQ credit is `64/hops` = 4 records in flight and one round trip is 76–86 s (A-41 arithmetic), so a repaint is roughly one round trip per shell. Five switches is **20 records and five round trips — around 5–7 minutes** of wall clock to look at five shells once, with every record repeated three times if the flood carrier is in use.
- Proposed fix (NOT applied): keep a bounded per-shell screen cache on the phone (the last N shells' rows and epoch) and let the host keep the `Differ` for a shell across a `Detach` for a short window, so a re-attach of a shell the phone already holds is a diff and not a repaint. Alternatively, make the launcher's rows carry the screen they last saw, so switching to a recently-seen shell paints immediately and asks for a diff.
- Open questions: whether `Differ` state is cheap enough to keep per session id instead of per channel; whether a cached screen can be invalidated safely when the shell printed while off-screen (it can — the epoch moves).
- Investigator: paging round · 2026-11-11

### A-321 · paging: switching shells throws away the phone's own scrollback · major ·
- Evidence: `MeshTerminalScreen.kt:709-716` (`LaunchedEffect(shells.current)` → `source.clear()` when the shell changes), `MeshTerminalSource.kt:631-645` (`clear()` → `forgetScreen()` → `history.clear()`), `MeshTerminalSource.kt:121` (`history = MeshHistory(MESH_HISTORY_MAX)`, `MESH_HISTORY_MAX = 2_000` at `:44`).
- Finding: two thousand rows of locally accumulated scrollback are dropped the moment the operator looks at another shell, and returning re-requests history from the host (`WantHistory` + pages). The rule is deliberate for *pixels* ("one shell's rows are never under another's name") but is applied to the scrollback too, which belongs to a shell and has a shell id.
- Why it hurts at range: the one thing a slow link makes precious is what you already have. A user who scrolls back, switches shells to check something, and returns pays a round trip (1 up + up to 3 pages down = measured 250 B framed = 2 records) to see what was free a moment before — and at 14 hops that is another minute.
- Proposed fix (NOT applied): hold history per shell id (a small map keyed by id, bounded overall by `MESH_HISTORY_MAX`), and drop only the engine and the drawn screen on a switch.
- Open questions: whether `MeshHistory`'s absolute-line numbering survives being kept across a switch (it does — `firstLine` comes from the host's `Commit`, which is per shell).
- Investigator: paging round · 2026-11-11

### A-322 · paging: the launcher is re-asked on every return, and its answer is 3 records · minor ·
- Evidence: `MeshTermDriver.kt:1456` (`openList()` queues `ListRequest` unconditionally), `:2106` (`backToList()` does the same), `MeshTerminalScreen.kt:749` (also once per connection when first live). Host answer: `crates/daemon/src/mesh_bridge.rs:1620-1666` (`Message::List` → `session_pages(...)` + `marks_pages(...)`). Measured with the real list on this host (13 shells, all v14 flags: id 10 B, label ≤24 B, cwd 60 B, origin/owner 1 B, shell 17 B): **8 pages, 806 B raw, 324 B deflated = 3 records**, and the marks page is appended to the same answer.
- Finding: the launcher costs 1 record up and 3 down, every time it is opened, whether or not anything changed, and the marks table is appended to every list even when it is empty (`mesh_bridge.rs:1667-1669` — "one record per list, empty included").
- Why it hurts at range: a user flipping between list and shell pays ~4 records ≈ 2 s of air direct, 6 s if flooded, plus a round trip each way. It is the cheapest screen in the app and it is still a minute at range.
- Proposed fix (NOT applied): nothing structural — but the list could be answered from a cached `listing()` with a short validity (the host already caches claims), and the marks page could be omitted when it would say nothing and the previous page said nothing.
- Open questions: how often the marks table is genuinely non-empty in use; whether the host's `listing()` RPC to the keeper is the real latency here rather than airtime.
- Investigator: paging round · 2026-11-11

### A-323 · paging: the list buffer merges runs and the first page of a run decides what the whole run means · major ·
- Evidence: `MeshTermDriver.kt:471` (`pages = ArrayList<SessionEntry>()`), `:2134-2146` (`onSessions`: `if (pages.isEmpty()) { listAskedAt = null; listAsks = 0; pagesAsked = listingForHeader || pick == Pick.Listing }`, then `pages.addAll(m.entries)`, and `if (!m.last) return`), `:2145` (`pages = ArrayList()` only on a `last` page); `MeshTermDriver.kt:805` (`failed()` clears `outgoing`, `handshake`, `session` and calls `forgetMarkPage()` — never `pages`).
- Finding: a run is identified by "the buffer was empty at the first page". A run interrupted by a session death, by a `List` answered after a `Detach`/`Attach` pair, or by a *pushed* list arriving mid-run leaves its pages in the buffer, and the next run's pages are appended to them; `distinctBy { it.id }.take(MAX_SESSIONS)` then commits a set drawn from two different answers, and the meaning of the whole thing was decided by the first page (asked-for vs pushed).
- Why it hurts at range: session replacement is the normal event at range (A-20, A-23), so a partially delivered list is the normal case rather than the exception — and the launcher then shows rows that are stale, duplicated-by-removal, or missing, with the user's cure (refresh) appending to the same buffer.
- Proposed fix (NOT applied): tag each page with the session index (or a run sequence) and drop pages from a different run; clear `pages` in `failed()` exactly as `forgetMarkPage()` is cleared there.
- Open questions: whether the host can be asked for a page-numbered list at all (it cannot today — `Sessions` carries only `last`, no index, `mesh_term.rs:405-414`).
- Investigator: paging round · 2026-11-11

### A-324 · paging: the list protocol has no page index, so a lost page is undetectable · major ·
- Evidence: `crates/protocol/src/mesh_term.rs:405-414` (`Sessions { last, cwd, origin, owner, shell, entries }` — no page number, no total), `:917-968` (the encoder); contrast `Marks`, which does carry `first`/`last` (`:429-430`). Phone side: `MeshTermDriver.kt:2145-2150` (a run ends only when `m.last` arrives).
- Finding: the only sequencing in a list answer is the `last` flag. If one middle page of an eight-page list is lost — the normal case at range, where each page is its own record — the phone receives `last` on the final page, has no idea a page is missing, and commits a launcher that silently omits shells. There is no gap detection, no retry of the missing page, and no sentence that says so.
- Why it hurts at range: lists are 3 records framed and 8 pages when unframed (A-322); losing one is expected at −120 dBm, and the resulting bug report is "some of my shells are gone", which is indistinguishable from "the shell died".
- Proposed fix (NOT applied): add a page index (or a first/next pair, as `Marks` already has) gated on a new `Sessions` flag bit; the phone then knows when a gap occurred, can ask once for the whole list again, and can say "list incomplete".
- Open questions: whether a stale-page-safe design is preferable (each page self-describing with a total count), and whether framing makes this moot in practice (framed, the list is 3 records, so a loss is per-record — still possible).
- Investigator: paging round · 2026-11-11

### A-325 · paging: a history press is unanswered for the route's whole round trip, with no feedback but one word · major ·
- Evidence: `MeshTerminalSource.kt:840` (`HISTORY_WAIT_MS = 10_000L`), `:787-800` (`askForHistory`: `if (awaitingHistory && now() - historyAsked < answerDeadlineMs(HISTORY_WAIT_MS, hops())) return` — the press is swallowed for the deadline), `MeshTermDriver.kt:2643-2646` (`answerDeadlineMs(base, hops) = base + hops*2000` → **38 s at 14 hops**), `MeshTerminalSource.kt:784` (`want` = the screen height, so one press asks for a whole page).
- Finding: at 14 hops a press that lands while an answer is outstanding does nothing at all for up to 38 s, and the only feedback is `scrollNotice = "fetching history"`. The press is neither queued nor counted; further presses are silently dropped.
- Why it hurts at range: the user's model is "press up again to go further up", and the app's model is "one page in flight, ignore the key". At range the difference is 38 s of an inert key that gives no reason.
- Proposed fix (NOT applied): show the deadline ("a page is on its way, ~40 s") and count dropped presses so the notice says what it did; or queue one extra press to fire on arrival.
- Open questions: whether `answerDeadlineMs` is even the right bound for a 3-page answer (it is sized for a single packet round trip).
- Investigator: paging round · 2026-11-11

### A-326 · paging: history is unreachable on a shell that has not committed a numbered screen · minor ·
- Evidence: `MeshTerminalSource.kt:781-790` (`top = screen?.let { if (it.firstLine > 0) it.firstLine.toInt() - history.size else 0 } ?: 0`; `if (top <= 0 || ask == null) { scrollNotice = "no history yet on this link" ... return }`), `crates/protocol/src/mesh_term.rs:490-496` (the `WantHistory` doc: the absolute line number comes from `Commit`).
- Finding: the phone can only ask for history once it knows the host's absolute line number, which arrives in a `Commit`. A shell whose first frame has not been committed — a screen that never painted, a session established but attach not yet answered — reports "no history yet on this link" and cannot ask, even though the keeper holds the rows.
- Why it hurts at range: "screen never painted" is precisely the at-range failure (A-102/A-108), so the two failures compound: no screen means no history means no way to see anything.
- Proposed fix (NOT applied): when `firstLine` is unknown, ask with `from = 0` and let the host clamp (it already does — `mesh_bridge.rs:2395-2404`), and let the answer place the page.
- Open questions: whether the host's clamp makes `from = 0` a full-history request rather than a page.
- Investigator: paging round · 2026-11-11

### A-327 · paging: going to the list restores the desk grid, so a bounce reflows the shell twice · major ·
- Evidence: `crates/daemon/src/mesh_bridge.rs:1972-1976` (`Message::Detach` → `release` + `restore_grid`), `:2006-2013` (`restore_grid` resizes the pty back to `attached.original`), `:1471-1480` (attach re-reads `original` and resizes to `remembered`), `MeshTermDriver.kt:1441` (the launcher queues `Detach`) and `:1383` (`open()` re-attaches).
- Finding: every visit to the launcher is a pty resize down to the desk's size and back up to the phone's. The shell sees two `SIGWINCH`, reflows its whole buffer twice, and the host then re-sends a full screen on the re-attach (A-320).
- Why it hurts at range: the user bouncing between list and shell to pick another shell pays two reflows and one repaint per bounce, and a full-screen TUI (a log, an editor) redraws completely both times — more output generated by the link's own behaviour.
- Proposed fix (NOT applied): do not restore the grid until the link has been unattached for a while (a short grace, like `ATTACH_GRACE` in reverse), so a quick bounce is one resize rather than two, and the desk is only given its pane back when the phone has really left.
- Open questions: whether a desk window legitimately needs its size back immediately (the comment at `:1683-1690` says it does when the user has stopped looking — a grace of a few seconds would still honour that).
- Investigator: paging round · 2026-11-11

### A-328 · paging: a rotation discards the scroll position, and the rule that protects height does not protect width · minor ·
- Evidence: `MeshTerminalSource.kt:509-515` — `reflow` is a width change, and `if (reflow) history.toBottom()`; the comment records the height case as fixed ("A width change is a reflow; a height change is not a reason to leave history"), citing ADR-0034. Rotation changes the width, so it parks the view at the bottom.
- Finding: ADR-0034's rule is implemented for height only. Rotating the phone while reading history throws the reader back to the live screen, and re-scrolling costs a press that (per A-325) may be inert for 38 s.
- Why it hurts at range: history is the thing a slow link makes expensive to reacquire, and rotation is the most casual accidental gesture on a phone.
- Proposed fix (NOT applied): keep the offset on a rotation too, and clamp it to what the new width can still place (the rows are re-wrapped by the phone's engine, so the offset is a row count in the new geometry).
- Open questions: whether a width change truly invalidates the row numbering (it does not — `firstLine` is the host's, and the host did not resize).
- Investigator: paging round · 2026-11-11

### A-329 · paging: the shells page ceiling is 134 B against a 143 B record, so an unframed list is one record per page · minor ·
- Evidence: `crates/protocol/src/mesh_term.rs:180` (`MAX_ROWS_BODY: usize = 134`), `:182` (`MAX_MESSAGE = 1 + 2 + MAX_ROWS_BODY`), `crates/daemon/src/mesh_bridge.rs:1421-1465` (`session_pages` packs to `MAX_ROWS_BODY`), `:858-866` (`streamed` gate: framing only at `FRAME_VERSION` 11+).
- Finding: the page size is chosen so a page never exceeds one message body, and with framing that is irrelevant (the run is concatenated and deflated). For a phone below version 11 the same list is 8 pages = 8 records for 13 shells; the phone in the field is version 14, so this is latent rather than active — but it means the page size is tuned for a path nobody takes.
- Why it hurts at range: none today; it matters because it makes the *unframed* fallback silently 3× more expensive, and the fallback is what a new/old client gets.
- Proposed fix (NOT applied): pack pages to the record payload (143) when unframed, or drop the unframed path for `Sessions` entirely once no shipped phone is below 11.
- Open questions: whether any deployed client is below `FRAME_VERSION`.
- Investigator: paging round · 2026-11-11

### A-330 · paging: `MAX_SESSIONS = 48` with 8 pages means a big host lists in 3 records and truncates in silence · minor ·
- Evidence: `crates/protocol/src/mesh_term.rs:206` (`MAX_SESSIONS: usize = 48`), `:1432` (`sessions.iter().take(MAX_SESSIONS)`), `MeshTermDriver.kt:2150` (`pages.distinctBy { it.id }.take(MeshTerm.MAX_SESSIONS)`).
- Finding: both ends truncate at 48 with no marker. A host with more shells shows 48 rows and says nothing; the phone cannot tell a complete list from a truncated one (compounded by A-324, which has no index at all).
- Why it hurts at range: this host had 13; the failure mode only appears on a busy machine, and then silently.
- Proposed fix (NOT applied): a "truncated" bit in the `Sessions` flags, and a row saying so.
- Open questions: whether 48 is a wire bound or a UI bound.
- Investigator: paging round · 2026-11-11
### A-380 · reference: the carrier is chosen **per attempt**, and we choose it per record from global flags · blocker

- Evidence: `mesh-research/clones/openhop_core/src/openhop_core/companion/base_send.py:1292-1346` (`_build_retry_packet`: "Build a retry attempt as a FLOOD request, whatever route the contact holds" — it masks `proxy.out_path_len = -1`, builds, and restores); `:262` and `:340` (`is_flood = contact.out_path_len < 0` — a *first* attempt is direct whenever any path is known); `:1220-1224` and `:735-738` (the retry loop: attempt 0 uses the held path, attempts 1..N call `_build_retry_packet`); firmware rationale quoted at `:1312-1318` (`MyMesh.cpp` masks `out_path_len` to `OUT_PATH_UNKNOWN` for one forced-flood request). Ours: `crates/daemon/src/mesh_serial.rs:1413-1417` (`want_flood = peer_path.is_none() || path_stale || direct_silent || …`) and `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1013` (`floodWanted = (!directWorks || directSilent) && floodTokens > 0`) — both global state, not this send's attempt.
- Finding: the reference has no ambiguity about *when* to flood: it floods only after a send on a held path produced nothing, and it floods **that one send's retry**, with the path masked for the duration of the build only. Our rule is a property of the *link*, evaluated identically for every record, so a single stale moment makes every record flood until a global flag flips — which is the mechanism behind the ledger's A-340 (flood-on-stale) and A-120 (the latch). The reference's version cannot latch, because nothing persists: the mask is restored on every exit path (`:1346-1347`).
- Why it hurts at range: at 16 hops one record costs 9.8 s direct and 65.6 s flooded (ledger, A-41/A-340). A per-record global rule spends the 65.6 s on data that a first direct attempt would have delivered in 9.8 s — and because the decision is not attached to an attempt, there is no event that ends it.
- Proposed fix (NOT applied): move the carrier decision to the send. `Command::Record` carries a `carrier: Carrier { Direct(PeerPath), Flood }` chosen by the caller's own attempt counter; the loop keeps the link-level rules only as a *fallback* for a send that has no attempt history. The phone's `AndroidRadioLink` does the same with its tracked-send table rather than `directWorks`/`directSilent`.
- Open questions: whether the phone's first attempt after a fresh handshake should count as attempt 0 (direct) or as a retry (flood), since a handshake that completed over flood says nothing about the direct chain; and whether a masked-path direct→flood escalation needs a per-contact not-before time to stop a rapid retry storm.
- Investigator: reference-comparison round · 2026-11-11

### A-381 · reference: a flood reply means *the peer has no route back to us*, so it teaches one — we mirror the carrier instead · blocker

- Evidence: `openhop_core/src/openhop_core/node/handlers/return_path.py:1-46` (the mechanism, quoting `simple_repeater` `onPeerDataRecv` and `BaseChatMesh::onPeerDataRecv`'s `handleReturnPathRetry`), `:377-440` (`maybe_teach_from_flood_reply`: fires only when the reply is flood-routed **and** we already hold a direct `out_path` — "a flood reply from a peer we hold a direct path for is the tell: the peer has no usable route back to us"), `:120-136` (`RETURN_PATH_COOLDOWN_S = 5.0`, a deliberate addition over firmware). Ours: `crates/daemon/src/mesh_serial.rs:1285`, `:2208`, `:2278` (`peer_via_flood` set by a flood, cleared only by a direct arrival) and `:1413-1425` (`want_flood` includes it); `AndroidRadioLink.kt:531`, `:594` (`directWorks` true only on a direct arrival, false on any flood).
- Finding: both systems see the same signal — a flood arrived although a path was known — and draw opposite conclusions. The reference concludes *the peer cannot reach us* and repairs that (send a PATH DIRECT down our own path, unthrottled by firmware but throttled to 5 s here). We conclude *flood works* and stop using direct, which removes the only traffic that could ever prove direct works again. The reference's repair also cannot bounce: `onPeerPathRecv` stores the path and returns `false`, so no reciprocal reply follows (`:60-66`).
- Why it hurts at range: the ledger's A-120 latch, and it is the reason a link that has ever flooded stays flooded: every record costs ~6.7× latency and 3× air while both ends wait for a direct record neither will send.
- Proposed fix (NOT applied): replace `peer_via_flood` on both ends with a **repair**: on a flood-carried record that arrived while `peer_path` is held, queue one `LDRR`/PATH teach sent **direct** along that path, throttled per peer by a 5 s cooldown. Clear the flood preference on *any* arrival (ADR-0038's own rule) and let the teach be what re-proves direct.
- Open questions: whether our `LDRR` reply is acceptable as a PATH teach to the phone (it carries the path the request crossed, which is what the reference embeds), and whether the cooldown should be keyed per peer key or per (peer, carrier).
- Investigator: reference-comparison round · 2026-11-11

### A-382 · reference dedupes **after** a reception-quality hold, so the best copy wins — we have no dedup of our own · blocker

- Evidence: `openhop_core/src/openhop_core/node/dispatcher.py:634-641` (a flood packet is held for `_flood_rx_delay_ms(len, snr)` **before** processing), `:552-554` (`_hold_flood_packet`), `:519-528` (`calc_rx_delay`: `(rx_delay_base ** (0.85 - score) - 1.0) * air_time_ms`), `:44-45` (`MIN_RX_DELAY_MS = 50`, `MAX_RX_DELAY_MS = 32000`), `:643-655` (the dedupe check explicitly "must run after the hold, like firmware's hasSeen — then drops this copy when it wakes"), `:176` (`rx_delay_base` default 0, i.e. stock firmware processes immediately). Ours: `crates/daemon/src/mesh_serial.rs:2207-2221` sends **every** flooded record to `Event::Recv`; the only dedup is the radio firmware's `SHA256(payload_type‖payload)` (`:447`), which our fresh wrapper nonce defeats on purpose.
- Finding: the reference's ordering — hold by reception quality, *then* dedup — makes the first-processed copy the **best-heard** one rather than the first to arrive, and it is what makes "which copy won" deterministic and lucky-free. We have no receive-side ordering at all: whichever copy the radio decoded first wins, and each of our three copies is a distinct packet by construction.
- Why it hurts at range: this is the ledger's A-20 at the mechanism level. Three handshake copies arrive seconds apart and in arbitrary quality order; we process all three, so three sessions exist. With the reference's ordering, the copies would collapse to one — the best-heard one — before the session layer ever saw them.
- Proposed fix (NOT applied): in `mesh_serial`, hold a flood-carried record for a delay scaled by its reception quality (`calc_rx_delay` in ms, clamped 50 ms…32 s) and keep a small content table keyed on `(payload_type, payload)` — the same key the firmware uses, so the behaviour is defensible to a MeshCore operator — dropping a copy whose entry was already delivered. Combined with A-20's idempotent `msg1` cache this makes "which copy won" irrelevant.
- Open questions: whether a hold is affordable at all on the host given A-61 (the receive loop currently blocks on sends, so a hold would compound); and whether the table should key on the pre-dedup hash the radio already computes.
- Investigator: reference-comparison round · 2026-11-11

### A-383 · reference ranks a route by decodability margin, then hops — we rank by hops alone · major

- Evidence: `openhop_core/src/openhop_core/node/handlers/return_path.py:639-662` (`_copy_rank`: "A copy at or above `reliable_margin_db` outranks every copy below it, however strong the weaker one's signal looks. Within that reliable tier the order is fewest hops, then margin, then RSSI"), `:138-196` (`SF_DEMOD_SNR_LIMIT_DB` per SF, `RETURN_PATH_MIN_MARGIN_DB = 2.0`, `RETURN_PATH_RELIABLE_MARGIN_DB = 6.0`, `RETURN_PATH_ASSUMED_NOISE_FLOOR_DBM = -108.0`, and the measurement that settles it: "four copies of one reply arrived at -16, -21, -69 and -80 dBm and *every one* reported +11.25 to +12.5 dB SNR … RSSI spanned 86 dB while decodability was pinned at 'certain'"), `:317-339` (`note_flood_copy` collects copies pre-dedup so a better one can still be chosen). Ours: `crates/daemon/src/mesh_serial.rs:404-436` (`prefer_route`: `candidate.hops < current.hops`, with `proven`/`dead`/`stale` gates and no signal term).
- Finding: our route decision has no notion of whether a candidate's link actually decodes. A one-hop copy at −124 dBm/−8.5 dB SNR is preferred to a six-hop copy at −107 dBm/+5 dB, and the ledger's A-80 records exactly that flipping the adopted path 1↔16 hops within eleven seconds. The reference's rule is the same shape as ours in the reliable tier (fewest hops wins) — the difference is the gate: a marginal copy is never allowed to win on hop count.
- Why it hurts at range: at the margin, hop count alone is noise. Every adoption from a copy below the demodulator limit replaces a working route with a ghost, and each replacement costs a full RTO (A-80) plus the record retransmissions that follow.
- Proposed fix (NOT applied): give `prefer_route` a margin term computed from the frame's SNR against `SF_DEMOD_SNR_LIMIT_DB[sf]` (already available in `crates/companion/src/codec.rs` as `last_snr_db`, per the ledger's A-40), with the reference's two-tier rule: `margin >= 6 dB` ⇒ rank by hops; below ⇒ rank by margin first, hops second. Candidates below `2 dB` are not routes.
- Open questions: whether the phone's radio reports per-frame SNR for a `LOG_RX_DATA` push (the host has it; the phone's `mesh heard … <snr> dB` line suggests yes), and whether the 6 dB/2 dB thresholds need recalibration for 62.5 kHz versus the 250 kHz the reference measured on.
- Investigator: reference-comparison round · 2026-11-11

### A-384 · the firmware tells you which route it used, and we throw that byte away · major

- Evidence: `mesh-research/clones/meshcore-pi/companionradio.py:131-150` (`sent_resp` builds `[RESP_CODE_SENT][flood?][tag 4][rtt 4]` — the second byte is the route the send actually took); our own crate documents the same layout `crates/companion/src/codec.rs:19-24` ("Ten bytes on the v1.18 protocol: the route flag, the tag, and the firmware's own suggested timeout"), but `crates/companion/src/session.rs:318-327` (`suggested_timeout_ms`) reads only `frame.get(6..10)` and nothing anywhere reads byte 1.
- Finding: the radio's own statement of the carrier is available on every confirmed send and is discarded. Both ends therefore infer the carrier from arrivals instead — which is exactly the inference that produced A-120's latch and A-340's flood-on-stale.
- Why it hurts at range: a correct carrier policy needs to know whether a *specific send* left direct or flooded. Inferring it from what arrives conflates "the peer did not answer" with "the peer answered by a route we did not expect", and on a link where answers take 40 s the conflation is permanent.
- Proposed fix (NOT applied): parse the route byte in `suggested_timeout_ms`'s sibling (`parse_send_confirm` returning `{ route: Direct|Flood, suggested: Option<Duration> }`), surface it from `Companion::send`, and let the carrier decision (A-380) use it. Firmware that answers a bare `OK` reports no route, which must read as "unknown", not "direct".
- Open questions: whether firmware sets the flag from the packet header we wrote (our `0x3E`/flood header) or from the route it actually used after its own deferral, which decides whether the flag can ever disagree with us.
- Investigator: reference-comparison round · 2026-11-11

### A-385 · reference scope resolution is a three-level chain; ours is a two-level one with no transient · major

- Evidence: `openhop_core/src/openhop_core/companion/base_config.py:334-369` (`_apply_flood_scope`: a pre-applied scope wins; `_flood_unscoped` short-circuits to a plain flood; otherwise `_resolve_flood_transport_key()`), `:371-384` (`_apply_default_flood_scope`: a *persisted default* only, used for adverts, bypassing both the transient override and the unscoped flag), `:386-...` (`_apply_path_hash_mode`). Ours: `crates/daemon/src/mesh_serial.rs:652-699` (`ScopeChoice`: `LDMUX_MESH_SCOPE` env pin → the radio's own default → none, resolved **once**).
- Finding: the reference resolves a scope **per packet**, from three sources in priority order (unscoped request, transient override, persisted default), and marks the packet so a later layer cannot silently re-decide. Ours resolves once per connection and has no per-send override, no persisted default of its own, and no advert-specific path.
- Why it hurts at range: the ledger's A-60 — an unscoped flood dies at 5 hops. Ours can only be scoped by an env pin or by a scope already on the radio; there is no way to test a region, no way to scope one send, and a `denyf` mesh cannot be served at all without editing the radio first.
- Proposed fix (NOT applied): a `ScopeChoice` with the reference's three levels (per-send override carried on `Command::Record`, then the radio's default, then the persisted `LDMUX_MESH_SCOPE`), and a separate advert path that uses the persisted default only — because an advert is firmware-built and would otherwise advertise unscoped on the one mesh that needs a scope.
- Open questions: whether MeshCore's companion command set lets a client send one transiently scoped packet at all (our `set_default_flood_scope` writes the radio's *persistent* default), which decides whether the transient level is reachable or must be emulated by write-restore-write.
- Investigator: reference-comparison round · 2026-11-11

### A-386 · reference paces transmissions with an airtime leaky bucket and never drops; our phone's bucket counts floods · major

- Evidence: `openhop_core/src/openhop_core/node/dispatcher.py:47-57` (`DUTY_CYCLE_WINDOW_MS = 3_600_000`, `MIN_TX_BUDGET_RESERVE_MS = 100`, `MIN_TX_BUDGET_AIRTIME_DIV = 2`, `MAX_TRANS_UNIT = 255`, duty `1/(1+airtime_factor)` = 50 % by default), `:809-830` (`_tx_budget_wait_s`), `:831-846` (`_await_tx_budget`: "Sleep (never under `_tx_lock`) until the airtime budget admits; **never drops**"), `:850-870` (`_debit_tx_budget` debits the packet's estimated airtime and sets `_tx_next_time`). Ours: `packages/mobile/.../AndroidRadioLink.kt:273-287` and `:2022` (`FLOOD_BURST = 4`, `floodTokens`, refilled one per `floodIntervalMs`), `:1027-1038` (copies are **skipped** when the bucket is dry); `crates/daemon/src/mesh_serial.rs:722-735` (`airtime_budget_ms` is `Some` only inside three European bands, else no allowance at all).
- Finding: the reference's pacer is denominated in **airtime** and its response to a dry budget is to *wait*, not to drop. Ours is denominated in **counts** and its response is to drop — which is precisely the ledger's A-230-class bug in `d4a20e3` (the bucket skipped handshake copies 2/3). Counting floods is also the wrong unit: one flood of a 133-byte record is 0.49 s of air, one of a 42-byte request is 0.22 s, and a bucket that cannot tell them apart paces neither.
- Why it hurts at range: on a shared city mesh the scarce resource is seconds of air per hour, and a count-based bucket spends it unevenly — permitting four small floods and four large ones equally — while a drop-based response loses the very copies that were added for reliability.
- Proposed fix (NOT applied): replace `floodTokens`/`refillFloodBudget` with an airtime meter (debit `record.len()`'s estimated airtime, refill at a duty cycle), and make a dry meter *delay* the copy rather than skip it, bounded by the send's own deadline. The host's EU-only `airtime_budget_ms` stays as the legal figure; this is a separate, always-on neighbourliness meter.
- Open questions: what duty cycle is right for a companion that carries a terminal; whether the delay-not-drop rule needs a floor below which a copy is abandoned (otherwise a budget-starved send never completes).
- Investigator: reference-comparison round · 2026-11-11

### A-387 · reference never blocks the receive path on a send — a delayed teach is queued with a settle window · major

- Evidence: `openhop_core/src/openhop_core/node/handlers/return_path.py:51-60` ("Firmware paces its retry with `sendDirect(..., 3000)` — a *queued* send whose 3 s delay is a settle window … not a busy wait. openHop honours that window (`RETURN_PATH_SETTLE_S`) in a background task rather than inline on the RX path: the injector may block on a TX lock and an airtime budget, and awaiting it inline would delay delivery of the very reply that triggered the teach"), `:104` (`RETURN_PATH_SETTLE_S = 3.0`), `:445-460` (`schedule_reteach_better_copy`: "it must not be awaited by the caller … or `wait_for_pending_reciprocals` would block for the whole window"), `dispatcher.py:831-846`. Ours: `crates/daemon/src/mesh_serial.rs` sleeps inline before answering a flood (`reply_delay(airtime_ms)`, the fix from `f413baf`), on the thread that reads the radio — the ledger's A-61.
- Finding: the reference names our defect and its fix in one paragraph: a delay belonging to an *outbound* packet must be a scheduled "not before", not a sleep on the path that delivers inbound packets. Two of our fixes (`f413baf`'s reply delay, A-61's route-reply sleep) introduced inline waits; the reference has the same delay and pays nothing for it.
- Why it hurts at range: at 14–17 hops the reply delay is 0.5–2 s and the send behind it can block for tens of seconds; every handshake copy arriving in that window is never read, so a rung is lost to a delay we chose.
- Proposed fix (NOT applied): an outbound queue with `not_before: Instant` per entry; the loop polls it between radio reads, sends what is due, and never sleeps on behalf of a queued packet. This is the same structure A-61 needs, so one change closes both.
- Open questions: whether the companion's serial session can tolerate a send being started from a different iteration than the one that queued it (the firmware's own `queueOutbound` suggests yes), and whether the radio's `busy`/`Disabled` answers then need to push the not-before forward rather than retry immediately.
- Investigator: reference-comparison round · 2026-11-11

### A-388 · reference bounds one attempt at 12 s and re-sends; ours waits out the crossing · question

- Evidence: `openhop_core/src/openhop_core/companion/timing.py:24-31` (`DEFAULT_MAX_ATTEMPTS = 3`, `MIN_TIMEOUT_MILLIS = 1500`, `MAX_TIMEOUT_MILLIS = 12000`) and `:104` (the clamp), with the rationale in the module docstring `:1-16` (firmware's `calcFloodTimeoutMillisFor`/`calcDirectTimeoutMillisFor`, and "previously used fixed 10 s / 15 s waits with no resend, so a single lost packet stalled for 10-15 s where firmware recovers in ~3 s"); `base_send.py:60` (`FRAME_LOGIN_PENDING_TTL_S = 120.0`, "a slow flood reply can still complete login … observed multi-hop replies can take over a minute"). Ours: `crates/session/src/arq.rs:187-191` (`rto_initial = 6 × airtime × hops`, 47 s at 16 hops), and the phone's `handshakeWaitMs` (ledger A-101/A-28).
- Finding: the reference resolves the "long crossing versus slow recovery" tension by capping a single wait at 12 s and re-sending up to twice, while keeping a 120 s *outer* window for the flow to complete; we resolve it the other way, waiting one full crossing before any retransmission. Both are coherent. What is not coherent is ours plus a budget shorter than the crossing: A-80/A-101 show attempts exhausted before one round trip completes.
- Why it hurts at range: three 65.6 s flood crossings inside a 120 s window do not fit, whatever the retry cadence. So the budget and the RTO must be derived from the same number, and today they are not.
- Proposed fix (NOT applied): keep our airtime-first RTO (a retransmission inside its own crossing is pure waste on a shared mesh) and instead make the *budget* explicit: `attempts × rto + one answer window`, so the numbers cannot disagree. Do **not** adopt the 12 s cap.
- Open questions: whether the phone should use the firmware's suggested timeout (the `RESP_CODE_SENT` value, already parsed at `crates/companion/src/session.rs:318-327`) for its own re-ask cadence instead of its own arithmetic, since that figure knows the route the radio actually used.
- Investigator: reference-comparison round · 2026-11-11

### A-389 · reference's queue protects an unacknowledged direct message from eviction; our session replacement clears the outbox · major

- Evidence: `openhop_core/src/openhop_core/companion/message_queue.py:9-44` ("When full, the oldest channel message is evicted; **direct messages are never displaced by another queue entry**", with `deque(maxlen=...)` deliberately avoided so eviction stays explicit). Ours: `crates/daemon/src/mesh_session.rs:329-332` (`self.session = Some(session)` replaces the session that owns the ARQ, then `outbox.clear()`), the ledger's A-23.
- Finding: the reference's queue has an eviction *policy* that ranks traffic by value; ours has no policy because the whole queue dies with the session. The principle transfers: data that has not been acknowledged is more valuable than data that has not been sent, and neither should be dropped by an event that has nothing to do with them.
- Why it hurts at range: at range a session replacement is routine (A-20 makes it three per handshake attempt), so an attach answer that was half-transmitted is discarded and re-derived — the "counts to 100 then resets" shape.
- Proposed fix (NOT applied): on replacement, hand the old ARQ's unacknowledged payloads back to the caller (one `Vec<(shell, bytes)>`), re-encode them onto the new session, and only then clear. If that is too much for one change, log how many bytes were lost so the loss is at least observable.
- Open questions: whether re-sending an attach request across a key change is idempotent at the daemon (it is a request, so yes by construction) and whether an unacknowledged *output* segment must be re-fetched from the shell instead of replayed.
- Investigator: reference-comparison round · 2026-11-11

### A-390 · reference logs one PATH DIAG line per attempt; we log after the fact and only sometimes · major

- Evidence: `openhop_core/src/openhop_core/companion/base_send.py:749-757`, `:798-807`, `:1235-1244` — `"[PATHDIAG] %s: route=%s attempt=%d/%d timeout=%.1fs out_path_len=%s"` emitted **before every transmit**, naming the carrier, the attempt index, the wait it expects and the path length in hand. Also `:1345-1346` (`retry forced to FLOOD`), `return_path.py:684` and the `[ReturnPath] Skip teach to 0x%02X: inbound path …` refusal line. Ours: `crates/daemon/src/mesh_serial.rs:1219-1233` region (`route_log_rare`, burst-level "request from … hops=N (adopted)"), the added `record N B sent by <carrier> over H hop(s)` line, and A-220's silent session death.
- Finding: the reference's instrumentation is *decision-first*: it prints the carrier and attempt it is about to use, so a log reader sees the policy as it runs. Ours is *outcome-first* and rate-limited, so the policy has to be inferred from adoptions — and A-220 records a death reason that is computed and never printed.
- Why it hurts at range: every investigation in the ledger's own history had to reverse-engineer what the code decided (A-30, A-280). A per-attempt line is one format string and closes that class.
- Proposed fix (NOT applied): one line per transmit at the point the carrier is chosen — `record <n> B attempt <i>/<n> carrier <direct|flood> path <h> hops rto <ms> rssi <dbm> snr <db> margin <db>` — rate-limited by *attempt*, not by burst, so three copies of one attempt are three lines and a reader can see them.
- Open questions: whether at 18 records per attach the volume is acceptable on a journald host (it is ~2 lines a second at range), and whether the same line should go to the daemon's own rotating log rather than journald.
- Investigator: reference-comparison round · 2026-11-11

### A-391 · reference computes link margin against the demodulator limit; none of our decisions see it · major

- Evidence: `openhop_core/src/openhop_core/node/handlers/return_path.py:160-186` (`SF_DEMOD_SNR_LIMIT_DB` for SF5–SF12; "LoRa demodulates below the noise floor, so what decides whether a copy's link works is its SNR margin over the demodulator limit for the spreading factor — not its absolute RSSI"), `:186-196` (`RETURN_PATH_ASSUMED_NOISE_FLOOR_DBM = -108.0` with the measurement "these nodes are noise-limited, not sensitivity-limited (datasheet sensitivity would be ~-126 dBm)"); `mesh-research/clones/filter-firmware/src/helpers/radiolib/RadioLibWrappers.cpp:310-317` (the same table in firmware). Ours: the host already captures the peer's own `(snr, rssi)` per received record — `crates/daemon/src/mesh_serial.rs:1289`, `:2210`, `:2281` (`peer_signal`), surfaced as `Event::Signal` at `:1786-1800` — but it is used only for the status display, and no caller converts it to a margin; every route decision (`:404-436`) reasons from hop counts alone.
- Finding: the reference's central quantity — dB above the demodulator limit — does not exist anywhere in our code. Every "is this good" judgement we make is either a hop count or an RSSI, and the reference's own measurement shows RSSI spanning 86 dB while decodability stayed constant.
- Why it hurts at range: our ledger puts the field link at ~0 dB margin (A-40). Without the quantity we cannot tell "marginal but working" from "dead", so we cannot make a range-appropriate decision: more copies, shorter records, or give up.
- Proposed fix (NOT applied): a `LinkMargin` value computed in `crates/companion` (`snr − SF_DEMOD_SNR_LIMIT_DB[sf]`, RSSI − assumed floor as fallback), logged on every arrival and used by A-383's route ranking and by the phone's status line ("0 dB — at the demodulator limit") instead of raw dBm.
- Open questions: whether `LOG_RX_DATA` carries SNR per frame on the firmware this radio runs (the phone's `mesh heard … -4.75 dB` line says yes for the phone's own radio) and whether 62.5 kHz changes the −108 dBm floor the reference measured at 250 kHz.
- Investigator: reference-comparison round · 2026-11-11

### A-392 · reference distinguishes `out_path_len == 0` (a known zero-hop route) from `< 0` (unknown) · minor

- Evidence: `openhop_core/src/openhop_core/companion/base_send.py:258-262` — "Only OUT_PATH_UNKNOWN (-1) floods; `out_path_len == 0` is a known zero-hop direct route, matching the builder's route selection and MeshCore sendRequest"; same note at `:339-341` for anon requests and `:1305-1316` for the forced-flood mask. Ours: `crates/daemon/src/mesh_serial.rs:1413-1417` treats `peer_path.is_none()` as "try the near field once, then flood", which is the right *behaviour* but is expressed as a special case rather than as a tri-state.
- Finding: this is the trap that makes a flood-on-stale rule look reasonable: if "no path" and "a zero-hop path" are the same value, then every place that tests for a path has to also test for staleness, and the tests multiply. The reference keeps three states (unknown / zero-hop / n-hop) and floods on exactly one of them.
- Why it hurts at range: not on its own — but the ledger's A-340 fix ("flood only when the path is unknown") cannot be implemented cleanly in our code until the states are distinct, because `peer_path: Option<PeerPath>` plus `path_stale()` is a four-way state.
- Proposed fix (NOT applied): an explicit `Route { Unknown, ZeroHop, Hops(PeerPath) }` in `mesh_serial`, with `carrier_for(route, attempt)` as the single place the decision lives, replacing the `want_flood` boolean expression.
- Open questions: whether `PeerPath` can represent zero hops today (the ledger's A-80 evidence suggests `hops_now` falls back to 1 when no path is held, which conflates the two).
- Investigator: reference-comparison round · 2026-11-11

### A-393 · reference refuses to teach a path it did not observe; our reversals are observed, and the trap is worth recording · minor (confirmed sound)

- Evidence: `openhop_core/src/openhop_core/node/handlers/return_path.py:76-86` — "an earlier revision, on a request timeout, taught the reverse of our own `out_path` on the assumption that routes are symmetric. Real routes frequently are not, and `onPeerPathRecv` overwrites `client->out_path` unconditionally, so a wrong guess replaced a working route with a dead one and the peer then answered DIRECT into a void — leaving no flood reply to correct it", with the alternative at `:86-92` ("the request itself is re-sent as a flood on retry, and the peer answers a flood request with a PATH-return, which is a real inbound path to teach from"). Ours: every adoption reverses a chain that actually arrived — `crates/daemon/src/mesh_serial.rs:297`, `:357`, `:580`, `:2002`, `:2097`, `:2137`, `:2150`, `:2181` (`PeerPath::reversed` applied to an inbound request/reply/advert/record path).
- Finding: our rule is the reference's corrected rule, not its mistake: we reverse only a path we observed, and `:1992-1996`'s comment states it ("the path the flood took is the phone's chain reversed"). No code path fabricates a route from our own outbound path. That is worth recording as sound, because the temptation to do it is exactly what a "route repair" fix would introduce.
- Why it hurts at range: it does not, today — but the ledger's register now proposes two repairs (A-120's probe, A-340's re-ask) whose cheapest-looking implementations are precisely the guess the reference had to remove.
- Proposed fix (NOT applied): none. Record the constraint: any future path repair must teach an observed chain or use a request/reply round trip, never a reverse of our own outbound path.
- Open questions: whether an advert's path (we reverse it too) is as trustworthy as a request's, since an advert is broadcast and its chain says only how it reached us.
- Investigator: reference-comparison round · 2026-11-11

---

**Adopt first (in this order):**

1. **A-380 + A-384 — a per-attempt carrier decision, fed by the route byte the firmware already returns.** It is the smallest change that removes the whole flood-on-stale/latch family, and the evidence is on the wire today.
2. **A-381 — repair the return path instead of mirroring the carrier.** One throttled PATH teach replaces the latch; it is also the only mechanism that gets a link *back* from flood to direct without a lucky arrival.
3. **A-382 — hold a flood by reception quality, then dedupe.** It collapses our three copies to one best-heard copy before the session layer, which is A-20's cause.

**Refuse: A-388's 12 s attempt cap.** It is coherent for a request/response companion that values recovery time; it is wrong for a terminal on a shared mesh where a retransmission inside its own crossing is pure airtime waste. Keep our crossing-length RTO and fix the *budget* instead.

**Could not determine:** (1) whether MeshCore's companion commands permit a *transient* per-send scope (A-385's middle level) or only a persistent default write; (2) whether the `RESP_CODE_SENT` route byte reflects the header we wrote or the route the firmware finally used after its own deferral; (3) whether firmware's reception-quality hold (`rx_delay_base`) is reachable from the companion protocol at all — the reference enables it in its own dispatcher, but a stock companion radio runs firmware's default of 0, so A-382 may have to be implemented on our side of the serial link rather than theirs; (4) what duty cycle (A-386) is defensible for a companion that carries a terminal, since the firmware's 50 % default is written for a repeater.
### A-360 · paths: a flood marks the path *proven*, which then locks out the peer's own fresh chain · blocker

- Evidence: `crates/daemon/src/mesh_serial.rs:2188-2204` (`prefer_route(..., arrived_by_flood, &candidate)` then `peer_path = Some(candidate); peer_path_proven = true;`) with `:2170` (`let arrived_by_flood = true;`); `:425-434` (`if proven { return false }` — a proven route is not displaced by any candidate); `:166-169` (`route_stale(hops) = max(60 s, 2 × 603 ms × hops)`); `:2141`/`:2154`/`:2246` (route-request and route-reply candidates are passed with `dead = false`, so they meet the `proven` gate); `filter-firmware/src/helpers/BaseChatMesh.cpp:359-363` (`onContactPathRecv` replaces the stored path whenever the peer sends one, with no notion of "proven" at all), `:370` and `:381` (a send is considered delivered only when an **ACK that crossed the path direct** arrives — that is upstream's proof, and it is an arrival, not a hop count).
- Finding: a flooded record was carried by the **flood** chain in the **inbound** direction; the path this end stores from it is the *reversal* of that chain — a guess about the outbound direction that has never carried one of our packets. The code nevertheless sets `peer_path_proven = true`. `prefer_route` then refuses every candidate that is not `dead`, so for the next `route_stale` window (60 s at 16 hops) the peer's **route request** — which it sends precisely because it cannot hear us — cannot replace the stored chain, and neither can a cache answer. The only things that still move it are another flood (which re-marks it proven) or the staleness timer.
- Why it hurts at range: at range every record arrives by flood, so the host's path is permanently "proven" by a chain that has only ever worked in one direction, while the phone is re-requesting a route every 5–20 s with a chain that does work in the other. Measured shape, journal 2026-11-10: `route request from 26f6071f hops=1 (adopted)` … `hops=16 (adopted)` … `hops=1 (adopted)` inside 70 s, i.e. the host's own chain moved under it repeatedly, and `flooded record 40 B from a 14 hop chain` / `from a 1 hop chain` — the same peer, two different chains, both taken as proof. Every record sent direct down the ghost costs 7.8 s of air and lands nowhere; the only recovery is A-120's escalation.
- Proposed fix (NOT applied): keep the two facts apart — *a chain exists* (learned from any arrival, cheap to replace) and *this path carried our traffic* (learned only from a record or ACK that arrived **direct**, and only that one is allowed to block shorter candidates). Concretely: set `peer_path_proven` only on the direct-receive path (`mesh_serial.rs:2237-2275`), leave it false for the flooded branch at `:2188-2204`, and let a route-request candidate that arrives with a fresher chain clear it.
- Open questions: whether a *direct* record arriving from the peer is enough proof, or whether proof needs the peer's acknowledgement of our packet (upstream uses an ACK, which we do not have) — with `directWorks` on the phone (A-120) we already treat "a direct record arrived" as proof, so the two ends would then agree.
- Investigator: path round · 2026-11-11

### A-361 · paths: while the flood latch holds, the whole path apparatus is unreachable · major

- Evidence: `crates/daemon/src/mesh_serial.rs:1413-1417` (`want_flood = peer_path.is_none() || path_stale || direct_silent || answer_flood_with_flood || peer_via_flood`), `:1285`/`:2208`/`:2278` (`peer_via_flood` set by any flooded record, cleared only by a direct one); `packages/mobile/.../AndroidRadioLink.kt:1013` (`floodWanted = (!directWorks || directSilent) && floodTokens > 0`), `:531` (`directWorks = true` only on a direct arrival); ledger **A-120** (the two flags latch).
- Finding: `prefer_route`, `route_stale`, `peer_path_proven` and the advert cache all exist to decide *which direct path to use*. But `want_flood` is true whenever `peer_via_flood` or `!directWorks`, and those flags are cleared **only by a direct record arriving** — which requires a direct packet to have been sent first. So on a link where everything arrives by flood, no direct packet is ever sent (except after the phone's flood budget runs dry or a host zero-hop attempt), and every path decision is dead code. This is the path-side reading of A-120; it is recorded separately because the fix differs: A-120's fix is the decay rule, this one's is the *probe* that makes a direct attempt happen on purpose.
- Why it hurts at range: the two carriers differ by 9.8 s versus 65.6 s per 172-byte record at 16 hops (A-340). A latch that keeps us on the expensive one costs ~6.7× on every record for as long as nothing forces a direct attempt — and at range nothing does, because a direct attempt is only made when `pathless_directs > 0` and the flood has already been chosen.
- Proposed fix (NOT applied): one deliberate direct attempt per N records (say 1 in 8) regardless of the latch, priced by `direct_round_trip(hops, airtime)`, and treat its arrival as the evidence that clears the latch. That is a probe, not a mirror: it is bounded, it is cheap (7.8 s at 16 hops for a 133 B record), and it is the only way both ends ever learn the cheap carrier works.
- Open questions: whether the probe should carry the same record (a duplicate the session would reject as replay) or a dedicated zero-payload control record; and whether the host or the phone should own the probing schedule so the two do not double.
- Investigator: path round · 2026-11-11

### A-362 · paths: the host calls a working path stale at 60 s, while one crossing at the margin takes 51 s and the round trip 102 s · major

- Evidence: `crates/daemon/src/mesh_serial.rs:166-169` (`route_stale(hops) = PEER_ROUTE_STALE.max(2 × 603 ms × hops)`, `PEER_ROUTE_STALE = 60 s` at `:156`); `packages/mobile/.../AndroidRadioLink.kt:1971` (`ROUTE_FRESH_MS = 120_000L`, flat, no hop term); ledger **A-40**/**A-41** (airtime 133 B = 490.0 ms; repeater hold 2.70 s/hop at the margin, 0.66 s at home); `mesh-research/clones/filter-firmware/src/Dispatcher.cpp:55-57` (the hold formula).
- Finding: the staleness window is priced from **airtime only** — 2 airtimes per hop, 19.3 s at 16 hops, so the 60 s floor wins and the hop term never matters above 6 hops. The mesh's own hold (`calcRxDelay`, which every repeater applies before re-transmitting a flood) is missing from the price, so at the margin the true one-way crossing of a 133 B record over 16 hops is 16 × (0.490 + 2.70) = **51.0 s**, round trip **102 s**, against a 60 s window: the host declares the path stale at **59 % of one round trip**. The two ends also disagree by construction: the phone keeps a route fresh for a flat 120 s with no hop term, the host for 60 s.
- Why it hurts at range: a path that is actively carrying is abandoned mid-exchange, the next record rides a flood (65.6 s instead of 9.8 s), and the phone's `routeFresh()` and the host's `path_stale` disagree about whether a route exists at all — which feeds the A-24 ladder branch ("no route, ask instead of handshake") on one end while the other end still believes it has one.
- Proposed fix (NOT applied): price the window from the same quantity the timers use — one round trip over the chain, including the firmware's hold (`2 × hops × (airtime + hold(score, airtime))`), with a floor that is a *policy* for "a quiet route is allowed to be idle" rather than a number that pre-empts the physics. Then the host and the phone compute the same figure from the same inputs instead of 60 s versus 120 s flat.
- Open questions: whether upstream's answer — no ageing at all (`mesh_serial.rs`'s analogue does not exist; `MyMesh::onSendTimeout() {}` is empty at `filter-firmware/examples/companion_radio/MyMesh.cpp:898`, and the only assignments to `OUT_PATH_UNKNOWN` are `CMD_RESET_PATH` at `:1318-1323` and a temporary path-discovery swap at `:1665-1672`) — is simply right, i.e. drop the timer and let a failed direct send be the only evidence.
- Investigator: path round · 2026-11-11

### A-363 · paths: there is no repair procedure, while upstream repairs in two packets · major

- Evidence: `mesh-research/clones/filter-firmware/examples/companion_radio/MyMesh.cpp:1318-1323` (`CMD_RESET_PATH` → `out_path_len = OUT_PATH_UNKNOWN`), `filter-firmware/src/helpers/BaseChatMesh.cpp:486-492` (a send with an unknown path **floods**, and only that send), `filter-firmware/src/Mesh.cpp:170-178` (the peer's reply to that flood is a `PAYLOAD_TYPE_PATH` sent **direct**, with a **500 ms** delay, carrying the chain the flood crossed), `BaseChatMesh.cpp:359-363` (on receipt it replaces the stored path); `mesh_serial.rs:1388-1425` (our only repair: escalate to flood and stay — see A-120/A-361).
- Finding: upstream has an explicit, cheap loop for a path that has died: reset → the next send floods once → the responder returns the path **direct** → both ends hold a fresh chain → the following send is direct. Two transmissions, and the repair is *paid for* rather than replacing the carrier. We have no equivalent: our path is never explicitly reset, our route reply is a flooded `LDMR` record rather than MeshCore's `PAYLOAD_TYPE_PATH` return, and nothing carries a path back to the peer (see A-364), so a dead path degrades into permanent flooding instead of a two-packet repair.
- Why it hurts at range: the repair costs one 48 B flood request (16 × (0.244 + 1.26) = **24.1 s** one way) plus one direct 40 B return (16 × 0.23 = **3.7 s**), about **28 s and two transmissions** — after which every record costs 7.8 s instead of 51.0 s. Permanent flooding instead pays 43 s extra on every record for the rest of the session, and A-120 says it never leaves that state on its own.
- Proposed fix (NOT applied): implement the loop explicitly, in three small pieces — (1) a *path reset* that marks the path suspect rather than absent (so the session is not torn down), (2) one flooded send *dedicated to re-learning* (the existing route request already does this: adopt the reversed chain of the reply), and (3) on receiving that reply, send a reciprocal path **direct** (500 ms, as upstream) so the peer gets a chain too. The 500 ms is already in `reply_delay(airtime_ms)`.
- Open questions: whether the reciprocal should be a MeshCore `PAYLOAD_TYPE_PATH` packet (which the peer's firmware would consume natively, no record, no session) rather than an `LDMR` wrapper — that would make the repair work against any MeshCore node, not only ldmux.
- Investigator: path round · 2026-11-11

### A-364 · paths: neither end exchanges a path; both infer one by reversing a flood chain, so discovery is bounded by two flood limits · major

- Evidence: `crates/daemon/src/mesh_serial.rs:2392-2408` (`PeerPath::reversed` — the only constructor used on receipt, at `:2006-2010`, `:2188-2197`, `:580-584`); `:557-597` (`refresh_peer_route` reads the radio's **advert** cache and reverses that path too); `packages/mobile/.../AndroidRadioLink.kt:544-562` (`onContact` → `contactPath` from the firmware's stored out-path); `filter-firmware/src/Mesh.cpp:448-470` (`createPathReturn` exists precisely so that a path can be **told** to the other end); ledger **A-349** (adverts die at `flood.max.advert`, default 8) and **A-60** (`flood.max.unscoped` default 5).
- Finding: no ldmux message carries a path. The phone's path sources are (a) the firmware's contact table, which is filled by the firmware's own adverts and PATH returns, and (b) the chain of a flooded route reply, reversed; the host's are (a) the advert cache (reversed) and (b) the chain of any flooded record or request, reversed. Each of those is capped by a flood limit before it can ever reach us: adverts at `flood.max.advert` (8 by default, 3 in `mcrs/profiles/defaults.conf`), and unscoped `REQ` floods at `flood.max.unscoped` (5 by default) on a mesh that enforces it.
- Why it hurts at range: our link is measured at 14–17 hops. On a default or regionalised mesh there is no path source that can reach that far (A-60 is the same fact from the forwarding side), and on a permissive mesh the only source that survives is the `REQ` flood — so the entire path life-cycle rests on a payload type that operators are most likely to rate-limit or scope.
- Proposed fix (NOT applied): add the missing half of the exchange — on answering a route request, also send a path **direct** to the peer (upstream's reciprocal, `Mesh.cpp:174-178`), and accept a path told to us rather than only inferring one. Prefer MeshCore's own `PAYLOAD_TYPE_PATH` if the firmware will carry it, since then other MeshCore nodes can hold a path to us too.
- Open questions: whether `CMD_GET_ADVERT_PATH`/`CMD_GET_CONTACTS` on the phone's firmware already gives a path that a *PATH return from the host* would have had to give, making the phone side cheaper than the host side; and whether a `PAYLOAD_TYPE_PATH` addressed to a peer needs a shared secret we do not have with a non-ldmux node.
- Investigator: path round · 2026-11-11

### A-365 · paths: a flood from a peer that already has a path means our path *return* was lost — upstream repairs, we overwrite · major

- Evidence: `filter-firmware/src/helpers/BaseChatMesh.cpp:340-345` (`if (packet->isRouteFlood() && from.out_path_len != OUT_PATH_UNKNOWN) handleReturnPathRetry(...)` — "we have direct path, but other node is still sending flood response, so maybe they didn't receive reciprocal path properly"), `:384-388` (the same rule on the ACK path), `:390-396` (`handleReturnPathRetry` re-sends the return path **direct** after 3 s); `crates/daemon/src/mesh_serial.rs:2170-2206` (we treat the same event as a new chain, adopt it, and set `arrived_by_flood = true` / `proven = true`).
- Finding: the two designs read the same observation in opposite ways. Upstream: "the peer flooded although I gave it a path ⇒ my path return did not arrive ⇒ re-send it, directly, using the path I already hold." Ours: "the peer flooded ⇒ this flood's chain is the truth ⇒ replace my path with its reversal." Upstream's reading is the one that preserves the cheap carrier; ours discards a working path every time the peer cannot hear us — which is exactly the situation the peer is in when it floods.
- Why it hurts at range: the condition fires on *every* record at range, so our path is replaced on every arrival, and by A-360 it is then locked as "proven". The repair upstream performs is one direct 40 B packet (3.7 s at 16 hops); the cost of not performing it is the 43 s per record that flooding adds.
- Proposed fix (NOT applied): invert the branch. When a record arrives by flood **and** a path is already in hand, do not adopt the flood chain: re-send our path to the peer direct (once per `route_stale` window), and only adopt the flood chain when there is no path in hand or the stored one has demonstrably failed (a direct send that produced nothing across its priced round trip — `direct_silent`, which already exists).
- Open questions: how the peer learns that the re-sent path is *ours for it* without a PATH payload (A-364) — with our current messages the only way is a record on the direct carrier, which is the probe A-361 proposes, so the two fixes are the same mechanism seen from two ends.
- Investigator: path round · 2026-11-11

### A-366 · paths: the phone discards its path on every connect, so every reconnect starts pathless · major

- Evidence: `packages/mobile/.../AndroidRadioLink.kt:766-769` (`closing = false; contactPath = null; advertPath = null; radioPublicKey = null;`), reached from the connect path; `:1540-1548` (`routeIsFresh` requires `hostPath != null && routeHeardAt > 0`); `:189-190` (`routeHops` reads those two fields); ledger **A-200** (a route switch tears the link down and back up), **A-24** (the ladder spends two rungs in three asking for a route when none is fresh).
- Finding: a path the *firmware* still holds in its contact table is thrown away by the app on every connection attempt, and nothing re-reads it before the first record is sent (`onContact` fills it only when the firmware pushes a contact update, and `CMD_GET_CONTACT_BY_KEY` is available: `vendor/.../MeshcoreSession.java:303`). So immediately after any connect the phone has no path, `floodWanted` is true (`!directWorks`), and the ladder's blind branch — which skips the handshake to ask for a route (A-24) — is taken.
- Why it hurts at range: at range a connect happens on every app start, every route switch, and every recovery (A-200), which is exactly when the operator is watching. Each one therefore begins with the most expensive sequence available — request floods, then flooded handshake copies — while a perfectly good path sits in the radio's own contact table, one cheap command away.
- Proposed fix (NOT applied): read the stored contact path (`CMD_GET_CONTACT_BY_KEY`) *before* the first record after a connect, and treat the firmware's path as the starting candidate (unproven, so A-360's rules still apply to it); clear the in-memory copy only when the firmware says there is none (`0xFF`, which `onContact` already handles at `:547-552`).
- Open questions: whether the firmware's stored path survives a BLE disconnect at all (it is in the contact store, so it should), and whether reading it costs a command that competes with the handshake for the radio's attention.
- Investigator: path round · 2026-11-11

### A-367 · paths: a radio that never took the host's time leaves `path_stale` permanently true · minor

- Evidence: `crates/daemon/src/mesh_serial.rs:568-593` (`age = unix_now() - received_at`; `plausible = received_at + 86_400 > unix_now()`; when implausible `heard = None`, and the candidate is still adopted with `*peer_path_heard = heard`), `:1388-1390` (`path_stale = peer_path_heard.is_none_or(|t| t.elapsed() >= route_stale(hops))`), `:1413-1417` (`want_flood` includes `path_stale`); `packages/mobile/.../MeshcoreSession.java:349` (`CMD_SET_DEVICE_TIME` — the host sets the radio's clock at open, which is what makes the stamp plausible).
- Finding: an advert-cache answer whose timestamp looks implausible (a radio that rejected or ignored `CMD_SET_DEVICE_TIME`, or a fresh flash) is adopted with `peer_path_heard = None`, and `None` is read by the carrier rule as *infinitely stale*, not as *unknown*. One such answer therefore makes `want_flood` true for the rest of the connection, whatever else arrives, because nothing else ever writes `peer_path_heard` again except a fresh candidate.
- Why it hurts at range: this is a single, silent configuration fault (clock not set on the radio) that converts the whole link to the expensive carrier and looks like "the mesh is slow", with no line in the journal naming it. It is cheap to detect and cheap to be wrong about in the safe direction.
- Proposed fix (NOT applied): distinguish "never heard" from "heard, but the stamp is unusable" — in the second case use `Instant::now()` (the cache answering at all is evidence the advert arrived while this radio was powered) and log `cache answer: stamp unusable, treating as now` once per connect.
- Open questions: how many firmware builds accept `CMD_SET_DEVICE_TIME` (the vendored lib documents it at `MeshcoreSession.java:349`, so ours does); whether `received_at == 0` is the shape a never-set clock produces.
- Investigator: path round · 2026-11-11

### A-368 · paths: reversal assumes a symmetric mesh, and nothing detects the asymmetric case · question

- Evidence: `crates/daemon/src/mesh_serial.rs:2392-2408` (per-hop reversal), `:2418-2426` (the comment that a direct path is consumed hop by hop); `filter-firmware/src/Mesh.cpp:334-341` (`removeSelfFromPath` — direct consumes from index 0), `:343-352` (flood appends its own hash at the end); `mesh-research/clones/openhop_core/src/openhop_core/companion/base_send.py:262` and `:340` (the reference trusts the peer's `out_path_len` rather than deriving one).
- Finding: a reversed flood chain is a correct direct path only if the same repeaters, in the same order, carry traffic in the opposite direction. MeshCore's own nodes do not assume this: a path is *told* to the other end (A-364) and used as given. Where a link is asymmetric — a marginal receiver on one side of a repeater, a directional antenna, a repeater that has one neighbour at its noise floor — reversal yields a chain that works one way, which is exactly the failure the host logs as `flooded record … from a 1 hop chain` beside `hops=16 (adopted)`.
- Why it hurts at range: asymmetry is the norm at the edge of coverage; a reversed chain that works inbound and fails outbound produces a link that handshakes (inbound works) and then carries nothing (outbound does not), which is the reported symptom and is currently indistinguishable from "the mesh is slow".
- Proposed fix (NOT applied): none of its own — it is a question for the next round: whether the two chained facts (`inbound chain` from the flood, `outbound chain` from the peer's report) should be stored separately rather than one being derived from the other. If they should, A-364 is the prerequisite.
- Open questions: whether any field measurement can separate "asymmetric" from "lost"; whether a TRACE (`PAYLOAD_TYPE_TRACE`) to the peer would give an independent per-direction reading cheaply.
- Investigator: path round · 2026-11-11

### A-369 · paths: the burst-convergence guard is shorter than the interval between bursts · question

- Evidence: `crates/daemon/src/mesh_serial.rs:77` (`REQUEST_BURST_MS = 10 s`), `:2014-2033` (`previous_burst` decides whether the first copy of a burst may force a replacement); `packages/mobile/.../AndroidRadioLink.kt:1525` (`val step = 1L shl minOf(routeAsks, 2)`) with `:1990` (`ROUTE_REQUEST_FLOOR_MS = 5_000L`), so requests go every 5/10/20 s ±25 %; handshake rungs at 8/16/32 s (ledger A-24, A-28).
- Finding: a request burst is *allowed to force a path replacement* only when the previous burst is older than 10 s. Consecutive request bursts are 5 s apart at the floor (3.75–6.25 s with jitter), so a second burst can be counted as part of the first and cannot replace the path — while a handshake rung at 8 s is just inside the window too. Nobody has measured which side of 10 s the real inter-burst spacing falls on at range, where the phone's own send confirmations are delayed by the crossing.
- Why it hurts at range: if the window is wrong in one direction the path converges on the longest copy (measured 2026-11-10: one request seen at 1, 2, 4, 17, 19, 20, 26, 27, 28 hops); if wrong in the other, every fresh burst replaces a working path. Both have been observed; the window is the thing that decides between them, and it is a flat 10 s.
- Proposed fix (NOT applied): derive the window from the crossing — a burst cannot be over before one copy could have arrived (`hops × (airtime + hold)`), so the window should be `max(10 s, one crossing)`, and burst identity should come from the request's own nonce rather than from a timer, which the code already does for replies (`ANSWERED_NONCES`, `mesh_serial.rs:187-194`; the nonce is parsed into `RouteRequest` at `crates/companion/src/codec.rs:728-741`).
- Open questions: what the measured inter-copy and inter-burst spacing is at 14–17 hops.
- Investigator: path round · 2026-11-11

### A-370 · paths: `proven` is set by the one event that proves nothing, and never by the one that does · major

- Evidence: `crates/daemon/src/mesh_serial.rs:2204` (a **flooded** record sets `peer_path_proven = true`) against `:2266-2277` (a record arriving **direct** sets `peer_path_heard`, `pathless_directs = 0`, `peer_via_flood = false` — and never `peer_path_proven`); `:2232-2237` (the cache read runs immediately *before* that arrival is recorded, as `refresh_peer_route`), `:580-592` and `:425-434` (with `proven == false` a cache candidate replaces the path in hand whenever it is shorter).
- Finding: the flag that means "traffic crossed this path" is set only by a record that crossed a **flood** chain in the opposite direction (A-360), and is never set by the record that actually arrived on the direct path. The direct branch then lets `refresh_peer_route` propose an advert-derived candidate, which `prefer_route` accepts on `candidate.hops < current.hops` alone — so the moment a record proves the path works, a shorter guess heard in the other direction is allowed to displace it.
- Why it hurts at range: this is the "working link replaced by a ghost" the `prefer_route` comment itself warns about, and it fires on the good event. A ghost chain costs 7.8 s of air per 133 B record (A-40/A-340) and lands nowhere, so the session then dies on retries (A-80) while the journal shows a healthy-looking `direct` carrier and a fresh `peer_path_heard`.
- Proposed fix (NOT applied): set `peer_path_proven = true` on the direct-arrival branch (`:2266-2277`), and read the advert cache *after* the arrival has been recorded so an arrival outranks a guess — i.e. one line in the direct branch, which also makes A-360's rule safe to apply (proven then means what its name says, and the flood branch can stop setting it).
- Open questions: whether `refresh_peer_route` should be skipped entirely when a record has arrived within the last `direct_round_trip(hops, airtime)`, since that is the window in which the direct path is demonstrably alive.
- Investigator: path round · 2026-11-11

---

**Summary**

1. **A-360 is the blocker**: a flooded record marks the reversed chain *proven*, and `proven` then locks out the peer's own route-request chain for a full 60 s — so the end that is being asked for a route is the one that cannot accept it.
2. **A-361/A-365** are the same defect seen from the two ends: while the flood latch holds, no direct packet is ever sent, so nothing can prove the cheap carrier; and we read "the peer flooded although we hold a path" as a new truth instead of as a lost path return.
3. **A-362/A-363** are the two numbers that decide the design: the host calls a path stale at 60 s while one crossing at the margin is 51 s (round trip 102 s); upstream never ages a path at all and instead repairs it in two packets (~28 s) — after which each record costs 7.8 s instead of 51.0 s.
4. **A-364/A-366** say where paths come from today: only reversed flood chains, bounded by `flood.max.advert` (8) and `flood.max.unscoped` (5), and the phone throws its copy away on every connect — so at 14–17 hops on a default mesh there is no path source at all.
5. **A-367/A-368/A-369/A-370** are the quiet ones: a radio that never took the time makes `path_stale` permanently true; reversal assumes symmetry and nothing detects the asymmetric case; the burst window is a flat 10 s against bursts 5 s apart; and `proven` is set only by a flood while a record arriving direct — the one event that proves the path — sets nothing, so a shorter guess may displace the path that just worked (A-370 is a one-line fix and belongs with A-360).
### A-400 · device: a half-written frame poisons the radio's parser, and that state survives every reconnect · major (mechanism for the "needs a power cycle" wedge)

- Evidence: `mesh-research/clones/filter-firmware/src/helpers/ArduinoSerialInterface.cpp:39-70` — the firmware's receive state machine: `RECV_STATE_IDLE` waits for `<`, the **next two bytes are the length with no sanity bound**, and the `default:` state consumes exactly `_frame_len` bytes, storing only the first `MAX_FRAME_SIZE`. That state is a member of the interface object, not of a session, and there is no command that resets it. Our side: `crates/companion/src/link.rs:29-60` (`write_all_within` returns `TimedOut` with a **partial frame already written**), `crates/daemon/src/mesh.rs:685-745` (the loop ends on that error and `supervise` reopens), `crates/companion/src/framing.rs:47-56` (`encode` emits `<`, len_low, len_high, payload).
- Finding: if a write stops after the start byte, or after the start byte and one length byte, the firmware is left holding a **partial length**. The next connection's first frame then supplies the missing length byte from its own bytes — `0x3C` (the start byte) or a length byte — so `_frame_len` becomes roughly `0x3C` in the high position, about **15–17 kB**, and the radio silently eats that many bytes of subsequent commands before parsing anything again. At 176 bytes per frame that is ~90 commands; at our 600 ms resend and 4.9 s deadline it is minutes of "the radio answers nothing". `app_start` times out, `prepare` fails, the port reopens, and the poisoned state is still there.
- Why it hurts at range: this is a second, previously unnamed explanation for the failure attributed to DTR/RTS holding the chip in the bootloader — a power cycle clears the parser state just as well as it clears a bootloader hold, so the cure fitted both hypotheses and the diagnosis was never settled. It also explains why *restarting the daemon* sometimes helped and sometimes did not: a reopen inherits whatever the parser was left holding. At range there is no power switch, so this class of fault is unrecoverable without one.
- Proposed fix (NOT applied): never leave a partial frame — poll for writability first and write the whole frame in one syscall; on `TimedOut`, close the port, let the tty discard, and reopen **with a deliberate DTR/RTS reset** plus a short boot delay, so the firmware's parser and boot state both start clean. A firmware-side length bound (`MAX_FRAME_SIZE` as the ceiling, as our own reader does) is the real fix and belongs upstream.
- Open questions: whether the USB-Serial-JTAG path can truncate a write at a byte boundary (a short `write` is normal for a tty; a killed process mid-buffer is the certain case); how fast the firmware drains a 15 kB backlog (it is byte-at-a-time in `checkRecvFrame`, so this is the difference between seconds and minutes); and whether the `TIOCMBIC` clear the daemon now performs actually reboots this board, which decides whether the parser is really reset.
- Investigator: device round · 2026-11-11

### A-401 · device: our frame reader never resynchronises inside a body, so one spurious start byte swallows real frames · major

- Evidence: `crates/companion/src/framing.rs:96-130` — the `State::Body` arm pushes every byte until `self.body.len() == self.length` and never looks for `SOF_RADIO` again; `:24-27` (`MAX_SANE_LENGTH = 512` against the firmware's `MAX_PAYLOAD = 176`); the tests at `:172-196` cover an **implausible** length (`0xFF 0xFF`) but none covers a plausible one.
- Finding: a single byte that happens to be `0x3E` where no frame starts is taken as a start byte; the next two bytes become a length, and if that length is between 1 and 512 the reader consumes that many bytes as a body, **discarding every real frame inside them** — a send confirmation (`OK`/`SENT`), a received record, a `LOG_RX_DATA` signal reading. Console text shares this port (`filter-firmware/src/MeshCore.h:28` routes `MESH_DEBUG_PRINTLN` to `Serial.printf`; `examples/companion_radio/MyMesh.cpp:2078` writes `"========= CLI Rescue ========="` raw), so the stream is not guaranteed to be pure frames, and nothing detects the loss.
- Why it hurts at range: the frame most likely to be swallowed is the one saying "your record went on the air", which turns a successful send into a timeout and a retry — and every retry is airtime on a link that has none to spare. It also makes a busy mesh look like a dead radio.
- Proposed fix (NOT applied): resynchronise in `State::Body` — on a `SOF_RADIO` byte whose implied remaining length does not fit, restart the frame from it and count the abandoned one; and lower `MAX_SANE_LENGTH` to `MAX_PAYLOAD`, since a frame longer than 176 does not exist and a longer promise is by definition corruption. Add the test: a spurious start byte followed by two real frames, both must arrive.
- Open questions: whether a restart-on-`0x3E` inside a body can misfire on a payload that legitimately contains `0x3E` (any frame may), which decides between that design and holding a candidate until completion; and how reachable this is from console text alone, given the CLI replies that begin `"> %s"` travel *inside* a framed payload (`MyMesh.cpp:2123`) rather than raw.
- Investigator: device round · 2026-11-11

### A-402 · device: the reader counts the bytes it throws away and nobody ever reads the count · major (observability)

- Evidence: `crates/companion/src/framing.rs:71` (`pub skipped: u64`, with the note "a reader that is dropping data should be able to say so"), `:103` (`self.skipped += 1`); a search of `crates/daemon/` finds no reader of it; `crates/daemon/src/mesh_serial.rs:2524-2535` (`status()` publishes counters this process computes, and no frame-level one).
- Finding: every byte the reader discards — console text, a resynchronisation, the ESP32's ROM banner after a reboot — is counted and never surfaced: not in `mesh status`, not in the journal, not once a minute. The one number that would prove "this port is dropping data" is dead code.
- Why it hurts at range: this audit has spent a day separating protocol faults from device faults using journals alone, and the number that names the difference exists and is unread. On the host the journal is the only instrument; it cannot currently say "the radio's stream is dirty".
- Proposed fix (NOT applied): publish the delta in `mesh status` and log one line when it is non-zero over a window: `radio stream: N byte(s) discarded, no frame start`. A second counter for a frame abandoned mid-body needs A-401 first.
- Open questions: a fresh open always skips the boot banner, so the first second after open must be exempt or the line cries wolf on every restart — decide whether to reset the counter at `app_start` success.
- Investigator: device round · 2026-11-11

### A-403 · device: any byte from the port counts as "the radio is alive", so console text suppresses the resend that would have recovered a lost command · major

- Evidence: `crates/companion/src/session.rs:661-679` (`pump`: `if n > 0 { heard = true; self.reader.feed(…) }` — `heard` is set **before** a single frame has been produced); `:563` and `:646` (`if !heard_anything && last_send.elapsed() >= RESEND_INTERVAL …`); `:29` (`RESEND_INTERVAL = 600 ms`).
- Finding: liveness is inferred from *bytes*, not from frames. A radio emitting console text makes every request believe it has been heard from, so the resend never happens and a command that was actually lost is never repeated. The mirror case is worse: a radio that has just rebooted emits a banner, which reads as perfect health for the length of the banner.
- Why it hurts at range: the resend is the only retransmission on the serial hop, and it is disabled by noise. One lost `SEND_RAW_PACKET` then costs the full command deadline plus a daemon-side retry round for a record that was fine — only its confirmation was lost.
- Proposed fix (NOT applied): key the resend on frames rather than bytes (`heard_anything = !self.pending.is_empty()` after a pump), and keep console bytes as a separate signal (A-402) so a jabbering radio is a named fault rather than health.
- Open questions: whether any firmware version answers a command with console text and no frame, which would require a compatibility check before tightening.
- Investigator: device round · 2026-11-11

### A-404 · device: a mid-session radio reboot is invisible, and only part of what it destroys is ever restored · major

- Evidence: `crates/daemon/src/mesh_serial.rs:1700-1706` — `APP_START` **is** re-sent every `PARAM_WATCH = 60 s` (`:67`) to re-read the radio's settings, so the app target and the firmware's version latch (`filter-firmware/examples/companion_radio/MyMesh.cpp:443`, `:561`) do resume within a minute; `crates/daemon/src/mesh_serial.rs:1077-1079` — `set_time` is sent **only** in `prepare()`, that is, only on a reopen; `crates/companion/src/framing.rs:71`/`:103` — the boot banner is skipped into a counter nobody reads (A-402); `crates/companion/src/link.rs:181-205` — a reboot over USB-Serial-JTAG does not close the tty, so no error is raised.
- Finding: nothing in the running session notices that the radio restarted. The app target self-heals within a minute by accident (the parameter watch), the **clock never does**, and the parser position (A-400) never does. So a reboot leaves a partially-restored radio that looks healthy and behaves subtly wrong.
- Why it hurts at range: a reboot is exactly what a brownout, a thermal trip or a marginal supply produces at a remote site, and the resulting symptom is "it stopped hearing the mesh" while sends still work — indistinguishable from a mesh that stopped forwarding. The one signal that would name it (the banner) is discarded, and the one thing that must be restored (the clock, A-405) is never re-sent.
- Proposed fix (NOT applied): treat skipped console text *after* the port is established as a reboot, and on it re-run the identify half of `prepare()` in place — `app_start`, `set_time`, `device_query` — without closing the port. That is a few lines and makes a brownout self-healing.
- Open questions: whether this board's USB-Serial-JTAG really keeps the tty across a firmware reset (the ROM re-enumerating would make the whole path moot because the port would reopen); and whether the firmware loses its scope preference on reboot, which would silently change which flood region our traffic uses.
- Investigator: device round · 2026-11-11

### A-405 · device: the radio's clock is set once at open, and the advert ages that route decisions rest on depend on it · major (cross-reference: A-340, A-80)

- Evidence: `crates/daemon/src/mesh_serial.rs:1077-1079` — `let _ = companion.set_time(…)` with the comment that a companion "starts its clock at zero at boot, so `CMD_GET_ADVERT_PATH` would report route ages against the epoch"; `crates/companion/src/codec.rs:114-121` (`get_advert_path` returns `received_at`); `crates/daemon/src/mesh_serial.rs:67`/`:1700-1706` (the periodic watch re-reads settings but **not** the time); ledger A-80/A-340 (route freshness decides both the flood carrier and the retransmission timer).
- Finding: "is this path still true" is answered with a timestamp the radio produced, corrected exactly once at open, best-effort, and silently (`let _ =`). A reboot resets the radio's clock to boot-zero (A-404), a long uptime lets it drift, and a failed `set_time` is indistinguishable from a successful one.
- Why it hurts at range: route freshness is what decides flood-versus-direct — the 20× carrier decision in A-340 — and what the retransmission timer is told (A-80). A wrong radio clock makes every advert look ancient (so we flood) or brand new (so we trust a dead path). That is a correctness dependency on a device clock that nothing checks.
- Proposed fix (NOT applied): compare the radio's reported time against ours on the existing one-minute watch and re-issue `set_time` when they disagree by more than a route's own staleness window; log the skew once per correction. Do not let `let _ =` hide a failure that invalidates routing.
- Open questions: whether the firmware's clock is RTC-backed across a reset (if it is, only drift applies); its measured drift over a day; and whether `CMD_GET_ADVERT_PATH`'s `received_at` is a timestamp at all or a per-entry counter — the vendored library and `MyMesh.cpp` do not obviously agree.
- Investigator: device round · 2026-11-11

### A-406 · device: the radio will not emit a published-category frame it cannot fit, and our largest packets sit exactly on that edge · minor

- Evidence: `mesh-research/clones/filter-firmware/examples/companion_radio/MyMesh.cpp:291-301` — `logRxRaw` writes the `PUSH_CODE_LOG_RX_DATA` frame only `if (_serial->isConnected() && len + 3 <= MAX_FRAME_SIZE)`, i.e. only for received packets of **173 bytes or fewer**; `crates/companion/src/framing.rs:20` (`MAX_PAYLOAD = 176`); `crates/companion/src/codec.rs:167` (`PACKET_MAX = 174`); `crates/daemon/src/mesh_serial.rs:2209-2210`, `:2280-2281` (SNR/RSSI are taken from exactly these `0x88` frames).
- Finding: the signal reading for a received packet is a push the firmware silently omits when the packet does not fit with the push's three bytes. Our pathless direct maximum is 173 bytes (header + path byte + 171), which fits with **zero bytes to spare**; one byte more of payload and the host sees no SNR/RSSI for that packet at all — not an error, just silence.
- Why it hurts at range: the whole margin analysis (A-40) is built from these readings, and the packets most likely to be near the edge are the largest records, which are also the ones most likely to fail. A missing reading after a failure is exactly when it would have been worth having.
- Proposed fix (NOT applied): nothing to change in the radio; on our side, treat "a record arrived but no `0x88` followed it" as a *reading gap* rather than as zero signal, and keep the record limit at 171 so the headroom cannot be spent by accident. Say so in the comment on `RECORD_MAX`.
- Open questions: whether `len` in `logRxRaw` is the raw packet length including the header (it is used for a `memcpy` of the raw packet, so yes), which makes 173 the exact ceiling; and whether any other push has a similar silent-drop guard worth costing.
- Investigator: device round · 2026-11-11

### A-407 · device: the send confirmation is the only proof a command reached the radio, and a swallowed frame is indistinguishable from a busy mesh · major

- Evidence: `crates/companion/src/session.rs:499-570` (`request_any`: the outcomes are a matching frame, `DISABLED`, `ERROR`, `Unexpected`, or `Timeout`); `:69` (`"no reply to 0x{expected:02x} within {:.1}s"` — one sentence shared by a device fault and a mesh fault); `crates/companion/src/codec.rs:18`, `:24` (`OK`, `SENT`); journal 2026-11-10 (`no reply to 0x05` recurring through the wedge periods); ledger A-220 (the session's death reason is discarded) and A-180 (the watchdog cannot fire).
- Finding: `Timeout` means "no matching frame arrived", and that one value covers at least four faults: the frame was never fully written (A-400), it was written and swallowed (A-401), the radio is busy on the air and will answer later, and the radio is wedged. The message names the command and the wait, and nothing about which.
- Why it hurts at range: every investigation so far has had to guess, and the guess was wrong twice in one day — the bootloader hold first, then the parser state. At range a wrong guess costs an hour of walking.
- Proposed fix (NOT applied): compose the timeout sentence from what the process already knows — the write outcome, how many frames completed during the wait, and how many bytes were discarded — e.g. `no reply to 0x05 within 4.9s (13 bytes discarded, 0 frames; the radio answered nothing)` versus `(1 frame of another kind arrived)`. Three facts, no protocol change.
- Open questions: whether the firmware ever answers `ERROR` when listen-before-talk expires rather than staying silent, which would separate "busy" from "dead" without inference.
- Investigator: device round · 2026-11-11

### A-408 · device: what the radio keeps that we do not reset, in one list · question

- Evidence: `mesh-research/clones/filter-firmware/src/helpers/ArduinoSerialInterface.cpp:39-70` (parser position, not resettable from the host); `examples/companion_radio/MyMesh.cpp:443`, `:561` (the app-target version latched from our last `APP_START`); `MyMesh.cpp:2076-2079` (`_cli_rescue` set by a serial condition and never cleared short of a reboot); `crates/daemon/src/mesh_serial.rs:1063-1078` (`prepare()` re-establishes app target, clock and scope, **only** on a reopen) and `:1700-1706` (the app target is also refreshed every 60 s by the parameter watch; the clock is not).
- Finding: the radio holds at least four things across our disconnects — the frame parser's position (A-400), the app-target version that decides which frame versions it emits, the CLI-rescue latch, and its clock — and our code re-establishes exactly the ones it happens to send at open, one of which is also sent every minute and one of which never is.
- Why it hurts at range: a reconnect is the host's only recovery mechanism, and it is *partial*: it papers over what it re-sends and silently inherits the rest. That is why "restart the daemon" sometimes worked and sometimes did not, which is the most confusing thing in this project's field history.
- Proposed fix (NOT applied): make the reopen honest — a deliberate line-level reset (DTR/RTS sequence, then a boot delay) so the firmware starts clean, followed by verifying `app_start` and `device_query` before publishing `radio_state: connected`. That turns "sometimes helps" into a defined recovery.
- Open questions: which of the four survive a line reset (the ROM-side and parser ones should not, which would make a reset a real cure — worth one bench confirmation); whether `_cli_rescue` is reachable from the companion port at all or only from the CLI UART; and whether a reset momentarily removes the tty, which the supervisor would see as a hang-up and treat as a plain reconnect.
- Investigator: device round · 2026-11-11

## Round: a day in the life of a range user

### The timeline, reconstructed from the field logs

Reconstructed from `journalctl --user -u ldmux` for 2026-11-10 and the phone's own durable log lines
(quoted in the session record and in the entries below). Times are real; costs are derived from the
Register's number table and the firmware's forwarding rules.

| time | the person | the code | cost at 14 hops | what they see |
|---|---|---|---|---|
| 18:26:22 | opens the app | BLE connect, contacts dump, settle, `DeviceInfo`, `refreshRoute`, `msg1` | 5–8 s bring-up (≤30 s bonded, ≤75 s unbonded) | "starting" |
| 18:26:22.9 | — | tailnet WebSocket answers first | 0.7 s | shell list, "attached" |
| 18:26:28 | taps a shell | `Attach` → `Profile` → `Rows`+`Commit` | ≈14 s direct, ≈92 s flooded | "asking host for the shell" |
| 18:38–18:46 | stands still, waits | ladder rungs at 8 s + 2 s/hop | rung 36 s, budget 372 s | "handshaking · 19 s", then the static verdict |
| 18:42:26 | switches to look at the phone | tailnet reconnects, attaches | 0.4 s | a live shell — over the tailnet, drawn as a radio link |
| 19:07–19:09 | walks out | requests adopted at 21 and 30 hops; `ignored for routing: the peer is pinned` | airtime, no session | "handshaking" |
| 20:45:31 | sits down somewhere new | radio session up at **17 hops** | — | — |
| 20:45:32 | — | tailnet WebSocket opens; the attach completes in **6 s** | 0.4 s, not 92 s | "attached" — believed to be the radio |
| 22:13:13 | comes home | advert at 0 hops, −32 dBm | — | — |
| 22:13:26 | — | `session replaced after 5443s (… replayed or out-of-order timestamp)` | — | "handshaking" |
| 22:18–22:22 | walks out again, then switches back twice | `handshake 1/3`, `2/3`, then `0/3` after a rebuild; 3 copies of 133 B per rung; `no path (broadcast, one hop)` | ≈1.5 s own air + ≈20 s of repeater air per rung | "handshaking", then the static verdict for the remaining 85 % |

### What each step costs, at 14 hops, with a healthy direct path and no loss

- **One crossing**: `hops × airtime` = 14 × 490 ms ≈ **6.9 s** (a direct packet is processed
  immediately at each repeater — `filter-firmware/src/Dispatcher.cpp:249-252`).
- **A flooded crossing**: 14 × (2.77 s hold + 0.49 s air) ≈ **46 s** one way
  (`Dispatcher.cpp:55-57`, `:243-248`).
- Cold start to a bound radio: **5–8 s**.
- Handshake: **≈14 s** (one lost copy: +36 s; a full ladder: 372 s).
- Shell list: **≈15 s**. Attach + profile: **≈14 s**. First full screen (3 framed records): **≈8 s**.
- **First usable prompt: ≈56–81 s** direct; **≈5–8 minutes** over floods.
- One keystroke round trip: **≈14 s** direct, **≈92 s** flooded.
- One shell switch: **≈36 s** direct. One drop-and-return: **≈40 s** direct.

### The three steps that remain unacceptable after every fix in the Register

1. **Typing.** A 14 s round trip per keystroke is the medium, not a bug: character-at-a-time
   interaction cannot work, and no item in the Register changes it. See A-426.
2. **The count of round trips per shell open.** Four sequential exchanges are the floor of the
   current shape (handshake, list, attach/profile, screen). The Register lowers each one's *risk*;
   none lowers the *count*. See A-426.
3. **Recovering from a drop.** At range a drop is routine, and the code that promises to give the
   person their place back is inert: every rebuild lands on the launcher. See A-420.

### A-420 · journey: the resume selector is never populated, so every drop lands on the launcher · blocker

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:256`
  (`var resumeShell by remember(plan) { mutableStateOf("") }`), `:263` (passed to
  `MeshRadioConnection(link, plan.creds, plan.lib, resumeShell, …)`), `:342` (`resumeShell = ""` before
  `attempt += 1` on every rebuild), `:914` and `:974` (the other two writes, both clearing),
  `:1017` (`holding = … && resumeShell.isNotEmpty()`), `:1129` (`key = resumeShell`);
  `Radio.kt:622-626` (the value becomes the connection's initial `selector`). `rg -n "resumeShell"
  MeshTerminalScreen.kt` returns seven lines and **not one of them assigns a shell id.**
- Finding: the "come back to the same shell" mechanism is inert. `resumeShell` is declared empty,
  cleared on every rebuild, clear on entering the list and on a closed shell, and never set from
  `attachedId` or the driver's live shell. So the connection is always built with an empty selector,
  `holding` can never be true, and the `key` is always "". The comments at `:250-256` ("a drop costs
  the user a pause, not their place") and `:378` describe behaviour that cannot occur, while `:338-344`
  does the opposite on purpose ("lands on the shell list… `resumeShell = ""`"). Two intents, one dead
  variable, and the dead one is the one the person was promised.
- Why it hurts at range: a drop is the normal event at range, not the exception (A-200, A-20, A-80).
  Each one now costs a list round trip, then an attach, then a profile, then a full screen: ≈40 s
  direct, several minutes over floods, and the person must find their shell again in a list that may
  itself be a merged run (A-100). The one recovery path the product advertises is the one that does
  not exist.
- Proposed fix (NOT applied): set `resumeShell` wherever a shell becomes the attached one — in the
  `onOpen`/attach success path and from `driver.attachedShell` when the view goes `Live` — and delete
  whichever of the two contradictory intentions is wrong at `:342`. If landing on the list is the
  decision, say so and remove the dead state, the `holding` computation and the `key`.
- Open questions: whether the tailnet screen has the working equivalent (the mesh one may have lost
  the assignment in the ADR-0025 port); whether resuming should also restore the scroll position
  (A-328 says rotation discards it).
- Investigator: journey round · 2026-11-11

### A-421 · journey: the host adopts a cached path with no age bound, so it times a 14-hop link as one hop · blocker

- Evidence: `crates/daemon/src/mesh_serial.rs:570` (`let age = unix_now().saturating_sub(u64::from(path.received_at))`),
  `:574-578` (`plausible` = the stamp is within **24 h**; `heard = Instant::now() - age`),
  `:579-584` (`prefer_route(...)` then `*peer_path = Some(candidate)`), `:595` (`route_log("cache answer hops={} age={age}s")`).
  Journal 2026-11-10: `22:21:40 route cache answer hops=0 age=506s`, `22:23:40 … age=626s`,
  `22:20:44 route request … hops=16 (adopted)`, `22:21:20 … hops=1 (adopted)`, `22:21:42 … hops=1 (adopted)`.
- Finding: the host's route for the phone comes from the radio's advert-path cache, and the only age
  test is "implausible" (>24 h). An entry heard **8.4 minutes** earlier, at **zero hops**, is adopted
  as the peer's path. At 22:21:40 the phone was 16 hops away and the host took a 0-hop path for it.
  That path then sets the session's hop count (`:1877-1881`, ledger A-80) and therefore the
  retransmission timer, and it is the path direct records are sent down.
- Why it hurts at range: this manufactures exactly the state the Register blames A-80 and A-340 for
  — a link that is 14–17 hops being timed and addressed as a neighbour — and it does so with no
  packet loss involved. The advert that would have corrected it cannot arrive: adverts die past
  `flood.max.advert` (A-60), so the cache holds the last one it heard, which is the one from home.
  A range user therefore gets a *shorter and shorter* path the longer they are away.
- Proposed fix (NOT applied): bound the age of a cache answer by what the route is used for — adopt
  `hops` from a cached entry only while it is inside `route_stale(hops)` (the same bound used
  everywhere else), otherwise keep the entry as a *fallback* for a direct attempt but never let it
  set the session's hop count. The timer must come from traffic (A-80), never from the cache.
- Open questions: whether `PeerPath::reversed` on a 0-hop entry is even a usable path (it is the
  broadcast case); whether the host should refuse to answer a route request from a cache entry older
  than the phone's own advert interval (60 s).
- Investigator: journey round · 2026-11-11

### A-422 · journey: the two counters the screen promises never reset are reset by every rebuild · major

- Evidence: `MeshTerminalScreen.kt:185-204` (the ladder "keeps climbing and it never stops", give-up
  after `LINK_GIVE_UP_MS`), `:206` (`LINK_GIVE_UP_MS = 10 * 60_000`), `:326-344` (on the cap:
  `showList = true; resumeShell = ""; attempt += 1`), `:438` (`attemptNo by conn.driver.attempt`),
  `:1474` (`WaitingLine(waiting, attemptNo) { conn.driver.linkEffort() }`);
  `MeshTermDriver.kt:629` (`attemptsThisLink = 0` inside `start()` — a new driver means a new
  connection, `MeshTerminalScreen.kt:263`), `:418` (`linkEffort()` reads those fields), `:2536`
  (`linkEffortLine`). Phone log 2026-11-10: `22:18:33.995 mesh handshake 1/3`,
  `22:18:55.389 mesh handshake 2/3`, then `22:22:13.475 mesh handshake 0/3` — the handshake number
  and the rung count both went back to their first value after a rebuild.
- Finding: the screen shows two numbers: the age of the current attempt ("seconds") and the effort
  that "does not reset" (`tries · route requests, (no) answer from the mesh`). The effort lives in the
  driver, and a rebuild replaces the driver, so the effort resets exactly when the link has been down
  long enough for the person to be watching it. The field log shows the handshake counter going from
  2/3 to 0/3 in the same window in which the person reported "it counts and then goes back to zero".
  The one number designed to be trustworthy is the one that is not.
- Why it hurts at range: the person's only evidence of progress is these counters, and at range the
  rebuild ladder (2 s doubling to 30 s, full jitter — `:315-330`) fires constantly. The screen
  therefore restarts its story every few seconds while the mesh has, in fact, been answering and not
  answering in a stable pattern. That is the difference between "the host is not there" and "the host
  is there and slow", which is the only question worth answering at range.
- Proposed fix (NOT applied): hold the effort above the connection — hoist `attemptsThisLink`,
  `blindAsks` and `routeAnsweredThisLink` into a `remember(plan)` holder passed into each new driver,
  cleared only when the operator asks (`attach`) — and print the rebuild count (`rebuilds`) beside it
  so a restart of the story is visibly a restart.
- Open questions: whether the seconds-and-effort line should survive a *route* change (A-200) as well,
  or reset because the medium changed.
- Investigator: journey round · 2026-11-11

### A-423 · journey: nothing on either end ever says which transport carried a step · major

- Evidence: host journal 2026-11-10 `20:45:32 remote: connection from 100.120.129.96:40718 (permessage-deflate)`
  and `20:45:35 … :40730`, while the radio session was up at 17 hops (`route flooded record 40 B from a
  14 hop chain`, `20:45:41`); phone log `20:45:31.718 sessions parsed=13 attachable=13`,
  `20:45:34.877 page 212d480f32: connected, attaching (take over)`, `20:45:35.354 page …: attached`;
  `MeshTermDriver.kt:712` (the only line naming a carrier, and it names it for the *handshake*);
  `AndroidRadioLink.kt` logs the carrier for flooded sends only (`mesh record by flood N/3`).
- Finding: an attach that completes in **6 s** cannot be the radio (a list answer alone is ≈15 s direct
  and minutes flooded), and no line on either end says "this went over the tailnet". The person
  concluded "it worked over LoRa"; the analyst reading both logs cannot tell either, because the
  transport names are only printed for *connects*, not for *steps*. There is no transport field in the
  view state, the notification, the header, or the durable log.
- Why it hurts at range: every range conclusion drawn from a session is unfalsifiable, and the two
  transports have costs three orders of magnitude apart. The Register's A-200 says switching routes
  tears the radio down; this is the mirror case — the tailnet silently taking over while the radio
  session stays up — and together they mean the person cannot know what they are testing.
- Proposed fix (NOT applied): one line when a connection is built, naming the chosen transport and why
  ("mesh transport: radio — no tailnet route" / "mesh transport: tailnet"), tagged onto every durable
  log line as a one-letter prefix, plus a glyph in the header. The decision is already made in one
  place (`meshScreenFor`); it only has to be said.
- Open questions: whether the app prefers the tailnet when both are available (the 20:45 evidence says
  it does, and that is the right choice, but nothing states it); whether a radio session should be torn
  down when the tailnet takes over (that is A-200's cost, seen from the other side).
- Investigator: journey round · 2026-11-11

### A-424 · journey: waiting out of range spends the mesh's airtime for ten minutes, and there is no way to stop it · major

- Evidence: `MeshTerminalScreen.kt:206` (`LINK_GIVE_UP_MS = 10 * 60_000`), `:315-330` (full jitter
  under `rebuildCapMs`, so a rebuild every 2–30 s), `:296-310` (only `NeedsRadio`/`NeedsPermission`
  stop the loop), `:926-928` (the notification's Disconnect is the only stop, and it is not on the
  terminal page). Phone log 2026-11-10, four minutes out of range: `22:18:29.158 mesh route request
  (unscoped) x3` · `22:18:34.103–35.159 3 × 133 B` · `22:18:51.178 x3` · `22:18:55.498–56.518 3 × 133 B`
  · `22:22:02.548 x3` · `22:22:13.589–14.609 3 × 133 B` · `22:22:21.641 mesh advert sent (flood)` ·
  `22:22:23.008 x3`.
- Finding: the default behaviour when out of range is a ten-minute ladder of rebuilds, each of which
  reconnects the radio, asks for a route and sends a handshake, and every one of those records is a
  flood with three copies. Four measured minutes cost the phone ≈7 s of its own transmit time and, by
  the Register's numbers (14 repeaters × 490 ms per 133 B copy × 3 copies ≈ 20.6 s per rung), roughly
  **80 s of aggregate repeater airtime** on a shared band — for a link that was never going to form.
- Why it hurts at range: the person's own device is the least appropriate thing on that band to spend
  it, and it spends it automatically, for ten minutes, invisibly, every time the phone is out of range
  with the app open. On a duty-cycled band this is also their own allowance (A-425's neighbour): 36 s
  an hour in the 868 sub-band.
- Proposed fix (NOT applied): make the ladder's cost visible and bounded — print the rebuild count and
  the airtime spent in the waiting line (the meter already exists, `MeshRouteSection.kt:396-403`),
  stretch the cap after the first two failures (a `rebuildCapMs` that follows the route, not a flat
  30 s), and put a "stop trying" control on the terminal page that is the same action as the
  notification's Disconnect.
- Open questions: whether a rebuild should re-send the handshake at all within the same minute when the
  previous rung produced no answer (the ladder inside the driver already covers that interval).
- Investigator: journey round · 2026-11-11

### A-425 · journey: the grid debounce is tuned to the keyboard animation, not to the link · major

- Evidence: `MeshTerminalScreen.kt:132-140` (`GRID_SETTLE_MS = 250`, "long enough to outlast the
  keyboard animation (≈200 ms on the devices measured)"), `:899-906` and `:1255-1260` (both paths
  debounce by it); phone log `20:45:34.718 grid 46x38 (asked 46 x 38)` and `20:45:35.423 grid 46x21
  (asked 46 x 21)` — **705 ms apart**, so both survive the 250 ms debounce.
- Finding: the debounce exists to coalesce the keyboard's animation, and 250 ms is right for a link
  where a record is 490 ms of air and an answer is 14 s away it is not: two grid changes 700 ms apart
  both go out, and each one costs a `Resize` record, a new `Profile` answer and a full screen. The
  measured pair is exactly the keyboard opening on the first attach — the moment the person is
  waiting for their first prompt.
- Why it hurts at range: the first interaction of the session carries ≈2 extra round trips (≈44 s
  direct, minutes flooded) for a whitespace change the person did not ask for, and the screen shows
  "asking host for the shell" throughout. It also spends the airtime twice for one layout.
- Proposed fix (NOT applied): debounce against the link, not the animation — hold a `Resize` until the
  answer to the in-flight `Attach`/`Resize` has arrived (the driver already knows: `pick`/`profile`
  pending), and coalesce to the last measured grid at that moment. A single settle of
  `answerDeadlineMs(GRID_SETTLE_MS, hops)` collapses the pair at 14 hops.
- Open questions: whether a rotate at range should defer the resize until the next keystroke instead
  (the pty can stay at the old width for one interaction).
- Investigator: journey round · 2026-11-11

### A-426 · journey: the floor is four sequential round trips, and no Register item reduces the count · major

- Evidence: `MeshTermDriver.kt:639` (`connect`), `:700-716` (hint, then `msg1`), `:1461-1475` and
  `:1428-1455` (a list request per launcher visit), `:1298-1320` (`setGrid` → new `Profile`),
  `Radio.kt:640-660` (attach selector → `Attach`), `:1055-1070` (first paint after `Commit`);
  Register A-102/A-108 (cost *within* one exchange), A-140 (compression), A-160 (echo), A-320/A-327
  (per-shell mirror). Numbers from the Register's table: a direct crossing at 14 hops ≈ 6.9 s, a
  flooded one ≈ 46 s.
- Finding: opening a shell is four sequential exchanges — handshake, list, attach+profile, first
  screen — and each one waits for the previous answer. With a healthy direct path that is ≈42 s; over
  floods it is ≈3 minutes; the measured minimum with bring-up is 56–81 s. Every Register item makes an
  exchange *cheaper or safer* (fewer transmissions, fewer floods, a working echo, a mirror), and none
  of them reduces the **number** of exchanges, which is what sets the floor.
- Why it hurts at range: this is the price of the first useful second, paid on every reconnect
  (A-200, A-420) and on every shell switch. A product whose happy path is a minute of waiting before
  the first character is not usable for the thing a terminal is for.
- Proposed fix (NOT applied): merge the exchanges, in three separable steps: (1) let `Attach` carry
  the grid and the previous selector so an attach answers with `Profile` **and** the first screen in
  one reply (the host already has both); (2) remember the last list per session on the phone and show
  it while one refresh runs in the background, so the launcher is not a round trip; (3) make the
  handshake's answer optionally carry the shell list, which removes the list exchange entirely on the
  common path.
- Open questions: whether (3) is a wire change that fits the `features` gate; how much of the list is
  worth carrying (8 pages of 806 B is not, but the *names* of five shells is).
- Investigator: journey round · 2026-11-11

### A-427 · journey: the one line the operator reads at the failing step says the opposite of what the phone does · minor

- Evidence: `MeshTermDriver.kt:707-712` — the per-attempt line ends
  `gateway.routeHops?.let { "$it hop(s)" } ?: "no path (broadcast, one hop)"`. Phone log
  2026-11-10: `22:22:13.475 mesh handshake 0/3 on no path (broadcast, one hop)` immediately followed by
  `22:22:13.589 mesh record by flood 1/3: Accepted · 133 B · dest 97` (and the same pair at
  `22:18:33.995` / `22:18:34.103`).
- Finding: with no route the record is not a one-hop broadcast — since the flood carrier (ADR-0037) it
  rides a flood, and the phone logs that two lines later. The comment above the line justifies the
  wording with the pre-flood world ("a direct packet … reaches one hop and stops"), and the host's log
  in the same window proves the flood crossed (`flooded record 121 B from a 6 hop chain`, `18:30:09`).
- Why it hurts at range: this is the line the operator and the analyst read at exactly the step that
  fails, and it names the wrong carrier. The person's own durable log therefore argues that the record
  could not go anywhere, when the record did go — and the reason for the failure (A-421, A-240) is
  somewhere else entirely.
- Proposed fix (NOT applied): say what happens — `no route: sending by flood (3 copies)` — and add the
  distance actually heard (`heard N hops`) to the same line, so the log states the medium instead of
  the intent.
- Open questions: whether the same line should say which carrier the *previous* attempt used when it is
  a re-ask.
- Investigator: journey round · 2026-11-11

### A-428 · journey: every rebuild pays for a contacts dump the terminal never uses · minor

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:811`
  (`withTimeoutOrNull(CONTACT_WAIT_MS) { contact.await() }`), `:815` (`withTimeoutOrNull(COMMAND_SETTLE_MS)
  { while (!s.isIdle()) delay(20) }`), `:818-822` (a still-busy session is closed and reported as "the
  radio did not finish its contact query"), `:825` (battery request after the settle), `:1948`
  (`CONTACT_WAIT_MS = 3_000`), `:2063` (`COMMAND_SETTLE_MS = 2_500`);
  `MeshTerminalScreen.kt:326-344` (a rebuild is a whole new connection).
- Finding: the link waits up to 5.5 s of serial round trips for a firmware contact dump before it will
  send the first record, on the stated reason that the raw send must not race the one other command
  the link issues. Nothing in the terminal path reads a contact; the host is reached by `RAW_CUSTOM`
  and by the route request. At range, where rebuilds are frequent (A-424), this is paid repeatedly and
  is indistinguishable from the link being slow.
- Why it hurts at range: it is the first 5.5 s of every recovery, on top of the handshake, and it is
  spent on traffic that does not carry a byte of the session. It also inflates the "starting" phase
  that A-280 leaves unexplained.
- Proposed fix (NOT applied): one question to answer first (below); the change itself is to skip the
  contacts query when `setAutoDrain(false)` is set and nothing in the profile consumes contacts, and
  rely on the session's own idle gate (`isIdle()`) before the first raw send.
- Open questions: does the firmware require a contacts query before it will accept `CMD_SEND_RAW_DATA`
  (some companion builds answer raw sends only after the queue has been drained once)? What fills
  `radioPublicKey`/path width today — the contacts dump or `SELF_INFO`? Until that is settled this
  entry stays a `minor` with a measurement attached.
- Investigator: journey round · 2026-11-11
### A-460 · measurement: the link's own measured round trip is computed on every ack and then thrown away · major

- Evidence: `crates/session/src/arq.rs:670-683` — on every advancing ack the ARQ takes a sample from the
  **first-attempt** record (`attempts == 1`) as `now_ms - sent_at` and smooths it 1/8 like TCP
  (`srtt = (srtt*7 + sample)/8`); the value is used internally at `:724-731` (`fast_retx_floor` =
  `max(airtime, srtt/2).max(200)`) and `:740` (`rto = rto_initial.max(srtt)`); `:475` exposes it as
  `smoothed_rtt_ms()`; the only readers in the tree are `crates/session/tests/arq.rs:546` and `:574`.
  It appears in **no** status field (`crates/daemon/src/mesh_session.rs:42-80` — `SessionSnapshot` and
  its `to_json` carry handshakes/sent/retransmits/bytes/pending and no time), in **no** log line
  (`mesh_session.rs` has three `eprintln!` sites, `:235`, `:264`, `:321`, none of them timing), and is
  **not reachable from the phone** (`crates/session-ffi/src/lib.rs` exposes status/pending/deadline
  only — `ldmux_session_status:657`, `ldmux_session_pending:680`, `ldmux_session_next_deadline:691` —
  and `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/NativeMeshSession.kt:39-40`
  binds exactly `status` and `pending`).
- Finding: the single honest end-to-end measurement of this link — a real round trip, taken from the
  air, including every repeater's hold, automatically preferring the sample that was not retransmitted —
  exists inside the ARQ, corrects its own timers with it, and is discarded at every boundary. The
  ledger prices the same quantity by hand in four places (A-41's 2.70 s per hop, A-340's 9.8 s versus
  65.6 s at 16 hops, A-80's 2.94 s RTO against a 43 s crossing, A-164/A-165's 13 s and ~64 s echo) and
  every one of those is an estimate that `srtt` would have replaced with a number.
- Why it hurts at range: the field record of 2026-11-10 was reconstructed from journal lines that say
  *what* happened and never *how long it took*. A single `srtt` value in the session line would have
  separated "the mesh is slow" from "our timer is wrong" in one reading; instead A-80 was found by
  reading `prefer_route` and redoing the airtime arithmetic. Every future range report will be as
  expensive as that until this number is visible.
- Proposed fix (NOT applied): carry it out through all three layers, smallest first.
  (1) `SessionSnapshot.srtt_ms: Option<u64>`, filled from `session.smoothed_rtt_ms()` in
  `SessionHost::snapshot`, printed by `print_mesh_session` in `crates/daemon/src/bin/ldmux.rs` as
  `  round trip   12.4 s measured (smoothed over n samples)` — and one journal line on the first
  sample of a session, because a journal is what survives.
  (2) `ldmux_session_rtt_ms(*mut LdmuxSession, *mut u64) -> i32` in the C ABI, plus a sample count so a
  caller can tell "12 s" from "12 s from one sample", and the matching `external` on
  `NativeMeshSession`.
  (3) The phone's durable log gets it on the session line (see A-463).
- Open questions: whether the sample should also be taken from acks that advance over a *retransmitted*
  record (deliberately excluded today by `attempts == 1`, which is right for the estimator and wrong
  for "how long did this exchange actually take"); and whether the same number should be published per
  shell, since a burst on one shell inflates the sample for all of them.
- Investigator: measurement round · 2026-11-11

### A-461 · measurement: the hop count that sets every timer changes with no record of the change · major

- Evidence: `crates/daemon/src/mesh_session.rs:177-182` — `set_hops` assigns `self.hops` and forwards it
  to the session, with no log and no history; it is called from `crates/daemon/src/mesh.rs:992` on
  `Event::Limit { record_max, hops }`, i.e. from whichever path the carrier last adopted. The only
  visible trace of a change is an unrelated route line (`mesh_serial.rs:2196`,
  `route from a flooded record hops=N`), which is rate-limited (A-466). On the phone the same quantity
  is set silently at `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTermDriver.kt:917`
  (`s.setHops(sessionHops)`) and on every `followRoute` (`:885-891`), and `sessionHopsFor`
  (`:2586`) derives it from `routeHops ?: meshReach`.
- Finding: the input that decides the first retransmission, the ARQ's per-record chunking, the flood
  interval and every wait the app sizes over the air is changed in place, silently, from at least three
  sources on each end. The journal therefore cannot answer "what was the timer told at 22:20:44" — which
  is exactly the question A-80 (a 1↔16 hop flip in 11 s, settling on 1) had to be answered by reading
  the code and matching it to a route line.
- Why it hurts at range: at range the path changes often — that is what a lossy multi-hop route does —
  so the hop count is not a constant that can be inferred once. A silently moving input to a timer is
  indistinguishable from a timer bug, and the investigation pays for that difference every time.
- Proposed fix (NOT applied): one line per change, on both ends, naming the value, the previous value
  and the source: `ldmuxd: session hops 16 (was 1) from the route` / phone
  `mesh hops 16 (was 1) from routeHops`. Gate it behind the existing `LDMUX_MESH_TRACE` only if the
  volume is a problem; at range it fires a handful of times per link, and it is the cheapest possible
  answer to "what did the timer know".
- Open questions: whether a *temporarily* longer hop count should also be logged when the session
  refuses it (`followRoute` skips while `pending > 0`, `:888`), since a refusal is itself a decision
  worth seeing.
- Investigator: measurement round · 2026-11-11

### A-462 · measurement: the margin of every arriving record is delivered with it, kept only as "the last one", and never as a distribution · major

- Evidence: the companion prefixes each received packet with SNR (quarter-dB) and RSSI, and the host
  reads them per record: `crates/daemon/src/mesh_serial.rs:2209-2211` and `:2280-2282` —
  `peer_signal = Some((snr, rssi))`, a **single** variable, overwritten by every arriving peer record.
  It is published only on the 20 s stats watch (`STATS_WATCH`, `:70`; publish at `:1781-1802`) into
  `MeshSnapshot.snr_quarter_db`/`radio_rssi`/`signal_from_peer` (`crates/daemon/src/mesh.rs:970-981`),
  and printed on demand as one reading (`crates/daemon/src/bin/ldmux.rs:343-372`). The phone does the
  same on its side, reporting a summary instead of a series (`AndroidRadioLink.kt:1582-1584`,
  `mesh heard N packet(s) in Ns · last flood … dB … dBm`).
- Finding: the measurement that decides whether this link can work at all — the SNR of a record that
  actually arrived, against SF7's −7.5 dB demodulation floor — is available for every packet and is
  retained as exactly one number: the most recent. There is no minimum, no median, no count below the
  threshold, and no per-record association, so the ledger's central physical claim (A-40: "the field
  link runs at about 0 dB margin") is an inference from scattered summary lines rather than a
  distribution anyone can re-derive.
- Why it hurts at range: margin is the one quantity that separates "our protocol is wrong" from "this
  distance is not reachable today", and the two need opposite responses — code versus antenna and
  height. With only the last reading, a link that spends 90 % of its time at −8 dB and one lucky packet
  at −4 dB looks healthy at the moment someone asks.
- Proposed fix (NOT applied): keep per-session extremes and a coarse histogram in the carrier —
  `snr_min`, `snr_max`, count of peer records, count below the tuple's SNR floor — carried in
  `MeshSnapshot` and printed in one line when a session ends or on demand:
  `signal   14 records, SNR −8.5…−4.0 dB (median −6.9), 9 of 14 below the SF7 floor`. The floor per SF is
  a four-entry table (`-7.5, -10, -12.5, -15` at SF7…SF10, from the firmware's own thresholds) and the
  tuple is already known from `SELF_INFO`.
- Open questions: whether the SNR byte on a *flooded* record describes the copy that reached us or the
  hop before us (the repeater rewrites the header; worth one bench reading before the number is trusted
  as "the margin of the link").
- Investigator: measurement round · 2026-11-11

### A-463 · measurement: the phone's durable log has no line at the two events that matter most · major

- Evidence: the mesh path's entire durable vocabulary is twelve `appLog` call sites —
  `MeshTermDriver.kt` (9: drop `:771`, rejected record `:977`, grid `:1287`, fit `:1319`, `:1334`,
  profile `:1942`, commit `:2207`, handshake `:700-716`, plus the stale-handshake note),
  `MeshTerminalScreen.kt:327` and `MeshTerminalSource.kt:422`, `:486`. The **session-established**
  path (`MeshTermDriver.kt:910-936`, where `session = s`, `sessionHops`, `handshakeTries = 0` and the
  view moves to `ProfileMissing`) contains no `appLog`. The attach request/answer path carries
  `attachAskedAt`/`attachAsks` (`:540-541`) and never logs either the ask or the elapsed time to the
  answer.
- Finding: the phone writes nothing when a session comes up, nothing when an attach is asked, nothing
  when it is answered, and nothing with a duration anywhere. The durable log is a list of *events*
  without *times*, so the field timeline of 2026-11-10 could only be reconstructed from the host
  journal — and the host cannot see the attach, the grid, the profile or the commit. Neither end
  alone can say how long the user waited.
- Why it hurts at range: the operator's report is always "it hung / it counted to 100 / it went back to
  0", and every one of those maps to a step whose duration this log does not record. Six lines at the
  right places would turn each report into a number, with no airtime cost — the log is local.
- Proposed fix (NOT applied): one line per step with elapsed milliseconds from the attempt's start, in
  the existing `appLog` style: `mesh session up · 41 s · 16 hop(s) · record 143 B`,
  `mesh list asked · try 2`, `mesh list ${n} shell(s) · 9 s`, `mesh attach ${id} · 7 s`,
  `mesh first screen · 13 s from tap`. Keep the counters the screen already computes
  (`attemptsThisLink`, `attachAsks`) so the numbers agree with the UI rather than being a second truth.
- Open questions: whether the elapsed values should also be surfaced in the route panel (the user can
  then see "last open 41 s" and stop wondering), which is a UX decision rather than a measurement one.
- Investigator: measurement round · 2026-11-11

### A-464 · measurement: the radio's own airtime counter is published but never sampled, so per-record airtime is a formula nobody checks · major

- Evidence: `crates/companion/src/codec.rs:1119-1122` — `STATS_RADIO` reports `tx_air_secs` and
  `rx_air_secs` since boot; `crates/daemon/src/mesh.rs:978-979` stores them; `crates/daemon/src/bin/ldmux.rs:374`
  prints them on demand (`radio  1234 s transmitted, 5678 s received since the radio booted`). Nothing
  samples them on a schedule: the 20 s stats watch (`mesh_serial.rs:1781-1802`) republishes the
  cumulative value, and no site computes a delta. Meanwhile every timer is priced from
  `airtime_ms`, which is **computed** from the tuple, not measured: `mesh_serial.rs:1110`
  (`self_info.time_on_air_ms(RECORD_MAX)`) → `crates/companion/src/codec.rs:1054-1084` (the RadioLib
  formula with a 32-symbol preamble below SF8).
- Finding: the only ground truth about what this radio actually spends on the air is available every
  20 s and is never differenced, while the figure the system uses to size every timer and to publish
  "airtime this hour" is arithmetic that no measurement has ever confirmed. The ledger's own note that
  "`tx_air_secs` is never read" is **imprecise** — it is read and printed on demand; it is *never
  sampled*, which is the defect that matters.
- Why it hurts at range: a formula error of the size the preamble fix corrected (≈50 ms per record at
  62.5 kHz, `codec.rs:1064-1068`) moves every timer and every budget by 8 %, and the correcting
  measurement was sitting in `STATS_RADIO` the whole time. At range, an 8 % error in the retransmission
  budget is an 8 % error in how often the mesh is retried.
- Proposed fix (NOT applied): on the existing stats watch, keep the previous `(tx_air_secs, records
  sent)` pair and emit one line per five minutes, or on session end:
  `airtime  14 records sent, 6.9 s transmitted by the radio, 0.49 s each (formula 0.47 s)`. That single
  line makes `time_on_air_ms` self-checking in the field and turns the ledger's per-record figures into
  measurements.
- Open questions: whether the firmware's counter is wall-clock airtime or rounded seconds (it is `u32`
  seconds, so a 14-record sample is coarse — the sampling window may need to be minutes, not seconds);
  and whether `rx_air_secs` counts the radio's own receive-window time (useful) or only decoded packets
  (not useful).
- Investigator: measurement round · 2026-11-11

### A-465 · measurement: the frame trace cannot measure anything — no timestamps, no transmit side, no identity · major

- Evidence: `crates/daemon/src/mesh_serial.rs:228-265` — `trace_rx` prints
  `trace rx route=… type=… hops=… w=… len=… snr=… rssi=…` per packet; it carries **no timestamp**, there
  is **no `trace_tx`** anywhere in the file, and the line names no record identity (no seq, no hash), so
  two copies of one record and two different records look alike. It is enabled by `LDMUX_MESH_TRACE`
  (`:224-226`) and, being `eprintln!`, is timestamped only by the journal's own arrival time.
- Finding: the one facility that could measure the questions the ledger is reduced to estimating — how
  long a copy takes to arrive, whether three copies are spaced or queued (A-29), whether a repeater holds
  for 2.7 s or 0.66 s (A-41), whether a flood's copies arrive at different times (A-382) — prints data
  that cannot answer any of them. What is missing is not a field but a key and a clock.
- Why it hurts at range: every airtime figure in the ledger is arithmetic precisely because this path was
  never measured. A trace that carried a monotonic millisecond prefix and a transmit counterpart would
  convert the whole "Numbers every later round should reuse" table into observations on the next trip.
- Proposed fix (NOT applied): in `trace_rx`/a new `trace_tx`, prefix with the monotonic millisecond
  (`{:>10} ms`) and add the record's first bytes or its sequence so copies correlate; `trace_tx` prints
  the same fields plus the carrier (`raw_custom`/`flood`) and the outcome. Keep it behind the same env
  var; it costs no airtime and writes only when asked.
- Open questions: whether the companion reports *transmit* SNR/RSSI as zeroed bytes (the trace would show
  it), and whether the firmware's own `SEND_RESULT` arrives before or after the packet leaves the air —
  which decides whether tx can be timestamped at the start or the end of transmission.
- Investigator: measurement round · 2026-11-11

### A-466 · measurement: rate-limited logging cannot be counted, and it is used for the events a measurement needs · major

- Evidence: `crates/daemon/src/mesh_serial.rs:217-222` — `route_log_rare` prints only if the *same* `last`
  slot is older than 60 s, then updates it. It guards the record-send line (`:1496`), the flooded-record
  and hop lines (`:2046`, `:2173`, `:2196`) and others. Each call site has its own `Option<Instant>`, so
  the limit is per site, not global.
- Finding: the lines a measurement would count — how many records went out, on which carrier, over how
  many hops — are emitted at most once per minute per site regardless of how many events occurred, and
  the suppression leaves no trace. Any number derived from the journal undercounts by construction, and
  the undercount is invisible: a link that sent 40 records in a minute writes one line and reads the same
  as a link that sent one.
- Why it hurts at range: at range the interesting quantity *is* the rate. The ledger's A-102/A-108
  conclusion ("an attach costs 18–27 transmissions") had to be derived from code and arithmetic because
  the journal cannot count transmissions at all; with this rule in place it never could.
- Proposed fix (NOT applied): keep the rate limit and put the count in the surviving line —
  `… (37 since 12:04, 12 suppressed)` — which needs one counter per slot and costs nothing else. That
  makes every rate-limited line a sample *and* a census, which is what an investigation needs from it.
- Open questions: whether the slots should key on a tuple (site, peer, carrier) rather than a bare
  `Option<Instant>`, so a change of carrier is never suppressed by a previous carrier's line.
- Investigator: measurement round · 2026-11-11

### A-467 · measurement: nothing counts how many transmissions one record cost, which is the unit every fix is priced in · major

- Evidence: `crates/session/src/arq.rs:331-341` — `Stats` counts `sent`, `retransmits`,
  `fast_retransmits`, `delivered_bytes`, `duplicates`, `out_of_window`, `bad_acks`, `budget_stalls`,
  all **aggregate**; the per-record state (`inflight` entries with `attempts`, `sent_at`, `refused`) is
  internal and dropped when the record is acknowledged. The daemon publishes the aggregates
  (`crates/daemon/src/mesh_session.rs:42-80`), and the phone sees none of them (A-460's ABI note).
- Finding: the cost of one unit of work — "this paste, this screen, this attach, in transmissions" — is
  not recorded anywhere, although the ARQ knows it exactly for every record it sends (`attempts` per
  in-flight record) and then forgets it on ack. Every proposed fix in the Register is priced by
  multiplying records by a formula; none can be checked after the fact.
- Why it hurts at range: airtime is the scarce resource and "transmissions per record" is its exchange
  rate. When A-80 makes a record retransmit 15–20 times, the journal shows one "sent" counter rising; it
  cannot show that a single 143-byte record consumed 20 transmissions, which is the fact that makes the
  bug expensive rather than merely wrong.
- Proposed fix (NOT applied): in `Session::poll`'s ack path, when an in-flight record is retired, fold
  `attempts` into a small histogram in `Stats` (`attempts_1`, `attempts_2_3`, `attempts_4_8`, `attempts_9p`)
  and keep `max_attempts` and `worst_seq`. That is four `u32`s, published in `SessionSnapshot` and
  printed as one line: `attempts  118 of 131 records first try, 9 took 2–3, 4 took 9+ (worst 19)`.
- Open questions: whether the histogram should be per shell (the bridge knows the shell, the ARQ does not
  carry it in `Outgoing`), which matters when one shell is the busy one.
- Investigator: measurement round · 2026-11-11

### A-468 · measurement: the phone already owns a measured-RTT adaptive policy that the radio path never feeds · major

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/FramePacing.kt:43`, `:81`,
  `:89-108` — a smoothed-RTT estimator with hysteresis (`PREDICT_ON_MS` / `PREDICT_OFF_MS`,
  `SLOW_ON_MS` / `SLOW_OFF_MS`), a repaint interval derived as `RTT/2`, and a predictive-echo gate; tested
  at `androidUnitTest/.../FramePacingTest.kt`. It is fed by the tailnet path only:
  `TerminalController.kt:64` (`private val pacing = FramePacing()`), `:207`
  (`attach.onRtt = { sample -> … pacing.onRttSample(sample) }`), with the sample measured by an
  application-level ping inside the same socket the output travels on
  (`AttachClient.kt:462-470`). Grepping `pacing|FramePacing|predict` under `.../mesh/` yields only
  `MeshRoute.kt:131`, a comment saying the UI should **not** carry radio knowledge.
- Finding: the project already built — and tested — exactly the machinery the radio path lacks: a measured
  round trip, hysteresis so it cannot flap, a cadence derived from it, and a predictive echo switched on
  *only* when the measurement says the link is slow. None of it is connected to the mesh transport, whose
  equivalent measurements exist independently (A-460's `srtt`, A-462's per-record SNR) and are discarded.
- Why it hurts at range: the radio path currently applies flat guesses where this class exists: the
  verdict fires at 30 s regardless of the link (A-280), echo is never predictive (A-160), and the repaint
  cadence has no relationship to the measured round trip. At 13–64 s per echo, "predictive echo when the
  measured RTT crosses a threshold" is not a nicety — it is the difference between a terminal and a
  screenshot viewer.
- Proposed fix (NOT applied): make the mesh driver feed the same `FramePacing` — sample from `srtt` on ack
  (A-460's ABI call) rather than from a ping, since the ARQ's ack *is* the ping and costs no airtime — and
  let the existing hysteresis decide predictive echo and cadence. Nothing new is designed; one existing,
  tested class is wired to the second transport.
- Open questions: whether `FramePacing`'s thresholds (tuned for a WebSocket/Tailscale RTT of tens to
  hundreds of ms) need a second calibration for a 13 000 ms link, or whether the clamps already compose —
  worth a unit test with a 13 s sample before trusting it.
- Investigator: measurement round · 2026-11-11

### A-469 · measurement: what can be settled on a bench, and what still needs a field trip · question

- Evidence: the ledger's number table (`docs/RADIO-AUDIT-LOOP.md`, "Numbers every later round should
  reuse") is entirely arithmetic from firmware formulas and code paths; the field readings that do exist
  are summaries (`mesh heard … −121 dBm`, `snr −4.75 dB`) with no series behind them (A-462). The
  mechanisms to change that are named in A-460…A-465; the hardware available is one host radio and one
  phone radio (`docs/MESH-RADIO-CONSIDERATIONS.md` §1, §2).
- Finding: the boundary between "bench" and "range" is sharper than it looks, and nothing in the ledger
  has drawn it. Setting the two apart decides which of the fifteen Register items can be verified before
  anyone leaves the house.
  - **Bench, one radio plus the host:** per-record airtime against `STATS_RADIO` deltas (A-464);
    `time_on_air_ms`'s preamble and low-data-rate terms; serial resync after a deliberately truncated
    write (A-400); whether `RESP_CODE_SENT`'s byte 1 carries the route used (A-384); how many copies of a
    flood record reach a receiver with no repeater in between (A-20's premise).
  - **Bench, two radios in range of each other (no repeaters):** direct versus flood timing at zero
    hops — which isolates the receiver's own decode and the firmware's own deferral from any repeater's
    hold (A-41); whether a flood's copies arrive spaced or queued (A-29); whether the SNR byte describes
    the last hop or the origin.
  - **Range only:** link margin at a given distance (A-40); how many repeaters a flood actually crosses
    here (A-60); whether the local repeaters apply a scope or an unscoped cap; the 2.70 s-per-hop hold,
    which depends on the city's `txdelay` and tier settings; and every user-facing duration in A-160,
    A-280, A-420.
  - **Neither, without code:** transmissions-per-record (A-467), `srtt` in production (A-460), and the
    83–85 % dead-screen window (A-280), which is a UI decision and can be settled by reading.
- Why it hurts at range: a field trip is the most expensive measurement instrument this project has, and
  the ledger's airtime and timing numbers are used to justify every proposed fix. Three of the four
  bench-able questions above would pin the constants that all of them rest on.
- Proposed fix (NOT applied): none — this entry exists so the next round does not spend a trip on
  something a bench answers, and so the Register's fixes can name which verification they need.
- Open questions: whether the phone's radio can be put in a receive-only, logging configuration while the
  host transmits (which would make the second-radio bench cheap), and whether a second host radio is
  available at all.
- Investigator: measurement round · 2026-11-11
# Adversarial audit of the audit — `docs/RADIO-AUDIT-LOOP.md`

Scope: the ledger against the code it cites. Each entry quotes the ledger claim, then the code or
arithmetic that refutes it. Entries whose evidence held up are not listed here; a short "verified
correct" list closes the file.

Severity column = severity **of the ledger error** (how much work it would cause if acted on as
written), not of the underlying defect.

---

### A-440 · ledger: A-360's central mechanism is refuted at the request call site · **major**

- `ledger:` A-360 (blocker) says: "`prefer_route` then refuses every non-`dead` candidate for a full
  `route_stale` window (60 s at 16 hops) — **including the peer's route request**, sent precisely
  because it cannot hear us. The end being asked for a route is the one that cannot accept it."
- **Refuted.** The request call site passes `dead = !previous_burst`
  (`crates/daemon/src/mesh_serial.rs:2013-2024`), and `prefer_route` returns `true` before it ever
  looks at `proven` (`:418-420`: `if dead || stale { return true }`; `proven` is only consulted at
  `:427`). The first request of a burst therefore always replaces the path; `REQUEST_BURST_MS`
  (`:77`, 10 s) is what stops the *later* copies of the same burst doing so, which is the intended
  convergence, not a lock-out.
- Corrected statement: `proven` can only block **non-request** candidates — a route reply
  (`:2246`, passes `false`), and the advert-cache refresh path. The "the end being asked for a route
  cannot accept it" conclusion does not follow, and the path round's headline ("`prefer_route` then
  refuses …") overstates a narrower guess-vs-proof rule.
- What survives from A-360: a flood-derived reversed chain is marked `proven`, which is a real
  overclaim (the inbound chain is not evidence about the outbound one), and it does suppress
  shorter *replies* and cache guesses for up to 60 s.

### A-441 · ledger: register row 1 (from A-340) would delete the one escalation the reference also performs · **major**

- `ledger:` Register row 1: "flood only when no path exists; treat stale as 're-ask the path', not
  'flood the record'". A-340's title: "the reference client floods only when the path is *unknown*;
  we flood whenever it is *stale*".
- **Refuted by the same round's own evidence.** The reference floods the **retry**, whatever route
  the contact holds — `openhop_core/.../base_send.py:1292-1346` (`_build_retry_packet`: "Build a
  retry attempt as a FLOOD request, whatever route the contact holds … The stored path is only
  masked, never cleared"), which the ledger's own A-380 cites. Its docstring even names the failure
  we are trying to avoid: a peer taught a wrong route "answers into a void with no flood reply left
  to correct it".
- Corrected statement: the reference floods when the path is unknown **and on every retry**. Our
  `want_flood` (`mesh_serial.rs:1413-1417`) contains two things the reference does not have —
  `path_stale` (floods *before* a direct attempt has failed) and `peer_via_flood` (sticky) — and one
  it does: `direct_silent` (escalate after a direct send produces nothing). The fix is to remove
  `path_stale` and `peer_via_flood`, not escalation; register row 1 as written would make a lost
  record never escalate, which is a regression against both the reference and our own ADR-0038.

### A-442 · ledger: A-80's premise contradicts its own evidence, and its fix is inverted · **major**

- `ledger:` A-80 (blocker) Finding: "at range no record ever crosses **direct**, so `proven` is never
  set by the carrier that matters". Its Proposed fix: "Then `proven` should also be set by 'this route
  has been carrying records and being acked', not only by a direct arrival."
- **Refuted twice over.** (1) A-80's own evidence line cites `:2192-2204` — "a flooded record sets
  `peer_path_proven = true`" — so `proven` *is* set at range, by the only carrier that arrives; the
  entry cites the line that refutes its premise. (2) The proposed fix says proof is "only set by a
  direct arrival", which is exactly backwards: the direct branch sets `peer_path_heard` but **not**
  `peer_path_proven` (`:2266-2280`, verified — only `pathless_directs = 0` and `peer_via_flood =
  false`), while the flood branch sets it (`:2198-2206`).
- Corrected statement: the symptom A-80 reports is real and its arithmetic is right (hops=1 ⟹
  `rto_initial = 6 × 0.490 × 1 = 2.94 s` vs a 16-hop crossing of ~51 s). The mechanism is the
  opposite of what the entry says: a flood-derived reversed chain is taken as proof of a direction it
  never travelled, and the direct arrival — the only event that proves the outbound path — marks
  nothing. Fixing "as written" would add proof-on-ack on top of flood-proof and change nothing.

### A-443 · ledger: register row 2's fix creates a new failure and collides with A-81 · **major**

- `ledger:` Register row 2: "→ time by the maximum hop count seen from the peer, never by a shortened
  one". A-81 (same round, not in the register) proposes raising the constant instead: "`rto_initial =
  hops × (hold(score_record) + hold(score_ack))` … Keep MeshCore's `6 × airtime × hops` only as the
  floor".
- **Both cannot be right, and the register's version is harmful.** The timer uses the path actually
  used to send — `hops_now = peer_path … map_or(1, |p| p.hops)`
  (`crates/daemon/src/mesh_serial.rs:1877-1881`) → `arq.rs:187` `scaled = airtime * 6.0 * hops`.
  Timing that same send by "the maximum hop count seen" means a genuine 1-hop neighbour whose request
  once arrived via a 16-hop detour waits `6 × 0.490 × 16 = 47 s` before its first retransmission on a
  link that echoes in ~3 s — the mirror image of the bug being fixed. Applied together with A-81
  (6 → ~11 airtimes) the product is 86 s per attempt at 16 hops.
- Corrected statement: the defect is not "shortest vs longest" but "the *sending* path is a guess
  derived from inbound traffic". Time the send by the path it is sent on (correct today) and make the
  path trustworthy (A-370: mark proof on a direct arrival; A-363: repair a dead path in two packets),
  rather than inflating the constant or the hop count.

### A-444 · ledger: A-81's own arithmetic does not close · **minor**

- `ledger:` A-81: "a real round trip is ≈11 airtimes per hop, not 6. At hops=16 the timer is 47 s
  against a 63 s round trip — 74 % of the way there".
- Arithmetic: 16 hops × 0.490 s = 7.84 s per airtime. 11 airtimes ⟹ **86 s**; the stated 63 s is
  **8** airtimes per hop (8 × 7.84 = 62.7 s). So either the hold factor is 8 (not 11) or the round
  trip is 86 s — and then 47 s is 54 %, not 74 %.
- The 86 s figure is the one that agrees with the rest of the ledger (A-41's 2.70 s/hop hold + 0.49 s
  of own air × 16 ≈ 51 s one way), which makes the 63 s in A-81 the outlier.

### A-445 · ledger: A-60's number and its "reference repeater" are both wrong, and its own field evidence contradicts it · **major**

- `ledger:` A-60 (blocker): "on any mesh configured the way the reference repeater ships, our traffic
  dies at **5 hops**", citing `mcrs/profiles/defaults.conf:21-22` and `mcrs/CONFIG.md`.
- **Refuted on the number and on the source.** `mcrs` is a third-party Rust reimplementation, not the
  firmware that runs on repeaters. The shipped C++ firmware defaults the cap to **0**:
  `filter-firmware/src/helpers/CommonCLI.h:48` (`uint8_t flood_max_unscoped = 0;`), enforced at
  `filter-firmware/examples/simple_repeater/MyMesh.cpp:445` via
  `mesh::isFloodHopLimitExceeded` (`helpers/RoutingPolicy.h:16-23`, `if (… ROUTE_TYPE_FLOOD && hops
  >= flood_max_unscoped) return true;` — with 0, *every* unscoped flood is refused at the first
  repeater).
- And this mesh demonstrably forwards them: the ledger's own field lines show adverts at 5–12 hops and
  `route request (unscoped) x3` reaching the host at 1–16 hops. So the deployed repeaters are not at
  the default, which means the entry's premise cannot be the observed cause of anything — it is a
  portability risk, real but conditional.
- Corrected statement: unscoped floods are a real hazard with a *worse* worst case than stated (0
  hops, not 5, on a firmware-default repeater) and a *better* local reality than stated (this mesh
  forwards them). Severity should stay high but the entry must say "on a repeater at the firmware
  default" and stop presenting the mcrs number as the reference. An implementer testing against "5"
  is testing the wrong threshold.

### A-446 · ledger: "eight records per poll" is the L2 credit, not the range figure · **minor→moderate**

- `ledger:` A-102 (blocker at range): "the ARQ may hand over up to eight records per poll"; Register
  row 6 repeats it: "the credit window hands over eight records at once". A-140 (batch round) says
  "the credit window of 4 at 16 hops".
- Code: the ARQ window is `credit = (64.0 / hops).clamp(1.0, 8.0)` (`crates/session/src/arq.rs:227`)
  — **4 at 16 hops, 2 at 28**. `CREDIT_WINDOW = 8` (`crates/mesh/src/constants.rs:14`) is the L2 cap
  and only binds at ≤8 hops.
- Corrected statement: at range a poll can hand over **four** data records at 16 hops, not eight. The
  entry's conclusion (a burst deafens the sender) is unaffected, but a blocker priced at 2× the range
  figure invites a window change twice as large as needed.

### A-447 · ledger: two entries, one defect — the `pages` buffer · **minor**

- `ledger:` A-100 ("the list page buffer survives a session death, so runs merge") and A-323 ("the
  list buffer merges runs and the first page decides what the whole run means") cite the same lines
  (`MeshTermDriver.kt:471`, `:2145`, `failed()` never clearing `pages` at `:765-806` / `:805`) and
  propose the same fix (clear `pages` in `failed()`). A-107 covers A-323's second half (a pushed run
  classified by the first page).
- Correction: collapse A-100 and A-323 into one entry before the fix pass, or the same five-line
  change will be scheduled, implemented and reviewed twice.

### A-448 · ledger: A-61's fix names infrastructure that does not exist · **minor**

- `ledger:` A-61 Proposed fix: "the outbox/pending-send structure already exists in `Companion`".
- Code: `Companion` has no send queue — `crates/companion/src/session.rs:477-481` are
  `has_pending()` / `next_push()`, both **read**-side (pending pushes from the radio). The outbox is
  `SessionHost.outbox` (`crates/daemon/src/mesh_session.rs:330`), one layer up, and it is drained by
  `poll`, not by a timer.
- Correction: the asynchronous reply needs a new delayed-send slot in the carrier loop (or an
  `outbox`-style queue in `mesh_serial`), not a reuse of something in `Companion`.

### A-449 · ledger: A-24's supporting "measurement" is a different packet size · **moderate**

- `ledger:` A-24: "a flooded handshake *can* cross 14 hops (measured: host `flooded record 121 B from
  a 0 hop chain`, and earlier `flooded record 40 B from a 14 hop chain`)".
- The 121 B handshake observed crossed **0** hops; the record that crossed **14** was **40 B** — a
  hint. At a ~0 dB-margin link those are not interchangeable evidence: the firmware's own score
  (`RadioLibWrappers.cpp:319-328`) is `((snr − threshold)/10) × (1 − len/256)`, i.e. 0.075 ×
  `(1 − 40/256)` = 0.063 for the hint against 0.075 × `(1 − 133/256)` = 0.036 for the handshake — the
  hint is ~1.75× more likely per attempt, before per-hop holds enter.
- Correction: A-24's conclusion (the blind ladder should not skip the handshake now that flood exists)
  may still be right, but it needs a measured 121 B flood hop count, not a 40 B one.

### A-450 · ledger: A-200 is graded `blocker` against the ledger's own definition · **minor**

- `ledger:` A-200 (blocker): "changing the route destroys the radio session".
- The ledger's own scale says `blocker` = "the link cannot work, or works only by luck". Here the link
  works; the user pays a full ladder for a UI action. And the code says the rebuild is deliberate:
  `Config.kt:219-221` ("choosing a route does rebuild the page").
- Correction: `major`. Kept above cosmetic matters because the *cost* is enormous (≥190 s at a
  neighbour), but as a blocker it outranks defects that actually break the link, and the ledger is
  ordered by severity.

### A-451 · ledger: A-41's title conflates a per-repeater bound with a typical crossing · **minor**

- `ledger:` A-41 title: "MeshCore delays **every flood at every repeater** by up to 32 s, so a crossing
  takes 45–58 s while our rung interval is 8–36 s".
- Code: `MAX_RX_DELAY_MILLIS 32000` (`filter-firmware/src/Dispatcher.cpp:11`) is a clamp; the delay is
  `(10^(0.85−score) − 1) × airtime` (`:53`), and at the measured score (~0.03) it is ~2.7–3.5 s/hop,
  which is where 45–58 s comes from. Fourteen hops at the cap would be **448 s**.
- Correction: state the bound and the typical separately — "up to 32 s per repeater (a bound never
  reached at this preset); ~2.7–3.5 s per repeater at the measured score, so 45–58 s over 14 hops".
  As written the sentence reads as if 32 s × 14 hops ≈ 58 s.

### A-452 · ledger: the "0.66 s at home" anchor is not derivable from any cited reading · **minor**

- `ledger:` Numbers table: "Repeater hold per hop: 0.66 s at home, **2.70 s at the margin** for a 133 B
  packet — 4.1× worse than the timers we ship".
- Arithmetic with the firmware's own formulas (`RadioLibWrappers.cpp:319-328` score,
  `Dispatcher.cpp:53` delay): 0.66 s for a 133 B packet implies `score ≈ 0.48` ⟹ `SNR ≈ +2.5 dB`. The
  ledger's authoritative readings are −6.75 dB (which gives 2.70 s) and +5.5 dB (which gives 0.33 s).
  Neither produces 0.66 s.
- Correction: anchor the home figure to a cited SNR reading (or label it "assumed +2.5 dB"), because
  the 4.1× ratio is quoted in A-340 and in the shape round's headline as if measured.

### A-453 · ledger: A-304's "the flooded reply is one packet" is true of route replies only · **minor**

- `ledger:` A-304: "the host's flooded reply is one packet, not three, so row 6.228 is closed
  prematurely".
- Code: `send_route_reply` builds one packet and calls `send_retrying`
  (`crates/daemon/src/mesh_serial.rs:290-352`; the single `build_route_reply_flood_in_scope` at `:309`) — one transmission unless the radio refuses. But the
  **handshake answer** is a control record from `outbox` and, when pathless, goes through the ×3 copy
  loop (`:1441-1468`, `RECORD_FLOOD_COPIES = 3` at `:513`). A-22, A-34 and A-240 all price it as ×3,
  correctly.
- Correction: say "route reply (path return)" explicitly. As worded it invites "fixing" path returns
  to three copies, which would triple route-discovery air — the single most expensive thing on the
  link per A-341/A-340's own numbers.

---

## Verified correct (do not re-open)

Checked against the code and left alone: **A-20** (each accepted `msg1` replaces the session and
advances `next_index`, `mesh_session.rs:280-345` — no dedup), **A-21** (the pending handshake is
consumed before the `isHandshakeKind` guard, `MeshTermDriver.kt:907-918`), **A-22** (`seq: None`,
popped once, `on_send_refused` cannot restore it, `:380-405`), **A-30** (`handshakes += 1` per accepted
message, `:333`), **A-31** (persist with two `sync_all`s before the reply is queued, `:303-331`),
**A-120** (`peer_via_flood` set at `:2208`, cleared only at `:2278` — a genuine absorbing latch),
**A-180** (`Event::Status` every 500 ms refreshes `last_carrier`, `mesh.rs:966`, `mesh_serial.rs:1809`),
**A-220** (`session = None` before any log, `:411-431`), **A-370** (the direct branch sets
`peer_path_heard`, not `proven`), **A-420** (`resumeShell` only ever assigned `""`), **A-41/A-362**
(`route_stale = max(60 s, 2 × 603 ms × hops)`, `:166-169`).
### A-480 · ownership: the radio can only be yielded by stopping the whole daemon · major
- Evidence: `crates/daemon/src/bin/ldmux.rs:1171-1186` — the `mesh` CLI has exactly two verbs, `status` and `preset`; `crates/daemon/src/daemon.rs:1841` is the only other `mesh.*` method (`mesh.preset`); `crates/daemon/src/mesh.rs:685-740` (`supervise`) has one loop and no parked/idle state — it reopens the port for the life of the process; `crates/daemon/src/mesh.rs:133-137` reads `LDMUX_MESH_SERIAL` once at startup; observed: the other automation runs `systemctl --user stop ldmux.service` / `start` (journal 12:46:54Z, 12:56:46Z, 13:03:23Z, 13:04:29Z) to free `/dev/ttyACM0`.
- Finding: there is no way to ask a running daemon to put the radio down. The only lever is stopping the process, which is a full teardown: session, route, claims and every timer go with it. The two tools are therefore forced into a stop/start handover for a resource the kernel already arbitrates.
- Why it hurts at range: each handover costs the range user a re-handshake and a re-attach (A-100…A-108), and the phone is the end that cannot tell why. The daemon has no state that says "someone else has the radio", so `mesh status` reports a link failure and the phone retries into a port it cannot have.
- Proposed fix (NOT applied): a `mesh.release` / `mesh.acquire` pair on the existing RPC surface, mirroring `request_preset`: the radio belongs to the link task, so a caller leaves a request and the link task acts on its next pass (`crates/daemon/src/mesh.rs:465-496`). `supervise` gains a `parked` state that waits on `wake` instead of reopening, and the snapshot gains `state: "released"` with a reason.
- Open questions: whether release should be time-bounded (a lease that expires so a forgotten release cannot strand the radio), and whether the phone should be told the radio was released rather than left handshaking.
- Investigator: ownership round · 2026-11-11

### A-481 · ownership: the exclusive ownership already exists and is anonymous · major
- Evidence: `crates/companion/src/link.rs:150-162` (`lock_exclusive`: `flock(LockExclusiveNonblock)`, "One owner per radio. The tty permits several opens, so without an advisory lock two clients interleave frames on one port"), taken at `crates/daemon/src/mesh_serial.rs:1070` and held for the connection; `crates/companion/src/link.rs:92-98` (`O_CLOEXEC` is what keeps the lock from being inherited by every spawned shell); failure text `crates/daemon/src/mesh_serial.rs:1070-1074` (`"{device}: another process holds this radio ({e})"`).
- Finding: the OS-level ownership protocol is already correct and already enforced. What is missing is that it is **anonymous and one-directional**: nothing names the holder, `ldmux mesh status` does not report that the radio is held elsewhere, and the explaining sentence only exists inside ldmux's own journal. A second tool learns the radio is busy only by failing to open it or by running `fuser`.
- Why it hurts at range: two tools that both want the radio have no shared vocabulary, so the one that cannot see a holder resorts to stopping the other. That is the observed behaviour, and it is a coordination failure papered over with `systemctl stop`.
- Proposed fix (NOT applied): publish the holder. Write `<state>/radio.lock` (pid, process name, device, since) immediately after the flock succeeds and remove it on release, and add `holder` to the `mesh status` shape (`crates/daemon/src/mesh.rs:378-410`). Cost is one file write per open and one JSON field; a second tool can then read who has it instead of guessing.
- Open questions: whether a stale lock file (process killed -9) should be reaped by pid liveness or ignored in favour of the flock, which is authoritative.
- Investigator: ownership round · 2026-11-11

### A-482 · ownership: stopping the daemon closes the port with a write possibly in flight and sends no close command · major
- Evidence: `crates/daemon/src/bin/ldmuxd.rs:301-324` — on SIGTERM the process calls `daemon_ref.lock()` / `d2.stop()` and then `std::process::exit(0)`; `crates/daemon/src/daemon.rs:1417-1440` (`stop()` aborts the accept loops, the remote listener and the streams — it does not touch the mesh task or the serial fd); writes happen on the link thread with a 2 s bound (`crates/companion/src/link.rs:167-183`, `write_all_within`); the firmware's parser takes its frame length from the next two bytes with no bound (`filter-firmware/src/helpers/ArduinoSerialInterface.cpp:39-70`, recorded as A-400).
- Finding: `std::process::exit(0)` skips the destructors, so the tty is closed by the kernel wherever the writer happened to be. There is no `CMD_` that tells the radio the session is over, and nothing drains or pads an in-flight frame first. If the exit lands mid-frame, the firmware is left waiting for the remainder of a length it has already accepted — the exact desynchronisation A-400 describes — and the **next** owner inherits it.
- Why it hurts at range: this is the mechanism by which "stop ldmux so the other tool can use the radio" is not a clean handover. It would explain a radio that needs a power cycle after an external test session, and it separates two faults that have been conflated all along: a wedged radio can be caused by a stop, not only by the DTR/RTS reset.
- Proposed fix (NOT applied): make the stop graceful for the radio, not just for the sockets. Either have `stop()` signal the link task to finish the frame in flight and close the carrier before `exit`, or (cheapest) require that the process exit only after the link task has acknowledged a `release` (A-480), which is the same machinery. No new protocol: the last thing sent is the end of a frame, not the middle of one.
- Open questions: whether this board's USB-Serial-JTAG truncates a write at a byte boundary or keeps the tty across a firmware reset (A-400 lists this as an open bench measurement, and it decides whether a mid-frame exit can desynchronise the parser at all).
- Investigator: ownership round · 2026-11-11

### A-483 · ownership: a restart destroys the route and the answered-nonce ring, so the first records after it are floods · major
- Evidence: `crates/daemon/src/mesh_serial.rs:1207-1216` (`peer_path`, `peer_path_heard`, `peer_path_proven` are locals of the run loop, not persisted), `:1413-1417` (`want_flood = peer_path.is_none() || path_stale || direct_silent || answer_flood_with_flood || peer_via_flood`), `:187-199` (`ANSWERED_NONCES = 16`, an in-process ring); persisted instead: `:1191-1205` (the hour's airtime window is read back from `window_record`), `:1133-1136` (the peer's *key* is read back from `peer_record`), `crates/daemon/src/mesh_session.rs:524-545` (pairing and its replay floor in `mesh-pairing.json`), `crates/daemon/src/daemon.rs:453` (`RESTORE_GRACE_MS = 60_000`) with claims in `<state>/claims.json` (`:2792`).
- Finding: a restart loses the session, the peer path and its proof, and the answered-nonce ring. It keeps the hour budget, the peer key, the pairing and the replay floor, and claims only for sixty seconds. Because `peer_path` starts as `None`, **the first records after every restart are flooded**, and because the nonce ring is empty, the three copies of an in-flight request are all answered.
- Why it hurts at range: a restart converts a working direct path into the expensive carrier for as long as re-learning takes (A-340 prices it: 65.6 s versus 9.8 s at 16 hops for one record). The external tool's stop/start cycles therefore do not merely interrupt the link, they push it onto the worst carrier, and a burst of `route reply sent ok=true` lines after a restart can be one request answered three times.
- Proposed fix (NOT applied): persist the peer path with its proof and age alongside `peer_record`, and load it on open with the same "a route that merely exists is not a route that works" check the code already applies. The nonce ring is cheaper still: the duplicates arrive within seconds, so a restart mid-burst is rare and this half only needs a note, not code.
- Open questions: how long a persisted path should be trusted after a restart (A-421 shows a 506 s cached path being adopted at the wrong hop count, so a naive load would make that worse).
- Investigator: ownership round · 2026-11-11

### A-484 · ownership: a daemon restart also repairs the radio clock, which makes every stop/start a confound in the field data · minor
- Evidence: `crates/daemon/src/mesh_serial.rs:1075-1080` — `prepare` calls `companion.set_time(unix_now())` on every open ("The radio's clock is what stamps an advert … one command, best effort"); `crates/daemon/src/mesh.rs:684-718` (`supervise` re-runs `link` → `prepare` on every reconnect); the earlier reading (A-404/A-405) that the radio clock is never restored concerns a radio that reboots **mid-session**, where the daemon does not reopen.
- Finding: a daemon stop/start re-times the radio. So the stop/start cycles by the other tool have been incidentally repairing the clock, and any field observation taken inside such a window is measuring a radio whose advert ages were just reset. The distinction matters: the clock loss is real but belongs to mid-session reboots, not to daemon restarts.
- Why it hurts at range: advert age drives the flood-versus-direct decision (A-340) and the route freshness the timer leans on (A-80). A confound there invalidates any conclusion drawn from logs that straddle a restart, and it means a restart can appear to "fix" the link for reasons unrelated to the change being tested.
- Proposed fix (NOT applied): none in the protocol. Log one line per open saying the radio clock was set, with the offset the firmware reported, so a reader can see whether a window straddled one. That is the same instrumentation gap as A-465/A-466.
- Open questions: whether the firmware reports a readable clock back, so the offset (not just the set) can be logged.
- Investigator: ownership round · 2026-11-11

### A-485 · ownership: ldmux can blame "another process" for its own lock · minor
- Evidence: `crates/daemon/src/mesh.rs:52-63` — the comment records it: `prepare` runs on a blocking task that cannot be cancelled and holds the `flock` while it works (`APP_START` 5 s, `DEVICE_QUERY` 5 s, the width write 5 s, the scope read 1.5 s = 16.5 s worst case), and "a ten-second bound fired inside that window abandoned the task with the lock still held, so the next attempt failed with 'another process holds this radio' — the wrong cause for the wrong symptom"; bound now `CARRIER_TIMEOUT = 30 s` (`:62`); the reopen cadence is `backoff` 1 s doubling to a 30 s cap (`:712-738`).
- Finding: the timeout that guards the open is not a cancellation, so an open that overruns leaves a zombie task holding the exclusive lock while the supervisor opens again and is refused by its own lock, with a message naming another process.
- Why it hurts at range: after an external stop/start cycle this is exactly the state a reader will encounter, and the message points at the other tool when the fault is internal. It also means a fast external handover can collide with ldmux's own 16.5 s of setup work.
- Proposed fix (NOT applied): distinguish the two cases in the sentence — check whether the lock is held by this process (a pid recorded in `<state>/radio.lock`, per A-481) and say "this daemon is still opening the radio" instead. The overrun itself is already mitigated by the 30 s bound; what is wrong is only what the operator is told.
- Open questions: whether the blocking task can be made open-only (take the lock, do the identification on the next pass) so the setup work never sits inside the critical section.
- Investigator: ownership round · 2026-11-11

### A-486 · ownership: a release/acquire costs one request slot and one supervisor state · question
- Evidence: `crates/daemon/src/mesh.rs:465-496` — `preset_request: Arc<Mutex<Option<Retune>>>` with `request_preset`/`take_preset` and `wake.notify_one()`; the doc comment states the principle: "The radio belongs to the link task, which owns the carrier, so every other caller can only leave a request. It is applied on the loop's next pass and the snapshot then says whether the firmware took it." `crates/mesh/src/error.rs:13-27` — `MeshError` has `Closed`, `Timeout`, `Io`, `Unsupported`; `crates/daemon/src/mesh.rs:685-740` — the supervisor's shape.
- Finding: the seam needed for a clean handover already exists and is idiomatic in this codebase. A release is the same shape as a retune: a request slot the link task takes, plus one outcome the supervisor can distinguish (`released`) so it parks instead of reopening, plus the snapshot state and one RPC method. Nothing in the carrier, session or protocol has to change.
- Why it hurts at range: this is the difference between a coordination fix that costs a day and one that costs a rewrite. It also removes the reason for stopping the daemon, which is what costs the range user a handshake and an attach.
- Proposed fix (NOT applied): `MeshError::Released`, `release_request: Arc<Mutex<Option<String>>>` (reason/holder), a `parked` arm in `supervise` that awaits `wake`, `state: "released"` in the snapshot, `mesh.release` / `mesh.acquire` in `crates/daemon/src/daemon.rs` next to `mesh.preset` (`:1841`), and `ldmux mesh release|acquire` in `crates/daemon/src/bin/ldmux.rs` beside `status`/`preset` (`:1118-1186`).
- Open questions: whether the parked state should also end the message to the phone (a `Closed` record) or simply stop counting as reachable.
- Investigator: ownership round · 2026-11-11

### A-487 · ownership: which option to choose · major
- Evidence: (a) `crates/daemon/src/bin/ldmuxd.rs:215` — with `LDMUX_MESH_PSK` set and `LDMUX_MESH_SERIAL` unset the daemon prints a reason and disables the mesh, and `crates/daemon/src/mesh.rs:603-680` (`disabled_json`) keeps the status shape; the variable lives in `~/.config/ldmux/env` (`deploy/ldmux.env.example:29`). (b) would require the other tool to speak the companion protocol (`crates/companion/`). (c) is priced by A-486. (d) is A-481 plus A-486.
- Finding, judged honestly:
  - **(a) ldmux without the radio** is the cheapest safe option today: no code, one line in `~/.config/ldmux/env`, one restart, and the daemon then never opens the port so nothing has to stop it. The cost is total — the phone's radio route cannot work at all while it is set, so it is not a coexistence story, it is a handover.
  - **(b) the other tool using ldmux's mesh** is not cheap: it needs the companion protocol to expose captures and raw sends, which is a larger surface than a release command and duplicates what the phone already does.
  - **(c) release/acquire** is the permanent fix and is small (A-486). It keeps both tools usable and, because the release is a request the link task honours, it also fixes the mid-frame exit (A-482).
  - **(d) named ownership** — (c) plus the lock file of A-481 — is what makes the handover self-explaining, and it is (c) plus one file write.
- Why it hurts at range: the choice decides whether the radio path can be tested at all while another experiment is running. Option (a) is a switch, not a fix, and it should be stated as such rather than presented as coordination.
- Proposed fix (NOT applied): today, (a) — set it explicitly and tell the operator the radio is off. Then (d), because a lease that names its holder is the only version where neither tool has to know the other exists.
- Open questions: whether the other tool would honour a lease at all, which is outside this repository and decides whether (d) is worth building.
- Investigator: ownership round · 2026-11-11

### A-488 · ownership: a parked link must reset its backoff, or a resume waits thirty seconds · minor
- Evidence: `crates/daemon/src/mesh.rs:684-740` — `backoff` doubles per failed attempt to a 30 s cap, and is reset to 1 s only when `bound` says an open completed (`:718-736`); `bound` is set true inside `session` (`:834`), i.e. only after a successful open.
- Finding: a release is not a failed open, but it would flow through the same loop. If the parked state is implemented as "the open attempt failed", the backoff climbs while parked and the first reopen after an acquire waits up to 30 s.
- Why it hurts at range: a 30 s delay on resume looks exactly like a dead radio, and it lands on top of the handshake ladder the phone has already been climbing (A-24, A-280). The park must be distinguished from a failure.
- Proposed fix (NOT applied): reset `backoff` to its floor on every park and on every acquire, exactly as `bound` resets it after a completed open; treat "released" as an outcome that is neither success nor failure.
- Open questions: none beyond the park's shape in A-486.
- Investigator: ownership round · 2026-11-11

### A-489 · ownership: an eighteen-minute handover costs the range user a full attach · major
- Evidence: `crates/daemon/src/daemon.rs:453` (`RESTORE_GRACE_MS = 60_000`) with claims persisted in `<state>/claims.json` (`:2792`), `crates/daemon/src/ownership.rs:15` ("reloaded with a re-claim grace"); the observed cycle from the journal: stop 12:46:54Z, start 13:04:29Z — **17 min 35 s**; the phone's whole path after a lost claim is the launcher, a list round trip, an attach and a screen (A-100…A-108), and the resume that would skip it is dead code (A-420).
- Finding: any handover longer than sixty seconds discards the phone's ownership claims, so the phone must choose a shell again from scratch. Every observed cycle was three orders of magnitude longer than the grace.
- Why it hurts at range: an attach at range is minutes (A-102/A-108: 18–27 transmissions, an ARQ timer that expires mid-run), so each external test session does not pause the user's shell, it takes their place away and charges them minutes to get it back.
- Proposed fix (NOT applied): none specific to ownership — but a release/acquire (A-486) removes the reason for the long stop, and it makes the grace question moot because the daemon never goes away.
- Open questions: whether a claim should survive a longer absence when the shell itself is untouched, which is a product decision rather than a bug.
- Investigator: ownership round · 2026-11-11

### A-490 · ownership: does the firmware need a reset before another owner opens it? · question
- Evidence: `crates/daemon/src/mesh_serial.rs:1075-1100` — every open begins with `APP_START`, then `set_time`, `device_query`, a width write and a scope read; there is no `CMD_` to close a session; `crates/companion/src/link.rs:80-99` opens the tty plain (`O_CLOEXEC`) and deliberately never touches DTR/RTS, clearing `HUPCL` so that closing the port does not drop the modem lines ("that is the part that turns 'the daemon restarted' into 'the radio rebooted'").
- Finding: both tools open the same tty and begin with `APP_START`. Whether the firmware is stateless between owners, or needs the previous session closed (or the parser reset, per A-400), is not determined by the code.
- Why it hurts at range: if the firmware carries state across owners, a clean handover needs a documented close, and the release command of A-486 must send it. If it does not, the release can simply drop the fd, which is simpler and has no failure mode.
- Proposed fix (NOT applied): one bench measurement — open, send `APP_START`, close without any shutdown, then reopen from a second process and send `APP_START` again; record whether `DEVICE_QUERY` answers normally. That is the whole experiment.
- Open questions: as above, plus whether the firmware's `APP_START` itself sets the clock (the daemon sets it separately at `:1080`, which suggests it does not).
- Investigator: ownership round · 2026-11-11

## Decision recorded: radio ownership (2026-11-11)

- **ldmux keeps the radio.** `LDMUX_MESH_SERIAL` stays set, and the Meshmore work must use the daemon's mesh or wait its turn.
- **No lease machinery.** The conflict is a development-side collision, not a product requirement: Meshmore is a phone application that uses one radio at a time, and the desktop side only needs to take turns. Building `mesh.acquire`/`mesh.release` would be machinery around a development inconvenience.
- **What the audit did establish, and what stands regardless:** the kernel already arbitrates the port (`flock` exclusive at `link.rs:158`, taken at `mesh_serial.rs:1070`), so ownership is defined but not *nameable*; and a SIGTERM can land mid-frame, which the firmware's unbounded length parser (A-400) turns into a wedged radio for the next owner. That is why a stop/start is not a neutral act, and why "wait" is the right answer for a dev collision but "never stop mid-transfer" is worth remembering.
- **Field-data consequence:** a daemon restart resets `peer_path`/`proven`/`heard` (run-loop locals) so the next link starts on flood, and it re-sets the radio clock, so any measurement spanning a restart is confounded. Two stops were recorded in the same twelve hours as the range tests.

## Bench radio ownership (M-1…M-8)

Findings about the **LoRa bench tools** under `~/Documents/lora`, not about this repo. Recorded here because their symptoms were read as protocol failures.

# Radio ownership: why the desktop work uses the Heltec, and where the conflict really is

Read-only investigation, 2026-11-11. Evidence is `file:line` or a command output.

### M-1 · which radios exist on this host, and which one the daemon owns · fact

- Evidence: `lsusb` → `Bus 001 Device 021: ID 303a:1001 Espressif USB JTAG/serial debug unit` and no Seeed device present. `ls -l /dev/serial/by-id/` → exactly one entry: `usb-Espressif_USB_JTAG_serial_debug_unit_F8:5B:1B:BE:C5:3C-if00 -> ../../ttyACM0`. `fuser -v /dev/ttyACM0` → `ldeem 619161 F.... ldmuxd`.
- Finding: **one radio is attached, and it is the Heltec** (`HELTEC-BASE`, ESP32-S3, USB VID:PID `303a:1001`). It is at `/dev/ttyACM0` and ldmux's daemon holds it. The Wio Tracker L1 is not plugged in at all (`~/Documents/lora/INVENTORY.md` records it as USB `2886:1667`, nRF52840).
- Why it matters: there is no second candidate on this desk. Any desktop tool that wants a companion over USB can only have the Heltec.

### M-2 · the two radios cannot each be both USB and BLE · constraint

- Evidence: `mesh-research/clones/filter-firmware/variants/heltec_v4/platformio.ini:182` `[env:heltec_v4_companion_radio_usb]`, `:190 -D ENABLE_USB_INTERFACE`; `:202` `[env:heltec_v4_companion_radio_ble]` with no `ENABLE_USB_INTERFACE`. `variants/wio-tracker-l1/platformio.ini:58` `[env:WioTrackerL1_companion_radio_usb]`, `:72 -D ENABLE_USB_INTERFACE`; `:86` `[env:WioTrackerL1_companion_radio_ble]`, whose `build_src_filter` adds `helpers/nrf52/SerialBLEInterface.cpp` and no USB flag. `examples/companion_radio/main.cpp:67-70` — `ArduinoSerialInterface usb_serial_interface` exists **only** under `#if defined(ENABLE_USB_INTERFACE)`.
- Finding: **the USB companion and the BLE companion are different builds.** A board flashed for BLE answers nothing on `/dev/ttyACM0`; a board flashed for USB is not reachable by the phone's BLE link.
- Why it matters: this is the whole answer to "why not use the Wio". The Wio is committed to BLE because the phone uses it through BLE. Reflashing it to `WioTrackerL1_companion_radio_usb` would give the desktop a radio and take the phone's away.

### M-3 · the recorder that wants the port is the LoRa research project, not Meshmore · fact

- Evidence: `~/Documents/lora/tools/mc_listen.sh:7` `PORT="${MC_PORT:-/dev/ttyACM0}"`, `:19` `--advert --advert-every "${MC_ADVERT_EVERY:-60}"`; `~/Documents/lora/tools/mc_recorder.py:239` `ap.add_argument("--port", default="/dev/ttyACM0")`. Meshmore's own device procedure is BLE-only: `~/Documents/meshmore-sns/M6-interop-runbook.md:4` "via a BLE-only capture", `:15` "≥2 Seeed Studio T1000-E flashed with MeshCore companion firmware", `:21-22` "choosing the **Companion Bluetooth** variant, BLE pairing"; `meshmore_sns_app/TOOLING.md:214` "Two MeshCore companions are bonded to this phone… the serial node is /dev/ttyACM0" (a note about the bench, not a test path).
- Finding: **Meshmore needs no serial port.** Its on-device procedure is BLE to a phone-paired companion. The process that takes `/dev/ttyACM0` is `mc_recorder.py` from `~/Documents/lora`, run out of `~/Documents/lora/.venv`. It appeared to be "Meshmore" because the agent that ran it had its working directory in `meshmore-sns`.
- Why it matters: no Meshmore design change is needed, and no lease machinery is justified by Meshmore. The conflict belongs to the LoRa bench work.

### M-4 · the recorder opens the port without taking the lock, so two owners can hold it at once · major

- Evidence: `~/Documents/lora/tools/mc_recorder.py:76` `MeshCore.create_serial(port, baud, rts=False, dtr=False, auto_reconnect=True, …)` — the vendored client. `.venv/lib/python3.13/site-packages/meshcore/serial_cx.py:14` `def __init__(self, port, baudrate, cx_dly=0.2, rts=False, dtr=True)` — no `flock` anywhere in the package (`grep -rn flock` over it returns nothing). The repo's own transport *does* lock (`tools/mcproto/transport.py:205-217`, `fcntl.flock(… LOCK_EX|LOCK_NB)`), but the recorder does not use that path.
- Finding: ldmux takes an **advisory** exclusive lock (`crates/companion/src/link.rs:158` `LockExclusiveNonblock`, taken at `crates/daemon/src/mesh_serial.rs:1070`), and the recorder takes **none**. An advisory lock is only an exclusion if both sides take it, so the two can hold `/dev/ttyACM0` simultaneously and interleave frames on one firmware that handles exactly one command at a time.
- Why it matters: the visible symptom on both sides is silence, not a refusal. `~/Documents/lora/data/meshcore-rx.log` ends with repeated `no response from base (not a serial companion?); retrying in 3 s` — the recorder's own explanation guesses at the firmware build when the actual cause can be a daemon sharing the port. That line is also *wrong* in a second way: the firmware here demonstrably is a serial companion (ldmux reads its preset and status from it).

### M-5 · stopping the daemon is not required, and the daemon already recovers by itself · finding

- Evidence: `crates/daemon/src/mesh.rs:685-732` — `supervise` retries forever with a backoff that resets to 1 s on every successful open and rises to a 30 s cap; a failed `prepare` becomes `state = "down"` with `last_error` set, and the loop tries again. `crates/daemon/src/mesh_serial.rs:1069-1074` — `SerialLink::open` succeeds first (a tty permits several opens), *then* `lock_exclusive()` fails with `"{device}: another process holds this radio"`.
- Finding: the daemon does not need to be stopped to free the radio; it backs off and reopens when the port is free. What it cannot do is refuse a client that never asks. Stopping it was a way to get the port *now*, and it cost more than it bought.
- Why it matters: a stop/start is not neutral. `crates/daemon/src/mesh_serial.rs:1066-1080` shows the open path re-sends the radio's time (`set_time`) and re-reads the device; the run-loop locals (`peer_path`, `proven`, `heard`) begin empty, so the first link after every restart starts on the flood carrier. Any range measurement that spans a restart is measuring a cold link.

### M-6 · the correct target for the desktop capture is a second companion, not this one · recommendation

- Evidence: M-1 (only one radio attached), M-2 (USB and BLE are exclusive builds), M-3 (the recorder is the only consumer), M-4 (the recorder does not lock).
- Finding: there are three honest options and no fourth.
  1. **A third board** flashed `companion_radio_usb` (a Heltec V4 or a Wio Tracker L1 that is not the phone's) and left on the bench for captures. Cheapest in the long run: no coupling to ldmux, no restart, and the private test preset can live on it permanently.
  2. **Let the daemon have the radio and read the capture from ldmux.** Not possible today: ldmux never subscribes to the firmware's receive log (`grep -rn "rx_log\|RX_LOG" crates/companion/src/codec.rs crates/daemon/src/mesh_serial.rs` → no hits), so it has no per-packet record. It would be a small feature, and it is the only option that removes the conflict instead of scheduling it.
  3. **Take turns, with the recorder locking.** Stop the daemon only for the length of a capture, and make `mc_recorder.py` use `mcproto.transport.SerialTransport` (which locks) instead of `MeshCore.create_serial`, so a mistake is a clear refusal rather than silent frame interleaving.
- Why it matters: without (1) or (2), every capture is a cold-link restart for ldmux, and the ledger's range measurements inherit that confound.

### M-7 · two documentation errors that made this look like a Meshmore problem · minor

- Evidence: `~/Documents/meshmore-sns/meshmore_sns_app/TOOLING.md:214` states the serial node is `/dev/ttyACM0` as if Meshmore used it; `M6-interop-runbook.md` is BLE-only. `~/Documents/lora/tools/mc_recorder.py:203-212` blames a hung disconnect for holding the port, and `:88` blames "not a serial companion?" — neither names the real competing owner.
- Finding: the machine-readable record of who wants this port is wrong in both repos, which is why the conflict was diagnosed as "another agent is stopping the daemon" rather than "two clients share one tty".
- Why it matters: the next person to hit this will reach the same wrong conclusion.

### M-8 · the daemon is currently hearing the public mesh on the Heltec · fact

- Evidence: `ldmux mesh status --json` → `radioPreset [910525, 62500, 7, 5]`, `noiseFloorDbm -90`, `radioRxSecs 299`, `radioTxSecs 0`, `session.state "waiting"`. Journal 07:14-07:19: `route advert heard from eacc4434 hops=8 rssi=-74dBm`, `… 1d15ec92 hops=11 rssi=-76dBm`, `… 2a043f04 hops=13 rssi=-76dBm`.
- Finding: the Heltec is on the North American public preset (`910.525/62.5/SF7/CR5`) and hears the city mesh at 1-13 hops, −74 to −86 dBm. It has transmitted nothing since the 06:04 start. `~/Documents/lora/docs/06-CURRENT-STATE-AND-NEXT-TEST.md:11-14` still documents the bench at the *private* preset `917.000/125/SF11/CR5`; `meshmore-sns/meshmore-sns-radio-config.md:190-200` documents the current one. The older doc is stale.
- Why it matters: a LoRa range test at the private preset needs the Heltec retuned, and that is the daemon's radio. Two programs cannot hold this board on two channels, so that test cannot be scheduled around ldmux at all — it needs its own board.
### A-520 · field: the wake lock expires ten minutes after leaving the screen, and nothing renews it · blocker

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/BackgroundSession.android.kt:63` (`WAKE_LOCK_MAX_MS = 10 * 60 * 1000L`), `:111` (`acquireWakeLock()` is called **only** from `onStartCommand`), `:140-148` (`acquire(WAKE_LOCK_MAX_MS)` — a timeout, so the lock dies on its own), `:309` (`if (wanted == running && (wantText == lastText || !wanted)) return` — the early return that makes renewal impossible while the text is unchanged), `:292-296` (`setActive` only stores the wanted state), `commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:420-434` (`BackgroundSession.setActive(wanted, text)` from `LaunchedEffect(view, disconnected)`, with `text` constant "linked to desktop over radio" while the view is `Live`), `:412-416` (the field note: "no app log line at all between 00:21 and 00:55 while walking out to 28 hops").
- Finding: the foreground service holds a **partial wake lock with a ten-minute timeout**, and it is renewed only by a *new* `startForegroundService`, which `applyLockState` issues only when the service transitions or when its text changes. On the mesh route the text is two fixed sentences chosen from the view, so a link that stays `Live` off screen for ten minutes produces no renewal: the service keeps running (and the notification keeps saying "linked to desktop over radio") while the CPU is free to nap. The comment at `MeshTerminalScreen.kt:412-416` records the symptom and blames the service lifetime; the lock's timeout is the other half of it and was never addressed.
- Why it hurts at range: at range every useful thing happens with the phone in a pocket — that is when the ladder has to run, when the host's answer must be received, and when the 120 s answer window and 8/16/32 s rungs are timed. A wall-clock timer in a `delay()` under a napping CPU is not late by milliseconds; it waits for the next maintenance window, which is minutes. The one piece of field evidence in the code base (34 minutes with **no log line at all**) is exactly the shape this produces, and it is the shape the operator reads as "it just sits on handshaking".
- Proposed fix (NOT applied): renew the lock from a tick that exists for it — for example a `Handler`/coroutine in `LdmuxService` re-arming every `WAKE_LOCK_MAX_MS / 2` while `wantLock` is true — or acquire without a timeout while the service is up, since the service is already bounded by `BackgroundSession.releaseGraceMs` and by `LINK_GIVE_UP_MS`. In either case write one line when the lock is (re)acquired and when it lapses, so a field log can show whether the CPU was ever awake.
- Open questions: whether this device's doze entry actually defers the driver's `delay()`s or only the radio's callbacks (a 15-minute background test with the app logging every 30 s would show it in `mesh-log.txt` as a gap); whether a `Handler`-based renewal survives the process being frozen without the lock (it does not — which is why the renewal has to come from the service, not from the composition).
- Investigator: field round · 2026-11-11

### A-521 · field: the durable log keeps about one hour at range, and it fills fastest exactly when the link is worst · major

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/AppLogFile.kt:30-31` (`MAX_BYTES = 256 * 1024`, `KEEP_BYTES = 128 * 1024`), `:39-46` (`append` → `appendText` → `trim` once past the maximum), `:33` (a fixed-width stamp), `androidMain/kotlin/io/ldmux/mobile/AppLog.android.kt:14-21` (every `appLog` reaches both the buffer and the file), `androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1047-1051` (one disk line **per flood copy**), `:1569-1589` (`reportHeard` writes a line every report window — 10 s when no route is fresh, 60 s when one is), `:1658-1662` (advert lines), and the measured density in the ledger's own field excerpt (12 lines in 27 s, 22:22:01–22:22:28).
- Finding: at range the file carries roughly **1,400–1,600 lines an hour** at ~95 bytes a line, about 140 KiB, so a 256 KiB file trims to 128 KiB after roughly 55 minutes and then holds about **one hour of history**. The line rate is not incidental: `reportHeard` reports every 10 s while there is no fresh route, and every flooded record costs three lines (`1/3`, `2/3`, `3/3`). The log is therefore densest during the retry storm that follows a failure, which is the one period a reader needs.
- Why it hurts at range: a range trip is hours. The phone's durable log is the only field record (logcat rolls in minutes, per the file's own header), and today it keeps the last hour — so the failure that started the retries is pushed out by the retries themselves. This is the third time the same evidence problem has appeared in the ledger (A-205, A-220's silence, A-465's rate limits) in a new form: the record exists, and it deletes itself.
- Proposed fix (NOT applied): keep a small ring of files (`mesh-log.1.txt`, `.2.txt`, …) instead of one, so the total is bounded by the disk rather than by a single 256 KiB ceiling and a trip keeps its whole story; and demote the per-copy line to one line per record with a copy count (the copies are already distinguishable by their outcome), which alone halves the rate. A separate, never-trimmed `mesh-failure.txt` holding the last session death would survive even a much tighter budget.
- Open questions: the real line rate on a day at range (the estimate above comes from the ledger's excerpt, not from a measured file); whether the per-copy line is load-bearing for the `record by flood 1/3 → 2/3` reading that diagnoses carrier trouble (it was, on 2026-11-10) — if it is, one line with `3/3` at the end carries the same information.
- Investigator: field round · 2026-11-11

### A-522 · field: the link's freshness and send floors are wall-clock subtractions with no negative guard, so a backward clock step makes a dead route look fresh and silences its own repair · major

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:1533-1534` (`routeIsFresh(now) = hostPath != null && routeHeardAt > 0L && now - routeHeardAt <= ROUTE_FRESH_MS`), `:1971` (`ROUTE_FRESH_MS = 120_000L`), `:1384` (`stale = System.currentTimeMillis() - routeHeardAt > ROUTE_TRUST_MS`), `:1654` (`if (now - lastRequestAt < requestIntervalMs()) return false`), `:1633` (`if (now - lastAdvertAt < ADVERT_INTERVAL_MS) return`), `:674` (`val plausible = heardAt in (now - 86_400_000L)..(now + 60_000L)`), against `:1716` (`if (airtimeWindowStartMs <= 0L || now - airtimeWindowStartMs >= 3_600_000L || now < airtimeWindowStartMs)` — **this one guards against a negative difference, and the others do not**), and `commonMain/kotlin/io/ldmux/mobile/mesh/Radio.kt:630-631` (the driver's own clock is `TimeSource.Monotonic`, so the two halves of one link disagree about what time it is).
- Finding: every age and floor in the link is `System.currentTimeMillis() - stamp`, and a stamp in the future makes the difference negative. A negative difference is `<= ROUTE_FRESH_MS`, so the route reads **fresh**; it is `< requestIntervalMs()`, so a new route request **returns false** and is never sent; it is `< ADVERT_INTERVAL_MS`, so no advert goes out; it is not `> ROUTE_TRUST_MS`, so the route is never treated as stale. One backward clock correction therefore produces a link that believes a dead route is fresh, never asks again, and never announces itself — while the driver's monotonic timers carry on retransmitting into that route. The airtime meter at `:1716` shows the author knew the hazard and handled it in one place only.
- Why it hurts at range: this is the after-hours failure, not the during-test one. A phone that has been in radio mode with no network for hours is exactly the phone whose clock gets corrected when it rejoins, and the correction can be minutes. The resulting state is indistinguishable from "no coverage", it persists until the clock catches up (which may be the whole correction interval), and nothing logs a clock change because nothing compares clocks.
- Proposed fix (NOT applied): measure link-local ages on a monotonic source, as the driver already does (`Radio.kt:630`) — `SystemClock.elapsedRealtime()` or a single `TimeSource.Monotonic` origin in the link — and keep wall clock only for values that are persisted across processes (the airtime window, which already has its guard). Where wall clock must be used, clamp a negative difference to zero rather than letting it read as "just now".
- Open questions: whether the radio's own `received_at` (used for advert plausibility at `:674`) is a phone-clock value or the radio's own counter — if it is the radio's, a phone clock change also silently invalidates the 24-hour plausibility test that A-421 found the host relying on.
- Investigator: field round · 2026-11-11

### A-523 · field: connection priority is never requested, so the BLE link runs at the platform default for the whole outing · minor

- Evidence: `grep -rn "requestConnectionPriority\|CONNECTION_PRIORITY_LOW_POWER\|CONNECTION_PRIORITY_HIGH"` over `packages/mobile/composeApp/src` and `packages/mobile/vendor/libmeshcore-android/src` returns **zero hits**; `packages/mobile/vendor/libmeshcore-android/src/main/java/io/iotone/meshcore/android/AndroidBleTransport.java:53` (`PREFERRED_MTU = 256`), `:303` (`requestMtu(PREFERRED_MTU)` — the one connection parameter that *is* tuned), `androidMain/kotlin/io/ldmux/mobile/mesh/RadioScanner.android.kt:168` (`SCAN_MODE_LOW_LATENCY`, transient and correct).
- Finding: the app tunes the MTU and never the connection interval, so Android keeps the link at its default (balanced) interval for the entire session — while the traffic is a few hundred bytes a minute when idle and bursts when a screen arrives. There is no low-power state for an idle link and no elevated state for a transfer, so the cost is paid uniformly at a rate chosen for neither.
- Why it hurts at range: the phone is the end with a battery and no mains, and at range the link is idle for most of an outing (the path loop wakes once a minute, `AndroidRadioLink.kt:1958-1960`). An interval parameter is the single largest lever on BLE energy that does not change behaviour at all. Graded `minor` rather than `major` because the cost is unmeasured here: no reading in this repository prices the BLE half of the battery, and the ledger should not manufacture severity for a number nobody has.
- Proposed fix (NOT applied): request `CONNECTION_PRIORITY_LOW_POWER` on connect and while idle, escalate to `BALANCED` when a record is in flight or a screen is owed, and drop back when the pump goes quiet — driven from `AndroidRadioLink`'s existing send/settle boundaries, not from a new timer.
- Open questions: what the measured idle current of this link is (one hour with the screen off and a `dumpsys batterystats` delta would settle both this entry and A-520); whether the vendored transport exposes the `BluetoothGatt` needed to request priority, or whether it has to be added to that library.
- Investigator: field round · 2026-11-11

### A-524 · field: the off-screen release closes the transport without a `Detach`, so the host keeps the claim and the phone's grid · major

- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:379-383` (`AppLifecycle.setReleaseHandler { released = true; BackgroundSession.setActive(false, ""); conn.close() }`) and `:384-387` (`onDispose { remove(); conn.close() }`) — neither calls the driver's release, `commonMain/kotlin/io/ldmux/mobile/mesh/MeshTermDriver.kt:1689-1700` (`releaseShell()`: `queue(TermMessage.Detach); pump()`, then clears the mirror), which exists for exactly this and is only reached from the launcher's page close, and `MeshTerminalScreen.kt:373-378` (the comment states the intent: "The tailnet route lets go of its shells after the platform's grace and reconnects on return; this route now does the same").
- Finding: the mesh route's off-screen release closes the connection and says nothing to the host. The `Detach` that would release the shell is only sent when the operator closes a page in the launcher. So after the 45 s grace the phone has dropped the transport while the host still holds the phone's attach — its claim on the shell and, per `releaseShell`'s own comment, the grid the phone displaced (`:1684-1687`: "A `Detach` releases the shell and the host restores the grid the phone displaced"). The host learns the truth only when its own ARQ gives up on that session, which the ledger prices in minutes at range (A-101, A-102).
- Why it hurts at range: at range the grace fires on every walk, every pocket, every glance away — and each one leaves the shell claimed by a phone that is no longer there. A desk window that wants that shell is locked out for the ARQ's minutes, and the pty stays at the phone's dimensions. The tailnet route releases it properly because a socket closing is a `Detach` there; on this route a transport closing is silence.
- Proposed fix (NOT applied): make the release path call `releaseShell()` (best effort, bounded, before `conn.close()`), so the same `Detach` the launcher already sends goes out on a release; if the queue cannot be pumped in the grace window, log one line saying the shell was released ungracefully so the host-side silence is explainable.
- Open questions: whether the host treats the 45 s release as a claim expiry on its own (`claims.json`, 60 s restore grace) so the desk lockout is shorter than the ARQ timeout in practice; whether `releaseShell()`'s `pump()` can complete inside the release callback or has to be queued behind the closing coroutine.
- Investigator: field round · 2026-11-11

### A-525 · field: log lines are written to disk from the library's receive callback, on the thread that also receives frames · question

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/mesh/AndroidRadioLink.kt:508-541` (`override fun onRawData(frame: RawDataFrame)` — a callback of the vendored transport — containing `appLog("mesh radio inbox full, dropping one raw record")` at `:540`), `:604` (the same for flooded records), `:656-660`, `:690` (the scope line, also in a callback), `:137-138` (`inbox = Channel<ByteArray>(INBOX_CAPACITY)`, `INBOX_CAPACITY = 128`), `androidMain/kotlin/io/ldmux/mobile/AppLog.android.kt:14-21` (`appLog` → `AppLogFile.append`), `AppLogFile.kt:39-46` (`appendText` — open, write, close, every line).
- Finding: some log lines are emitted from inside the transport's receive callback, and each one performs a blocking open-write-close on the app's private files. The inbox in front of the session layer is 128 records deep and the code already anticipates filling it (`"mesh radio inbox full, dropping one raw record"`), so a slow callback is not a hypothetical cost — it is the mechanism that would produce those drops. Nothing measures how long that write takes on this device, which is why this is graded a question rather than a defect.
- Why it hurts at range: the receive callback is where the host's answer to a handshake or an attach arrives. At range those answers are the scarce thing in the whole system (one 133-byte record, three copies, a 120 s window). A write that takes milliseconds is irrelevant; a write that stalls behind a busy filesystem during a burst is exactly the "the answer never came" shape that the ledger has spent fourteen rounds explaining.
- Proposed fix (NOT applied): move the file write off the callback thread — a single-writer queue drained by one dedicated coroutine (`Dispatchers.IO`), with a bounded buffer and a drop counter — so the callback does an in-memory append and nothing else. That is a small change to `AppLogFile` and it also makes A-521's rotation cheap to do off-thread.
- Open questions: measured duration of `appendText` on this device (a `StrictMode` disk-write violation log or a 10,000-append timing would answer it); whether the transport already marshals callbacks to a private executor whose latency is the real bound.
- Investigator: field round · 2026-11-11

### A-526 · field: the durable log's stamps carry no year and no zone · minor

- Evidence: `packages/mobile/composeApp/src/androidMain/kotlin/io/ldmux/mobile/AppLogFile.kt:33` (`SimpleDateFormat("MM-dd HH:mm:ss.SSS", Locale.US)`), `:41` (`line = "${stamp.format(Date())}  $message"`), and A-522 (the wall clock it formats can jump).
- Finding: a line says `11-10 22:41:07.120` with no year and no timezone, formatted by the default `Locale.US` pattern against the device's current zone. A log kept across a trip, or across a clock correction, cannot be ordered against the host's journal without assuming both clocks agreed — which is the assumption A-26, A-27 and A-522 all show to be false at range.
- Why it hurts at range: every investigation in this project has been a correlation between the host's journal and the phone's file. The host stamps with its own zone and year; the phone does not. When the two disagree about the hour — which is the after-hours failure this round is about — the phone's lines can only be placed relative to each other, and a clock jump makes even that wrong.
- Proposed fix (NOT applied): format with the year and an explicit offset (`yyyy-MM-dd'T'HH:mm:ss.SSSZ`), and keep a monotonic counter beside it per process so a reader can tell a clock jump from a real gap.
- Open questions: whether anything on the phone already records the boot id or a monotonic origin that could serve as the second column.
- Investigator: field round · 2026-11-11

---

**What is sound, for the record.** The foreground service itself is the right mechanism and its notification is deliberately the quietest the platform allows (`BackgroundSession.android.kt:160-200`, `IMPORTANCE_MIN` at `:176`, no sound, no badge, no icon) — that is the correct reading of the platform requirement. The in-memory buffer is properly bounded (`AppLog.kt:51`, `CAPACITY = 300`) so a chatty caller shortens history and cannot grow memory. The driver's timers are monotonic (`Radio.kt:630`), which is why ages computed inside it are safe. The airtime window guards against a clock step (`AndroidRadioLink.kt:1716`) and is persisted per radio address so a reopened page cannot read a fresh hour out of an old spend. The lease, the release grace and the give-up are all *bounded* — the defects here are that the bounds are unreachable or misplaced (A-203, A-520, A-524), not that they are missing.

**Summary**

1. **A-520 (blocker):** the partial wake lock has a ten-minute timeout and is renewed only when the service's text changes, so a live link in a pocket loses its CPU after ten minutes — the most likely cause of the measured 34-minute log silence, and a defect in the half of the problem A-201 did not address.
2. **A-521:** the durable log holds about **one hour** at range (~1,500 lines an hour, 95 bytes a line, trimmed to 128 KiB of a 256 KiB file) and is densest during the retries that follow a failure, so a trip keeps the last hour rather than the story.
3. **A-522:** link freshness and send floors subtract from the wall clock with no negative guard (the airtime meter has one, nothing else does), so one backward clock correction after hours offline makes a dead route read fresh and stops the link asking or announcing — while the driver, which is monotonic, keeps retransmitting into it.
4. **A-524** is the release checking out without telling the host: the off-screen close sends no `Detach`, so the shell stays claimed and the pty stays at the phone's width until the host's ARQ gives up, which is minutes at range. **A-525** could still be hiding an unanswered answer: some log lines are written to disk from the transport's own receive callback, and the inbox in front of the session is 128 deep with a drop message that already exists.
5. **Not determined:** the measured battery of this link (needs one hour of `batterystats` with the screen off), the real log line rate over a day at range, and whether the transport's callback latency is dominated by our disk write or by its own executor — all three are one device test each and would settle A-520, A-521, A-523 and A-525 together.
### A-500 · tailnet-as-control: the radio route drops every shell claim the instant a session ends, where the tailnet waits a grace sized from the client's own reconnect · blocker
- Evidence: `crates/daemon/src/mesh_bridge.rs:964-972` (`tick_once`: `if !self.link.established() { self.drop_all().await; return; }`, and the same on any `generation` change), `:1303-1328` (`drop_all` releases each shell's claim and restores the pre-phone grid); `crates/daemon/src/daemon.rs:90-100` (`RECONNECT_GRACE_MS = 15_000`, "Sized against the client's own reconnect policy, not against impatience: when the phone's link drops it spends `NETWORK_PROBE_MS` (2.5 s) … then `RETRY_BASE_MS` (400 ms, plus jitter)"); `crates/daemon/src/remote.rs:1334` (the tailnet releases a device's claims only when the grace expires); `crates/daemon/src/mesh_bridge.rs:24` ("**A new handshake drops every attach.** … nothing carries over a session that ended"); phone side `MeshTermDriver.kt:2596-2623` (ledger A-28: the radio's own ladder budget is `rung × 7 + max(rung, 120_000)`).
- Finding: the tailnet holds a claim for 15 s after the connection dies, and that number is derived from how long the phone takes to come back. The radio route releases claims in the same tick the session stops being established, and its reconnect is not 2.5 s plus 400 ms but a handshake ladder of **≥176 s at a neighbour and ≈372 s at 14 hops** (ledger A-28). The comment at `mesh_bridge.rs:1300-1302` states the intent honestly ("a claim left behind would lock a desk window out of a shell nobody is watching"), but "nobody is watching" is false for the first several minutes of a radio rebuild: the phone is watching, and it is holding on.
- Why it hurts at range: at range `!established` is the *normal* state for minutes at a time, and each of those windows hands every attached shell back to the claim table. Any desk window, `ldmux attach`, or waiting window can take it, so the phone's return is a takeover in the ADR-0010 sense — it displaces a desk the operator may be using, and if it cannot, it reports "held by desktop" for a shell it never let go of. This is the same failure `RECONNECT_GRACE_MS` was introduced to fix for the tailnet (its own comment records the bug: "a dropped link became a real handover to a waiting desk window, and the phone reported a takeover nobody had made").
- Proposed fix (NOT applied): hold claims across a `generation` change for a grace derived from the radio's own ladder, and keep releasing at once on a **`Detach`** (a deliberate act) as today. The quantity already exists phone-side (`handshakeWaitMs`) and the daemon would need it stated in the wire vocabulary the way hop caps already are — the smallest correct shape is a `Hold { on }`-style hint or a per-connection grace the `Attach` carries.
- Open questions: whether the phone can tell the host "I am still here, rebuilding" cheaply (a one-record `Attach` intent) so the grace need not be guessed; whether a desk window taking a shell during a radio rebuild should be refused outright rather than allowed by ADR-0010.
- Investigator: tailnet round · 2026-11-11

### A-501 · tailnet-as-control: input to an unattached channel is silently discarded on the radio, where the tailnet names the refusal · major
- Evidence: `crates/daemon/src/mesh_bridge.rs:1816-1820` (`Message::Input` → `let Some(session) = … else { return; }`), `:1868-1871` (`Message::Key` → `let Some(a) = … else { return; }`), `:1900-1903` (`Message::Resize` → the same); `crates/daemon/src/remote.rs:1240` (`send_error("hello_required", …)`), `:1671` (`send_error("not_attached", "attach before typing")`), `:1695` (`"attach before sending keys"`); `crates/protocol/src/mesh_term.rs:336` (the `Message` enum has no error variant), `:273-286` (`closed::` carries six reasons only).
- Finding: the tailnet has a refusal for every way a write can be wrong (`not_attached`, `hello_required`, `not_holder`, `bad_key`, `input_failed`, `handler_error`, …) and the phone logs it (`AttachClient.kt:617-640`, `page …: error not_attached`). The radio bridge returns from the handler with nothing said: no reply, no `Closed`, and no counter — `stats` has `held_writes`, `resizes_unchanged`, `inputs`, `keys` but no field for "input for a channel that has no attach".
- Why it hurts at range: A-500 clears `self.shells` on every session end while the phone still holds the channel, so the first thing the operator does after a rebuild — type — lands in exactly this branch. It vanishes with no message, and with A-160 (no local echo in live mode) and A-280 (the screen inert for minutes) the operator has three different reasons for "nothing happened" and no way to tell them apart.
- Proposed fix (NOT applied): count the drop (a `drops_unattached` field beside `stats.held_writes`) and answer on the shell channel with `Closed { reason: closed::NOT_FOUND }` or a new `closed::NOT_ATTACHED`, so the phone can say "re-attach" instead of drawing an unchanged screen. Both cost one record and only fire when something is already wrong.
- Open questions: whether the phone should treat `NOT_ATTACHED` as "retry the attach" or as "refresh the list"; whether the same silence exists on the `MouseClick`/`Scrollback` paths (they return early for a missing decoder too).
- Investigator: tailnet round · 2026-11-11

### A-502 · tailnet-as-control: the radio's held-screen pause and its 30 s grace are unreachable, so every drop lands on the shell list · blocker (UI)
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:1017-1018` (`val holding = !terminalLive && !showList && resumeShell.isNotEmpty() && heldScreen != null && (view is LinkDown || Handshaking || Idle)`), `:256` (`var resumeShell by remember(plan) { mutableStateOf("") }`), with `resumeShell` assigned only `""` at `:342`, `:914`, `:974` (ledger A-420); `:1053-1057` (the only user of `MESH_FAILURE_GRACE_MS`: `LaunchedEffect(holding) { if (!holding) return; delay(MESH_FAILURE_GRACE_MS); if (holding) openList() }`), `:168` (the constant, with its own rationale at `:162-167`); `:1032` (`val needList = !terminalLive && !holding && !conn.driver.attaching`), `:1033-1038` (`repeat(LAUNCHER_TRIES) { conn.driver.launcher() … }`).
- Finding: `holding` cannot be true, because it requires `resumeShell.isNotEmpty()` and that value is never assigned. So the "pause drawn as a pause" feature and the thirty-second grace built for it are both dead, and the live condition is `needList` — which is true the instant the terminal stops being live and no attach is in flight. The screen therefore asks the host for the shell list immediately on every drop, at range where every drop is followed by a multi-minute rebuild.
- Why it hurts at range: the screen the operator was reading disappears, the shell is released (`launcher()` queues `Detach`, `:1441`), and the list request is made on a link that has just been rebuilt or is still rebuilding — the four sequential round trips of A-426, priced in minutes. Ledger A-420 records the dead `resumeShell`; this is its UI consequence, and the two together are the whole of "it drops me back to the list".
- Proposed fix (NOT applied): assign `resumeShell` from the shell the driver actually attached (`attachedId`/`markPage` already know it) so the pause and the grace become live, or delete `holding`, `MESH_FAILURE_GRACE_MS` and their comments and state that the list is the answer. Leaving a carefully justified constant unreachable is worse than either.
- Open questions: whether `needList` should also be gated on "an attach has been attempted since the last live state", so a rebuild does not race the list request.
- Investigator: tailnet round · 2026-11-11

### A-503 · tailnet-as-control: the tailnet turns backpressure into a protocol answer, the radio builds records until its window fills · major
- Evidence: `crates/daemon/src/remote.rs:209` (`WS_BACKLOG_BYTES = 64 * 1024`, "answer `resync` instead of building a history the client cannot show"), `:1042-1046` (`Out::Resync` → in-place resync when the client advertised `inplaceResync`, else `send_error("resync", …)`), `:373-379` (`resyncing`, `resync_task` — the resync path is single-flight), `:152-153` (`MAX_PENDING_BYTES = 1 MiB` bound before a resync); radio: `crates/daemon/src/mesh_bridge.rs:12-16` ("**Keep the pipe full, never build a backlog.** … the bound is the session's own window ([`PENDING_LIMIT`]), past which the queue would only grow"), ledger A-102/A-108 (eight records handed over at once, 1 ms between receives).
- Finding: on the tailnet the queue depth *is* a signal, and crossing it changes the protocol — the host stops sending deltas and answers with a fresh snapshot instead. On the radio the queue depth is a *credit count in records*, which says nothing about whether the phone can draw them; when it is reached the bridge keeps filling the session with deltas that are already stale, and the ARQ then has to carry all of them across a 76-86 s round trip.
- Why it hurts at range: a delta written for a screen the phone has since scrolled past is worth exactly as much as a full frame, and costs the same airtime; the tailnet's rule (drop the differ, owe one frame) turns k stale records into one. The machinery to do it on the radio already exists in the same file (`owe_frame`, `defer_resync`, `owe_frame_after_reset`, `:557-600`), and nothing triggers it on queue depth.
- Proposed fix (NOT applied): a `BACKLOG_RECORDS` threshold in `mesh_bridge.rs` — when a shell's unsent records exceed it, clear that shell's differ and set `owe_frame`, exactly as `defer_resync` does for a refusal. The threshold should be stated as a number of records at the link's airtime, not a byte count, since one record is one unit of air.
- Open questions: whether `PENDING_LIMIT` is the right bound at all at 16 hops, or whether a smaller bound (fewer, fresher records) is strictly better there.
- Investigator: tailnet round · 2026-11-11

### A-504 · tailnet-as-control: the tailnet's stall deadline is progress-based; the radio's retransmission deadline is a flat multiple of the hop count · major (mechanism to copy)
- Evidence: `crates/daemon/src/remote.rs:176-207` (`Drained`: "Without movement, not elapsed: a slow link that is draining is not stuck. … a fixed allowance turned \"slow\" into \"disconnected\" … Progress extends the deadline"), `:246-253` (`WS_SEND_TIMEOUT_MS` is "a stall allowance, not a message allowance … per 64 KiB slice"); radio: `crates/session/src/arq.rs:187-191` (`rto_initial = 6 × airtime × hops`) as recorded in ledger A-80, and `arq.rs:670-683` / `:475` (the measured `smoothed_rtt_ms`, unreachable outside tests — ledger A-460).
- Finding: the tailnet already implements the rule the radio lacks: a deadline that only expires while nothing is *progressing*, re-armed by evidence. The radio computes its deadline from a hop count — which ledger A-80 and A-442 show can be a one-hop figure for a sixteen-hop chain — and has no progress term at all, although every ack is a progress event and every ack already yields an RTT sample.
- Why it hurts at range: a flat deadline is wrong in both directions at once: too short for the crossing (so the first retransmission fires at 4.6 % of the round trip, A-80) and too long once the link recovers. `Drained`'s shape fixes both with one rule, and its comment is the argument: "the only fact that distinguishes a patient wait from a wedged one is whether the backlog is falling".
- Proposed fix (NOT applied): give `crates/session/src/arq.rs` a deadline object with `Drained`'s semantics — a base window re-armed by each ack (and by each arrival), floored by the measured `srtt` — and delete the hop-count multiplication. Record the shape, not the constants, in `docs/ARCHITECTURE.md` §13 so the next constant has a rule to satisfy.
- Open questions: whether "progress" on the radio should be per-record (an ack names a sequence) or per-session (any arrival); whether a link that is retransmitting into a fading chain looks "stuck" by this rule and should (it is not progressing).
- Investigator: tailnet round · 2026-11-11

### A-505 · tailnet: the app's own link-failure grace is shorter than the app's own retry ceiling, so a live retry can be interrupted · minor (defect in its own right, both routes)
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/AppContent.kt:83-89` (`LINK_FAILURE_GRACE_MS = 8_000`, "Long enough that a wifi handover, a sleeping host's resume or a radio retransmission is a pause"), `:489-502` and `:744-752` (the two effects that wait it out and then clear `pageOrder`/show the launcher); `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/LinkTiming.kt:15-16` (`RETRY_BASE_MS = 400`, `RETRY_MAX_MS = 10_000`), `:28-29` (`reconnectDelayMs` = 0.4 s doubling to a 10 s cap).
- Finding: the app's reconnect backoff **caps at 10 s**, which is longer than the 8 s grace. A link that has been down for half a minute is retrying at 6.4-10 s intervals, so the grace can expire *between* two attempts: the page is cleared and the launcher shown while the next attempt is still to come, and when it succeeds the operator is on a screen that has forgotten which shell they were in.
- Why it hurts at range: this constant is shared by both routes, and on the radio it is far worse than on the tailnet — see A-502, where the *radio's* list request fires with no grace at all. Whichever route the operator is on, the "grace" is not derived from the app's own attempt cadence, so it cannot be right for either.
- Proposed fix (NOT applied): state the grace in terms of `reconnectDelayMs` (for instance, the pause until the *next* attempt's own deadline plus half a beat), so it is derived from the behaviour it is waiting for, exactly as the daemon's `RECONNECT_GRACE_MS` is derived from `NETWORK_PROBE_MS` + `RETRY_BASE_MS`.
- Open questions: whether the launcher should show an attempt counter so a grace that fires mid-retry is visible rather than looking like a decision.
- Investigator: tailnet round · 2026-11-11

### A-506 · tailnet-as-control: the tailnet's liveness is a beat priced against the daemon's idle window; the radio has no idle liveness at all · question
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/Heartbeat.kt:1-24` (`CLIENT_PING_INTERVAL_MS = 4_000`, stated once for both sockets because `WS_IDLE_MS` is 20 s), `:29-35` (`PRESENCE_PING_INTERVAL_MS = 60_000`), `:37-44` (`CLIENT_MISSED_PONGS = 3`), `:46-52` (`CLIENT_PING_SEND_TIMEOUT_MS = 2_000`, bounded so the beat happens at all); `crates/daemon/src/remote.rs:211` (`WS_IDLE_MS = 20_000`), `:220` (`PRESENCE_IDLE_MS = 150_000`, "two and a half beats"); radio: `packages/mobile/…/mesh/MeshTerminalScreen.kt:170-180` (`QUIET_POLL_MS = 2_000`, `QUIET_NOTICE_MS = 10_000`, "the poll is a local read, not a transmission"), ledger A-180 (the host's watchdog is refreshed by its own 500 ms status event).
- Finding: on the tailnet, liveness is a property of traffic the peer must answer, and both ends' windows are derived from the other's cadence. On the radio there is no beat: an idle session says nothing and is owed nothing, so nothing on either end can notice it has died until the operator types — and the first keystroke then pays for the discovery with one ARQ attempt over a chain whose round trip is 76-86 s.
- Why it hurts at range: "the shell is fine until I type, and then it is a two-minute silence" is a worse experience than a beat, and A-160 means the operator sees nothing while it happens. But the tailnet's numbers do **not** transfer: a 4 s beat is one record per 4 s of airtime, which at range is an order of magnitude more expensive than the traffic it protects.
- Proposed fix (NOT applied): decide the beat in airtime, not seconds — the natural candidate is the existing flood/advert cadence (one zero-body record per `floodIntervalMs`, which the mesh already measures for itself), used to age both ends' session and to fail it early rather than on first write. This belongs with A-460 so the number is measured before it is chosen.
- Open questions: whether a session can be aged from traffic the mesh already carries (adverts heard, floods heard) without adding a single transmission; what "dead session detected on first write" actually costs today in wall clock.
- Investigator: tailnet round · 2026-11-11

### A-507 · tailnet-as-control: compression is symmetric on the tailnet and one-way on the radio, and the client half of it already ships in this app · major (confirms A-140 with the mechanism)
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/WebSockets.kt:23-30` (`install(WebSocketDeflateExtension)` — the phone deflates what it *sends*, and the daemon's `WsReader` inflates it, `crates/daemon/src/ws.rs:204`), `crates/daemon/src/ws.rs:40-51` (`MIN_COMPRESS_BYTES = 96`, `DEFLATE_LEVEL = 3`, ADR-0019's measurements), `:1-34` (the module doc's compression rules); radio: `packages/mobile/…/mesh/MeshFrameDictionary.kt:4-13` (a preset dictionary already exists phone-side), ledger A-140 (1.6 KB paste: 12 records plain against 2 deflated).
- Finding: the app already carries a working deflate client and a pinned dictionary; on the radio the downlink uses both and the uplink uses neither. The tailnet proves the client half is not a research problem — it is installed in `ldmuxWebSockets()` and exercised on every page load.
- Why it hurts at range: A-140 prices the gap at 7 s of air against 1.2 s for a 1.6 KB paste, times three if the record goes by flood, and the paste blocks every keystroke behind it. This entry exists to say the missing half is a wire message, not new capability.
- Proposed fix (NOT applied): as A-140 — a phone→host framed message, feature-gated in `Attach` like every other wire change. Use the dictionary already pinned by `MeshFrameDictionary`, and `MIN_COMPRESS_BYTES`'s rule (do not compress what is smaller than the framing) so a keystroke stays one plain record.
- Open questions: whether deflate's context takeover is affordable over a radio session (a lost record corrupts the dictionary for the next one) — the tailnet's answer is per-connection takeover and TCP reliability, which the radio does not have; this may force `no_context_takeover` on the mesh, at a worse ratio.
- Investigator: tailnet round · 2026-11-11

### A-508 · tailnet-as-control: the tailnet has a refusal vocabulary and the radio has six close reasons, which is why the phone cannot say why anything failed · major
- Evidence: `crates/daemon/src/remote.rs` — `send_error` codes at `:1216` `bad_json`, `:1240` `hello_required`, `:1257` `not_holder`, `:1415` `handler_error`, `:1465` `kill_failed`, `:1516` `close_failed`, `:1585` `bad_size`, `:1590` `not_attached`, `:1598/1607` `resize_failed`, `:1614/1642` `bad_cell`, `:1628` `no_mouse`, `:1649/1677` `input_failed`, `:1706` `bad_key`, `:1117` `resync`; the client's per-code recovery and log at `AttachClient.kt:617-640`; radio: `crates/protocol/src/mesh_term.rs:273-286` (`closed::NOT_FOUND`, `AMBIGUOUS`, `EXITED`, `UNAVAILABLE`, `VERSION`, `HELD`) and no error variant in `Message` (`:336`).
- Finding: the two routes have opposite design philosophies about failure: the tailnet names it and the client acts on the name; the radio has a six-word vocabulary that is only ever sent *about a shell that is closing*. Ledger A-25 and A-220 found the symptoms (a refusal reason that lives only in the host's journal, a session death that is computed and discarded); this is the structural cause — there is nowhere on the radio wire to put a reason.
- Why it hurts at range: the durable phone log is the only diagnostic available to a range user, and today it can say "no answer" for a dead link, a refused handshake, an unattached channel and a held shell alike.
- Proposed fix (NOT applied): the smallest useful subset rather than all fifteen — `not_attached` (A-501), the handshake refusal of A-25, and a session-death reason (A-220) — as one small unsealed control record, best-effort and rate-limited, since none of the three carries a secret.
- Open questions: whether an unsealed refusal leaks anything an observer could use (a `BadFilter` refusal must stay silent, which argues for a reason code that cannot distinguish "wrong key" from "no such pairing").
- Investigator: tailnet round · 2026-11-11

### A-509 · tailnet-as-control: the tailnet attach survives a reconnect and re-attaches itself; the radio requires a fresh attach by policy · question (decide before fixing A-500/A-502)
- Evidence: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/AttachClient.kt:218-235` (`onEnded`, `onRejected`, "the loop stops: there is nothing to re-attach to"), `:300-320` (`reconnectNow`, "the way a page re-attaches after its shell became *free*"), `:463-497` (the page's own loop, ping and backoff); `crates/daemon/src/daemon.rs:90-100` (the fifteen-second grace that exists so the shell is still the phone's when it returns); `crates/daemon/src/mesh_bridge.rs:24` ("A new handshake drops every attach. The phone must `Attach` again; nothing carries over a session that ended").
- Finding: both routes have a rebuild, but only the tailnet's is invisible: the client's loop re-attaches the same shell, and the daemon keeps the claim long enough for that to be a resume rather than a takeover. The radio's rule is stated as policy ("nothing carries over a session that ended"), and at range that policy is paid for on every handshake — list, attach, profile, screen (A-426's four round trips, minutes at 14 hops).
- Why it hurts at range: the handshake is not an exceptional event there, it is the steady state. A rule written for "a session that ended" is being applied to a session that was replaced while the operator watched.
- Proposed fix (NOT applied): none until the question below is answered. If the rule is not a security property, the bridge can rebuild its shell map from the keeper's claims (`Host::holder` already answers `"mesh"`) and skip the list entirely.
- Open questions: **is dropping attaches across a re-key of the same pinned peer a security property?** Pairing pins one peer key (`crates/session/src/handshake.rs`, `PinMismatch`), so a re-handshake is the same device; if that is the argument, the map can carry over. If not, A-500's grace is still right for the claims and the attach stays fresh.
- Investigator: tailnet round · 2026-11-11

### A-510 · tailnet-as-control: the one habit that separates the two routes — every tailnet deadline is derived from the peer's own behaviour, and the radio's are not · major (rule for the next change)
- Evidence: derived — `WS_IDLE_MS` from the 4 s page ping (`crates/daemon/src/remote.rs:211` + `Heartbeat.kt:19-24`), `PRESENCE_IDLE_MS` as "two and a half beats" of the 60 s presence ping (`:220`), `RECONNECT_GRACE_MS` from `NETWORK_PROBE_MS` + `RETRY_BASE_MS` (`daemon.rs:90-100`), `MESH_FAILURE_GRACE_MS` from the driver's own verdicts (`MeshTerminalScreen.kt:162-168` — the one derived radio constant, and A-502 shows it is unreachable). Not derived — `LINK_FAILURE_GRACE_MS = 8_000` (`AppContent.kt:89`), `route_stale` 60 s against a 102 s round trip (ledger A-362), `rto_initial` from a hop count that can be wrong (A-80), the 30 s handshake verdict against a ≥176 s budget (A-28), the launcher's 120 s of retries against the same budget (`MeshTerminalScreen.kt:104-113`, `LAUNCHER_TRIES = 300` × `LAUNCHER_RETRY_MS = 400`).
- Finding: the tailnet's constants read as measurements of the other end; the radio's read as guesses that happen to be integers. This is not a stylistic observation — five of the ledger's blockers are a constant that does not match the link it governs, and the tailnet has the same class of constant written correctly, in the same repository, by the same hand.
- Why it hurts at range: at range the ratio between "the number the code assumes" and "the number the link delivers" is 4-20×, so an underived constant is not a small error, it is a wrong behaviour (a retransmission inside the crossing, a verdict inside the ladder, a grace inside a retry).
- Proposed fix (NOT applied): a rule, not a change — every radio deadline is either **derived** from a measured quantity (srtt, airtime, hop count from the peer's own traffic, the flood interval) or **named as policy** with its cost in the comment. Put it in `docs/ARCHITECTURE.md` §13 beside the existing radio invariants, and grep for bare millisecond literals in `mesh_*` when reviewing.
- Open questions: which of the underived constants can actually be derived today from what the code already knows (this is A-460's list).
- Investigator: tailnet round · 2026-11-11

### A-511 · tailnet-as-control: the radio launcher gives up asking for the shell list before the link can possibly be up · major
- Evidence: `packages/mobile/…/mesh/MeshTerminalScreen.kt:104-113` (`LAUNCHER_TRIES = 300`, `LAUNCHER_RETRY_MS = 400` — 120 s of retries, "Two minutes of retries covers a radio handshake on a slow tuple"), `:1032-1038` (the loop, and `conn.driver.launcher()` returning `false` while `session == null` — `MeshTermDriver.kt:1431-1432`), `:1435-1456` (what `launcher()` queues: `Detach` then `ListRequest`, and it re-arms `listAsks`); the budget it is trying to outlast: ledger A-28 (`handshakeWaitMs` = `rung × 7 + max(rung, 120_000)`, so ≥176 s at a neighbour and ≈372 s at 14 hops).
- Finding: the comment states the intent correctly and the number is 120 s, which is **shorter than the minimum handshake budget the driver itself enforces** (176 s at a neighbour). At a neighbour the loop expires 56 s before the ladder's last attempt is even answered; at 14 hops it expires 252 s early. After that the screen draws a state with no retry until the operator presses something, which restarts it ("went back to 0").
- Why it hurts at range: this is the mechanic behind the operator's report — a counter that climbs, stops, and then resets when they touch it. Every reset sends another `Detach` + `ListRequest` and re-arms `listAsks`, so the cure costs a record and can race the attach the rebuild is about to make (`:1029-1031` records that exact race: "keystrokes vanish with no message").
- Proposed fix (NOT applied): derive the launcher's retry window from the same budget the driver uses (`handshakeWaitMs` once a hop count exists, else its no-route figure), and stop the loop on a *state* change rather than on a count — the driver already knows whether a handshake is still pending.
- Open questions: whether the launcher should ask at all while no session exists (it is a free local read that returns false, so the cost is only the expiry), and whether `attemptNo` (the refresh square) should be the only way to restart it.
- Investigator: tailnet round · 2026-11-11
### A-540 · claims: a mesh claim is exempt from the release sweep *and* always "engaged", so it lasts exactly as long as the session — and the session never dies while idle · blocker

- Evidence: `crates/daemon/src/daemon.rs:369` (`mesh_watched: HashSet<String>`), `:2996-2999` (the sweep's first branch: `if self.mesh_watched.contains(&sid) { ever_backed.insert(..); unbacked_since.remove(&sid); continue; }` — no path to `release_claim`), `:2336` and `:2484` (the `reclaim`/`claim` verbs set it for `device.kind == "mesh"`), `:2319` (the only clear is the `release` verb); `crates/daemon/src/ownership.rs:70-72` (`if kind == "phone" || kind == "mesh" { return true }` — a mesh claim is engaged unconditionally, so `HeldBy` is returned to any other device without consent); `crates/session/src/arq.rs:108-115` and `:145` (`heartbeat_ms: 0` — "with an empty queue and nothing in flight, nobody is waiting on an answer, and the ARQ's own ladder detects a dead peer as soon as there is anything to send"); `crates/daemon/src/mesh_session.rs:471-473` (`wants_poll` is false when the outbox is empty and nothing is pending), `:483-487` (`state: "established"` **iff** `self.session.is_some()`); `crates/daemon/src/mesh_bridge.rs:963-967` (`tick_once`: `if !self.link.established() { self.drop_all().await; return; }` — the only thing that gives a mesh claim back).
- Finding: a mesh claim has no timer of any kind. `release_unbacked_claims` skips it while `mesh_watched` holds the session id, and the only code that clears `mesh_watched` for a shell is the bridge's own `release`, which runs from `drop_all` — and `drop_all` runs only when `link.established()` goes false. `established()` is true for as long as `SessionHost::session.is_some()`, and the session is dropped only when the ARQ exhausts its retries, which requires something to be outstanding. With `heartbeat_ms: 0` and an idle shell (no output, nothing owed, nothing pending) there is nothing outstanding, so the session stays "established" indefinitely, `drop_all` never runs, and the claim is held by a phone that is not there. `engaged()` then makes it indisplaceable: every other device is refused with `HeldBy { kind: "mesh" }` unless it passes `takeover`.
- Why it hurts at range: at range the phone disappears for ordinary reasons — the app is killed, the screen turns off (A-520), the route is switched (A-200), the walk ends, the battery goes — and the common case is that it disappears *without a `Detach`* (A-524: the off-screen release calls `conn.close()` and never `releaseShell()`). The shell then stays claimed by `mesh-link` on the host, a desk window cannot attach to it without takeover, and the pty stays fitted to the phone's grid. Nothing in the journal says so: the claim is not an event, and `ldmux mesh status` reports the *session*, not the claims. The duration is not "the grace", it is "until something happens to be sent", which on a quiet shell can be hours.
- Proposed fix (NOT applied): give the mesh claim the timer the other claims have. Concretely: (1) in the sweep, treat `mesh_watched` as "backed" **only while the link reports a live session whose peer has been heard within a bound** — `release_unbacked_claims` already receives `attached`; let the daemon ask the mesh runtime for `(established, last_inbound_age)` and feed an empty `attached` once that age exceeds the reconnect grace; and (2) have the bridge advertise one `heartbeat` record per idle interval equal to the crossing (a heartbeat is already exempt from the budget, `mesh_session.rs:366`), so a vanished peer is discovered by the ARQ instead of by luck. Either alone bounds the claim; both make it a lease rather than a hope.
- Open questions: whether an idle heartbeat is affordable at range (one 5-byte record per crossing is ~1 record per 15 s at 14 hops, against a screen that costs 3 records — cheap, but it is the only permanent spend in the design); whether the sweep should be driven by the mesh runtime's own silence counter rather than by a new query; and whether a claim should be released on `Closed{EXITED}` (it is not: `mesh_bridge.rs:1289-1294` removes the attach but never releases the claim).
- Investigator: claims round · 2026-11-11

### A-541 · claims: every mesh client is the same device (`mesh-link`), so no phone can ever be told a shell is another phone's · major

- Evidence: `crates/daemon/src/mesh_bridge.rs:2018` (`const MESH_DEVICE: &str = "mesh-link";`), `:2283` and `:2310` (all mesh RPCs carry that one id and kind `mesh`); `crates/daemon/src/ownership.rs:155-176` (the same `device.id` re-claiming is *idempotent, not a conflict*: it returns `Acquired` and never `HeldBy`); `crates/daemon/src/mesh_bridge.rs:1333-1347` (`refuse_held_write` guards a write by asking `holder(session)` and comparing — which, for a second phone, compares `mesh-link` with `mesh-link` and finds no displacement).
- Finding: the ownership model is per device, and the mesh has exactly one device identity for all phones, all links and all sessions. Two phones that attach the same shell both get `Acquired`, both keep a live bridge, both write to the same pty, and neither is displaced, because `displaced()` and `refuse_held_write` are answered by a holder string that is identical on both sides. The `HeldBy` consent path — the whole reason a takeover exists — cannot be reached over the radio.
- Why it hurts at range: unknown whether the operator carries two phones, but the failure is silent and it interleaves keystrokes from two devices into one shell, which is neither safe to type into nor recoverable by the user (the screen shows one grid; the other phone's writes appear in both). It also means the phone's own "take it back" affordance can never fire over the radio, so a shell held by a *stale* mesh claim (A-540) reads as taken by itself.
- Proposed fix (NOT applied): make the mesh device id per link, e.g. `format!("mesh-link:{peer_key_prefix}")` from the session's pinned peer key, keeping `kind: "mesh"` so `engaged()` and the sweep's rules still apply to it. Then two phones are two devices and the ordinary consent path works.
- Open questions: whether anything keys on the literal `"mesh-link"` (grep says only `MESH_DEVICE`, but `claims.json` from a previous daemon would carry the old string, and a rename needs a restore rule); whether a per-link id should also appear in `list`'s owner column.
- Investigator: claims round · 2026-11-11

### A-542 · claims: nothing tests the claim a vanished phone leaves behind · major

- Evidence: `crates/daemon/src/daemon.rs:3914-3935` (the sweep's tests: a claim with no connection freed after `CLAIM_GRACE_MS`, one that had a connection freed after `RECONNECT_GRACE_MS`, a device-specific one kept) — all three use ordinary device kinds; `crates/daemon/src/mesh_bridge.rs` has tests for attach, detach, hold, refusal and `drop_all`'s effects, but none that leaves a claim held across a link that stops answering while idle.
- Finding: the mesh exemption is tested only in the direction that it *keeps* a live claim (`daemon.rs` comment at `:3043` describes the oscillation it fixed). The opposite direction — the link stops but stays "established" and the claim must still expire — has no test, which is why A-540 shipped: the exemption was added for a real defect and no test asked what bounds it.
- Why it hurts at range: at range every reclaim of a shell runs through this branch, and the branch has one tested behaviour and one untested one. A test can be written with the existing seams: the sweep takes `attached` and `now` as arguments, so a test only needs a claimed shell whose id is in `mesh_watched`, `attached` empty, and a `now` past any bound it introduces.
- Proposed fix (NOT applied): once A-540 gives the claim a bound, add the test that pins it — "a claim watched by a mesh link that has not been heard for longer than the reconnect grace is released, and `list` says the shell is free" — beside the existing graces in `daemon.rs`'s test module.
- Open questions: whether the bridge should also be tested for the sequence "session dies while nothing is owed", which needs the `FakeLink` to expose a way to become unestablished without an ARQ failure (it has `established: bool`, so it does).
- Investigator: claims round · 2026-11-11

### A-543 · claims: the mesh exemption lives only in memory, so a daemon restart changes the rule from "for ever" to "sixty seconds" · major

- Evidence: `crates/daemon/src/daemon.rs:369` and `:494` (`mesh_watched` starts empty), `:453` (`RESTORE_GRACE_MS = 60_000`), `:2843-2844` and `:2883` (`load_claims` restores each claim with a deadline of `mono_ms() + restore_grace_ms`), `:2918` (the deadline is removed on any re-claim); `:3111-3112` (`release_device`, the "a dropped connection frees its claims" path, is called only from `crates/daemon/src/remote.rs:1334`, `:1551`, `:2478` — never for the mesh).
- Finding: before a restart a stale mesh claim is permanent (A-540). After a restart the same claim is restored from `claims.json` with a 60 s deadline and, because `mesh_watched` is empty in a fresh process, the ordinary sweep frees it when the deadline passes. So the same physical situation — a phone that is gone — has two different lifetimes depending on whether the daemon happened to restart in between, and the restart is not an operator action the phone can see.
- Why it hurts at range: the phone that reconnects after a restart must re-claim inside 60 s or the shell is offered to a desk window. Reconnecting at range is a handshake plus an attach, measured at 42 s with a healthy direct path and about three minutes over floods (A-426), so a restart at range can silently lose the claim it was meant to preserve. Conversely, a phone that never comes back holds the shell for ever if the daemon did *not* restart — the worse of the two behaviours is the one that needs no event.
- Proposed fix (NOT applied): restore the exemption with the claim — write the claim's device kind and a "watched" flag into `claims.json` and restore `mesh_watched` for entries that carry it, then bound it by A-540's rule. That makes the lifetime a function of the link, not of the process's age.
- Open questions: whether `claims.json`'s schema can take a flag without a version gate (it is internal state, not wire); whether 60 s should instead be derived from the route, since the reconnect it exists for is the one A-426 prices.
- Investigator: claims round · 2026-11-11

### A-544 · claims: the code comment that justifies "engaged unconditionally" cites two releases that do not happen · major

- Evidence: `crates/daemon/src/ownership.rs:61-69` — "it releases the claim when the operator leaves the page or the app (`releaseShell`, and the daemon's own `release_device` on a dropped connection), so its claim cannot outlive their attention". Against the code: `packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/MeshTerminalScreen.kt:379-383` (the off-screen path calls `conn.close()` and never `releaseShell()`, recorded as the ledger's A-524), and `release_device` is never called for the mesh (A-543's evidence).
- Finding: the only stated reason a mesh claim is treated as permanently engaged is that it is always released promptly. Neither named mechanism fires for a phone on the radio: the screen-off path does not send `Detach`, and the daemon never runs the per-device release for a mesh link. So the rule's justification is void while the rule remains, which is exactly the shape of A-540.
- Why it hurts at range: a comment that states a false invariant is how the next engineer re-derives the bug. Every one of the ledger's documentation entries (A-300…A-318) is the same class, and this one sits directly on the claim path the user feels.
- Proposed fix (NOT applied): correct the comment to what is true — a phone's claim is engaged because the phone cannot be reached to ask, and it is bounded by the release the *link* performs when its session ends (A-540 makes that bound real) — and name the missing `releaseShell` explicitly so the two halves are fixed together.
- Open questions: whether the `mesh` branch of `engaged()` should be an explicit named predicate (`is_mobile()`) so the tailnet phone and the radio phone cannot drift apart again.
- Investigator: claims round · 2026-11-11

### A-545 · claims: what a stale claim actually costs, and what it blocks, in the terms the user feels · major

- Evidence: `crates/daemon/src/ownership.rs:180-184` (`HeldBy { device }` is returned to any non-takeover claim while the holder is engaged); `crates/daemon/src/mesh_bridge.rs:1415-1418` (a refused attach answers `Closed { HELD }`); `:1972-1978` (`Detach` releases *and* calls `restore_grid`, the only thing that puts the pty back to `original`); `crates/daemon/src/daemon.rs:910-917` (the comment asserting `close_session` refuses to kill a shell somebody holds).
- Finding: a stale mesh claim blocks three different things, with three different symptoms. (1) A desk window (`ldmux attach`, or the spawned terminal) is refused the shell and must take it over: the operator sees "take it back" on a shell nobody is using. (2) The pty keeps the phone's geometry: `restore_grid` never runs, so the shell stays fitted to a screen that is not there — 46×21 from the field log — until whoever attaches next resizes it. (3) The shell cannot be closed by `close_session` while the mesh watches it, so an abandoned shell survives a cleanup. The phone itself is *not* blocked: it re-claims as `mesh-link` and gets `Acquired` idempotently (A-541).
- Why it hurts at range: the whole point of the radio path is that the operator is away from the desk, so the visible symptom is at the desk on return: shells "held by the phone" that the phone has not looked at for hours, at the wrong size, and refusing to be cleaned up. That reads as the daemon being broken, and nothing in `mesh status` mentions claims.
- Proposed fix (NOT applied): none of its own beyond A-540; but the *reporting* is worth having — `list`'s owner line for a mesh claim should carry the age of the last record from that peer, so "held by the phone" is visibly "held by a link last heard 40 minutes ago".
- Open questions: whether `close_session` refusing on a watched shell is intended or a consequence (the comment at `:913` says it is intended, but it was written when a watched shell implied a live phone).
- Investigator: claims round · 2026-11-11

### A-546 · claims: a takeover over the radio costs one exchange but repaints the whole screen, and the displaced party learns late · minor

- Evidence: `crates/daemon/src/mesh_bridge.rs:1413` (`self.host.claim(one, takeover)`), `:1429-1440` (a re-attach releases the old claim and re-claims), `:1540-1560` (the `Profile` and the marks are re-sent on every attach), `:1488` (`differ: Differ::for_attach(version, one)` — a *fresh* differ per attach, so the answer is a full frame); the ledger's A-426 (the four-exchange floor: 42 s direct, ≈3 min flooded at 14 hops) and A-320 (a full 46×38 screen is 1318 B, 10 records unframed, 3 framed).
- Finding: taking a shell over the radio does not add an exchange — the claim RPC is local to the daemon — but it does throw away the difference state: `Differ::for_attach` starts fresh, so the first paint after a takeover is a full screen (3 compressed records), and the marks page is re-sent. The displaced device is told by a `Closed`/`Sessions` push only when it next writes (`refuse_held_write`) or re-attaches (`displaced()`), never at the moment of the takeover, so it can keep writing into a shell it lost for as long as it takes its next record to be refused.
- Why it hurts at range: two records for the profile and marks plus three for the frame is ~2.5 s of air direct and ~15 s over floods, on top of the attach; and the displaced phone's next keystrokes are sent (air spent) before they are refused, so a takeover costs the loser a round trip of typed input.
- Proposed fix (NOT applied): keep the differ for a shell across a takeover when the incoming device is the same version (the phone already holds the screen), so the first paint is a diff; and push `Closed { HELD }` to the displaced attachment as soon as the claim moves rather than when it next writes.
- Open questions: whether the displaced push can be routed at all, since the bridge owns the phone's stream and the claim is taken by the daemon (it would need a daemon→bridge nudge, which the bridge's tick could poll).
- Investigator: claims round · 2026-11-11

### A-547 · claims: should a claim be tied to a session at all? (the lease question) · question

- Evidence: everything above. The three graces exist because a claim outlives evidence: `CLAIM_GRACE_MS = 3_000` (`daemon.rs:88`), `RECONNECT_GRACE_MS = 15_000` (`:100`), `RESTORE_GRACE_MS = 60_000` (`:453`), each a guess at how long a client takes to come back; `mesh_watched` (`:369`) exists because none of them fit a link that is not a connection; and `engaged()` (`ownership.rs:70`) exists because a mobile device cannot be asked.
- Finding: the claim's lifetime is derived from three things that are not the claim's own: a timer for connections, an exemption for links, and a "the device is mobile" flag. Each was added for a real defect and each is unbounded in a different direction, which is why A-540 and A-543 exist. A session lifetime is no better a bound than a connection lifetime — the session also outlives evidence while idle (A-540) — and it disappears on restart.
- Why it hurts at range: the radio is the case where the evidence is weakest (a crossing is 7–46 s, the app can vanish without a word, and the link can be up with a dead peer), so a rule that is "correct for a connection" is exactly the rule that fails here.
- Proposed fix (NOT applied): make the claim a **lease the peer renews**, not a record of a connection. The phone already sends input ("a write is the only proof of use this daemon has", `ownership.rs:213-215`); the missing half is a renewal that costs almost nothing when the operator is idle — one tiny `Alive` message per lease period, whose period is derived from the route (`max(2 × crossing, ENGAGED_MS)`) and whose renewal is piggybacked on any record already going that way. The host then has one rule for all devices: a claim is held for one lease after the last proof of life, whatever the transport. The three graces collapse into one, `mesh_watched` disappears, and `engaged()` becomes "the lease is current".
- Open questions: whether an `Alive` message is cheaper than the heartbeat A-540 proposes (they are the same record — one could serve both); what a phone that is genuinely idle-batch should send (it composes without sending, so it has no "use" to report — the lease would have to be renewed by *being connected*, which brings the problem back unless the renewal is automatic); and whether the desk clients should adopt the same lease rather than `ENGAGED_MS`.
- Investigator: claims round · 2026-11-11

### A-548 · claims: two small inconsistencies on the claim path that are worth one line each · minor

- Evidence: `crates/daemon/src/mesh_bridge.rs:1287-1296` (`if screen.dead { reply Closed{EXITED}; self.shells.remove(&shell); }` — no `release`, so a shell that exits while attached keeps the claim until something else gives it back); `crates/daemon/src/daemon.rs:3008-3011` (the restored-deadline branch clears `unbacked_since` and continues, so the unbacked clock only starts on the first sweep after the deadline — correct as written, but it means a restored claim is freed one grace later than the deadline it was given).
- Finding: (a) a shell that exits under a phone attachment is removed from the bridge's table without a release, so its claim survives the shell's death — for a mesh claim that is permanent (A-540), and the sweep's `retain` at `:2992-2993` only drops the *deadline*, not the claim; (b) a restored claim is freed one grace later than the deadline it was given, because the deadline branch clears `unbacked_since` and the clock only starts after it expires.
- Why it hurts at range: (a) an exited shell stays "held by the phone" for ever and cannot be closed by `close_session` (A-545), which is the worst case of the same defect at the smallest scale; (b) 60 s becomes 60 s + 3 s or 60 s + 15 s depending on `ever_backed`, which matters only for how fast a desk window gets a shell back after a restart — small, but it is the difference between two documented numbers.
- Proposed fix (NOT applied): release the claim on `Closed{EXITED}` in the bridge (the attach is gone, so the claim has nothing behind it), and start `unbacked_since` at the deadline rather than after it, so the restored grace means what its name says.
- Open questions: whether releasing on `EXITED` races the keeper's own session teardown, which also releases claims (the daemon's `registry` retains the session id briefly).
- Investigator: claims round · 2026-11-11
