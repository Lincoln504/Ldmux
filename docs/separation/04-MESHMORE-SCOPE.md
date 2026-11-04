# What MeshMore actually is

- Written 2026-11-04 against `meshmore-sns` at `5d97625`.
- The correction to the working assumption: MeshMore is not a mobile app. It is four shipping codebases, a protocol library, a JVM library, and a marketing site.
- LDMUX depends on two of those codebases, which is why the dependency is larger than it looks.

## The inventory

| Path | Name and version | Purpose in its own words | Size | Class |
|---|---|---|---|---|
| `meshmore_sns_app/responsive_starter_app` | `meshmore_sns_app` 1.0.193+9 (`pubspec.yaml:20`) | "a Flutter client for the Meshcore LoRa mesh network" | about 150 files under `lib/`, `meshcore_controller.dart` alone 4,024 lines | Ships to users, Android and iOS |
| `meshmore_sns_app/packages/meshcore` | `meshcore` 0.1.2 | "Pure-Dart implementation of the Meshcore companion-radio protocol, no Flutter dependencies" | `frame_codec.dart` 1,342, `constants.dart` 630, `mesh_carry.dart` 296 | Library, protocol |
| `meshmore_sns_app/packages/meshcore_gateway` | `meshcore_gateway` 0.1.0 | "The wire contract between MeshMore, which owns the radio, and a local client app, LDMUX" | `session.dart` 337, `server.dart` 203, `governor.dart` 193 | Wire contract only, no socket |
| `meshmore_sns_app/packages/meshcore_host` | `meshcore_host` 0.1.0 | "The MeshMore host-side process, the same L2 gateway the phone runs, with a MeshSender over a USB-serial companion radio" | `meshmore_gateway.dart` 423, `serial_mesh_sender.dart` 536 | Ships to users, a daemon |
| `libmeshcore` | Gradle `java-library` | "Pure-Java client library for the MeshCore companion-radio protocol, a faithful port of the hardware-verified Dart reference" | 60 Java files, 12 test classes | Library, protocol |
| `libmeshcore-android` | Gradle `com.android.library` | "Android and AndroidXR BLE transport and live integration test harness" | 5 Java files | Library, transport |
| `meshmore-xr` | `com.iotj.meshmore.xr` 0.3.0 build 83 | "A native Android XR client for the MeshCore LoRa mesh, built for optical see-through glasses" | `MainActivity.kt` 6,718, `MeshLink.kt` 2,276, about 35 more Kotlin files, about 60 test files | Ships to users, Android XR |
| `meshmore-xr-www` | Hugo site, `meshmorexr.iotj.cc` | Marketing, support and privacy, English and Japanese | 8 content pages | Support, web |
| `brand` | none | Logo work and mockup generators | 7 Python scripts | Tooling |
| `screens` | none | Six SVG UI mockups | 6 files | Documentation asset |

## What each shipping product does

### `meshmore_sns_app`, the Flutter app

- 22 routes, all reachable (`lib/app_router.dart:36-140`).
- Six-view swipe shell: dashboard, chat, nodes, grid, map, settings (`app_router.dart:37`).
- Radio configuration: frequency, bandwidth, spreading factor, coding rate, TX power, path hash size, identity, advert policy (`/settings/device`).
- Connected apps screen: the LDMUX gateway on and off, pairing token as QR and text, bound peer, airtime used, revoke (`connected_apps_screen.dart:112-199`).
- Named networks, channels with PSKs, hashtag-derived public channels, direct messages.
- Six radio tools: stats, ping, trace, discover, soak, range (`app_router.dart:88-99`).
- Offline maps with region packs and PMTiles, hyperlocal radar grid, node telemetry, TTS, six themes, English and Japanese.
- Implemented and reachable: all of the above. The README's not-built list is R24 to R27 (`README.md:96-101`).

### `meshcore_host`, the host daemon

- One process, one serial port, one loopback listener.
- Opens the port, runs `APP_START`, prints the radio's own preset (`bin/meshmore_gateway.dart:130-133`).
- Serves the L2 gateway on `127.0.0.1:<port>` (`:84-87`).
- Resolves `--peer-name` against the radio's contact table and writes `radioKey` and `peerKey` into a 0600 state file (`:100-127`).
- Three timers: stats every 60 s (`:139`), deaf check every 30 s (`:150`), presence every 2 s (`:184`).
- Exit codes: 64 usage, 69 the radio never answered `APP_START` (`:67`), 78 peer name not found (`:115`), 75 temporary failure from two independent causes, a deaf radio (`:165`) and a port closed for three consecutive checks (`:192`), 0 clean stop (`:208`).
- No reconnect logic of its own. Recovery is systemd's (`deploy/meshmore-gateway.service:20-27`).

### `meshmore-xr`, the glasses app

- Real BLE code, not a prototype: `MeshLink.kt` scans, bonds with explicit pairing variants (`:873-917`), opens a session, tracks delivery states (`:354`), threads, channels, and a radio config with a write-then-commit path (`:1059`).
- `MainActivity.kt` is a 6,718-line Compose surface with `HorizonScene` (`:1702`), `SearchSurface` (`:5928`), `AlertsPanel` (`:6259`), `RadioLogPanel` (`:6312`) and `StatusScreen` (`:6377`).
- Consumes `libmeshcore-android` (`app/build.gradle.kts:169`) and `io.iotone.meshcore.crypto.ChannelCrypto` (`Channels.kt:5-6`).
- `FakeMesh.kt` is a test-only peer generator shaped from a real 350-peer census (`FakeMesh.kt:28-36`).

