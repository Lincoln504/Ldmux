# Public-mesh audit: complete root-cause investigation, architectural critique, and design review

## 0. Scope and evidence base

- Audited 2026-11-11 against `ldmux` branch `fix/public-mesh-batch`, HEAD `9cf722e`.
- Documentation-only pass: no code, service, or radio state was modified.
- Consolidates and supersedes the intermediate 2026-11-10 field notes and ledger scratchpads (`docs/MESH-RADIO-CONSIDERATIONS.md`, `docs/RADIO-AUDIT-LOOP.md`, and `docs/RADIO-AUDIT-DISPOSITION.md`, all readable in git at `9cf722e`) into one authoritative root-cause, firmware, and integration review paired with [`docs/RADIO-PLAN.md`](RADIO-PLAN.md) and [`docs/adr/0039-batch-transaction-radio-architecture.md`](adr/0039-batch-transaction-radio-architecture.md).
- Corpus inspected:
  - **All 11 Pi session transcripts** (`2026-10-31` through `2026-11-11`, **750 user prompts** and **55 structured `ask_user_question` answers**, indexed chronologically in full).
  - **Complete git commit history** of the radio, session, companion, daemon, and mobile layers (`c65520c` through `9cf722e`, plus `stash@{0}`).
  - **Live host `journalctl` logs** (`2026-11-05` through `2026-11-11`) and `/home/ldeen/Documents/lora` bench/field capture records (`docs/01..10`).
  - **MeshCore firmware C++ source** (`examples/companion_radio/MyMesh.cpp`, `src/Mesh.cpp`, `src/Dispatcher.cpp`, `src/Packet.cpp`, `src/helpers/BaseChatMesh.cpp`, `src/helpers/SimpleMeshTables.h`, `src/helpers/TransportKeyStore.cpp`, `src/helpers/esp32/SerialBLEInterface.*`, `src/helpers/nrf52/SerialBLEInterface.*`, `examples/simple_repeater/MyMesh.cpp`) and 25+ ecosystem repositories in `/home/ldeen/Documents/mesh-research/` (`openhop_core`, `rns-meshcore`, `meshcore-pi`, `meshcore.js`, `mcrs`, `mcsim`, `coresplitter`, `meshcore-keymind`, `meshcore-open-hdden`, `meshcore-open-tak`, `wadamesh-tak`).
  - **Complete `ldmux` source** across `crates/companion`, `crates/mesh`, `crates/session`, `crates/session-ffi`, `crates/protocol`, `crates/daemon`, `crates/keeper`, `packages/mobile`, and `packages/mobile/vendor/libmeshcore*` (**38,000+ lines of radio-related code across Rust, C-FFI, Kotlin, and Java**).
- **Citation convention.** `RC-n` and `IC-n` are defined in this file. `S1`–`S20` are the field symptoms in §4. `V1`–`V20` are the firmware verifications in §6.2. `A-nnn` ids name entries of the 320-row audit ledger, which was consolidated into §6 and removed from `docs/`; each is readable with its evidence at `git show 9cf722e:docs/RADIO-AUDIT-LOOP.md`.

---

## 1. Large-scale architectural diagnosis: why 38,000 lines of radio code failed in the field

Before listing the 12 concrete code and firmware root causes (§2), the largest lesson of this audit is **structural**: the bugs were the inevitable product of **five large-scale architectural mistakes** in how the radio stack was partitioned across languages, crates, tasks, and UI trees:

1. **Split-brain protocol duplication across 4 languages (`Rust`, `C-FFI`, `Kotlin`, `Java`) despite already shipping a Rust JNI library (`libldmux_session_jni.so`)**:
   - The Android app already compiles and loads Rust via JNI (`crates/session-ffi`), yet drew the JNI boundary at the lowest possible level (`Handshake` + `Session` only).
   - Everything above and below that narrow seam was written and maintained **twice (or three times)**:
     - Companion framing & command serialization: `crates/companion` (Rust, 2,870 lines) vs `vendor/libmeshcore*` (`MeshcoreFrameCodec.java` + `MeshcoreSession.java`, 2,295 lines).
     - Mesh routing, `LDRQ`/`LDRR`/`LDMR`, flood pacing & carrier selection: `crates/daemon/src/mesh_serial.rs` (3,469 lines) vs `RadioRoute.kt` (591 lines) + `AndroidRadioLink.kt` (2,127 lines).
     - Terminal wire codec & screen mirror: `crates/protocol/src/mesh_term.rs` (3,796 lines) vs `MeshTerm.kt` (1,049 lines) + `MeshTermDriver.kt` (2,673 lines).
   - **Consequence**: Every protocol rule drifted between host and phone (`awaitsOkOrError` in Java vs strict 1-command-at-a-time in Rust; `answerRequest` double-wrapping `LDRR` in Kotlin; `sessionUp(true)` never called in Kotlin; `directWorks` vs `peer_via_flood`; `120 s` vs `60 s` route freshness), and **no test in the repository could ever run the real phone stack against the real host stack**.

2. **Inverted boundary between `Session` (`crates/session`) and `Carrier` (`mesh_serial.rs` / `AndroidRadioLink.kt`)**:
   - `Session::poll_with_seq` sits above an opaque `send(record: &[u8])` wall and must seal ciphertext to a scalar `record_limit` **before** the carrier decides whether `Attempt k` will travel via **Direct (`RAW_CUSTOM`, 2 B + path)** or **Flood (`LDMR`, 12–16 B + growing travel path)**.
   - Conversely, the carrier receives opaque ciphertext bytes without knowing whether the packet is `Attempt 1` (try direct) or `Attempt 2` (escalate to flood) or what `seq` it belongs to.
   - **Consequence**: Produced the 162-byte `logRxRaw` (`0x88`) cliff, the `requeue_remainder` stream-corruption bug, and the prior plan's misguided proposal to shrink all direct records to 93 bytes (cutting direct throughput by >2.2×).

