# Mesh session stability

Why the mesh link drops, what the industry does about it, and what we change.

Written 2026-11-01 after a night of live failures. Every claim about our own code has a `file:line`;
every claim about an outside system names the document it comes from, because the numbers matter and
they are the whole argument.

---

## 1. What actually fails

Three distinct faults were measured on the real pair (phone radio + host radio, 910.525 MHz / 500 kHz
/ SF7 / CR5, both ends a metre apart at −27 to −37 dBm):

**A. The host refused every bind when it had no route to the phone.** `serial_mesh_sender.dart`
returned `BindError.noPath` whenever `contact.directPath` was null. A route is learned *only* from a
packet that arrived along one, and a flooded packet carries no route — so two ends that both flood can
never satisfy that condition. Result: `pathLen=255` in the host's own banner, every bind refused, the
phone re-keying for ever. Fixed in `d5ae10a`.

**B. Binding without a route then refused every send.** `bindPeer` had been taught to accept a null
path; `sendRecord` still refused one. The handshake reply was accepted by the daemon, handed to the
tunnel, and dropped by the sender without reaching the radio — so the phone never received `msg2`,
re-keyed after six seconds, and the host counted another "accepted handshake". Fixed in `d5ae10a`.

**C. Every drop costs a full handshake and the terminal.** There is no session identity that survives a
reconnect: a lost record means a new X25519 exchange, a new session, a new attach, and the UI falls
back to the launcher. This is the structural fault, and A and B were symptoms that made it visible.

---

## 2. What the standards and the field do

The constants in the IP suite were calibrated on paths whose round trip is tens of milliseconds. SF7 at
500 kHz is fast for LoRa, but a single record plus its ack still occupies the channel for hundreds of
milliseconds, and the radio is half duplex, so it cannot transmit and listen at once.

- **RTO and Karn.** RFC 6298 sets the initial RTO at 1 s precisely because 97.5% of measured
  connections had RTTs under one second, and forbids taking an RTT sample from a retransmitted segment
  because the ack is ambiguous. Where the estimator cannot tell which transmission was acked, the cure
  is exponential backoff by two, "extremely effective in balancing performance and efficiency in
  networks with high packet loss". A fixed 6 s re-key interval on a link whose handshake round trip
  can exceed 6 s is the classic pathology: it retransmits *before* the answer can legally arrive,
  discards the answer when it does, and never learns the real RTT.
- **QUIC's probe timeout** adds the peer's `max_ack_delay` to the estimate explicitly rather than
  absorbing it into variance, doubles on each expiry, and — the part that matters most here — declares
  persistent congestion by *duration*, not by counting timeouts, because "application patterns impact
  PTO expiration" and a sender that is silent between short bursts may never see a PTO at all.
- **Failure detection ≠ retransmission.** CoAP says a requester should keep a timeout unrelated to its
  retransmission timers in case the server is gone; LoRaWAN treats a confirmed uplink with no ack as a
  coverage fact, not a liveness fact. Two instruments, two jobs.
- **Session identity decoupled from the path.** QUIC's connection ID exists so that "changes in
  addressing at lower protocol layers do not cause packets for a QUIC connection to be delivered to the
  wrong endpoint"; DTLS gained connection IDs for exactly our failure — RFC 9146 names NAT rebinding as
  the cost of identifying a session by its 5-tuple. RFC 9000's path validation is probed with 64 bits
  of entropy and abandoned only after three probe timeouts, deliberately generous so that one lost
  challenge is not a dead path.
- **Mosh is the closest working design.** It treats the terminal as state, not a byte stream: numbered
  source/target states, every datagram an idempotent diff, roving accepted "without any timeouts or a
  notion of reconnection" because any authenticated packet with a higher sequence number simply takes
  over, and a heartbeat at least every three seconds. Its documented limit is the one we must not
  copy: it synchronizes only the current screen, so scrollback missed during an outage is gone — which
  is why our line-numbered accumulation is the right shape and mosh's is not enough.
- **Keepalive numbers.** QUIC notes that "sending packets every 30 seconds is necessary to prevent the
  majority of middleboxes from losing state"; MQTT declares a client gone at 1.5 × its keepalive; ICE
  uses 15 s; mosh pads to 3 s. On a private radio link with no middleboxes, the binding constraint is
  the peer's own silence detector, not NAT.
- **Reconnect storms.** Every source that discusses them says the same thing: exponential backoff
  *with full jitter*, because identical backoff is still synchronised. Two ends that double on the same
  schedule collide on the same schedule.
- **Metastable states are real.** Meshtastic's field notes and the MeshCore path-hash literature both
  describe nodes that remain listed as reachable while being unable to hear, and caches that are only
  invalidated by traffic that the invalidation would itself prove. Our deaf-radio watchdog
  (`f0e43d4`) exists for that class.

