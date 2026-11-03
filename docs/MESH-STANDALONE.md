# LDMUX standalone: the phone owns the radio

Status: design of record for `feat/standalone-radio`, 2026-11-02.
Supersedes the phone-side recommendation in `docs/MESH-RADIO-OWNERSHIP.md`.
The host side is unchanged.

## 1. Decision

- **The Android app owns its BLE radio.** It talks to the MeshCore companion over
  GATT, using the vendored Java MeshCore library as the base.
- **The phone MeshMore gateway, token, Binder handoff and loopback socket are
  gone from LDMUX's mesh path.** MeshMore may keep its own opt-in gateway for
  another client. LDMUX no longer calls it.
- **The host side is unchanged.** `ldmuxd` still reaches the host radio through
  MeshMore's `meshcore_host` gateway (`meshmore_gateway.dart`). A phone-only
  standalone does not remove that process.
- **The L3 session is unchanged.** Noise `IKpsk2_25519_ChaChaPoly_SHA256`, ARQ,
  terminal profile, row codec and driver stay. The L2 carrier is replaced.
- **The only pairing is the host's `ldmux code --mesh`.** It carries the host
  session key, the PSK and the host radio public key. The gateway token is gone.
- **Encryption stays inside the raw record.** MeshCore's raw-custom payload has
  no firmware cipher. LDMUX's Noise session is the confidentiality and integrity
  boundary. A channel PSK is neither needed nor used.

## 2. Ownership

```
Phone:  LDMUX
          ├─ BLE scanner / permissions / bond
          ├─ MeshCore companion session (APP_START, SELF_INFO, contacts, config)
          ├─ raw carrier (CMD_SEND_RAW_DATA / CMD_SEND_RAW_PACKET / 0x84)
          ├─ L3 Noise session + ARQ + terminal profile
          └─ terminal screen, keys, launcher

Host:   Heltec radio ─ USB ─ meshmore_gateway (serial companion + L2 gateway)
          └─ ldmuxd / keeper ─ PTY, terminal state, shells
```

The phone radio and the host radio must be on the same preset. The phone sets
its own. The host has its own provisioning command
(`meshcore_host/tool/configure_radio.dart` today, or a future LDMUX host bridge).
One BLE central means LDMUX and MeshMore cannot both hold the same companion
session at once.

## 3. Wire facts

### 3.1 OTA packet

```
[header 1][transport_codes 4?][path_len 1][path 0..64][payload 1..184]
```

- Header bits: route 0..1, payload type 2..5, version 6..7.
- `path_len` is **encoded**, not a byte count: low 6 bits are hop count, high
  2 bits are `hash_size - 1`. `0xFF` means path unknown.
- Effective path bytes are `hop_count * hash_size`, not `path_len`.
- `MAX_PACKET_PAYLOAD = 184`, `MAX_PATH_SIZE = 64`, `MAX_TRANS_UNIT = 255`.
- Raw-custom is payload type `0x0F`: opaque bytes, no firmware format, no
  firmware encryption.

Evidence: MeshCore `docs/packet_format.md` and `docs/payloads.md`;
`src/Packet.h` / `src/Packet.cpp`; local firmware `MyMesh.cpp`;
`vendor/libmeshcore/.../model/OtaPacket.java`.

### 3.2 Send: `CMD_SEND_RAW_DATA` 0x19

```
19 [path_len int8] [path bytes] [payload]
```

- `path_len` is the encoded value above. It is not the path byte count.
- Firmware requires `path_len >= 0`, at least four payload bytes, and checks
  `i + path_len + 4 <= frame length`.
- That firmware check treats `path_len` as a byte count while `sendDirect()`
  interprets it as encoded. For one-byte hash paths the two agree. For two- and
  three-byte hash paths they do not.
- **LDMUX therefore uses 0x19 only when `path_len <= 0x3F`.** That covers
  zero-hop and all one-byte-hash paths.
