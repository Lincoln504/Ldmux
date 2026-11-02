# LDMUX standalone: the phone owns the radio

Status: design of record for `feat/standalone-radio`, 2026-11-02.

This document records the measured facts, the protocol boundary, the migration, and the
open issues that are deliberately **not** this branch. It replaces the "keep MeshMore as
the radio owner" recommendation in `docs/MESH-RADIO-OWNERSHIP.md` for the phone; it does
**not** replace the host's radio gateway.

## 1. Decision

- **The Android app owns its BLE radio.** It talks to the MeshCore companion over GATT,
  using the Java MeshCore library as the base.
- **The phone MeshMore gateway, token, Binder handoff and loopback socket are removed
  from LDMUX's mesh path.** MeshMore may keep its own gateway for other clients; LDMUX
  no longer calls it.
- **The host side is unchanged.** `ldmuxd` still talks to the host radio through
  MeshMore's `meshcore_host` gateway (`bin/meshmore_gateway.dart`). A phone-only
  standalone does not remove that process; a "no MeshMore anywhere" product needs a
  separate host bridge.
- **The L3 session is unchanged.** The existing Noise session, ARQ, terminal profile,
  row codec and driver stay. The only transport replaced is the L2 carrier.
- **The only pairing is the host's `ldmux code --mesh`.** It carries the host session key,
  the PSK, and the host radio public key. The gateway token is gone from the phone.
- **Encryption stays inside the raw record.** MeshCore's raw-custom payload does not
  encrypt; LDMUX's Noise/AEAD is the confidentiality and integrity boundary. A MeshCore
  channel PSK is neither needed nor used for the terminal.

## 2. What the host and the phone each own

```
Phone:  LDMUX
          ├─ BLE scanner / permissions / radio selection
          ├─ MeshCore companion session (APP_START, SELF_INFO, contacts, config)
          ├─ raw carrier (CMD_SEND_RAW_DATA / PUSH_CODE_RAW_DATA)
          ├─ L3 Noise session + ARQ + terminal profile
          └─ terminal screen, keys, launcher

Host:   Heltec radio ─ USB ─ meshmore_gateway (serial companion + L2 gateway)
          └─ ldmuxd / keeper ─ PTY, terminal state, shells
```

- The phone's radio and the host's radio must be on the same preset. The phone sets its
  own; the host has its own provisioning command (`meshcore_host/tool/configure_radio.dart`
  today, or a future LDMUX host bridge). One radio cannot be on two presets at once.
- The host radio gateway still binds the phone as its peer. `PUSH_CODE_RAW_DATA` carries
  no sender identity, so the host's single-peer bind remains the addressing fact.

## 3. The MeshCore wire facts

### 3.1 OTA packet

`[header 1][transport codes 4?][path_len 1][path 0..64][payload 1..184]`

- Header bits: route in bits 0..1, payload type in bits 2..5, version in bits 6..7.
- Transport codes exist only for `TRANSPORT_FLOOD` / `TRANSPORT_DIRECT`.
- `path_len` low 6 bits are hop count; high 2 bits are `hash_size - 1`.
- `MAX_PACKET_PAYLOAD = 184`; `MAX_PATH_SIZE = 64`; `MAX_TRANS_UNIT = 255`.
- Payload type `0x0F` is `PAYLOAD_TYPE_RAW_CUSTOM`: **opaque bytes, no firmware format,
  no MeshCore encryption**.

Evidence: `docs/packet_format.md` and `docs/payloads.md` in MeshCore; the local firmware
`examples/companion_radio/MyMesh.cpp` and `src/Mesh.cpp`; the Java library's
`OtaPacket.java` documents the same layout.

### 3.2 Companion raw commands

- **Send:** `CMD_SEND_RAW_DATA` = `0x19`, body `[path_len int8][path bytes][payload]`.
  - `path_len >= 0`: firmware calls `createRawData` and `sendDirect(pkt, path, path_len)`.
  - `path_len < 0`: firmware returns `ERR_CODE_UNSUPPORTED_CMD` with
    `// flood, not supported (yet)`.
  - There is no flood fallback for raw custom.
