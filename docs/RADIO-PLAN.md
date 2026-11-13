# The radio path: architectural redesign and plan of record

## 0. Status and reading order

- Documentation-only architectural specification and plan of record, revised 2026-11-11 against `ldmux` `9cf722e`.
- Implementation and deployment remain paused until the operator authorizes execution.
- [`docs/RADIO-AUDIT-REVIEW.md`](RADIO-AUDIT-REVIEW.md) is the companion root-cause, firmware, field-log, and cross-layer integration audit (`RC-1`..`RC-12`, `IC-1`..`IC-8`, `S1`..`S20`, and the consolidated verification of the `9cf722e` audit scratchpads).
- [`docs/adr/0039-batch-transaction-radio-architecture.md`](adr/0039-batch-transaction-radio-architecture.md) records the architectural decision in the ADR index.
- **Citation convention.** `A1`–`A5` are the transformations in §1, `B1`–`B8` the verification gates in §9, `S0`–`S5` the stages in §10, `RC-n`/`IC-n`/`S1`–`S20` the findings in the review, and `A-nnn` ids are the ledger entries readable at `git show 9cf722e:docs/RADIO-AUDIT-LOOP.md`.
- `docs/ARCHITECTURE.md` describes the currently deployed code and links here for the radio redesign.

---

## 1. Large-scale architectural transformations (`A1`–`A5`)

The radio path grew to **38,000+ lines across Rust, C-FFI, Kotlin, and Java** because it was built by bolting a 50 ms live-streaming multiplexer (`mesh_bridge.rs`, 6,305 lines) and a TCP-like byte-stream ARQ (`arq.rs`, 1,060 lines) onto a half-duplex multi-hop LoRa mesh, while duplicating the companion, routing, and wire protocols across Rust, Kotlin, and Java.

Rather than patching symptoms in place, this specification executes **five large-scale architectural transformations** that delete ~12,000+ lines of duplicated/streaming machinery and replace them with a deterministic, end-to-end-testable batch transaction architecture:

### A1 · Unify `RouteTable`, `Carrier`, `Handshake`, and `TransactionLayer` into a single pure-Rust `RadioEndpoint` state machine (`crates/mesh` + `crates/session`)
- **Problem eliminated**: Previously, `Session::poll_with_seq` (`crates/session`) sat above an opaque `send(record: &[u8])` boundary and had to seal ciphertext to a scalar `record_limit` **before** the carrier (`mesh_serial.rs` / `AndroidRadioLink.kt`) decided whether `Attempt k` would travel Direct (`RAW_CUSTOM`, up to `172 - P_out` B) or Flood (`LDMR`, `93..157` B). That inversion caused the 162 B `logRxRaw` (`0x88`) cliff (`RC-6`) and the `requeue_remainder` stream corruption (`RC-7`).
- **New architecture**:
  - One deterministic, I/O-free Rust state machine (`RadioEndpoint`) owns route state (`RouteTable`), per-attempt carrier selection (`carrier_for(route, attempt)`), bootstrap fragmentation (`KIND_MSG1_FRAG`), idempotent handshake responder/initiator, and message-part sealing (`KIND_UP` / `KIND_DOWN`).
  - On every transmit poll, `RadioEndpoint` selects the carrier (`Direct(path)` vs `Flood(scope)`) for **that specific attempt first**, computes the exact byte ceiling for that carrier, seals the message part (or sub-half) to fit that exact ceiling, and outputs a ready-to-send MeshCore OTA packet (`<= 174 B` TX, guaranteed `<= 173 B` at the receiver's `0x88` log).
  - Inbound packets are filtered by direction (`KIND_UP = 0x13` for `Phone → Host`, `KIND_DOWN = 0x16` for `Host → Phone`) and `view.path.hops == 0` on direct packets, so **first-hop repeater self-echoes (`RC-3`) are rejected in 1 byte before touching AEAD**, and `RouteTable` updates **only after cryptographic verification succeeds**.

### A2 · Move the Android JNI boundary up (`crates/session-ffi`): one shared Rust radio core on both Host and Phone
- **Problem eliminated**: Although the Android app already loads `libldmux_session_jni.so` (`crates/session-ffi`), it previously exposed only `Handshake` and `Session`, leaving 8,000+ lines of companion framing (`MeshcoreSession.java`, `MeshcoreFrameCodec.java`), packet routing (`RadioRoute.kt`, `AndroidRadioLink.kt`), and terminal wire codecs/drivers (`MeshTerm.kt`, `MeshTermDriver.kt`) duplicated in Java and Kotlin (`RC-2`, `RC-4`, `RC-5`, `RC-8`, `RC-10`).
- **New architecture**:
  - Both host and phone execute the **exact same Rust companion driver (`crates/companion`)**, the **exact same `RadioEndpoint` (`crates/mesh` + `crates/session`)**, and the **exact same `mesh_term` codec and per-shell screen mirror (`crates/protocol`)**.
  - On Android, the BLE layer (`AndroidBlePipe.kt`, ~200 lines) is reduced to a **pure byte-frame pipe** (GATT write to `6E400002`, GATT notify from `6E400003`, `RadioLease`, and `WakeLock`) feeding raw companion frames into the Rust engine over JNI.
  - **End-to-end `VirtualMesh` verification**: In `cargo test`, the real Rust Phone Engine and the real Rust Host Engine run directly against each other across an in-process simulated multi-hop repeater mesh (`VirtualMesh`: 0–63 hops, 1B/2B/3B path hashes, `logRxRaw` 173 B filter, `SimpleMeshTables` 160-entry dedup, repeater half-duplex `txdelay` collisions, packet loss, and asymmetric paths).

### A3 · Replace the Host's 6,305-line 50 ms streaming `MeshBridge` and lossy `Exchange` ring with a single-task Demand-Driven RPC Transaction Engine (`MeshRpcService`, ~800 lines)
- **Problem eliminated**: `crates/daemon/src/mesh_bridge.rs` (6,305 lines) ran a 50 ms continuous `Differ` polling loop across a lossy `Arc<Mutex<Exchange>>` queue (`mesh.rs:1165`), streaming unsolicited `Profile`/`Rows`/`Commit` frames and relying on a phone-sent `Message::Hold { on: true }` brake pedal that cancelled resize repaints (`owe_frame = false`, `RC-11`) and burned 34 records/min on idle (`S12`).
- **New architecture**:
  - Delete the 50 ms `Differ` loop, `Attached::hold`, `owe_frame`, `resync_wanted`, `flushing`, `Commit{IDLE}`, and the `Exchange` mutex split between `mesh.rs` and `mesh_bridge.rs`.
  - Replace `mesh_bridge.rs` with a **demand-driven transaction service (`MeshRpcService`)** invoked directly when a complete request transaction arrives:
    - The host transmits **zero unsolicited terminal frames** on the radio route.
    - Every phone request (`List`, `Attach`/`Takeover`/`Create`, `Send`, `Fetch`, `WantHistory`, `Close`, `Detach`) is a discrete transaction that produces **exactly one** atomic response burst (`Sessions`, `Snapshot`, `Applied + Snapshot`, `HistoryPage`, `Closed`) with the request's `ack_tx_id` piggybacked in the response header.

### A4 · Replace byte-stream `Arq` + `TermDecoder` with a Message-Oriented Reliable Transaction Layer (`tx_id`, `part_idx`, `part_count`, `ack_tx_id`, `ack_mask`) over persistent `StatelessTransportState`
- **Problem eliminated**: Byte-stream `Arq` (`arq.rs`) + stateful `TermDecoder` (`mesh_term.rs`) caused permanent head-of-line blocking when any fragment of a stale screen was lost, fired mid-burst uplink `Ack`s (`ack_delay_ms = 1.2 s`) after `seq=0` that collided with `seq=1..k` at repeaters, and destroyed the `StatelessTransportState` Noise session whenever a single record exhausted retries (`RC-7`).
- **New architecture**:
  - Each logical request or response is assigned a monotonic `tx_id: u16` and split into `part_count: u8` (`1..=16`) parts (`part_idx: u8`).
  - **95% of messages (`Send`, `Fetch`, `Attach`, `List`, `Closed`, and diff `Snapshot`s) have `part_count == 1` (`part_idx == 0`)** — one self-contained packet.
  - Multi-part responses (such as a full-screen `Snapshot` of 2–4 parts) carry `(part_idx, part_count)` so the receiver knows `remaining = part_count - 1 - part_idx` parts are still coming in the burst and **holds its selective `Ack(ack_tx_id, ack_mask)` until `part_idx == part_count - 1` arrives (or the burst-tail timer expires)** — eliminating mid-burst ACK collisions by construction.
  - **Clean superseding (zero head-of-line blocking)**: A newer `Snapshot` (`tx_id = 11`) cleanly supersedes an incomplete older `Snapshot` (`tx_id = 10`) without corrupting any stream parser.
  - **Persistent `StatelessTransportState` across transaction timeouts**: If a transaction times out during an RF dead spot, **only that transaction fails (`Outcome::TimedOut`); the `Noise_IKpsk2` `StatelessTransportState` session stays UP**! The next tap of `Refresh` or `Send` works immediately in 1 packet with zero handshake. If the host genuinely restarted (`session == None`), it answers an unknown `KIND_UP` packet with a 25-byte PSK-authenticated `KIND_NEED_HANDSHAKE (0x15)` record so the phone re-handshakes immediately in 1 round trip instead of waiting minutes.

### A5 · Unify Mobile App shell management, 5-shell pager, per-shell state, viewport sizing, and device ownership across Tailnet and Radio
- **Problem eliminated**: `App.kt` forked the mobile UI into `AppContent.kt` and `MeshTerminalScreen.kt` (1,698 lines), causing cross-shell screen bleed (`S7`), missing the 5-shell cap (`D-7`), leaving `resumeShell = ""` unassigned (`S14`), detaching after 10 s in background (`CLAIM_RELEASE_MS`, `S14`), and clipping rows `0..12` off the top of the screen on keyboard-down `46×32` grids (`S2`, `RC-11`).
- **New architecture**:
  - Share **one** multi-shell page model (`PageOrder` capped at `MAX_OPEN_PAGES = 5`, `shownSessionId` / `resumeShell` tracked by shell ID), **one** per-shell state map (`ShellPageState` holding each shell's `TerminalEngine`, `ScreenMirror`, `snapshotId`, `MeshHistory` + scroll offset, and `MeshInputBatcher` draft), **one** keyboard-up-gated viewport measurement + cursor-aware `GridPlacement`, and **one** cryptographic owner identity across Tailnet and Radio (`device.id = hex(phone_static_pub[..8])`, §8.3).

---

## 2. Product requirements and resolved operator decisions

All product decisions are frozen from the operator's prompts and structured questionnaire answers through 2026-11-11:

| Aspect | Specification |
|---|---|
| **Radio interaction model** | Tap to send, tap to receive. Live character-at-a-time streaming on the radio route is removed; tailnet streaming is unchanged. |
| **Controls** | Two distinct controls on the batch bar: **`Send`** (sends the immutable draft; dimmed/disabled when draft is empty) and **`Refresh`** (fetches the current screen via `Fetch`; never sends or touches the draft). Disabled controls stay visible and dimmed, never disappearing. |
| **Draft accumulation & `Enter`** | Typing and key presses (`Enter`, `Tab`, arrows, `C-c`, sticky modifiers) accumulate locally in the shell's draft and render as faint provisional local echo on top of the last committed screen. Pressing `Enter` appends an `Enter` key action (`⏎`) to the draft; tapping `Send` transmits the exact accumulated draft as one atomic operation without inventing an extra unrequested `Enter`. |
| **Multiline paste** | Both the bar's `Paste` button and multiline IME insertions (`onValueChange` delta `> 1` char containing `\n`) produce a `Paste` item in the draft. When sent, the host wraps the complete assembled paste in `\x1b[200~ … \x1b[201~` if the pane currently has `MODE_BRACKETED_PASTE` enabled; if not, newlines are converted to `\r` only when the paste is explicitly sent, never split into per-line records on the phone. |
| **Single-burst settle or mid-state answer (`3.5 s`)** | Each `Send` produces **at most one automatic downlink burst** from the host. After writing to the PTY, the host observes the pane for up to `INLINE_SETTLE_MAX_MS = 3_500 ms` (`500 ms` quiet after first screen change, or `700 ms` if no change occurred). If settled within `3.5 s`, the host returns `[Applied(Ok, settled=1) + Snapshot(settled=1)]` in one burst. If still churning at `3.5 s` (e.g. `cargo build`, `tail -f`), the host captures the live mid-state screen at `3.5 s` and returns `[Applied(Ok, settled=0) + Snapshot(settled=0)]` in one burst, then stops transmitting so the bar shows `applied · output still changing · tap refresh` with zero colliding follow-up waves. |
| **Queueing & interrupting** | A second `Send` pressed while an earlier `Send` is in flight queues in order behind it. Tapping `Refresh` sends `Fetch` and returns one current `Snapshot` without cancelling or re-sending any `Send`. |
| **Per-shell state & 5-shell cap** | Up to **5 concurrently open shells** (`MAX_OPEN_PAGES = 5`), matching the tailnet route. Each open shell retains its own `mirror`, `snapshot_id`, `draft`, `history` cache, and scroll anchor (`offset`). Switching shells immediately displays that shell's cached screen marked `stale` and issues one `Attach`/`Fetch` (diffed against that shell's `snapshot_id`). |
| **Drop & restart recovery** | Because `StatelessTransportState` is preserved across transaction timeouts, a transient drop never requires a re-handshake; the screen stays on `resumeShell` marked `stale`. After a cold app restart, the app opens on the shell list with no automatic input replay. |
| **Backgrounding & lifecycle** | Going to the background scrubs only **unsent local draft text** (and stops voice dictation). It does **not** send `Detach` after 10 s (`CLAIM_RELEASE_MS` is removed) and keeps `LdmuxService` + BLE active while an operation is owed (up to `60 s`) plus a 5-minute idle grace, renewing the partial `WakeLock` on a 5-minute ticker. |
| **Notifications & marks** | When an owed answer `Snapshot` arrives while the app is off screen, post one notification per shell **without terminal text**, cleared when that shell is opened. Agent `done`/`attention` marks (`ldmux buzz`) remain a separate mechanism shown on launcher rows and replayed on `List`/`Attach`. |
| **Airtime & cost display** | The law or nothing: enforce the 1 % (36 s/hr) / 0.1 % (3.6 s/hr) cap only inside EU regulated sub-bands (`868.0–868.6`, `868.7–869.2`, `869.4–869.65 MHz`); no artificial cap on US/AU/NZ 902–928 MHz. No cost lecture or record-count badge on the terminal screen. |
| **Path hash width** | Read and follow the radio's configured `path_hash_mode` (`1..=3` bytes) by default so public 1-byte meshes and regional 2-byte/3-byte meshes both work out of the box without silent mutation; allow explicit width selection in the radio preset panel. |
| **Ownership & "take it back"** | The authenticated phone identity is the phone's own session static key (`device.id = hex(phone_static_pub[..8])`, §8.3), identical over tailnet and radio. Mesh claims survive re-handshakes (`max(2 × crossing, 180 s)` grace), require no periodic heartbeat, and are immediately displaceable by local desktop typing or `ldmux attach` (answering `Closed { reason: HELD }` on the phone's next `Send`/`Fetch`). `"in use on desktop / take it back"` is drawn **only** inside an open shell page when `Closed { HELD }` is returned — never on the launcher home screen and never simultaneously with `"waiting for shell screen"`. |

