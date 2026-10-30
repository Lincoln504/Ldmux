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

## Result: the two-radio leg, 2026-10-29

Second radio: Seeed Wio Tracker L1 Pro, flashed from this tree to
`WioTrackerL1_companion_radio_usb` (PlatformIO upload, nrfutil, 48 s), then
configured over serial to 910.525 MHz / 62.5 kHz / SF7 / CR5 / 22 dBm — it had
been 917.0 / 125.0 / SF11. Its key is `26f6071f…`, the phone's radio is
`615f431f…` (`HELTEC-BASE`).

**Order that worked, given two USB-C ports and three devices:** the Heltec has no
battery so it must stay tethered, the Seeed and the phone must be swapped. Pair
and bind while the phone is attached; the actual over-the-air exchange needs the
Seeed attached and does not need the phone attached at all.

### What bound

- Desktop side, `meshcore_gateway` on `/dev/ttyACM1` + `ldmuxd` with
  `LDMUX_MESH_PEER` = phone radio key: `state ready`, capacity 172 B per record,
  58 records left this hour, radio connected.
- Phone side: HELLO ok, PAIR ok, then `BIND_ERR` / `not_a_contact`.
- The radios themselves were fine: the Seeed listed `HELTEC-BASE pathLen 0`, and
  the Heltec listed `WIOld` at SNR 11.0 / RSSI −12 after five forced flood
  adverts (`tool/advertise.dart`).

### Why it stops

`BIND` requires the peer to be a **contact** on the phone's radio, not merely a
node it has heard. MeshMore offers `Add to device contacts`, and on this radio the
add did not take — probably a full table (350/350 measured earlier), but the app
never surfaced the radio's reply, so that stays a hypothesis. The screen that
could free slots (`Radio contacts`) does not respond to taps, so the recovery
path is unreachable from the UI. See `meshmore-sns-architecture-audit.md`
(D1, D2) for the evidence.

### Consequence for this runbook

Until a free contact slot exists and the peer is added as a device contact, a
two-radio validation cannot get past BIND. Check `Radio contacts` first and
confirm the count against `max contacts: 350` in Device configuration before
blaming the mesh.

## First over-the-air session, end to end (2026-10-29)

Path: LDMUX on the phone → MeshMore's gateway on the phone (BLE to the Heltec V4)
→ LoRa 910.525 / 62.5 / SF7 / CR5 → Seeed Wio Tracker L1 Pro (USB) → host
`meshmore_gateway` → `ldmuxd` (test instance, own state dir and socket).

Result, from `ldmux mesh status`:

```
state    ready
capacity 172 B per record
budget   56 records left this hour
radio    connected
airtime  1.6 s this hour (gateway's own estimate)
session  established
  phone key  0e4e32f0… (pinned)
  handshakes 1 accepted, 0 refused
  records    6 sent, 1 retransmitted, 0 dropped, 3 pending
  received   18 bytes
```

Host configuration, with no public key copied by hand:

```
meshmore_gateway --port /dev/ttyACM3 --listen 41731 \
                 --peer-name HELTEC-BASE --state /tmp/gw.json
LDMUX_MESH_STATE=/tmp/gw.json \
LDMUX_MESH_PSK=<64 hex> ldmuxd
```

`--peer-name` resolved `HELTEC-BASE → 615f431f…` from the radio's own contact
table. The state file then held the loopback address, the token, the daemon's radio
key and the peer key, and `ldmuxd` read all four from it. Two settings in total.

What had to be fixed before it worked, each found on hardware:

- **A stale stored gateway port.** The gateway falls back to an ephemeral port when
  its preferred one is taken, so a stored pairing can point at nothing while
  MeshMore is healthy. The failure line now names the address and says re-pair
  instead of accusing MeshMore of not running.
- **An old session key.** The phone still held `mk=9f8d6852…` after the daemon's
  state dir was recreated; the host logged `handshakes 0 accepted, 1 refused`
  because the responder's key had changed. Re-pairing from the live daemon fixed it,
  and the refusal was visible in `mesh status` rather than silent.
- **`leave` rewriting the saved route.** Leaving the mesh screen set the route back
  to tailnet, so the next launch showed "unreachable" and the mesh route looked
  lost. Leaving is a view change now.
- **Settings clipped.** The dialog's column was unbounded and drew under the button
  row; the scroll area is bounded now, and the two scan labels no longer wrap and clip.

## Second run: one UI for both routes, and keys by name (2026-10-30)

Path unchanged: LDMUX on the phone → MeshMore (BLE, Heltec V4) → 910.525 / 62.5 / SF7
→ Seeed L1 (USB) → host gateway → `ldmuxd`.

What the screen is now. The mesh route draws the tailnet route's own chrome: `AppHeader`
(name, one status line, settings square), the shared `PortraitBar` (shell header with
`n / m shells` and its arrows, Esc/Tab/Home/End/PgUp/PgDn, sticky Ctrl/Alt/Shift, C/c,
paste, Enter, arrow pad, keyboard toggle), and a block cursor from the host's `Commit`.
`SettingsDialog` is drawn once by `App` for every route, so pairing no longer requires
leaving the mesh screen. The differences are one subtitle word — `live (reduced)` — the
host's own `Not on this link:` line, the pending-typing line and the hour's record count.

Measured, by driving the phone over wireless adb (`/tmp/ui.py`, accessibility tree only):

| check | result |
|---|---|
| typed command reaches the host | `echo MESHTEXT_TWO > /tmp/meshtest.txt` → file on the host, contents `MESHTEXT_TWO` |
| DECCKM on | `tput smkx; cat -v` then ↑ → `^[OA` |
| DECCKM off | `tput rmkx; cat -v` then ↑ → `^[[A` |
| interrupt | `C/c` → `^C` at the pane |
| switch shells | `▶` → header `2 / 2 shells`, new screen |
| list shells | menu → shells → both host shells listed by id |
| settings | gear on the mesh screen opens the same dialog, mesh section included |
| budget | `43` of 58 records left after the run |

