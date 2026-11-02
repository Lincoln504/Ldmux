# Mesh session stability

Why the mesh link drops, what the industry does about it, and what we change.

Written 2026-11-01 after a night of live failures. Every claim about our own code has a `file:line`;
every claim about an outside system names the document it comes from, because the numbers matter and
they are the whole argument.

---

## 1. What actually fails

Three distinct faults were measured on the real pair (phone radio + host radio, 910.525 MHz / 500 kHz
/ SF7 / CR5, both ends a metre apart at −27 to −37 dBm). Three more were measured the same evening,
after the app was left in the background; they are in §8.

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

### 3.3 A rebuilt connection keeps the terminal, and resumes the shell (done)

Dropping to the launcher on every blip is why the app "keeps handshaking". Now the last painted screen
stays where it is, the link's own line sits over it, and the rebuild **re-attaches the same shell id**
rather than asking the host to list again. The claim already survives (`7a8ef08`), so the shell is
there to come back to.

The split is the important part: **the picture belongs to the screen, the input queue belongs to the
connection.** Carrying a batcher across a rebuild re-delivers text the old session had already
accepted - measured 2026-11-01, one `input text` came out as `yhgecho dsh9 > /tmpmecho dsh9 >
/tmp/meshin9` because the tail of the old queue was prepended to the new one. Typed text is addressed
to one session and one sequence space, so it dies with that session. A picture has no addressee.

Leaving the terminal cancels the resume: a rebuild after the user goes back to the list lands on the
list.

### 3.4 A session has an identity that outlives a record (deferred, on purpose)

**Decision, 2026-11-01: not doing this yet.** After 3.3 the reconnect costs the user nothing visible -
the screen stays, the shell re-attaches, and the only price is one 3-record handshake on a channel with
no voluntary budget - so a wire-format change bought no user-visible behaviour. The case that would
justify it is a link that drops often enough for the handshake itself to matter, and that is not what
the measurements show. Recorded here so the reasoning is not lost, not because it is wrong.

The sketch that follows is what it would look like.

The durable fix, and the one that makes the rest small: the L3 session gets an epoch (random 8 bytes at
establish), carried in every record header. A reconnect presents `(epoch, last-line-seen)`; the host
resumes the same session state instead of a fresh handshake, and the phone re-attaches to the same
shell in one round trip. Records from before the reconnection are deduplicated on `(epoch, seq)` — the
SSP idempotency rule — so a retransmission across a resume is harmless rather than fatal.

This is QUIC's connection ID and mosh's state number, applied to our own framing. It changes the wire
format, so both ends move together: `STREAM_VERSION` 4 → 5 was the last such move, and this is the same
kind of change.

### 3.5 Backoff gets jitter (done)

The screen's 2 s → 30 s ladder draws each wait uniformly below its cap. Two ends that both double on
2, 4, 8, 16 meet again on 2, 4, 8, 16; that is the shape every account of a reconnect storm prescribes
against.

### 3.6 Keep the two failure detectors separate

Already true, and worth writing down so it stays true:
`SILENCE_FAILURE_MS = 20_000` on the phone and `radioLooksDeaf` (2 min, air frames only) on the host are
*transport* liveness detectors. ARQ retransmission and RTO are *delivery* instruments. A timeout means
"wait longer", never "the peer is gone"; only the silence detectors may rebuild anything.

---

### 3.7 A rebuilt surface has to be able to type again (done)

The surface and the hidden field that carries the IME are recreated with the connection. The system
keyboard stays drawn because the window still wants one, but the new field has never been focused, so
typing goes nowhere. The screen now asks for the keyboard again on every new connection.

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

---

## 6. Testing typing on a real phone

Two ways in, and only one of them is robust.

- **`adb shell input text`** injects through the input method, so it needs the hidden
  `BasicTextField` to be focused. It works when it is (verified: `echo dshten > /tmp/meshin9` arrived
  byte for byte) and silently does nothing when it is not, which is exactly the state a rebuild leaves
  behind. It also splits long strings into several commits, so a partial send is indistinguishable
  from a lost one.
- **`adb shell input keyevent <code>`** goes in as hardware keys, which the surface already maps to
  terminal keys through `onPreviewKeyEvent`. It needs no focus and no IME, and each key is one
  observable event. `touch /tmp/m18` (15 key codes plus Enter) created the file on the host, and
  `echo hi` printed `hi`. **Use this one.**