3. **Adapting a 50 ms continuous live-streaming multiplexer (`mesh_bridge.rs`, 6,305 lines) + byte-stream `Arq` (`arq.rs`, 1,060 lines) to a half-duplex batch radio link**:
   - On the host, `mesh_bridge.rs` runs a 50 ms `Differ` polling loop in a separate task across a lossy `Arc<Mutex<Exchange>>` queue (`mesh.rs:1165`), streaming unsolicited `Profile` + `Rows` + `Commit` messages and relying on a phone-sent `Message::Hold { on: true }` brake pedal to stop streaming in batch mode.
   - Underneath it, `Arq` (`arq.rs`) + `TermDecoder` (`mesh_term.rs`) enforce an **uninterruptible TCP-like byte stream** where:
     - Every arriving record in a multi-record screen triggers a delayed uplink `Ack` (`ack_delay_ms = 1.2 s`) **mid-burst**, colliding head-on with the remaining downlink records at half-duplex repeaters.
     - One lost record of an obsolete screen snapshot causes permanent head-of-line blocking (a newer `Snapshot` cannot supersede an in-flight `Snapshot` without corrupting `TermDecoder`'s byte stream).
     - Exhausting retries on a single record (`Failure::RetriesExhausted`) **destroys the cryptographic `Noise_IKpsk2` `StatelessTransportState` session (`mesh_session.rs:411`)**, forcing a full multi-minute re-handshake even though `StatelessTransportState` uses explicit 64-bit counters and never loses cryptographic state on packet loss.

4. **Two forked mobile applications inside one APK (`AppContent.kt` vs `MeshTerminalScreen.kt`)**:
   - `App.kt:120` branches between `AppContent` (Tailnet) and a separate 1,698-line monolithic composable `MeshTerminalScreen` (Radio) that reimplemented shell paging, lifecycle, keyboard/viewport measurement, and header navigation from scratch — introducing single-mirror cross-shell bleed (`S7`), missing the 5-shell cap the tailnet route enforces (`MAX_OPEN_PAGES`), never assigning `resumeShell` (`S14`), detaching after 10 s in background (`CLAIM_RELEASE_MS`, `S14`), firing an automatic `listShells()` on shell open (`:808`), and saving the keyboard-down `46×32` grid before the IME finished opening (`S2`).

5. **Two incompatible device-identity and claim-ownership models (`ownership.rs` & `daemon.rs`)**:
   - Tailnet identifies a phone by its pairing token with a 15 s reconnect grace; Radio hardcodes `"mesh-link"` for every radio client (`mesh_bridge.rs:2018`), exempts `"mesh-link"` from claim sweeps (`daemon.rs:2996`), and drops all radio claims on the first tick of a re-handshake (`mesh_bridge.rs:964`).

---

## 2. The 12 proven code & firmware root causes (`RC-1` through `RC-12`)

### RC-1 · Self-jamming connect burst (14 flood packets before a session exists)
- **Evidence**: `AndroidRadioLink.kt:818, 829, 1732`, `MeshTermDriver.kt:693, 717, 732`, `journalctl` (`2026-11-10 18:42:04, 20:44:39, 20:45:41`).
- **Mechanism**:
  - `AndroidRadioLink.connect()` floods `SELF_ADVERT` (100+ B) and ~20 ms later floods 3× `LDRQ`, waiting `replyWaitMs()` (`5 s` when pathless — shorter than a multi-hop flood round trip!).
  - When that 5 s wait expires while the host's flooded `LDRR` is still crossing the mesh, `MeshTermDriver.connect()` calls `refreshRoute()`, floods 3× `RouteHint` (`LDRH`, 40 B wrapped in `LDMR` by `send()` at `:1010`), and 20 ms later floods 3× `msg1` (121 B).
  - First-hop repeaters enter TX mode to forward `SELF_ADVERT` and 3× `LDRH` (and the host's oncoming `LDRR`) at the exact moment `msg1` is transmitted, deafening the half-duplex repeaters to `msg1`.
  - Proven in `journalctl`: `route flooded record 40 B from a 1/3/14 hop chain` (`LDRH`) arrived at the host (where `mesh_serial.rs:2125` ignored it for routing because `route_hint()` is only checked on direct `RAW_CUSTOM` packets at `:2177`), while `msg1` behind it was lost.

### RC-2 · BLE companion command-window race in `MeshcoreSession.java` + `AndroidRadioLink.kt` delivers `ERR_CODE_NOT_FOUND` to `sendRawPacket` and overflows nRF52 `bleuart`
- **Evidence**: `MeshcoreSession.java:387, 685-705, 816, 994-1009, 1081`, `AndroidRadioLink.kt:784-785, 1058, 1282, 1834`, `MyMesh.cpp:1651, 1715, 1851, 2050`, `nrf52/SerialBLEInterface.cpp:375-393`.
- **Mechanism**:
  - `MeshcoreSession.awaitsOkOrError` (`:685`) returns `false` for `0x1E` (`GET_CONTACT_BY_KEY`), `0x2A` (`GET_ADVERT_PATH`), and `0x1F` (`GET_CHANNEL`), assuming they never return `OK` or `ERROR`. In firmware (`MyMesh.cpp`), all three return **`RESP_CODE_ERROR (0x01)` (`ERR_CODE_NOT_FOUND = 2`)** when the entry is absent!
  - Because `ldmux` keeps no contacts (`ADR-0030`), `s.getContact(hostKey)` (`0x1E`) **always** returns `ErrorFrame(2)` on a radio without a saved chat contact for the host, and `s.getAdvertPath(key)` (`0x2A`) returns `ErrorFrame(2)` whenever the 16-entry RAM advert cache misses.
  - `AndroidRadioLink.refreshRoute()` (`:1282`, called on every `MeshTermDriver.connect()` at `:693`) calls `runCatching { s.getContact(hostKey) }` on its **last line and returns immediately without awaiting `contactDone` or `s.isIdle()`**; `startPathRefresh()` (`:1834`) also fires unawaited `getAdvertPath`/`getContact` every 10–60 s.
  - Because `awaitsOkOrError` returns `false` for `0x1E` and `0x2A`, `s.canSendRaw()` returns `true` and `s.sendRawPacket()` (`0x41`) is written to BLE while `0x1E` or `0x2A` is still in flight.
  - When `ErrorFrame(ERR_CODE_NOT_FOUND = 2)` from `0x1E`/`0x2A` arrives, `MeshcoreSession.dispatch()` (`:996`) sees `pendingRaw != null` and **delivers `ErrorFrame(2)` (`RadioSendResult.NotFound`) to `sendRawPacket`'s callback (`pendingRaw`)**!
  - In `AndroidRadioLink.send()` (`:1058`), receiving `RadioSendResult.NotFound` when `path.encoded > 0` **wipes `contactPath = null` and `advertPath = null` (destroying the route `ensureRoute()` just learned!)** and retries zero-hop; when `path.encoded == 0`, `MeshSendOutcome.ErrNotFound` reaches `MeshTermDriver.transmit()` (`:2149`), which calls `failed(MeshLinkState.Refused, "the radio refused the record: ErrNotFound")` and **kills the session**!
  - Furthermore, `s.requestBatteryStorage()` (`MeshcoreSession.java:387`, called at `AndroidRadioLink.kt:784` right before `readDeviceInfo()` at `:785`) is sent **untracked**, yet `dispatch()` (`:1081`) calls `trackedAnswered()` on `BatteryStorageFrame`, settling `deviceQuery(3)` early and desynchronizing `pendingTracked` across `connect()`.
  - And on nRF52 BLE companions (`nrf52/SerialBLEInterface.cpp:375-393`), writing `getContact` (`33 B`) and `sendRawPacket` (`149 B`) back-to-back into `bleuart` exceeds `MAX_FRAME_SIZE (176)` (`182 B > 176 B`), triggering `"BLE RX overflow, draining all"` and dropping both commands.

### RC-3 · Passive first-hop repeater self-echoes treated as inbound peer records (`mesh_serial.rs:2432, 2268` & `AndroidRadioLink.kt:522`)
- **Evidence**: `Dispatcher.cpp:195`, `MyMesh.cpp:2050`, `Mesh.cpp:97-105`, `mesh_serial.rs:2268-2284, 2432-2443, 2633`, `AndroidRadioLink.kt:522-540`, `mesh_session.rs:230-234`.
- **Mechanism**:
  - In MeshCore, `CMD_SEND_RAW_PACKET` (`MyMesh.cpp:2050`) calls `Dispatcher::sendPacket` without calling `_tables->markSeen(pkt)`, and `Dispatcher::checkRecv` (`Dispatcher.cpp:195`) emits `PUSH_CODE_LOG_RX_DATA (0x88)` on **every** packet heard on the air before `Mesh::onRecvPacket`.
  - When the host or phone sends a direct `RAW_CUSTOM` (`0x3E`) packet through its first-hop repeater `R1`, `R1` calls `removeSelfFromPath` (`Mesh.cpp:100`) and retransmits the packet ~500 ms later. Because LoRa is omnidirectional, **the sender's own radio hears `R1`'s retransmission**!
  - On the host, `incoming_record()` (`mesh_serial.rs:2432`) **never checks `view.path.hops == 0`** (pinned by a mistaken test at `:2633`), so for **every** multi-hop direct send ($1..63$ hops), the host accepts `R1`'s retransmission (`hops = N - 1`) as an incoming record from the phone!
  - On a 1-hop direct send (`[R1]`), `R1` strips `R1` so `hops == 0`, and the sender's radio emits **both `0x88` and `0x84` (`onRawData`)** for its own echoed packet!
  - Both `mesh_serial.rs:2268-2284` and `AndroidRadioLink.kt:522-540` immediately stamp `last_peer_record = now` / `lastInboundAt = now`, `peer_path_heard = now` / `routeHeardAt = now`, `routeProven = true`, `directWorks = true`, and `peer_via_flood = false` **before `Session` checks the record**, fooling the sender into believing the peer is answering on a dead direct route while feeding its own outbound ciphertext into `Session::receive` (causing the `Record(BadTag)` drops and the `unknown record kind` on echoed `msg2` `0x12` cited in `mesh_session.rs:230-234`!).
  - Conversely, in `AndroidRadioLink.onRawData` (`:522`), when a genuine 0-hop direct packet arrives while `hostPath == null`, `if (hostPath != null)` is `false`, so `directWorks` stays `false` and `lastInboundAt` is never updated!

### RC-4 · Phone `answerRequest` (`AndroidRadioLink.kt:1466-1478`) is triple-broken (host-initiated discovery in `ADR-0035` never worked)
- **Evidence**: `AndroidRadioLink.kt:619-637, 944, 1466-1478`, `RadioRoute.kt:275-309`.
- **Mechanism**:
  - When an `LDRQ` route request from the host arrives at the phone (`:619-637`), the phone never calls `RadioRoute.heardFloodPath(raw)` to adopt the route the host's request just travelled.
  - In `answerRequest(raw)` (`:1472`), if `advertPath` and `contactPath` are both `null`, `val path = advertPath ?: contactPath ?: return` returns early without answering the host's request at all.
  - When a path *is* present, `val reply = RadioRoute.replyFlood(...)` (`:1473`) builds a complete OTA `LDRR` flood packet and passes it to **`send(reply)` (`:1476`)**, which treats `reply` as a session record and **double-wraps the `LDRR` packet inside an `LDMR` session-record flood (or `0x3E` `RAW_CUSTOM` direct packet)**! The host's `parse_route_reply_flood` (`codec.rs:766`) can therefore **never** parse a route reply from the phone.

### RC-5 · Handshake retry ephemeral-key churn, `routeHops == null` 8-second timer, `lock.withLock` blocking, non-idempotent `msg1`, and direct `wasSeen` dedup
- **Evidence**: `MeshTermDriver.kt:693, 906, 1013, 1049, 1091-1140`, `mesh_session.rs:285-333`, `Mesh.cpp:97`, `Packet.cpp:42`.
- **Mechanism**:
  - `MeshTermDriver.tick()` (`:1091`) times flooded handshake retries using `handshakeRetryFor(gateway.routeHops)` (`null` → **8.0 seconds**!) instead of `sessionHopsFor(gateway.routeHops, gateway.meshReach)` (`14–28` hops → `36–64 s`).
  - When `gateway.routeHops == null || !gateway.routeFresh` (`:1092`), it resets `handshakeTries = 0` (so the 8 s timer never doubles), calls `gateway.refreshRoute()` **while holding `MeshTermDriver.lock`** (blocking `onRecord` for `5–38 s` while flooding 3× `LDRQ` into the air), and at `blindAsks % 3 == 0` (`24 s`) **abandons `handshake` and calls `connect()` to generate a new `msg1` with a new ephemeral key**!
  - When the host's `msg2` (encrypted to Attempt 1's ephemeral key) arrives at `t = 25–35 s`, the phone tries `h.finish(msg2)` with Attempt 2's ephemeral key, fails with `HandshakeError::Crypto`, and calls `failed("the handshake could not finish: Crypto")`!
  - On the host, `SessionHost::on_handshake` (`mesh_session.rs:285`) has no idempotency cache, so every duplicate copy of `msg1` replaces `self.session`, increments `next_index`, clears `outbox`, runs 2 `fsync`s, and queues 3 flooded `msg2` copies; on the phone, `MeshTermDriver.onRecord` (`:906`) consumes `handshake` on any incoming record kind (`KIND_TRANSPORT`).
  - And if a static `msg1` or cached `msg2` is re-sent via Direct (`RAW_CUSTOM`) without a per-transmission nonce, `SimpleMeshTables::wasSeen` (`Mesh.cpp:97`, `Packet.cpp:42`) hashes `[payload_type][payload]` and drops the retry at the first repeater.

### RC-6 · Receiver 173-byte `logRxRaw` (`0x88`) cliff, oversized `LDRR` / `msg1`, and ESP32 4-frame BLE TX queue
- **Evidence**: `MyMesh.cpp:287-296, 832-847`, `codec.rs:716-805`, `MeshRecordLimits.kt:23-45`, `esp32/SerialBLEInterface.h:27`, `esp32/SerialBLEInterface.cpp:178-199`.
- **Mechanism**:
  - `MyMesh::logRxRaw` silently drops any received packet where `len + 3 > 176` (`len > 173 B`).
  - Published `FLOOD_MAX = 162` is `12 + 162 = 174 B` at 0 hops (`174 + 3 = 177 > 176`, dropped even at 0 hops!) and `12 + S + P_arrival + R` at range; 121-byte `msg1` exceeds 173 B when `P_arrival + S > 40 B`; `build_route_reply_flood` (`49 + S + P_carried + P_arrival`) carries a redundant inner path and exceeds 173 B when `P_carried + P_arrival + S > 124 B`.
  - On ESP32 BLE companions (`esp32/SerialBLEInterface.h:27`), `send_queue` holds only **4 frames** drained at 1 frame per `60 ms`, and each received direct `RAW_CUSTOM` packet writes **both `0x88` and `0x84` (2 frames)** in the same millisecond; when `send_queue` is full, `writeFrame()` silently drops the radio's `OK`/`SENT` command reply, causing `AndroidRadioLink.send()` (`:1079`) to time out and tear down the BLE link.

### RC-7 · Byte-stream `Arq` (`arq.rs`) + `requeue_remainder` stream corruption + mid-burst `Ack` self-collisions + session death on timeout
- **Evidence**: `session.rs:259-271`, `arq.rs:584, 640, 755, 863, 913`, `mesh.rs:1044, 1185, 1240, 1254`, `mesh_session.rs:411`.
- **Mechanism**:
  - `Session::poll_with_seq` (`session.rs:259-271`) splits an in-flight slot when `record_limit` shrinks, sealing the prefix under the old `seq` and calling `requeue_remainder` (`arq.rs:755`) to assign a new `seq` to the tail after later records were already numbered — duplicating and reordering bytes when the original packet arrived and its ACK was lost.
  - When a multi-record screen is transmitted, the receiver's `ack_delay_ms` (`1.2 s`) fires **after the first record (`seq=0`)** while the sender/repeaters are still transmitting `seq=1..k`, launching an uplink `Ack` straight into the oncoming downlink burst.
  - On the host (`mesh.rs:1044, 1185, 1254`), `h.poll_with_seq()` is called **before** `h.inbound` is moved to `runtime.exchange.inbound` for `mesh_bridge`, preventing the host's response from piggybacking the `Ack` in the same turn.
  - `Arq::poll()` (`arq.rs:863`) clears `front.refused_since = None` on every re-poll, making `REFUSAL_DEADLINE_MS (30 s)` unreachable; `sent_at` is stamped at `send()` instead of `poll()`, corrupting `srtt`.
  - When any slot fails after `MAX_TRANSMISSIONS`, `Arq` sets `Failure::RetriesExhausted` and `mesh_session.rs:411` **destroys the `StatelessTransportState` Noise session**, forcing a full handshake rebuild on a transient RF fade.

### RC-8 · Batch mode `Sync` splits one command + `Enter` + unhold into 3+ separate LoRa records
- **Evidence**: `MeshInput.kt:90-162, 225`, `MeshTermDriver.kt:1451-1460`, `MeshTerminalScreen.kt:692`.
- **Mechanism**:
  - `MeshInputBatcher` stores `Run.Text("ls")` and `Run.Keys(["Enter"])` as separate runs and pops one run per step.
  - `MeshInputPump.step()` calls `conn.driver.input()` and `conn.driver.keys()`, which call `sendToShell()` (`MeshTermDriver.kt:1451`).
  - `sendToShell()` **bypasses `outgoing` (`flushOutgoing()`)**, calling `s.send(SHELL, message.encode())` and `pump()` directly on each step while `setHold(false)` sends `Hold(false)`.
  - Tapping `Sync` once after typing `"ls"` + `Enter` transmits **3 separate LoRa records** (`Hold(false)`, `Input("ls")`, `Key(["Enter"])` — 9 flood packets when flooding!) milliseconds apart, which collide at the first half-duplex repeater.

### RC-9 · Multiline keyboard paste split by `InputDelta.kt:54` into per-line `Enter` keys (`S8`)
- **Evidence**: `TerminalSurface.android.kt:870`, `InputDelta.kt:42-60`.
- **Mechanism**:
  - `BasicTextField.onValueChange` feeds IME/clipboard multiline paste deltas into `inputOpsFor(field, next)`, which loops character-by-character and turns every `\n` or `\r` into `InputOp.Keys(listOf("Enter"))` instead of routing multiline pastes to `paste()`. Combined with `RC-8`, a 5-line paste executes each line immediately as a separate LoRa packet.

### RC-10 · Phone flood token bucket is 100% dead code (`AndroidRadioLink.kt:245, 987`)
- **Evidence**: `AndroidRadioLink.kt:245, 987, 1027, 1038`, `MeshTermDriver.kt:757, 906`.
- **Mechanism**:
  - `gateway.sessionUp(true)` is never called anywhere in `packages/mobile`. Because `sessionUp` is permanently `false`, `floodTokens` is reset to `FLOOD_BURST (4)` on every send and never decremented.

### RC-11 · Blank terminal with misplaced cursor (`S2`) and keyboard/resize non-update (`S10`)
- **Evidence**: `TerminalSurface.android.kt:318`, `MeshTerminalScreen.kt:692, 808, 1124-1135`, `GridPlacement.kt:129`, `mesh_bridge.rs:655, 1559`, `MeshTermDriver.kt:2195-2214`.
- **Mechanism**:
  - `TerminalSurface.android.kt:318` runs a 250 ms debounce while the soft keyboard takes ~450–500 ms to animate open. At 250 ms, `onGrid` fires with the **keyboard-DOWN** height (`46×32`) and saves `lastGrid = 46 to 32` and `config.meshLastGrid = "46,32"`. (`Takeover` has no grid bytes on the wire at all, falling back to `158×82` in `mesh_bridge.rs:1559`.)
  - When `46×32` arrives while the keyboard-UP viewport is `46×19`, `GridPlacement.placeGrid` (`GridPlacement.kt:129`) sets `dy = viewportHeight - gridH = -13 * cellHeight`, **clipping rows `0..12` (where a shell's prompt and text sit!) off the top of the screen** and rendering only blank bottom rows `13..31`.
  - Painting sets `source.hasScreen = true`, triggering `MeshTerminalScreen.kt:692` to send `Hold(true)`. On the host (`mesh_bridge.rs:655`), `Hold(true)` sets `self.owe_frame = false`, **cancelling the owed `46×19` resize repaint** that was deferred behind the automatic `listShells()` call (`MeshTerminalScreen.kt:808`).
  - Furthermore, if `Rows` (`FrameDict`) is dropped as `Malformed` after `Profile` initialized `mirror.lines` to empty strings, `Commit` sets `painted = mirror.lines.isNotEmpty()` (`true`) and paints a blank grid with only the cursor.

### RC-12 · 10-second `CLAIM_RELEASE_MS` wedges the UI on `ProfileMissing` (`S14`), and `closeNow()` never releases `RadioLease` (`S17`)
- **Evidence**: `MeshTerminalScreen.kt:256, 425-439, 1017-1038`, `AndroidRadioLink.kt:1233, 1249`.
- **Mechanism**:
  - Leaving the app for 10 seconds calls `conn.driver.releaseShell()` (`MeshTerminalScreen.kt:425`), which sends `Detach` and sets `mirror.profile = null` while leaving `pick = Pick.Attached`.
  - On return (`:436`), `val id = resumeShell` is always `""` (never assigned anywhere in `MeshTerminalScreen.kt`), so `conn.driver.open(id)` never runs while `conn.driver.attaching` (`pick == Pick.Attached && mirror.profile == null`) is `true`, making `needList = false` — permanently wedging the screen on `"asking host for the shell"` (`ProfileMissing`).
  - `RadioLease.release(context)` (`ACTION_FREE`) is called only in `AndroidRadioLink.close()` (`:1233`, zero callers), never in `closeNow()` (`:1249`), so Meshmore is never notified when ldmux releases the radio.

---

## 3. Critique of prior plans and self-critique of our earlier draft

| Design flaw in prior plans / earlier draft | Why it fails under critical analysis | Corrected design in `docs/RADIO-PLAN.md` |
|---|---|---|
| **Keeping the incremental 7-layer split across Rust, Kotlin, and Java** | Leaves ~8,000 lines of duplicated companion/routing/protocol state machines in Kotlin/Java to drift from Rust and prevents running end-to-end phone↔mesh↔host tests in `cargo test`. | **Architectural Change A2**: Move the JNI boundary up (`crates/session-ffi`) so both host and phone run the **same** Rust Companion + `RadioEndpoint` + `mesh_term` engine; Android BLE becomes a ~200-line dumb byte pipe (`AndroidBlePipe.kt`). |
| **Keeping byte-stream `Arq` (`arq.rs`) + stateful `TermDecoder` underneath a batch RPC protocol** | In a byte stream, a lost fragment of a stale screen blocks all newer screens (head-of-line blocking), triggers mid-burst uplink `Ack`s after `seq=0` that collide with `seq=1..k`, and tears down the `StatelessTransportState` Noise session when retries exhaust. | **Architectural Change A4**: Replace byte-stream `Arq` + `TermDecoder` with a **Message-Oriented Reliable Transaction Layer (`tx_id`, `part_idx`, `part_count`, `ack_tx_id`, `ack_mask`)** directly over persistent `StatelessTransportState`. |
| **Sending an intermediate `Applied(settled=0)` at `1.2 s` and then a settled `Snapshot(settled=1)` at `2.0 s` (our earlier §6.3 draft!)** | If a command takes `1.5–3.0 s` (`git status`, `cargo check`), the `1.2 s` packet is still crossing hop 1–2 of a 14-hop chain when the `2.0 s` `Snapshot` is transmitted behind it, causing the two downlink waves to **collide with each other inside the repeater chain**! | **Single-Burst `3.5 s` Settle-or-Midstate Rule (§6.3)**: Host observes the pane for up to `3.5 s` (`500 ms` quiet after change or `700 ms` no change) and emits **EXACTLY ONE downlink burst** (`Applied + Snapshot(settled=1)` if settled within `3.5 s`, or `Applied + Snapshot(settled=0)` mid-state if still churning at `3.5 s`) and **never** transmits a second unsolicited wave. |
| **Running `LDRQ`/`LDRR` route discovery as a separate round trip before `msg1` during `connect()`** | Costs an extra flood round trip (15–45 s at range), and when `LDRQ`'s wait expires early, the host's `LDRR` collides head-on with the phone's `msg1`! | **1-Round-Trip Combined Discovery + Bootstrap (§5.1)**: `msg1` (`KIND_MSG1_FRAG`) teaches the host the route to the phone, and `msg2` (50 B) completes the handshake AND teaches the phone the route to the host in **one** round trip. |
| **Shrinking all records in an epoch to 93 B (64 B payload) (prior draft §3.3)** | Cuts direct-route capacity by >2.2× (`140 B → 64 B`), doubling packet counts for every screen repaint when a direct route works. | Full direct capacity (`min(172 − P_out, 171)`) on direct routes; if a `> 69 B` direct part must retry over flood, split that part into two `SUB_A`/`SUB_B` sub-halves carrying the same `(tx_id, part_idx)`. |
| **Re-sending `msg1` or cached `msg2` over Direct (`RAW_CUSTOM`) without a per-transmit nonce** | `SimpleMeshTables::wasSeen` (`Mesh.cpp:97`, `Packet.cpp:42`) hashes `[payload_type][payload]` and drops byte-identical direct retries at the first repeater that forwarded the first attempt! | Append a 2-byte random `tx_nonce: u16` to every `KIND_MSG1` / `KIND_MSG1_FRAG` / `KIND_MSG2` transmission so every retry has a unique OTA packet hash while hitting the responder's idempotency cache. |

---

## 4. Complete symptom-to-root-cause matrix (`S1`–`S20`)

| Id | Operator-reported symptom (with session timestamp) | Proven root cause(s) (`RC-1`..`RC-12`) | Owning stage in `RADIO-PLAN.md` |
|---|---|---|---|
| **S1** | **Handshake never completes at range** (30 s handshaking → link dropped → repeat; `2026-11-10 05:07, 06:20, 07:56, 07:58, 13:21`) | `RC-1` (precursor `SELF_ADVERT` + `LDRQ` + `LDRH` self-jam `msg1`), `RC-2` (`getContact` `ERR_CODE_NOT_FOUND` delivered to `sendRawPacket`, wiping `advertPath` or killing session), `RC-3` (1st-hop repeater self-echoes), `RC-4` (`answerRequest` double-wraps `LDRR`), `RC-5` (8 s `routeHops=null` retry + 24 s ephemeral-key abandon + non-idempotent `msg1` + direct `wasSeen` drop), `RC-6` (`logRxRaw` 173 B cliff). | S1, S2, S3 |
| **S2** | **Blank terminal with a misplaced cursor** after connecting (`2026-11-08 22:31`, `2026-11-09 13:32, 17:39, 18:00`) | `RC-11`: (a) 250 ms `onGrid` saves keyboard-down `46×32` grid before IME opens (`46×19`), and `placeGrid` (`GridPlacement.kt:129`) clips rows `0..12` (prompt/text) off the top of the screen; (b) `Hold(true)` clears `owe_frame = false` (`mesh_bridge.rs:655`), cancelling the `46×19` resize repaint deferred behind auto `listShells()`; (c) split `Profile`/`Rows`/`Commit` paints empty lines when `Rows` is dropped. | S4, S5 |
| **S3** | **"Waiting for the shell screen" for 40–75 s** (`2026-11-08 22:10`, `2026-11-10 00:27, 03:44, 07:12`) | `RC-6` (162 B flooded records dropped by `logRxRaw`), `RC-7` (mid-burst `Ack` collisions + `mesh.rs` `poll` before `inbound` handoff), `RC-11` (auto `listShells()` competing with `Attach`/`Resize`), `RC-12` (10 s `CLAIM_RELEASE_MS` wedge). | S2, S3, S4, S5 |
| **S4** | **Shell list empty or shows only 2 rows**, fixed by refresh (`2026-11-05 06:11`, `2026-11-09 16:01`) | `Sessions` pages had only a `last` bit and no `tx_id`/`part_idx`/`part_count`; lost initial pages or `pagesAsked` flip committed only the final 2-row page (`MeshTermDriver.kt:2026`). | S3, S4 |
| **S5** | **"Link dropped" while working, or works 2 s after refresh** (`2026-11-08 22:11`, `2026-11-10 03:13, 04:10`) | `RC-2` (background `startPathRefresh` `getContact` `ERR_CODE_NOT_FOUND` colliding with `sendRawPacket`), `RC-3` (repeater self-echoes), `RC-7` (session destroyed on single transaction timeout), plus stale cached adverts (`age=506s` 0-hop advert adopted at 16 hops). | S1, S2, S3 |
| **S6** | **"In use on desktop / take it back" shown when idle, on the phone's own shell, or simultaneously with "waiting for shell screen"** (`2026-11-06 20:32`, `2026-11-07 15:34`, `2026-11-10 03:44`) | `IC-1`: shared `"mesh-link"` device id on radio vs `config.deviceId` on tailnet, `Claim::engaged` always true for both kinds (`ownership.rs:68`), permanent `mesh_watched` exemption (`daemon.rs:2996`), plus overlapping `WaitingLine`/`HeldElsewhere` UI branches. | S4, S5 |
| **S7** | **Multiple shells opened from phone display the same output** (`2026-11-06 20:32`) | Single `mirror` / `MeshTerminalSource` per connection instead of per-shell state (`Architectural Disease 1`). | S5 |
| **S8** | **Multiline paste presses `Enter` on each newline and drops the rest** (`2026-11-07 21:34`) | `RC-8` (`sendToShell` sends separate records per run) + `RC-9` (`InputDelta.kt:54` splits `\n` into `Enter` keys) + `RC-7` (`requeue_remainder` stream corruption). | S3, S4, S5 |
| **S9** | **"Waiting for the answer" stays up after answer arrived; needs a second tap** (`2026-11-06 13:20`, `2026-11-10 00:05`) | Unindexed stream + separate `Commit{IDLE}` record lost or suppressed while `flushing` was true. | S4, S5 |
| **S10** | **Batch mode keeps counting up on `Show`; rotation/keyboard does not update screen** (`2026-11-10 00:01, 00:05`) | `RC-11` (`Hold(true)` clears `owe_frame = false` on host, cancelling `Resize` repaint while `onFrame()` re-queues `Hold`). | S4, S5 |
| **S11** | **Up button in batch mode says "no history on this link yet"** (`2026-11-09 21:35`) | `WantHistory` gated on `firstLine > 0` from a committed screen and timed with a flat 10 s deadline. | S4, S5 |
| **S12** | **Airtime alarm: 742.4 s in an hour / 34 records per idle minute** (`2026-11-08 06:21`, `2026-11-09 13:32`) | `RC-10` (`sessionUp` never set to `true`, disabling flood token bucket) + 50 ms `Differ` loop + 4 s resync retry livelock + `RECORD_FLOOD_COPIES = 3`. | S2, S3, S4 |
| **S13** | **Previous UI renders underneath "securing the link"** (`2026-11-06 20:46`) | `holding` branch in `MeshTerminalScreen.kt:998` rendered `TerminalSurfaceFor` behind handshake states. | S5 |
| **S14** | **Brief trip to home screen / back button leaves screen blank or stuck** (`2026-11-06 20:54`, `2026-11-10 13:24`) | `RC-12`: 10 s `CLAIM_RELEASE_MS` calls `releaseShell()` (`mirror.profile = null`, `pick = Pick.Attached`) while `resumeShell` is always `""`. | S5 |
| **S15** | **Desktop `Mod+Enter` flashes/closes or opens `xterm` instead of `alacritty`** (`2026-11-06 09:28, 22:19`) | Desktop terminal resolver read GNOME schemas on XFCE / bare WM (closed in ADR-0028; keeper display env documented in `AGENTS.md`). | Closed (ADR-0028) |
| **S16** | **Phone app stuck on handshaking / crashed at range** (`2026-11-10 13:27`) | 10-minute `WAKE_LOCK_MAX_MS` expired without renewal (`BackgroundSession.android.kt:63`) + `MeshTermDriver.lock` held for 5–38 s inside `refreshRoute()`. | S1, S5 |
| **S17** | **Meshmore and ldmux evicting each other's BLE GATT connection every 30 s** (`2026-11-10 13:26`) | `RC-12`: `AndroidRadioLink.closeNow()` (`:1249`) never called `RadioLease.release(context)`. | S1 |
| **S18** | **Host Heltec radio wedged on daemon restart** (`2026-11-05 13:44`, `2026-11-10 12:08`) | CDC-ACM asserted DTR/RTS on open (fixed in `bd73e93`), plus un-resynchronized frame parser when closing mid-frame (`framing.rs`). | S1 |
| **S19** | **"Works at home on private parameters, fails in the city on public mesh"** (`2026-11-08 14:17, 22:31`) | At 0 hops on 500 kHz bench tuple, airtime is 75 ms (8× faster), `P_arrival = 0`, no repeater self-jamming (`RC-1`), no repeater self-echo (`RC-3`), and zero-hop direct `RAW_CUSTOM` (`171 B`) bypasses the flood `logRxRaw` cliff (`RC-6`). | S1–S5 |
| **S20** | **Tailnet masking radio during testing** (`2026-11-10`) | Neither the app nor the host stated the active transport route on every event, so an observation made over the tailnet read as a radio result. | S0, S5 |

---

## 5. Cross-layer integration audit (`IC-1` through `IC-8`)

In addition to the 12 radio/protocol root causes (`RC-1`..`RC-12`), auditing the integration seams between `crates/daemon`, `crates/keeper`, `crates/companion`, `crates/protocol`, `crates/session-ffi`, and `packages/mobile` exposed **8 cross-layer integration bugs**:

### IC-1 · Tailnet ↔ Radio self-lockout and permanent desktop lockout (`ownership.rs:68`, `daemon.rs:2996`, `remote.rs:1290`, `mesh_bridge.rs:992, 2018`)
- **Mechanism**:
  1. On Tailnet (`remote.rs:1290`), the phone claims shells under `device.id = config.deviceId` (`kind = "phone"`), a random install UUID. On Radio (`mesh_bridge.rs:2018`), the host claims shells under `device.id = "mesh-link"` (`kind = "mesh"`).
  2. In `Ownership::claim` (`ownership.rs:150`), idempotent re-claim checks `e.device.id == device.id`, while `Claim::engaged` (`ownership.rs:68`) returns **`true` unconditionally forever** whenever `self.device.kind == "phone" || self.device.kind == "mesh"`.
  3. Consequently, when the **same phone** switches from Tailnet to Radio (or from Radio back to Tailnet), `e.device.id == device.id` is `false` and `e.engaged(now)` is `true`, so **the phone locks itself out of its own shell with `HeldBy` (`"in use on Pixel / radio · take it back"`, `S6`)**!
  4. Furthermore, because `daemon.rs:2996` (`if self.mesh_watched.contains(&sid) { continue; }`) exempts radio claims from `release_unbacked_claims` forever and `engaged()` is always `true` for `"mesh"`, a shell opened over radio and left idle locks out the desktop (`ldmux attach`) indefinitely until a manual `r` takeover.
  5. And when the desktop *does* take the shell back (`r`), `mesh_bridge.rs:992-1010` (`OWNER_CHECK` every 2 s) detects `r.owner != "mesh"` and **transmits an unsolicited `Message::Closed { reason: HELD }` over the LoRa radio** (triggering 5 ARQ retries if the phone is in a pocket or out of range!).

### IC-2 · `DaemonHost` in `mesh_bridge.rs` holds the global `daemon.lock().await` across synchronous keeper Unix-socket I/O and polls `list` every 2 seconds
- **Evidence**: `mesh_bridge.rs:996, 2033-2285` vs `remote.rs:2383` (`on_stream`).
- **Mechanism**:
  - `remote.rs:2383` carefully runs keeper calls on `PaneStream` via `tokio::task::spawn_blocking` outside `shared.daemon.lock().await` because keeper Unix socket calls are blocking.
  - `DaemonHost` in `mesh_bridge.rs:2033-2285` instead locks `self.0.lock().await` (the global `LdmuxDaemon` mutex!) across `handle("snapshot")`, `handle("send")`, `handle("resizePane")`, and `handle("list")` — and calls `self.host.listing().await` every 2 seconds (`OWNER_CHECK`, `:996`), querying `/proc` and every keeper session under the global daemon lock!

### IC-3 · Three-loop host task split (`mesh_serial.rs` ↔ `mesh.rs` `Exchange` mutex ↔ `mesh_bridge.rs` 50 ms ticker) drops inbound records and prevents ACK piggybacking
- **Evidence**: `mesh.rs:1044, 1165, 1185, 1254`, `mesh_session.rs:25`, `mesh_bridge.rs:958`.
- **Mechanism**:
  - `mesh.rs` polls `SessionHost::poll_with_seq()` at line 1044 **before** moving `h.inbound` into the `Arc<Mutex<Exchange>>` queue (`INBOUND_MAX = 1024`) at line 1254, and `bridge_loop` wakes on a separate 50 ms timer (`mesh.rs:1165`).
  - As a result, when a request arrives from the phone, `SessionHost` cannot piggyback the ARQ `Ack` onto the bridge's reply in the same turn, and if `Exchange.inbound` fills during a burst, decrypted messages are silently dropped after `Session` already acknowledged their sequence numbers!

### IC-4 · Crate dependency contamination across the Android JNI boundary (`crates/companion/Cargo.toml`, `crates/protocol/Cargo.toml`, `crates/session-ffi/Cargo.toml`)
- **Evidence**: `crates/companion/Cargo.toml:10-20`, `crates/protocol/Cargo.toml:10-22`, `crates/session-ffi/Cargo.toml:14-18`.
- **Mechanism**:
  - `crates/companion` unconditionally pulls in `tokio` and `nix` (`term`, `ioctl`, `poll`) for host USB serial I/O alongside pure byte framing (`codec.rs`, `framing.rs`).
  - `crates/protocol` unconditionally pulls in `tokio` (`net`, `rt`, `macros`) and `nix` (`user`, `fs`) for host Unix sockets alongside pure `mesh_term.rs` (`flate2` with `zlib-rs`) and `pairing.rs`.
  - Without splitting or feature-gating the host OS I/O modules (`serial` / `host-ipc`), `ldmux-session-ffi` could not link `ldmux-protocol::mesh_term` or `ldmux-companion::codec` into `libldmux_session.so` without dragging `tokio` and Linux `termios` into the Android NDK build.

### IC-5 · Provisional draft echo in `MeshTerminalSource.kt:606-612` writes raw `\r\n` with `DECAWM` off into `libghostty-vt`
- **Evidence**: `MeshTerminalSource.kt:556-615`, `MeshScreenBytes.kt:29-59`.
- **Mechanism**:
  - `MeshTerminalSource.paint()` writes `echoText` wrapped in `\u001b[?7l\u001b[2m … \u001b[22m\u001b[?7h` directly into `TerminalEngine` at `(cursorX, cursorY)`.
  - When the user presses `Enter` while the prompt is on the bottom row (`cursorY == rows - 1`), writing `\r\n` scrolls `TerminalEngine` up by one line (`\u001b[?7l` disables autowrap, not `LF` scrolling!), shifting the entire live screen out of row alignment before the command is even sent!
  - Conversely, when the user types a command wider than `cols - cursorX`, `\u001b[?7l` (autowrap off) causes every character past column `cols - 1` to overwrite the last cell on top of itself so the user cannot read what they are typing.
  - And in `MeshScreenBytes.kt:46`, `cleanRow(text, cols)` truncates at `cols` UTF-16 code points rather than display cells, clipping rows early whenever combining marks are present.

### IC-6 · Multiline IME paste in `InputDelta.kt:54` bypasses bracketed paste on BOTH Tailnet and Radio
- **Evidence**: `TerminalSurface.android.kt:870-875`, `InputDelta.kt:42-60`.
- **Mechanism**:
  - On Android, pasting multiline text from the soft keyboard's clipboard strip or long-press menu arrives through `BasicTextField.onValueChange` → `inputOpsFor(field, next)`.
  - Because `inputOpsFor` splits every `\n` into `InputOp.Keys(listOf("Enter"))` and emits `InputOp.Text` for each line instead of `InputOp.Paste`, pasting multiline text from Gboard bypasses `TerminalHandle.paste()` (`\x1b[200~…\x1b[201~`) on **both** Tailnet and Radio!

### IC-7 · Voice dictation on Radio sends unreviewed speech straight into the batch stream (`MeshTerminalScreen.kt:1322`)
- **Evidence**: `MeshTerminalScreen.kt:1322`, `AppContent.kt:694`.
- **Mechanism**:
  - When voice dictation finishes a phrase, `Dictation` calls `handle.sendText(it)`. In a unified batch model, dictated text must land in the shell's local draft (`DraftItem.Text`) and render in the provisional echo so the user can see and edit a misheard word before tapping `Send`.

### IC-8 · `native-freshness.test.ts:178` hardcodes `Java_dev_ldmux_` and never checks `libldmux_session.so`
- **Evidence**: `packages/mobile/test/native-freshness.test.ts:52, 65, 178`.
- **Mechanism**:
  - Line 178 checks `Java_dev_ldmux_mobile_term_NativeTerminal_${name}` instead of `Java_io_ldmux_...`, failing whenever `jniLibs/` is populated, and `NOT_IN_TERMINAL_LIB` excludes `ldmux_session.h` and `ldmux_session_jni.c` without a corresponding freshness and symbol check for `libldmux_session.so` (`NativeMeshSession.kt`).

---

## 6. Consolidated physical measurements, firmware verification, and `9cf722e` ledger disposition

This section consolidates the verified physical and firmware facts from the `9cf722e` field notes and 320-entry audit ledger (`MESH-RADIO-CONSIDERATIONS.md`, `RADIO-AUDIT-LOOP.md`, and `RADIO-AUDIT-DISPOSITION.md`, preserved in git at `9cf722e`):

### 6.1 Verified physical & RF measurements (`910.525 MHz / 62.5 kHz / SF7 / CR5`, preamble 32)
- **Hardware**: Host Heltec V3 (ESP32-S3 over USB CDC-ACM serial) and Phone Seeed Wio Tracker L1 (nRF52840 over Nordic UART BLE, MeshCore v1.17.1).
- **Measured field signal at range (2026-11-10)**: nearest repeaters arrived at **−107 to −124 dBm**, **SNR −6.75 to +5.5 dB** (and −8.5 dB on a 34-hop flood), vs **−84 to −88 dBm** at home. Semtech SX126x SF7 demodulation floor (`RadioLibWrappers.cpp:310-317`) is **−7.5 dB SNR** (~−126 dBm sensitivity at 62.5 kHz), so range links operate within 1–6 dB of the demodulation floor.
- **Verified Semtech time-on-air (`codec.rs::time_on_air_ms`, 2.048 ms/symbol)**:
  - `42 B` (`LDRQ`/`LDRR`): **223.7 ms**
  - `65 B` (`KIND_MSG1_FRAG`): **305.2 ms**
  - `93 B` (guaranteed flood part): **387.1 ms**
  - `121 B` (unfragmented `msg1`): **459.3 ms**
  - `172 B` (full direct record): **602.6 ms**

### 6.2 Verified MeshCore C++ firmware invariants (`V1`–`V20`)
1. **Receive-log 173-byte ceiling (`MyMesh.cpp:287-296`, `V3`)**: `logRxRaw` pushes `PUSH_CODE_LOG_RX_DATA (0x88)` only when `len + 3 <= MAX_FRAME_SIZE (176)` (`len <= 173 B`). Any OTA packet `> 173 B` at the receiving companion is silently dropped from `0x88`.
2. **Score-based RX delay is OFF by default (`V2`)**: `Dispatcher::calcRxDelay` is overridden in `simple_repeater/MyMesh.cpp:594,956` and `companion_radio/MyMesh.cpp:274,934` with `_prefs.rx_delay_base = 0.0f` (`if (_prefs.rx_delay_base <= 0.0f) return 0;`), so the score table is a scheduling heuristic, not a delivery probability, and it is not what delays a flood.
3. **Actual repeater per-hop delay (`V6`)**: `getRetransmitDelay` (`simple_repeater/MyMesh.cpp:597-605`) draws `nextInt(0, 5 * airtime * tx_delay_factor + 1)` with default `tx_delay_factor = 0.5` for flood (mean `1.25 × airtime`, max `2.5 × airtime`) and `direct_tx_delay_factor = 0.3` for direct (mean `0.75 × airtime`, max `1.5 × airtime`), so a flood crossing is about 1.3× a direct one at the same hop count, not the 6.7× an earlier note claimed.
4. **Shipped flood hop ceilings (`V5`)**: `simple_repeater/MyMesh.cpp:970-972` sets `flood_max = 64`, `flood_max_unscoped = 64`, and `flood_max_advert = 8` (Canadian community presets often configure `flood.max = 16`).
5. **Deduplication table (`Packet.cpp:22-45`, `SimpleMeshTables.h:22-72`)**: `calculatePacketHash` hashes `[payload_type][path_len if TRACE][payload]` into a 160-entry FIFO ring (`MAX_PACKET_HASHES = 160`) checked on both flood and direct packets (`Mesh.cpp:97, 141`), and `CMD_SEND_RAW_PACKET` (`MyMesh.cpp:2050`) does **not** call `markSeen`, so a sender hears its own first-hop repeater retransmission on `0x88` (`RC-3`).
6. **BLE companion UART queues (`nrf52/SerialBLEInterface.cpp:375-393`, `esp32/SerialBLEInterface.h:27`)**: nRF52 `onBleUartRX` drains `bleuart.available()` as a single frame and drops the buffer if `avail > 176` (`RC-2`); ESP32 `send_queue` has only `FRAME_QUEUE_SIZE = 4` slots drained at `60 ms` intervals (`RC-6`).

### 6.3 Disposition summary of the `9cf722e` 320-entry ledger
- Of the 320 `A-*`/`M-*` entries recorded at `9cf722e:docs/RADIO-AUDIT-LOOP.md`:
  - **236 confirmed**, **24 open questions** (all resolved in `RADIO-PLAN.md` §2), **23 conditional**, **20 duplicates**, **6 sound baselines retained**, **6 superseded by the batch transaction architecture**, and **4 refuted**: the SNR score read as a loss probability, the 5-hop unscoped flood cap, the 6.7× flood/direct cost ratio, and the claim that the shipped firmware keeps `flood_max_unscoped` at 0.
  - All surviving entries and the 20 new root-cause and integration findings (`RC-1`..`RC-12`, `IC-1`..`IC-8`) map to Implementation Stages `S0`–`S5` and Verification Gates `B1`–`B8` in [`docs/RADIO-PLAN.md`](RADIO-PLAN.md).

