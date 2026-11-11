# Radio path audit ledger (append-only)

Started 2026-11-10. Purpose: identify **everything** that makes the radio path fail, stall or waste
airtime at range, with evidence, so an advanced model can act on it later. **No code changes are made
during this audit** — the only file written is this one.

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
