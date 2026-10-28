# ldmux-pi-voice

A pi extension for voice mode (ADR-0022, `docs/VOICE-MODE.md`). Inert unless pi runs inside an
ldmux shell (`LDMUX=1`, `LDMUX_SOCKET`, `LDMUX_SESSION_ID`).

- `/voice on|off` toggles it; the state is stored with `pi.appendEntry("ldmux-voice", ...)` and
  restored from the session branch on `session_start`.
- When on, a prompt section asks for one closing fenced block tagged `say` (a one-to-three sentence
  spoken summary). On `message_end` the block is cleaned (control characters dropped, capped at
  1000 characters) and sent to the daemon's `speech` RPC. A markdown transformer hides the block
  from the transcript, including while it streams in.
- A daemon that is down is reported once per toggle, never thrown.

Install: `pi --extension /path/to/packages/pi-voice-extension/index.ts`, or place the directory
under `~/.pi/agent/extensions/`.

Test: `npm test` (or `node --test test/*.test.ts`; Node 24+ runs the TypeScript directly).
