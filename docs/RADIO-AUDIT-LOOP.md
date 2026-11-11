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