- Raw custom is direct-only. A negative path length is rejected with
  `ERR_CODE_UNSUPPORTED_CMD` ("flood, not supported (yet)"). There is no flood
  fallback.
- Payload ceiling is `174 - pathBytes` (`176 - opcode - path_len field - path`).

### 3.3 Send: `CMD_SEND_RAW_PACKET` 0x41

```
41 [priority] [complete MeshCore packet]
```

- The complete packet is `[header][path_len][path][payload]`.
- LDMUX uses a direct raw-custom header `0x3E` and the encoded path length.
- This is the carrier for two- and three-byte hash paths, and for any path whose
  encoded value is above `0x3F`.
- Payload ceiling is `172 - pathBytes` (`176 - opcode - priority - packet header
  - path_len field - path`).
- The firmware parses the whole packet and returns `ERR_CODE_ILLEGAL_ARG` on a
  malformed packet.

### 3.4 Receive: `PUSH_CODE_RAW_DATA` 0x84

```
84 [SNR int8 quarter-dB] [RSSI int8] [0xFF] [payload]
```

- It carries no sender identity and no packet header or path.
- The `0xFF` marker is mandatory. LDMUX rejects a frame without it as
  `MALFORMED`.
- The L3 Noise session authenticates the sender.

### 3.5 Size budget

| Fact | Value | Source |
|---|---:|---|
| OTA payload | 184 B | `MeshCore.h:20` |
| OTA path | 64 B | `MeshCore.h:22` |
| Companion frame | 176 B | firmware `MAX_FRAME_SIZE` |
| 0x19 raw payload | `174 - pathBytes` | firmware + codec |
| 0x41 raw payload | `172 - pathBytes` | firmware + codec |
| LDMUX record | 172 B | `MeshTerm` / protocol |
| LDMUX terminal payload | 143 B | `MeshTerm.kt` |
| LDMUX record floor | 160 B | `MeshCarryLimits` / gateway |

The full 172-byte record fits a zero-hop 0x19 send with two bytes to spare. A
160-byte record fits a 14-byte path on 0x19. A path that leaves less than the
160-byte floor is `NoPath`, not a truncated record.

### 3.6 Encryption

- MeshCore encrypts its **defined** payloads (channel text, direct text,
  requests, responses) with AES-128 and a 2-byte truncated HMAC.
- Raw custom is explicitly for applications with custom encryption. Firmware
  copies the bytes and adds no cipher.
- **LDMUX Noise stays inside the record.** It gives end-to-end confidentiality
  and integrity across every hop, independent of the radio's channel state. It
  costs the Noise header/tag and the ARQ header already in the 172-byte budget.
- **Do not wrap raw in a channel PSK.** It would double-encrypt, shrink the
  payload, require a channel slot, and add no authentication the L3 session
  lacks.
- Metadata is visible: OTA header, path, and the fact that a raw packet is
  flowing. That is inherent to raw custom.

## 4. Path policy

- A contact's `out_path_len` is decoded by `Contact.hasValidPathEncoding()`,
  `pathHashSize()`, `pathHopCount()` and `activePath()`. `activePath()` returns
  exactly `hops * hashSize` bytes, or empty for invalid/unknown.
- A known contact path is used as-is.
- The phone looks the host up by full public key with
  `CMD_GET_CONTACT_BY_KEY` 0x1E, not with a full contact dump. The reply is one
  contact frame or `ERR_CODE_NOT_FOUND`. This keeps the one answer window free
  and does not depend on the size of the radio's contact table.
- If the host radio is not in the phone's contact list, LDMUX sends a zero-hop
  direct packet. This is the bootstrap route for two radios in the same room
  and the first hop of contact discovery. It is not a multi-hop path and is
  never presented as one.
- If a known host contact has `0xFF`, an invalid hash-size code, a byte count
  that disagrees with the encoding, or leaves less than the 160-byte record
  floor, the link is `NoPath`.
