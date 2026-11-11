# Ledger disposition matrix — `docs/RADIO-AUDIT-LOOP.md`

Read-only audit. HEAD `138cdcd` plus one uncommitted, unvalidated parent edit (`crates/daemon/src/mesh_session.rs`,
handshake-reply cache). No edits, builds, tests, devices, services or web in this pass.

Scope: 312 `A-*` entries plus 8 `M-*` entries (320 rows). Every id appears exactly once in §3.

Purpose: tell the parent **which entries survive independent verification, which do not, which are the same defect
twice, and which are about a design it is replacing**. It is not a new findings round, and it does not choose the
carrier, the crypto or the delivery model.

---

## §1 Verification log — what I checked myself, and what came back

Everything below was read in source during this pass. Where a ledger entry's evidence is a *derived* number, I
either recomputed it from the code that produces it or marked it inference.

| # | Claim under test | Check | Result |
|---|---|---|---|
| V1 | A duplicate `msg1` builds a second session | `mesh_session.rs:211` sends **every** `KIND_MSG1` to `on_handshake`; `:378-379` advance `next_index` and increment `handshakes`; `mesh_serial.rs:2212` forwards **every** flooded record to `Event::Recv` | **Confirmed.** No content dedup anywhere on that path |
| V2 | *"MeshCore delays every flood at every repeater, so a crossing is 45–58 s"* (A-41) | `Dispatcher::calcRxDelay` is `virtual` (`Dispatcher.h:167`) and is overridden by repeater/companion/room-server; each override opens with `if (_prefs.rx_delay_base <= 0.0f) return 0;`, and `_prefs.rx_delay_base = 0.0f` is the shipped constructor value (`simple_repeater/MyMesh.cpp:594,956`; `companion_radio/MyMesh.cpp:274,934`; `simple_room_server/MyMesh.cpp:279,649`) | **Contradicted as stated.** The score-based hold is **off** on all three stock builds and is operator-configurable (`rxdelay`) |
| V3 | A flooded record of 162 bytes is deliverable | `logRxRaw` emits the `0x88` push only `if (len + 3 <= MAX_FRAME_SIZE)` with `MAX_FRAME_SIZE 176` (`MyMesh.cpp:291-292`, `BaseSerialInterface.h:5`), so **packet ≤ 173**. A flooded record packet is `1 + 1 + hops×hash + 10 + record`. Therefore `record ≤ 161 − hops×hash` (157 scoped), while `LDMUX_FLOOD_RECORD_MAX = 162` and `MeshRecordLimits.FLOOD_MAX = 162` | **Contradicted — and worse than recorded.** A 162-byte flooded record is **never received at any hop count**, including zero hops (174 + 3 = 177 > 176). No error is produced anywhere |
| V4 | *"The link runs at ~0 dB margin, so a packet is a coin toss"* (A-40) | `packetScoreInt` = `clamp((snr − threshold[sf])/10) × (1 − len/256)`, table commented *"Approximate … based on Semtech datasheets"*, used only to choose immediate-vs-delayed **receive processing** (`RadioLibWrappers.cpp:309-328`, `Dispatcher.cpp:243`) | **Unsupported as stated.** The score is a scheduling heuristic, not a delivery probability, and `score = 0` is a *design floor*, not an observation of loss |
| V5 | *"Unscoped floods are capped at 5 hops by default"* (A-60) | Shipped repeater constructor sets `flood_max 64`, `flood_max_unscoped 64`, `flood_max_advert 8` (`simple_repeater/MyMesh.cpp:970-972`); `CommonCLI`'s zeroed struct fields are overridden by those values before load | **Misattributed**, exactly as A-445 says. Note A-445's own replacement ("`CommonCLI.h:48` defaults `flood_max_unscoped = 0`") is **also wrong** as an effective default: 0 would drop every flood at hop 0, which no shipped node does |
| V6 | Flood and direct crossings differ by 6.7× (A-340/A-344) | Per-hop cost is the **retransmit** delay, not the score delay: flood `nextInt(0, 5·airtime·tx_delay_factor + 1)` with 0.5, direct `nextInt(0, 5·airtime·direct_tx_delay_factor + 1)` with 0.3 (`simple_repeater/MyMesh.cpp:597-605`) | **Contradicted.** Mean added hold ≈ 1.25×airtime (flood) vs ≈ 0.75×airtime (direct) → **≈1.3×**, not 6.7× |
| V7 | The recorded airtime table is faithful to our own code | Recomputed from `codec.rs::time_on_air_ms` (bw 62.5 kHz, SF7, cr 5, preamble 32, LDRO off at 2.048 ms/symbol): 172 B → **602.6 ms**, 174 B → **612.9 ms** | **Confirmed.** The table reproduces the code exactly (see also A-49). It omits RadioLib's `−20·IH` term, so it is ~1 symbol conservative — immaterial |
| V8 | An attach answer is many records and the ARQ expires mid-crossing | `RECORD_FLOOD_COPIES = 3` applied in a loop for flooded records (`mesh_serial.rs:1468`); route replies are a single packet (no loop) | **Confirmed**, and A-304 is right that replies differ from records |
| V9 | The host's silence watchdog cannot fire | `last_carrier = Instant::now()` on **any** `carrier.recv()` (`mesh.rs:966-967`); the radio thread publishes `Event::Status` every `STATUS_REFRESH = 500 ms` computed from counters with no I/O (`mesh_serial.rs:59,1809,2524`) | **Mechanism confirmed.** "Can *never* fire" is conditional on the longest blocking command being shorter than `max(75 s, 3×rto)`; see §7 |
| V10 | Input to an unattached shell vanishes in silence | `mesh_bridge.rs:1816-1820` returns with no reply, no counter, no state change | **Confirmed** |
| V11 | The wake lock lapses while the service keeps running | `BackgroundSession.android.kt:63` `WAKE_LOCK_MAX_MS = 10 min`; acquired only in `onStartCommand` (`:111`), released only in `onDestroy` (`:151`); start intents are gated on a stopped→running transition or a notification-text change | **Confirmed at the code level** (a fix exists only in an isolated worktree, not at HEAD) |
| V12 | A mesh claim outlives the phone | `daemon.rs:2996` skips `mesh_watched` ids in `release_unbacked_claims`; `CLAIM_GRACE_MS = 3_000` (`daemon.rs:88`); `mesh_bridge.rs:964-965` calls `drop_all` only when `!link.established()` | **Confirmed** |
| V13 | A session end drops every shell claim at once | `mesh_bridge.rs:1303` `drop_all`, reached from `:964-971` | **Confirmed** |
| V14 | **Not in the ledger:** an in-flight record that no longer fits duplicates and reorders bytes | `session.rs:259-271` splits the slot, seals the head under the **same `seq`**, and `arq.rs:763` `push_front`s the tail; `arq.poll` has already numbered every record in the batch (`arq.rs:900-922`), and the in-flight slot keeps the *original* full payload (`:913`) | **Confirmed blocker** (see N-1) |
| V15 | **Not in the ledger:** `Failure::RadioRefused` is unreachable | `arq.rs:857-863` and `:862` clear `refused_since` on **every** emission, including the one that precedes the refusal, so `refused_since + REFUSAL_DEADLINE_MS` never accumulates | **Confirmed** (N-2) |
| V16 | **Not in the ledger:** a refusal discards queued control messages | `MeshTermDriver.kt:1812-1816` calls `outgoing.clear()` on any non-`Queued` outcome, and `ListRequest`/`Create`/`Takeover`/`MakeLive`/`Detach`/`Resync`/`Hold` all ignore the return value | **Confirmed** (N-3) |
| V17 | **Not in the ledger:** the RTT sample can be zero | `arq.rs:913-921` stamps `sent_at = now_ms + slot_index × airtime`; `:672-676` then samples `now_ms.saturating_sub(sent_at)`, which is 0 for an early ack | **Confirmed mechanism** (N-4) |
| V18 | A-406 covers the receive-ceiling problem | Its evidence is right about `len + 3 ≤ 176`, but it concludes only that the *SNR reading* is missing, and grades it `minor` | **Severity error.** The whole packet, and therefore the record, is not pushed |
| V19 | The Register's rows are safe to act on | Rows 1–4 were written before A-441/442/445 and still carry the refuted premises | **Unreliable as written** (§5) |
| V20 | The ledger is append-only with one schema | Two numbering domains interleave (`A-320…A-349` appear after `A-380…A-393`), and the section header claims no code changes were made during the audit while `2ea77bb` landed inside it | **Metadata defect** (§8) |