The DECCKM pair is the point: the phone sends the name `Up`, never a byte sequence, and
the host encodes it in the pane's own mode through the daemon's single encoder. A phone
that kept its own key table could only ever have produced one of those two rows.

### Driving the phone without a USB cable

Wireless debugging drops its advertised port when the phone sleeps and comes back on a new
one. Read it from `adb mdns services` (`adb-<serial>-XXXX._adb-tls-connect._tcp  ip:port`)
and `adb connect` to that address. A stale `adb connect` survives a screen that has moved,
so a tap that "does nothing" is usually a stale connection, not a stale tap.

For text input, `adb shell input text` is parsed by the *device's* shell: `%s` is a space
and an unquoted `>` becomes a redirect. Wrap the argument in single quotes for the remote
shell.

## Third run: latency (2026-10-30)

The device reported "long pause, blank, then the response" with the link at 62.5 kHz. The
audit and the fixes, in order of size:

| change | commit | effect |
|---|---|---|
| bridge tick 1000 → 50 ms | `9624c73` | up to 1 s per direction |
| session ack delay 3000 → 150 ms | `9624c73` | up to 3 s of silence per pure ack |
| retransmit 20 s → 2 s (cap 30 s) | `9624c73` | a lost record no longer stalls a screen |
| window 4 → 8 | `9624c73` | a repaint pipelines instead of stalling |
| phone idle flush 700 → 100 ms (hold 200) | `d3b2797`, `ae2dc64` | ~90 ms per non-Enter keystroke, and a bounded cadence |
| `Rows` + `Commit` in one record | `398755d` | one frame per echo instead of two |
| radio SF7 → SF5 at 500 kHz | (config) | ~48 ms per frame instead of ~70 |

Aks turn out not to be on the critical path: every data record carries a cumulative ack, a
pure ack is only emitted when nothing else is queued, and the sender never waits for an ack
unless the window is full. Removing acks would buy nothing and cost the only delivery
guarantee.

Still open: a keystroke-level timing harness. The native terminal surface exposes no text to
the accessibility tree, so `uiautomator dump` cannot time an echo; timing must come from
host-side timestamps or screenshot deltas.

## The stall that felt like "my keys only appear when I press another key" (2026-10-30)

`Bridge::tick` refused to look at the screen while **any** record was unacked:

```rust
if self.link.pending() > 0 { break; }
```

so an echo could not even be *built* until the previous record's ack came back, and an ack on
this link only rides on the phone's next record or its pure-ack timer. Measured with the
`LDMUX_MESH_TIMING=1` probes: input arrived at 38529 ms, the echo was queued at 55080 ms, a
**sixteen-second** gap with the radio idle.

The gate is the session window now (`PENDING_LIMIT = 8`): the pipe stays full, the queue cannot
grow past what the link carries. Same measurement after the change: input at 31048 ms, echo
queued at 31050 ms.

Two bridge tests pinned the old policy and now set `pending` to the window, which is what "busy"
means. The timing probes stay in the daemon behind `LDMUX_MESH_TIMING=1`: the phone's terminal is
a native surface whose text is invisible to the accessibility tree, so host-side timestamps are
the only honest measurement of a keystroke's turnaround.

## The pairing that only worked from inside the app (2026-10-30)

Reported by the user: pairing the radio from the phone's own Bluetooth menu did not work, pairing
it from inside MeshMore did. Two states for one pairing.

The phone's own bond table (`dumpsys bluetooth_manager`) says why. Both of these are ordinary
`BOND_STATE_BONDED` entries, and they are not the same thing at all:

```
shim::record 003  xx:xx:xx:xx:c5:3d  name:"MeshCore-HELTEC-BASE"
    le_linkkey_known:T  ble_enc_key_size:0   le_authenticated:F  le_encrypted:F
shim::record 006  xx:xx:xx:xx:5a:9a  name:"MeshCore-WIOld"
    le_linkkey_known:T  ble_enc_key_size:16  le_authenticated:T  le_encrypted:T
```

Record 003 is a bond record with **no key material**. The radio's characteristics are MITM-only
(`bleuart.setPermission(SECMODE_ENC_WITH_MITM, …)`, `SerialBLEInterface.cpp:183`), so a keyless
record discovers services and then fails the first encrypted operation. `bondState` is
`bonded` either way, so the app trusted it.

The second half of the divergence is that the two menus wrote different stores. The Bluetooth
menu writes Android's bond. MeshMore's own pairing writes `mm.paired.id` in prefs. An OS bond
with an empty prefs record left `autoConnectIfPaired()` returning early, so the app read "No
device paired" for a radio the phone was already bonded to.

Fixed in MeshMore `18705e5`: the OS bond is adopted as the app's pairing at startup (preferring
the radio the app used last when the phone holds several), `autoConnect` tries a bonded radio
before scanning, a bond the radio cannot use is removed and re-made in place, and "forget the
radio" now removes the OS bond too so the next start cannot adopt it back. The repair only runs
while the radio is still linked — a link that dropped mid-open is not a bond problem, and
removing a good bond over it would cost the user a pairing.

`flutter test`: 1038 passing, 5 of them the new adoption rule.

Not yet verified on the device: the phone was off USB when the fix landed. The
expected reading is that pairing from the Bluetooth menu and pairing from the app leave the app
in the same state, and that a `ble_enc_key_size:0` bond repairs itself instead of reporting a
stale pairing.
