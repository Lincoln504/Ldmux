# ADR-0024: A pi extension returns, for voice only

**Status:** Accepted (2026-10-28). Amends the "no pi extension" half of ADR-0009.

**Update 2026-10-29.** The extension has now been observed inside a real pi 1.4.0 (RPC mode, an isolated
agent directory, a stub model and a stub daemon socket): load, `/voice on` persisted and restored,
the prompt section present only while on, exactly one `speech` RPC, inert outside ldmux, and a
dead socket warned once. Evidence and harness: `packages/pi-voice-extension/test/live/live.sh`,
`docs/VOICE-MODE.md` section 15. The consequence "the fenced block stays in the model's context"
below was confirmed: the stored assistant message still contains it. Original text unchanged.

- **Related:** ADR-0009 (removed the pi extension), ADR-0010 (ownership), ADR-0013 (platform
  seams), `docs/VOICE-MODE.md` (section 3.5 is the decision of record for the channels)
- **Scope:** `packages/pi-voice-extension`, the daemon `speech` RPC, the phone's voice mode.

## Context

ADR-0009 removed `packages/extension` on the grounds that ldmux is a remote terminal and not a
pi plugin: the extension only existed so pi sessions registered themselves, and every other
route (`new`, `adopt`, the mobile and web clients) works without it. That half of ADR-0009 still
stands for **session registration**: nothing here registers sessions.

Voice mode needs something else. The agent's spoken summary is the agent's own words, and
only pi knows which part of its answer is meant to be said. Scraping the terminal grid for it is
rejected in `docs/VOICE-MODE.md` section 3.4, and pi cannot emit a private escape sequence
(its TUI owns stdout). The user decided on 2026-10-28 to keep pi as the agent and to add an
extension for voice only.

## Decision

1. **A pi extension exists again, for voice only** (`packages/pi-voice-extension`). It registers
   `/voice on|off`, adds a prompt section requiring one fenced `say` block, reads the finished
   answer, and sends the block to the daemon's `speech` RPC. It registers no sessions, no tools
   and no UI beyond the command.
2. **It is inert outside ldmux.** If `LDMUX` is not `1`, or `LDMUX_SOCKET` or `LDMUX_SESSION_ID`
   is missing (the daemon exports all three, `daemon.rs`), the factory registers nothing.
3. **The daemon relays and holds no speech logic.** One RPC, one server-to-client message behind
   the `speechChannel` client feature (additive: a client that says nothing never gets one).
4. **Two producers, one event.** The extension is Channel A. Channel B, a private OSC 1338 in the
   byte stream, is parsed by the phone and covers any other program, including an own-CLI agent
   later. The phone has one handler.
5. **Non-goals stand:** no wake word, no silent cloud recognition, no second terminal renderer.

## Consequences

- **+** pi is voiced with exact text, no terminal parsing, and a redraw cannot corrupt it.
- **+** Replacing pi later removes the extension and nothing on the phone or daemon.
- **−** ldmux now depends on three pi extension hooks (`before_agent_start`, `message_end`,
  `registerMarkdownTransformer`). They are in pi's typings as of 2026-10-28
  (`dist/core/extensions/types.d.ts`), but they are pi's contract, not ours.
- **−** The model must obey a prompt instruction to emit the block. A turn without one is simply
  not spoken. The fence is chosen over a `speak` tool because a tool call costs a whole extra model
  round trip per turn.
- **−** The fenced block stays in the model's context (only the transcript display hides it).