---

## §2 Ten highest-impact confirmed groups

Ordered by how much of the field history each explains. Each is a *group*: fixing the mechanism closes every id
listed, so they must not be worked one id at a time.

### G1 — The flood carrier silently drops the records it was asked to carry (blocker)

`A-406` (+ `A-45`, `A-46`, `A-313`, `A-329`, and V3). The receive-log frame is `len + 3 ≤ 176`, so a flooded record
must be `≤ 161 − hops × hash_size`. We publish 162. **Every full-size flooded record is undeliverable, and the failure
is silent on both ends.** The field log's exact signature — `record by flood … 133 B` on a 28-hop route — is the
arithmetic closing: `161 − 28 = 133`. This is the most decision-critical finding in the whole audit and the ledger
grades it `minor`.

### G2 — Bytes owed are not bytes delivered, and the ARQ can corrupt what it does deliver (blocker)

`A-23`, `A-142`, `A-143`, `A-100`, `A-104`, `A-109`, plus N-1/N-3. Four independent post-ack discard paths
(`INBOUND_MAX` drop-oldest, the bridge hand-off queue, unattached `Input`, `outgoing.clear()`), a mid-flight split
that duplicates and reorders, and a session replacement that drops the old outbox. The user-visible shape is exactly
"typed, pressed sync, went blank".

### G3 — Path identity, proof, age and repair are one broken mechanism (blocker)

`A-80`, `A-86`, `A-360`, `A-361`, `A-362`, `A-364`, `A-365`, `A-366`, `A-367`, `A-370`, `A-421`, `A-126`, `A-125`,
`A-442`. Two facts are conflated: (a) an inbound chain exists, (b) our traffic crossed a path. `proven` is set by the
one that proves nothing (a flood, whose chain we reverse) and never by the one that does (a direct arrival). A cached
advert path is adopted with no age bound (`A-421`, field-proven: `hops=0 age=506s` adopted while the peer was 16 hops
away), and that number then sets every timer.

### G4 — The carrier is chosen from global flags, so both ends latch (blocker)

`A-120`, `A-121`, `A-128`, `A-380`, `A-381`. `directWorks` is set only by a direct arrival and cleared by any flood;
`peer_via_flood` mirrors it. Neither end sends direct until it hears direct, so neither ever hears direct. That is the
positive feedback ADR-0038 exists to reject, implemented on both ends.

### G5 — Timing is derived from the wrong quantity (major)

`A-41`, `A-42`, `A-44`, `A-47`, `A-62`, `A-63`, `A-65`, `A-101`, `A-105`, `A-163`, `A-164`, `A-165`, `A-341`,
`A-344`, `A-510`. Three distinct errors: the per-hop cost is the random retransmit delay (V6) not the score hold
(V2); the interval that matters is flight time `hops × airtime`, not one airtime (`A-163`, which the bridge cannot
compute because it never learns the hop count); and the source of truth is the firmware's own
`500 + (6a + 250)(h+1)` / `500 + 16a` budgets plus the ARQ's measured `srtt`, which exists and is unreachable
(`A-460`).

### G6 — The handshake is not idempotent, so duplicate copies and replays both misfire (blocker)

`A-20`, `A-21`, `A-22`, `A-24`, `A-240`, `A-241`, `A-246`. Three copies of one `msg1` build three sessions with three
indices; a captured `msg1` is an unauthenticated session-kill plus a transmission amplifier; the phone consumes *any*
record as `msg2`. The parent's in-progress reply cache addresses the first and third, not the second (see §6).

### G7 — Claims and lifecycle are tied to liveness (major)

`A-500`, `A-540`, `A-541`, `A-542`, `A-543`, `A-544`, `A-545`, `A-546`, `A-548`, `A-201`, `A-202`, `A-205`,
`A-206`. A claim is released by the session ending, and a mesh session does not end while idle, so a quiet shell
reads "held by the phone" for hours; every mesh client is the same device id, so two phones share one shell silently.

### G8 — The host blocks its own receive path, and cannot tell a wedged radio from a busy mesh (major)

`A-61`, `A-180`, `A-181`, `A-187`, `A-188`, `A-190`, `A-400`, `A-401`, `A-403`, `A-407`. Sleeping on the reader while
answering (up to ~19 s), a self-generated heartbeat that defeats the silence watchdog, no resynchronisation inside a
frame body, and one timeout sentence for four different faults.

### G9 — The UI states facts it does not have (major)

`A-160`, `A-161`, `A-162`, `A-280`, `A-282`, `A-283`, `A-284`, `A-501`, `A-220`, `A-423`. Three boundaries are
collapsed into "the sync worked": *drained* (local), *executed* (unknown to the phone), *answered* (`Commit{idle}`,
which does exist). The verdict at 30 s replaces the only moving evidence for the following 85 % of the attempt, and a
keystroke is invisible until the host echoes it.

### G10 — Nothing in the system can be measured, so no fix can be judged (major)

`A-460`…`A-469`, `A-232`, `A-467`, `A-390`, `A-225`, `A-229`. `srtt` is computed and exported nowhere; the radio's
own airtime counter is printed but never differenced; the hop count that sets every timer changes with no record;
rate-limited logs discard their suppressed counts. Without this, the next round of changes repeats the last two weeks.

---

*Matrix, new findings, open decisions and source-quality notes follow in the next sections.*
## §3 Disposition matrix

Dispositions: **confirmed** (source supports the stated mechanism) · **refuted** (source contradicts it) · **conditional** (true only under a configuration or premise the entry did not state) · **duplicate** (same defect as another id) · **question** (unresolved, keep as a question) · **superseded** (about a design being replaced, not a defect) · **sound** (verified correct — retain).

Stage codes: **S0** instrument · **S1** identity/envelope · **S2** uplink transactions · **S3** downlink snapshots · **S4** carrier and record budget · **S5** claims and lifecycle · **S6** per-shell state · **S7** UX truthfulness · **S8** docs and retirement.

| disposition | count |
|---|---|
| confirmed | 236 |
| question | 24 |
| conditional | 23 |
| duplicate | 20 |
| sound | 6 |
| superseded | 6 |
| refuted | 4 |
| needs-verification | 1 |

