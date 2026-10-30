# First hardware validation: runbook (prepared 2026-10-29, not yet run)

Nothing here has been executed on a phone or on the radio's USB image. Every command below was
checked for existence and syntax only. Sequence is **phone first, radio last**: the same Heltec
board serves the phone over BLE today, and the host chain needs it reflashed to the USB image.

## 0. Artifacts (built 2026-10-29)

| Artifact | Path | Size | SHA-256 |
|---|---|---|---|
| LDMUX debug APK, package `dev.ldmux.mobile` | `ldmux/packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk` | 67,955,440 B | `20a9ee26a806ca8153eda545f7b9099da7f18da5031223d52b2af3d0520246ac` |
| USB companion firmware, env `heltec_v4_companion_radio_usb` | `/tmp/mc-hwprep/.pio/build/heltec_v4_companion_radio_usb/firmware.bin` (+ `bootloader.bin`, `partitions.bin`) | 642,048 B | `2546bcdd04a467a288b1b5c784d96b0f7afc87b57b69af0e0499bcd93ba01a64` |
| MeshMore debug APK, package `com.iotone.meshmore_sns_app` | **NOT BUILT** (see below) | | |

- LDMUX APK contains `lib/arm64-v8a/libldmux_session.so` (1,424,840 B) and
  `lib/x86_64/libldmux_session.so` (1,541,928 B) (`unzip -l`). `aapt dump badging`: minSdk 28.
- **MeshMore cannot be built from this checkout.** `flutter build apk --debug` fails:
  `Your project's Gradle version (8.12.0) is lower than Flutter's minimum supported version of
  8.14.0` (`responsive_starter_app/android/gradle/wrapper/gradle-wrapper.properties`, tracked,
  `distributionUrl=...gradle-8.12-all.zip`). Cached wrappers exist (`~/.gradle/wrapper/dists/`
  has 9.7.0 and 9.7.1), but changing the wrapper is a platform-dir edit and was out of scope.
  The only APK on disk, `build/app/outputs/flutter-apk/app-debug.apk` (200,523,765 B, SHA-256
  `4125be88...a640251`, version 1.0.193, dated 2026-10-22) **predates the gateway work**: its
  `kernel_blob.bin` contains none of `GatewayServer`, `GatewayPairingPayload`,
  `meshmore://gateway`. Do not install it for this validation.
- Firmware was built from a copy of `meshcore-fw-alerts` at `d929643` **plus its four uncommitted
  working-tree edits** (`MyMesh.cpp`, `MyMesh.h`, `NodePrefs.h`, a test): `rsync` copies the working
  tree, not the commit. The image therefore carries the alerts patch.

## 1. Phone runbook (radio stays on BLE)

Prerequisite decision: unblock the MeshMore build first (Gradle wrapper >= 8.14, or a Flutter
that supports the installed AGP), then:

```
export ANDROID_HOME=$HOME/Android/Sdk
export JAVA_HOME=/home/ldeen/.local/jdk/jdk-21.0.12+8
export PATH=/nix/store/x8cc2489gk0lhbgm5q3y7pxfb7v9r706-flutter-wrapped-3.47.4-sdk-links/bin:$PATH
(cd meshmore-sns/meshmore_sns_app/responsive_starter_app && flutter build apk --debug)
ADB=$ANDROID_HOME/platform-tools/adb
$ADB devices -l                       # must list the phone; none was attached on 2026-10-29
$ADB install -r meshmore-sns/meshmore_sns_app/responsive_starter_app/build/app/outputs/flutter-apk/app-debug.apk
$ADB install -r ldmux/packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
$ADB shell am start -n com.iotone.meshmore_sns_app/.MainActivity   # launchable-activity verified with aapt on the (stale) APK
$ADB shell am start -n dev.ldmux.mobile/.MainActivity               # verified with aapt on the LDMUX APK
$ADB logcat -s flutter:V ldmux:V AndroidRuntime:E
```

Sequence:

1. Open MeshMore, connect the radio over BLE (`MeshCore-HELTEC-BASE`, `F8:5B:1B:BE:C5:3D`).
2. MeshMore starts its loopback gateway (`127.0.0.1:41731`) only while the radio is connected.
3. Settings, Connected apps: read the QR. It encodes `meshmore://gateway?v=1&host=127.0.0.1:<port>&token=<32 hex>`.
4. On the host: `ldmuxd` with `LDMUX_MESH_PSK` (64 hex) and `LDMUX_MESH_SELF` (host radio public key,
   64 hex; see section 2) in its environment, then `ldmux code --mesh-only`.
