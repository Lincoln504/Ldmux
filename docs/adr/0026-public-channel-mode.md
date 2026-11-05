# ADR-0026: Public-channel mode — typing held on the phone, one sync button

**Status:** Accepted (2026-11-04). Implements the mode asked for after the first field sessions on a
public channel. **Related:** ADR-0025 (the mesh terminal profile), `docs/MESH-GATEWAY.md` (the
record budget), `docs/MESH-LATENCY.md`. **Scope:** `packages/mobile/composeApp/src/commonMain/
kotlin/dev/ldmux/mobile/mesh/` — `PublicChannelHold.kt`, `PublicChannelBar.kt`,
`MeshTerminalSource.kt`, `MeshInput.kt`, `MeshTerminalScreen.kt`. No protocol change, no host
change, no radio change.

## The problem

On a public channel, everything typed is broadcast to everyone in range, and the mesh link's budget
is about 58 records an hour (`docs/MESH-GATEWAY.md`, ADR-0025). Two costs follow from sending as the
user types:

- **Disclosure.** A command half-typed, a path being corrected, a password typed into a prompt that
  has not answered yet: each keystroke is a frame on the air.
- **Airtime.** A 20-character command typed at a normal pace is two or three records by the
  batcher's own windows, and each one is airtime spent on a shared channel and taken from an hour
  that has about 58.

A person composing a command does not need each keystroke to leave the phone. They need the command
to leave when *they* decide, and the screen to answer then. That is the whole mode.

## Decision

**One mode, with one standalone control. Forced on a public channel, optional on a private one.**

Whether the channel is shared is *derived*, not asked: `isPublicChannel(preset)` compares the four
fields that decide who hears whom (frequency, bandwidth, spreading factor, coding rate) against the
region's public presets, and the radio dialog writes the answer into the config whenever it reads
`SELF_INFO` or applies a preset. Unknown reads as public, because that is the safe reading of "not
proved private". On a public channel the mode cannot be turned off — every keystroke there is a
broadcast, so held input is the only supported way to type — and the bar says why instead of
offering a toggle. On a private tuple the user may hold typing or not.

- **Input is held in the existing batcher.** `MeshInputBatcher` already accumulates typed text and
  named keys in order, coalesces them, refuses past a bound, and renders them readably (`⏎`, `^C`,
  `‹Up›`). Public-channel mode is *not a second queue*: it is a gate on the one pump loop that
  already drains that queue, so nothing about ordering, coalescing or the record limits changes.
- **The gate is the pump loop**, which is the only sender on this route. Every input path — the key
  bar, the hardware keyboard, a tap's arrow keys, a scroll on the alternate screen, paste, dictation
  — ends in `MeshTerminalHandle` → batcher, and the loop is what sends. There is no second path to
  close.
- **The mode and "flushing" are two flags.** `flushing` is true only from the sync press until the
  buffer is empty; `awaiting` stays true until the host's answer arrives. A single flag would send
  everything typed while the answer was on its way, which is exactly the live sending the mode
  exists to prevent. The pair has to be two *independent* lifetimes, and an earlier version made
  them one: `painted()` cleared both, so a screen arriving mid-drain — which on this link is the
  normal case, because an `Input` costs a record and the ARQ queue holds two — closed the drain and
  left the rest of a two-line command held with nothing in flight. Only three things end a drain:
  the buffer emptying, the deadline, and leaving the mode. A paint ends only the wait.
- **A screen that arrives mid-drain is held, not drawn.** It is the pty's reaction to a partial
  command, so drawing it would end the wait before the send was complete. The drain finishing is
  what draws the newest held screen ("send what is held, then show the shell's view" is honoured by
  the source, not by the user pressing again).
- **Every press restarts the deadline.** A monotonic press count, not "a wait is running": a Boolean
  that is already true cannot tell the screen's timer that the user pressed again, so a second press
  15 s into a 20 s wait inherited 5 s of clock it had not earned — and the deadline's own action set
  the Boolean false, which re-keyed the timer whose first line cleared the notice it had just
  written. The "no answer yet" line therefore rendered for at most one frame.
