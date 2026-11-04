# What the MeshMore-side research changes for LDMUX

- Written 2026-11-04, after the parallel research corpus at `/home/ldeen/Documents/mesh-research/`
  was re-read (its `notes/`, `notes/topics/`, and the repos cloned under `clones/`).
- **This document integrates nothing.** It records what the research says, what LDMUX already does
  about it, what it should change, and what is out of LDMUX's reach. Code changes made in the same
  pass are named where they exist; everything else is an open item.
- Method: every claim below names a locator — a file in the corpus, or a line in this repository.
  A claim without one is marked NOT EVIDENCED. The corpus's own method rules apply: a README claim
  is weak evidence even inside the repo, and a code path is strong evidence.
- The corpus was written to design MeshMore. This document takes only what touches LDMUX's mesh
  path: the L2/L3 carriage, its timers, its pacing, and its radio boundary. Media, FEC, voice,
  group-data registration, TAK bridging and the MeshMore app are out of LDMUX's scope and are
  listed at the end only so they are not mistaken for it.

## 1. The research's own summary of the constraint

`notes/topics/retries-and-session-state.md:11-19` states the governing fact plainly: every timer
constant in the IP suite was calibrated on round trips of tens of milliseconds, while one sub-GHz
LoRa frame can occupy the channel longer than a conservative TCP RTO. Its concrete example: a
25-byte payload at 125 kHz / SF12 is ~1483 ms on air, longer than a 1 s TCP RTO. Its rule: derive
every timeout from airtime × hops plus a processing allowance; never import an IP constant.

That is the frame for everything in section 2.

## 2. Facts that change or confirm LDMUX's behaviour

### 2.1 Unsalted retries are eaten by relays — LDMUX is already safe, and for a non-obvious reason

`retries-and-session-state.md:204-224` (anti-pattern 6) and `COVERAGE.md` decision D4 say every
retry's payload must be distinct, because MeshCore radios deduplicate by payload: an identical
retry is dropped by the first relay that forwarded the original, so a multi-hop transfer stalls at
the tail. Open TAK's docs measured "Got 213/215".

LDMUX satisfies this by construction, in the crypto layer rather than in the ARQ:
`crates/session/src/session.rs:111-131` seals each outgoing record on every `poll`, and
`record::seal` takes the session's incrementing `counter` as its nonce. A retransmission is sealed
again with a fresh nonce, so its ciphertext differs from the original even though the plaintext and
sequence number are identical. **This property is load-bearing and undocumented in the ARQ**: if
anyone ever caches the sealed bytes of a record to "retransmit cheaply", multi-hop delivery breaks
in a way that looks like a lost route. There is now a comment on `seal` and a test named for it;
the ARQ comment on `Slot` says the payload is the plaintext and the session re-seals.

### 2.2 A refusal is "not yet", not "lost" — implemented on the host, open on the phone

`companion-wire-layouts.md:244-256` records the binding throughput limit that appears in no protocol
document: the firmware's transmit table fills under burst sending, and meshcore-sar paces for it at
**350 ms per fragment (~2.86 packets/s)**. The firmware's code confirms the error code:
`ERR_CODE_TABLE_FULL = 3` (`meshcore-fw-alerts/examples/companion_radio/MyMesh.cpp:132`), which the
Android link already maps to `Busy` (`AndroidRadioLink.kt` `errorFor`).