5. In LDMUX, Settings, mesh section: scan the ldmux code first (session keys), then the MeshMore code
   (token). The order is enforced: the MeshMore code is refused until session keys exist.
6. Choose the Mesh route in the picker. It only offers a route that can run.

### What each state means

| State | Source | Meaning, and the fix |
|---|---|---|
| `SessionNotBuilt` | `resolveRoute` | `libldmux_session.so` did not load. Check logcat; ABI mismatch or missing lib. |
| `TerminalNotBuilt` | `resolveRoute` | Build constant false. Should not appear in this build. |
| `NeedsSessionKeys` | `resolveRoute` | No ldmux `--mesh` or `--mesh-only` code scanned yet. |
| `NeedsToken` | `resolveRoute` | No MeshMore gateway code scanned. |
| `NeedsPeer` | `resolveRoute` | The ldmux code had no `mr=`: `LDMUX_MESH_SELF` was unset when the code was made. |
| `LinkDown(Unreachable)` | driver | Label `mesh · MeshMore not running`: nothing listening on the loopback port. |
| `LinkDown(Refused)` | driver | Wrong token, or the host is not a contact in the radio. Re-pair or add the contact. |
| `LinkDown(RadioUnavailable)` | driver | MeshMore is up but the radio is not connected. |
| `LinkDown(GatewayBusy)` | driver | Another app holds the gateway (one client only). |
| `LinkDown(ProtocolMismatch)` | driver | Gateway and phone disagree on the L2 version. |
| `LinkDown(SessionFailed)` | driver | Label `mesh · host not answering`: handshake sent, no reply. |
| `ProfileMissing` | driver | Session is up but the host sent no `Profile`. |
| `HostClosed(reason)` | driver | Host ended the attach (e.g. ambiguous or no shell). |

(`MeshRoute.kt` `meshLinkLabel`/`meshProblemLabel`; driver views in `MeshTermDriver.kt`.)

### What a phone-only run can and cannot prove

- Can prove: the loopback gateway accepts the LDMUX app, L2 HELLO/PAIR/BIND on Android, `libldmux_session.so`
  loads on the device ABI, and the L3 handshake message 1 is produced and sent.
- Cannot prove: an over-the-air session. It needs a second radio running the USB image attached to a host
  running the daemon. With one board there is no peer.
- Also unproven: Android Keystore blobs, the QR scanners, Compose rendering, `ACCESS_LOCAL_NETWORK`
  behaviour on Android 16/17 with MeshMore backgrounded.

### Foreground-service dependency

The gateway lives inside MeshMore's process. MeshMore keeps it alive through a
`connectedDevice` foreground service (`AndroidManifest.xml` `ForegroundService`,
`foregroundServiceType="connectedDevice"`). If the service is not running (app swiped away, radio
disconnected, battery optimisation), nothing listens on port 41731 and LDMUX shows
`mesh · MeshMore not running`. Unverified on a device.

## 2. Desktop chain (radio flashed to USB, do this last)

This reflashes the board that currently serves the phone's BLE. Restore step is section 3.

```
# build already done in /tmp/mc-hwprep; to rebuild:
#   rsync -a --exclude .pio --exclude .git meshcore-fw-alerts/ /tmp/mc-hwprep/ && cd /tmp/mc-hwprep && ~/.venvs/pio/bin/pio run -e heltec_v4_companion_radio_usb
cd /tmp/mc-hwprep
~/.venvs/pio/bin/pio run -e heltec_v4_companion_radio_usb -t upload --upload-port /dev/ttyACM0
# REQUIRED: "Hard resetting via RTS pin" is not proof the new image runs (2026-10-28 evidence).
~/.platformio/packages/tool-esptoolpy/esptool.py --chip esp32s3 --port /dev/ttyACM0 --after hard_reset chip_id
ls -l /dev/ttyACM0     # the node may re-enumerate; re-check
```

Proof the USB image runs: `dart run tool/hardware_smoke.dart /dev/ttyACM0` (in
`meshmore-sns/meshmore_sns_app/packages/meshcore_host`) prints the radio's `key` (64 hex, use it as
`LDMUX_MESH_SELF`). **Warning:** it adds and removes one synthetic contact and transmits one packet.

