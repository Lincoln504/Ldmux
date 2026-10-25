# ADR-0022 — The terminal answers its own queries; modified Enter uses the Kitty keyboard protocol

**Status:** Accepted (2026-10-24).

- **Date:** 2026-10-24
- **Related:** ADR-0001 (the sanitizer), ADR-0013 (key names), `docs/SPEC.md` §5,
  `crates/keeper/src/emulator.rs`, `crates/daemon/src/stream.rs`,
  `packages/mobile/composeApp/src/commonMain/kotlin/dev/ldmux/mobile/Modifiers.kt`

## Context

Reported on a phone: `Shift+Enter` in `pi` submitted the line instead of inserting a
new line. Reproduced in the code and on the device (2026-10-24), and the cause is a
chain of three separate things, each correct-looking on its own:

1. **No component answered the terminal's capability query.** `pi` negotiates the
   Kitty keyboard protocol: it pushes flags (`CSI > 7 u`) and asks what the terminal
   has (`CSI ? u`). Its negotiation is response-driven — no answer means "this
   terminal cannot tell `Shift+Enter` from `Enter`". The daemon's sanitizer drops
   `CSI ? u` (and DA1, DSR/CPR, DECRQM, XTVERSION, the size reports) before any
   client sees it, because a client's answer would arrive at the shell as typed
   input (ADR-0001). The desk window's real terminal would have answered; the
   phone's engine had no write-back; and the keeper — which owns the emulator and
   the pty — registered no write-back either. So the answer did not exist anywhere.
2. **`Shift+Enter` had no encoding.** The protocol's own key table maps `Enter` to
   `CR` whatever the modifier, and the phone's client dropped Shift on Enter for the
   same reason ("shift sends the same byte"). Under the Kitty protocol the same key
   is `CSI 13 ; 2 u`.
3. **The client had no way to produce Shift at all.** The bar's modifiers were Ctrl
   and Alt, latched for one following key (a phone cannot hold a chord). Shift was
   not among them, so a newline was unreachable from the on-screen bar even with the
   protocol working.

The same gap broke every other query a full-screen program asks: a cursor-position
report, a device-attributes request, a mode report. Programs block on these or guess.

## Decision

**The terminal answers its own queries, and it is the keeper's emulator.** libghostty-vt
implements every reply (`GHOSTTY_TERMINAL_OPT_WRITE_PTY`) but only through a
write-back callback, which nothing registered. The keeper now registers one, with the
pty master as the userdata, on the thread that owns the master. The sanitizer keeps
dropping queries before a *client* sees them; there is exactly one answerer, so no
query is answered twice and no answer arrives at a shell that did not ask.

Consequences:
- DA1, DSR/CPR (`CSI 6 n`), DECRQM, XTVERSION, the Kitty query, mouse-mode and
  alternate-screen reports are all answered, for the desk window and the phone alike,
  in the identity of the emulator that actually holds the grid.
- A query is answered from inside `Terminal::write`, so the reply is a `write(2)` to
  the master and nothing else — the header forbids re-entering the terminal from a
  callback.

**A reply is not content, so it never re-enters the emulator.** The write-back writes
the answer into the pty master, which is the program's *input*. A program that echoes
what it reads (a `cat`, a pager, a shell with echo on) turns the answer back into
output, the emulator parses it, and the parameterised reply form (`CSI ? <flags> u`)
is query-shaped — so it answers it again, and again: measured 2026-10-25 at about
sixty replies a second, filling the pane and the wire. The keeper therefore drops the
parameterised form from the output stream before the emulator sees it
(`without_kitty_replies`). The *question* is the bare `CSI ? u`, the *push* is
`CSI > <flags> u`, and neither is touched.

**The daemon encodes a modified Enter/Escape/Space as `CSI u` when — and only when —
the program has the protocol on.** `key_name_to_bytes_in_mode` takes the flags the
keeper's emulator reports, and for those three keys (the only ones with no legacy
shifted byte) emits `CSI <codepoint> ; <1+modifiers> u`. Every other key keeps its
legacy encoding, which protocol parsers still accept and older programs require.
Without the flags bit 1 (disambiguate) the encoding is unchanged, so a program that
never asked cannot be sent a sequence it would type at itself.

**Shift joins the sticky modifiers.** `Modifiers` gains `shift`, `StickyKey` gains
`Shift`, and the wire spelling is `M-C-S-<key>` (Shift innermost). `S-Enter` on a
daemon without the protocol collapses to `Enter`, so the name is always safe to send;
an older daemon never sees a sequence it cannot encode. The bar gains a `⇧` key, and
`⇧` then `⏎` is a newline.

## Alternatives rejected

- **Let the client answer.** The phone's engine has no write-back path at all, and the
  desk window would answer as *the desk's* terminal, so the same session would report
  two different terminals depending on who is looking. The grid is the keeper's, so
  the answers are the keeper's.
- **Stop sanitizing queries** (forward them and let clients reply). The reply then
  travels back as `input`, which is the injection ADR-0001 exists to prevent, and the
  phone would still answer nothing.
- **Answer with modifyOtherKeys instead** (`CSI > 4 ; 2 m`). `pi` supports it, but it
  is a dead end: only modified keys, no event types, no alternate keys, and it is not
  what a modern terminal reports.
- **A `Shift` button that sends `LF`.** A raw `0x0A` is `Ctrl+J`, which many programs
  treat as submit or as a literal newline for reasons of their own; `pi` cannot tell
  it from `Ctrl+J` (`docs/terminal-setup.md`). The key must be a *distinct* key, not a
  byte that happens to work.

## What it costs

Nothing on the wire, and one pty write per query. The keeper parses nothing extra
(the engine already parsed the push); it reads one state field (`KITTY_KEYBOARD_FLAGS`)
for the same `Read` it already made for the cursor-key mode. An older keeper reports
no flags (the field defaults to 0), so a new daemon against an old keeper behaves
exactly as before.

## Verification

- `crates/keeper/tests/keeper.rs`: the query is answered with the pushed flags (both
  before and after a push), a cursor report and a device-attributes query are
  answered, and an echoed reply is never answered again.
- `crates/daemon/src/stream.rs`: `S-Enter` → `CSI 13;2u` only with the flag on.
- `crates/daemon/tests/kitty_keys.rs`: end to end — a program pushes the flag, the
  client sends `S-Enter`, the pane receives `CSI 13;2u`; plain Enter is still `CR`.
- On the device: the bar renders `⇧`, the arrow cross and `paste` (2026-10-24).
