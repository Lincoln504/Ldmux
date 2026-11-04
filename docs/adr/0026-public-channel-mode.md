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

**One mode, engaged explicitly, with one standalone control.**

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
  exists to prevent.
- **The view is frozen, and "sync" unfreezes it.** While the mode is on, a screen the host sends is
  *kept* (the newest one) rather than drawn. The sync button drains the buffer, and the first screen
  that arrives afterwards is drawn — the host's answer to what was sent. A frozen view never stops
  the link: the driver keeps polling and the session keeps its acks, because a frozen view is a
  view and not a disconnected terminal.
- **The bar says what is waiting and how stale the view is.** `12 characters and 1 key, about 2
  records · view 3 behind`. The record count is the number the user cannot otherwise see and the
  one that matters: an `Input` record carries 96 bytes and the hour has about 58.
- **A wait nobody answers ends.** Twenty seconds after a sync with no screen, the source draws the
  newest held screen and says `no answer yet: the link may be busy or down`. Without that, a link
  out of budget would leave a screen that looks live and is not.
- **Leaving the mode sends what is held.** Turning it off clears the gate, the queue drains, and the
  view follows again — the bar says which it is (`send what is held and leave`). A mode that could
  be left while silently dropping what you typed is worse than one that sends on the way out.

### Why one button can both send and receive

It cannot be a request/response, and it does not pretend to be. The mesh link is asynchronous:
sending queues records for the ARQ, and the host's answer arrives as its next `Commit` whenever the
screen changes. So the button is **release, then wait for the answer**, and the wait is the same
thing a separate "receive" button would do. Two buttons would be two ways to start one wait, and a
user pressing "receive" without having sent anything would get the same screen they already had.

When nothing is waiting, the button does the only honest thing left: it draws the newest screen the
host already sent (`pullHeld`). That is a real action on a frozen view — "show me where the shell is
now" — and it is why the control is never a dead button.

## What was rejected

- **A second queue or a mode flag inside the batcher.** The batcher's rules (urgency for a line end,
  idle and hold windows, backspace folding into pending text, the capacity refusal) are the product
  of measurement; a parallel "hold" queue would re-implement them or disagree with them.
- **Writing the typed text into the emulator as a fake local echo.** It is the most literal reading
  of "displayed locally", and it is wrong here: the engine is a faithful emulator of what the *host*
  said. Writing un-sent bytes into it corrupts the invariants every other feature relies on — the
  scrollback, the selection, the tap policy's cursor position, the local history's row identity. The
  pending text is a *view of the buffer*, drawn over the terminal, not a mutation of the terminal's
  truth. It also cannot leak a password: a held buffer in a password prompt is shown as a count, not
  as text, because the text never enters the engine.
- **Auto-enabling the mode when the applied radio preset is a public one.** Considered and refused:
  a mode that silences typing on its own is a mode whose first symptom is "the app stopped
  accepting input", with no way for the user to know why. The preset is knowable
  (`publicPresets(region)`), so this stays available as a *prompt* — a suggestion in the bar — if
  field feedback says it should.
- **A protocol change to ask the host for history or for a screen on demand.** The link has no such
  message and the budget cannot afford one per sync. The freeze is entirely client-side.

## Losses and edges, stated rather than hidden

- **The buffer is per connection.** Leaving the mesh terminal — to the shell list, to Settings, or
  by the app being killed — disposes the connection and the buffer with it. The bar's own leave
  action sends first; the launcher button and a process death do not. A confirm-on-leave was not
  added: the launcher is the escape hatch from this screen, and a modal on the way out of a terminal
  is a trap. Recorded here so the next session does not rediscover it as a bug.
- **A shell that another device takes while text is held** answers the send with `NotLive`; the
  pump reports `Blocked`, the buffer stays, and the bar keeps showing it. Nothing is lost but
  nothing is sent either, which is the honest pair.
- **The view can be frozen for as long as the user leaves it frozen.** `heldUpdates` counts the
  screens that passed, so the bar can say how far behind it is, and the pull shows the newest.
- **A sync that is answered by a screen identical to the frozen one** still ends the wait (the
  source treats "the same screen" as an answer). Nothing redraws, which is correct.
- **The mode is one buffer for one connection, not one per shell.** The connection carries one
  attached shell at a time (`Attach` per page), so a per-shell buffer would be a buffer for a shell
  the link is not on.

## Tests

- `PublicChannelHoldTest` (10): the first screen always draws; frozen screens count instead; a sync
  with nothing waiting pulls instead of sending; the drain ends when the buffer is empty, not when
  the answer arrives; a wait times out once; a second press while waiting is allowed; turning the
  mode off clears the wait and the count.
- `MeshInputTest`: `discard` throws away text and keys; the held summary counts characters, keys and
  the records they will take, against `MeshTerm.MAX_INPUT`.
- `MeshHistoryTest` / `MeshHistoryShiftTest`: unchanged by this ADR apart from the shift rule (see
  `docs/MOBILE-UX-AUDIT.md` §scroll), which is what makes a mesh scroll show anything at all.