---

## 3. What we change

Ordered by what unsticks the user first.

### 3.1 Control traffic is never gated by the airtime budget or by unreported budget

`mesh.rs:652-654` sets `records_left: budget.unwrap_or(0)` and `mesh_session.rs:213` takes
`min(records_left, l2_slots)`. A handshake reply is queued in `outbox` and drained by the same gate, so
"the gateway has not reported a budget yet" silently blocks the first handshake — the very exchange
that would tell the gateway anything. Acks and retransmissions are already exempt on the phone
(`AirtimePolicy.ownedChannel`, `7fb1e5c`); the host must match. **Handshake replies, acks and
heartbeats are control traffic: they are sent, full stop.**

### 3.2 A handshake is re-asked on a backoff, never faster than the answer can arrive

The phone currently re-keys every 6 s (`MeshTermDriver.kt:164`) and abandons the pending handshake, so
a `msg2` that arrives late is handed to the wrong handshake and fails to decode. Measured consequence:
`handshakes 5 accepted` with the phone still on `handshaking`.

The ladder becomes 8 s, 16 s, 32 s (capped), which is CoAP's doubling and RFC 6298's backoff, and the
first interval is chosen to exceed the observed handshake round trip rather than in spite of it. Total
give-up is unchanged in spirit (the transport is not abandoned quickly) but is now ~56 s of real
waiting instead of 18 s of self-defeating churn.

### 3.3 A rebuilt connection keeps the terminal, and resumes the shell

Dropping to the launcher on every blip is why the app "keeps handshaking". The screen keeps the last
frame, shows the link state as a ribbon over it, rebuilds underneath, and **re-attaches the same shell
id** rather than asking the host to list again. The claim already survives (`7a8ef08`), so the shell is
there to come back to.

### 3.4 A session has an identity that outlives a record

The durable fix, and the one that makes the rest small: the L3 session gets an epoch (random 8 bytes at
establish), carried in every record header. A reconnect presents `(epoch, last-line-seen)`; the host
resumes the same session state instead of a fresh handshake, and the phone re-attaches to the same
shell in one round trip. Records from before the reconnection are deduplicated on `(epoch, seq)` — the
SSP idempotency rule — so a retransmission across a resume is harmless rather than fatal.

This is QUIC's connection ID and mosh's state number, applied to our own framing. It changes the wire
format, so both ends move together: `STREAM_VERSION` 4 → 5 was the last such move, and this is the same
kind of change.

### 3.5 Backoff gets jitter

The screen's 2 s → 30 s doubling (`MeshTerminalScreen.kt:102-117`) gains full jitter:
`delay = random(0, min(cap, base × 2^attempt))`. With two ends rebuilding, uncorrelated retries are the
difference between a collision loop and a recovery.

### 3.6 Keep the two failure detectors separate

Already true, and worth writing down so it stays true:
`SILENCE_FAILURE_MS = 20_000` on the phone and `radioLooksDeaf` (2 min, air frames only) on the host are
*transport* liveness detectors. ARQ retransmission and RTO are *delivery* instruments. A timeout means
"wait longer", never "the peer is gone"; only the silence detectors may rebuild anything.

---

## 4. What not to do

- **Do not shorten the retry interval to make the UI feel faster.** Every measured failure tonight was
  made worse by re-asking before the answer could arrive.
- **Do not treat "the radio accepted it" as delivery.** `sentDirect` means the firmware queued the
  frame; the route may be dead (`9f63a0c`).
- **Do not reconnect to recover a bad state.** Reconnecting resets the RTT estimate and the ARQ window
  to cold values; the researched advice (WebRTC's estimator, QUIC's migration rules) is to let the
  existing estimate adapt unless the path itself changed.
- **Do not gate control frames on an application budget.** See 3.1.
- **Do not synchronize only the visible screen.** Mosh's own documented limitation; our scrollback is a
  line-numbered accumulation for exactly this reason (`docs/MESH-SCROLLBACK.md`).

---

## 5. Evidence log

- `d5ae10a` — bind-without-route sent by flood, not refused. Verified on hardware: `rssi −25`, one
  handshake accepted, 5 rx / 7 tx, no pending, phone showed the host's real shells.
- `f0e43d4` — deaf radio detected after 2 min of no air frames, `CMD_REBOOT`, exit 75, supervised
  restart on `/dev/serial/by-id/...`.
- `7920818` — header one line, refresh never dimmed, per-attempt seconds.
- Measurement 2026-11-01 00:1x: 42 s of samples, `state=ready`, `hs` constant at 3, `rx` flat at 20
  bytes with `tx` advancing one record per 3 s — the heartbeat being acked. Idle links are stable; the
  churn appears only when a drop forces a rebuild.
