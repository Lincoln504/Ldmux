# State of record, 2026-11-05

One page reconciling the project's own documents with the tree. It exists because
the work was spread across sessions whose working directory was not this
repository, so no single place said where things stood. Read this first, then
`docs/STATE.md` for detail.

Everything here was checked against the source and the repository at
`feat/standalone-radio`, HEAD `dae6150`. Where a claim comes from another document,
the document is named. **This file was rewritten on 2026-11-05 because it had
drifted**: it said the companion crate did not exist, that the host owns no radio,
that signing and the version code were unstarted, and that a fixed test flag was
still the CI story. Every one of those was wrong by the time it was read.

## Where the work happens

- The code is this repository.
- The bench checkout (`~/Documents/lora`) is hardware knowledge only: radio
  inventory, bring-up notes, link measurements, and the `mcproto` client. Neither
  repository references the other. It is not a dependency and not part of a release.
- Both measured the same radio and agree: `914.25 MHz / 500 kHz / SF8 / CR5 / 22 dBm`
  for the node called `HELTEC-BASE`. The bench's older notes saying
  `917.000 / 125 / SF11` are the stale side.

## Timeline

| When | What |
|---|---|
| 2026-09-13 – 10-05 | Specification, then the Rust port. |
| 2026-10-06 | Last commit on `main`. |
| 2026-10-24 – 11-01 | Mesh era: gateway, standalone phone radio, the separation plan (`docs/separation/`). |
| 2026-11-02 – 11-03 | Hardware pass, ARQ and refusal work, session stability, the radio-tuple write-up. |
| 2026-11-04 – 11-05 | Mobile UX; the serial carrier (host owns the radio); mesh protocol 7 and 8. |

`main` is `c9c7643` and **257 commits behind** this branch. There are **no tags**,
so nothing has ever been released.

## Three modes, one daemon

The app has exactly three ways to reach a desktop, and each has to work on its own:

| Mode | Carriage | State |
|---|---|---|
| tailnet | WebSocket with bearer auth, over Tailscale, LAN or cellular | Deployed and device-verified. |
| LoRa live | two MeshCore radios, keystrokes leave as typed | Verified on hardware: handshake, shell list, typing, resize. |
| LoRa manual sync | the same radios, held text sent on one action | Implemented; the same session carries it, so a sync costs records rather than a reconnect. |

- **The host owns its own radio.** `LDMUX_MESH_SERIAL` names the device and the
  daemon drives it directly through `crates/companion` — no gateway process and no
  loopback hop. The gateway path still exists (`LDMUX_MESH_ADDR`) and is the
  fallback for one release.
- The phone owns its radio over BLE itself (`docs/MESH-STANDALONE.md:1-20`).
- The serial carrier reports its budget source as `RadioGoverned`: no gateway is in
  the path, so the firmware's own duty cycle is the meter and the host does not
  apply a second one.
- The mesh terminal protocol is **version 8**: a phone can list, attach, type,
  resize, start a shell, end one, put one on the desk, and take one from the desk —
  and each `Sessions` entry says who started the shell and who holds it now.

## Release

- **Signing is implemented and proven.** `packages/mobile/keystore.properties` or
  `LDMUX_KEYSTORE*` supply it; `scripts/android.sh release|bundle` fail loudly
  without it. A signed bundle was produced with a throwaway 4096-bit key to prove
  the path, then the key was deleted. **The real upload key is David's to supply.**
- **`versionCode` comes from `git rev-list --count HEAD`**, with
  `LDMUX_VERSION_CODE` / `LDMUX_VERSION_NAME` overrides for a shallow clone.
- **A release cannot be packaged without both native libraries** on every shipped
  ABI (`libldmux_session.so`, `libldmux_terminal.so`); the tasks check and fail.
- **Licence texts ship inside the artifact.** `scripts/collect-licenses.sh` copies
  every crate's text plus the vendored libraries into `dist/licenses`, and the build
  packages it as assets (154 files in the bundle measured).
- **CI runs on every branch**, `nix flake check` evaluates, and
  `scripts/test-parallel.py --locked` works (it did not: argparse rejected the flag
  and the step was dead).

## Decisions still open

1. **The radio tuple the app ships.** The compiled default is
   `910.525 / 500 / SF7` (`mesh/Radio.kt`); the host radio is
   `914.25 / 500 / SF8`. A fresh install therefore cannot reach this host's radio
   without the user setting the tuple by hand. The options are in
   `docs/separation/05-RADIO-TUPLE.md`.
2. **The upload keystore and Play listing** (David): key, screenshots, feature
   graphic, content rating, data-safety form.
3. **The privacy policy URL**: text is ready in `docs/STORE.md`, needs hosting at
   `https://ldmux.io/privacy`.

## What is known to be unfinished

- A desk window displaced by a **mesh** claim is not covered by a test. The daemon
  sends the notice (instrumented and confirmed) and the same message is proven for a
  WebSocket claim by `the_desk_window_takes_the_shell_back_when_the_holder_lets_go`;
  the test for the mesh variant was removed rather than shipped red.
- The desktop-session environment problem (no `DISPLAY`, `WAYLAND_DISPLAY`,
  `XAUTHORITY` in ldmux shells) is recorded in `docs/STATE.md` and not fixed.
- `docs/RELEASE-AUDIT.md` carries the audit rows; several are stale and are being
  worked down rather than restated here.