- **The view is frozen, and "sync" unfreezes it.** While the mode is on, a screen the host sends is
  *kept* (the newest one) rather than drawn. The sync button drains the buffer, and the first screen
  that arrives afterwards is drawn — the host's answer to what was sent. A frozen view never stops
  the link: the driver keeps polling and the session keeps its acks, because a frozen view is a
  view and not a disconnected terminal.
- **Typing is echoed locally** (see "The local echo" below) and the bar says what is waiting and
  how stale the view is. `12 characters and 1 key, about 2
  records · view 3 behind`. The record count is the number the user cannot otherwise see and the
  one that matters: an `Input` record carries 96 bytes and the hour has about 58.
- **A wait nobody answers ends.** Twenty seconds after a sync with no screen, the source draws the
  newest held screen and says `no answer yet: the link may be busy or down`. Without that, a link
  out of budget would leave a screen that looks live and is not.
- **Leaving the mode sends what is held.** Turning it off clears the gate, the queue drains, and the
  view follows again — the bar says which it is (`send what is held and leave`). A mode that could
  be left while silently dropping what you typed is worse than one that sends on the way out.

### The local echo

What is held is **drawn at the cursor of the host's last screen**, so typing looks like typing:
characters appear where they would have, `Enter` starts a new line, `Tab` moves to the stop, and a
backspace erases. An earlier draft of this ADR rejected that as corrupting the emulator's truth.
That was wrong, and the reason it was wrong is worth writing down, because the objection is real if
the echo is written *incrementally*:

- **Every echo is rebuilt from the host's last screen, never painted onto the previous echo.** So
  the image is always exactly "the shell as it was, plus what you have typed since". Backspace,
  wrapping, a deleted character and a long line cannot make it drift, because nothing accumulates.
  The held text is remembered so that *every* paint puts it back: a repaint used to erase the echo
  while the bar went on counting the text, leaving the user counting something the screen no longer
  showed. It is sanitised on the way in, too — the host's rows are stripped of control characters by
  the screen encoder, and a pasted `ESC[2J` reaching the engine would clear the phone's copy of the
  host's screen (healed by the next paint, but a dropped byte is a smaller lie than a repaint).
- **Autowrap is off while the echo is written** (`\x1b[?7l` … `\x1b[?7h`). A fake line that wrapped
  would scroll the screen, and a row pushed into the local scrollback by a fake is history that
  never happened. With autowrap off the text stops at the last column instead.
- **Nothing is echoed while the view is parked in history**, where typing into the middle of rows
  above the live screen would be a fake of a screen the user is not looking at.
- **The host's screen always wins.** The next `Commit` — whether it is the answer to a sync or the
  frame after the mode is turned off — repaints the screen, which removes the echo. The one case
  that needed code is a Commit *identical* to the drawn screen: the source compares screens to skip
  a repaint, so an echo would have survived a sync answered by an unchanged screen. It now repaints
  when it was echoing.

What is **not** echoed is every named key the shell interprets: an arrow, `Escape`, `Home`, `End`,
`C-c`. Painting one locally would invent an effect that has not happened — the emulator has no
shell in it, and `Up` in a shell may recall history rather than move a cursor. They are counted in
the bar instead ("3 characters and 1 key"), which says what will be sent without pretending to know
what it will do.

**Prior art, checked rather than assumed** (2026-11-04):

- **VS Code's terminal "Local echo (Preview)"** for remote windows writes unconfirmed keystrokes
  *dimmed*, turns itself on above ~30 ms of latency, and **disables itself per program** for
  `vim`, `vi`, `nano` and `tmux` because predicting over a program that repaints the screen is
  unsafe. Both halves were taken: the echo is drawn faint (`ESC[2m`, rendered by the client), and
  it is not written at all on the alternate screen.