---

## 3. Layer 0: Companion Framing, Strict Command Serialization, and BLE/Serial Drivers

### 3.1 Stock companion framing and limits

- Both endpoints target stock MeshCore companion firmware (`v1.16.0+`, `v1.17.x`, `v1.18.x`) with `MAX_FRAME_SIZE = 176`, `MAX_PATH_SIZE = 64`, `MAX_PACKET_PAYLOAD = 184`, ESP32 BLE `FRAME_QUEUE_SIZE = 4` (`60 ms` interval), and nRF52 `BLEUart` RX ring buffer (`176 B` max per drain).
- Outbound raw packets use `CMD_SEND_RAW_PACKET (0x41)`: `[0x41][priority: u8][packet: 1..=174 B]`.
- Inbound packets are read from `PUSH_CODE_LOG_RX_DATA (0x88)`: `[0x88][snr_q4: i8][rssi: i8][packet: 1..=173 B]`. Because `MyMesh::logRxRaw` guards on `len + 3 <= 176`, any packet whose on-air length at the receiving radio exceeds **173 bytes** is silently dropped by the companion firmware. (When `0x88` is active, duplicate `0x84` `PUSH_CODE_RAW_DATA` frames for the same direct packet are deduplicated by `(kind, counter)` / `tx_nonce`.)
- Raw-send confirmation accepts both `RESP_CODE_OK (0x00)` (v1.17) and `RESP_CODE_SENT (0x06)` (10 bytes: `[0x06][is_flood: u8][tag: u32 LE][est_timeout_ms: u32 LE]`).

### 3.2 Strict one-command-at-a-time FIFO serialization (shared across Host Serial and Phone BLE)

- **Why strict serialization is mandatory (`RC-2`)**:
  - The MeshCore companion protocol has a single command-response window with no sequence numbers, and on nRF52 BLE companions (`nrf52/SerialBLEInterface.cpp:375-393`) overlapping BLE writes coalesce in `bleuart` or overflow `MAX_FRAME_SIZE (176)` and get dropped.
  - Furthermore, `CMD_GET_CONTACT_BY_KEY (0x1E)`, `CMD_GET_ADVERT_PATH (0x2A)`, and `CMD_GET_CHANNEL (0x1F)` all return `RESP_CODE_ERROR (0x01)` (`ERR_CODE_NOT_FOUND = 2`) when absent.
- **Specification**:
  - All companion commands (`APP_START`, `DEVICE_QUERY`, `SET_DEVICE_TIME`, `GET_DEFAULT_FLOOD_SCOPE`, `GET_BATT_AND_STORAGE`, `GET_ADVERT_PATH`, `SET_RADIO_PARAMS`, `SET_RADIO_TX_POWER`, `SET_PATH_HASH_MODE`, `SEND_RAW_PACKET`) enter **one strict FIFO command queue** where at most **one** command is in flight to the radio at any instant, and the next command is written only after the in-flight command's expected typed response, `OK`, `SENT`, `DISABLED`, or `ERROR` frame arrives (or its command timeout expires).
  - Delete `MeshcoreSession.awaitsOkOrError` (`MeshcoreSession.java:685-705`), make `requestBatteryStorage` (`0x14`) tracked, and delete all calls to `getContact(hostKey)` (`0x1E`) and `resetPath(hostKey)` (`0x0D`) in `AndroidRadioLink.kt`.
  - Delete connect-time `sendSelfAdvert(true)` and `maybeAdvertise()` (`RC-1`).
- **Host serial supervisor (`crates/companion` & `crates/daemon/src/mesh_serial.rs`)**:
  - Remove `std::thread::sleep(reply_delay)` from `send_route_reply` and remove blocking sleeps from `send_retrying` inside the push handler; use a non-blocking `VecDeque<ScheduledSend>` (`not_before: Instant`) drained between `1 ms` serial reads.
  - Frame parser resynchronization (`crates/companion/src/framing.rs`): if a `<` / `>` frame header declares `len == 0` or `len > MAX_FRAME_SIZE (176)`, scan forward for the next valid frame start byte instead of reading `len` garbage bytes.
  - Separate `last_serial_event` (local radio health) from `last_peer_rx` (peer radio traffic), so a self-generated 500 ms status report never counts as the peer being heard.
- **Phone `RadioLease` and `WakeLock` (`RadioLease.kt`, `AndroidRadioLink.kt`, `BackgroundSession.android.kt`)**:
  - Call `RadioLease.release(context)` (`ACTION_FREE`) inside `AndroidRadioLink.closeNow()` (`RC-12` / `S17`).
  - Renew `wakeLock.acquire(WAKE_LOCK_MAX_MS)` on a 5-minute coroutine ticker inside `LdmuxService` while active (`S16`).

---

## 4. Layer 1: Unified `RadioEndpoint` Carrier, Byte Budgets, Self-Echo Filtering, and Route Ranking

### 4.1 Exact byte budgets

Let `P_out = hops_out × hash_width` be the outbound path byte length (`0..=64`), `P_arrival = hops_in × hash_width` be the accumulated flood travel path byte length at the receiver (`0..=64`), and `S = 4` when a flood carries region `transport_codes` (`ROUTE_TYPE_TRANSPORT_FLOOD = 0x00`) or `0` when unscoped (`ROUTE_TYPE_FLOOD = 0x01`).

| Packet kind | On-air / frame formula | Receiver `0x88` bound (`≤ 173 B`) | Max usable record `R` |
|---|---|---|---|
| **Direct record (`RAW_CUSTOM` `0x3E`)** | Tx: `2 + P_out + R ≤ 174`; Rx: `P_arrival = 0` (`removeSelfFromPath`), `2 + R ≤ 173` | `R ≤ min(172 − P_out, 171)` (`view.path.hops == 0` required on RX!) | **171 B** (0 hops) · **156 B** (16×1B) · **140 B** (16×2B) · **108 B** (64B path) |
| **Flood record (`REQ` `LDMR`)** | `2 + S + P_arrival + 10 + R ≤ 173` | `R ≤ 161 − S − P_arrival` | **93 B** guaranteed at `P_arrival ≤ 64, S = 4` (**125 B** at `P_arrival ≤ 32, S = 4`) |
| **Route request (`REQ` `LDRQ`)** | `2 + S + P_arrival + 2 + 4 + 32 + 4 = 44 + S + P_arrival` | `48 + P_arrival ≤ 173` (`P_arrival ≤ 125`, always fits `MAX_PATH_SIZE = 64`) | Fixed 42 B payload |
| **Route reply (`REQ` `LDRR`)** | Compact format: `[hdr][codes?][path_byte][P_arrival][dest_h][src_h]["LDRR"][own_key: 32][nonce: 4]` = `44 + S + P_arrival` | `48 + P_arrival ≤ 173` (`P_arrival ≤ 125`, always fits `MAX_PATH_SIZE = 64`) | Fixed 42 B payload (travel path `P_arrival` **is** the route) |

- **Why `LDRR` drops the inner carried path**: a flooded `LDRR` accumulates its own travel path `P_arrival` hop-by-hop from responder to requester. Reversing `P_arrival` at the receiver gives the exact chain the reply just crossed, whereas carrying an inner path bloated `LDRR` by up to 69 bytes (`RC-6`).
- **Fix phone `answerRequest` (`RC-4`)**: when an `LDRQ` arrives from the peer, adopt its reversed travel path (`P_arrival`) as our outbound candidate and send the compact `LDRR` packet directly via `sendRawPacket(reply)` — **never** passing `reply` into `send(record)`!
- **Retire `RouteHint` (`LDRH`) (`RC-1`)**: delete `RouteHint.kt` and `route_hint()`.

### 4.2 Self-echo filtering and post-authentication route updates (`RC-3`)

- **Direction-tagged record kinds**:
  - `KIND_MSG1 (0x11)`, `KIND_MSG1_FRAG (0x14)`, and `KIND_UP (0x13)` are strictly **`Phone → Host`**. The Phone immediately ignores any received packet with `kind ∈ {0x11, 0x13, 0x14}` as a repeater self-echo of its own transmission.
  - `KIND_MSG2 (0x12)`, `KIND_NEED_HANDSHAKE (0x15)`, and `KIND_DOWN (0x16)` are strictly **`Host → Phone`**. The Host immediately ignores any received packet with `kind ∈ {0x12, 0x15, 0x16}` as a repeater self-echo of its own transmission.
- **Zero-hop requirement on inbound direct `RAW_CUSTOM` (`0x88`)**:
  - In `incoming_record`, require **`view.path.hops == 0` (`view.path.bytes.is_empty()`)**. A `ROUTE_TYPE_DIRECT` packet with `view.path.hops > 0` still has unconsumed repeater hops and is in transit to another node (`Mesh.cpp:78-107`).
- **Post-authentication route updates only**:
  - `RouteTable::on_authenticated_peer_rx(carrier, snr, rssi)` is called **only after** a packet from the peer passes AEAD tag verification (`StatelessTransportState::read_message`), `Handshake::accept`/`finish`, or PSK HMAC verification. An unauthenticated frame or self-echo can never touch `last_peer_rx`, `proven`, or `srtt`.

### 4.3 Per-attempt carrier selection and SNR-margin route ranking

- Delete `directWorks`, `heardByFlood`, `directsSinceStale`, `peer_via_flood`, `answer_flood_with_flood`, and `pathless_directs`.
- **Pure per-attempt function** (`carrier_for(route, attempt)`):
  - **`Attempt == 1` (first transmission of a transaction part or bootstrap initiation)**:
    - If `route` holds a non-expired candidate or proven path (`ZeroHop` or `Hops { bytes, .. }`): send **Direct (`RAW_CUSTOM`)** along `route` at full direct byte capacity (`min(172 − P_out, 171)`).
    - If `route` is `Unknown`: send **Flood (`LDMR`)** at the flood-safe size (`93 B`).
  - **`Attempt >= 2` (retransmission after `Attempt 1` timed out with no peer ACK)**:
    - Send **Flood (`LDMR`)** (`1` copy per retransmission attempt; if a `> 69 B` direct part is being retried over flood, seal it as `SUB_A` + `SUB_B` sub-halves under the same `(tx_id, part_idx)`, §5.3).
- **SNR-margin burst ranking**:
  - Collect flood candidate copies across `BURST_SETTLE_MS = 2_500 ms` and rank by Semtech SX126x SNR margin (`margin_db = snr_db − demod_limit_db(sf)`, where `demod_limit_db(7) = −7.5 dB`):
    - Reject copies with `margin_db < 2.0 dB` unless no other copy arrived.
    - Among copies with `margin_db >= 6.0 dB`, **fewer hops wins** (`candidate.hops < current.hops`), with higher `margin_db` as tie-breaker.
    - Among copies with `2.0 <= margin_db < 6.0 dB`, **higher `margin_db` wins** before hop count.
  - Reject cached `GET_ADVERT_PATH` entries older than `ROUTE_FRESH_MS = 120 s`.

---

## 5. Layer 2: 1-Round-Trip Bootstrap Fragmentation, Idempotent Handshake, and Message-Oriented Transaction Layer

### 5.1 1-round-trip combined discovery + bootstrap (`KIND_MSG1_FRAG` & `KIND_MSG2`)

- **No precursor `LDRQ` before `msg1` during `connect()` (`RC-1` & Self-Critique 4)**:
  - When connecting without a fresh route, the phone does **not** send `SELF_ADVERT`, `LDRQ`, or `LDRH` ahead of `msg1`. It sends **only `msg1`**, which both discovers the route and initiates the `Noise_IKpsk2_25519_ChaChaPoly_SHA256` handshake in a single round trip!
- **Bootstrap `msg1` fragmentation (`KIND_MSG1_FRAG = 0x14`) and per-transmit nonce (`RC-5`, `RC-6`)**:
  - Complete `msg1` is 121 bytes (`KIND_MSG1 = 0x11 | filter_tag: 16 | noise_msg1: 104`).
  - When sent Direct over a path with `P_out <= 49` bytes, `msg1` is sent as `[KIND_MSG1 (0x11)][filter_tag: 16][noise_msg1: 104][tx_nonce: u16]` (`123 B`). The trailing 2-byte random `tx_nonce` changes on every retry so `SimpleMeshTables::wasSeen` (`Mesh.cpp:97`, `Packet.cpp:42`) never drops a direct retry at a repeater, while the 121-byte cryptographic prefix `[0..121]` remains identical across all retries of the same connection attempt!
  - When sent by Flood (`LDMR`) or over a direct path with `P_out > 49` bytes, `msg1` is split into **2 fragments** of **67 and 66 bytes**:
    ```text
    [KIND_MSG1_FRAG (0x14): 1 B][init_id: u16 LE][part_byte: (part_idx << 4) | 2][msg1_slice: 61 B / 60 B][tx_nonce: u16]
    ```
    - `init_id = u16::from_le_bytes(msg1[1..3])`.
    - **Each fragment carries a `tx_nonce` too, which the original draft of this paragraph did not.** A fragment is chosen when the initiation is flooded *or* when `P_out > 49`, and the direct case has no `LDMR` wrapper to vary, so a byte-identical retry would be dropped by `wasSeen` at the first repeater and the retry would be permanently dead on a 50-hop direct path. The nonce is stripped on reassembly, so cache identity stays the 121-byte content.
    - Inside an `LDMR` scoped flood at the maximum 64-byte path (`12 + 4 + 64 + 67 = 147 B <= 173 B`), **both fragments are guaranteed to pass `logRxRaw` (`<= 173 B`) at every legal hop count and hash width (`1B`, `2B`, `3B`)**.
    - Part 1 is paced after Part 0 by `2.5 × airtime` (~900 ms at the public preset) so Part 1 does not collide with first-hop repeaters forwarding Part 0.
- **Stable ephemeral key across retries of the same connect attempt (`RC-5`)**:
  - The phone keeps the **same `Handshake` instance (`msg1` bytes and ephemeral key $e$)** across all 3 retry rungs of a connection attempt (`handshakeRetryFor(sessionHopsFor(routeHops, meshReach))`, minimum `12 s` first rung, doubling across 3 attempts + `120 s` answer window), changing only the outer `LDMR` nonce / trailing `tx_nonce` on each re-send!
  - Thus, even if Attempt 1's `msg2` arrives after Attempt 2's `msg1` was transmitted, **both Attempt 1 and Attempt 2 share the exact same ephemeral key $e$ and produce the exact same `msg2` from the host's idempotent responder cache**, so `h.finish(msg2)` **always succeeds**!
- **`msg2` (`KIND_MSG2 = 0x12`, 52 bytes with `tx_nonce: u16`)**:
  - Wire layout: `[KIND_MSG2 (0x12): 1 B][noise_msg2: 49 B][tx_nonce: u16]` (`52 B`). The session index is **sealed inside** the answer (`hs.write_message(&[index])`, `crates/session/src/handshake.rs:344`), not carried as a clear byte: `MSG2_LEN` is 50, so the Noise part is 49. The earlier draft of this paragraph put the index in the clear and made the Noise part 48; the total was right and the breakdown was not.
  - Fits in a single packet on all carriers (`12 + 4 + 64 + 52 = 132 B <= 173 B`).
  - When answering a flooded `msg1`, the host adopts `msg1`'s reversed travel path as its outbound candidate, sends `msg2` Direct along that path, and schedules 1 flooded `LDMR` copy of `msg2` `1_500 ms` later in the `not_before` queue (cancelled if a `KIND_UP` record arrives first) — guaranteeing fast direct completion on symmetric links and 1-round-trip flood completion + route teaching on asymmetric links.

