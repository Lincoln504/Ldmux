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
| 1.7 | No CI job builds the Android artifact or runs the Kotlin suite | CLOSED: there is no CI (`.github/workflows` removed at the operator's instruction, 2026-11-06), so the Kotlin suite and the release build are local commands |
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
| 3.1 | Bluetooth permission denied at the moment of use | FIXED 2026-11-06 — a named state with a verb: "allow Bluetooth", and the app's settings page once the system stops showing the dialog (the only way back from a permanent denial) |
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
| 5.1 | Tailscale absent or down: "unreachable" cannot say why | PARTLY — the link's own failure sentence is now shown on the radios and the tailnet probe still collapses refused/timeout/no-route into one word |
| 5.2 | Silent link (NAT expiry, hung daemon) reported and never retried | FIXED `8fd09b6` |
| 5.3 | `connected` disagreed with the link label | FIXED `8fd09b6` |
| 5.4 | Network change not detected | OK — probe on wake, 4 s heartbeat |
| 5.5 | Auth refused: retried forever | OK — `Rejected` is terminal until re-pair |
| 5.6 | Reconnect storms cost the host CPU | PARTLY — an unwatched session now polls at 4 Hz instead of 100 Hz and an attach wakes it; there is still no per-peer rate limit |

## 6. Radio connection

| # | Failure | Status |
|---|---|---|
| 6.1 | No radio selected | OK — `NeedsRadio`, named |
| 6.2 | Radio held by another app (MeshMore) | PARTLY — the picker's empty state names this cause and the link's failure sentence is shown, but there is still no state that detects it |
| 6.3 | Radio stops advertising while connected, so a scan finds nothing | PARTLY — the empty state names the two causes (powered off, held by another app or phone); a bonded-device fallback still does not exist |
| 6.4 | The host had no radio at all, so the phone handshook forever | FIXED 2026-11-05 — `LDMUX_MESH_SERIAL`, host owns the Heltec |
| 6.5 | Preset mismatch between the two ends | FIXED 2026-11-06 — the pairing code carries the host radio's tuple (`mp=`), the app shows a `desktop's radio` row when it differs and one press matches it, applying only the four channel fields so power and path width stay local |
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
| 8.3 | A partial mesh config kills the daemon at startup | FIXED — a malformed mesh setting now warns, disables the mesh link and leaves the daemon and every shell running (`bin/ldmuxd.rs`) |
| 8.4 | A shell dies while a phone is attached | OK — "This shell has ended." then exit |
| 8.5 | Keeper wedged: daemon waits forever | FIXED 2026-11-06 — the replacement now works under systemd (`last_pid` is 0 there, so the old code skipped the kill and returned Ok), restarts the unit, falls back to the detached path, and certifies with a `List` rather than a socket connect |
| 8.6 | Reaper fails after a panic | FIXED (keeper/daemon) |
| 8.7 | A shell that ignores SIGTERM | OK — SIGTERM, a grace, then SIGKILL to the group and the leader |

## 9. UX states and error surfaces

| # | Failure | Status |
|---|---|---|
| 9.1 | `open` was a live button that did nothing | FIXED `b44a8de` |
| 9.2 | Control width below the touch target | FIXED `2b27c2e` |
| 9.3 | A resync that never answers says "resyncing" forever | FIXED `8fd09b6` |
| 9.11 | A shell created from the phone appeared on a blank page in one adb-driven run on the tailnet | NOT A DEFECT in normal use: the operator reports the prompt appears on creation, and the host has it. The log for the run showed `attached` twice, then `grid 46x21 (asked 46 x 21)`, then no frame — which is a consistency question worth a test (the surface rebuilds its engine for the measured grid), not a user-visible fault |
| 9.4 | The radio's failure reason is discarded | FIXED 2026-11-06 — `RadioLink.lastProblem` carries the sentence the link built to the picker and to `LinkDown`; `match` also reports a refused write |
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
| 10.7 | Token embedded in `connectionKey` and used as a Compose rebuild key | NOT A DEFECT — it is only a `remember` key; nothing logs it and `Endpoint.toString()` omits it. Recorded so the row is not re-opened |
| 10.8 | Token accepted in the query string as well as the header | FIXED 2026-11-06 — the query string is no longer a credential; the header is the only path, and the probe and the upgrade read the same one |
| 10.9 | The handshake's filter tag is bespoke crypto that its own comment says wants review | OPEN — the only custom crypto in the tree |
| 10.10 | The Keystore path has never run on a device | FIXED — verified on the device 2026-11-06: the stored token is `IV(12) + 64-byte token + tag(16)` in `shared_prefs`, no plaintext key remains, and the app is connected |
| 10.11 | The JNI entry points still carried the old package, so every native call failed | FIXED `6d4e8d2` — caught by `MeshSessionAbiTest`, 11 tests |
| 10.2 | Cleartext permitted where the config says it is not | OK — restricted to tailnet/loopback by code |
| 10.3 | A terminal program writes the clipboard without consent | OK — `ldmux clipboard-write on`/`off`, off by default |
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
| 12.3 | `mirror --rw` on a free shell takes the claim unrecorded, so a second live window can displace it | MOOT — `ldmux mirror` is gone; the read-only view is `ldmux attach <id> --readonly`, which sends no `hello` and takes no claim |
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
| 14.2 | A stale install keeps presenting a fixed bug | FIXED 2026-11-06 — a build id in every binary, `--version`, the startup line and an additive `build` field on `ping`, so "which build is running" is one command |
| 14.3 | The heavy test suite fails under load | MITIGATED — heavy tests serialised, workloads right-sized; still load-sensitive |
| 14.4 | No tag, ever | OPEN |
| 14.5 | `main` 296 commits behind the work | OPEN — a merge is its own review |

## 15. The 2026-11-06 audit pass

Six independent audits (phone radio, host radio, onboarding, ongoing UX, host
lifecycle, security) over the whole tree, with the device and the deployed daemon
in reach. This is what they found that this document did not have a row for, and
what closed it.

| # | Failure | Status |
|---|---|---|
| 15.1 | The radio pairing dies at the first reboot: the mesh static key, the pinned phone key and the PSK lived in `%t/ldmux`, a tmpfs | FIXED — a persist directory (`LDMUX_PERSIST_DIR`), set in the unit, with a one-time adoption from the old place (ADR-0027) |
| 15.2 | A serial port that stops draining hangs the radio thread and then the supervisor; the link stays `ready` for ever with no error | FIXED — the write is bounded at 2 s and the teardown detaches instead of joining |
| 15.3 | The receive side is skipped while output is queued, so `SEND_RESULT`s wait and 75 s later the link blames the carriage | FIXED — one biased `select!`, receiving unconditional |
| 15.4 | A desk takeover does not stop the phone writing into the same pty, and neither side is told | FIXED — `Input`/`Key` ask the in-memory claim table and refuse a non-holder with `Closed { HELD }` |
| 15.5 | A wedged keeper is never replaced under systemd: `last_pid` is 0 in unit mode, so "replacing it" replaced nothing | FIXED — restart the unit, fall back, certify with a `List` |
| 15.6 | Every session ever viewed keeps a 100 Hz poller until the shell exits (~0.8 % of a core each, both sides) | FIXED — 250 ms unwatched, 10 ms watched, and `register` wakes the waiter |
| 15.7 | No binary can say which build it is; the daemon and eight desk windows ran different installs invisibly | FIXED — build id, `--version`, startup line, additive `ping.build` |
| 15.8 | The bearer token is accepted in the query string, which travels further than a header | FIXED — header only; `scripts/measure_latency.ts` used the query form and is deleted |
| 15.9 | The first-run scanner throws its outcome away: a wrong code does nothing and a mesh-only code leaves the screen identical | FIXED — every outcome has a sentence and a verb |
| 15.10 | A phone paired with `ldmux code --mesh-only` can never unpair | FIXED — `unpair` follows either half |
| 15.11 | No pairing path on a phone without Play services (no paste field anywhere) | FIXED — paste a link, same parser and same fingerprint |
| 15.12 | A denied Bluetooth permission has no recovery, and the second denial is permanent | FIXED — button on the picker and on `NeedsPermission`; opens the app's settings once the dialog is gone |
| 15.13 | A first bond can outlast the 30 s connect deadline, and the failure blames `APP_START` | FIXED — 75 s while unbonded, and the line says to accept the pairing request |
| 15.14 | Every connect failure collapses into one sentence; six causes look identical | FIXED — `RadioLink.lastProblem` reaches the picker and `LinkDown` |
| 15.15 | The airtime meter is per-link and in memory, so leaving the page refills the legal hour | FIXED — persisted per radio address with a wall clock |
| 15.16 | An untracked battery request shares the one answer window with a tracked query, so `isIdle()` can lie during the preset write | FIXED — sent after the contact settles |
| 15.17 | Radio live mode shows a dead link as live for up to 20 s | FIXED — "link quiet · N s" after 10 s |
| 15.18 | The mesh key bars send keys without returning the view to live (the tailnet fix, missing here) | FIXED |
| 15.19 | The pane size is re-asserted after the shell was let go | FIXED — skipped while the list is showing |
| 15.20 | `match` on the desktop's radio adopts the host's power and path width, which are local choices (22 dBm in a 14 dBm band) | FIXED — only the four channel fields, and a refused write is reported |
| 15.21 | §9's airtime arithmetic and Appendix B's tuple claim were stale: both described 914.25 MHz/SF8 and a deployment that could not be reached | FIXED — the deployed tuple is the app's default, and §9 is computed from the tuple the radio reports |
| 15.22 | The only "more" link in the app answered HTTP 410 | FIXED — removed; the empty state carries the setup copy |
| 15.23 | `ldmux mesh --json`, `peer 00000000` for a serial link, "gateway" wording in mesh status, dead rows in `list` | FIXED |
| 15.24 | Twelve doc references pointed at files deleted in `731970f`, including `AGENTS.md`'s own "read `docs/SPEC.md`" and the units' headers | FIXED — `ARCHITECTURE.md`, `RELEASE-AUDIT.md` and `adr/` are the set, and every reference names one of them |
| 15.25 | Six leaked test keepers held PTYs since 29 Oct, one from a nix shell | CLEANED — killed by pid; `restart-keeper.sh` still picks the first match by name, which is a real hazard when more than one keeper exists |

| 15.26 | The CLI opened terminal windows (`window`, `live`, `mirror`) instead of running in the one you have | FIXED — `ldmux attach <id>` is the desk client in your terminal, `ldmux new` creates without attaching, and the four window verbs are gone |
| 15.27 | A spawned window opened in the *system's* configured terminal, not the one the user runs (gsettings answered `x-terminal-emulator`, Debian resolves that to xfce4-terminal, while the user had ten alacritty windows) | FIXED — an emulator already running for this user now outranks the desktop setting, and the first window this chain produced on this machine is the reason the order changed |
| 15.28 | The keybinding wrapper carried a second copy of the terminal selection rules in shell, so the two drifted | FIXED — `ldmux terminal` is the one resolver and `ldmux terminal --exec <command>` applies the emulator's template; the wrapper asks it and opens the terminal itself |
| 15.29 | `LDMUX_TERMINAL`/`LDMUX_TERMINAL_CMD` were bespoke variables no new user would find | FIXED — removed: `$TERMINAL`, `xdg-terminal-exec` and the desktop's own setting are the conventions; an emulator needing unusual flags is a wrapper script named by `$TERMINAL` |
| 15.30 | Every spawned window lived in `ldmux.service`'s cgroup: the unit's memory figure was 20x the daemon's, each restart logged "Found left-over process … unclean termination", and a stop left the windows behind | FIXED — a transient scope under `ldmux-windows.slice`, held on a pipe until the move is visible, with a silent fallback when there is no user bus |
| 15.31 | The loopback gateway transport was still in the tree: `GatewayCarrier`, its client, its frame/message types, `LDMUX_MESH_ADDR`/`TOKEN`/`PEER` and `Budget::Reported` | FIXED — deleted: the shipped topology is one radio the daemon owns; what remains in `crates/mesh` is the carrier seam, the record limits and the two reports a carrier hands upward |
| 15.32 | Deleting the gateway removed the three integration tests that drove the L3 path through it, leaving the handshake and the record path covered only by unit tests | FIXED — an in-crate test drives `session()` over a fake `Carrier`: handshake, one record each way, asserted on the runtime's inbound queue and the phone's session |
| 15.33 | The keeper's SIGPIPE test waited for the string `SigIgn:` and then parsed a line that had not arrived, so it failed under load | FIXED — `peek_until` waits for a complete line, and the launcher tests name their own stub terminal instead of a real emulator (a real one opened a window and waited for ever) |

| 15.34 | A second daemon given only `LDMUX_STATE_DIR` (a test, a scratch instance) inherited the deployment's persist directory, read the real PSK and grabbed the host radio out from under the daemon that owns it | FIXED — no `LDMUX_PERSIST_DIR` plus a `LDMUX_STATE_DIR` means `<state>/persist`; the deployed unit sets the persist directory explicitly, so a scratch run keeps its own credentials |

| 15.35 | The "configured terminal" step read a **GNOME** `gsettings` schema on an **XFCE/i3** session, and that key answered the Debian alternative `x-terminal-emulator`, resolved to `xfce4-terminal.wrapper` — so a spawned window opened in a terminal the user does not use, over ten alacritty windows | FIXED — each desktop's configuration is read only on that desktop (GNOME/Cinnamon/MATE `gsettings`, KDE `kdeglobals`, XFCE `xfconf`/`helpers.rc`); a bare window manager is asked nothing; and an indirection is followed through the symlink and the `.wrapper` shim to the program that actually opens |
| 15.36 | There was no way for a user to say which terminal they use: `$TERMINAL` set in a shell never reaches the daemon, and the desktop's setting may name nothing (this host's XFCE `helpers.rc` had no `TerminalEmulator` at all) | FIXED — `ldmux terminal set <program>` writes `$XDG_CONFIG_HOME/ldmux/terminal` and outranks every probe; `clear` removes it; `--explain` prints which step answered. On this host the missing XFCE setting was also set (`TerminalEmulator=alacritty`), so the desktop itself now agrees |

## 16. The tailnet route, audited separately (2026-11-06)

The radios got most of the attention in this pass; this is the same audit for the
route that is used every day. Verdict first: **the protocol and the payload are right for
the medium, and the only thing worth changing is one measurable inefficiency.**

| Aspect | Finding |
|---|---|
| Encoding | JSON control messages and binary output frames. **Protobuf would be a net loss**: the hot path (terminal output) already crosses as a raw binary frame with an 8-byte header, control messages are low-rate and small (an `input` message is ~60 B), and end-to-end `permessage-deflate` at level 3 is already negotiated on both ends (7.1x on repetitive output, ADR-0019). A binary control encoding would save tens of bytes per keystroke — below RTT noise on a tailnet — and cost the additive-wire property and the ability to read the protocol in a log |
| Snapshot payload | A JSON envelope around the pane text, measured 45.5 % larger than the raw text (4.2 KB → 6.2 KB on a live `RCNT` shell) because of escaping, and sent once per attach or resync, not per frame. Deflate absorbs most of it. Correct as it is |
| Latency | `TCP_NODELAY` set at accept, and keepalive probes at 15 s idle / 5 s interval / 3 misses. Keystroke echo is one message; the daemon drains the keeper at 10 ms while watched, and the phone paces frames at RTT/2 bounded to 33-150 ms |
| Auth | Bearer token from the `Authorization` header only (verified live: no token 401, query token 401, header without upgrade 400, unknown path 404), constant-time comparison, tailnet-or-loopback peer check before the token, 64-connection cap enforced **pre-auth** with 503 |
| Frame limits | Frame, message and inflate expansion all capped; an unknown-length deflate cannot expand past the message cap; a bad upgrade is a 400, not a dropped connection |
| Flow control | One bounded queue per subscriber; overflow drops the subscriber and forces a resync rather than growing memory; snapshot fencing stops the tail being painted twice |
| Resync | `inplaceResync` when the client advertises it, full snapshot otherwise; epoch ordering keeps a stale frame from landing after a newer screen |
| Reconnect | Shared backoff with full jitter, capped at 256 s; `Rejected` is terminal until re-pair |
| **Open** | A **watched** session still asks the keeper 100 times a second (`WATCHED_POLL`), one socket call each. Measured now with nine desk windows open and no tailnet client: **82 ticks / 10 s in the daemon (8.2 % of a core) and 47 in the keeper (4.7 %)**. The unwatched case is fixed (4 Hz). The real fix is an additive `Peek { wait_ms }` long-poll in the keeper so a poll blocks on data instead of asking, but it has to be arranged so a 500 ms wait cannot delay an attach's snapshot (the daemon holds the stream gate across a poll, and a snapshot drains the same tail), which is a keeper-protocol change with terminal correctness at stake. Costed, not done |

## 17. The surface pass (2026-11-06)

An audit of what the software *offers*, not of whether it works: features nothing reaches, verbs
that duplicate a call, and a test double shipped in the binary. Deleted, not hidden.

| # | Finding | Status |
|---|---|---|
| 17.1 | The desktop inventory scanned `~/.pi/agent/sessions`, paired `pi` processes with transcripts by ancestry and linked them to sessions (~450 lines, one `xprop` per window). The phone parsed `piSessions` and never rendered it: the picker uses workspaces and windows only | DELETED end to end — host scan, `xprop` pid enrichment, the protocol type, the app model, nine tests |
| 17.2 | The session `kind` was a parameter nothing honoured (`new(kind)` forced `"shell"`, with a comment saying so), and the CLI advertised `new --kind shell\|pi` and `run <shell\|pi>` | DELETED — the parameter is gone from the RPC, the CLI and the Kotlin client; the record still says `"shell"` |
| 17.3 | `ldmux desktop`, `ldmux snapshot` and `ldmux remote` printed JSON a human does not read, duplicating RPCs the app and the attach client already call | DELETED — the RPCs stay; `capture` covers the human case |
| 17.4 | The desk client still fell back to the tailnet WebSocket and bearer token for "a daemon that predates the attach socket" — a second transport nobody can reach | DELETED — one transport, the 0600 peer-uid-checked Unix socket |
| 17.5 | The 656-line in-memory test keeper was `pub mod` and therefore compiled into `ldmuxd` | MOVED to `crates/testkeeper`, a dev-dependency; `strings` on the release binary finds none of it |
| 17.6 | `crates/mesh/src/frame.rs` held only the error type, after the frame codec went with the gateway | RENAMED to `error.rs` |
| 17.7 | The `follow` message was handled as an explicit no-op ("client-owned scroll") and no client sends it | DELETED — the variant, its struct, the frame type and the handler |
| 17.8 | `ldmux list --json` printed the human empty-state line when there were no sessions, so a JSON reader got prose | FIXED — `--json` always prints JSON |

Net: −1,910 lines / +161 across 33 files, with the Rust gate (48 binaries), clippy and the Kotlin
suite (574 cases) green.
| 17.9 | `ldmux run` was `ldmux shell` with the directory-reuse turned off, and `shell --name` already forces a second shell | DELETED — one verb for "become the window here" |
| 17.10 | `ldmux close` printed "a desktop window holds it" for **any** claim, including the phone and the radio link | FIXED — "another device holds it", which is what the daemon actually reports |
| 17.11 | `ldmux forget` exited 0 when it forgot nothing, so a script could not tell a no-op from a success | FIXED — exits 1 with the reason |
| 17.12 | The CLI's daemon deadline (5 s) was shorter than the daemon's own keeper deadline (8 s), so a slow-but-successful call was reported as a timeout | FIXED — 15 s, above the daemon's worst case |
| 17.13 | Four flags were accepted and ignored (`--final`, `--id`, `--stdin`, `--title`); an ignored flag is worse than an unknown one | DELETED |
| 17.14 | No bound on live shells: a loop of `new` allocated PTYs until the keeper hit its descriptor limit, with a late and unexplained failure | FIXED — `LDMUX_MAX_SESSIONS` (default 64) refused at `new` with a sentence naming the limit and the cure |
| 17.15 | `ldmux list` printed a `kind` column that is always `shell` | FIXED — dropped from the row, still in `--json` |
| 17.16 | `ldmux adopt ""` answered "no such session: ldmux-" (a name the user never typed) | FIXED — usage error before the call |
| 17.17 | `nix flake check` still listed `the_desktop_inventory_links_a_running_pi_to_its_session` in its skip guard, and that test was deleted: the guard requires exactly one match, so the flake check would go red | FIXED |
| 17.18 | The keeper unit hard-coded `%t/ldmux/keeper.sock` while the daemon derives `<LDMUX_STATE_DIR>/keeper.sock`, so an overridden state directory gave the daemon a socket nobody bound and it spawned a second keeper | FIXED — the keeper resolves the socket through the same `paths` resolver, and the unit passes no path |
| 17.19 | A release built outside a git checkout silently got `versionCode = 1`, so the second upload is rejected and two builds collide | FIXED — `bundleRelease`/`assembleRelease` fail when neither `LDMUX_VERSION_CODE` nor a commit count is available |
| 17.20 | `scripts/android.sh release|bundle` never ran the Kotlin suite, so an artifact could ship with a failing app test | FIXED — the test task runs first and a red suite stops the artifact |
| 17.21 | `collect-licenses.sh` printed "N without a licence file" and exited 0, and the bundle task did not check the licence tree, so an artifact could ship without the notices MIT/BSD/Apache require | FIXED — the collector fails on a missing text, and the release tasks refuse to package without `dist/licenses/THIRD-PARTY-NOTICES.md` |
| 17.22 | `ldmux-attach` (the desk client binary) was built and shipped but installed by nothing | FIXED — `install.sh` links it with the rest |
| 17.23 | `~/.local/state/ldmux/releases/` grew for ever: 24 releases and 237 MB on this machine | FIXED — the installer keeps the current release and the two before it |
| 17.24 | `install.sh` rewrote `@BIN_DIR@` inside the unit *comment* that explains the substitution | FIXED — only `ExecStart=` lines are substituted |
| 17.25 | The keeper unit set `LDMUX_TOKEN_FILE`, which the keeper never reads | REMOVED |
| 17.26 | The `panel` setting changes nothing while Settings is on screen: it only affects the landscape side panel, and Settings holds portrait, so a press looked ignored | FIXED — the row says "the key panel's side, in landscape" |
| 17.27 | `protocol::rpc::method` listed RPC names that nothing used and that had already drifted (no `release`, no `resizePane`), so it documented a contract it could not enforce | FIXED — the list is complete and the CLI's calls go through it, so a renamed method is a compile error on the client half |
| 17.28 | The differ kept an open frame's epoch across `reset`/`Resize`, so a resync mid-frame painted the abandoned patches over the fresh screen, permanently, on rows the host said were blank | FIXED — `reset` clears `frame_open` and moves the epoch; a geometry change does the same; both mirrors drop pending rows on `Profile` |
| 17.29 | `Arq::poll` gated `Close` on the *data* budget, so a close asked for while the hour's allowance was spent was never sent and counted a stall every poll; the module doc said the opposite of the code | FIXED — control traffic is not budget-gated (slot-bounded only), and the doc says what the code does |
| 17.30 | `build_raw_direct` truncated a path that was not a whole number of hashes and never checked the 176-byte frame ceiling, producing a packet whose receiver walks the path wrongly: a permanent "peer never answers" | FIXED — both are refused at the builder with a named reason |
| 17.31 | `plain_row` deleted tabs, so every indented or column-aligned line was reflowed on the phone | FIXED — expanded to the next 8-column stop |
| 17.32 | A `Profile` body of a shape neither decoder arm matched was skipped as unknown, leaving the phone with no profile and no error | FIXED — malformed |
| 17.33 | Only `version`, `minVersion`, `cwdVersion` were pinned by the fixture; a bump of `KEY_VERSION`/`RESIZE_VERSION`/`STREAM_VERSION`/`LINE_VERSION`/`ORIGIN_VERSION`/`OWNER_VERSION` on one side was silent | FIXED — all eight published in `fixture_value` and asserted by the app's codec test |
| 17.34 | The host enforced no airtime budget: the firmware's own limiter is a 50 % duty cycle (`getAirtimeBudgetFactor()` = 1.0, `Dispatcher.cpp`), so the 1 % a European band allows was nobody's job | FIXED — the carrier's published countdown bounds new records, charged per record (the ARQ's limit is per poll); acks and retransmissions still pass |
| 17.35 | "Waiting for the answer" lingered after the answer: the phone ended a sync wait on a 4 s quiet timer because nothing told it the pane had stopped changing | FIXED — `Commit` carries an additive `IDLE` flag, sent once per burst when the pane has been unchanged for 700 ms; the phone ends the wait on it and keeps the timer as the fallback |
| 17.36 | The radios name their holder `"mesh"`, and the app rendered the kind literally: the operator saw "in use on mesh" and could not tell what held the shell | FIXED — kinds are mapped to sentences ("the radio link", "the desktop", "another phone"); a device name passes through |
| 17.37 | The notification icon's `d` had a 4-unit right side against 2 everywhere else, and its stem stood 2 units from the counter | FIXED — the counter runs to the stem (every stroke 2 units) and the letters are 1 unit apart; the mark is still centred exactly |
| 17.38 | The Noise prologue was a fixed string that did not name the suite, so a suite change on one end would derive different keys from the same flags instead of failing | FIXED — the prologue carries `SUITE`, with a test that fails if the two drift |
| 17.39 | CI (`.github/workflows`, removed at the operator's instruction) claimed to run the gate on every branch | REMOVED — the gate is the four local commands, and `docs/ARCHITECTURE.md` says so |
| 17.40 | R8 was off, so a release carried unused code, and the licence notices were excluded from the artifact by a `META-INF` packaging rule | FIXED — R8 on with per-boundary keep rules (JNI, manifest components) and `dist/licenses` shipped as assets |
| 17.41 | Nothing recorded *why* the radios carry their own session instead of using MeshCore's channel encryption, so the next reader would have to re-derive it from the firmware | DOCUMENTED — `docs/adr/0029-the-session-rides-raw-custom.md`, with the firmware evidence, the costs and the rejected alternatives |
| 17.42 | The README described the app and the host but never the security model, so "encrypted" was a claim with no page behind it | DOCUMENTED — a Security section: what each route protects, what it does not, where secrets live, and which ADR has the reasoning |
| 17.43 | `docs/ARCHITECTURE.md` §8 and §9 still said the firmware was the airtime meter, and §4 did not list the `Commit` idle flag | FIXED — the budget section describes the host's countdown and its per-record charge, the failure row says what the host now does, and the flag vocabulary is written down |
| 17.44 | The README carried the security model inline, described things in the second person, and led with prose instead of features | RESTRUCTURED — features first in the operator's words, security moved to `docs/SECURITY.md`, and every sentence impersonal |
| 17.45 | The mark in the README was a single icon with the launcher's full corner radius | the pair now: tighter radius, `ld` and its vertical mirror which reads `pl`, centred together (`docs/media/README.md` says how to rebuild them) |
| 17.46 | The README said "radio route", which names a transport nothing can hold; the offline mode is two physical radios, and the description never said they are optional or that one travels with the phone | REVISED — "the radios" throughout, a bullet stating two MeshCore radios are needed and optional, and the description says "no internet" |
| 17.47 | The mark in the README sat side by side | it stacks now: `ld` above, the 180-degree copy reading `pl` beneath |
| 17.48 | The landscape screenshot shared one row with three portrait ones at 24 % width, so a 2400 x 1080 picture rendered smaller than the phones it was meant to show | FIXED — the three portraits take 30 % each, the landscape sits on its own centred line at 62 % |
| 17.49 | The modes table described the tailnet transport with "a WebSocket to the daemon, bearer token, tailnet or loopback only" — four clauses for one word, and the token and the binding are the security page's business | FIXED — the cell says "a WebSocket" |
| 17.50 | The two marks were stacked; they are a wordmark and read as one line | FIXED — side by side on a 1100 x 512 canvas, 588 px apart, centred as one image |
| 17.51 | The README title read `ldmux` while the repository is `Ldmux`; the two titles are the only places the name is capitalised | FIXED — `# Ldmux`, with every command, binary and file name still lowercase |
| 17.52 | The description said "no internet and no network at all", which contradicts the project: MeshCore is a network | FIXED — "when there is no internet at all" |
| 17.53 | The radio link worked only with both radios in direct range: the host sent every record with an empty path (zero-hop), never stored the path an arriving record took, and the phone could not learn a route from a companion host that never advertised | FIXED — the host reverses the arriving path (`PeerPath`) and replies along it; it floods a self-advert on open and every `LDMUX_MESH_ADVERT_SECS` so the phone's radio stores a contact with a route; the phone re-asks for that contact every 45 s without a route and every 10 min with one |
| 17.54 | The session's first retry was a fixed 2 s, shorter than a round trip at 62.5 kHz (553 ms a record) or across repeaters, so every slow link retransmitted records the peer already had and spent the hour's allowance on duplicates | FIXED — `Config::with_airtime` scales the timers by the record airtime, carried to the host by `Bound::airtime_ms` and to the phone by `ldmux_session_set_airtime` |
| 17.55 | `commanded::SEND_RAW_DATA` was the name of command 0x41, which is `CMD_SEND_RAW_PACKET`; the misnomer invited the 0x19 path-length ambiguity the phone already avoids | RENAMED — `SEND_RAW_PACKET`, with the difference explained at the constant |
| 17.56 | Nothing recorded how MeshCore treats a raw record across repeaters, so "one hop" read like a design choice rather than a firmware constraint | DOCUMENTED — `docs/ARCHITECTURE.md` §11 quotes `Mesh.cpp`'s direct-only `RAW_CUSTOM` case and states what the host and app now do about it |

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
ours, or over LoRa radios when there is no internet at all. The same shell can be on the phone
and in a desk window at once. Nothing is collected. What you need is a Linux machine with a
systemd user session and the daemon installed, Tailscale for the network route, and two
MeshCore companion radios for the offline mode.

Content rating: no user-generated content is hosted, no ads, no purchases, no gambling, no
violence. The app is a development tool for adults, and the questionnaire answers follow from
that.

## Appendix B: open decisions

None of these are defects. Each is a decision that changes what ships.

1. ~~The radio tuple the app compiles in.~~ **Closed 2026-11-06.** The deployed radio runs
   910.525 MHz, 500 kHz, SF7, CR5, which is the app's compiled default, so a fresh install
   reaches this host with no tuple work. The code also carries the host's tuple, so a
   deployment that differs is one press to match. The physical tuple stays a deployment
   choice; the app adopts what the radio reports.
2. Whether the host radio is a shipped feature. The daemon owns it when `LDMUX_MESH_SERIAL` is
   set, and nothing installs or provisions it, so today the route works for an operator who
   reads `docs/ARCHITECTURE.md` section 11 and not for a new user. The in-app side now says
   what the hardware must be; the host side still has no command that finds and names the port.
3. The upload keystore and the store assets, which are the operator's to supply.
4. Whether the mesh link carries a heartbeat while a phone is attached. Today it does not, so a
   phone that vanishes without a `Detach` keeps its shell claim until the link is rebuilt or
   the daemon restarts, and the phone cannot tell a dead host from a quiet one. A record every
   five minutes costs about 12 of the hour's 255 records at 500 kHz/SF7 and about 4 % of the
   budget at 62.5 kHz. Open because it trades an invariant ("an idle screen costs nothing")
   against a liveness guarantee.

## Appendix C: known unfinished

- A desk window displaced by a mesh claim is not covered by a test. The daemon sends the notice
  and the same message is proven for a WebSocket claim, but the mesh variant has no test.
- A shell created from a phone is recorded with `createdBy` set to `phone`, and rows keep their
  actions across an app restart because of it. There is no test that asserts that round trip.
- Dictation depends on the device's speech recogniser, which may be cloud-based. The app says
  so, and nothing here can change it.
- Multi-hop is one direction only: the phone looks up a route, the host always sends zero-hop,
  so a phone one relay away attaches and never hears a reply (`ARCHITECTURE.md` section 11 says
  so now). Implementing the host-side lookup is a feature, not a fix.
- **Airtime on the host is neither enforced nor observable as a limit.** `Budget::RadioGoverned`
  returns `u8::MAX`, so the session loop never gates a record, on the argument that the radio's
  firmware runs the transmitter duty cycle. That argument is not evidenced in this tree.
  `ldmux mesh status` no longer presents the radio's count as a gate ("up to 255 left this
  hour (the field's ceiling)"), which is honest but leaves the duty cycle unattributed: either
  the firmware meters (say so, with evidence) or the host must.
- A spawned window is scoped, but its **workspace placement** (i3/sway/hyprland) is only
  exercised by `desktop_e2e`; nothing asks for a workspace from a phone today, so the adapter
  is carried, not used.
- Font scale, RTL, dark mode and split screen are still unverified (3.6).
- R8 is off, so a release carries unused code (1.8), and nothing outside this machine builds the Android
  artifact (1.7).