## The protocol library

- Pinned to `companion-v1.15.0`, commit `dee3e26ac081a5c668c69b66c16a6544a44ddc5b`, asserted in code at `meshcore/lib/src/codec/constants.dart:22-24` and tested at `test/constants_test.dart:9`.
- 58 command constants (`constants.dart:170-407`), 57 encoders (`frame_codec.dart:38-769`). The gap is `sendChannelDatagram` at `0x3E`, declared and never used.
- 40 inbound frame classes (`codec/inbound.dart:18-436`), dispatched from one total `decode()` (`frame_codec.dart:776`), with unknown opcodes becoming `UnsupportedFrame` (`inbound.dart:425`).
- The connection layer is deliberately thin: state machine, 8 s handshake timeout (`client/meshcore_connection.dart:40`), `sendCommand` (`:153`), no correlation and no tracked queue. Correlation lives in the host's `_serialise` and `_awaitingReply` (`serial_mesh_sender.dart:236-290`).
- 18 test files, including fuzzing, interop replay, network shaping and four JSON vector sets.

## The gateway contract

- Version 1, magic `"MG"`, 7-byte header, 512-byte maximum payload (`meshcore_gateway/lib/src/constants.dart:10-30`).
- 14 frame types (`:76-90`): hello, pair, bind, record, recv, send_result, status, close, each with an ok and an err form.
- Limits: record 172, floor 160, discovery 142, credit window 8, pre-auth timeout 5 s, token 16 bytes (`:44-70`).
- Loopback only (`server.dart:39`), one client at a time with an explicit busy close (`:17-20`), constant-time token compare (`pairing_store.dart:80-82`), pre-auth timeout (`session.dart:297-303`).
- Port 41731 is a preference, not a guarantee: Android reserves it and returns EPERM, so the gateway falls back to an ephemeral port and every consumer must read the port after start (`server.dart:44-56`).
- Two governor policies: one percent duty cycle at 36 s per hour, and owned channel with no cap but still metered (`governor.dart:15-27`).
- The airtime estimator is built from the radio's real parameters on both ends (`gateway_service.dart:207-215`, `bin/meshmore_gateway.dart:88-93`), so the old flat 4 ms per byte estimate is gone from the code.

## The documentation

| File | Size | Specifies | Built or planned |
|---|---|---|---|
| `meshmore-sns-spec.md` | 155 KB | Requirements R1 to R108 plus the change record | Built |
| `meshmore-sns-architecture.md` | 62 KB | Architecture of record, sections 1 to 13 | Built, with a phased plan |
| `meshmore-sns-architecture-audit.md` | 74 KB | Append-only audit trail, 2026-10-24 to 10-29 | Built |
| `meshmore-sns-UX-brief.md` and `.html` | 32 KB and 97 KB | Six visual concepts | Design |
| `meshmore-sns-UX-spec.md` | 32 KB | U0 to U15 implementation plan | Historical |
| `meshmore-sns-maps-spec.md` | 27 KB | Offline maps | Built |
| `meshmore-sns-meshcore-interop.md` | 30 KB | MeshCore as a shared transport, protocol facts | Built, plus hardware notes |
| `meshmore-sns-private-mesh-investigation.md` | 30 KB | Private mesh mechanisms, haptics, headers | Investigation |
| `meshmore-sns-radio-alerts.md` | 22 KB | Firmware beep and the per-board matrix | Built, flashed |
| `meshmore-sns-radio-config.md` | 16 KB | Radio settings, time on air, presets | Built |
| `meshmore-sns-tools-spec.md` | 13 KB | The tools surface | Built |
| `meshmore-sns-telemetry-research.md` | 14 KB | Altitude against phone GPS | Research |
| `meshmore-sns-gateway-handoff.md` | 9 KB | Binder handoff design of record | Built |
| `meshmore-sns-protocol-coverage.md` | 6 KB | Opcode coverage | Stale, see `03-DEFECTS-MESHMORE.md` |
| `meshmore-sns-ux-coverage.md` | 6 KB | Which capabilities get UI | Built |
| `meshmore-sns-geofence-tracking.md` | 8 KB | Tracking and geofencing | Earmarked |
| `meshmore-sns-meshbook-mbk.md` | 5 KB | Meshbook | Earmarked |
| `meshmore-sns-microclimate-wx.md` | 10 KB | Microclimate | Earmarked |
| `lobospeak-design.md` and `-mappins-spec.md` | 30 KB and 22 KB | A separate product on the same transport | Design |
| `M6-interop-runbook.md` | 5 KB | On-device interop procedure | Runbook |
| `AndroidXR_*` and `UX-XR-best-practices.md` | 34, 24 and 24 KB | XR platform research | Reference |

## What this means for the separation

- MeshMore is a product family with its own users, its own release cadence and its own roadmap.
- LDMUX uses exactly two of its pieces: the host daemon and the gateway wire contract.
- Everything else in MeshMore is irrelevant to LDMUX, which is why the dependency is a boundary problem and not a code-sharing problem.
- The Binder handoff is built, not planned (`GatewayHandoff.kt:62`, `AndroidManifest.xml:14`, `gateway_handoff.dart:68`), so the phone-side coupling is also real code and not a design sketch.