| id | disposition | impact | reason / correction | stage |
|---|---|---|---|---|
| | | | **C1 · handshake identity, idempotency and replay** | |
| A-20 | confirmed | blocker | V1: every KIND_MSG1 accepted; next_index advanced; no content dedup | S1 |
| A-21 | confirmed | blocker | phone consumes any record as msg2 before checking kind | S1 |
| A-22 | confirmed | major | outbox popped once; no reply retransmission of its own | S1 |
| A-23 | confirmed | blocker | replacement drops Session + outbox.clear(); owed bytes lost (see G2) | S1 |
| A-24 | needs-verification | conditional | A-449: the supporting measurement is a different packet size; restate before acting | S4 |
| A-25 | confirmed | major | refusal reasons exist only in host last_error; phone sees nothing | S1 |
| A-26 | conditional | minor | ±5 min window is intended; 'no recovery path' depends on phone clock being right | S1 |
| A-27 | duplicate | none | A-241: the floor is unreachable anyway, so its persistence has no effect | S1 |
| A-28 | confirmed | major | verdict at 30 s, ladder continues to 176-372 s (see G9) | S7 |
| A-29 | duplicate | none | same defect as A-62/A-65: copy spacing vs forward window | S4 |
| A-30 | confirmed | minor | two counters and one sentence disagree | S7 |
| A-31 | confirmed | minor | two fsyncs before the answer is queued, per copy | S1 |
| A-32 | question | minor | one pairing per host: design question, folded into S1 identity work | S1 |
| A-33 | question | minor | 1-byte index reused every 255 handshakes: harmless while replacement is rare | S1 |
| A-34 | sound | none | rung cost arithmetic: retain as reference; reproduces codec.rs exactly | S0 |
| | | | **C5 · physical layer and timing derivation** | |
| A-40 | refuted | question | V4: score is a scheduling heuristic, not a loss probability; '0 dB margin' is inference | S0 |
| A-41 | conditional | major | V2: score hold is OFF by default on all stock builds; true only if operator sets rxdelay | S4 |
| A-42 | conditional | minor | score length-penalty is approximate; the *conclusion* (short records are cheaper) is sound by airtime | S4 |
| A-43 | confirmed | major | SNR parsed into a single variable, never used or displayed (dup A-462) | S0 |
| A-44 | duplicate | none | same mechanism as A-40/A-41: score 0 means maximum delay, and delay is off by default | S0 |
| A-45 | confirmed | minor | airtime reference 172 B vs 174 B on air | S4 |
| A-46 | confirmed | minor | airtime meter over-counts 1-3 bytes | S4 |
| A-47 | duplicate | none | reply delay ignores score: same as A-41 | S4 |
| A-48 | duplicate | none | radio tx airtime parsed, never compared (dup A-232/A-464) | S0 |
| A-49 | sound | none | verified: the two ldmux airtime implementations agree; table reproduces codec.rs | S0 |
| A-50 | conditional | minor | 24 % penalty follows from the verified airtime formula; 'at the margin' framing depends on A-41 | S4 |
| A-51 | question | minor | direction asymmetry unmeasured; needs the per-exchange counter (S0) | S0 |
| | | | **C6 · MeshCore conformance and repeater etiquette** | |
| A-60 | refuted | minor | V5: shipped defaults are 64/64/8; '5 hops' is mcrs. Conditional portability risk only | S4 |
| A-61 | confirmed | major | host sleeps/answers on the reader thread; handshakes arriving then are unread | S3 |
| A-62 | confirmed | major | 8 s rung still shorter than a 14-hop crossing under V6 (~17 s) | S4 |
| A-63 | confirmed | minor | 4 s route wait < crossing | S4 |
| A-64 | conditional | minor | '~84 repeater transmissions' is unmeasured fan-out; needs S0 | S4 |
| A-65 | confirmed | major | 400 ms copy spacing is inside every repeater's retransmit window (V6) | S4 |
| A-66 | conditional | minor | env-pinned scope can desync adverts from records | S4 |
| A-67 | conditional | minor | 1-byte hashes + loop detect: real, configuration-dependent | S4 |
| A-68 | confirmed | minor | payload version bits never checked; defensive only | S4 |
| A-69 | sound | none | reply scope and airtime arithmetic match the reference: retain | S4 |
| A-70 | confirmed | minor | one of two deadlines knows about listen-before-talk | S4 |
| | | | **C7 · session, ARQ and delivery integrity (incl. attach)** | |
| A-80 | confirmed | blocker | timer priced from path hops that A-421 can set to 0 on a 16-hop link | S4 |
| A-81 | conditional | major | A-443/A-444: base factor exists but the arithmetic does not close; restate | S4 |
| A-82 | confirmed | major | simulated air is ~5x faster than real air; timing regressions pass tests | S0 |
| A-83 | confirmed | major | window 8 far below bandwidth-delay product at range | S3 |
| A-84 | confirmed | major | loss recovery serialised, one record per timeout | S3 |
| A-85 | confirmed | major | phone refuses route refresh while anything is pending, which is always | S4 |
| A-86 | duplicate | none | flood chain treated as timing distance: same as A-80/A-360 | S4 |
| A-87 | confirmed | minor | zero-hop path publishes hops=0 then clamps | S4 |
| A-88 | conditional | minor | retransmissions exempt from budget: design choice with a real overspend | S4 |
| A-89 | duplicate | none | refusal rewinds attempt: same cluster as N-2 | S4 |
| A-90 | duplicate | none | 3 copies = 1 ARQ transmission: same accounting defect as A-103/A-183 | S0 |
| A-91 | sound | none | the parts that are correct: retain, do not re-litigate | S0 |
| A-100 | confirmed | major | pages buffer survives session death; runs merge (dup A-323) | S6 |
| A-101 | confirmed | major | list/attach deadlines below the route's own retransmission timer | S3 |
| A-102 | confirmed | blocker | 3 copies x credit window deafens a half-duplex sender | S3 |
| A-103 | duplicate | none | airtime meter counts 1 for 3 copies (same as A-90/A-183) | S0 |
| A-104 | confirmed | major | asks spent, counter never resets, screen waits for ever | S7 |
| A-105 | duplicate | none | profile deadline below one round trip: same as A-101 | S3 |
| A-106 | confirmed | minor | retried attach makes host release and re-claim | S3 |
| A-107 | confirmed | minor | run beginning during listing assumed to answer the list | S6 |
| A-108 | confirmed | major | attach cost arithmetic: recompute with the G1 limit, not 162 | S3 |
| A-109 | confirmed | major | queued attach arms a retry the queue refused; extend with N-3 (outgoing.clear) | S2 |
| A-110 | question | minor | radio attach never exercised end to end: needs a harness (S0) | S0 |
| | | | **C3 · carrier selection and pathless behaviour** | |
| A-120 | confirmed | blocker | V: directWorks set only by direct arrival, cleared by any flood; peer_via_flood mirrors | S4 |
| A-121 | confirmed | minor | session gate removed by ce0e600; comment still claims it | S8 |
| A-122 | conditional | major | 'one record per ~58 s' follows from flood pacing; host bucket absence confirmed (A-302) | S4 |
| A-123 | confirmed | major | 3 copies back to back buy no time diversity | S4 |
| A-124 | conditional | minor | records ARE re-sealed with a fresh nonce; identical-bytes resend is the route request path | S4 |
| A-125 | confirmed | major | adopting a route commits to direct for the stale window | S4 |
| A-126 | confirmed | major | path churn: field-proven 1<->16 hops in 11 s | S4 |
| A-127 | duplicate | none | hardcoded 603 ms airtime (same as A-191) | S4 |
| A-128 | confirmed | minor | heardByFlood written, never read | S4 |
| A-129 | confirmed | major | carrier decisions logged at most once a minute | S0 |
| A-130 | conditional | minor | directWorks survives reconnects (link cached per address) | S4 |
| A-131 | conditional | minor | aggregate airtime arithmetic: recompute with G1 limits and V6 | S0 |
| | | | **batch · uplink framing and drain semantics** | |
| A-140 | confirmed | major | downlink compressed, uplink not: 1.6 KB paste = 12 records vs 2 | S2 |
| A-141 | confirmed | major | frame atomic across ticks: a big screen paints nothing until the last tick | S3 |
| A-142 | confirmed | blocker | paste has no integrity boundary; half-applied command on session death | S2 |
| A-143 | confirmed | major | 64 KiB paste queue unrelated to link capacity | S2 |
| A-144 | confirmed | minor | dictionary is one developer's vocabulary, home dir included | S3 |
| A-145 | confirmed | minor | MAX_SEGMENT counts bytes, wide glyphs fragment | S3 |
| A-146 | question | minor | burst pacing vs drain rate: mismatch largely absorbed | S0 |
| A-147 | confirmed | minor | MAX_BODY caps a frame at 5 records; big frames split and re-pay headers | S3 |
| A-148 | question | minor | no on-wire cost signal; superseded by an explicit snapshot cost field | S3 |
| A-149 | confirmed | major | HashMap order + break on window fills starves shells | S6 |
| | | | **live · interactive typing at range** | |
| A-160 | confirmed | blocker | no local echo in live mode; keystroke invisible until host echoes | S7 |
| A-161 | confirmed | major | no per-keystroke state; srtt measured but exported nowhere (V: A-460) | S0 |
| A-162 | confirmed | major | quiet notice restarts on every local send, i.e. on the failing activity | S7 |
| A-163 | confirmed | blocker | bridge never learns hop count; pacing and window sized for one hop | S4 |
| A-164 | conditional | minor | direction of the argument is right; the numbers rest on A-41 (refuted) | S4 |
| A-165 | confirmed | minor | typing windows in airtimes while the round trip is thousands of ms | S4 |
| A-166 | confirmed | major | a long line costs one record per 16 backspaces | S2 |
| A-167 | confirmed | minor | arrow/control key costs a record and gives no feedback | S7 |
| A-168 | confirmed | minor | the one latency number shown is 4-8x optimistic | S7 |
| A-169 | question | minor | IME composition reaches the shell keystroke by keystroke | S7 |
| A-170 | confirmed | minor | TUI declared 'large and slow' with no behaviour change | S7 |
| | | | **C12 · host scheduling, serial handling and device state** | |
| A-180 | confirmed | major | V9: Status is self-generated every 500 ms and refreshes last_carrier; 'never fires' depends on command timeouts | S0 |
| A-181 | confirmed | major | one thread serves both directions; every command trades against the reader | S3 |
| A-182 | confirmed | major | remembered peer radio key is a routing pin with no clear path | S4 |
| A-183 | duplicate | none | 3-copy flood charged as one record (same as A-90/A-103) | S0 |
| A-184 | confirmed | major | mutating TX re-sent on silence: one record can reach the air many times | S3 |
| A-185 | confirmed | major | send path has no backpressure; the bounding timeout is dead code | S3 |
| A-186 | confirmed | major | host restart silent to the phone; a whole session-timeout of air is spent discovering it | S3 |
| A-187 | duplicate | none | flood answer sleeps the reader (same as A-61) | S3 |
| A-188 | confirmed | minor | parameter watch blocks the reader up to 5 s a minute | S3 |
| A-189 | confirmed | major | carrier diagnostics rate-limited to one line a minute | S0 |
| A-190 | confirmed | minor | three async methods block a tokio worker up to 10 s | S3 |
| A-191 | duplicate | none | hardcoded 603 ms airtime (same as A-127) | S4 |
| A-192 | conditional | major | the 'fresh nonce' is a clock field; unique in practice, not entropic | S4 |
| A-193 | question | minor | event channel coupling and capacity | S3 |
| A-194 | confirmed | minor | open timeout cannot cancel; a timed-out attempt leaves a thread holding the port | S3 |
| | | | **app lifecycle and transport ownership** | |
| A-200 | confirmed | blocker | route change tears the link down and restarts the ladder; nothing logs it | S5 |
| A-201 | confirmed | major | 45 s release grace is a tailnet figure; at range it costs a re-handshake | S5 |
| A-202 | superseded | none | 'ldmux never yields the radio' — user has decided ldmux keeps the radio; no lease built | S5 |
| A-203 | confirmed | major | the ten-minute give-up is unreachable; a dead link retries for ever | S5 |
| A-204 | confirmed | minor | documented backoff ceiling wrong by an order of magnitude | S8 |
| A-205 | confirmed | major | durable log cannot say which transport was live | S0 |
| A-206 | superseded | none | the two routes are mutually exclusive by construction — accepted trade, not a defect to fix now | S5 |
| A-207 | confirmed | minor | a refused lease still connects | S5 |
| A-208 | question | minor | two ldmux processes: field PIDs were sequential, not concurrent | S0 |
| A-209 | confirmed | minor | every connect pays up to 3 s waiting for the lease | S5 |
| | | | **C13 · observability and measurement** | |
| A-220 | confirmed | blocker | session death sets session=None before the only line that would report it | S0 |
| A-221 | confirmed | major | a refused handshake writes nothing; a locked-out peer looks idle | S0 |
| A-222 | confirmed | major | route_log_rare suppresses changes, not repeats | S0 |
| A-223 | confirmed | major | 'sent by flood' printed before the attempt | S0 |
| A-224 | confirmed | major | carrier decision and reasons absent from journal and status | S0 |
| A-225 | confirmed | major | retransmissions counted, never said | S0 |
| A-226 | confirmed | major | cannot distinguish no coverage from a deaf radio | S0 |
| A-227 | confirmed | major | phone logs every flooded copy and no direct send | S0 |
| A-228 | confirmed | major | phone send timeout closes the transport without a word | S0 |
| A-229 | confirmed | minor | x3 states attempt count, not copies sent | S0 |
| A-230 | confirmed | minor | lastError can be hours old and reads as current | S0 |
| A-231 | confirmed | major | the two ends' logs cannot be joined though the wire carries an id | S0 |
| A-232 | duplicate | none | no measured airtime anywhere (dup A-48/A-464) | S0 |
| A-233 | confirmed | major | failed stats read silently skipped, freezing the only live signal | S0 |
| A-234 | question | minor | per-packet view off by default and only reachable by editing a file | S0 |
| | | | **C4 · security and threat model** | |
| A-240 | confirmed | blocker | V1+V: replayed msg1 destroys the live session, fsyncs twice, queues 3 flooded replies | S1 |
| A-241 | confirmed | major | the replay floor is unreachable, so only the ±5 min window protects | S1 |
| A-242 | confirmed | major | a direct route reply is unauthenticated and can steer the host's path | S1 |
| A-243 | confirmed | major | routing peer learned from unauthenticated traffic and persisted | S1 |
| A-244 | confirmed | minor | peerKey in status means the Noise pin, not the routing peer | S0 |
| A-245 | confirmed | minor | flood wrapper unauthenticated; injected records cost one AEAD open with no rate bound | S1 |
| A-246 | confirmed | minor | msg2 has no cheap drop: two X25519 per injected 50-byte packet | S1 |
| A-247 | confirmed | minor | public-channel rationale describes a disclosure the transport prevents | S8 |
| A-248 | question | minor | refusal reasons must not become an oracle: guidance for A-25 | S1 |
| A-249 | question | minor | advert identity/location belongs to the radio; ldmux cannot state it | S0 |
| A-250 | sound | none | what is genuinely sound: retain | S1 |
| | | | **C15 · tests, harness and simulation** | |
| A-260 | confirmed | blocker | five of six blockers live where no harness can stand (private supervise, Context-bound carrier, clockless fake link) | S0 |
| A-261 | confirmed | major | simulated physics 3-5x too optimistic | S0 |
| A-262 | confirmed | blocker | the connection loop has no test at all | S0 |
| A-263 | confirmed | blocker | the phone carrier policy is referenced by no test | S0 |
| A-264 | confirmed | major | no harness counts transmissions | S0 |
| A-265 | confirmed | major | the only airtime measurement is outside the gate | S0 |
| A-266 | confirmed | major | Kotlin suite in no automated gate; no CI | S0 |
| A-267 | confirmed | major | two tests pin blockers as intended behaviour | S0 |
| A-268 | duplicate | none | the A-20 fix tested one layer below the defect (dup A-267 family) | S0 |
| A-269 | confirmed | major | no test connects route choice to the retransmission timer | S0 |
| A-270 | confirmed | minor | reply_delay and the blocking send path untested (dup A-61 family) | S0 |
| A-271 | confirmed | major | nothing tests a full attach or a replacement in flight | S0 |
| A-272 | question | minor | worth adopting from mcsim: two pieces, not the simulator | S0 |
| | | | **C10 · user-visible truthfulness (UX)** | |
| A-280 | confirmed | major | verdict deletes the only moving evidence for 83-85 % of the attempt | S7 |
| A-281 | confirmed | minor | the link-stopped warning is painted in the good-news colour | S7 |
| A-282 | confirmed | major | screen says 'direct' for a zero-hop flood | S7 |
| A-283 | confirmed | major | the one actionable number is three taps away; SNR never surfaced | S7 |
| A-284 | confirmed | major | the verdict's advice is the most expensive action available | S7 |
| A-285 | confirmed | major | three comments describe three drop behaviours; the code takes the costliest | S8 |
| A-286 | confirmed | minor | notification says 'linked' in states that are not linked | S7 |
| A-287 | confirmed | minor | nothing in the app can stop the ladder | S7 |
| | | | **C14 · documentation drift** | |
| A-300 | confirmed | major | ADR-0038 says decay on any arrival; code decays only on a direct one (same defect as A-120) | S8 |
| A-301 | confirmed | major | ADR-0037/ARCHITECTURE still describe flood-on-stale | S8 |
| A-302 | confirmed | major | ADR-0038 says a token bucket paces floods; the host has none | S8 |
| A-303 | confirmed | major | 'one reply per 5 s' is now 600 ms; no request floor exists | S8 |
| A-304 | confirmed | minor | route replies are one packet, not three (verified V8); row 6.228 closed early | S8 |
| A-305 | confirmed | major | three rows of the timing table are stale | S8 |
| A-306 | confirmed | major | §18 'Deployed' names a four-day-old commit | S8 |
| A-307 | confirmed | major | the documented per-hop delay model is wrong in both directions (V2/V6 confirm) | S8 |
| A-308 | confirmed | major | AIRTIME's first-timeout formula is a superseded design | S8 |
| A-309 | confirmed | major | DIRECT_SEND_PERHOP_FACTOR cited to the wrong file; formula loses two terms | S8 |
| A-310 | confirmed | major | a constant that does not exist, at a third of its real value | S8 |
| A-311 | confirmed | major | carriage-silence backstop documented as working; V9 says it cannot fire as described | S8 |
| A-312 | confirmed | minor | ladder cap documented as 40 s; it is 20 s | S8 |
| A-313 | confirmed | major | advertised record limit is 162; the correct figure is 161 - hops*hash (V3) | S8 |
| A-314 | confirmed | minor | ADR-0037 capacity arithmetic stale, conclusion false | S8 |
| A-315 | confirmed | major | handshake clock rule documented without its tolerance | S8 |
| A-316 | confirmed | minor | phone advert cadence documented 3 min, is 60 s | S8 |
| A-317 | confirmed | minor | MESH-RADIO-CONSIDERATIONS describes the budget and the fix it was written to ask for | S8 |
| A-318 | confirmed | major | 'asked again with fresh keys at 8/16/32 s' is not what the ladder does | S8 |
| | | | **paging, grid and multi-shell** | |
| A-320 | confirmed | major | every shell switch destroys mirror+differ; five shells = five repaints | S6 |
| A-321 | confirmed | major | switching shells throws away local scrollback | S6 |
| A-322 | confirmed | minor | launcher re-asked on every return, 3 records | S6 |
| A-323 | confirmed | major | list buffer merges runs (dup A-100) | S6 |
| A-324 | confirmed | major | list protocol has no page index; a lost page is undetectable | S3 |
| A-325 | confirmed | major | history press unanswered for a whole round trip, one word of feedback | S7 |
| A-326 | confirmed | minor | history unreachable before the first numbered commit | S6 |
| A-327 | confirmed | major | bounce via the launcher reflows the pty twice | S6 |
| A-328 | confirmed | minor | rotation discards scroll position | S6 |
| A-329 | confirmed | minor | shells page ceiling 134 B vs a 143 B record: one page per record | S3 |
| A-330 | confirmed | minor | MAX_SESSIONS x pages truncates a big host in silence | S6 |
| | | | **shape · structural alternatives (design-level, not defects)** | |
| A-340 | refuted | major | '6.7x latency / 3x air' rests on A-41 (V2/V6). The *policy* difference (flood-on-stale vs flood-on-unknown) is real | S4 |
| A-341 | refuted | none | 4.1x per-hop hold: refuted by V2; the real ratio is ~1.3x (V6) | S4 |
| A-342 | superseded | none | MeshCore's message layer as an alternative: a design option, not a defect | S4 |
| A-343 | conditional | minor | records ARE re-sealed (fresh nonce) so dedup does not swallow them; identical resends are the route-request path | S4 |
| A-344 | conditional | major | framing cost of a list is real; the flood/direct multiples are not | S3 |
| A-345 | superseded | none | single-record stateless mode: design option | S3 |
| A-346 | confirmed | minor | compression fixes no blocker and the uplink half is missing (dup A-140) | S2 |
| A-347 | superseded | none | 'poor man's mode' judgment: design option | S3 |
| A-348 | confirmed | minor | the ledger's round-trip figures disagree; V7 supplies one canonical derivation | S0 |
| A-349 | confirmed | major | path-first is not portable; the advert cap is flood_max_advert = 8 (V5), not 3 | S4 |
| | | | **C3/G3 · path identity, proof, age and repair** | |
| A-360 | confirmed | blocker | flooded record sets proven and then locks out the peer's own fresh chain (A-442 refines the framing, not the defect) | S4 |
| A-361 | confirmed | major | while the flood latch holds, the path apparatus is unreachable | S4 |
| A-362 | conditional | major | numbers rest on A-41; the ordering defect (stale window shorter than a crossing) stands | S4 |
| A-363 | confirmed | major | no repair procedure; upstream repairs in two packets | S4 |
| A-364 | confirmed | major | neither end exchanges a path; both reverse a flood chain | S4 |
| A-365 | confirmed | major | a flood from a peer that has a path means our teach was lost; we overwrite | S4 |
| A-366 | confirmed | major | phone discards its path on every connect | S4 |
| A-367 | confirmed | minor | a radio that never took the host time leaves path_stale permanently true | S4 |
| A-368 | question | minor | reversal assumes a symmetric mesh; asymmetric case undetected | S4 |
| A-369 | question | minor | burst-convergence guard shorter than the interval between bursts | S4 |
| A-370 | confirmed | major | proven never set by a direct arrival — the one-line inversion | S4 |
| | | | **reference-client mechanisms** | |
| A-380 | confirmed | blocker | reference chooses carrier per attempt; we choose per record from global flags | S4 |
| A-381 | confirmed | blocker | reference reads a flood reply as 'no route back' and teaches one; we mirror | S4 |
| A-382 | confirmed | blocker | reference dedupes after a quality hold; we have no receive-side dedup | S4 |
| A-383 | conditional | major | margin-then-hops ranking depends on the score being meaningful (V4 weakens it) | S4 |
| A-384 | confirmed | major | RESP_CODE_SENT carries the route used; we discard the byte | S4 |
| A-385 | confirmed | major | scope resolution two-level with no transient, vs a three-level chain | S4 |
| A-386 | confirmed | major | reference paces with a leaky bucket and never drops | S4 |
| A-387 | duplicate | none | reference never blocks receive on send (same mechanism as A-61) | S3 |
| A-388 | question | minor | 12 s attempt cap vs waiting out the crossing | S4 |
| A-389 | duplicate | none | reference queue protects unacked direct messages (same as A-23/A-142) | S2 |
| A-390 | confirmed | major | one PATH DIAG line per attempt vs after-the-fact occasional logging | S0 |
| A-391 | conditional | major | link margin vs demodulator limit: depends on V4 (score not a probability) | S0 |
| A-392 | confirmed | minor | out_path_len 0 vs <0 distinction | S4 |
| A-393 | sound | none | our reversals are of observed chains: the corrected rule. Retain the trap note | S4 |
| | | | **device · companion firmware as a device** | |
| A-400 | confirmed | major | half-written frame poisons the firmware parser; state survives reconnect | S3 |
| A-401 | confirmed | major | reader never resynchronises inside a body | S3 |
| A-402 | confirmed | minor | skipped-byte count written and unread | S0 |
| A-403 | confirmed | major | any byte counts as liveness, so console text suppresses the resend | S3 |
| A-404 | confirmed | major | mid-session radio reboot invisible; only part of the damage restored | S3 |
| A-405 | confirmed | major | radio clock set once at open; advert ages depend on it | S3 |
| A-406 | confirmed | blocker | SEVERITY RAISED: not merely a missing SNR reading — the whole packet is not pushed, so record <= 161 - hops*hash (V3) | S4 |
| A-407 | confirmed | major | one timeout sentence for four different faults | S0 |
| A-408 | question | minor | what the radio keeps that we do not reset | S3 |
| | | | **range-user journey** | |
| A-420 | confirmed | blocker | resumeShell never assigned; every drop lands on the launcher | S6 |
| A-421 | confirmed | blocker | cached advert path adopted with no age bound; field-proven hops=0 age=506s | S4 |
| A-422 | confirmed | major | counters that promise never to reset are reset by every rebuild | S7 |
| A-423 | confirmed | major | nothing says which transport carried a step | S0 |
| A-424 | confirmed | major | waiting out of range spends ten minutes of mesh airtime with no stop | S5 |
| A-425 | confirmed | major | grid debounce tuned to the keyboard animation, not the link | S6 |
| A-426 | confirmed | major | floor is four sequential round trips; no Register row reduces the count | S3 |
| A-427 | confirmed | minor | the line at the failing step says the opposite of what the phone does | S7 |
| A-428 | confirmed | minor | every rebuild pays for a contacts dump the terminal never uses | S3 |
| | | | **ledger self-corrections (A-440…A-453)** | |
| A-440 | conditional | major | correct that the request call site differs; 'central mechanism refuted' is overstated (both sites exist) | S8 |
| A-441 | confirmed | major | correct: Register row 1 would delete the escalation the reference performs | S8 |
| A-442 | confirmed | major | correct on proof provenance and the inverted fix; adopt with A-370 | S8 |
| A-443 | confirmed | major | correct: Register row 2's fix collides with A-81 | S8 |
| A-444 | confirmed | minor | correct: A-81's arithmetic does not close | S8 |
| A-445 | conditional | major | correct that '5 hops' is mcrs; but its own 'CommonCLI defaults 0' is also wrong (V5) | S8 |
| A-446 | confirmed | minor | correct: eight is the L2 credit, not the range figure | S8 |
| A-447 | duplicate | none | correct: A-100 and A-323 are one defect | S8 |
| A-448 | confirmed | minor | correct: A-61's fix named infrastructure that does not exist | S8 |
| A-449 | confirmed | minor | correct: A-24's measurement is a different packet size | S8 |
| A-450 | conditional | minor | partly: A-200 is a real blocker for field testing even if not for the link | S8 |
| A-451 | confirmed | minor | correct, and reinforced by V2 | S8 |
| A-452 | confirmed | minor | correct: the 0.66 s anchor is not derivable | S8 |
| A-453 | confirmed | minor | correct: A-304's claim holds for route replies only | S8 |
| | | | **measurement and instrumentation** | |
| A-460 | confirmed | major | srtt measured on every ack and reachable only from tests | S0 |
| A-461 | confirmed | major | the hop count that sets every timer changes unlogged | S0 |
| A-462 | confirmed | major | SNR kept as one variable, never a distribution (dup A-43) | S0 |
| A-463 | confirmed | major | durable log has no line at the two events that matter most | S0 |
| A-464 | confirmed | minor | CORRECTS THE LEDGER: tx_air_secs IS printed by ldmux mesh status; it is never differenced | S0 |
| A-465 | confirmed | major | frame trace has no timestamps, no tx side, no identity | S0 |
| A-466 | confirmed | major | rate-limited logging cannot be counted | S0 |
| A-467 | confirmed | major | nothing counts transmissions per record | S0 |
| A-468 | confirmed | major | the phone owns a measured-RTT policy the radio path never feeds | S0 |
| A-469 | question | minor | what a bench settles vs what needs a field trip | S0 |
| | | | **radio ownership** | |
| A-480 | confirmed | major | the radio can only be yielded by stopping the whole daemon | S5 |
| A-481 | confirmed | major | exclusive ownership exists (`flock`) but is anonymous | S5 |
| A-482 | confirmed | major | stopping closes the port with a write possibly in flight; no close command | S3 |
| A-483 | confirmed | major | a restart destroys route and answered-nonce ring; first records flood | S4 |
| A-484 | confirmed | minor | a daemon restart repairs the radio clock, confounding field data | S0 |
| A-485 | confirmed | minor | ldmux can blame 'another process' for its own lock | S0 |
| A-486 | question | minor | release/acquire costs one request slot; NOT TO BE BUILT per user decision | S5 |
| A-487 | superseded | none | which ownership option: decided (ldmux keeps the radio, no lease) | S5 |
| A-488 | confirmed | minor | a parked link must reset backoff or resume waits 30 s | S5 |
| A-489 | confirmed | minor | a handover costs a full attach | S5 |
| A-490 | question | minor | does the firmware need a reset before another owner opens it | S3 |
| | | | **tailnet-as-control** | |
| A-500 | confirmed | blocker | V13: every claim dropped the instant a session ends; tailnet waits 15 s | S5 |
| A-501 | confirmed | major | V10: input to an unattached channel discarded silently | S2 |
| A-502 | confirmed | blocker | held-screen pause unreachable because resumeShell is never assigned (dup A-420) | S6 |
| A-503 | confirmed | major | tailnet turns backpressure into a protocol answer; radio builds until the window fills | S3 |
| A-504 | confirmed | major | tailnet deadline is progress-based; radio's is a flat hop multiple | S3 |
| A-505 | confirmed | minor | the app's link-failure grace is shorter than its own retry ceiling (both routes) | S5 |
| A-506 | question | minor | tailnet liveness vs the radio's absence of idle liveness | S5 |
| A-507 | duplicate | none | compression symmetric on tailnet, one-way on radio (dup A-140) | S2 |
| A-508 | confirmed | major | tailnet has a refusal vocabulary; radio has six close reasons | S2 |
| A-509 | question | minor | tailnet re-attaches after reconnect; radio requires a fresh attach by policy | S5 |
| A-510 | confirmed | major | the rule: every tailnet deadline derives from the peer's behaviour | S0 |
| A-511 | confirmed | major | radio launcher gives up before the link can be up | S7 |
| | | | **phone as a field instrument** | |
| A-520 | confirmed | blocker | V11: ten-minute timed lock renewed only on a notification-text change | S5 |
| A-521 | confirmed | major | durable log keeps ~1 hour at range and is densest when the link is worst | S0 |
| A-522 | confirmed | major | wall-clock subtraction with no negative guard; a backward step silences repair | S0 |
| A-523 | confirmed | minor | BLE connection priority never requested | S5 |
| A-524 | confirmed | major | off-screen release closes the transport without Detach | S5 |
| A-525 | question | minor | log writes on the frame-receive callback thread | S0 |
| A-526 | confirmed | minor | durable log stamps carry no year and no zone | S0 |
| | | | **C7/G7 · claims and takeover** | |
| A-540 | confirmed | blocker | V12: mesh claim exempt from the sweep and always engaged; an idle shell is never checked | S5 |
| A-541 | confirmed | major | every mesh client is 'mesh-link', so two phones share one shell silently | S5 |
| A-542 | confirmed | major | nothing tests the claim a vanished phone leaves behind | S0 |
| A-543 | confirmed | major | the mesh exemption is in memory only; a restart changes the rule | S5 |
| A-544 | confirmed | major | the comment justifying 'engaged unconditionally' cites two releases that do not happen | S8 |
| A-545 | confirmed | major | what a stale claim blocks, in user terms | S5 |
| A-546 | confirmed | minor | takeover costs one exchange but a full repaint; loser learns late | S5 |
| A-547 | question | minor | should a claim be tied to a session at all (lease question) | S5 |
| A-548 | confirmed | minor | two small claim-path inconsistencies | S5 |
| | | | **bench radio ownership (M-1…M-8, other project)** | |
| M-1 | confirmed | none | which radios exist and which one the daemon owns: environment fact | S0 |
| M-2 | confirmed | none | USB and BLE are mutually exclusive firmware builds: constraint on any bench plan | S0 |
| M-3 | confirmed | none | the recorder that wants the port is the LoRa research project, not Meshmore | S0 |
| M-4 | confirmed | major | the bench recorder takes no flock, so two owners can interleave frames. OUTSIDE ldmux: an environment hazard, not a repo fix | S0 |
| M-5 | confirmed | none | stopping the daemon is not required; the daemon recovers by itself | S0 |
| M-6 | confirmed | none | the correct desktop-capture target is a second companion | S0 |
| M-7 | confirmed | minor | two documentation errors made this look like a Meshmore problem | S8 |
| M-8 | confirmed | none | the daemon is currently hearing the public mesh on the Heltec | S0 |

