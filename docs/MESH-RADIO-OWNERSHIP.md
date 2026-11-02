# Should LDMUX own the radio? A review of the Java MeshCore libraries

**Status:** investigation, 2026-11-01. No code changed. The decision is to keep
MeshMore as the radio owner for now; the conditions that would change that are
at the end.

## 1. The question

MeshMore owns the phone's BLE link to the MeshCore radio and exposes a loopback
gateway; LDMUX is a blind client of that gateway. Pairing has two steps (a gateway
token and the host's session keys), and MeshMore must be running.

Should LDMUX drop MeshMore's IPC and pairing and drive the radio itself, using
David Kordsmeier's Java MeshCore libraries that are already in this repository?

## 2. What is actually in the repository

`libmeshcore` and `libmeshcore-android` are real and are David's work:

- `a8fe3b9` (2026-06-12) "Implement a meshcore client library, android friendly,
  CI friendly", and `b8e994e` (2026-06-13) the Android module. Further work
  through `7386999` (2026-08-17), which added capacity/contacts/channels calls to
  `MeshcoreSession`.
- 5,626 lines of main Java (core + Android), 1,871 lines of tests, MIT.
- It is a **companion-protocol client**: frame codec, CayenneLPP telemetry,
  channel/DM/identity crypto, typed frames and models, a Nordic-BLE transport, and
  a `MeshcoreSession` that does `APP_START`, the self-info handshake and the
  `SYNC_NEXT_MESSAGE` drain.
- It is **not** used by the Flutter MeshMore app. The Flutter app has its own Dart
  stack (`packages/meshcore`, `meshcore_gateway`, `meshcore_host`: 8,107 lines and
  26 test files) and does not reference `io.iotone.meshcore` anywhere.
- It **is** used by `meshmore-xr`, the Kotlin/Compose AndroidXR app
  (`MeshLink.kt:20-25`, `:1761-1762`), for channel/contact messages, adverts and
  telemetry.

### What it does not have

The exact carriage LDMUX uses is missing:

- The command enum jumps from `DEVICE_QUERY (0x16)` to `GET_CHANNEL (0x1F)`
  (`MeshcoreCommand.java:68,71`) and ends at `SEND_CHANNEL_DATAGRAM (0x3E)`
  (`:106`). There is no `CMD_SEND_RAW_DATA` (0x19) and no `CMD_SEND_RAW_PACKET`
  (0x41).
- There is no `PUSH_CODE_RAW_DATA` (0x84) frame; a grep of the Java sources for
  raw-data names and 0x84 finds only CayenneLPP's `DIRECTION` constant.
- `MeshcoreSession`'s public API (`MeshcoreSession.java:139-451`) sends channel and
  direct text, adverts, contacts, channels and radio parameters. No raw send, and
  `SessionListener` has no raw-data callback.

The transport is public, so an app *could* hand-build a 0x19 frame and parse 0x84
from an `UnsupportedFrame`; that is app-level protocol work, not a capability the
library provides. The missing pieces are the **path lookup, send serialisation,
OK/ERR correlation, airtime accounting and inbound raw dispatch** that MeshMore's
`MeshcoreMeshSender` already implements (`mesh_sender_adapter.dart:38,114-185`).

### Version and test state

- The library is pinned to **`companion-v1.15.0`**
  (`MeshcoreConstants.java:31`, commit `dee3e26a…`). The deployed firmware is
  **1.17.1** (`d929643`), and the Dart package already implements the 1.17-era
  raw commands.
- `./gradlew test` on 2026-11-01: **122 tests, 1 failed, 1 skipped** — not the
  "98-test suite" the README claims. The failure is
  `AdvertCodecTest.truncatedAdvertFailsControlled`: the July 30 change
  `dbd62f6` deliberately accepts a short advert as a **bare advert** (real
  firmware pushes only the 32-byte key, and the old parser dropped every advert),
  and the test still expects a decode failure. It is a stale test, not a runtime
  regression, but the suite is not green.

## 3. The hardware fact that decides the shape

The nRF52 companion radio tracks **one BLE connection**:

- `SerialBLEInterface::onConnect` overwrites `_conn_handle` with the newest
  handle and clears the shared send/receive queues
  (`meshcore-fw-alerts/src/helpers/nrf52/SerialBLEInterface.cpp:147-153`).
- `isValidConnection` accepts only the handle currently stored
  (`:359-371`). A second central's callbacks are therefore ignored, and the first
  app's link goes silent.

The companion protocol is also a **single command stream**: one
`BaseSerialInterface`, one queue, one `pending_req` slot. Two apps cannot each
have an independent companion session. The real choices are one owner, a shared
owner service, or exclusive use by whichever app is open.

The BLE link is not "handed over" to another app; it is stolen by the newest
connection. This is the same conclusion as
`meshmore-sns-meshcore-interop.md` §5 (options A–D; "hand the BLE link over —
not possible").

## 4. What LDMUX uses today

- The radio and BLE link are MeshMore's; LDMUX has **no BLE code and no BLE
  permissions** (`packages/mobile/composeApp/src/androidMain/AndroidManifest.xml`
  has INTERNET, network state, the MeshMore handoff permission, audio,
  foreground-service and wake-lock entries — no `BLUETOOTH_SCAN`/`CONNECT`).
- The gateway is a **blind carrier**: `PAIR`, `BIND`, `SEND`, `RECV`, `STATUS`;
  the records are opaque. It never exposes `SYNC_NEXT_MESSAGE`, channel writes or
  radio configuration to the client (`meshcore_gateway/lib/src/messages.dart`,
  `mesh_sender.dart`).
- There are **two pairings**, and only one is MeshMore's:
  1. the gateway token + bound port, delivered over Binder by
     `GatewayHandoffService` (now automatic; no typing), and
  2. the host's L3 keys: `host_key`, `psk`, `host_radio` from
     `ldmux code --mesh` (`crates/protocol/src/pairing.rs:47-56`), stored in the
     phone's Keystore (`MeshEnrollment.kt`). Dropping MeshMore does **not**
     remove this one.
- The phone's uplink uses `CMD_SEND_RAW_DATA` over a contact's direct path
  (`mesh_sender_adapter.dart:185`); the phone's `bindPeer` refuses a contact with
  no path (`:155-165`). Raw custom is **direct-only** in this firmware: flooded
  raw custom is not delivered, and `CMD_SEND_RAW_DATA` rejects a flood path
  (`meshmore-sns-meshcore-interop.md` §2.4a). A zero-hop path reaches a
  neighbour; beyond that, a route is required.
- MeshMore's UI provisions the host as a contact. A standalone LDMUX would need
  that provisioning, or an explicit zero-hop/neighbour path story.
- The host side of the same carriage is `serial_mesh_sender.dart` (535 lines,
  path trust, flood warm-up, `CMD_RESET_PATH`, airtime metering); the phone side
  is `MeshcoreMeshSender` plus the gateway's governor. The Java library contains
  neither.

## 5. The options

| Option | What it buys | What it costs |
|---|---|---|
| **A. Keep the gateway (current)** | One radio owner; MeshMore's chat and LDMUX work at the same time; the tested Dart stack owns the hardware quirks (bond repair, unnamed radios, path trust, flood, deaf-radio watchdog) | Two apps, a token, a loopback surface, and MeshMore's engine lifetime. The remaining cold-start friction is that MeshMore's Flutter engine must exist to serve the handoff. |
| **B. Standalone LDMUX** | No MeshMore, no gateway token, no loopback; LDMUX controls the radio directly | MeshMore's chat loses the radio while LDMUX runs (single BLE central); LDMUX must implement BLE scan/bond/reconnect, raw carriage, path/flood, airtime, contact provisioning and a foreground service. The L3 pairing remains. |
| **C. Shared Kotlin radio service** | One owner, both apps, no token; a real radio API for future clients; David's library is a base | A rewrite of the radio stack from Dart to Kotlin (or a Kotlin service with a Flutter bridge), including the raw carriage the library lacks. Weeks of hardware-in-the-loop work; the Dart stack must remain the reference until parity. |
| **D. Merge LDMUX into MeshMore** | No IPC, no pairing, one process, radio lifetime already solved | The terminal has to live in Flutter (platform view/AAR or a native terminal engine), two products become one release train. |

## 6. Critical assessment

- **The 2026-11-01 failures were lifecycle bugs, not evidence against the
  architecture.** The gateway was a lazy provider; Back finished MeshMore's
  Activity and destroyed the engine; a closed gateway did not end the driver's
  `run`; the mesh route had no platform release. All four are fixed and measured
  (`docs/MESH-SESSION-STABILITY.md` §8): a cold MeshMore served the gateway as
  soon as it was opened, Back kept it alive, and a 60-second background returned
  to the same shell in 3 seconds with one handshake.
- **Standalone does not remove pairing.** It removes the gateway token (already
  automatic). The `ldmux code --mesh` ceremony remains; removing that would be a
  deliberate L3 trust change, not an architecture change.
- **The single-BLE-central fact is decisive for coexistence.** If MeshMore is
  installed and used for chat, standalone LDMUX means MeshMore stops receiving
  while the terminal is open. The radio's buffer is finite and has no history
  fetch; an overflow loses messages (MeshMore's own `background_keepalive.dart`
  and the interop doc §5).
- **The Java library is a messaging client, not a tunnel.** It is a good second
  implementation and test oracle, and the XR app proves it can drive a radio. It
  is pinned to 1.15.0, has no raw commands or raw session, no path/airtime
  logic, and its own suite is one test short of green. Using it for LDMUX is a
  port of the remaining phone-side stack, not a swap of one dependency.
- **The Dart stack is the most hardware-tested code in either repository.** The
  October commits alone cover bond repair, unnamed radios, path trust, flood
  policy and the deaf-radio watchdog. Reimplementing that in Kotlin is the
  largest risk in any standalone plan.
- **If the goal is only "do not open MeshMore"**, a headless on-demand owner is
  the targeted fix: a FlutterEngine/radio service that starts the gateway when
  the handoff is asked, rather than a second full stack. That still needs a
  single-engine design (the UI and the headless owner must not both run
  `MeshcoreController`), so it is its own project, but a much smaller one than B.

## 7. Recommendation

1. **Keep MeshMore as the radio owner.** The gateway is the correct shape for a
   single BLE central shared by two apps, and the recent failures are fixed.
2. **If MeshMore must not be opened**, scope a headless on-demand owner (one
   engine, one `MeshcoreController`, started by the handoff service), not a
   standalone radio stack in LDMUX.
3. **If LDMUX must work on a phone with no MeshMore installed**, build **C**, not
   B: extend David's library with
   - `CMD_SEND_RAW_DATA` (0x19) / `CMD_SEND_RAW_PACKET` (0x41) /
     `PUSH_CODE_RAW_DATA` (0x84),
   - a raw session API and `SessionListener.onRawData`,
   - path lookup, send serialisation, OK/ERR correlation, airtime overhead,
   - BLE scan/bond/reconnect and a `connectedDevice` foreground service,
   - a current-firmware pin and a green suite,
   then put it behind a service. Keep the Dart stack as the reference until the
   Kotlin service passes the same hardware tests. This is weeks of work and a
   second radio stack to maintain, not a refactor.
4. **If the product is one app**, take **D**: embed LDMUX's terminal in MeshMore
   and delete the gateway. That is the only option that removes both the IPC and
   the second app without pretending the radio can have two owners.

## 8. What would change this answer

- MeshMore is not needed on the same phone as LDMUX, and losing its chat while
  the terminal runs is acceptable: B becomes viable, with the port above.
- A third-party radio API is the goal: C is the right shape (the interop doc's
  option B), with the capability-scoped tunnel, not a raw companion surface.
- LDMUX's terminal becomes a MeshMore feature: D.
- A firmware change allows multiple independent companion sessions or flooded
  raw custom. Neither exists today.

## 9. Evidence

- Java library: `libmeshcore/README.md`, `MeshcoreConstants.java:31`,
  `MeshcoreCommand.java:68-106`, `MeshcoreSession.java:139-451`,
  `SessionListener.java`; `./gradlew test` on 2026-11-01 (122 tests, 1 failed, 1
  skipped).
- Single BLE session:
  `meshcore-fw-alerts/src/helpers/nrf52/SerialBLEInterface.cpp:147-166,359-371`.
- Current carriage: `meshmore_sns_app/packages/meshcore_gateway/lib/src/`
  (`messages.dart`, `mesh_sender.dart`), and
  `responsive_starter_app/lib/meshcore/gateway/mesh_sender_adapter.dart:38,114-185`.
- LDMUX ownership: `docs/MESH-GATEWAY.md` §1; `docs/MESH-TRANSPORT.md` §"the mesh
  transport is a separate gateway process".
- The two pairings: `crates/protocol/src/pairing.rs:47-56` and
  `MeshEnrollment.kt`; the gateway token is a separate store.
- Prior analysis: `meshmore-sns-meshcore-interop.md` §5 (who owns the radio),
  §7 (phased plan), §8 (merge or tunnel).
