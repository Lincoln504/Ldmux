# Release audit: every way this can fail

Started 2026-11-05. One page per aspect, each item a failure that can actually happen,
not a checklist of virtues. Status is one of:

- **FIXED** — a defect that was found, with the commit or file that closes it.
- **OPEN** — known, not yet closed.
- **OK** — checked and found correct, with what was checked.
- **UNKNOWN** — plausible, not yet investigated. Every UNKNOWN is a to-do.

This is the map. The design it audits is `docs/ARCHITECTURE.md`, and the decisions it
implements are in `docs/adr/`.

## 1. Build and release

| # | Failure | Status |
|---|---|---|
| 1.1 | No signing config, so nothing uploadable | FIXED `b44a8de` |
| 1.2 | `versionCode` a literal, so the second upload is rejected | FIXED `b44a8de` |
| 1.3 | No path produces a bundle | FIXED `b44a8de` |
| 1.4 | An unsigned release builds silently and is rejected at upload | FIXED — the task now fails instead |
| 1.5 | `applicationId` asserts a domain the project does not own | FIXED `4639f0c` (`io.ldmux.mobile`) |
| 1.6 | The bundle has never actually been built, so "it will build" is an assumption | FIXED 2026-11-05: a signed AAB was produced with a throwaway 4096-bit key, with the licence texts inside it, and the key was then deleted. The real upload key is the operator's to supply |
| 1.7 | No CI job builds the Android artifact or runs the Kotlin suite | OPEN by decision: CI runs the Rust gate on every branch, and the Android artifact is built locally |
| 1.8 | R8 is off; a release carries unused code | OK for 0.1, decide later |
| 1.9 | The native session library is rebuilt by a Gradle task, so a stale `.so` is possible | OK — `buildSessionAndroid` runs before packaging |
| 1.10 | The bundle shipped `armeabi-v7a` and `x86` splits from a dependency while our own libraries are 64-bit only, so a 32-bit device installs and dies with `UnsatisfiedLinkError` | FIXED 2026-11-06: `ndk { abiFilters }` declares `arm64-v8a` and `x86_64`, and `bundleRelease` now opens the produced bundle and refuses any other ABI or any missing library. The old guard read `jniLibs`, which cannot see what a dependency merged |

## 2. Install, upgrade, first run

| # | Failure | Status |
|---|---|---|
| 2.1 | Fresh install shows a blank screen under `unpaired`, reads as broken | FIXED `b44a8de` |
| 2.2 | The package id changed, so the previous install is a different app | FIXED by instruction — uninstall `dev.ldmux.mobile` first |
| 2.3 | The app on the phone is a 14 September build, so every fix since is invisible | FIXED — needs one install |
| 2.4 | Upgrading loses the pairing (token in app data) | OPEN by design — the package id changed for 0.1 (`dev.ldmux.mobile` → `io.ldmux.mobile`), so the previous install is a different app and nothing migrates. Pairing again is one scan, and the listing and first-run notice say so |
| 2.5 | A reviewer with no host, no tailnet and no radio cannot get past first run | OPEN by design — the listing must say what it needs |
| 2.6 | Backup restores a token onto another device | OK — `allowBackup=false`, rules exclude everything |

## 3. Permissions and platform

| # | Failure | Status |
|---|---|---|
| 3.1 | Bluetooth permission denied at the moment of use | OK — named state, not silent |
| 3.2 | `RECORD_AUDIO` requested at launch instead of on use | OK — requested on first mic press |
| 3.3 | `ACCESS_FINE_LOCATION` needed on API 28-30 for BLE scanning | OK — `maxSdkVersion=30`, declared |
| 3.4 | Denying notifications leaves a foreground service with no notification | UNKNOWN — check the service's behaviour on denial |
| 3.5 | The `specialUse` sub-type is read by a reviewer | OK — text present in the manifest |
| 3.6 | Font scale, RTL, dark mode, split screen break the layout | UNKNOWN |

## 4. Pairing and identity

| # | Failure | Status |
|---|---|---|
| 4.1 | QR contains host, port and token; scanned by Play services on device | OK — nothing leaves the device except to that host |
| 4.2 | A stale pairing points at a host that moved | OK — `RouteUnavailable` with a reason |
| 4.3 | Two phones pair to one host; who owns a shell | OK — ownership + consent, ADR-0010 |
| 4.4 | A pairing code is on screen while someone photographs it | OPEN — inherent; the token is bearer |
| 4.5 | The mesh pairing secret and PSK live in the same code as the token | OK — documented as sensitive |