**How to read "confirmed".** It does not mean 236 findings were independently re-derived. Twenty-one *mechanisms*
were re-derived from source in this pass (§1, V1–V20). An entry that shares a verified mechanism inherits that
confidence. For the rest, I read the cited file:line where it was load-bearing for a stage decision and checked that
the claim matched what the code does; numbers that depend on the refuted timing model are marked `conditional`, not
`confirmed`. Nothing here was confirmed merely because the ledger asserted it.

---

## §4 Findings that are new, or materially re-graded

### N-1 — An in-flight record that no longer fits duplicates and reorders the byte stream (blocker, NEW)

`crates/session/src/session.rs:259-271` splits an oversized slot, seals the head **under the same `seq`**, and
`crates/session/src/arq.rs:763` `push_front`s the tail. But `arq.poll` has already numbered every record in that
batch (`arq.rs:900-922`), and the in-flight slot keeps the **original full payload** (`:913`). Three consequences,
all reachable exactly when a route grows by one repeater while a record is in flight:

1. the receiver drops the reshaped `seq` as a duplicate, then delivers the tail as *new* data → **the same bytes twice**;
2. the tail is delivered *after* records numbered later in the same poll → **scrambled text** in a paste;
3. the tail is re-queued on **every** retry of the same slot → up to four extra copies.

