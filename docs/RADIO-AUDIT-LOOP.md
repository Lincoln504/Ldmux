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
| 1 | physical layer: preset, SF/BW/CR, sensitivity, airtime per record | pending |
| 2 | MeshCore forwarding: flood limits, scoping, rate limits, advert caps, dedup | pending |
| 3 | handshake: IK, sizes, timestamps, replay tolerance, pairing lifecycle | pending |
| 4 | carrier selection: direct/flood evidence rules, oscillation, cost | pending |
| 5 | session: ARQ, RTO vs hops, window, fragmentation, ordering | pending |
| 6 | attach flow: list/profile answer, one-record fit, waits, UI states | pending |
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
