# LDMUX and MeshMore: extent, defects, separation

- What this directory is: the audit record behind the decision to separate LDMUX from MeshMore, and the fix register for both repositories.
- Written 2026-11-04 against `ldmux` branch `feat/standalone-radio`, first at `c96d316` and
  **re-verified at `7b8510b` plus the refusal-pace and sequence work committed after it**. The
  `meshmore-sns` side is `feat/ldmux-gateway` at `5d97625`. When a file's citations and the tree
  disagree, the tree is right and the file is a defect.
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
| `09-RESEARCH-LOG.md` | What the MeshMore-side research corpus changes for LDMUX's mesh path specifically, with what is satisfied, what is open, and what another implementation owns |

## Status words

- PROVEN: read in the source at the commit above, with file and line.
- SUSPECTED: inferred from code or documentation without a run.
- DATED: a measurement or an observation, true as of the date given and not a property of the code.
- OPEN: needs a decision from the owner.

## What this pass did not do

- No radio was driven. The gateway held the serial port for the whole pass, so "quiet" is not "deaf" and no airtime was measured.
- The Kotlin and Android suites were not run, because they need `nix develop .#mobile`.
- `cargo test --workspace` is green with `zbarimg` on `PATH`; without it, the two QR tests fail for
  that reason alone (`02`, defect 17). A count is deliberately not written here: it changes with
  every commit.
- `docs/separation/02-DEFECTS-LDMUX.md` now carries a status per defect against the current tree and
  was rewritten for it. Ten of its defects are fixed in code, not proposed.
- The `meshmore-sns` defects in `03` are still proposals: nothing was changed in that repository.
- The mobile sources were read, not compiled. The C ABI additions were syntax-checked against the
  Android NDK headers; the Kotlin side was written to keep every existing fake compiling, through
  default methods on `SessionPort`, and is unverified until the mobile suite runs.
- A parallel research track wrote `/home/ldeen/Documents/mesh-research/notes/` between 10:50Z and 15:01Z on 2026-11-04. `08-RESEARCH-INTEGRATION.md` records only what that corpus changes here, and every number it contributed was recomputed against local source before it was used.

## The one-paragraph answer

- LDMUX uses MeshCore as a radio and a packet carrier, and it bypasses MeshCore's application layer entirely.
- The firmware is unmodified and load-bearing. The companion protocol is load-bearing and extended. The channel, PSK, advert and contact-identity layer is not used at all.
- The session, the ARQ, the terminal and the cryptography above the packet are LDMUX's own.
- The structural cost is that the host reaches its radio through MeshMore's Dart gateway, so "standalone" is true on the phone and false on the host.