Start both processes. Plain `nohup` died with exit 143 in the earlier session; use `setsid`:

```
export PATH=/nix/store/x8cc2489gk0lhbgm5q3y7pxfb7v9r706-flutter-wrapped-3.47.4-sdk-links/bin:$PATH
TOKEN=00112233445566778899aabbccddeeff          # 32 hex; make a fresh one for anything real
cd /home/ldeen/Documents/meshmore-sns/meshmore_sns_app/packages/meshcore_host
setsid nohup dart run bin/meshmore_gateway.dart --port /dev/ttyACM0 --listen 41731 \
  --token $TOKEN --state /tmp/gw-live.json > /tmp/gw-live.log 2>&1 < /dev/null & disown

cd /home/ldeen/Documents/ldmux
mkdir -p /tmp/ldmux-live
LDMUX_STATE_DIR=/tmp/ldmux-live LDMUX_SOCK=/tmp/ldmux-live/ldmuxd.sock \
LDMUX_MESH_ADDR=127.0.0.1:41731 LDMUX_MESH_TOKEN=$TOKEN \
LDMUX_MESH_PEER=<64 hex: the phone radio's public key> \
LDMUX_MESH_PSK=<64 hex> LDMUX_MESH_SELF=<64 hex from hardware_smoke> \
setsid nohup ./target/debug/ldmuxd > /tmp/ldmuxd-live.log 2>&1 < /dev/null & disown

LDMUX_SOCK=/tmp/ldmux-live/ldmuxd.sock ./target/debug/ldmux mesh status
LDMUX_SOCK=/tmp/ldmux-live/ldmuxd.sock ./target/debug/ldmux mesh status --json
LDMUX_SOCK=/tmp/ldmux-live/ldmuxd.sock ./target/debug/ldmux code --mesh-only
```

Binaries exist at `target/debug/ldmux` and `target/debug/ldmuxd` (checked). If the CLI does not
find the socket, pass the same `LDMUX_STATE_DIR`/`LDMUX_SOCK` it was started with.

### Expected output and what it proves

| Output | Proves |
|---|---|
| `mesh: no link configured on this host` | Daemon up, no `LDMUX_MESH_*` set (disabled, not broken). |
| `state ready`, `capacity 172 B per record`, `budget 58 records left this hour`, `radio connected` | Gateway talks to the real radio over serial, BIND found a route, governor budget derived from the preset (2026-10-28 live result). |
| `state down`, `last bind refused with code 2` | `no_path`: the peer is not a contact, or has a flood-only route. Not a gateway fault. |
| `signal no reading, battery not reported` | Sentinels (-128 / 255) rendered as words. |
| `session off (set LDMUX_MESH_PSK ...)` | PSK missing. |
| `session <state>`, `gateway key ...`, `phone key none yet` | L3 host is up with a static key; no phone has completed a handshake yet. |
| `phone key ... (pinned)`, `handshakes 1 accepted` | A phone handshake was accepted and its key pinned. Only possible once the phone chain reaches the host. |
| `last a downlink record arrived; no session is configured` | Records came in without `LDMUX_MESH_PSK`. |

One board, one radio: the host chain proves L2 and the L3 host being ready, not an over-the-air
session. That needs a second radio on the phone side.

## 3. Restore the BLE image (so the phone app works again)

```
cd /tmp/mc-hwprep   # or a fresh copy
~/.venvs/pio/bin/pio run -e heltec_v4_companion_radio_ble -t upload --upload-port /dev/ttyACM0
~/.platformio/packages/tool-esptoolpy/esptool.py --chip esp32s3 --port /dev/ttyACM0 --after hard_reset chip_id
bluetoothctl scan on     # MeshCore-HELTEC-BASE must advertise again
```

Stop the host processes first: `pkill -f meshmore_gateway.dart` and `pkill -x ldmuxd`
**in a separate call** (a `pkill -f` pattern inside the same bash command line kills that shell, exit 143).

## 4. Not verified on 2026-10-29

- MeshMore APK build (blocked, above). No device attached to ADB (`adb devices` empty).
- MeshMore's activity name was read from the stale APK; re-check on the rebuilt one.
- Nothing on a phone, nothing on the radio's USB image since 2026-10-28.

## Phone-only run, 2026-10-29 (Pixel-era Android 16, Moto Edge 2024)