### 5.2 Host idempotent responder cache and `KIND_NEED_HANDSHAKE` (`0x15`)

- **Idempotent responder cache (`SessionHost::on_handshake`)**:
  - Keyed by the 121-byte `msg1[..121]` within `TIMESTAMP_WINDOW_MS` (`±5 min`), bounded to `64` entries.
  - If `msg1[..121]` matches the active session's initiation, re-queue the cached `msg2` (with a fresh `tx_nonce: u16`) and **return immediately without rebuilding `Session`, without advancing `next_index`, without clearing outbound state, and without calling `fsync`**.
  - If `msg1[..121]` matches a superseded initiation, ignore it without touching the live session.
- **PSK-authenticated `KIND_NEED_HANDSHAKE (0x15)` when host has no session**:
  - If the host receives a `KIND_UP (0x13)` transport record while `self.session == None` (e.g., after a daemon restart while the phone still holds its session), instead of staying silent for 3 minutes, the host emits a 25-byte `KIND_NEED_HANDSHAKE` control record (rate-limited to once per `5_000 ms`):
    ```text
    [KIND_NEED_HANDSHAKE (0x15): 1 B][wall_ms: u64 LE][hmac_psk("NEED_HS" || host_pub || wall_ms): 16 B]
    ```
  - The phone verifies the 16-byte PSK HMAC (so a stranger can never spoof a session reset) and immediately initiates a handshake without waiting out its transaction retry ladder.

### 5.3 Message-Oriented Reliable Transaction Layer (`KIND_UP = 0x13` & `KIND_DOWN = 0x16`)

Directly inside `Noise_IKpsk2`'s `StatelessTransportState` (`[kind: 1 (`0x13` up / `0x16` down)][counter: u32 LE][len: u8][ciphertext + 16 B tag]`, **24 B** session overhead: 6 clear + 2 inner + 16 tag, `crates/session/src/record.rs:26-28`), replace the byte-stream `Arq` + `TermDecoder` with a **Message-Oriented Transaction Header (10 bytes, including the `tx_nonce`)**:

```text
[tx_id: u16 LE][part_byte: u8][part_count: u8][ack_tx_id: u16 LE][ack_mask: u16 LE][tx_nonce: u16 LE][payload: 0..=137 B]
```

**Three corrections to this header, all found by implementing it against `VirtualMesh` (2026-11-11):**

- **`tx_nonce: u16` is new and load-bearing.** MeshCore suppresses a duplicate by `SHA256(payload_type || payload)` at every receiver and repeater, so a retransmitted part whose bytes are unchanged is dropped on arrival and the sender retries for ever. Measured in the harness: 292 `DroppedDedup` entries in one eight-hop run, and not one message completed. The `msg1`/`msg2` boot messages already carry this exact field for this exact reason; the transaction header needed it too.
- **The payload is 137 bytes, not 141.** The record layer's overhead is 24 bytes (6 clear + 2 inner + 16 tag), not the 22 this page assumed, and the transaction header is ten bytes rather than eight.
- **`part_count` is `1..=16`, which is 1888 bytes at the public preset, not unlimited.** `ack_mask` is one `u16`, so a message with more parts than 16 cannot be acknowledged. A larger message is refused (`BeginError::TooLarge`) rather than truncated.
- **Parts are paced `2.5 × airtime` apart, not sent back to back.** A repeater is half duplex, so while it forwards the part it just heard it cannot hear the next; sixteen parts handed over together lose all but the first at the first repeater. The same `2.5 × airtime` this page already uses for the `msg1` fragments and the burst tail applies to every part.

- **Fields**:
  - `tx_id: u16 LE`: monotonic transaction/message ID (`1..=u16::MAX`, `0` = pure ACK with empty payload).
  - `part_byte: u8`: bits `0..5` = `part_idx` (`0..=15`); bits `6..7` = `sub_half` (`00` = full part, `01` = low half `SUB_A`, `10` = high half `SUB_B` when a direct part is retried over a flood carrier that cannot hold it). Parts are cut to **118 bytes**, which is `min(137, 2 × 59)`: a 171-byte direct record carries at most 137 payload bytes, while a 93-byte flood payload carries 59, so 118 is the largest part that can always be halved onto the narrow carrier instead of being re-cut.
  - `part_count: u8`: total parts in `tx_id` (`1..=16`; `0` when `tx_id == 0` pure ACK).
  - `ack_tx_id: u16 LE` & `ack_mask: u16 LE`: selective-ACK bitmask confirming which parts (`0..15`) of the peer's `ack_tx_id` have been received (`ack_mask == (1 << peer_part_count) - 1` confirms the entire message `ack_tx_id`).
- **Key properties (closing `RC-7` and Self-Critiques 1 & 2)**:
  1. **Zero mid-burst ACK collisions**: When receiving a multi-part message (`part_count > 1`), the receiver knows `remaining = part_count - 1 - part_idx` parts are still in flight in the current burst and **suppresses its standalone `Ack` until `part_idx == part_count - 1` arrives (or the burst-tail timer `2.5 × airtime` expires)**.
  2. **Selective repeat of missing parts only**: If a 4-part full-screen `Snapshot` loses part 1 (`ack_mask = 0b1101`), the sender retransmits **only part 1**, never parts 0, 2, or 3.
  3. **Full direct capacity + lossless flood sub-splitting**: a part carries up to `137` payload bytes on a 171-byte direct record (`171 - 24 - 10`), and parts are cut to `118` so the split is always possible. If an unacknowledged direct part must retry over a 93-byte flood on `Attempt 2`, it is sealed as two flood packets with `sub_half = 01` (`SUB_A`) and `sub_half = 10` (`SUB_B`) carrying the **same `(tx_id, part_idx)`**, each with 59 payload bytes.
  4. **Zero head-of-line blocking**: A newer `Snapshot` (`tx_id + 1`) cleanly supersedes any unacknowledged older `Snapshot` (`tx_id`).
  5. **Persistent Noise session**: Exhausting retries on `tx_id` marks only `tx_id` as `TimedOut`; the `StatelessTransportState` session is **never** destroyed on a transaction timeout.

---

## 6. Layer 3: Single-Burst RPC Protocol (`mesh_term.rs` & `MeshRpcService`)

Bump `mesh_term::VERSION` to **15**. Every transaction body is a single self-contained `TermMessage` (no stream concatenation across unrelated messages):

### 6.1 Uplink request messages (`Phone → Host`, carried in `KIND_UP`)

- **`Send` (`T_SEND = 0x0E`) — coalesced batch input + keys + paste**:
  ```text
  [0x0E][op_id: u32 LE][chunk_idx: u8][chunk_count: u8][have_snapshot_id: u32 LE][cols: u8][rows: u8][flags: u8][shell_len: u8 | shell_id][items_bytes...]
  ```
  - `op_id`: monotonic per-app-run operation ID (`1..=u32::MAX`).
  - `chunk_idx` / `chunk_count`: **the message is one chunk of a larger command**, and this is the correction §12.2 (AR-3) records. A command whose item stream does not fit one message is sent as `chunk_count` messages with one `op_id`; the host holds them and **applies the joined bytes once, when `chunk_idx == chunk_count - 1`**, so a partial paste is never written to a pty. `chunk_count = 1` means the whole command, which is the common case and costs two bytes. Chunking is deliberately *not* a second fragmentation inside the transaction layer: transactions already retransmit parts, and two schemes for one job is two things to get wrong.
  - `have_snapshot_id`: the `snapshot_id` of the last complete `Snapshot` painted by the phone for `shell_id` (`0` if none).
  - `cols`, `rows`: the phone's current terminal grid (`1..=200`, `1..=120`), so the host ensures the PTY matches `(cols, rows)` before executing the command.
  - `flags`: bit 0 = `deflated` (when `items_bytes` is raw-deflate compressed against `FRAME_DICTIONARY`, used whenever raw `items` > 96 B and deflate is smaller); bit 1 = `interrupt` (set when the draft contains `C-c` / `\x03`).
  - `items`: sequence of self-delimiting items in exact user composition order:
    - `0x01 (Text)`: `[0x01][len: u16 LE][utf8_bytes]`
    - `0x02 (Paste)`: `[0x02][len: u16 LE][utf8_bytes]`
    - `0x03 (Keys)`: `[0x03][count: u8][name_len: u8 | ascii_name]...`
  - **Result (`RC-8`, `RC-9`)**: Typing `"ls -la"` + `Enter` and tapping `Send` encodes as **one ~30-byte `Send` message in a single `part_count = 1` LoRa packet** (instead of 3 separate records!).
- **`Fetch` (`T_FETCH = 0x0F`) — explicit screen refresh or resize**:
  ```text
  [0x0F][have_snapshot_id: u32 LE][cols: u8][rows: u8][shell_len: u8 | shell_id]
  ```
  - Fits the PTY to `(cols, rows)` if changed, then captures and returns one `Snapshot` diffed against `have_snapshot_id`.
- **`Attach` (`0x01`), `Takeover` (`0x0B`), `Create` (`0x08`), `List` (`0x05`), `WantHistory` (`0x0B`/`0x0A`), `Close` (`0x09`), `Detach` (`0x04`)**:
  - `Attach`, `Takeover`, and `Create` always include `[version: u8][cols: u8][rows: u8][have_snapshot_id: u32 LE]`, fitting the PTY **before** capturing the initial `Snapshot` (`D-8`).

### 6.2 At-most-once host execution and paste safety (`MeshRpcService`)

- **Authenticated `(device_id, op_id)` dedup ring**:
  - Retains the last 64 `AppliedOutcome` entries per paired device.
  - If a duplicate `Send(op_id)` arrives (e.g. after an ACK loss), the host returns the cached `Applied` status + current `Snapshot` **without writing to the PTY a second time**.
- **Ordered PTY handoff** — `mesh_apply::apply_send` (`crates/daemon/src/mesh_apply.rs`) turns the items into an ordered plan of steps, so the rule lives in one tested place instead of in the middle of the service:
  - `Text(bytes)`: bytes for the pty. A text run carrying a newline is treated as a paste, which is recoverable where executing it is not; the phone sends the operator's `Enter` as a key, so the normal case never takes that branch.
  - `Paste(bytes)`: encoded by the same `paste_bytes` rule `Input` already uses — wrapped in `\x1b[200~ … \x1b[201~` with `ESC` stripped where the pane negotiated bracketed paste, `\n` folded to `\r` where it did not.
  - `Keys(names)`: handed to `host.keys(&session, &names)`, because only the keeper can read the pane's live cursor-key and Kitty keyboard mode.
  - Adjacent steps of the same kind are merged, so a draft of several runs costs one keeper call; a key splits the write, because the text after it must not land before it. An empty item refuses the whole command, so nothing partial is ever written.

### 6.3 Single-Burst `3.5 s` Settle-or-Midstate Rule and Atomic `Snapshot` (`T_SNAPSHOT = 0x8E`)

- **Why the host always sends ONE downlink burst per `Send` (Self-Critique 3)**:
  - Sending an early packet at `1.2 s` and a second packet at `2.0 s` launches two downlink waves into the repeater chain that collide with each other.
  - Instead, after writing `Send(op_id)` to the PTY at `t_apply`, the host suppresses any standalone ACK and observes the pane every `100 ms` for up to **`INLINE_SETTLE_MAX_MS = 3_500 ms`**:
    1. **Settled after change (`settled = 1`)**: at least one visible screen change occurred after `t_apply` AND the screen has remained unchanged for **`SETTLE_QUIET_MS = 500 ms`**.
    2. **Settled with no change (`settled = 1`)**: no visible screen change occurred for **`SETTLE_NO_CHANGE_MS = 700 ms`** since `t_apply` (`cd`, `export`, silent prompt, password entry).
    3. **Mid-state at budget cap (`settled = 0`)**: the pane is still actively changing when **`INLINE_SETTLE_MAX_MS = 3_500 ms`** expires (`cargo build`, `tail -f`, `top`).
  - At that moment (`0.5 s .. 3.5 s` after `t_apply`), the host captures the screen and emits **EXACTLY ONE downlink transaction** (`T_SNAPSHOT = 0x8E`, carrying `ack_tx_id = send_tx_id` piggybacked in the transaction header!) and **never transmits a second unsolicited wave**:
- **Atomic `Snapshot` (`T_SNAPSHOT = 0x8E`) wire layout**:
  ```text
  [0x8E][flags: u8][chunk_idx: u8][chunk_count: u8][snapshot_id: u32 LE][base_id: u32 LE]
  [applied_op_id: u32 LE][applied_status: u8][cols: u8][rows: u8][cursor_x: u8][cursor_y: u8]
  [first_line: u32 LE][shell_len: u8 | shell_id][body_bytes...]
  ```
  - `chunk_idx` / `chunk_count`: the same envelope as `Send`, for the same reason — a first full paint of a dense screen deflates past what one transaction carries (`16 × 118 = 1,888` bytes, §5.3). The phone reassembles by `snapshot_id` and paints only the joined body, so a chunked snapshot is still atomic: it is one screen, delivered in pieces, never a half-drawn one.
  - `body_bytes` is raw-deflate compressed against `FRAME_DICTIONARY` when bit 4 is set.
  - `flags`:
    - `bit 0`: `full` (`1` = full screen, `0` = row diff against `base_id`)
    - `bit 1`: `alternate` (`1` = alternate screen active)
    - `bit 2`: `dead` (`1` = shell process exited)
    - `bit 3`: `settled` (`1` = pane settled, `0` = mid-state / still changing at `3.5 s` or on `Fetch`)
    - `bit 4`: `deflated` (`1` = `body_bytes` is raw-deflate compressed with `FRAME_DICTIONARY`)
    - `bit 5`: `has_applied` (`1` = `applied_op_id` and `applied_status` are present)
  - **Why this eliminates `S2`, `S9`, and `S10` by construction**:
    1. `Applied` outcome (`applied_op_id`, `applied_status`), screen geometry (`cols`, `rows`), cursor (`cursor_x`, `cursor_y`), `settled` bit, and all row patches travel in **one atomic `Snapshot` transaction**.
    2. It is impossible for `Profile` to clear the mirror without its `Rows`, impossible for `Commit{IDLE}` to be lost separately from the screen, and impossible for two downlink bursts from the same command to collide in the repeaters!

---

## 7. Layer 4: Unified Mobile App State, Viewport Fidelity, Lifecycle, and Ownership

### 7.1 Viewport and keyboard geometry fidelity (closing `S2` and `S10`)

- **Keyboard-up grid gating (`TerminalSurface.android.kt:318` & `MeshTerminalScreen.kt:1124`)**:
  - When `portrait && keyboardWanted && !imeUp`, do **not** save `(cols, rows)` to `lastGrid` or `config.meshLastGrid` and do **not** send `Fetch`/resize until `imeUp == true` (or `IME_WAIT_MS = 700 ms` elapses).
  - When leaving a shell to the launcher (`showList = true`), ignore the layout pass caused by the keyboard closing so `lastGrid` and `meshLastGrid` retain the keyboard-UP grid (`46×19`).
- **Cursor-aware grid placement (`GridPlacement.kt:129`)**:
  - When `gridH > viewportHeight`, compute `dy` so `cursorY` remains inside the visible viewport (`0 <= dy + cursorY * cellHeight <= viewportHeight - cellHeight`), clamped to `[viewportHeight - gridH, 0f]` — never clipping rows `0..12` off the top of the screen when the prompt and cursor are near the top!