What LDMUX did before this pass: counted a refusal as a transmission attempt. Five refusals — a
budget that refills, or a burst the radio's table could not take — failed the whole session and
forced a Noise handshake, which also costs records. That is anti-pattern 5 in the research's own
list (`retries-and-session-state.md:210-214`, the LoRaWAN "network ACK path shorter than the device
retry budget" failure) arriving from the other direction.

Implemented 2026-11-04: `Arq::on_refused` (`crates/session/src/arq.rs:470`) gives the attempt back
and retries after `REFUSED_RETRY_MS = 350` (`arq.rs:252`), the research's own pace; the daemon routes
`SEND_RESULT` refusals into it (`crates/daemon/src/mesh_session.rs:283`,
`crates/daemon/src/mesh.rs` in the `SendResult` arm). A refused record no longer advances the retry
ladder, and a spent budget no longer kills the session.

**Open:** the phone's side has the same defect and no equivalent hook. The C ABI
(`crates/session-ffi/src/lib.rs`) has no "this record was refused" call, and its `poll`/`take_record`
pair discards the ARQ sequence number, so Kotlin cannot name the refused record. The fix needs an
additive ABI call (peek the front record's sequence, then refuse it by number) plus the Kotlin
binding. It was not made here because the mobile build cannot be verified in this environment and an
unverified signature change is worse than a documented gap.

### 2.3 Karn's rule — already implemented, now with a reason

`retries-and-session-state.md:41-53` states that MeshCore's ACKs carry no timestamp, so a sample from
a retransmitted record is ambiguous and must be discarded. `Arq::apply_ack`
(`crates/session/src/arq.rs:430`) takes a sample only from a slot whose `attempts == 1`, which is
Karn's rule; the comment cites it. No change.

### 2.4 The first retry is shorter than the firmware's own timeout on any multi-hop path

`retries-and-session-state.md:97-114` gives the firmware's own direct timeout formula,
`500 + ((airtime × 6 + 250) × (hops + 1))`. At the deployed tuple's 138 ms airtime that is 1.6 s for
a neighbour, 2.7 s at one hop, 3.7 s at two and 4.8 s at three. LDMUX's `rto_initial_ms` is 2000
(`crates/session/src/arq.rs`, `Config::for_payload`), so on a two-hop path the ARQ retransmits about
1.7 s before the firmware itself would have given up — one duplicate transmission per expiry, paid
out of the same hourly budget.

The smoothed-RTT floor added in this pass fixes the steady state (after one ack the interval is at
least the measured round trip), but the first transmission still starts at 2 s. **Open:** derive the
initial interval from the preset and the hop count, or raise it to cover the firmware's own timeout.
The daemon does not know the hop count; the phone does (`RadioPath.encoded & 0x3F`), so the honest
fix crosses the ABI — or takes the simplest safe step, a fixed initial of ~3 s on a link that is
expected to be one hop at most. NOT DONE, because it changes measured device behaviour and needs a
device run to justify.

### 2.5 Randomised initial timeouts are the cheapest anti-collision device available

`retries-and-session-state.md:68-85` describes CoAP's `ACK_TIMEOUT` 2 s with an `ACK_RANDOM_FACTOR`
of 1.5: the initial timeout is drawn uniformly between 2 s and 3 s and then doubled. The point is
that simultaneous retries spread without any negotiation. LDMUX's first retry is a fixed 2 s, so two
ends that lose the same frame retry in lockstep. **Open, low priority:** a two-radio private link
has almost no contention to spread; a jittered initial would matter only on a shared channel.
Adding jitter also makes the ARQ's timing tests non-deterministic unless the jitter source is
injected, which is why it was not done in this pass.

### 2.6 A sliding window on a half-duplex link is questioned by the theory and justified by LDMUX's measurements

`retries-and-session-state.md:147-162` argues that with `η = T_X / T_rtd` already tiny, windowing
buys little and mostly adds state, and notes that every practical implementation in the ecosystem
uses chunked one-way transfer with FEC rather than a sliding window.

LDMUX runs an 8-record sliding window and measured why
(`docs/MESH-SESSION-STABILITY.md`, `docs/MESH-GATEWAY.md`): on this link an ack only rides on the
peer's next record or its pure-ack timer, so one-update-at-a-time cost a whole round trip per
update — 16 s from keystroke to echo on the device. The window is what keeps the pipe full between
host round trips. The two views are not in conflict: the research is arguing about *bulk media over
a lossy shared channel*, and LDMUX's window is about *interactive terminal records on a private
pair*. Documented here so the next reader does not "fix" the window away.

### 2.7 `0x19` cannot flood, and its path byte is read two ways — the phone already avoids it, the host does not

`companion-wire-layouts.md:73-111` verifies `CMD_SEND_RAW_DATA` (25): `path_len` is read as an
`int8_t` for the bounds guard, then passed to `sendDirect`/`copyPath`, which reads the same byte as
the packed hop-count/hash-width field. Below 0x40 the two readings coincide; at 0x40 and above they
diverge. `path_len = 0xFF` is rejected — raw data cannot be flooded at all, while channel data
(`0x3E`) explicitly permits `0xFF` as flood. It also confirms `0x19` is fire-and-forget: only
`UNSUPPORTED_CMD` and `TABLE_FULL` are ever reported, and OK is written before any air outcome.

LDMUX's phone link sends every record through `0x41 CMD_SEND_RAW_PACKET` at every path width
(`AndroidRadioLink.kt`, `rawPacketFor`), which removes the ambiguity and gives the host the whole
packet. The **host** still reaches its radio through MeshMore's gateway, whose carrier is `0x19`
(`meshmore-sns/.../mesh_carry.dart:206`) — so the host's path is subject to that byte's dual reading
until the host radio client is LDMUX's own. That is a separation item, not a bug to fix in LDMUX's
code today, and it is why the separation plan's host-radio step matters beyond ownership.

### 2.8 The path cache never expires; only the radio owner can flush it

`retries-and-session-state.md:97-114` records that MeshCore's path cache is replaced
unconditionally with no timer expiry and no reboot persistence, and that after N failed direct
retries the sender should discard the cached path and flood again. That is why a stale route
silently degrades to direct range.

The phone link already does the client-side half: on `NotFound` with a non-empty path it drops the
path and retries as a neighbour (`AndroidRadioLink.kt`, `rawPacketFor`/`send`). The daemon cannot do
this at all — it does not own the radio and the gateway protocol has no "forget this path" frame.
**Cross-boundary fact:** a host-side stale path can only be cleared by the radio owner (MeshMore's
gateway today, LDMUX's host radio client after separation) or by the peer's phone.

### 2.9 Field latency is 0.6–1.3 s per attempt; the UI must admit it

`retries-and-session-state.md:180-192` gives measured one-way figures on comparable links. With a
138 ms frame and an ack riding on the next record, a round trip on this link is several hundred
milliseconds before any retry. Nothing in LDMUX should show "sending" for under a second as if it
were stalled; the phone's `SILENCE_FAILURE_MS` and the driver's recovery ladder already treat
seconds as normal. Recorded so a future "why is it slow" does not turn into a shortened timeout.

### 2.10 The regulatory floor: 500 kHz is what makes the deployed tuple lawful

`radio-legality-and-presets.md` and `COVERAGE.md` decision D11 state that a fixed channel below
500 kHz is not defensible under Part 15.247 and that "500 kHz" is the floor. LDMUX's deployed tuple
is 914.25 MHz / **500 kHz** / SF8 (`docs/MESH-STANDALONE.md:415`), so it meets the floor; the 62.5
kHz public MeshCore presets do not. The same note says MediaMesh's 911.150/250 kHz preset inherits
the problem. Relevant to LDMUX as a constraint on any future preset change: do not drop below
500 kHz on a fixed channel and then describe it as compliant.

### 2.11 ALOHA: the packet count is a tax on every node, not a local cost

`retries-and-session-state.md:163-179`: pure-ALOHA success is `e^(−2G)`, and the number of packets a
feature emits is a shared-channel tax. LDMUX's burst pacing (`BURST_GAP` 1 s), the bridge's quiet
cadence and the ARQ's refusal pace all reduce that tax; nothing in LDMUX should treat airtime as
free. This is the quantitative backing for keeping `BURST_GAP` and for not adding chatter.

## 3. Where the research and LDMUX's own design disagree

| Topic | Research says | LDMUX does | Who is right here |
| --- | --- | --- | --- |
| Sliding window | Mostly adds state; `η = T_X/T_rtd` is tiny | Window 8, measured to remove a 16 s echo stall | LDMUX, for a private interactive pair; the research is about bulk media on a shared channel (`§2.6`) |
| Heartbeat | Send a keepalive well inside 2 minutes behind carrier NAT | `heartbeat_ms = 0`, never | Both: LDMUX's peer is a radio link with its own L2 liveness (BLE disconnect, TCP close); a heartbeat would be a record. The phone-hosted-gateway case is MeshMore's, not LDMUX's |
| Retry budget as attempts AND wall clock | Both, because duty cycle separates them | Attempts only, 5, with doubling to a 30 s cap | Open: the wall-clock bound is implicit in the doubling (≈60 s to failure) and is not stated or tested as a bound |

## 4. Facts about the host's radio path that LDMUX depends on and cannot fix

These are documented so that a defect seen from LDMUX's side is routed to its owner.

- **The gateway charges airtime before checking whether the client is still there**
  (`meshcore_gateway/lib/src/session.dart`, `_onBoundFrame`: charge, then `if (!_open) return`). A
  client that disconnects mid-send still spends budget. LDMUX cannot observe or prevent it.
- **`ERR_CODE_TABLE_FULL` is 3, not 6.** A client that discriminates on code 6 misclassifies a full
  transmit table as an illegal argument (`companion-wire-layouts.md:73-111`,
  `MyMesh.cpp:132`). LDMUX's Android mapping is already right; the host's gateway mapping is
  MeshMore's to keep right.
- **`0x19` is the gateway's carrier and cannot flood.** Every host-side record therefore depends on
  the radio having a route. The phone's `0x41` carrier does not have that limitation. This asymmetry
  is one more reason the host radio client should be LDMUX's.
- **A path learned once persists until a retry threshold trips.** Nothing in LDMUX's daemon can
  clear it; the peer's phone and the radio owner can (`§2.8`).

## 5. Not LDMUX's scope, recorded so it is not mistaken for it

The corpus's settled decisions are about MeshMore's media and product design and are listed only to
be excluded: `0x5643`/`0xAE1C` media adoption and registration (D1, D2), FEC constants (D3),
Codec2 voice and the 250 kHz requirement (D6), capability negotiation in the app-name tail (D8),
group-data namespaces, TAK bridging and CoT, Meshpix/MediaMesh transports, and the XR brief. LDMUX
carries a terminal over MeshCore; none of these changes its carriage. If a future LDMUX feature adds
media, the FEC and salting rules become binding — the salting one already is, and LDMUX satisfies it
by accident of its crypto layer (`§2.1`).

## 6. Open items this pass leaves

1. Phone-side refusal accounting (`§2.2`) — needs an additive ABI call and a Kotlin binding, and a
   device build to verify.
2. Deriving or raising the first retry interval above the firmware's own direct timeout (`§2.4`).
3. Jittered initial retry, if the link is ever shared (`§2.5`).
4. Stating and testing the retry budget as a wall-clock bound as well as an attempt count (`§3`).
5. The host's carrier choice and path flushing wait on the host radio client (`§4`).

## 7. Sources

Corpus, read 2026-11-04: `notes/COVERAGE.md`; `notes/MESHMORE-GAPS.md`;
`notes/topics/retries-and-session-state.md`; `notes/topics/companion-wire-layouts.md`;
`notes/topics/radio-legality-and-presets.md`; `notes/topics/protocol-and-firmware.md`;
`notes/topics/two-profile-radio.md`; `notes/topics/INDEX.md`.

Repository, this pass: `crates/session/src/arq.rs` (Karn, refusal handling, retry interval);
`crates/session/src/session.rs` (per-record sealing and nonce); `crates/daemon/src/mesh.rs`
(gateway timeouts, credit by sequence, budget probe); `crates/daemon/src/mesh_bridge.rs` (cadence,
inbound wake); `packages/mobile/.../AndroidRadioLink.kt` (`0x41` carrier, `errorFor` mapping);
`docs/MESH-STANDALONE.md:415` (deployed tuple).

Firmware, read at `meshcore-fw-alerts`: `examples/companion_radio/MyMesh.cpp:120-145` (error codes),
`:1519-1544` (raw-data guard and its failure modes).