- Multi-hop path reset/discovery (`CMD_RESET_PATH`, `CMD_SEND_PATH_DISCOVERY_REQ`,
  `PUSH_CODE_PATH_DISCOVERY_RESPONSE`, `PUSH_CODE_PATH_UPDATED`) is **not
  implemented in this branch**. A learned multi-hop path is therefore not used
  without a recovery rule. The bench is direct.
- `PUSH_CODE_RAW_DATA` carries no sender identity. The host's single-peer bind
  remains the addressing fact.

## 5. Transport discipline

### 5.1 BLE

- Scan for the MeshCore service UUID, with the `MeshCore-` name as a fallback.
- Scan results are serialized on the main thread and bounded by a generation
  token. A cancelled scan cannot deliver a stale callback.
- Bond after service discovery, before notifications. The characteristics are
  encrypted, so the bond must be usable before APP_START.
- Request MTU 256 and require at least 179 (176-byte frame + 3-byte ATT
  overhead). No fragmentation is assumed.
- Write with response when the RX characteristic advertises `PROPERTY_WRITE`;
  use write-without-response only when that is the only property. A write
  failure ends the link and fails a pending raw callback.
- APP_START is sent only after the bond, MTU and notification setup complete.
- One BLE link exists per radio address and is reference-counted. Settings and
  the mesh terminal acquire the same link; opening Settings over a live
  terminal used to start a second GATT client on the same address, which left
  the first link half-dead and the dialog stuck at `closed`. The link closes
  when the last owner releases it.
- The mesh route uses the same foreground service as the tailnet route, so the
  BLE link survives a short app switch.

### 5.2 Commands and airtime

- Companion OK/ERR has no sequence number. `MeshcoreSession` now serializes
  tracked commands: one is written, its OK/ERR/EndOfContacts/DeviceInfo is
  awaited, then the next is written. A raw send is refused while any tracked
  command is queued or in flight, and a tracked command waits while a raw send
  is in flight.
- A by-key contact answer is one `ContactFrame` with no END_OF_CONTACTS. It
  releases the answer slot itself; a contact dump stays busy until
  END_OF_CONTACTS. Treating the single answer like a dump left the session
  permanently non-idle, and the connect path closed a healthy link 2.5 s after
  APP_START.
- A raw timeout is a dead link, not permission to send again. One raw send in
  flight. A late OK must not resolve a later send.
- LDMUX disables the MeshCore message drain. It has no chat consumer, and every
  drain command shares the same answer window.
- Radio `OK` means the frame was handed to the transmitter. It never means
  delivery. The L3 ARQ owns delivery.
- Airtime has two modes. **Open is the default** because the default preset is a
  private tuple, not a public MeshCore channel. **Metered** spends a 36 s/hour
  budget on data records (control and handshake records are not gated) and is
  available for an operator who explicitly joins a public mesh. Open must still
  respect the regional duty cycle. The status line shows records left when
  metered; `null` is not "unlimited".
- The meter is per adapter session: it adds the LoRa time-on-air of each
  accepted data record, rolls the window after one hour, and refuses a send
  with `RadioSendResult.Budget` when the budget is spent. It is an estimate for
  our own transmit time, not a round-trip prediction; the radio's own duty
  cycle remains the hard outer bound.
- The old gateway's 36 s/hour governor was removed with the gateway, and the
  standalone path now owns this counter explicitly. The mode is persisted in
  config (`meshOpenAirtime`, default `true`) and applied when a connection is
  built. The preset dialog shows the current mode and toggles it; it does not
  write a mode merely by being opened.

### 5.3 Error mapping

| Firmware code | Meaning | LDMUX |
|---:|---|---|
| 1 | unsupported command / bad raw shape | visible unsupported, not "too long" |
| 2 | not found | `Refused` / `NoPath` |
| 3, 4 | busy / table full | `Busy`, retry by ARQ |
| 6 | illegal argument | `NoPath` for a path/capacity failure |
| timeout | no answer | dead link, rebuild |

## 6. Radio preset