## 5. Tailnet connection

| # | Failure | Status |
|---|---|---|
| 5.1 | Tailscale absent or down: "unreachable" cannot say why | OPEN — degrades the mode, no specific message |
| 5.2 | Silent link (NAT expiry, hung daemon) reported and never retried | FIXED `8fd09b6` |
| 5.3 | `connected` disagreed with the link label | FIXED `8fd09b6` |
| 5.4 | Network change not detected | OK — probe on wake, 4 s heartbeat |
| 5.5 | Auth refused: retried forever | OK — `Rejected` is terminal until re-pair |
| 5.6 | Reconnect storms cost the host CPU | UNKNOWN — see failure audit |

## 6. Radio connection

| # | Failure | Status |
|---|---|---|
| 6.1 | No radio selected | OK — `NeedsRadio`, named |
| 6.2 | Radio held by another app (MeshMore) | UNKNOWN — no "another app has the link" state; the connect just fails |
| 6.3 | Radio stops advertising while connected, so a scan finds nothing | UNKNOWN — the picker says "no radio found" with no cause |
| 6.4 | The host had no radio at all, so the phone handshook forever | FIXED 2026-11-05 — `LDMUX_MESH_SERIAL`, host owns the Heltec |
| 6.5 | Preset mismatch between the two ends | OPEN — the decision in `05-RADIO-TUPLE.md` |
| 6.6 | L3 handshake retried with fresh keys, bounded at 3 tries | OK |
| 6.7 | Radio unplugged mid-session | OK — transport state drives the loop |
| 6.8 | Public channel forces batch mode | OK — ADR-0026 |
| 6.9 | Multi-hop paths unimplemented; a relayed peer is unreachable | OPEN — named, not hidden |

## 7. The session and terminal data path

| # | Failure | Status |
|---|---|---|
| 7.1 | Records lost on a lost send result | FIXED `d00111c` (credit expiry) |
| 7.2 | A tracked command that is never answered wedges the link | FIXED |
| 7.3 | Reconnect backoff never returns to its floor | FIXED `d00111c` |
| 7.4 | A replayed record applied twice | FIXED — replay window (`crates/session`) |
| 7.5 | UTF-8 or an escape sequence straddling two records | OK — `Utf8Carry`, record framing |
| 7.6 | Record larger than the carrier floor: silently truncated | OK — sends are all-or-nothing, no truncation exists |
| 7.10 | Two 1024-record inbound queues dropped the oldest **silently**, after the ARQ had acknowledged it | FIXED `9ceea6d` — counted, logged at powers of two |
| 7.11 | A record more than 7 ahead of a gap is refused as `out_of_window` | OK — not loss; it stays unacked and is retransmitted, costing one round trip per gap record |
| 7.12 | `heartbeat_ms` is 0, so an idle session cannot tell a dead peer from an idle one | OPEN — the UI should say when nothing has been heard for N minutes |
| 7.13 | A `Close` is suppressed while the budget is spent, so an ended session lingers until the peer times out | OPEN — a close should ignore the budget, as a retransmission does |
| 7.7 | A resync never answered is never re-asked | FIXED `8fd09b6` |
| 7.8 | `seq 1 100`-class output batched into more messages than the budget | OK — pacer is open defect 22 |
| 7.9 | No pacer: a screen update leaves as a burst of up to eight records | OPEN — defect 22 |

## 8. The shell, keeper and host

| # | Failure | Status |
|---|---|---|
| 8.1 | Daemon restart loses sessions | OK — keeper owns them; verified live 2026-11-05 |
| 8.2 | Keeper restart ends every shell | OK by design — warned by the install script |
| 8.3 | A partial mesh config kills the daemon at startup | OPEN — exit 1 on `PSK` without a link; should warn and disable |
| 8.4 | A shell dies while a phone is attached | OK — "This shell has ended." then exit |
| 8.5 | Keeper wedged: daemon waits forever | UNKNOWN — see failure audit |
| 8.6 | Reaper fails after a panic | FIXED (keeper/daemon) |
| 8.7 | A shell that ignores SIGTERM | UNKNOWN — see failure audit |

## 9. UX states and error surfaces