- **Delete automatic `listShells()` on shell open (`MeshTerminalScreen.kt:808`)**:
  - Eliminates the redundant competing `ListRequest` round trip when opening a shell.

### 7.2 Per-shell state, `resumeShell`, and the 5-shell cap (`A5`)

- **Enforce `MAX_OPEN_PAGES = 5`** in shell selection, matching Tailnet (`D-7`).
- **Assign `resumeShell = id`** whenever a shell is opened, switched to, or created; clear `resumeShell = ""` only when the user explicitly navigates to the launcher or closes the shell (`RC-12`).
- **Per-shell state map (`ShellPageState` keyed by `shellId`)**:
  - Each open shell retains its own `ScreenMirror`, `snapshotId`, `MeshHistory` (and scroll `offset`), and `MeshInputBatcher` draft across shell switches and connection rebuilds (`S7`).

### 7.3 Truthful UI states and batch bar

| State | Condition | Bar / status text |
|---|---|---|
| **Drafting** | Local draft non-empty, nothing in flight | `<N chars / keys> · tap send` (`Send` active, `Refresh` active) |
| **Queued (connecting)** | `Send` pressed while bootstrap handshake is in flight | `queued · connecting (try k/3 · Ns)` |
| **Sending** | `Send` in flight, awaiting host `Snapshot` response | `sending · <elapsed>s` |
| **Answered (settled)** | Correlated `Snapshot(settled = true)` painted | `up to date` (clears after 3 s) |
| **Mid-state (running)** | Correlated `Snapshot(settled = false)` painted (command still churning at `3.5 s` or via `Refresh`) | `applied · output still changing · tap refresh` |
| **No answer** | Transaction retry budget expired without `Snapshot` | `no answer · input outcome unknown` (draft not auto-replayed; user chooses `Send again` or `Refresh`; **Noise session stays UP**) |
| **Refused** | `Snapshot(applied_status = Refused*)` or `Closed` received | `not applied: <reason>` (draft restored so user never loses typed text) |
| **Stale** | Showing cached screen while refreshing or switching shells | `stale · refreshing…` |

---

## 8. End-to-End System Integration, Crate & JNI Boundaries, Keeper Settle Engine, and Code Deletion Budget

To make the entire application simple, robust, and free of cross-layer drift, the five architectural transformations (`A1`–`A5`) integrate across every crate, JNI boundary, daemon task, keeper stream, and Compose screen as follows:

### 8.1 Pure-Rust Crate Graph & Zero-Runtime JNI Boundary (`crates/*` → `libldmux_session.so`)