- **Receive:** `PUSH_CODE_RAW_DATA` = `0x84`, body
  `[snr int8, quarter-dB][rssi int8][0xFF][payload]`.
  - It carries no sender identity; the L3 Noise session authenticates the sender.
- **Firmware receive filter:** raw custom is accepted only on a direct route
  (`pkt->isRouteDirect()`).

Evidence: `MyMesh.cpp:1519-1534` (send), `MyMesh.cpp:807-813` (receive),
`Mesh.cpp:292-299` (direct-only receive), `Mesh.cpp:589-604` (raw copy, no cipher).

### 3.3 Size budget

| Fact | Value | Source |
|---|---:|---|
| OTA payload | 184 B | `MeshCore.h:20` |
| OTA path | 64 B | `MeshCore.h:22` |
| Companion frame | 176 B | Dart `mesh_carry.dart:33` |
| Raw payload | `174 - pathLen` | Dart `mesh_carry.dart:60` |
| LDMUX record ceiling | 172 B | `MeshGatewayFrame.kt:32` |
| LDMUX terminal payload | 143 B | `MeshTerm.kt` / protocol |
| Existing record floor | 160 B | `MeshCarryLimits` |

The terminal's 143-byte payload fits every path up to 31 bytes at the 174-byte raw
ceiling. The carrier must refuse a record rather than truncate it.

### 3.4 Encryption placement

- MeshCore encrypts its **defined** payloads (channel text, direct text, requests,
  responses) with AES-128 and a 2-byte truncated HMAC.
- Raw custom is explicitly "for applications with custom encryption"; firmware copies
  the bytes and does not add a cipher.
- **LDMUX's Noise `IKpsk2_25519_ChaChaPoly_SHA256` stays inside the record.**
  - It gives end-to-end confidentiality and integrity across every hop.
  - It authenticates the host and phone independently of the radio's channel state.
  - It costs a 16-byte tag and the record header; that is already in the 143-byte budget.
- **Do not wrap raw in a MeshCore channel PSK.** It would double-encrypt, shrink the
  payload, require a channel slot, and add no authentication the L3 session lacks.
- Metadata is visible: OTA header, path, and the fact that a raw packet is flowing. If
  metadata privacy is required, raw custom is the wrong carrier; a MeshCore direct
  encrypted packet has its own limits and cannot carry the arbitrary terminal stream.

## 4. The Java library: usable base, not a tunnel

The library is `libmeshcore` + `libmeshcore-android` (MIT, `io.iotone.meshcore`).

It provides:

- BLE transport (`AndroidBleTransport`, Nordic BLE 2.10.1, MTU 256, one frame per write).
- Companion session (`MeshcoreSession`: `APP_START`, `SELF_INFO`, queue drain).
- Codec (`MeshcoreFrameCodec`), contacts, channels, text, adverts, telemetry.
- Radio configuration (`setRadioParams`, `setTxPower`, `setChannel`).
- Crypto for defined payloads (channel/DM), not raw.

It does not provide, and this branch adds:

1. `CMD_SEND_RAW_DATA` (0x19) and a public send.
2. `PUSH_CODE_RAW_DATA` (0x84) typing and a callback.
3. A send-result contract (`OK`/`ERR`), because `MeshcoreSession.send` is private and
   swallows transport errors.
4. Contact/path lookup, `CMD_RESET_PATH` (13), `CMD_SEND_PATH_DISCOVERY_REQ` (52),
   `PUSH_CODE_PATH_DISCOVERY_RESPONSE` (0x8D), and path updates (0x81).
   The current bench can use a zero-hop direct path; multi-hop needs these.
5. BLE scanning and runtime permissions; the library explicitly leaves scanning to the
   caller.
6. A handshake timeout and reconnect policy; `MeshcoreSession` waits indefinitely for
   `SELF_INFO` and has no reconnect.

Library constraints:

- Java 17 records/sealed types; LDMUX currently compiles JVM 11. This branch moves the
  Android module to Java/Kotlin 17 (AGP desugaring) or vendors the library as a 17
  module.
