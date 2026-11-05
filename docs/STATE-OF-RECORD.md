# State of record, 2026-11-05

One page reconciling the project's own documents, the git history, and the audits
done on 2026-11-05. It exists because the previous six weeks of work were spread
across sessions whose working directory was not this repository, so no single place
said where things actually stood. Read this first, then `docs/STATE.md` for detail.

Everything here was checked on 2026-11-05 against the source and the repository at
`feat/standalone-radio`, HEAD `5b03d6b`. Where a claim comes from another document,
the document is named.

## Where the work happens

- The code is this repository. Sessions that worked on it have run with their
  working directory set to a separate bench checkout, and edited this tree by path,
  which is why "the project" has sometimes been resolved to the wrong repository.
  Nothing was lost; the record of it was spread out.
- The bench checkout (`~/Documents/lora`) is hardware knowledge only: radio
  inventory, bring-up notes and link measurements. Neither repository references the
  other. It is not a dependency and not part of a release.
- Both have measured the same radio, and they agree: this repository's hardware pass
  of 2026-11-02 (`docs/HARDWARE-VALIDATION.md:856-878`) and a fresh read-back over
  USB on 2026-11-05 both report `914.25 MHz / 500 kHz / SF8 / CR5 / 22 dBm` for the
  node called `HELTEC-BASE`. Where the bench's older notes say
  `917.000 / 125 / SF11`, the notes are the stale side.

## Timeline

| When | What |
|---|---|
| 2026-09-13 – 10-05 | Specification, then the Rust port. |
| 2026-10-06 | Last commit on `main` (23 that day). |
| 2026-10-24 – 11-01 | Mesh era: gateway, standalone phone radio, the separation plan (`docs/separation/`). |
| 2026-11-02 – 11-03 | Hardware pass, ARQ and refusal work, session stability, the radio-tuple write-up. |
| 2026-11-04 – 11-05 | Mobile UX: public-channel mode (ADR-0026), scroll-back, settings as a screen, rotation, reconnect lands home. |

600-odd commits, of which **220 are on `feat/standalone-radio` and not on `main`**.
`main` is level with `origin/main` and is six weeks behind the work. There are **no
tags**, so nothing has ever been released.

## Two routes, one daemon

| Route | Carriage | State |
|---|---|---|
| `direct` | tailnet, LAN or cellular, WebSocket with bearer auth | Deployed and device-verified. |
| `mesh` | two MeshCore LoRa radios | Phone side proven on hardware; host side depends on a process this repository does not contain |

- The phone owns its radio over BLE itself; MeshMore is out of that path
  (`docs/MESH-STANDALONE.md:1-20`).
- **The host owns no radio.** `crates/daemon/src/mesh.rs:12` dials a
  MeshMore-style gateway on loopback (`127.0.0.1:41731`), and that gateway —
  including the host radio's preset — is configured out of band
  (`docs/MESH-HOST-RADIO.md:6-8`, `docs/MESH-STANDALONE.md:420-424`). This is open
  defect 1 and the largest gap between "works on the bench" and "a user can set it
  up".
- Proven over the air on 2026-10-29 and 2026-11-02: session established, host shell
  listed, typed input reached the host terminal (`docs/HARDWARE-VALIDATION.md:856-878`).

## Decisions still open