- **Strict separation of pure protocol/state crates from host OS I/O (`IC-4`)**:
  - `crates/mesh` (`ldmux-mesh`): pure Rust (`std` + `hmac` + `sha2`), zero `tokio`, zero `nix`. Owns:
    - Companion frame parser/encoder (`framing.rs`) and command/push codecs (`codec.rs`: `APP_START`, `DEVICE_QUERY`, `SELF_INFO`, `SEND_RAW_PACKET`, `LOG_RX_DATA`, `GET_ADVERT_PATH`, `LDRQ`, compact `LDRR`, `LDMR`, `ScopeInfo` HMAC-SHA256).
    - Strict 1-command-at-a-time `CompanionFifo` state machine (shared by Host USB serial and Phone BLE).
    - `RouteTable`, `carrier_for(route, attempt)`, Semtech SX126x `airtime_ms`, and SNR-margin burst ranking.
  - `crates/companion` (`ldmux-companion`): thin Linux host USB serial driver (`termios`, `flock`, non-blocking `not_before: Instant` scheduler) built on `ldmux_mesh::CompanionFifo`. The **answer table is an input, not a derivation**: the FIFO is shared, but which response opcodes settle which command is host- or phone-specific, so each side supplies its own (the host's is `crates/companion/src/window.rs`) and the client engine in S4 supplies the phone's.
  - `crates/protocol` (`ldmux-protocol`):
    - Default `host-ipc` feature gates `socket.rs` and `paths.rs` (`tokio` + `nix` Unix sockets).
    - With `default-features = false` (used by `ldmux-session-ffi`), depends only on `serde`, `serde_json`, `uuid`, and `flate2` (`zlib-rs`, pure Rust, no C) — exposing `mesh_term.rs` (Protocol v15 `Send`, `Fetch`, atomic `Snapshot`, `FRAME_DICTIONARY`, `ScreenMirror`), `pairing.rs`, and `utf8.rs`.
  - `crates/session` (`ldmux-session`): pure Rust (`snow`, `hmac`, `sha2`, `zeroize`), zero I/O. Owns `Handshake` (`KIND_MSG1`, `KIND_MSG1_FRAG`, `KIND_MSG2`, `KIND_NEED_HANDSHAKE`), `StatelessTransportState`, the Message-Oriented Transaction Layer (`tx_id`, `part_idx`, `part_count`, `SUB_A`/`SUB_B`, burst-tail `ack_mask`), and the unified `RadioEndpoint`.
  - `crates/session-ffi` (`ldmux-session-ffi` → `libldmux_session.so`):
    - Links `ldmux-mesh`, `ldmux-session`, and `ldmux-protocol` (`default-features = false`).
    - Pulls **zero `tokio`, zero `nix`, zero background threads, and zero C libraries** into Android (`arm64-v8a`, `x86_64` with `-Wl,-z,max-page-size=16384`) or host JVM unit tests.
    - Exposes a **pull-only, single-handle, `catch_unwind`-guarded C ABI**:
      - Kotlin passes `now_ms: u64` + raw BLE notification bytes (`ldmux_client_on_ble_rx`) and user actions (`ldmux_client_send`, `ldmux_client_fetch`, `ldmux_client_attach`, `ldmux_client_list`, `ldmux_client_history`, `ldmux_client_close`, `ldmux_client_detach`).
      - Kotlin drains `ldmux_client_poll_ble_tx` (at most 1 BLE command in flight at any instant) and `ldmux_client_poll_event` (rendered `ScreenMirror` frames, shell lists, history pages, and link telemetry), and schedules its single coroutine wake timer at `ldmux_client_next_deadline(handle)`.
      - **Zero JNI upcalls from C into the JVM** (`AttachCurrentThread` is never used).
  - **Freshness & symbol gate (`packages/mobile/test/native-freshness.test.ts`, `IC-8`)**:
    - Fix `Java_dev_ldmux_` → `Java_io_ldmux_mobile_term_NativeTerminal_${name}` for `libldmux_terminal.so` and add a matching symbol + source-hash check for `libldmux_session.so` (`Java_io_ldmux_mobile_mesh_NativeMeshSession_${name}`).

### 8.2 Host Single-Task `MeshRpcService` & `PaneStream` Eventfd Settle Engine (`IC-2`, `IC-3`)

- **Eliminate the 3-loop split (`mesh_serial.rs` ↔ `mesh.rs` `Exchange` ↔ `mesh_bridge.rs` 50 ms ticker)**:
  - Delete `Exchange`, `INBOUND_MAX`, and the 50 ms `bridge_loop` task.
  - One unified async `MeshTask` owns `RadioEndpoint` and `MeshRpcService`:
    - Inbound OTA frames from `mesh_serial` feed `RadioEndpoint::receive_ota(now_ms, snr, rssi, packet)`.
    - Whenever `RadioEndpoint` completes a request transaction `(tx_id, shell, req_msg)`, `MeshRpcService` handles it **before** `RadioEndpoint::poll_tx` runs so the response transaction always piggybacks `ack_tx_id = tx_id` in the same turn.
- **Eventfd-driven settle window via `PaneStream` (`crates/daemon/src/stream.rs`, zero keeper polling, zero global `daemon.lock()` contention)**:
  - Never call blocking keeper IPC while holding `shared.daemon.lock().await` (`IC-2`). Instead, resolve the session's `Arc<PaneStream>` under a brief in-memory lock (matching `remote.rs:1888`) and run keeper operations via `on_stream` (`spawn_blocking`).
  - When `Send(op_id, have_snapshot_id, cols, rows, items)` arrives:
    1. Check `(device_id, op_id)` in the in-memory dedup ring; if already applied, capture 1 snapshot and return immediately.
    2. Fit `(cols, rows)` if changed, subscribe a temporary `tokio::sync::Notify` (`output_notify`) to `PaneStream::subscribe_owned`, and write `items` (`Text`, `Paste` with live `bracketed_paste`, `Keys` with live cursor/Kitty mode) to the pane.
    3. Await `output_notify`:
       - If no PTY output arrives within `SETTLE_NO_CHANGE_MS = 700 ms`, the command was silent (`cd`, `export`, empty `Enter`) — unsubscribe, capture **one** keeper snapshot, and emit `Snapshot(settled = 1, applied_op_id = op_id, ack_tx_id = tx_id)`.
       - Once the first PTY output arrives, wait until either `output_notify` is quiet for `SETTLE_QUIET_MS = 500 ms` (`settled = 1`) or `INLINE_SETTLE_MAX_MS = 3_500 ms` from `t_apply` elapses (`settled = 0`), then unsubscribe, capture **one** keeper snapshot, and emit `Snapshot(settled, applied_op_id = op_id, ack_tx_id = tx_id)`.
    4. **Result**: The keeper renders **exactly 1 snapshot per user action** (down from 20 snapshots/sec in `mesh_bridge.rs`), with sub-millisecond `eventfd` responsiveness and zero global lock contention.

### 8.3 Unified Cryptographic Device Identity & Zero-Airtime Ownership Handoff (`IC-1`)

- **One `device.id` across Tailnet and Radio (`hex(phone_static_pub[..8])`, `kind = "phone"`)**:
  - When `MeshEnrollment` generates or loads the phone's 32-byte X25519 static public key `local_pub` in `MeshSessionKeyStore`, `LdmuxConfig.deviceId` is set to `hex(local_pub[..8])` (16 lowercase hex chars).
  - On **Tailnet**, `hello.device` sends `{ id: hex(local_pub[..8]), name: config.deviceLabel, kind: "phone" }`.
  - On **Radio**, `SessionHost` authenticates the phone's 32-byte `remote_static_pub` in `Noise_IKpsk2` `msg1` and derives the **exact same** `DeviceInfo { id: hex(remote_static_pub[..8]), name: device_label, kind: "phone" }`.
  - **Result**: Switching the same phone from Tailnet to Radio (or from Radio to Tailnet) is recognized by `Ownership::claim` (`ownership.rs:150`) as the **same device (`e.device.id == device.id`)**, re-acquiring the phone's shells idempotently with zero `HeldBy` self-lockouts (`S6`) and zero QR re-pairing!
- **Bounded Radio Engagement & Silent Desktop Takeover (`ownership.rs`, `daemon.rs`, `MeshRpcService`)**:
  - Delete `OWNER_CHECK` (`mesh_bridge.rs:992`) and `mesh_watched` permanent exemption (`daemon.rs:2996`).
  - A claim held on the radio route (no active WebSocket in `shared.conns`) refreshes `active_at = now` on every authenticated `Attach`, `Takeover`, `Send`, or `Fetch`, and remains `engaged(now)` for `MESH_ENGAGED_MS = 300_000 ms (5 minutes)`.
  - After 5 minutes of radio inactivity, `engaged(now)` becomes `false`, allowing a desktop window (`ldmux attach`) to attach cleanly without a manual takeover prompt.
  - Within those 5 minutes, if the operator presses `r` (`takeover: true`) in a desktop window, the desktop acquires the claim in memory and restores the desktop PTY size **without transmitting any unsolicited radio packet**. If the user later taps `Send` or `Refresh` on the phone for that displaced shell, `MeshRpcService` returns `Closed { reason: HELD }` (or `Snapshot(applied_status = RefusedHeld)` with draft preserved), displaying `"in use on desktop · take it back"` on that shell page.

### 8.4 Unified Mobile UI, Provisional Draft Renderer, and Input Pipeline (`IC-5`, `IC-6`, `IC-7`)

- **One shared Compose shell pager and launcher (`AppContent.kt` + `TerminalArea.kt`)**:
  - Both Tailnet and Radio use the same `pageOrder` (`MAX_OPEN_PAGES = 5`), `shownSessionId` / `resumeShell` tracker, `HorizontalPager` keyed by `session.id`, `PortraitBar`, `SidePanel`, `ScrollPad`, `VoiceBanner`, and `HeldElsewhere`.
  - On the Radio route, each open page is backed by a per-shell `MeshPageController(sessionId)` (owning its own `TerminalEngine`, `ScreenMirror`, `snapshotId`, `MeshHistory`, and `MeshInputBatcher` draft) and renders the single `BatchBar` (`status` + **`Send`** + **`Refresh`**) directly above `PortraitBar` (or in `SidePanel` in landscape).
- **Non-scrolling, visible provisional draft echo (`MeshScreenBytes.kt` & `MeshTerminalSource.kt`, `IC-5`)**:
  - Remove the code-point truncation `n < cols` from `cleanRow` (keep control-char sanitization only; `libghostty-vt` handles `wcwidth` and margin clamping natively).
  - Format the unsent draft (`Text`, `Paste`, `Keys`) into visible glyph tokens (`⏎` for `Enter`, `⇥` for `Tab`, `^C` for `C-c`, `⎋` for `Escape`, `←↑↓→` for arrows) instead of raw `\r\n` control characters, so pressing `Enter` on the bottom row (`cursorY == rows - 1`) **never scrolls `TerminalEngine` up by a line**!
  - When the provisional draft text exceeds `cols - cursorX` columns on the bottom row (`cursorY == rows - 1`), horizontally window the draft (`…` + rightmost `cols - cursorX - 1` cells) so the latest typed characters and cursor stay visible without wrapping/scrolling the engine.
- **Unified multiline paste and dictation across Tailnet and Radio (`InputDelta.kt`, `IC-6`, `IC-7`)**:
  - In `InputDelta.kt:42`, when an `onValueChange` insertion is `> 1` char and contains `\n` or `\r`, emit `InputOp.Paste(inserted)` instead of splitting on `\n` into `InputOp.Keys("Enter")`.
  - On Tailnet, `InputOp.Paste` calls `handle.paste(text)` (bracketed paste when mode 2004 is active). On Radio, `InputOp.Paste` appends `DraftItem.Paste(text)` to the shell's draft.
  - Voice dictation (`Dictation`) appends transcribed speech to the shell's local draft on Radio so the user can review or edit before tapping `Send`.

### 8.5 Net Code Simplification & Deletion Budget

| Component | Current lines | Target lines | Net change & what is removed |
|---|---|---|---|
| `crates/daemon/src/mesh_bridge.rs` → `mesh_rpc.rs` | 6,305 | ~800 | **−5,500 lines**: deletes 50 ms `Differ` loop, `Hold`, `owe_frame`, `resync_wanted`, `OWNER_CHECK`, `Profile`/`Rows`/`Commit` split streaming. |
| `packages/mobile/vendor/libmeshcore*` (`Java`) | 2,295 | 0 | **−2,295 lines**: replaced by shared Rust `CompanionFifo` + ~200-line `AndroidBlePipe.kt`. |
| `AndroidRadioLink.kt` + `RadioRoute.kt` + `RouteHint.kt` | 2,756 | ~250 | **−2,500 lines**: replaced by shared Rust `RadioEndpoint` over JNI + `AndroidBlePipe.kt`. |
| `MeshTermDriver.kt` + `MeshTerm.kt` + `PublicChannelHold.kt` | 3,870 | ~350 | **−3,520 lines**: replaced by shared Rust `mesh_term` client + transaction engine over JNI. |
| `MeshTerminalScreen.kt` (forked UI) | 1,698 | ~350 | **−1,350 lines**: merged into shared `AppContent.kt` / `TerminalArea.kt` + `BatchBar.kt`. |
| **Total radio stack** | **~16,924** | **~1,750** | **−15,165 lines deleted (~90% reduction in radio glue/duplicated code)** |

---

## 9. Offline Verification Matrix (`B1`–`B8`)

Every stage is gated by deterministic offline tests (`VirtualMesh` in Rust + JVM/native tests in Kotlin) before any hardware run:

| Gate | Contract proven |
|---|---|
| **B1: Companion FIFO & `0x88` byte bounds** | Strict 1-command-at-a-time FIFO serialization (no command overlaps `0x2A`/`0x14`/`0x41`; `ERR_CODE_NOT_FOUND` on `0x2A` never settles `0x41`). All packet builders (`RAW_CUSTOM`, `LDMR`, `LDRQ`, compact `LDRR`, `KIND_MSG1_FRAG`) at 0..63 hops across 1B/2B/3B path hashes (`<= 64 B` path, scoped and unscoped) satisfy `<= 174 B` TX and `<= 173 B` RX (`0x88`). |
| **B2: Self-echo rejection & post-auth routing** | Repeater 1 forwarding our own 1-hop (`hops=0`) or $N$-hop (`hops=N-1`) direct `RAW_CUSTOM` packet is rejected by direction byte (`KIND_UP` vs `KIND_DOWN`) and `view.path.hops == 0`, never updating `last_peer_rx`, `proven`, or `srtt`. Genuine 0-hop direct arrival with `hostPath == null` updates route state after AEAD verification. |
| **B3: 1-round-trip bootstrap & idempotency** | No `SELF_ADVERT`/`LDRQ`/`LDRH` sent before `msg1`; 2-part `KIND_MSG1_FRAG` reassembles across loss/reordering; phone keeps the same ephemeral key across all 3 retry rungs with fresh `tx_nonce` (defeating `SimpleMeshTables::wasSeen`); duplicate `msg1` hits host cache without resetting session; `KIND_NEED_HANDSHAKE` (`0x15`) triggers immediate re-handshake after daemon restart. |
| **B4: Message-oriented transactions & burst ACKs** | Multi-part `Snapshot` suppresses receiver `Ack` until `part_idx == part_count - 1` (zero mid-burst ACK collisions); selective `ack_mask` retransmits only missing parts; `> 63 B` direct part retries over flood as `SUB_A`/`SUB_B` under the same `(tx_id, part_idx)`; newer `Snapshot` supersedes stale in-flight `Snapshot`; transaction timeout keeps `StatelessTransportState` session alive. |
| **B5: Single-burst `3.5 s` settle & at-most-once `Send`** | Typing `"ls"` + `Enter` and tapping `Send` emits **1** `Send` packet; multiline IME `onValueChange` produces a `Paste` item wrapped in `\x1b[200~…\x1b[201~` when `bracketed_paste` is on; duplicate `(device_id, op_id)` returns cached outcome without re-writing PTY; fast command settles in `500 ms` via `PaneStream` eventfd and returns 1 burst (`settled=1`); churning command (`tail -f`) returns 1 mid-state burst (`settled=0`) at `3.5 s` and zero unsolicited follow-up packets. |
| **B6: Atomic `Snapshot`, provisional echo & viewport fidelity** | `Snapshot` applies geometry + cursor + rows atomically; pressing `Enter` or typing > 46 chars in draft at bottom row (`cursorY == rows - 1`) renders `⏎` and windows horizontally without scrolling `TerminalEngine`; keyboard-down grid is never saved/sent while `keyboardWanted && !imeUp`; `placeGrid` keeps `cursorY` visible when `gridH > viewportHeight`; no automatic `listShells()` on shell open. |
| **B7: Per-shell state, 5-shell cap, lifecycle & cross-route ownership** | 5-shell cap enforced; `resumeShell` assigned on open/switch; switching shells preserves mirror/history/draft/scroll offset; 10 s `CLAIM_RELEASE_MS` removed; `closeNow()` calls `RadioLease.release()`; `WakeLock` renewed on timer; `device.id = hex(phone_static_pub[..8])` shared across Tailnet and Radio (switching routes never triggers `HeldBy` self-lockout); desktop takeover sends zero unsolicited radio packets; `native-freshness.test.ts` checks both `.so` libraries under `io_ldmux`. |
| **B8: Full-stack `VirtualMesh` end-to-end gate** | **Implemented** in `crates/daemon/tests/mesh_full_stack.rs`: the real `LinkLoop` (the loop `session()` runs) with a real `DaemonHost` over `LdmuxDaemon` + the in-memory keeper, against the real `ldmux_session::client::Client`, over a `VirtualMesh` with two repeaters, two-byte path hashes and 15 % loss. Seven scenarios, each a condition reached by stepping a virtual clock rather than a sleep: handshake and first screen; a command written **once** and settled; a multiline paste wrapped in the pane's own markers; two shells as two pages with each command reaching its own (`B11`); a `Fetch` answered without touching the pane; a 20 s blackout recovered with **no** new handshake; a host restart recovered by `KIND_NEED_HANDSHAKE` against the same pinned key. The harness lives in `crates/radio-harness` (a dev-dependency of both crates) because a module under one crate's `tests/` cannot be reached from the other — which is why this gate did not exist while both halves were green. |

---

## 10. Implementation Stages (`S0`–`S5`)

| Stage | Architectural transformation & scope | Verification gate |
|---|---|---|
| **S0: Crate feature split, `VirtualMesh` harness, observability & gate fix** | Gate host-only `tokio`/`nix` IPC in `ldmux-protocol` (`host-ipc`) and move pure companion framing + `CompanionFifo` into `ldmux-mesh`; build `VirtualMesh` repeater simulator (`logRxRaw` 173 B filter, `SimpleMeshTables` dedup, half-duplex repeater collisions); fix `native-freshness.test.ts` (`io_ldmux` + `libldmux_session.so`). | `scripts/test-parallel.py`, `nix run .#mobile-test` |
| **S1: L0 strict companion FIFO, BLE pipe & `RadioLease`** | Enforce strict 1-command-at-a-time `CompanionFifo` serialization; non-blocking `not_before` queue in `mesh_serial.rs`; frame parser resync; delete connect-time `sendSelfAdvert`/`maybeAdvertise` and `LDRH`; fix `RadioLease.release` in `closeNow()` and `WakeLock` renewal. | B1, B7 |
| **S2: A1 + A4 — Unified `RadioEndpoint`, 1-RTT bootstrap & Transaction Layer** | Unify `RouteTable` + `carrier_for` + `Handshake` (`KIND_MSG1_FRAG`, stable retry key + `tx_nonce`, idempotent responder cache, `KIND_NEED_HANDSHAKE`) + Message-Oriented Transaction Layer (`KIND_UP`/`KIND_DOWN`, `tx_id`, `part_idx`/`part_count`, `SUB_A`/`SUB_B`, burst-tail `ack_mask`, persistent `StatelessTransportState`) + self-echo rejection (`hops == 0` & direction byte). | B1, B2, B3, B4 |
| **S3: A3 — Host `MeshRpcService`, `PaneStream` settle engine & Protocol v15** | Replace 50 ms streaming `mesh_bridge.rs` and `Exchange` ring with single-task `MeshRpcService` + `PaneStream` eventfd settle engine; implement Protocol v15 (`Send` with coalesced text/paste/keys + deflate, `Fetch`, atomic `Snapshot` with embedded `Applied`); `(device_id, op_id)` dedup ring; Single-Burst `3.5 s` Settle-or-Midstate Rule; unified `hex(phone_static_pub[..8])` ownership & silent desktop takeover. | B5, B6, B7 |
| **S4: A2 — Shared Rust Client Engine over JNI (`crates/session-ffi`)** | Expose the unified Rust client engine (`CompanionFifo` + `RadioEndpoint` + `mesh_term` client + per-shell `ScreenMirror`) through `crates/session-ffi` (`libldmux_session.so`) to Android; wire `VirtualMesh` end-to-end gate between Phone Engine and Host `MeshRpcService`. | B8 |
| **S5: A5 — Mobile UI unification, draft renderer, viewport fix & delete legacy stack** | Unify Radio and Tailnet pager/launcher in `AppContent.kt` (`MAX_OPEN_PAGES = 5`, `shownSessionId`, per-shell `MeshPageController` + `BatchBar`); fix `InputDelta.kt` multiline paste (`InputOp.Paste`); fix non-scrolling `⏎` provisional draft echo; fix keyboard-up grid gating and cursor-aware `GridPlacement`; delete `vendor/libmeshcore*` and legacy duplicated Kotlin driver code. | B5, B6, B7, B8 |
| **S6: A2 + the bridge replacement — one stage, after the phone** | Replace the host transport **and** the bridge in the same change, because a swapped transport has no `Link` to preserve: `RadioEndpoint` over `SerialCarrier` serves the phone, and the frame loop that consumed `Link::take_inbound() -> Vec<(u8, Vec<u8>)>` is deleted with it. The wire does not change: it is already 15. Scoped in §10a. | B8, B11 |

---

## 11. Implementation status (2026-11-12)

What has landed, what the tests prove, and what is still open. Every claim here is a file in the
tree or a command that was run; nothing is a plan.

| Stage | Status | What exists |
|---|---|---|
| **S0** | **Done** | `ldmux-protocol` has a `host-ipc` feature (default on) owning `tokio` + `nix`; `cargo tree -p ldmux-protocol --no-default-features` is `serde`, `serde_json`, `uuid`, `flate2` and nothing else. `crates/mesh/src/sim/` is the `VirtualMesh` harness (173-byte receive ceiling, `SHA256(payload_type‖payload)` 160-entry dedup, the origin's own first-hop echo, half-duplex deafness, per-hop forward delay, path accumulation and consumption, flood hop caps, scoping) with `airtime.rs` beside it, pinned against the companion's own time-on-air by `crates/companion/tests/airtime_agreement.rs`. `native-freshness.test.ts` now checks both `libldmux_terminal.so` and `libldmux_session.so` for staleness and for their `Java_io_ldmux_*` symbols (6/6). |
| **S1** | **Done** | `crates/mesh/src/budget.rs` (`172 − path` direct, `161 − path − scope` flood, 93 guaranteed and 125 near, all derived and compile-time pinned). `crates/mesh/src/route.rs` (`Candidate`, `ProofWitness` as the only route to `Proven`, SNR-margin ranking, `carrier_for`). `crates/session/src/direction.rs` (the six kinds, one definition, re-exported by `handshake.rs` and `record.rs`). `crates/mesh/src/companion/fifo.rs` plus the host's answer contract in `crates/companion/src/window.rs`: one command in flight, settled only by its own answer, a foreign answer reported rather than consumed. |
| **S2** | **Done** | `crates/session/src/bootstrap.rs`: `Initiator` holds one ephemeral across every rung, `msg1` direct 123 B or two fragments 67/66 B with a per-transmit `tx_nonce`, `msg2` 52 B, `KIND_NEED_HANDSHAKE` 25 B, `Responder` with a 64-entry ±5 min cache that re-emits its cached answer without rebuilding the session, and a 5 s answer rate limit. `crates/session/src/transaction.rs`: 10-byte header, parts of 118 B so any part halves onto the narrow carrier, selective `ack_mask` retransmission, ack suppression to the burst tail, clean supersession, and a timeout that leaves the session intact. `crates/session/src/endpoint.rs`: one state machine for both ends — role, bootstrap, per-attempt carriage, post-authentication route updates, transaction dispatch, deadlines and a `Stats` that names every rejection. `crates/session/tests/endpoint.rs` runs two real endpoints over one `VirtualMesh`; the six-repeater, 15 %-loss gate delivers a request and its answer with the route proven by a direct arrival. |
| **S3** | **Live for a version-15 phone** | Done: the three messages are in the wire contract and pinned by the shared fixture on both sides (`T_SEND = 0x0E`, `T_FETCH = 0x0F`, `T_SNAPSHOT = 0x8E`, with the chunk envelope of §12.2). Done: `crates/daemon/src/mesh_rpc.rs` is the service — chunk assembly, the 64-entry `(device, op_id)` at-most-once ring, and the settle engine (`SETTLE_QUIET_MS` 500, `SETTLE_NO_CHANGE_MS` 700, `INLINE_SETTLE_MAX_MS` 3_500) — behind a five-operation `MeshHost` seam, with ten unit tests and four end-to-end tests through the real `Bridge`. Done: `crates/daemon/src/mesh_bridge.rs` dispatches `Send` and `Fetch` to it and **builds no frame for a shell whose phone attached as 15**, so one host serves both protocols and a retransmitted command is never typed twice. Done: `VERSION` is 15 and the phone speaks it, so §12.4's half-migration is closed, with §12.7a's rule keeping the frame loop alive until a phone actually uses the new model. Done: `crates/daemon/src/mesh_carrier.rs` is the `CarriageSink` over the real radio — pure `build_packet` for both carriages, the carriage travelling *with* the send so a lost `set_carriage` cannot mislabel the next record, and `classify_push` keyed on the `LDMR` magic (§12.7b). Done: every command in `crates/companion` goes through `CompanionFifo` with the `window.rs` answer table, so a response that is not the in-flight command's settles nothing (§12.7, `RC-2`), pinned by seven `fifo_wiring` tests including the exact field sequence. The transport swap is **not** S3's tail: it moved to S6 (§10a), because `Link`'s page byte is a frame-loop fact and a swapped transport has no `Link` to preserve. |
| **S4** | **Done** | `crates/session-ffi` gains the shared client engine (companion FIFO, `RadioEndpoint`, `mesh_term` client) behind the pull-only C ABI in §8.1, so the phone stops reimplementing the protocol in Kotlin. |
| **S5** | **Done** | The screen runs on the engine and Kotlin holds no protocol on the radio path. App fixes: the claim wedge (`CLAIM_RELEASE_MS` gone, `resumeShell` by one rule), the IME-gated grid and cursor-aware placement, multiline paste as one paste operation on both routes, the provisional draft drawn with `⏎`/`⇥` and windowed by display width, and the flood bucket actually metered. **S5a** the values-only ABI (`ldmux_client_jni.c`, `NativeRadioClient.kt`, `AndroidBlePipe.kt`); the capability survey then added the six features the engine could not express. **S5b-0** radio management moved into the engine (`ldmux_client_radio`/`_route`/`_set_scope`/`_set_preset`, sentinels rather than zeros, `RadioLease` on the pipe). **S5b-1a** `RadioEngine` + `AndroidRadioEngine` (one loop on a single-thread dispatcher, one timer from `nextDeadline`) and `MeshEngineDriver`, the translator that speaks the screen's existing `MeshTermView` from engine values. **S5b-1b** the source and the screen on the engine: `MeshTermDriver` (2,552), `MeshInput` (538), `PublicChannelBar` (277), `PublicChannelHold` (318) and `RouteHint` (41) deleted, the bar rebuilt as `Send`/`Refresh` from `ClientEvent.Settled`, and `LDMUX_ITEM_STREAM` on `ldmux_client_send` so a draft's text and its Enter are **one** record (`RC-8` closed at the ABI, not moved down a layer). **S5b-1c** the settings screens on the engine behind one refcounted factory, so a second screen cannot open a second GATT; the four facts the settings screen shows came back through a length-prefixed ABI; metering restored and pinned by `no_allowance_means_no_record_starts`; `AndroidRadioLink` and `RadioRoute` deleted; `vendor/libmeshcore` gone, its 479 surviving lines of GATT plumbing renamed to `:ble` (`io.ldmux.mobile.ble`). |
| **S6** | **Done** | **B8** is now a real gate (`crates/daemon/tests/mesh_full_stack.rs`, seven scenarios over `VirtualMesh`), which is the join `AR-15` showed was missing. **S6a**: `MeshRpc::on_message` dispatches every message the app sends and `MeshHost` grew the nine calls it needed, each chosen so a rule has one owner (the host decides whether a holder is in use; `release` reports a refusal as a state). **S6b**: the link task drives a `RadioEndpoint` over the `RadioCarrier` and hands every delivered message to the service — **12,281 lines removed, 1,717 added** — with `mesh_bridge.rs` down to `BridgeStats` + `DaemonHost`, `mesh_serial.rs`'s route policy gone, and `mesh_session.rs` deleted. A version-14 claim gets one `Closed { reason: VERSION }`. Paste reads the bracketed-paste mode at send time, because bash sets it with its first prompt. **S6c**: the v14 C ABI deleted (`arq.rs`, `session.rs`, their tests, `ldmux_session_jni.c`, the old `session-ffi` entry points) — **4,846 lines removed** — with the one remaining call, a keypair for a fresh pairing, added as `ldmux_client_keypair`; `LDMUX_ERR_*` re-derived from the header's own numbers, which the Android tests caught. Old stack deleted in this work: **≈31,000 lines**; new: ≈26,000 including ≈9,000 of tests. |

- **`CMD_APP_START` carries an app name, and the firmware ignores it without one.**
  `MyMesh.cpp:1076-1080` handles the command only under `cmd_frame[0] == CMD_APP_START && len >= 8`,
  reading the name from `cmd_frame[8]`; the shared builder sent a single `0x01`. Measured on the
  phone 2026-11-12: the radio accepted every write (`ble send 1B` / `ble send ok 1B`, in a loop) and
  answered nothing, so `SELF_INFO` never arrived, the settings screen read "not read" for ever, and
  the link could not start. `command::app_start(app_name)` now emits `0x01`, seven reserved bytes and
  the name, and both ends call it.
- **A session record cannot ride a flood on stock firmware.** See AR-10's corrected disposition:
  `REQ` is decrypted per peer before delivery, so the plaintext `LDMR` wrapper inside one is
  dropped, and the flood carrier is visible only through the receive log's debug tap. The direct
  carrier is the delivered one, and a direct arrival is delivered with no destination check
  (`Mesh.cpp:292-298`), so an empty path reaches every companion in range.
- **A retransmission is already new bytes.** The transaction header's `tx_nonce` is sealed fresh on
  every transmission (`endpoint.rs`), so the receiver's `wasSeen` hash differs and the "identical
  bytes are dropped" worry applies to the bootstrap's first form, not to transactions. No wrapper is
  owed on the direct carrier, which is why its budget stays `172 − path`.

- **A path is the only way past one hop, and the firmware can supply it.** `CMD_SEND_SELF_ADVERT`
  (mode 1) has the radio emit a flooded advert, and `CMD_GET_ADVERT_PATH` returns the chain that
  advert took, in travel order **towards** us — so a sender reverses it hop group by hop group
  (`crates/mesh/src/companion/command.rs::AdvertPath::outbound`). Both ends now ask for it: the host
  on bring-up, the phone whenever its route table holds no candidate. A learned chain is stored as
  `Proof::Candidate` and can never be mistaken for a proven route.
- **The host measures its own radio.** Three status lines used to be defaults printed as readings:
  a `0 dBm` noise floor (physically impossible), `0 s transmitted` after an hour of transmissions,
  and "no reading yet" for a circuit that had heard 641 s of airtime. The carrier now asks
  `GET_STATS radio` on bring-up and every fifteen seconds, the record path borrows the last measured
  noise floor and airtime instead of publishing zeros, and the airtime line reports the radio's own
  counter when it has one.

### Corrections the implementation forced on this document

Each was found by building the thing and is now reflected in the sections above:

- **`KIND_MSG1_FRAG` carries its own `tx_nonce`** (67/66 B, not 65/64): a fragment is used on direct paths too, where nothing else varies, so a byte-identical retry would be dropped by `wasSeen`.
- **`msg2` carries the session index sealed inside the Noise message** (`MSG2_LEN` is 50: `[kind][noise 49]`), not as a clear byte; the total with the nonce is still 52.
- **The session wrapper is 24 bytes**, not 22 (`6 clear + 2 inner + 16 tag`), so a 171-byte record carries 137 payload bytes, and parts are cut to 118.
- **Two defects the end-to-end gate found in the new code, both fixed**: an acknowledgement piggybacked on a data record was decoded and then dropped (`transaction.rs` applied it only in the pure-ack branch), so every delivered message still timed out; and the endpoint learned a route only from handshake traffic, so a candidate expired 120 s after the bootstrap and direct was never used again.
- **A `Send` and a `Snapshot` both state their uncompressed length in the clear** (`items_len`, `patch_len`), because a decoder has no other way to size an inflate: the first form of both omitted it and the decoder could only guess, which `Frame` had already solved the same way. A `Snapshot`'s patch stream is the *only* part that is ever deflated, and a body over `MAX_BODY` is refused rather than split, which is what makes `Err(TooLong)` the signal to send a smaller diff.
- **`VERSION` was not bumped, and that is a deliberate correction to this document's §10.** A version byte is what a build can answer; these messages exist and are pinned, but nothing serves one until the daemon's receive path is the transaction service, so a phone that attached as 15 would wait for an answer nothing sends. §10 `S3` now says `TRANSACTION_VERSION` first and the `VERSION` bump with the host half, in one commit.

---

## 12. Critical audit of the planned architecture (2026-11-11)

Written after S0–S2 landed and S3 began, against the code as it exists rather than as this page
imagines it. Each finding names the evidence, states whether the plan, the code or both are wrong,
and gives a disposition. Nothing here is a preference; anything that changes the design is scoped
against the rest of the system, and the sections above are amended where the finding changes them.

### 12.1 What is actually wired, and what is only tested

| Component | Tests | Production path | Verdict |
|---|---|---|---|
| `ldmux_mesh` (budget, route, sim, airtime, `CompanionFifo`) | 47 tests | only `budget`/`route`/`sim` used by `ldmux-session`; `CompanionFifo` used by nothing | **inert** |
| `ldmux_session` (bootstrap, transaction, endpoint, direction) | 87 tests | `handshake`/`record`/`session`/`arq` used by the daemon; `bootstrap`, `transaction`, `endpoint`, `direction` used by nothing | **inert** |
| `ldmux_protocol` v15 (`Send`, `Fetch`, `Snapshot`) | 129 + 735 tests | **live**: a phone that attaches as 15 is served by it | **wired** |
| `crates/daemon/src/mesh_apply.rs` and `crates/daemon/src/mesh_rpc.rs` | 10 unit + 4 end-to-end | **live** for a version-15 phone; a version-14 phone keeps the frame loop | **wired** |
| `crates/daemon/src/mesh_bridge.rs` (6,305 lines), `mesh.rs`, `mesh_serial.rs`, `mesh_session.rs` | the whole e2e suite | **this is the transport**, for both protocol versions | live |

**AR-1 — the tree is green while the deployed path is unchanged, and that is the most dangerous
state this work can be in.** Every test that proves the new design passes, and the code the phone
actually talks to is untouched. A reader can easily take `scripts/test-parallel.py` passing as
evidence that the radio works; it is evidence that two disjoint stacks each work. The plan's gates
(B1–B8) do not distinguish them, because the gates test the new stack and the e2e tests test the old
one.

*Disposition:* **S6** (§10a) is the stage that removes the ambiguity, and it must **delete** the old modules
rather than leave them beside the new ones — a fallback path that nobody chose is how two designs
survive a year. Until it lands, §11's status table is the only truthful statement of what is live,
and this table belongs beside it.

### 12.2 A message cannot always fit the transport that must carry it

The plan's §6 makes every request and every answer **one message**, and §5.3 makes one message **one
transaction** of at most 16 parts. The arithmetic, as implemented:

- a part carries **118 bytes** (`transaction.rs`, `MAX_PARTS = 16`),
- so one message carries at most **1,888 bytes**,
- while the codec allows a `T_SEND` body of **600** bytes (`MAX_BODY`) and a `T_SNAPSHOT` body that
  inflates to **2,048** (`MAX_SNAPSHOT_INFLATED`).

Three separate ceilings with no ordering between them. Two concrete failures follow.

**AR-2 — a snapshot between 1,888 and 2,048 bytes is accepted by the codec and rejected by the
transport.** `T_SNAPSHOT` checks `stream.len() > MAX_SNAPSHOT_INFLATED` (`mesh_term.rs:1120`) and the
transaction layer then refuses a message it cannot part. The host would build an answer, fail to
send it, and the phone would wait out its ladder.

*Disposition (done 2026-11-11):* the ordering is now **proved** rather than assumed.
`crates/session/tests/payload_fits.rs` asserts that a body of `MAX_BODY` plus its two framing bytes
fits one transaction at the **worst** legal record sizes (171 direct, 93 flooded, so the proof holds
at every hop count and path width), that a command at `MAX_COMMAND_TOTAL` and a screen at
`MAX_SNAPSHOT_TOTAL` each need no more chunks than the field can number, and that a chunk still
holds one whole item. It lives in a test because the wire and the transport deliberately cannot see
each other: `ldmux-protocol` must not learn the session and `ldmux-session` must not learn the
terminal protocol, so `ldmux-protocol` is a **dev-dependency** of `ldmux-session` and nothing more.

**AR-3 — a paste larger than about 550 bytes cannot be sent at all, which is a regression against
the protocol this replaces.** Today `Input` carries 137 bytes and the app sends as many as it needs,
so a 2 kB paste worked (at a ruinous airtime cost). Under v15 the whole draft is one `Send`, whose
body is capped at `MAX_BODY = 600`; deflate helps text but not code or base64, and the transport cap
of 1,888 would bite next. A terminal that refuses a 40-line paste is not a terminal.

*Disposition — a scoped wire change, made now because version 15 has never been deployed:* both
`T_SEND` and `T_SNAPSHOT` gain **`chunk_idx: u8` and `chunk_count: u8`**, and the receiver
reassembles by `(peer, op_id)` / `(peer, snapshot_id)` before acting. This keeps the plan's two
invariants — one command is applied at most once, and a command is applied whole or not at all —
while removing the size ceiling. It is deliberately *not* a change to the transaction layer:
transactions already retransmit parts, and a second fragmentation scheme inside them would be a
second thing to get wrong. The host holds chunks until `chunk_idx == chunk_count - 1`, applies the
joined bytes once, and answers with a `Snapshot`; a partial paste is never written to a pty, so the
failure mode is a retry rather than half a command running.

*Implemented 2026-11-11, on both ends, with the fixture as the cross-language proof:* the field is
on `Message::Send` and `Message::Snapshot` in `crates/protocol/src/mesh_term.rs` (headers 14 and 25
bytes), on `TermMessage.Send` and `TermMessage.Snapshot` in the app's `MeshTerm.kt`, and the
regenerated fixture carries a three-chunk `Send` and a two-chunk `Snapshot` whose `chunkIdx` is not
zero — a codec that reads the header in the wrong order fails there rather than in the field.
`SendAssembly` (host) and `SnapshotAssembly` (phone) are the only way to consume chunks, and they
enforce the three rules that make the envelope safe: nothing is handed on until the last chunk, a
duplicated chunk is a repair rather than a fault, and a chunk of another command or another screen
is refused **without** touching the assembly in progress, so the caller decides which one died.
Both are bounded (`MAX_COMMAND_TOTAL` 16 KiB, `MAX_SNAPSHOT_TOTAL` 32 KiB) and both have tests,
including a 16 KiB command assembled from chunks and re-encoded, and a refused chunk that leaves
the first command intact. `MAX_SEND_ITEM` stays 400, so a long paste is several adjacent `Paste`
items: `apply_send` now joins adjacent items **of the same kind** as raw bytes *before* the
bracketed-paste rule runs, so a 2 kB paste is one wrapped region rather than five, while typed text
and a paste stay separate runs because they mean different things (a test pins both).

### 12.3 The gates do not run the configuration that ships

**AR-4 — `crates/session/tests/endpoint.rs` builds its endpoints with `max_rungs(12)`,
`max_attempts(6)` and `rto_ms(2_000)`, while the shipped defaults are 3, 5 and a value derived from
airtime.** The gate therefore proves a design that retries four times as often as the real one. A
gate is only as good as the difference between its configuration and production's, and here that
difference is exactly the parameter the field failures were about (how long the phone keeps asking).

*Disposition (done 2026-11-11):* `Run::shipped` builds the same link with **no** tuning applied —
the stress values are now applied only when a test asks for them, so `shipped` is the default
configuration rather than a restatement of it that could drift. `a_link_comes_up_at_the_retry_ladder
_that_ships` connects, sends and receives one repeater away at those defaults, and `Run::new` says
in its doc comment that it stresses the ladder and why. The `rto_ms` in the harness is no longer a
fixed 2 s either: it is `retransmission_timeout_ms(repeaters)`, derived from the route.

### 12.4 Version negotiation is half-migrated

**AR-5 — `VERSION` is 14 and `TRANSACTION_VERSION` is 15.** The reasoning (nothing serves `Send`
until the host half lands, so a phone that attaches as 15 would wait for an answer that no code
sends) is correct, and the constant split makes the dependency explicit. The risk is that the split
outlives its reason: an app that can encode v15, negotiates v14, and is tested against v15 fixtures
looks finished and is not. *Disposition:* keep the split, but make it a **compile-time coupling**
in the host half: the service that serves `Send` sets `mesh_term::VERSION` to 15 in the same change
that gives it a receive path; that happened in S3, and **S6** (§10a) is the only stage that may delete the frame loop. Record it in §11
(already done) and in the release audit when the daemon changes. **Closed 2026-11-11**: the host
now serves `Send`/`Fetch`, `VERSION` is 15, and the app's own constant moved with it.

### 12.5 Route proof is one-sided by construction

**AR-6 — only the end that sent a direct packet can mark a path proven, because `prove()` needs
`last_direct_path`** (`endpoint.rs`). The six-hop gate shows the asymmetry exactly: the host proves,
the phone does not. This is not a defect — a receiver of a direct packet has had its path consumed
and genuinely knows nothing about it — but it means `routes_proven` is a measure of *sending*
direct, not of holding a good route, and a UI or a log that reads it as route quality is wrong.
*Disposition:* keep the rule, document what the counter means where it is surfaced, and let the
phone's candidate rank direct traffic without proof (it already does).

### 12.6 Dependency and boundary decisions the plan implied but did not record

**AR-7 — `sha2` was added to `ldmux-mesh`** for the simulator's duplicate hash, and `ldmux-mesh` is
linked into the Android `.so`. `AGENTS.md` requires an ADR line for a new dependency. It is pure
Rust and already in the tree through `ldmux-session`, so the decision is small, but an unrecorded
exception becomes the precedent for the next one. *Disposition:* one line in ADR-0039 naming the
crate, why the simulator must hash exactly as the firmware does, and why nothing C-backed is
acceptable there.

**AR-8 — the companion command table lives in `ldmux-companion` while the queue lives in
`ldmux-mesh`.** That split is right — the FIFO is shared, the host's answer set is host-specific —
but the phone's half of the client (S4) must supply its own table, and the plan never said so.
*Disposition:* §8.1 names the table as an input to the client engine rather than something the
engine derives.

### 12.7 What the audit does **not** find overscoped

The question was whether the plan asked for more than the problem needs. On the evidence, no:

- **`SUB_A`/`SUB_B` narrow-carrier halves** are not speculative: the flood budget is 59 payload
  bytes and a direct part is 118, so any direct part retried on a flood needs exactly this split.
  Removing it would mean re-cutting a part, which is the defect `RC-7` recorded.
- **The per-transmit `tx_nonce`** is not decoration: without it a retried record is byte-identical
  and `wasSeen` drops it at the first repeater, measured in the harness as 292 drops in one run.
- **`KIND_NEED_HANDSHAKE`** is the only mechanism that lets a phone recover in seconds after a
  daemon restart; the alternative is every phone waiting out its ladder, which was `RC-5`.
- **`CompanionFifo`** is the whole of `RC-2`; it is 582 lines including its tests and has no
  alternative that is smaller.
- **The `VirtualMesh` harness** paid for itself twice inside one stage: it found the dropped
  piggybacked ack and the route that was never re-learned, neither of which any unit test of the
  parts would have caught.

The one place the plan is **underscoped** is chunking (§12.2, AR-3): it assumed every command fits
one message, and a paste does not. That is being corrected before version 15 ships, which is the
cheapest moment it will ever be.

### 12.7a AR-9 — a version claim is not a migration

**Found by reading the B-stage wiring rather than by a test, and it would have bricked the app.**
The host stopped streaming `Rows`/`Commit` for any shell whose phone attached with version 15
(`speaks_v15`), on the reasoning that a v15 phone is served by `Snapshot`. The app was already
carrying `VERSION = 15` because its codec can decode the new messages, while the controls that
send `Send`/`Fetch` are the S5 work that has not landed. Deployed, that pairing gives a phone that
claims 15, receives no rows, and never asks for a screen: a **frozen picture on every shell**, with
no error anywhere.

A version byte is what a build can **decode**. It is not a promise that the build has stopped
relying on the old path. The host now keeps the frame loop until the phone *demonstrates* the
request/answer model by sending a `Send` or a `Fetch`, and stops streaming for that shell only from
that moment (`MeshRpc::has_driven`/`mark_driven`, `mesh_bridge.rs`). A phone that has not been
updated cannot be broken by the host being ready first, which is what a migration is supposed to
mean.

Pinned by `a_version_15_phone_keeps_receiving_rows_until_it_asks_for_its_screen`: rows before the
first request, none after it.

### 12.7b AR-10 — a flooded record does not arrive as the payload type its name suggests, and it arrives through the receive log

**Found while writing the carrier's receive classifier, by a test rather than by reading.** The
firmware flood-routes `PAYLOAD_TYPE_REQ`; a session record's flood wrapper (`LDMR`) sits **inside**
a `REQ` packet, not inside a `RAW_CUSTOM` one. A classifier keyed on `RAW_CUSTOM` — which is what a
reader of `ADR-0029` would naturally write, because the direct carrier really is `RAW_CUSTOM` —
accepts every direct arrival and **silently ignores every flood arrival**. That is the same shape as
the original field failure: the record crosses the mesh, is heard, and never reaches the
application, with no error on either end.

*Disposition, corrected 2026-11-12 after reading the delivery path:* classification is by the
**push code first and the wrapper magic second**, and the correction matters more than the original
finding. The firmware reports a received packet as one of three pushes and they do not share a
layout (`examples/companion_radio/MyMesh.cpp`):

| push | frame | what it is |
|---|---|---|
| `0x84` `PUSH_CODE_RAW_DATA` | `[0x84][snr×4][rssi][0xFF][payload]` | a **direct** `RAW_CUSTOM`, handed to the application (`:832-853`) |
| `0x8E` `PUSH_CODE_CONTROL_DATA` | `[0x8E][snr×4][rssi][path_len][payload]` | a **control** packet, zero-hop and `payload[0] & 0x80` only (`Mesh.cpp:70-76`) — never a record |
| `0x88` `PUSH_CODE_LOG_RX_DATA` | `[0x88][snr×4][rssi][whole packet]` | the receive **log**, a debug tap over every packet the radio demodulates (`:291-301`) |

**A flood is delivered as the receive log, and that is a delivery path.** `REQ` is in the
firmware's decrypt-only family (`Mesh.cpp:131-147`: `dest_hash`, `src_hash`, then a MAC per peer), so
a plaintext `LDMR` wrapper inside a `REQ` fails every peer's MAC and is forwarded rather than handed
up as a payload. It reaches the application through `PUSH_CODE_LOG_RX_DATA` instead, and that push is
**not** a debug tap an operator has to switch on: `Dispatcher::checkRecv` calls `logRxRaw` for every
demodulated packet (`src/Dispatcher.cpp:199`), and `MyMesh::logRxRaw` writes the frame whenever the
serial is connected, with no preference gate (`examples/companion_radio/MyMesh.cpp:291-297`). The
`Dispatcher.h:159` no-op is the bare-node default, which the companion radio overrides.

*Corrected 2026-11-13.* This section previously said a flood had no delivery path at all and that the
flood carrier depended on a radio feature we did not own. That was wrong, and it was load-bearing:
it justified sending the answer direct-first as a necessity rather than as the cost decision it is,
and it left `ADR-0037`'s flood fallback looking undeliverable when it is not. The direct carrier is
still the cheaper one — it crosses only the repeaters in its path, while a flood costs every repeater
in range a transmission — and it is still the one that arrives first.

The classifier was written against `0x88` alone. Measured in the field on 2026-11-12, with both
radios reading `910.525 / 62.5 / SF7 / CR5 / path 1` in one room and the phone's radio answering
`OK` to every send: the host reported **0 bytes received and 0 handshakes** for its whole run,
because a real arrival comes as `0x84` and the classifier returned `None`. `crates/mesh/src/
companion/push.rs` now reads all three codes with their own layouts, pinned by
`a_direct_record_arrives_as_raw_data_and_not_as_the_debug_log`,
`a_control_push_is_not_a_record_carrier` and
`a_flood_is_read_from_the_receive_log_and_keeps_its_sender`.

*The remaining consequence, recorded here as open work:* reaching past one hop needs a **path**, and
the firmware can give one without any custom protocol — `CMD_SEND_SELF_ADVERT` (mode 1, flood) and
`CMD_GET_ADVERT_PATH`, whose chain is appended per hop and must be reversed for the outbound
direction.

### 12.7b-2 AR-16 — the range arithmetic, and the product statement it forces

**Measured and computed 2026-11-13, after the operator's correction that no version has ever worked
at range.** This section exists because the plan optimised for what could be demonstrated — a link in
one room — and never stated what the band can carry at distance.

One transmission crosses `n` hops with probability `q = (1−p)^n`, where `p` is the per-hop loss, so
the expected transmissions per delivery is `1/q`:

| per-hop loss | q at 12 hops | attempts for 95 % | q at 30 hops | attempts for 95 % |
|---|---|---|---|---|
| 5 % | 0.54 | 5 | 0.21 | 13 |
| 10 % | 0.28 | 9 | 0.042 | 70 |
| 20 % | 0.069 | 42 | 0.0012 | 2500 |

A screen update is a request **and** a response, so it costs `2/q` records. Airtime is the measured
602.6 ms per full 172-byte record and the public band's 1 % hour is 36 s, which is 59.7 full records an
hour — so a 12-hop update at 10 % loss is about seven records, or one update every seven minutes.
**A 64-byte record is about 140 ms rather than 224 ms, because the preamble is fixed**, which is why
small records and delta screens are worth more than any retry tuning: the same hour carries 257 of
them instead of 59.

**The product statement this forces.** Interactive typing is deliverable up to roughly twelve hops at
10 % per-hop loss or better, where a small-record update lands in one to two minutes. At 30 hops and
10 % loss it is about eleven minutes even with small records, and at 20 % it is hours. Beyond that the
honest product is **store-and-forward**: send a command, read the result minutes later.

**What is not yet measured, and gates every number above: the per-hop loss.** The field's SNR range
(−6.75 to +5.5 dB against a −7.5 dB demodulation floor) implies anything from under 1 % to over 50 %.
That measurement is the first thing a range trip should take, and every ladder and retry decision
should be re-derived from it rather than from the table above.

**A path is airtime, not reachability.** A direct send crosses the same hops and loses the same `p`;
what it saves is fan-out — a flood costs every repeater in range a transmission, a direct packet costs
one per hop. That is why the route request matters at range even though it cannot improve delivery.

### 12.7b-3 AR-17 — intentionality: what the refactor meant to do, and where the intent itself was wrong

**Written 2026-11-13**, after the operator's correction that **no version has ever worked at range** —
the old stack carried a shell only when the repeaters were in the same room. This section judges the
decisions, not the bugs; the bugs are in §12.8 and the arithmetic is AR-16.

**What the old stack did.** It asked for a route with a flooded `LDRQ`, took the `LDRR` reply's chain,
and sent the session over that path with a byte-stream ARQ. In one room that gave one request, one
reply, and a session on the first handshake (`RELEASE-AUDIT.md` 6.41), and a command's output came
back (17.60). At range it never worked, and the reason is now visible: the route request was only
ever exercised across one or two hops, the ARQ's ceiling was below its own first rung (6.177), and
nothing in the design accounted for the loss product over 14–35 hops (AR-16).

**What the refactor intended.** Replace the byte-stream ARQ with a message-oriented transaction layer
(ADR-0039), move the protocol out of Kotlin into Rust (A2 there), give the radio one carrier
abstraction, and route by advert path instead of by request (`ADR-0030`, `ADR-0032`, `ADR-0035`).

**Where the intent itself was wrong — four decisions, not bugs:**

1. **The route request was deleted on a false premise.** `RADIO-PLAN.md:157` justified it: the phone
   sends "only `msg1`, which both discovers the route and initiates the handshake in a single round
   trip". A flood's chain teaches the **receiver** the road back, and `learn()` runs on arrivals, so
   the sender learns nothing from its own transmission. The plan removed the only mechanism that
   could carry a route past the advert cap, and recorded the removal as a simplification.
2. **The advert path was assumed sufficient without checking the cap.** A repeater drops an advert at
   `flood_max_advert`, 8 by default (`examples/simple_repeater/MyMesh.cpp:972`,
   `src/helpers/RoutingPolicy.h:16-22`), against a 14–35 hop mesh. The replacement could not reach
   the distance the requirement is about, and no audit checked the constant.
3. **The airtime budget of the requirement was never stated**, so no decision was ever measured
   against it. AR-16 now says a 12-hop screen update at 10 % loss is about seven records and a 30-hop
   one is about forty-seven: the design was chosen without that number on the page.
4. **The reachability claim was written before the delivery path was read.** AR-10 first said a flood
   "has no delivery path at all", which is false (`src/Dispatcher.cpp:199`, `MyMesh.cpp:291-297`), and
   that false premise then justified the answer's carriage order and the flood fallback's status.

**The meta-pattern, and it is the one to keep:** the plan reasoned from our own abstractions — route
table, carriers, proofs, a simulated mesh — instead of from the firmware's behaviour and the airtime
arithmetic. The simulator encoded the same assumptions, so it could not falsify them. Every field
failure of 2026-11-12/13 was an assumption the simulator shared, which is why the gate was green
throughout a period in which the product never worked at range.

**What follows for the way this is worked on.** A claim about the firmware is checked against the
firmware before it becomes a design input. A requirement that depends on a physical quantity states
the quantity and its arithmetic. A change that removes a capability says which capability replaces it
and measures that the replacement reaches the same distance. And the in-room case runs on every
change, because a regression gate that never exercises the working path is not a gate.

### 12.7b-4 AR-18 — the acknowledgement is already in every header, and the naive fix for it is refused

**Found 2026-11-13, fixed wrongly, then reverted — recorded so the next attempt starts here.**

Every part's transaction header already carries `ack_tx_id` and `ack_mask`
(`crates/session/src/transaction.rs`, `Header`), so a reply acknowledges the request it replies to at
no cost. But `poll` tries `readied_ack` before `take_part`, so on every request/response exchange the
receiver sends a **standalone acknowledgement first** and then the reply that carries the same
acknowledgement: two records for one fact, and on a screen ask the ack is a third of the answer's
airtime.

Tried: take the part first when it would carry the ack, and clear `ack_owed` when a part carries one.
That halves the ack traffic in isolation and is pinned by a test that a reply carries the ack and no
standalone one follows. **It broke `a_route_from_nothing_carries_a_message_and_its_answer_six_repeaters
_away`**: preferring a part starves the acknowledgment whenever this end has outbound work of its own,
and narrowed to "only when the part would actually carry that ack" it still did, because the peer then
retransmits for want of an ack the next part never carried. Reverted rather than left half-right.

**What it actually needs is the correlation the design does not have.** A request/response exchange
should complete the *request* when its *response* arrives, so that no standalone ack is wanted at all
and a lost response is retried as a response. Until that exists, the standalone ack stays and costs
what it costs; the arithmetic in AR-16 already counts it.

### 12.7b-5 AR-19 — the session index must change per session, and a stable one is refused

**Considered and rejected 2026-11-13.** The review recommended a stable index per peer, with the
counter continuing across re-handshakes, to remove a round trip per re-handshake. Measured against
the code, it removes the only signal that the peer restarted.

The index is assigned per handshake, and a record carrying a different one is `WrongIndex`, which now
drops the stale session and re-handshakes (`3bc5957`). A host restart therefore becomes visible: the
phone sees the new index, follows it, and the exchange recovers. Make the index a function of the
pairing and that signal disappears — the phone keeps its old session, the restarted host's counter
begins at zero, and the first record is refused as a duplicate with nothing to recover from, which is
the very state `3bc5957` was written to end.

A stable index would need a counter that survives a restart, which means persisting it; that is more
state, more failure modes, and it still does not detect a restart, it only tolerates one. The round
trip is the price of the detection, and it is worth paying at range because a link that cannot tell
the peer restarted retransmits into a session that no longer exists.

### 12.7c AR-12 — the engine shipped without six features the route already had, and reintroduced a fixed defect

**Found by refusing to rewire, then by a survey I should have asked for at the start of the stage.**
A brief said "put the Android radio route on the Rust engine". The engine, as first written,
covered what the protocol needs and not what the *screen* needs: `create`, `takeover`,
`make_live`, scrollback paging, ending a page, and keys reachable through the ABI were all
missing, and `Message::Created` was decoded by nothing. Rewiring against that surface would have
removed five shipped behaviours silently — the failure shape where nothing errors and the screen
simply stops showing something.

Worse, `Client::open` sent `cols: 0, rows: 0`. That is `RC-11`'s clipped picture, reintroduced
in new code by a constructor that had nothing to fill the fields with, and **no test would have
caught it**: the phone asks for a screen at zero size, the host fits the pty to zero, and the
picture is wrong in a way that looks like a rendering bug.

*Disposition:* every capability the radio path exposes is now enumerated against the engine's
surface before a stage is called done (the table lives in §11's S5 row and in the stage's report),
the six are implemented with tests, and `open` requires the phone's grid. The general rule this
costs: **a new layer is not complete when it speaks the protocol; it is complete when it covers the
features the old one shipped.**

### 12.7d AR-13 — the pipe it was supposed to have was still holding the protocol

**Found by the survey `AR-12` demands, before a line of the rewire was written.** `A2` says the
phone's BLE layer is a pipe: it writes frames and feeds notifications back. The survey of all 31
`MeshTerminalScreen` members found a fourth gap that no protocol work would have closed —
`AndroidRadioLink` still **owns radio management**: `APP_START`, `SELF_INFO`, `STATUS`, the preset,
device info and route reads. The settings screens call it directly for scanning and applying a
tuple.

That has two consequences, and both are architectural rather than cosmetic:

- **`AndroidRadioLink` cannot be deleted**, so the protocol survives in Kotlin whatever the terminal
  path does;
- the terminal path would take the radio while the settings screens kept a second owner of one GATT
  link, and a second owner sending commands outside the engine's FIFO is `RC-2` with a different
  name.

*Disposition:* **S5b-0** moves radio management into the engine — `ldmux_client_radio` for the last
`SELF_INFO`/`STATUS` reading, `ldmux_client_route` for hops/proven/fresh/carriage,
`blind_asks`/`route_answered` on `ClientStats`, the preset and device info through the engine, and
`RadioLease` moving to `AndroidBlePipe` so one owner still asks another app to yield (ADR-0036).
Every one of those commands goes through the same `CompanionFifo` the records use.

Two smaller things the same survey caught, both worth keeping: `hopsNow()` feeds
`answerDeadlineMs(HISTORY_WAIT_MS, hops())`, so without a route fact the up-key loses a page that was
still arriving on a long route; and `marks` needs **no** request at all, because a `Marks` answer
rides the `List` reply.

### 12.7e AR-14 — a killed session can keep its pty master (keeper, unrelated to the radio work)

**Found while making the fd-leak test honest, and it is a real leak, not a flaky test.** Eight
spawn/kill cycles with a live child leave **four `/dev/ptmx` descriptors open against one before**,
reproducibly, and they never come down: a probe printed the same count every second for thirty
seconds. The sessions are gone from the keeper's own `List`, so this is not a registry leak — the
emulator thread is not exiting, or something else still holds its `Arc<OwnedFd>`.

It was invisible because the test counted **every** descriptor in the process with a
twelve-descriptor allowance, so it passed whenever sixty sibling tests happened to inflate the
baseline, and failed whenever they did not. The bound was measuring the neighbours.

*Cause and fix:* counting live emulator threads named it — **two of eight killed sessions left
their emulator thread asleep in `poll` with the pty master in its hand.** The channel disconnect was
not a reliable stop: a wake could be missed, and a clone of the handle kept the channel alive. The
session now sets a flag on its last drop as well as waking, and the thread checks that flag at the
top of every pass. `Emu::drop` also only sets it when the handle is the last one, because a
`Resize` clones it for the length of one request and a clone's drop must not end the session.

The test is un-ignored and is the proof: eight cycles, zero threads left, pty count back to its
baseline.

### 12.7f AR-15 — the branch was mid-cutover while the app moved before the host did (closed)

**Kept as written because the finding is the interesting part, not the fix.** For the two commits
between S5b-1b and S6b, S5b had moved the app onto the engine and version 15 while the daemon still
ran the ARQ byte-stream and the frame loop. A phone and a host on that tree spoke **different**
transports: the app sent `KIND_UP` transactions, the daemon expected ARQ-framed records with a page
byte. Neither end could parse the other.

Every test passes because both sides are tested against their own model. That is the same shape as
`AR-1` — a green tree proving two disjoint stacks — now with the extra hazard that the two are
supposed to be *one* stack.

*Disposition:* **closed by S6b** (`cc5884b`): the daemon drives a `RadioEndpoint`, the frame loop is
deleted, and a version-14 claim is told `Closed { reason: VERSION }`. The tailnet route was never
affected. What a reader should keep from this entry is the shape of the failure — two stacks, each
green, that had stopped talking to each other — because it is the one this whole plan exists to
prevent and it happened anyway, in the two commits between the app moving and the host moving.

### 12.7g AR-20 — the message boundary between the two engines had four silent drops and no test that crossed it

**Found 2026-11-13 while chasing "the host acknowledges the phone's `List` and never answers it".**
All four are now fixed (`RELEASE-AUDIT.md` 6.234–6.236); the finding is recorded because the shape
recurs and because the fix for each was small.

- **The host had a second push classifier.** `mesh_carrier::classify_push` read only the `0x88`
  receive log and only a bare `RAW_CUSTOM` payload; the phone read the same bytes through
  `ldmux_mesh::companion::push::classify`, which also takes a direct `0x84` arrival and a flooded
  `LDMR` record. Measured with the host radio hearing the public mesh: 28 pushes in 45 s, every one
  refused, `received 0 bytes`.
- **The shared classifier was too permissive in one arm.** It took **any** `PAYLOAD_TYPE_REQ`
  payload as a session record. `REQ` is MeshCore's own request type and the public mesh carries
  other operators' `REQ` floods continuously, including every `ldmux` `LDRQ`. On the phone that read
  `received=25 admitted=0` with every refusal counter at zero.
- **The link loop dropped four requests before the service saw them.** It resolves a channel slot
  first; `List`, `Create`, `Close` and `MakeLive` name no shell in a field and fell through the
  catch-all. `List` is the first request a phone makes, so the launcher never left "asking host for
  shells".
- **A lost list answer was permanent.** The phone holds one request until its answer arrives, by
  design; nothing cleared the flag when the answer never came.

**Why no test caught any of them.** Each component had tests, and every one of those tests entered
below the boundary that was broken. `MeshRpc` handles `List` correctly and is unit-tested doing it;
`mesh_full_stack` exercised `Attach` and `Fetch` and never a `List`; the push classifier was tested
on the phone's fixture and the host's copy was tested on its own. This is `AR-17`'s P4 ("a contract
with two owners has no cross-boundary test") in its third instance on this branch, and the cheap
answer is the one that took the longest to find: **the gate is a phone engine against the daemon's
own loop, and the assertion is on the message the operator was waiting for.**

*Disposition:* **fixed 2026-11-13**, three commits, each with a test that fails without it. The
open question that produced the search — whether a one-part message accepted at the record layer
ever becomes a message at the transaction layer — is answered: it does, and the loss was above it.

### 12.8 Findings index

| Id | Finding | Severity | Disposition |
|---|---|---|---|
| AR-1 | the new stack is inert while the old one runs | **risk, partly closed** | the version-15 upload and the settle engine are now on the live path (`mesh_rpc.rs` reached from `mesh_bridge.rs`); the transport swap that deletes `mesh_session.rs` and the `Exchange` ring moved to **S6** (§10a) and is blocked on S4/S5, because a transport-body change is a hard cut |
| AR-2 | codec accepted a body the transport cannot carry | **defect** | **fixed:** `crates/session/tests/payload_fits.rs` proves the ordering at the worst legal record sizes |
| AR-3 | a paste over ~550 bytes could not be sent | **defect (regression)** | **fixed:** `chunk_idx`/`chunk_count` on `T_SEND` and `T_SNAPSHOT`, with `SendAssembly`/`SnapshotAssembly` and a non-zero-chunk fixture case |
| AR-4 | gates ran non-production retry configuration | **gate weakness** | **fixed:** `Run::shipped` plus `a_link_comes_up_at_the_retry_ladder_that_ships` |
| AR-5 | version negotiation half-migrated | **closed** | `VERSION` is 15 on both ends in the same change that gave the host a receive path for it |
| AR-6 | route proof is one-sided | **by design** | document what the counter means |
| AR-7 | `sha2` in `ldmux-mesh` without an ADR line | **process** | **fixed:** ADR-0039 gained a Dependencies section naming the crate and why the simulator must hash as the firmware does |
| AR-8 | the client's command table is an input, not a derivation | **doc** | **fixed:** §8.1 says each side supplies its own answer table, so the S4 client engine gets the phone's rather than inheriting the host's |
| AR-9 | a version claim stopped the frame loop for an app that cannot yet ask for its screen | **defect (would brick the app)** | **fixed:** the host keeps streaming until the phone has actually sent a `Send` or a `Fetch` |
| AR-10 | a flood arrival is a `REQ` carrying `LDMR`, not a `RAW_CUSTOM`; a payload-type classifier drops every flooded record. **Corrected 2026-11-12:** records arrive as `0x84` (direct) or through the `0x88` receive log (flood), never as `0x8E`. **Corrected 2026-11-13:** the `0x88` push is emitted for every demodulated packet with no preference gate (`src/Dispatcher.cpp:199`, `MyMesh.cpp:291-297`), so a flood *is* delivered and the flood carrier is a real fallback | **defect (silent loss)** | **fixed:** all three push codes are classified with their own layouts (`crates/mesh/src/companion/push.rs`); advert-learned paths remain the way past one hop |
| AR-20 | the message boundary between the two engines dropped `List`, `Create`, `Close` and `MakeLive`, the host had a second push classifier, and a lost list answer was permanent | **defect (silent loss)** | **fixed:** one classifier for both ends, the four requests reach the service, and the transaction's own end frees the next ask; each pinned by a test that fails without it (`RELEASE-AUDIT.md` 6.234–6.236, §12.7g) |
| AR-11 | the transport swap cannot precede the phone, and has no `Link` to preserve | **plan correction** | **fixed in the plan:** S6, after S4 and S5 |
| AR-12 | the client engine lacked six shipped features, and `open` sent a zero grid (a fixed defect reintroduced) | **defect (silent regression)** | **fixed:** the six are implemented and tested; capability surveys are now part of a stage's definition of done |
| AR-13 | the BLE pipe still owned radio management, so `AndroidRadioLink` could not die and one GATT link would have had two owners | **architecture** | **S5b-0:** radio management moves into the engine, through the same command FIFO, with the lease on the pipe |
| AR-14 | a killed keeper session could keep its pty master (2 of 8, permanent) | **defect (leak)** | **fixed:** the emulator thread now stops on a flag set by the session's last drop, not on a channel disconnect that could be missed |
| AR-15 | the branch was mid-cutover: the app spoke the transaction transport while the daemon spoke ARQ | **cutover hazard** | **fixed by S6b:** the daemon drives the endpoint, the frame loop is deleted, a v14 claim is told `Closed { VERSION }` |
| AR-11 | the transport swap cannot precede the phone, and has no `Link` to preserve | **plan correction** | **fixed in the plan:** S6 (§10a) is one stage — transport and bridge together, after S4/S5 — because `Link`'s page byte is a frame-loop fact that every v15 message already carries as a field |

### 10a. S6 scope: the host transport and the bridge are one stage

**Why one stage and not two.** `Link` (`mesh_bridge.rs:40-110`) exists to hide the *v14* session
from the frame loop, and its shape is a frame-loop fact: `take_inbound() -> Vec<(u8, Vec<u8>)>`
carries a per-page byte (`self.decoders.entry(shell)`, `:1046`) and `handle(shell, m)` uses that
byte as the page index for `Attach`/`List`/`Resync`/`Detach`/`Hold` (`attach_with(shell, …)`,
`:1776`). Every version-15 message already names its shell *inside* itself and `MeshRpc` already
demultiplexes on it (`SendRequest { shell, … }` is built from the message), so once the transport is
replaced there is no consumer left for the page byte. Swapping the transport under an unchanged
bridge is therefore not a coherent stage; the two go together.

**Order.** After S4 and S5. A transport-body change is a hard cut — the old body is an ARQ
byte-stream, the new one is a transaction — so a host that swaps first cannot serve any deployed
phone. The daemon and the app ship from one tree, so a flag day is acceptable; a silent one is not.

**Retained from `mesh_bridge.rs`** (moves to the service, keeps its tests):

- ownership and claims, including takeover and the `HELD` refusal;
- `list` / `create` / `close` / `make_live`, with their version gates collapsing to one version;
- marks (`MARKS_VERSION`), history paging, and send-to-desktop;
- the five `MeshHost` operations the service calls (`screen`, `input`, `keys`, `resize`,
  `listing`) and the at-most-once `(device, op_id)` ring.

**Deleted with the transport**, with measured sizes:

| Deleted | Lines | Why nothing else needs it |
|---|---:|---|
| the frame loop in `mesh_bridge.rs` — `Differ`, `Attached::hold`, `owe_frame`, `resync_wanted`, `flushing`, the 50 ms tick, and the `Profile`/`Rows`/`Commit` builders (28 message arms) | ~4,500 of 6,794 | a `Snapshot` is the whole screen; a diff against a `snapshot_id` replaces `Differ`, and there is no hold because the host never streams |
| `crates/daemon/src/mesh_session.rs` (`SessionHost`; the identity files already moved to `mesh_identity.rs`) | 707 | `RadioEndpoint` owns the handshake, replay window and outbound queue |
| `Exchange`, `INBOUND_MAX`, the silence watchdog, the retry ladder, `awaiting`, the budget reservation in `crates/daemon/src/mesh.rs::session()` | ~1,000 of 1,870 | the endpoint's transaction layer owns retransmission, acking and deadlines; the loop becomes `poll` → `send` → `take_inbound` → `next_deadline_ms` |
| `crates/daemon/src/mesh_serial.rs`'s route policy — `route_hint`, `route_reply`, `send_route_reply`, `prefer_route`, `first_time_question`, `refresh_peer_route`, `LDRQ`/`LDRR`, the flood ladder, `REPLY_*` pacing | ~2,300 of 3,565 | `RouteTable` + `carrier_for` make the decision once, per attempt, before sealing; the carrier keeps only `open`/`prepare`, the command thread, the stats poll and the budget |
| `crates/session/src/session.rs` and `crates/session/src/arq.rs`, if nothing but their own tests still uses them | 295 + 1,068 | `transaction.rs` replaces both; `session-ffi` moves to the endpoint in S4 |

Net: about **9,800 lines deleted**, against the ~7,900 the new core adds — the first change in this
work that takes the tree down rather than up.

**The wire consequence.** On the radio route these stop existing: `Hold`, `Resync`, `Profile`,
`Rows`, `Commit`, `Sessions`' paging loop (a `List` answer becomes one `Snapshot`-style message),
`Input`, `Key`, and the `Attach` grid fields (a `Fetch`/`Send` carries the grid). `v15` remains the
only version. A v14 phone that attaches gets **one `Closed { reason: VERSION }`** and is not left
waiting: the attach path already answers `closed::VERSION` for a version it cannot serve
(`mesh_bridge.rs:1534-1538`), and S6 must keep that by refusing the *transport* for a v14 claim
rather than by half-serving it. The app, in turn, must refuse to start on the radio route against a
host that did not answer v15 — an explicit sentence, not a spinner.

**The gate (`B11`).** Multi-shell survives *by message field*, and the test has to prove that rather
than assume it:

1. **Three pages over one session, by message field.** One `VirtualMesh` link, one endpoint, a phone
   engine driving three shells. Each `Send` names its shell id; assert that the three answers carry
   the three different `shell_id`s and that no message carries a page byte (a fixture assertion over
   the encoded bytes: the transaction payload's first byte is the message tag, not an index).
2. **Interleaving.** Two pages' commands sent in flight at once resolve to their own shells: the
   `(device, op_id)` ring and the settle engine keep them apart, and neither answer names the other's
   id.
3. **A v14 claim gets one `Closed { VERSION }`.** Exactly one message, no rows, no hang, and the
   session is not torn down while another page is live.
4. **Nothing streams.** After a `Send` and its `Snapshot`, twelve seconds of a printing shell produce
   zero unsolicited records (`B5`'s rule, re-asserted through the new transport).
5. **The page byte is gone from the wire.** A test that decodes every record of the run and fails if
   any transaction payload is not a well-formed `mesh_term` message — the check that would have
   caught the deleted `Link` contract surviving by accident.