- The phone can change only the radio it is attached to:
  `CMD_SET_RADIO_PARAMS` (0x0B), `CMD_SET_RADIO_TX_POWER` (0x0C), and
  `CMD_SET_PATH_HASH_MODE` (0x3D, `3D 00 [mode]`, mode 0..2 = 1..3 bytes).
- **Preset is one tuple**: frequency, bandwidth, SF, CR, TX power and path-hash
  width. The three writes are serialized through the session queue and the
  read-back after a reconnect is the only proof.
- **No LDMUX-specific public preset.** A private tuple is generated inside a
  region's legal envelope and applied to both radios by whatever path reaches
  them. The phone can always set its own radio. The host radio is configured
  separately unless it is also reachable over BLE.
- Public presets are available behind a `public` action: USA/Canada, EU/UK
  narrow and wide, Australia and New Zealand. They are for joining an existing
  mesh or returning a radio to a stock state. The host must use the same
  frequency, bandwidth and SF. CR may differ because every packet carries it.
- The preset dialog opens on the tuple the radio is actually using, not on a
  fresh proposal. It shows the radio and link state, the selected tuple (which
  is the applied tuple on open), the region, `new private`, `restore`, `public`,
  the airtime mode, `apply` and `unset`. `restore` applies the tuple read at open, so a
  public experiment is reversible. `unset` clears the selected radio but keeps
  the session keys and the route. Choosing a radio from the scan opens the dialog
  immediately, so first run is permission → scan → bond → current tuple →
  apply → verify → host-key/path status.
- The app-side airtime meter is open by default. The one-line `airtime` action
  toggles metered/open for operators who join a public mesh.
- `RadioAirtime` is a pure Kotlin implementation of the LoRa time-on-air
  formula. It is used for the budget display and the per-record estimate; it
  is not used to claim delivery.
- **Legal envelope for a private tuple** (from the regulatory research, 2026-11):
  - USA/Canada 902-928 MHz: a fixed-channel DTS emission needs at least
    500 kHz 6 dB bandwidth. 62.5 kHz public MeshCore presets do not satisfy
    that. A 500 kHz private tuple does. Keep the centre between 902.250 and
    927.750 MHz so the occupied bandwidth stays inside the band, and keep
    conducted power at or below 30 dBm / 8 dBm per 3 kHz.
  - EU/UK 863-870 MHz: 500 kHz is illegal in the 869.4-869.65 MHz sub-band
    because the band is 250 kHz wide. Use 62.5, 125 or 250 kHz and stay
    entirely inside a permitted sub-band. 869.525 MHz with 125 kHz is safer
    than 869.618 with 62.5 kHz, whose upper edge has under 1 kHz of margin.
  - Australia 915-928 MHz: 1 W EIRP, radiated PSD at most 25 mW per 3 kHz,
    no duty cycle. 62.5/125/250 kHz fit the density limit at normal power.
  - New Zealand 915-928 MHz: 1 W EIRP, digital modulation or frequency
    hopping, no duty cycle in the band.
  - No firmware or app check makes a tuple legal. The preset is a suggestion;
    the operator remains responsible.
- TX power is clamped to `SELF_INFO.maxTxPowerDbm`.
- The write is not trusted. LDMUX waits for the tracked commands to finish,
  reconnects, reads `SELF_INFO` and `DEVICE_INFO`, and verifies frequency,
  bandwidth, SF, CR, TX and path-hash mode.
- On failure it restores the previous tuple, including path-hash mode, and says
  the result is unverified if it cannot prove the restore.
- `CMD_SET_CHANNEL` is a channel slot (name + 16-byte PSK), not a radio preset.
  The terminal does not use a channel slot.

## 7. Migration

### Removed from LDMUX

- `MeshGatewayClient`, `MeshGatewayFrame`, `MeshGatewayCode`, `MeshSocket`,
  `JavaMeshSocket`, `SocketFactory.android.kt`.