- Pinned to firmware `companion-v1.15.0`; deployed firmware is 1.17.1. Raw and path
  commands exist in both; verify on hardware.
- The library's test suite has one stale advert failure, not a raw/runtime regression.

## 5. The transport adapter

`MeshRadioGateway` implements the existing `GatewayPort`:

| Member | Standalone implementation |
|---|---|
| `state` | `Idle`, `Connecting`, `Bound`, `RadioUnavailable`, `NeedsRadio`, `NeedsPermission`, `NoPath`, `Closed` |
| `status` | `MeshStatus(radioState = 0, rssi from 0x84, battery null, airtime 0, opsLeftThisHour = null)` |
| `received` | `0x84` payloads, in order, in an unbounded channel |
| `send(record)` | `0x19` with the host's path; OK → `SentDirect`, ERR → the matching failure; timeout → `LocalTimeout` |
| `uplinkRecordMax` | 172 (conservative), or `174 - pathLen` when smaller |
| `close()` | close the session and transport, idempotent |

- Never map `SentDirect` to delivery. The session's ARQ owns delivery.
- Never report `opsLeftThisHour = 0`; unknown is null, which the driver treats as
  `UNKNOWN_BUDGET` (255), not as a stall.
- A missing path is `NoPath`, not `Refused`; it must not tell the user to re-pair.
- The host contact and path are learned from `requestContacts`; the current bench may
  use a zero-hop direct path. Path reset after repeated unanswered sends is a follow-up
  unless the hardware proves it necessary now.

## 6. Radio preset

- The phone can change only the radio it is attached to:
  `CMD_SET_RADIO_PARAMS` (0x0B: frequency, bandwidth, SF, CR) and
  `CMD_SET_RADIO_TX_POWER` (0x0C).
- The host radio is configured separately. The host tool today is
  `meshcore_host/tool/configure_radio.dart`; the host gateway must be restarted if the
  preset changes while it is running.
- The LDMUX private preset used on this bench is **910.525 MHz / 500 kHz / SF7 / CR5 /
  22 dBm**. The public USA/Canada preset is **910.525 / 62.5 / SF7 / CR5 / 22 dBm**.
  They are different presets and cannot be active at once.
- `CMD_SET_CHANNEL` is a channel slot (name + 16-byte PSK), not a radio preset. The raw
  terminal does not use a channel slot.
- **Quick preset:** a radio setup screen in LDMUX reads `SELF_INFO`, shows the current
  frequency/bandwidth/SF/CR/TX, and writes the LDMUX preset with one action. It then
  reconnects and re-reads `SELF_INFO` because the write itself only returns OK.
- The pairing code / setup screen must state the host's preset. A mismatch is the first
  thing to check when the terminal finds no path.

## 7. Migration

### Remove from LDMUX's mesh path

- `MeshGatewayClient`, `MeshGatewayFrame`, `MeshGatewayCode`, `MeshSocket`,
  `JavaMeshSocket`, `SocketFactory.android.kt`.
- `MeshTokenStore`, `KeystoreMeshTokenStore`, `meshTokenStore()`, the token argument to
  `MeshEnrollment`.
- `MeshHandoff.kt`, `MeshHandoff.android.kt`, `MeshHandoffUi`, the Binder service
  permission, `openMeshMore`, and the automatic/foreground handoff effects.
- `MeshCredential.GatewayToken`, `MeshRouteProblem.NeedsToken`,
  `MeshLinkState.NeedsToken`.
- `LdmuxConfig.meshGateway` / `meshGatewayEndpoint` and the endpoint in
  `MeshScreenPlan`.
- Gateway-specific strings and tests.

### Add

- Vendored `libmeshcore` and `libmeshcore-android` modules, with raw support and the
  local modifications documented.
- `MeshRadioGateway`, `MeshRadioScanner`, `MeshRadioPreset`, Android permissions.
- A radio selection/preset screen, and a stable `MeshScreenPlan` identity based on the
  radio address rather than the whole display config.
