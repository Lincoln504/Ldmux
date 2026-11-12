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
  [0x0E][op_id: u32 LE][have_snapshot_id: u32 LE][cols: u8][rows: u8][flags: u8][shell_len: u8 | shell_id][items_bytes...]
  ```
  - `op_id`: monotonic per-app-run operation ID (`1..=u32::MAX`).
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
- **Ordered PTY handoff**:
  - `Text(bytes)`: write via `host.input(&session, &bytes)`.
  - `Paste(bytes)`: read the pane's live `bracketed_paste` mode from the keeper right before writing; if `true`, strip `ESC (0x1b)` and wrap the entire paste in `\x1b[200~ … \x1b[201~`; if `false`, convert `\n` to `\r` and write in one call.
  - `Keys(names)`: encode for the pane's live cursor-key and Kitty keyboard flags via `host.keys(&session, &names)`.

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
  [0x8E][flags: u8][snapshot_id: u32 LE][base_id: u32 LE][applied_op_id: u32 LE][applied_status: u8]
  [cols: u8][rows: u8][cursor_x: u8][cursor_y: u8][first_line: u32 LE][shell_len: u8 | shell_id]
  [body_bytes (raw-deflate compressed with FRAME_DICTIONARY when bit 4 is set)...]
  ```
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
  - `crates/companion` (`ldmux-companion`): thin Linux host USB serial driver (`termios`, `flock`, non-blocking `not_before: Instant` scheduler) built on `ldmux_mesh::CompanionFifo`.
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
| **B8: Full-stack `VirtualMesh` end-to-end gate** | Real Rust Phone Engine ↔ `VirtualMesh` (14 hops, 2B hashes, `logRxRaw` 173 B filter, `SimpleMeshTables` dedup, repeater half-duplex collisions, 20% packet loss) ↔ Real Rust Host `MeshRpcService` + `TestKeeper`: connect, open shell, run fast command, run multiline paste, run `tail -f` + `Refresh` + `C-c`, switch shells, switch Tailnet ↔ Radio ownership, survive 2-minute RF blackout with zero re-handshake, and survive daemon restart via `KIND_NEED_HANDSHAKE`. |

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

---

## 11. Implementation status (2026-11-11)

What has landed, what the tests prove, and what is still open. Every claim here is a file in the
tree or a command that was run; nothing is a plan.

| Stage | Status | What exists |
|---|---|---|
| **S0** | **Done** | `ldmux-protocol` has a `host-ipc` feature (default on) owning `tokio` + `nix`; `cargo tree -p ldmux-protocol --no-default-features` is `serde`, `serde_json`, `uuid`, `flate2` and nothing else. `crates/mesh/src/sim/` is the `VirtualMesh` harness (173-byte receive ceiling, `SHA256(payload_type‖payload)` 160-entry dedup, the origin's own first-hop echo, half-duplex deafness, per-hop forward delay, path accumulation and consumption, flood hop caps, scoping) with `airtime.rs` beside it, pinned against the companion's own time-on-air by `crates/companion/tests/airtime_agreement.rs`. `native-freshness.test.ts` now checks both `libldmux_terminal.so` and `libldmux_session.so` for staleness and for their `Java_io_ldmux_*` symbols (6/6). |
| **S1** | **Done** | `crates/mesh/src/budget.rs` (`172 − path` direct, `161 − path − scope` flood, 93 guaranteed and 125 near, all derived and compile-time pinned). `crates/mesh/src/route.rs` (`Candidate`, `ProofWitness` as the only route to `Proven`, SNR-margin ranking, `carrier_for`). `crates/session/src/direction.rs` (the six kinds, one definition, re-exported by `handshake.rs` and `record.rs`). `crates/mesh/src/companion/fifo.rs` plus the host's answer contract in `crates/companion/src/window.rs`: one command in flight, settled only by its own answer, a foreign answer reported rather than consumed. |
| **S2** | **Done** | `crates/session/src/bootstrap.rs`: `Initiator` holds one ephemeral across every rung, `msg1` direct 123 B or two fragments 67/66 B with a per-transmit `tx_nonce`, `msg2` 52 B, `KIND_NEED_HANDSHAKE` 25 B, `Responder` with a 64-entry ±5 min cache that re-emits its cached answer without rebuilding the session, and a 5 s answer rate limit. `crates/session/src/transaction.rs`: 10-byte header, parts of 118 B so any part halves onto the narrow carrier, selective `ack_mask` retransmission, ack suppression to the burst tail, clean supersession, and a timeout that leaves the session intact. `crates/session/src/endpoint.rs`: one state machine for both ends — role, bootstrap, per-attempt carriage, post-authentication route updates, transaction dispatch, deadlines and a `Stats` that names every rejection. `crates/session/tests/endpoint.rs` runs two real endpoints over one `VirtualMesh`; the six-repeater, 15 %-loss gate delivers a request and its answer with the route proven by a direct arrival. |
| **S3** | **Not started** | Entry points, in order: `crates/protocol/src/mesh_term.rs` (bump `VERSION` to 15, add `T_SEND`/`T_FETCH`/`T_SNAPSHOT` with the `(device_id, op_id)` ring), then `crates/daemon/src/mesh_bridge.rs` replaced by a single-task `MeshRpcService` that drives `RadioEndpoint` and settles through `PaneStream`, then delete `mesh_session.rs`, `mesh_serial.rs`'s loop and the `Exchange` ring. |
| **S4** | **Not started** | `crates/session-ffi` gains the shared client engine (companion FIFO, `RadioEndpoint`, `mesh_term` client) behind the pull-only C ABI in §8.1, so the phone stops reimplementing the protocol in Kotlin. |
| **S5** | **Partly done** | Landed: the claim wedge (`CLAIM_RELEASE_MS` gone, `resumeShell` assigned by one rule, `MeshResumeTest`), the blank/clipped screen (grid sent only after the IME gate, cursor-aware `GridPlacement`), multiline paste as one paste operation on both routes, the provisional draft drawn with `⏎`/`⇥` and windowed by display width (`term/DisplayWidth.kt`), and the flood token bucket now actually metered (`sessionUp(true)` is called). 725 JVM tests pass. Open: the unified pager and per-shell state across `AppContent` and `MeshTerminalScreen`, the `Send`/`Refresh` controls, dictation into the draft, batch coalescing (which needs v15 — separately sending `Hold(false)`, `Input` and `Key` cannot be fixed inside the old protocol), and deleting `vendor/libmeshcore`. |

### Corrections the implementation forced on this document

Each was found by building the thing and is now reflected in the sections above:

- **`KIND_MSG1_FRAG` carries its own `tx_nonce`** (67/66 B, not 65/64): a fragment is used on direct paths too, where nothing else varies, so a byte-identical retry would be dropped by `wasSeen`.
- **`msg2` carries the session index sealed inside the Noise message** (`MSG2_LEN` is 50: `[kind][noise 49]`), not as a clear byte; the total with the nonce is still 52.
- **The session wrapper is 24 bytes**, not 22 (`6 clear + 2 inner + 16 tag`), so a 171-byte record carries 137 payload bytes, and parts are cut to 118.
- **Two defects the end-to-end gate found in the new code, both fixed**: an acknowledgement piggybacked on a data record was decoded and then dropped (`transaction.rs` applied it only in the pure-ack branch), so every delivered message still timed out; and the endpoint learned a route only from handshake traffic, so a candidate expired 120 s after the bootstrap and direct was never used again.