The file's own comment claims "same order, same bytes, one extra record", which is false once `next_seq` has advanced.
The fix belongs where the sequence number is assigned, not after `seal`. This is the one defect in the delivery layer
that can silently corrupt user input, and it is not in the ledger.

### N-2 — `Failure::RadioRefused` is unreachable (major, NEW)

`arq.rs:857-863` clears `refused_since` on every emission, and the emission always precedes the refusal, so
`refused_since + REFUSAL_DEADLINE_MS` never accumulates. A permanently wedged radio is retried at
`refused_retry_ms()` indefinitely — the exact outcome the deadline exists to prevent. This is the *mechanism* behind
several ledger symptoms (`A-89`, `A-427`, `A-407`) that were each recorded separately without it.

### N-3 — A refusal discards queued control messages (major, NEW; extends A-109)

`MeshTermDriver.kt:1812-1816` calls `outgoing.clear()` on any non-`Queued` outcome. The comment justifies this for
*input* (the pump re-offers), but `ListRequest`, `Create`, `Takeover`, `MakeLive`, `Detach`, `Resync` and `Hold` all
ignore the return value, so one queue-full silently loses a request nothing re-issues.

### N-4 — The round-trip sample can be zero (minor, NEW)

`arq.rs:913-921` stamps `sent_at = now_ms + slot_index × airtime`; `arq.rs:672-676` then samples
`now_ms.saturating_sub(sent_at)`. An ack that arrives "early" yields 0 and drags the smoothed estimate down. Harmless
for retransmission timing (both timers take a floor), but this is the number every future fix would be priced
against, so it must not read 0.

