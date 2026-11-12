# ADR-0034: A hold bounds output, not validity — and the attach states the grid

**Status:** Accepted (2026-11-09). **Under revision, 2026-11-11:** the `Attach` grid field stands (and is extended to `Takeover`, `Create`, `Send`, and `Fetch`), but `Hold(true)` clearing `owe_frame = false` (`mesh_bridge.rs:655`) while `Resize` was deferred behind `pending > 0` cancelled the resize repaint and produced a blank clipped screen (`RC-11` in [`docs/RADIO-AUDIT-REVIEW.md`](../RADIO-AUDIT-REVIEW.md)). Replaced by demand-only `MeshRpcService` and atomic `Snapshot` in [`docs/RADIO-PLAN.md`](../RADIO-PLAN.md) §1, §6, §7. Amends ADR-0025 (the mesh terminal profile) and ADR-0026
(public-channel mode). **Related:** `docs/ARCHITECTURE.md` §9, `docs/AIRTIME.md`, ADR-0021.
**Scope:** `crates/protocol/src/mesh_term.rs` (protocol 13), `crates/daemon/src/mesh_bridge.rs`,
`packages/mobile/composeApp/src/commonMain/kotlin/io/ldmux/mobile/mesh/` —
`MeshTerm.kt`, `MeshTermDriver.kt`, `MeshTerminalSource.kt`, `PublicChannelHold.kt`,
`MeshTerminalScreen.kt`.

## The problem

Batch mode holds the host's drawing so that a shell printing a build log does not spend the hour's
airtime on a picture in a drawer (ADR-0026). Two things that are not output were caught by the same
hold, and both left the operator looking at a screen that was *wrong*, not merely old:

- **A grid change.** That is a rotation or a keyboard, and the pane's rows are reflowed by the host.
  The hold suppressed the resulting frame, so the phone kept drawing the old grid's rows into a new
  viewport until the shell was left and re-entered — one `Resize` record out and nothing back.
- **An explicit "show me".** It released the hold, waited for a screen and re-held it: three records
  a press, and a wait that ended on four seconds of quiet — which a shell that prints every two
  seconds never reaches, so the view stayed live and kept spending records.

And the first paint of a shell paid for a repaint nobody saw. The attach carried no grid, so the
host fitted the pane to the geometry this phone used last time, drew a full frame at it, then
applied the phone's `Resize` and drew again: measured on the device, **seventeen records to open a
shell**, about ten seconds of air, for a pane the operator never wanted at that size.

## Decision

1. **Protocol 13: the `Attach` states the phone's grid** (`cols`, `rows`; `0` means "not stated").
   The version is the first byte of the message, so both ends know which shape they are reading
   without a second flag, and a version-12 attach is decoded exactly as before. The host fits the
   pty to the stated grid *before* it builds anything and skips `ATTACH_GRACE`, which exists only to
   wait for a number it now has. A stated grid beats the remembered one; the remembered one stays as
   the hint for a phone that states nothing.

2. **A hold bounds output, not validity.** Exactly two things owe a frame while a shell is held, and
   they share one flag (`owe_frame`):
   - an **accepted `Resize`** (the differ is reset so the frame starts the screen over, so nothing is
     missed by a phone that just rebuilt its engine at a new size);
   - a **`Resync` from a frozen view**, which is what the `show` press sends.

   Everything else stays suppressed, and the hold resumes after the one frame — the phone sends no
   release and no re-hold, so a press costs one record out and the difference back.

3. **A `show` waits for one screen, not for quiet.** A press that sent nothing is asking to see the
   newest screen, which is what the release sends; a press that sent text is waiting for a command's
   answer, which arrives over several screens and still ends on quiet (or on the host's own idle
   flag). Treating them alike is what kept the view open for ever on a shell that never goes quiet.

4. **A grid the wire allows but no screen has is refused.** 255x255 is sixty-five thousand cells and
   twenty-odd records of a full frame; a measurement bug must not be able to spend twelve seconds of
   air. The bound is 200x120, far above any real device.

## Alternatives

- **Keep the release-and-re-hold for `show`.** Works, and is what ran. It costs two control records
  per press and needs two phone-side flags (`holdWanted`/`holdSent`) whose disagreement with the host
  is the class of bug this ADR removes.
- **End the `show` wait on a timer.** A timer cannot tell "the answer is still coming" from "nothing
  changed", so it either closes the view early or holds it open on a printing shell. The screen
  arriving is the fact, and the host's idle flag is the exact end for a sent answer.
- **State the grid in a second message after the attach.** That is what a `Resize` in the same record
  was, and it leaves the host guessing for one grace period; two bytes in the attach cost nothing.
- **Let the phone keep a grid across attaches without stating it.** A rotated phone would then be
  drawn at the old geometry on every reconnect, which is the bug the stated grid removes.

## Consequences

- A mixed deploy is refused rather than wrong: a version-13 phone against a version-12 host is told
  `Closed{VERSION}`, because a host that reads the new attach would misparse the selector.
- `ATTACH_GRACE` now exists only for a phone that states nothing, so first paint is limited by the
  airtime of one full frame rather than by a grace as well: measured **2.3 s** to a painted shell at
  46x17, against about ten seconds.
- `docs/AIRTIME.md` carries the per-action cost, measured on the device, which is what these
  decisions are justified by.