| # | Failure | Status |
|---|---|---|
| 9.1 | `open` was a live button that did nothing | FIXED `b44a8de` |
| 9.2 | Control width below the touch target | FIXED `2b27c2e` |
| 9.3 | A resync that never answers says "resyncing" forever | FIXED `8fd09b6` |
| 9.11 | A shell created from the phone opens on a blank page until the next keypress | OPEN 2026-11-06, evidenced: the host has the prompt (`ldmux capture` prints it), and the log for the phone's page reads `attached`, `attached` again, `grid 46x21 (asked 46 x 21)` and then no frame. The same class as the radio defect fixed in `4fa5f66`: the surface rebuilds its engine for the measured grid, and the full screen the host sent for that grid is not painted. The radio route is fixed; the tailnet surface is not |
| 9.4 | The radio's failure reason is discarded | OPEN — `MeshRouteSection.kt:250` `runCatching {}` |
| 9.5 | A refused action's message vanishes after 4 s, not a live region | OPEN |
| 9.6 | The shell list survives a dead link with `open` armed | FIXED `b44a8de` |
| 9.7 | Settings opens as a screen and ends the session | OPEN — a decision, expensive on radio |
| 9.8 | Settings sub-flows opened at the end of the list, under `code` rather than under their own row | FIXED `ac87947`, **verified on the device 2026-11-05**: the picker renders at y 1080–1228 under the `radio` row at 949–1005, and pushes `code` from 1092 to 1468 |
| 9.12 | The scroll pad stepped three rows, so a page turn moved a seventh of a screen | FIXED `aca7220` — a page is the measured pane less two rows; measured 184→165→146 on a 21-row pane |
| 9.13 | A page-up clamped at the oldest *loaded* row and read as "jumped to the beginning" | FIXED `aca7220` — the tap pages history in first, then lands one page above the live bottom |
| 9.14 | Typing while reading scrollback sent the keys to a prompt off screen | FIXED `aca7220` — input returns the view to live, in both routes |
| 9.9 | No scroll affordance on settings | OPEN |
| 9.18 | Settings read as a two-column table: a fixed 92 dp label band, values in a second band, 6 dp everywhere so a heading and a row were the same distance apart | FIXED `33ecbbe` — a list of statements, uppercase section headers carrying the hierarchy, 8 dp rows against 20 dp between sections, one-character indent for a note that belongs to the row above. The mesh pairing became one titled row instead of three bare lines and a row called `code` |
| 9.10 | Text size needs 21 taps | OPEN |
| 9.11 | `AppHeader` always reads `ldmux` | OK — a convention, not a defect |
| 9.15 | A shell created headlessly from the CLI was drawn with the amber `no window` anomaly, which says something went wrong when nothing did | FIXED `c57f99a` — the daemon keeps `hadWindow`, backfilled at registry load; the label fires only for a window that went away. Verified on the device: no row says it any more, and the seven windowed sessions report `hadWindow=true` |
| 9.16 | A shell created from the phone must show a phone icon, offer send-to-desktop, and be the only kind the phone may close | OK — verified on the device: the row carries `opened on this phone`, `send to desktop` and `close shell`; `+ shell` creates one end to end |
| 9.17 | The scroll pad's page size and the read-then-type behaviour must hold in all three modes | FIXED in both routes (`aca7220`); verified on the device for tailnet only, since the mesh route needs a paired radio. The mesh route's history is local, so a page turn repaints without a round trip and needs no refresh |

## 10. Security and privacy

| # | Failure | Status |
|---|---|---|
| 10.1 | Token at rest on the phone, in plain preferences, while the mesh keys beside it were Keystore-encrypted | FIXED `6d4e8d2` — same `KeystoreBlob`, one-time migration |
| 10.7 | Token embedded in `connectionKey` and used as a Compose rebuild key | OPEN — anything that logs that key leaks the bearer token |
| 10.8 | Token accepted in the query string as well as the header | OPEN — the probe uses it; URLs travel further than headers |
| 10.9 | The handshake's filter tag is bespoke crypto that its own comment says wants review | OPEN — the only custom crypto in the tree |
| 10.10 | The Keystore path has never run on a device | OPEN — one manual round trip closes it |
| 10.11 | The JNI entry points still carried the old package, so every native call failed | FIXED `6d4e8d2` — caught by `MeshSessionAbiTest`, 11 tests |
| 10.2 | Cleartext permitted where the config says it is not | OK — restricted to tailnet/loopback by code |
| 10.3 | A terminal program writes the clipboard without consent | OK — `ldmux clipboard-write on|off`, off by default |
| 10.4 | The gateway token sat in `/tmp` | FIXED 2026-11-05 — gateway removed, file deleted |
| 10.5 | Timing instrumentation on in production | FIXED 2026-11-05 — env cleaned |
| 10.6 | No telemetry anywhere | OK — dependency audit finds no analytics SDK |