What was done: MeshMore and LDMUX built and installed over the existing installs
(the previous builds were signed with `ANDROID_USER_HOME=~/.config/.android`, so
that variable must be set or `adb install -r` fails with a signature mismatch),
the radio stayed on the BLE image, MeshMore's gateway was turned on, and LDMUX
was paired for a mesh-only route and switched to it.

Results, in order:

- **The gateway could not start at all.** `ServerSocket.bind(127.0.0.1:41731)`
  returned `EPERM`. 41731 is listed in the device's
  `/proc/sys/net/ipv4/ip_local_reserved_ports`, and binding a reserved port as
  an app UID is refused with `Operation not permitted`. Fixed: the preferred port
  is now a preference, with an ephemeral fallback on EPERM/EADDRINUSE, and the
  real port is what the Connected apps payload carries. The gateway then reported
  `Listening on 127.0.0.1:42989`.
- **Scanning cannot pair two apps on the same phone.** MeshMore displays its code
  on the device LDMUX is running on, so there is nothing for LDMUX's camera to
  point at. Fixed: a "paste a code" field that routes by scheme (`ldmux://` versus
  `meshmore://` or a bare 32-hex token), so a user never has to know which box a
  code belongs in.
- **The L2 path works on the device.** With `adb forward` to the phone's gateway,
  the desktop client completed `connect -> HELLO -> PAIR` and was refused at
  `BIND` with `not_a_contact`, which is correct: the `mr=` key in the test pairing
  was random, not the host radio's key. The LDMUX screen showed the same refusal
  and named both cures.
- **Two UI defects the device showed.** The mesh screen drew its status line under
  the system clock (no `safeDrawingPadding`), and the `leave` button was squeezed
  to one character wide by a `SpaceBetween` row. Both fixed.

What this proves: the loopback gateway, the L2 handshake, token auth and the bind
refusal rule on Android 16 with a real radio attached over BLE.

What it does not prove: a bound session, the L3 handshake over the air, or the
terminal profile, because a bound session needs a peer that is a real contact.
That needs the second radio.

## Result: the phone leg, 2026-10-29 (real device, one radio, no peer)

Device: Motorola Edge 2024, Android 16 (API 36). Radio: Heltec V4 on the BLE
companion image, linked at 910.525 MHz / 62.5 kHz / SF7 / CR5 / 22 dBm.

### Four defects only the device showed

1. **The gateway could not start at all.** `SocketException: Failed to create
   server socket (OS Error: Operation not permitted, errno = 1)` on
   `127.0.0.1:41731`. Not a permission: this device's
   `/proc/sys/net/ipv4/ip_local_reserved_ports` contains **41731**, and binding
   a reserved port as an app UID returns EPERM rather than EADDRINUSE. Fixed by
   treating the preferred port as a preference and falling back to an ephemeral
   one (`GatewayServer._bind`), with the real port shown and encoded in the QR.
2. **Same-phone pairing could never happen by scanning.** MeshMore's QR is drawn
   by the other app on the same screen, so LDMUX's camera has nothing to point
   at. Added a type/paste field that routes by scheme (`ldmux://` versus
   `meshmore://` or a bare 32-hex token).
3. **The mesh screen drew under the status bar** and the "leave" control was
   squeezed into a one-character column. `safeDrawingPadding()` plus a weighted,
   wrapping label.
4. **The app would not install over the existing build**: the earlier APKs were
   signed with `~/.config/.android/debug.keystore`, not `~/.android`. Building
   with `ANDROID_USER_HOME=/home/ldeen/.config/.android` preserves user data.

### What the phone leg proved

- The loopback gateway runs on Android 16 and reports its real port:
  `Listening on 127.0.0.1:42989`.
- The full L2 exchange succeeds over the real network stack on the device.
  Verified from the host through `adb forward` with `GatewayClient`:
  `connected` → `hello ok caps=2 state=0` → `pair ok` →
  `bind refused: not_a_contact`.
- The refusal is correct, not a defect: the test used a random host radio key,
  which is not a contact of the attached radio. LDMUX showed
  `mesh · refused (re-pair, or add the host as a contact)`.
- Airtime used stayed at `0.0 s of 36 s`, so nothing was transmitted for a
  session that never bound.

### What it did not prove

- No successful `BIND`, so no session, no terminal profile, no over-the-air leg.
  Those need a second companion radio acting as the host, or a genuine contact
  entry for the host radio's key.