- **mosh** shows predicted characters *underlined* until the server confirms them, and the iOS
  mosh terminal's prediction design adds the rule that makes prediction safe: every prediction
  expires (at most one frame), nothing speculative lives indefinitely, and **nothing is predicted
  when the program has turned remote echo off** — its `confirmed_epoch` cannot advance at a
  password prompt, so a tentative character is not drawn there. Our echo expires differently but
  for the same reason: it is rebuilt from the confirmed screen on every change and any paint of
  the host's screen replaces it, so no fake outlives the next *paint*. Note the word: while the
  view is frozen the host's screens are kept rather than painted, so the base the echo is drawn on
  is as old as the freeze — which is what the bar's `view N behind` is counting, and the reason a
  frozen view should not be left frozen for long.
- **The half-duplex radio argument is the mode's premise**, and the numbers support it:
  LoRaWAN's `RECEIVE_DELAY1` is 1 s and no uplink may be broadcast during a receive window, and
  measured device-to-device LoRa round trips start at ~34 ms (SF7/500 kHz) at the physical layer
  alone. Input genuinely cannot be dribbled out per keystroke on this medium; batching is what the
  medium does.
- **What was not taken** is the prediction machinery itself: a held buffer is not a speculative
  character in flight. Nothing has been sent, so there is no server state to contradict, no rollback
  to compute and no acknowledgement to track. The simpler invariant — "the confirmed screen plus
  what has not been sent" — is the whole state.

**A limitation, stated rather than hidden:** the mesh profile's `Commit` carries no password flag
(the tailnet route's frame does), so the phone cannot know it is at a password prompt. Text typed
into one is echoed locally and shown, exactly as a terminal with echo on would show it. Closing
that gap is a protocol bump (a flag on `Commit`), which is why it is not in this change.

### Why one button can both send and receive

It cannot be a request/response, and it does not pretend to be. The mesh link is asynchronous:
sending queues records for the ARQ, and the host's answer arrives as its next `Commit` whenever the
screen changes. So the button is **release, then wait for the answer**.

**Two buttons were considered and are worse here.** "Send" and "receive" would be two ways to start
one wait: the host answers a send anyway, so a separate receive only means *holding back* a screen
the phone already has. It also gives the user two ways to be wrong — send without receive leaves a
stale view, receive without send shows a screen that predates the typing — where one button has one
sequence. What the two-button design was really asking for was *immediacy*, and the local echo
answers that directly: the text appears as it is typed, so pressing sync does not have to produce
an instant result to feel like it did something.

When nothing is waiting, the button does the only honest thing left: it draws the newest screen the
host already sent (`pullHeld`), and it is labelled **`show`** in that state rather than `sync`,
because a control should name what it does.

## What was rejected

- **A second queue or a mode flag inside the batcher.** The batcher's rules (urgency for a line end,
  idle and hold windows, backspace folding into pending text, the capacity refusal) are the product
  of measurement; a parallel "hold" queue would re-implement them or disagree with them.
- ~~**Writing the typed text into the emulator as a fake local echo.**~~ **Reversed** — it is
  implemented, and the section above says how it is kept from drifting. The objection held only
  against an incremental fake, which this is not.
- ~~**Auto-enabling the mode when the applied radio preset is a public one.**~~ **Reversed, with
  the objection kept**: the mode is forced on a public channel, and the objection's real complaint —
  that a mode which silences typing with no explanation reads as a broken app — is answered by
  saying so in three places instead of one: the bar is always drawn with `the radio is on a public
  channel`, Settings' radio row says `public channel · typing is held until you press sync`, and the
  local echo means typing is never silent even when it is not sent.
- **A protocol change to ask the host for history or for a screen on demand.** The link has no such
  message and the budget cannot afford one per sync. The freeze is entirely client-side.

## Losses and edges, stated rather than hidden

- **The buffer is per connection, and per shell.** A switch to another shell discards what was
  held, because the text was addressed to a shell that is no longer on screen and sending it into
  the next one would be a command the user never composed there. It says so for a few seconds. A
  rotation's `Resize` also clears the screen but is not a switch, so it does not discard.
