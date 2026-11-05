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

## The ceiling was below a real window (fixed 2026-11-03)

The columns ceiling was 500, documented as "well above any real display". It is not: a wide window
at a small font asks for more, and the clamp then pinned the pane at 500 while the window kept
growing, so the right of the window stayed blank and the terminal looked like a smaller view inside
the window rather than more text. 2000 now (`ldmux_protocol::MAX_PANE_COLS`), which is 510k cells at
the row ceiling. Verified end to end against a local daemon: a 620-column window left `stty size`
reporting `40 620` inside the shell, where the same probe reported `40 500` before the change.

The keeper is a long-lived process and clamps on its own, so a host that has not restarted the
keeper and the daemon still has the old ceiling.

## A window that opened at the wrong grid (fixed 2026-11-05)

Reported: a desk window opened normally paints a terminal that does not match the window it is in,
and only resizing the window makes the text fit. Two defects in `crates/attach/src/lib.rs` together
produce exactly that, and neither is visible once the window is open, which is why it read as a
start-up problem that fixed itself "sometimes".

1. **The cached size was never re-measured.** The client reads its tty size once, sends it with the
   attach, and from then on compares the *cached* value with each snapshot. The daemon resizes the
   pty to that value before the snapshot, so the snapshot **agrees** with it — whatever it was. A
   wrong reading at start-up (a window the manager had not finished placing, or `TIOCGWINSZ`
   returning zero on a pty whose size is not set yet, which falls back to 80x24) is therefore baked
   in for the life of the window, and the re-assert below it can never see it. The fix measures the
   window on every snapshot: one ioctl, which cannot disagree with the window.
2. **The SIGWINCH handler was registered after the attach was sent.** A window manager places a
   window (and therefore resizes it) while the client is starting. A `SIGWINCH` delivered before the
   handler exists is ignored — its default disposition is to do nothing — so a placement resize in
   that gap was lost with no trace. The handler is now registered before the size is read.
3. **Two ioctls per resize.** The `SIGWINCH` branch called `tty_size()` twice, so a resize between
   the two calls produced a width from one window and a height from the next: a grid neither window
   ever had. One call now.

Checked with a new test in `crates/daemon/tests/attach_client.rs`,
`a_window_opens_at_the_size_of_the_terminal_it_is_drawn_in`: the attach binary runs on a real pty of
100x30 against a real daemon and keeper, `stty size` is typed into the pane, and the answer must be
`30 100`. It pins the opening path — a window whose size is known before it starts ends at exactly
that grid. **The lost-signal race itself is not reproduced by a test.** It needs a resize inside the
space between the attach write and the handler registration, which a test can only reach by luck,
and a timing-dependent test here would fail on the schedule of the machine rather than on the code.
What the test does prove is that the pane follows the window's measured size rather than a value
invented on the way, which is the part that made a wrong reading permanent.