A shell to type into must be chosen by evidence, not by name: several keeper children are plain
`/bin/bash` with a pi session running inside them, and typing into one is a message to pi. The test
for it is whether the bash process has children of its own, matched to the registry's session id by
start time (`/proc/<pid>/stat` field 22 against `createdAt`, to the second). Measured 2026-11-01:
`16db506fe9` runs pi, `9d1bac2059` is bare.

## 7. Evidence from the device, 2026-11-01

- Typing before a drop: `echo dshten > /tmp/meshin9` arrived exactly, file content `dshten`.
- A gateway restart while attached: the terminal stayed on screen at 8, 20, 30, 45 and 60 s - never
  the shell list - and `echo hi` was answered afterwards, so the link resumed underneath.
- Keystrokes are not duplicated by a rebuild any more once the batcher is per-connection.
- `claims.json` is now fsynced before its rename and the directory after it: the rename is atomic
  against a reader, not against a power cut.

---

## 8. The stale screen and the missing gateway, 2026-11-01 evening

Three more faults, measured on the same pair after the app had been left in the background for a long
time. The first two are session faults. The third is the pairing path, and it is why "get token from
MeshMore" changed nothing.

**D. A gateway that closed did not end the connection, so the screen froze on "closed".** The driver's
`run` loop ends only on `transportDead` (`MeshTermDriver.kt:611`, `while (!transportDead)`).
`onGatewayState` (`MeshTermDriver.kt:334`) recorded every non-bound state as `linkProblem` and left
the loop turning, so a peer close - the gateway closing the socket - painted `MeshLinkState.Closed`
and then nothing ever happened again: the socket was gone (`/proc/<pid>/fd` had no socket for the
app), the caller's rebuild was never reached because `run` never returned, and only the refresh
button (which bumps `attempt` directly) could recover. **Fix:** any terminal gateway state now sets `transportDead` (`MeshTermDriver.kt:348`),
which returns `run` and hands the retry to the caller. Test: `a gateway that closed ends the run so the
caller can rebuild`.

**E. The mesh route never let go when the app left the screen.** The tailnet route registers
`AppLifecycle.setReleaseHandler` and releases its shells after the platform's grace; the mesh route did
not, so a 60-second background left the socket open in a frozen process, the last committed frame on
screen as if it were live, and typing addressed to a session nobody was driving. **Fix:** the mesh
screen now registers the same handler (`MeshTerminalScreen.kt:158`): at the grace it
marks `released` and closes the connection; the connection effect waits for the foreground and rebuilds
at once (not on the failure jitter, because nothing failed), and the new connection re-attaches
`resumeShell`. A connection that ends while off screen, or whose jitter finishes while backgrounded,
takes the same return path. Measured after the fix: Home, 60 s, return - the same plain shell was live
again in 3 s, one new handshake accepted, `records 30 sent, 0 pending`.

**F. MeshMore's gateway was not running even with the switch on, and a Binder bind cannot start it.**
`GatewayService` was a lazy provider, so its `load()` (and therefore the listener) ran only when
Settings -> Connected apps was opened; the screen's "Listening on 127.0.0.1:41731" is a default, not the
bound port. A handoff bind starts MeshMore's **process** (`BIND_AUTO_CREATE`) but not its Flutter engine,
so the handoff store - in memory, published by Dart - answered `off` and LDMUX said "MeshMore is not
serving" while MeshMore was running. Worse, Back on MeshMore's Dashboard finished the Activity and
destroyed the engine, which stopped the radio link and the gateway; the foreground service kept the
process alive around neither. **Fix, MeshMore:** the gateway provider is `lazy: false` so it is created
with the app (`responsive_starter_app/lib/main.dart:114`); Back on the Dashboard calls
`moveTaskToBack(true)` through `meshmore/app_lifecycle` (`MainActivity.kt:36`, `home_shell.dart:175`)
instead of finishing the Activity. **Fix, LDMUX:** the handoff is attempted automatically for
`Unreachable` and `Refused` as well as `NeedsToken`, and the button opens MeshMore when the offer is
"not serving" (the press is the user's permission to switch apps), then retries on return. Measured:
MeshMore force-stopped, LDMUX started, button pressed - MeshMore opened, its gateway listened on 41731
with no visit to Connected apps, Back kept the listener, and LDMUX linked and listed the shells.