- `MeshTokenStore`, `KeystoreMeshTokenStore`, `meshTokenStore()`, the token
  argument to `MeshEnrollment`, `GatewayToken`.
- `MeshHandoff.kt`, `MeshHandoff.android.kt`, `MeshHandoffUi`, the Binder
  service permission, `openMeshMore`, the automatic/foreground handoff effects.
- `MeshRouteProblem.NeedsToken`, `MeshLinkState.NeedsToken`,
  `MeshGatewayEndpoint`, `LdmuxConfig.meshGateway`.
- Gateway-specific strings, scan UI and tests.

### Added

- Vendored `libmeshcore` and `libmeshcore-android`.
- `RadioLink`, `RadioPreset`, `RadioSelfInfo`, `RadioDeviceInfo`,
  `MeshRadioGateway`, `MeshRadioConnection`.
- `AndroidRadioLink`: BLE scan/permission/bond through the transport, path
  model, raw 0x19/0x41, preset read/write/verify.
- `RadioScanner`, `RadioPickerDialog`, `RadioPresetDialog`, `meshRadio` and
  `meshRadioName` config/prefs.
- `MeshCarrier.kt`: `MeshLinkState`, `MeshStatus`, `MeshSendOutcome`,
  `MeshSendResult`.

### Kept

- `MeshTermDriver`, `MeshSession`, `MeshTerm`, `MeshTerminalSource`, terminal
  screen, key bar, launcher, `MeshSessionKeyStore`, host pairing ceremony.
- The host gateway and daemon.

## 8. Java library changes

The vendored library is `libmeshcore` + `libmeshcore-android` (MIT,
`io.iotone.meshcore`) at source `7386999`.

LDMUX changes:

1. `CMD_SEND_RAW_DATA` 0x19 and `CMD_SEND_RAW_PACKET` 0x41 encoders.
2. `PUSH_CODE_RAW_DATA` 0x84 typing and marker validation.
3. `RawDataFrame`, `RawSendCallback`, `onRawData`.
4. `Contact` path decoding: encoded length, hash width, exact byte count,
   invalid/unknown handling.
5. `CMD_SET_PATH_HASH_MODE` 0x3D.
6. Tracked-command gate and raw-send correlation safety.
7. BLE write-type selection, MTU gate, `awaitBond` (create and wait, never remove),
   manager close.
8. RF-log capture keeps bytes, not per-frame hex strings.
9. Auto-drain can be disabled for LDMUX.
10. A by-key contact answer releases the command slot; a dump waits for
    END_OF_CONTACTS.

Remaining gaps for this branch:

- No path discovery or path-update handling. A radio `NotFound` send drops the
  stored path and retries zero-hop once; a path that dies silently still needs
  the ARQ or a rebuild.
- No automatic reconnect inside the library; the screen rebuilds.
- No BLE scan; the app owns it.
- No bond repair/remove. The connect path creates a bond if needed and never
  removes one; a stale duplicate record is pre-existing and not repaired here.

## 9. Tests and verification

Run affected classes during development; run the full suite once before a
commit and record wall time.

| Layer | Test | Timing |
|---|---|---|
| Codec | 0x19 encoded path, 0x41 packet, 0x84 marker/empty, 4-byte minimum | < 1 s |
| Contact | 0x00, 0x03, 0x41, 0x7F, 0x80, 0xC0, 0xFF | < 1 s |
| Session | fake transport: tracked command rejects raw; ERR does not resolve raw; timeout | < 2 s |
| Adapter | fake radio: path capacity, NoPath, error mapping, one send | < 2 s |
| Driver | unknown budget not stalled; ErrTooLong -> NoPath; no reconnect loop | existing suite |
| Route | no token, missing keys/peer, radio name, connectionKey | < 2 s |
| Scanner | generation, cancel, permission-pending | < 2 s |
| Session | tracked commands serialize; raw refused while tracked; raw answer pumps tracked | added |
| Hardware | scan, bond, MTU, SELF_INFO, 0x19/0x41 round trip, preset, shell | once |