### N-5 — A-406 understates its own consequence by an order of magnitude (severity, WAS `minor`)

The entry concludes that the missing frame costs an SNR *reading*. In fact the firmware does not emit the push at
all, so the **record is never delivered**, silently, on both ends — and our published flood limit (162) exceeds the
hard ceiling (161 at zero hops) *today*. This is G1 and it should be a blocker.

### N-6 — The real per-hop cost is the retransmit delay, not the score hold (correction)

`A-41`, `A-341`, `A-344`, `A-164` and the Register's "20×" all rest on the score formula. On the shipped builds that
formula is behind a zero `rx_delay_base`, so what actually delays a packet is
`nextInt(0, 5·airtime·factor + 1)` with factor 0.5 (flood) or 0.3 (direct). At 16 hops and 490 ms that is
**≈17.6 s flooded vs ≈13.8 s direct (≈1.3×)**, not 65.6 s vs 9.8 s. Direct-first is still the right policy — for
airtime multiplicity, half-duplex contention and shared-medium responsibility — but the justification must be
restated, and every timer sized from the 6.7× figure must be re-derived.

### N-7 — The correction entry A-445 is itself partly wrong (correction to a correction)

It correctly identifies that the "5 hops" figure belongs to `mcrs`. It then substitutes "`CommonCLI.h:48` defaults
`flood_max_unscoped = 0`", which is a zero-initialised struct field that the shipped repeater constructor overwrites
with 64 (`simple_repeater/MyMesh.cpp:970-972`). A value of 0 would mean "drop every flood at hop 0", which no
shipped node does.

