# Desk-window resize: what was wrong, and what the fix assumes

Written 2026-11-03. `ldmuxd attach` follows the window it is drawn in: a SIGWINCH on the tty
becomes `{type:"resize"}`, the daemon clamps it and asks the keeper for a PTY resize, and a
fresh snapshot comes back. The reported symptom — a desk window painting a phone-sized pane —
turned out to be the mobile client's attach size, not this path, and that is fixed. Four
defects were left over in the path itself, three on the desk side and one on the phone.

## Desk side (fixed in 3769eef)

1. **Every SIGWINCH was a round trip.** Dragging a window edge emits tens of them, and each
   one cost a PTY resize, a libghostty-vt reflow and a full snapshot back. The client now waits
   120 ms for the drag to settle and sends the size it ended on (`RESIZE_SETTLE`,
   `crates/attach/src/lib.rs`). One resize per settled drag instead of one per pixel column.
2. **A row wider than the window hard-wrapped the picture.** Rows were written verbatim, so the
   terminal wrapped 82 rows into 164 lines, the screen scrolled, and the cursor was drawn on a
   row nothing else agreed with. `clip_row` cuts each row to the window width and copies escape
   sequences whole, so a cut never leaves half a CSI or a dangling OSC payload.
3. **The re-assert loop never converged.** `should_reassert` compared an unclamped tty size
   against the daemon's clamped grid, so a window wider than 500 columns or taller than 255 rows
   re-asserted every 2 s for ever (`crates/attach/src/lib.rs`). Both sides now compare the
   clamped size.

## Phone side (fixed in 501adf6)

4. **A refused grid was never re-sent.** The daemon clamps to 20..500 columns and answers
   anything outside that with `bad_size`. Nothing re-sends a size on its own, so a phone that
   asked for a width the daemon would not take kept painting a grid the host never adopted, and
   every later frame arrived for a width nothing matched. The refusal now asks again inside the
   daemon's bounds.

## What the fix does not claim

The reflow itself is libghostty-vt's, and its viewport rule (an anchored viewport stays
anchored) is ADR-0004; this document says nothing new about it. The clamping constants live in
the daemon's stream (`MIN_PANE_COLS`, `MAX_PANE_COLS`) and are mirrored, not shared, by the
attaching client and the phone, so a change there needs three edits.

## How it was checked

`cargo test -p ldmux-attach` covers `clip_row` (a wide row, a row cut inside a CSI, an OSC
payload, a row already inside the window) and the settle path. The device check on 2026-11-03
covered the phone's side of the same path: resize on rotation, the panel arriving in landscape,
and `bad_size` no longer sticking.