- `LdmuxConfig.meshRadio` (address, not a secret) and a mesh connection key.

### Keep

- `MeshTermDriver`, `MeshSession`, `MeshTerm`, `MeshTerminalSource`, the terminal screen,
  the key bar, the launcher, `MeshSessionKeyStore`, the host pairing ceremony.
- The host gateway and daemon. A phone-only standalone does not change them.

## 8. Tests and timing

The rule is **one fail-fast loop**: fake transport for logic; the emulator/device only
for the facts a fake cannot prove (permissions, GATT, RSSI, path).

| Layer | Test | Timing target |
|---|---|---|
| Codec | `0x19` body, `0x84` parse, size bounds | < 1 s |
| Session | OK/ERR correlation, timeout, no-op before READY | < 2 s |
| Adapter | state mapping, unknown budget, no path, `ErrTooLong` | < 2 s |
| Driver | existing `MeshTermDriverTest` with a fake radio | existing suite |
| Route | no token, radio missing, permission denied | < 2 s |
| Hardware | connect, preset, zero-hop raw round trip, attach | measured once, not repeated |

Do not rerun the 665-test Android suite after every edit. Run the affected test class
during development; run the full suite once before the commit; record wall time.

## 9. Open issues outside this branch

### Desktop shell fit and history

Measured live on 2026-11-02:

- The current `ldmux-live-*` windows map to tty sizes of 158x40, 158x19, 158x82 and
  318x40; the corresponding window geometries are 2552x1404, 2552x698, 2552x2816 and
  5112x1404. Those widths agree with the tty columns. The supplied screenshot is
  1782x1696, smaller than any of those windows: it is a **crop**, not proof of a live
  grid/window mismatch.
- `ldmux-terminal` calls `ldmux window`, which creates a **new shell** and a new attach
  window. That is normal separate-shell behavior; it is not shell history reuse.
- The desk attach client is a screen mirror. `render_snapshot` clears the visible screen
  (`\x1b[2J\x1b[H`) and repaints full rows on every snapshot. It never requests a
  history page. The terminal emulator's scrollback therefore contains repeated
  snapshots, not the shell's scrollback; the keeper's history is not shown. That is the
  concrete "history is not working" defect.
- The remaining multi-client pane issue is real: the holder wins and a read-only mirror
  cannot resize; a mirror can paint the holder's phone-sized grid. The existing
  re-assert (`crates/attach/src/lib.rs:242-268`) fixes the holder, not the mirror.
- A "plain bash window with no layer" is not the current architecture: the keeper owns
  the PTY and the window is an attach client. A true plain terminal requires a sidecar
  or a local-shell mode and would lose remote attach. This needs a separate decision.

The desktop fix belongs in a separate branch:
1. Request a bounded history page once per attach and write it into scrollback before the
   live screen.
2. Do not clear the visible screen on every snapshot; use the snapshot diff or a
   dedicated renderer so scrollback is not duplicated.
3. Decide the multi-client viewport rule (holder, last writer, or per-client viewport).
4. Persist/bless window geometry; the daemon currently passes no `--geometry`.

### Branch comparison

- `main` is the merge-base `c9c7643`; `feat/meshmore-gateway` is 218 files / 34,522
  insertions ahead. `port/rust` has no commits ahead of the current branch.
- The current branch is the last working branch. `main` is not a newer development
  branch; it simply lacks the mesh work. There is no evidence of a regression from
  `main` to this branch, but `docs/STATE.md` contains stale "not implemented" statements
  that must not be used as the comparison baseline. Use the latest dated sections of
  `docs/HARDWARE-VALIDATION.md` and `docs/MESH-GATEWAY.md`.

## 10. Non-goals

- Replacing the host `meshcore_host` gateway in this branch.
- Simultaneous MeshMore chat and LDMUX terminal on one BLE radio (one central, one
  connection).
- Multi-hop path discovery/reset unless the bench proves it necessary; zero-hop direct
  first, with the missing path work tracked.
- MeshCore channel PSK or a second encryption layer.
- A full desktop-terminal rewrite; the desktop history/fit fix is separate.