## 11. Resource limits and failure injection

Audited 2026-11-05. The picture is better than expected: almost everything is bounded.

| # | Failure | Status |
|---|---|---|
| 11.1 | State file written non-atomically or non-durably | OK — tmp + `sync_all` + rename + dir fsync |
| 11.2 | A session's output grows without limit | OK — 4 MiB per session, drains in chunks |
| 11.3 | Scrollback grows without limit | OK — 10 000 lines / 40 MiB, both overridable, never silently zero |
| 11.4 | Wire-side queues unbounded | OK — input 16 KiB, keys 32, pending 1 MiB, snapshot 4 MiB, backlog 64 KiB, JSON 16 MiB, connections 64 |
| 11.5 | Mesh queues unbounded | OK — inbound 1024, outbound 8 KiB, pending 8; the phone's inbox 128 |
| 11.6 | A wedged keeper blocks forever | OK — every keeper call has an 8 s timeout, and a connect failure respawns once |
| 11.7 | A shell that ignores SIGTERM survives | OK — SIGTERM, grace, then SIGKILL, group and leader |
| 11.8 | Disk full during a registry or token write | OK for those two (logged, atomic); the pairing write path is UNKNOWN |
| 11.9 | A client that reconnects every second costs the host CPU | UNKNOWN — no per-peer rate limit found; the 64-connection cap bounds memory, not work |
| 11.10 | Clock jump or host suspend breaks a deadline | UNKNOWN |

## 12. Multi-device and ownership

| # | Failure | Status |
|---|---|---|
| 12.1 | Two devices hold one shell | OK — ADR-0010, consented takeover |
| 12.2 | A dead window holds a shell forever | OK — `RECONNECT_GRACE_MS` bounds it |
| 12.3 | `mirror --rw` on a free shell takes the claim unrecorded, so a second live window can displace it | OPEN — desktop audit |
| 12.4 | Closing a desk window kills a shell the phone is typing into | FIXED 2026-10 |

## 13. Observability

| # | Failure | Status |
|---|---|---|
| 13.1 | A silent radio and a quiet mesh look identical | OK — `mesh status` names the state |
| 13.2 | The startup line named an empty address for a serial link | FIXED `c70d516` |
| 13.3 | No crash reporting, so a store crash is invisible | OK by policy — the cost of collecting nothing |

## 14. Release operations

| # | Failure | Status |
|---|---|---|
| 14.1 | "Verified on a device" referred to binaries older than HEAD | FIXED 2026-11-05 — deployed build is current |
| 14.2 | A stale install keeps presenting a fixed bug | OPEN — nothing pins the spawned binary to the running one |
| 14.3 | The heavy test suite fails under load | MITIGATED — heavy tests serialised, workloads right-sized; still load-sensitive |
| 14.4 | No tag, ever | OPEN |
| 14.5 | `main` 220 commits behind the work | OPEN |

## How to use this

Work top-down by status: **OPEN** and **UNKNOWN** are the release's actual to-do list. A
section with every row FIXED or OK is done and should not be re-litigated without new
evidence.

## Appendix A: store submission

Identity is `io.ldmux.mobile`, settled when the project took `ldmux.io`. Play does not verify
the domain, but the id can never change after the first upload, so the reverse-domain id follows
the domain the project owns. Namespace, manifest and applicationId agree.

What the console needs that does not exist yet: a hosted privacy policy URL (the project owns
`ldmux.io`, so `https://ldmux.io/privacy` is the natural home), a feature graphic at 1024 by
500, phone screenshots, and a 512 by 512 icon. The launcher icon in `res/` is an in-app icon,
not a store asset.

### Privacy policy text