Hardware is the only proof of permissions, GATT, bond, MTU, RSSI and a real
path. Do not repeat the full suite after every edit.

## 10. Desktop issues (separate branch)

Measured and code-read on 2026-11-02, status re-checked 2026-11-03:

- `ldmux-terminal` runs `ldmux window`, which creates a new shell and a new
  attach window. That is normal separate-shell behavior.
- **History defect — open.** The attach client never requests a history page. It
  sends only `attach` with size/features. The daemon supports `scrollback` and
  `SnapKind::History`. `render_snapshot` clears the visible screen and paints the
  current screen only. The desk window's own scrollback is therefore the rows it
  happened to see, not the shell's history.
- **Fit defect for mirrors — open.** A read-only mirror cannot resize the PTY
  (`should_reassert` returns false for read-only). It paints the holder's grid in
  its own window. `mirror_argv` carries no geometry. A row wider than the mirror
  is now cut rather than wrapped (`clip_row`), so it reads correctly, but the
  mirror still shows only the holder's columns.
- **Paint hazard — fixed (2026-11-03).** `render_snapshot` cut each row to the
  window but left the program's own modes in place: a scroll region would make the
  clear apply to that region, insert mode would shift every character, and a
  pending SGR would colour the pane's text. It now resets the scroll region, insert
  mode and SGR, disables autowrap for the paint, and restores wrap after the last
  row. `crates/attach/src/lib.rs`, test `a_snapshot_neutralises_what_the_program_left_behind`.
- **Resize — fixed (2026-11-03).** A drag's SIGWINCH burst is settled into one
  resize, rows are clipped, and the re-assert compares clamped sizes:
  `docs/DESKTOP-RESIZE.md`.
- Remaining fix plan: request a bounded history page once per attach and buffer
  live output until it is painted; give a mirror the holder's geometry or a
  per-client viewport.

## 11. Branch comparison

- `main` is the merge-base `c9c7643`; `feat/meshmore-gateway` is 218 files /
  34,522 insertions ahead.
- `docs/STATE.md` has stale "not implemented" statements. Do not use it as a
  baseline.
- No regression from `main` was found in the inspected code. This is a recorded
  conclusion, not a fresh full-tree reproduction.
- The last working branch's session-stability fixes, folder list, handshake
  ladder, background release and control-traffic rules remain in the L3 path.

## 12. Open items

- Hardware pass, completed 2026-11-02 on the WIO (`ED:76:07:68:5A:9A`) and the
  HELTEC-BASE host: scan, bond/PIN, MTU 247, SELF_INFO, DEVICE_INFO, a private
  tuple written and read back (914.25 MHz / 500 kHz / SF8 / CR5 / 22 dBm), a
  session established over the air, the host's shell listed, and typed input
  reaching the host terminal. Not yet measured: preset rollback after a
  rejected write, and the background-release path. The current/public/restore/
  airtime controls were added after that pass and are tested at the model level
  only.
- First-run is one combined flow: permission, scan, bond, connect, read the
  current tuple, generate and apply a private tuple, verify, show the
  host-key/path status. It is implemented and measured.
- The host radio must use the same tuple. It was configured out of band with
  `meshcore_host/tool/configure_radio.dart`; a USB-only host radio needs a host
  provisioning path, which this branch does not have.
- Airtime counter and the metered/open mode are wired for the phone's own
  radio. The host radio's budget is not metered by this app. Open is the
  default because the default preset is private.
- Multi-hop path reset/discovery is a follow-up; do not silently use a
  multi-hop path without it.
- Desktop history/fit fixes on a separate branch.
- MeshMore phone gateway remains opt-in and off by default. It must not run
  while LDMUX owns the radio.

## 13. Non-goals

- No host gateway rewrite.
- No channel PSK on the terminal.
- No raw-custom flood.
- No delivery claim from a radio OK.
- No local-shell mode in this branch.
- No UI redesign beyond the radio picker and preset dialog.