- **The buffer is per connection.** Leaving the mesh terminal — to the shell list, to Settings, or
  by the app being killed — disposes the connection and the buffer with it. So does a link rebuild
  after a radio drop, which is the same disposal by a different route. The bar's own leave action
  sends first; the launcher button, a process death and a rebuild do not. A confirm-on-leave was not
  added: the launcher is the escape hatch from this screen, and a modal on the way out of a terminal
  is a trap. Recorded here so the next session does not rediscover it as a bug.
- **A shell that another device takes while text is held** answers the send with `NotLive`; the
  pump reports `Blocked`, the buffer stays, and the bar keeps showing it. Nothing is lost but
  nothing is sent either, which is the honest pair.
- **The view can be frozen for as long as the user leaves it frozen.** `heldUpdates` counts the
  screens that passed, so the bar can say how far behind it is, and the pull shows the newest. The
  rows that scrolled while frozen are still kept as scrollback: the host numbers them, so they are
  history whether or not the screen was drawn, and a build log watched through a freeze scrolls
  back to its beginning afterwards.
- **A sync that is answered by a screen identical to the frozen one** still ends the wait, and it
  repaints when it was echoing: without that, a fake line would survive the sync that was supposed
  to replace it.
- **A password prompt is echoed.** The mesh `Commit` carries no password flag, so the phone cannot
  mask it (the tailnet route can). Typed text at a prompt is drawn locally like any other, which is
  what a terminal with echo on does. A protocol flag on `Commit` would close it; that is a host and
  phone change with a version bump, deliberately not in this one.
- **The classification is read from the radio, not only from the dialog.** It used to be written
  only when the radio dialog read `SELF_INFO` or applied a preset, so an install that never opened
  that dialog was classified "public" — the safe default — and then *forced* into held typing while
  the bar said "the radio is on a public channel", which on a private tuple was simply false. The
  link reads `SELF_INFO` as part of its own handshake, so the terminal screen now hears the answer
  and saves it; `connectionKey` excludes the field, so saving does not rebuild the connection. A
  tuple changed by another app is still only noticed when this app reads the radio, and until then
  the previous answer stands — including "public", which is the cautious one. A radio classified
  *private* while the mode is on does not turn the mode off by itself: leaving is the user's
  decision, and the bar offers it once the mode stops being forced.
- **The mode is one buffer for one connection, not one per shell.** The connection carries one
  attached shell at a time (`Attach` per page), so a per-shell buffer would be a buffer for a shell
  the link is not on.

### One defect found while making the echo dim

Faint (`ESC[2m`) was **carried by the frame and never drawn**. The engine hands it over as flag bit
3 with the colour untouched — measured: a faint cell arrives with the same `255,255,255` foreground
as a normal one — so the client owns half of the attribute, and `drawnFg` ignored it. Every program
that de-emphasises text (`ls` extracts, a `fzf` list, a diff's context) was shown at full strength.
`drawnFg` now blends halfway to the cell's own background, which is also right on a reverse-video
cell because the drawn channels are already swapped. Test: `faint text is drawn halfway to its own
background`. That this was needed for the echo is incidental; it was a rendering defect before it.

## Tests

- `PublicChannelHoldTest` (10): the first screen always draws; frozen screens count instead; a sync
  with nothing waiting pulls instead of sending; the drain ends when the buffer is empty, not when
  the answer arrives; a wait times out once; a second press while waiting is allowed; turning the
  mode off clears the wait and the count.
- `MeshInputTest`: `discard` throws away text and keys; the held summary counts characters, keys and
  the records they will take, against `MeshTerm.MAX_INPUT`.
- `MeshHistoryTest` / `MeshHistoryShiftTest`: unchanged by this ADR apart from the shift rule (see
  `docs/MOBILE-UX-AUDIT.md` §scroll), which is what makes a mesh scroll show anything at all.