> ldmux collects nothing.
>
> The app connects only to a machine you name yourself: your own computer, reached over a
> private tailnet or over a radio link. There is no account, no sign-in, and no server run by
> the developer.
>
> - No analytics, no crash reporting, no telemetry. The app contains no such SDK, so there is
>   nothing to report and nothing to opt out of.
> - No data leaves your device except to your own machine. What you type is sent to the host
>   you paired with. It is not sent anywhere else.
> - The pairing token is stored on the device and excluded from backup, so the system cannot
>   move it off the phone.
> - The camera is not used. Pairing scans a QR code through Google's Play services code
>   scanner, which decodes on the device and sends nothing to the developer.
> - Speech is optional and separate. Dictation uses Android's own speech recogniser, and if the
>   device's recogniser is cloud-based, audio may go to that provider. That is the device's
>   setting, not something this app controls, and the app says so when it happens.

### Data safety form

| Question | Answer | Why |
|---|---|---|
| Does the app collect or share user data | No | no analytics, no crash reporting, no account, no developer server |
| Is data encrypted in transit | Not applicable | the only traffic is to a host the user chose, and the developer is not a party to it |
| Can users request deletion | Not applicable | the developer holds nothing |

The answer is defensible because it is true of the code: an audit of the dependency list finds
no analytics or crash SDK.

### Permission justifications

| Permission | Declaration |
|---|---|
| `BLUETOOTH_SCAN` with `neverForLocation` | finds the phone's own MeshCore radio, never used to derive location, so no location access is requested on Android 12 or later |
| `BLUETOOTH_CONNECT` | connects to that radio |
| `BLUETOOTH`, `BLUETOOTH_ADMIN`, `ACCESS_FINE_LOCATION` at `maxSdkVersion=30` | the same radio on Android 8.0 to 11, where a scan requires location permission |
| `RECORD_AUDIO` | dictation, turning speech into text at the prompt, requested when the microphone is first used |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_SPECIAL_USE` | keeps a connection alive for a 45 second grace after the user switches away, so a quick switch does not drop the open shell |
| `POST_NOTIFICATIONS` | the service's own notification, and denial is not fatal |
| `WAKE_LOCK` | partial, bounded, released with the service |
| `INTERNET`, `ACCESS_NETWORK_STATE` | the connection to the host |

### Listing copy

Short description: a terminal for a shell on your own Linux machine, over tailnet or radio.

Full description: ldmux is a terminal for a shell that runs on a Linux machine you own, not on
the phone. The shell lives on the host in a keeper process that outlives every client, so
putting the phone down, losing the network or restarting the app leaves the shell and its
scrollback intact. It is a terminal, so `vim`, `ssh`, `top` and anything else your shell can run
work, with the alternate screen, mouse reporting, bracketed paste and scrollback intact. It is
reachable over your own tailnet, which needs no server of ours because there is no server of
ours, or over a LoRa radio when there is no network at all. The same shell can be on the phone
and in a desk window at once. Nothing is collected. What you need is a Linux machine with a
systemd user session and the daemon installed, Tailscale for the network route, and two
MeshCore companion radios for the radio route.

Content rating: no user-generated content is hosted, no ads, no purchases, no gambling, no
violence. The app is a development tool for adults, and the questionnaire answers follow from
that.

## Appendix B: open decisions

None of these are defects. Each is a decision that changes what ships.

1. The radio tuple the app compiles in. The default is 910.525 MHz, 500 kHz, SF7, CR5, and the
   host radio here runs 914.25, 500, SF8, CR5, so a fresh install cannot reach this host without
   the user setting the tuple by hand. The options are: make the compiled default match the
   deployment, make the deployment match the default, ship no default and adopt what the radio
   reports, or adopt the public MeshCore preset at 910.525 and 62.5 kHz, which costs about nine
   times the airtime. The recommendation on record is to adopt what the radio reports and treat
   the physical tuple as a deployment choice.
2. Whether the host radio is a shipped feature. The daemon owns it when `LDMUX_MESH_SERIAL` is
   set, and nothing installs or provisions it, so today the route works for an operator who
   reads `docs/ARCHITECTURE.md` section 11 and not for a new user.
3. The upload keystore and the store assets, which are the operator's to supply.

## Appendix C: known unfinished

- A desk window displaced by a mesh claim is not covered by a test. The daemon sends the notice
  and the same message is proven for a WebSocket claim, but the mesh variant has no test.
- A shell created from a phone is recorded with `createdBy` set to `phone`, and rows keep their
  actions across an app restart because of it. There is no test that asserts that round trip.
- Dictation depends on the device's speech recogniser, which may be cloud-based. The app says
  so, and nothing here can change it.
- Multi-hop path recovery on the radio route is not implemented.