### N-8 — Airtime arithmetic is faithful; the 24 % figure is conditional (clarification)

`A-49` is right and I could reproduce the whole table from `codec.rs::time_on_air_ms` (V7). `A-50`'s "24 % penalty
for 143 B vs 50 B" follows from the same verified formula and is *sound as airtime*, though its "at the margin"
framing inherits A-41's problem.

---

## §5 Ledger-internal defects that affect how the parent should use it

1. **The Register (top of file) is no longer safe to act on.** Rows 1, 2 and 4 carry premises that A-441, A-442 and
   A-445 respectively refute, and rows 1 and 2 still prescribe fixes that would create new defects (deleting
   per-attempt escalation; re-pricing the RTO while colliding with A-81). Treat the Register as a *reading order*,
   not a plan.
2. **The "Numbers every later round should reuse" block is partly wrong.** The airtime table is verified (V7); the
   "2.70 s per hop at the margin" and "65.6 s flooded at 16 hops" lines are not (V2, V6).
3. **The append-only claim is false in effect.** The section header says no code changes were made during the audit,
   yet `2ea77bb` (the replay-tolerance change that created A-20 and A-240) landed inside the audit window and is
   recorded in the ledger's own entries. Any later reader will mis-date the audit boundaries.
4. **Two numbering domains interleave** (`A-320…A-349` appear after `A-380…A-393`), and per-round ids collide in
   spirit: `A-162` (live, quiet notice) and the flood-`162` constant are unrelated and easy to confuse. Recommend the
   parent reference entries by *mechanism group* (G1–G10) rather than by id.