1. **Which radio tuple is canonical.** Five claims are in the tree
   (`docs/separation/05-RADIO-TUPLE.md`). The app's compiled default
   (`packages/mobile/composeApp/src/commonMain/kotlin/dev/ldmux/mobile/mesh/Radio.kt:66-73`,
   `910.525 / 500 / SF7`) does not match the deployed radio
   (`914.25 / 500 / SF8`), so **a fresh install cannot reach the host's radio**.
   Options A (reflash the radios), B (change the constant), C (no constant: adopt
   what the radio reports and refuse on mismatch, the document's recommendation),
   D (adopt the public MeshCore preset `910.525 / 62.5 / SF7`).
2. **Settings opens as a screen and ends the session** (`App.kt:88-104`,
   `mesh/MeshTerminalScreen.kt:230-238`). On the radio that costs a fresh handshake.
   Named as a decision in `docs/MOBILE-UX-AUDIT.md` §4.
3. **Scroll affordance on settings**: scrollbar or fade
   (`SettingsScreen.kt:90` is a bare `verticalScroll`).
4. **A password typed at a mesh prompt is echoed**, because the mesh `Commit` has no
   password flag (`docs/MOBILE-UX-AUDIT.md`).
5. **Host radio provisioning**: ship a gateway and a provisioning path, or document
   the gateway as an external dependency. This decides whether the radio route is a
   feature or a demo.

## Defects

The register is `docs/separation/02-DEFECTS-LDMUX.md`: 10 fixed, 1 partial, 1
corrected, 2 withdrawn, **9 open**.

| # | Open defect |
|---|---|
| 1 | The host reaches its radio only through MeshMore's Dart gateway |
| 8 | The ARQ retry is shorter than the carrier's own timeout past one hop |
| 9 | Five radio presets asserted as the live one |
| 11 | The vendored Java library has no upstream tracking |
| 14 | Timing instrumentation is on in production |
| 15 | The gateway token and radio key sit in `/tmp` |
| 22 | No pacer: a screen update leaves as a burst of up to eight records |
| 23 | `Stats::sent` counts control records and retransmissions |
| 24 | No randomised initial retry |

- Partial: 5 — the bridge's 50 ms full-screen poll is improved but still takes the
  daemon-wide mutex while awaiting a keeper round trip
  (`crates/daemon/src/mesh_bridge.rs:709-716`).
- 20 is fixed in code and unverified on a device.

## Mobile UX audit, done 2026-11-05

Commissioned 2026-11-04, the assigned sub-agent failed, and it was re-run on
2026-11-05. **No P0.** Three P1 and four P2, each with a file and a smallest fix:

| Sev | Finding | Where |
|---|---|---|
| P1 | `Open` is a live button with a no-op inside its lambda, so a screen reader announces an enabled button that does nothing | `LauncherScreen.kt:370-382` |
| P1 | Touch targets have a height floor but no width floor; a one-glyph `−`/`+` is ~30x36 dp in thin density | `Theme.kt:222`, `Controls.kt:173-187` |
| P1 | A resync that is never answered is never asked again, and nothing times out | `mesh/MeshTermDriver.kt:717-721` |
| P2 | The radio's failure reason is discarded, so "radio said nothing" and "never asked" look the same | `mesh/MeshRouteSection.kt:250` |
| P2 | A refused action's message vanishes after 4 s and is not a live region | `AppContent.kt:847-860` |
| P2 | The shell list survives a dead link and `Open` stays armed | `LdmuxClient.kt:552,572,599` |
| P2 | Settings ends the session (decision 2 above) | `App.kt:88-104` |

Confirmed still open from `docs/MOBILE-UX-AUDIT.md`: no scroll affordance on settings,
text size needs 21 taps, `AppHeader` always reads `ldmux`. There are no Compose UI
tests, so a regression in the semantics tree would not be caught.

## Planned, and how much of it exists

Reconciled 2026-11-05 from `docs/SPEC.md`, all 26 ADRs, `docs/separation/06-SEPARATION-PLAN.md`
and the state documents. "Done" means code or a commit, not a document claiming it.

| Planned | Status | Evidence |
|---|---|---|
| Session layer outliving clients; keeper as a unit, shells as scopes | done | `crates/keeper`, ADR-0018/0021 |
| WebSocket transport with token auth, deflate, snapshot-then-stream | done | `crates/daemon/src/{remote,ws}.rs` |
| Ownership with consented handoff | done | `crates/daemon/src/ownership.rs` |
| Noise `IKpsk2` session, ARQ, replay window | done | `crates/session/{session,arq}.rs` |
| Mesh terminal profile as in-session row diffs | done | `crates/protocol/src/mesh_term.rs` |
| Phone owns its radio over BLE | done | `AndroidRadioLink.kt` |
| Public-channel mode (hold typing, one sync) | done, field pass unrecorded | `PublicChannelHold.kt` + test |
| **A host companion crate (`crates/companion`), no gateway** | **not started** | crate absent; `daemon/src/mesh.rs:12` still dials `127.0.0.1:41731` |
| Separation phases P2 (cutover), P3 (delete), P4 (tuple) | not started | depend on the crate above |
| Retry derived from preset and hop count | not done | `arq.rs:510` has no hop term, though `:446` quotes the formula |
| Record padding to 64/128/ceiling buckets (ADR-0020 §4) | not started | no padding code |
| ML-KEM stage; handshake on rekey; PSK rotation from the CLI | not started | `needs_rekey` is set and never read |
| Multi-hop path reset and discovery | not started | `CMD_RESET_PATH` / `CMD_SEND_PATH_DISCOVERY_REQ` absent from the vendored library |
| Host-radio provisioning (installer, screen, packaging) | not started | `docs/MESH-HOST-RADIO.md:6-8` |
| Host-side airtime metering | not done | the meter covers the phone's own radio only |
| Delete the `0x19` raw-send path | phone stopped calling it, cleanup not done | `MeshcoreSession.sendRawData` survives unused |

Two intents exist in one document and are tracked nowhere else: SPEC §5.4's REST surface
("aspirational") and SPEC §4.5's "Pi RPC as a complementary channel". ADR-0020's padding buckets
were a condition of the security argument and are tracked by nothing at all.

## Planned vs done

## Release blockers, none of them started

- No signing configuration (`packages/mobile/composeApp/build.gradle.kts:71-74`), so
  no uploadable artifact; `scripts/android.sh:25,27` builds debug only.
- No release job in CI (`.github/workflows/ci.yml` is Rust-only: flake check,
  clippy, tests, `cargo deny`).
- `versionCode = 1` is a literal (`build.gradle.kts:63`).
- The Kotlin suites are not in the gate; `scripts/android.sh test` runs by hand.
- No privacy policy, data-safety answer, content rating or listing material.
- The device verification of 2026-11-02 was performed on binaries older than HEAD
  (`docs/STATE.md:200,286-287`). A release cut needs a device pass on the artifact
  being shipped.

## How to resume, in order

1. Decide the radio tuple (decision 1). It is one constant and a refusal path, and
   the hardware half is settled.
2. Decide the host radio's future (decision 5). Everything else about the radio
   route follows from it.
3. Land or retire the one unlanded commit, `ca8aa91` (desktop attach: scrollback on
   first attach, autowrap slides), which is in no other branch.
4. Fast-forward `main` to `feat/standalone-radio`, or cut a release branch from it,
   and start tagging.
5. Fix the three P1 UX findings; they are local and small.
6. Then release plumbing: signing, `versionCode`, a Kotlin test job, store material.

## Verification commands

```
cargo check --workspace --all-targets          # green at 5b03d6b, 2026-11-05
nix develop --command cargo clippy --workspace --all-targets --locked -- -D warnings
nix develop --command python3 scripts/test-parallel.py --locked
scripts/android.sh test                        # JVM suites, not in CI
scripts/android.sh native-test                 # JNI shim host tests
```
