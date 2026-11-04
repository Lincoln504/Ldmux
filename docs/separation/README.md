# LDMUX and MeshMore: extent, defects, separation

- What this directory is: the audit record behind the decision to separate LDMUX from MeshMore, and the fix register for both repositories.
- Written 2026-11-04 against `ldmux` branch `feat/standalone-radio` at `c96d316` and `meshmore-sns` branch `feat/ldmux-gateway` at `5d97625`.
- Firmware reference: `meshcore-fw-alerts` at `685a869`, which is ahead of the `companion-v1.17.1` build `d929643` that the radios actually run.
- Every claim carries a file and line, a commit, or a date. A claim without one is marked NOT EVIDENCED.

## Files

| File | Answers |
|---|---|
| `01-EXTENT.md` | To what extent does LDMUX use MeshCore, layer by layer, and what is load-bearing |
| `02-DEFECTS-LDMUX.md` | Every defect found in `ldmux`, with severity and evidence |
| `03-DEFECTS-MESHMORE.md` | Every defect found in `meshmore-sns`, with severity and evidence |
| `04-MESHMORE-SCOPE.md` | What MeshMore actually is, package by package, and what each product does |
| `05-RADIO-TUPLE.md` | The five conflicting radio presets, the airtime arithmetic, and the options with their UX consequences |
| `06-SEPARATION-PLAN.md` | How to make LDMUX independent of MeshMore, in phases, with what to delete |
| `07-SOURCES.md` | Firmware and protocol facts, with the line that proves each one |
| `08-RESEARCH-INTEGRATION.md` | What the parallel field research adds, corrects, and changes in the plan |

## Status words

- PROVEN: read in the source at the commit above, with file and line.
- SUSPECTED: inferred from code or documentation without a run.
- DATED: a measurement or an observation, true as of the date given and not a property of the code.
- OPEN: needs a decision from the owner.

## What this pass did not do

- No radio was driven. The gateway held the serial port for the whole pass, so "quiet" is not "deaf" and no airtime was measured.
- The Kotlin and Android suites were not run, because they need `nix develop .#mobile`.
- `cargo test --workspace` ran: 314 passed, 2 failed only because `zbarimg` is not installed.
- No file in either repository was modified by the audit itself. The fixes in `02` and `03` are proposals until they are applied.
- A parallel research track wrote `/home/ldeen/Documents/mesh-research/notes/` between 10:50Z and 15:01Z on 2026-11-04. `08-RESEARCH-INTEGRATION.md` records only what that corpus changes here, and every number it contributed was recomputed against local source before it was used.

## The one-paragraph answer

- LDMUX uses MeshCore as a radio and a packet carrier, and it bypasses MeshCore's application layer entirely.
- The firmware is unmodified and load-bearing. The companion protocol is load-bearing and extended. The channel, PSK, advert and contact-identity layer is not used at all.
- The session, the ARQ, the terminal and the cryptography above the packet are LDMUX's own.
- The structural cost is that the host reaches its radio through MeshMore's Dart gateway, so "standalone" is true on the phone and false on the host.