5. **Severity is not consistently applied.** `blocker` is used for both "the link cannot work" and "the UI is wrong
   for minutes" (`A-502`), and `minor` for a defect that silently destroys records (`A-406`).

---

## §6 Design decisions the parent still owns (this audit does not make them)

| # | Decision | What the evidence settles | Evidence still missing |
|---|---|---|---|
| D1 | Cryptographic shape: keep Noise (re-handshake per process start) vs adopt a standalone AEAD keyed from the existing PSK | `snow` cannot export cipher keys, so "keys forever after one handshake" is not available without either a re-handshake or a new dependency | whether a C-free AEAD dependency is acceptable (project rules require an ADR line) |
| D2 | Delivery model: identified request/response vs ordered stream with ARQ | the four post-ack discard paths (G2) mean the stream's guarantee is not currently true; identified transactions make each retry safe *by construction* | whether a large first paint / history fetch needs the stream's ordering (it probably needs *some* fragmentation) |
| D3 | Carrier: corrected REQ-log wrapper vs native `GRP_DATA` vs both behind a feature bit | `GRP_DATA` payload is 165 B **path-independent**, against `161 − hops×hash` for the log wrapper — a real gain at range; but it needs a channel slot on both radios, shares a FIFO that evicts channel frames first, and gives no acknowledgement and no peer identity | a measurement of the corrected wrapper's ceiling on the real route; operator consent to provision a channel |
| D4 | Whether execution is allowed to be *at-most-once* rather than exactly-once | a pty write is not transactional; "exactly once across a daemon crash" needs an execution journal, which is machinery the user did not ask for | the parent's judgement on whether "at most once, and the user retries" is acceptable wording |
| D5 | Whether batch mode survives as a *mode* or becomes the only radio behaviour | the user's own record: sync mode's point was "not relying on a continuous connection"; if the envelope stops requiring one, the mode distinction may be unnecessary | whether live typing at a neighbour is worth keeping as an explicitly-labelled cheap carrier |
| D6 | Whether the phone keeps a per-shell mirror (cost: memory + staleness) or re-fetches on switch (cost: a round trip per switch) | `A-320/A-327` price the refetch at 20 records and 5 round trips for five shells | device memory budget; the user's tolerance for a switch cost |

---

## §7 Uncertainties that should not be resolved by inference

- **Whether the silence watchdog can *ever* fire** (V9). The mechanism is confirmed; the conclusion needs the
  longest blocking serial command, which I did not measure. If any command can block > 75 s, the watchdog *can*
  fire and A-180 drops to `conditional`.
- **Whether an early ack really occurs** (N-4). The arithmetic allows it; whether it happens on a real route depends
  on how fast a neighbour acknowledges relative to the poll's own airtime reservation.
- **The operator's actual repeater configuration** on the user's route — `rxdelay`, `f_txdelay`, `d_txdelay`,
  `f_max*`, scope and `denyf`. Three shipped defaults differ across upstream, `CommonCLI` zero-init and `mcrs`, and
  only an operator read settles it. **This single unknown changes every timing number in the ledger.**
- **The introduction version of `CMD_SEND_CHANNEL_DATA` (62)** and of `CMD_SEND_RAW_PACKET` (65). Both exist at
  `companion-v1.17.0` (65 confirmed against the tagged source); the clones' `library.json` says `1.10.0` and is
  stale, so the honest rule is feature-detect, not assume.
- **Whether the BLE transport carries the same 176-byte frame ceiling** as UART. The vendored constants say yes; no
  device was inspected.
- **Whether `M-4` (the bench recorder's missing `flock`) actually bites.** Both processes *can* hold the port; whether
  they did during any given wedge is unproven.
- **The measured `h_max`** on the real route. It converts directly into an honest record limit; the ledger's journal
  lines already contain the raw material (`route request … hops=N`).

---

## §8 Source-quality concerns

1. **Four tiers of evidence are blended in the ledger without labels**: upstream tagged C++, the community spec (self
   described as AI-written, "upstream wins"), machine-generated wiki pages, and third-party forks. Several entries
   cite `openhop_core` or `mcrs` as though they were the firmware (A-445 proves this for one; `A-60`, `A-381`,
   `A-386` and `A-388` carry the same risk).
2. **Numbers presented as firmware behaviour are often base-class behaviour** that the shipped derived classes
   override (`calcRxDelay` is the clearest case).
3. **`mcrs` (a Rust rewrite) and `openhop_core` (a Python rewrite) are treated as authority for policy** in the
   "shape" and "reference" rounds. They are useful *design* comparators and must not be quoted as what the radios do.
4. **The ledger's per-round investigators were not all equally rigorous**, and two rounds reached conclusions that a
   single grep refutes (A-464 for `tx_air_secs`; A-442 for A-80's premise). The append-only format preserved both the
   error and, later, the correction, which is the format working — but it means the file must be read to the end
   before anything is believed.
5. **No entry cites a measurement taken by the investigator.** Every "blocker" is an argument from code plus
   arithmetic. That is the strongest reason to make S0 (instrumentation) the first stage: the ledger is a hypothesis
   set, not a set of observations.

---

## §9 Bottom line for the parent

- **4 entries are refuted as written** (`A-40` as a loss model, `A-60`, `A-340`'s multiples, `A-341`); `A-41` is
  refuted for the shipped defaults but `conditional` because an operator can switch it on; and **1 correction is
  itself wrong** (`A-445`'s replacement default). Six entries describe a design the user is replacing
  (`A-202`, `A-206`, `A-342`, `A-345`, `A-347`, `A-487`).
- **20 entries are duplicates** of another id, and they cluster into ten mechanisms (G1–G10). Working the matrix row
  by row would fix the same thing up to six times.
- **Two blockers are not in the ledger at all**: the mid-flight split that duplicates and reorders (`N-1`) and the
  unreachable refusal deadline (`N-2`). Both are in the session layer, both are cheap to test deterministically, and
  both would have been caught by the seams `A-260` asks for.
- **One severity is materially wrong**: `A-406` is a blocker (G1), not a minor, and it is the single defect that best
  explains "the handshake works and nothing else arrives".
- The ordering the evidence supports is: **measurement (S0) → the record budget and carrier decision (S4) → delivery
  integrity (S2/S3) → claims and lifecycle (S5) → per-shell state (S6) → UX truth (S7) → docs and retirement (S8)**,
  with S0 first because nine of the ten mechanisms above are currently priced by numbers none of which were measured.
