# Remote-terminal practice: input latency, echo prediction, and key encoding

**Status:** research + design. Nothing in this document is implemented except
where marked **Done**. Written 2026-09-14.

This is the "does it feel like a real remote terminal?" document. Everything
here is about the *input* path — how a keystroke becomes bytes, how fast the
result comes back, and what to do when the round trip is 150–250 ms.

## 1. Why this is a first-class problem

Mobile RTT to a home box is 150–250 ms on WiFi/5G and worse on LTE with a
tailnet hop in between. That is the same order of magnitude as the threshold at
which typing stops feeling direct: a character that appears a fifth of a second
after the key press reads as "laggy", even though nothing is broken. The prior
work in `dooboxes` measured exactly this class of problem and recorded the
targets (`docs/1_CORE_SPECIFICATIONS/TESTING_STRATEGY.md:535-566`): screen
update latency **< 100 ms p95** as a launch blocker, with 150–250 ms called out
as the *realistic mobile* figure to design against.

So the goal is not to make the network fast. It is to make the *perceived*
latency small.

## 2. What "proper remote terminal practice" actually consists of

In rough order of value per unit of work:

1. **Client-driven pane geometry** — the phone tells the server how big the
   screen is, the server resizes the pane. **Done** (`Resize` in
   `packages/protocol/src/messages.ts`; `resizeWindow` in
   `packages/daemon/src/tmux/control-plane.ts`).
2. **Key encoding on the client** — turn key events into the byte sequence the
   *application* expects, which depends on terminal modes the pane is in (cursor
   keys application mode, bracketed paste, mouse). **Deliberately not done, and
   now judged unnecessary for the prediction work** — see §5.1. We send literal
   text as bytes (`input`) and named keys (`key`), and tmux encodes the named
   keys with full knowledge of the pane's real modes, which is strictly more
   information than our snapshot carries.
3. **Predictive local echo** — show the keystroke before the echo returns.
   **Not done**; design in §5.
4. **Frame coalescing / adaptive rate** — do not repaint faster than the link
   can carry; do not let a flood starve input. `dooboxes`
   (`MOBILE_TERMINAL_PERFORMANCE.md:186-196`) specifies *max 50 Hz, min
   max(2 Hz, 1/2 RTT)*, explicitly so that buffer bloat cannot delay Ctrl-C.
   **Done** (`FramePacing` + `TerminalController.scheduleRender`). See §5.3 for
   where this deviates from the source and why.
5. **Atomic repaint on connect/resize** — a snapshot is applied as a unit so a
   late capture cannot paint over a newer one. **Done** (`snapshotEpoch`,
   `sendState`).
6. **Filtering DCS/clipboard/device-status out of the stream** — a hostile or
   buggy program on the remote side must not be able to write to the phone's
   clipboard or query the device through our transport. **Done**
   (`packages/daemon/src/sanitize.ts`).
7. **Bounded scrollback paging** — history is fetched in bounded chunks, never
   by one giant capture. **Done** (`MAX_HISTORY_LINES`, `scrollback`).

## 3. Prior art: what `dooboxes` already worked out

`dooboxes` has a complete, portable design for (3), dated 2026-02-08, in
`docs/4_RESEARCH/current/mobile/PREDICTIVE_ECHO_AND_TERMINAL_RENDERING_RESEARCH_FEBRUARY_2026.md`.
It is scoped as P2/post-MVP there (2–4 weeks, `MOBILE_TERMINAL_PERFORMANCE.md:103`)
and **not implemented** — but the hard thinking is done and is directly reusable.

### 3.1 The measured case (mosh's SSP)

`:96-104`:

| Metric | SSH | Mosh | Improvement |
|---|---|---|---|
| Median latency (EV-DO) | 503 ms | 5 ms | 99% |
| Mean latency (EV-DO) | 515 ms | 173 ms | 66% |
| Predictable keystrokes | 0% | 70% | — |
| Error rate | 0 % | 0.9 % | acceptable |

The mechanism (`:89-95`): idempotent diffs; **epoch-based confidence**; a
**server-side ~50 ms delay before echoing**, to give the client's prediction time
to land first (this is what removes flicker — the prediction is never painted
and then immediately contradicted); and sequence-number synchronisation.

### 3.2 The confidence algorithm (`:107-125`)

1. A new *epoch* starts tentatively; its predictions are held.
2. If **any** prediction in the epoch is confirmed, the whole epoch is displayed.
3. More correct predictions ⇒ higher confidence for the next epoch.
4. A hard-to-predict keystroke increments the epoch and **resets confidence**.

The property that matters: prediction is never a coin flip shown to the user —
it is a state machine that earns the right to be shown.

### 3.3 The prediction gate (`:127-142`)

**Predictable** (safe to echo locally): alphanumerics; common punctuation
`. , ; : ' " ! ? - _ = + [ ] { } ( ) @ # $ % ^ & * ~ \` | /`; space; TAB;
ENTER.

**Unpredictable** (must wait for the server): control characters other than
TAB/ENTER (Ctrl-C, Ctrl-D, Ctrl-Z …); navigation keys (arrows, PageUp/Down,
Home/End); function keys; **password mode**; **raw-mode applications** (vim,
top, …).

This gate is the whole safety story, and it is small enough to port verbatim.
`dooboxes` even wrote it out in executable form, `isPredictable(keystroke,
terminalMode)` with a `PREDICTABLE_PATTERNS` regex table (`:186-215`), plus a
`PredictionEngine` sketch (`addPrediction` / `confirmEpoch` / `loseConfidence` /
`rollback`, `:175-185`).

### 3.4 The sshx precedent (`:156-196`)

Sequence tracking with a **3-miss tolerance** before accepting the server's
number, and a two-tier rolling buffer (client 8 MB, prune at 12 MB; server 2 MB
per shell) for reconnect. Two ideas worth keeping: tolerate a small number of
disagreements before resynchronising, and bound the reconciliation state.

Its one directly relevant gotcha is a **scaling** warning, not an echo one: the
sshx xterm.js zoom patch (`:210-245`) records that CSS-transform scaling broke
font/canvas measurement and pointer→cell mapping, fixed by reading `clientWidth`
and dividing pointer coordinates by the transform scale. That is the failure mode
of any "just scale it to fit" path — relevant to our own canvas, which scales
only by rounding now (`TerminalSurface.android.kt`), and would need the same
correction if a zoom gesture is ever added.

## 4. What our transport gives us (and what it doesn't)

- Client → server: JSON `input {data}` (literal text) and `key {keys[]}` (named
  keys). Server → client: binary `Output` frames plus `snapshot` repaints.
- We have **no** server-side echo delay. mosh's 50 ms is a property of SSP's
  state-synchronisation model; adding an artificial delay to tmux's output would
  slow every other client and is not worth it. Our equivalent is to reconcile
  *after* the fact (§5.2).
- We do have something mosh does not: a cheap, exact **full snapshot on demand**.
  That makes a mispredict recoverable in one round trip, which is what makes
  prediction safe here.

## 5. Design for ldmux

### 5.1 Phase 1 — nothing to build: the input/key split *is* the gate

An earlier draft of this document proposed moving key encoding into the shared
core as a prerequisite for prediction. On inspection that is the wrong call, for
two reasons:

- tmux's `send-keys` with a *name* (`Up`, `C-c`, `Escape`) is encoded by tmux
  against the pane's **real** current modes. Our snapshot carries `appCursor`
  and `bracketedPaste` but not the whole truth, so a client-side encoder would
  be encoding from stale, partial state and would be *less* correct, not more.
- Prediction needs the **exact bytes** it is guessing ahead with. Our protocol
  already separates the two cases that matter:
  - `input {data}` — literal text typed by the user, byte-for-byte what the tty
    will echo. **Predictable.**
  - `key {keys[]}` — named keys, resolved server-side. **Unpredictable.**

The existing split therefore *is* the predictable/unpredictable gate of §3.3,
enforced by construction: control keys, arrows, function keys and anything else
with special meaning travel as `key` and are never predicted. Prediction is
restricted to literal text, which is exactly the safe subset — and the unsafe
subset (password entry is text, though) is handled by the echo-detection
heuristic in §7.

This is worth stating plainly because it removes what looked like the largest
piece of prerequisite work and replaces it with a rule about which message type
may be predicted.

### 5.2 Phase 2 — predictive echo

**Gate:** predict only `input` (literal text), never `key`; only on the normal
screen; and never at a password prompt. That last one needs no heuristic: the
emulator reports `GHOSTTY_RENDER_STATE_DATA_CURSOR_PASSWORD_INPUT`, which the
frame header now carries to the client. The timeout heuristic below stays as a
second line of defence for panes where the flag is not set.

**Paint:** feed the predicted bytes into the local `libghostty-vt` engine
immediately, so the character appears with the key press. Record it as a
pending prediction in the current epoch.

**Reconcile on the next server output**, by prefix match:

- output **starts with** the pending bytes ⇒ the prediction was right; discard
  the prediction and feed only the remaining bytes. Nothing is double-printed
  and nothing flickers.
- output **does not** ⇒ the prediction was wrong (the application did something
  else with the key). The predicted bytes are already in the local engine and a
  VT cannot un-apply them, so **request a fresh snapshot** and repaint. One
  round trip, and only on a mispredict. With a correct gate this is rare, and
  mosh's measured 0.9 % error rate is the expected order.

**Confidence:** keep the epoch state machine from §3.2 rather than a boolean.
Held epochs mean a shaky prediction is never shown; confirmed epochs make the
next one immediate.

**Rollback for free:** because every snapshot rebuilds the engine from scratch
(`TerminalController.onSnapshot`), "discard the prediction" is one snapshot
request, not a diff-undo. The architecture already has the recovery primitive.

### 5.3 Phase 3 — adaptive frame rate (done)

Coalesce output frames and repaint on a schedule bounded by the link, so a
`yes`-style flood cannot starve input. Implemented in `FramePacing`
(`commonMain/.../FramePacing.kt`), and consumed by `TerminalController.scheduleRender`.
RTT comes from the `ping`/`pong` the protocol already had: `AttachClient` sends a
ping every 4 s on the *same* connection the output arrives on, so the number is
the delay the user actually experiences. It is shown in the sidebar
(`link 42 ms · 30 fps`), because a number the user can see is the only way to
tell "this link is slow" from "this app is slow".

| | dooboxes' note | here | why |
|---|---|---|---|
| ceiling | max 50 Hz | **30 Hz** (`33 ms`) | Terminals stream text, not video. Input responsiveness is protected by coalescing and the predictor, not by painting faster; 60 Hz just wakes the CPU. |
| floor | max(2 Hz, ½ RTT) | **6.7 Hz** (`150 ms`) | 2 Hz reads as broken rather than slow, and below the floor the predictor is the better answer to latency than more draws. |
| estimate | ½ RTT | **½ RTT, asymmetric** | A pong queues behind whatever is already in flight, so one slow sample says nothing about the link: a *lower* sample is adopted whole, a higher one is absorbed at 0.25. |

Scrolling is exempt: it is a local view change, so it paints immediately
(`renderNow`) and never goes through the pacing window.

### 5.4 Scrollback is the emulator's, not a client-side buffer (done)

Not in dooboxes' list, but part of the same honesty about where the truth lives.
libghostty-vt owns the scrollback and exposes a viewport-scroll API
(`ghostty_terminal_scroll_viewport` with TOP/BOTTOM/DELTA/ROW tags) plus a
scrollbar struct (`GHOSTTY_TERMINAL_DATA_SCROLLBAR`, `{total, offset, len}`). The
client scrolls the *emulator's* viewport and the existing render path simply
shows the result — there is no second history buffer to keep in sync with the
pane, and no per-swipe round trip. History is paged in once (`scrollback` with a
line count) and then scrolled locally, which is only possible because the
scrollback lives in the engine.

## 6. Measuring it (so "feels fast" is falsifiable)

A harness now exists: `scripts/measure_latency.ts` (`just latency <sid> [runs]`).
It types a unique marker through the real path (client → daemon → tmux → pty →
shell echo → back) and reports the distribution.

**Measured 2026-09-14, over `adb reverse` (loopback): p50 1.0 ms, p95 3.3 ms,
max 3.3 ms over 12 runs.** That is a *floor*, not a mobile number — it excludes
the network entirely, and its value is as a regression baseline for the client
and daemon (if this moves, we broke something local). The number that matters is
the same tool pointed at the tailnet address (`--url ws://…`), which is not yet
measured.

1. **Round-trip echo latency:** as above. This is the dooboxes SLA metric.
2. **Key-to-paint latency (local):** timestamp at key dispatch and at the
   Compose frame containing the predicted character — the number prediction
   improves; not measured yet.
3. **Mispredict rate:** mispredicts / predictions. If this is not well under a
   few percent the gate is wrong, and the honest answer is to predict less.

None of this is implemented; there is no latency harness yet.

## 7. Open questions

- **Password/echo-off detection — SOLVED, and better than expected.** An earlier
  draft proposed a timing heuristic (no echo in N ms ⇒ stop predicting) because
  a tmux capture cannot show the remote's ECHO flag. That turns out to be
  unnecessary: libghostty-vt's render state exposes
  `GHOSTTY_RENDER_STATE_DATA_CURSOR_PASSWORD_INPUT` (`render.h:187-188`), i.e.
  the emulator *already knows* when the cursor is at a field that will not echo.
  The frame header now carries it (byte 16) and `TerminalController.sendText`
  refuses to predict when it is set. The timing rule is kept only as a fallback.
  This is a case where the engine is strictly better informed than the protocol.
- **Prefix matching across chunk boundaries.** Implemented (the reconciler keeps
  the unmatched tail of the prediction and treats an exhausted output as a
  partial confirmation, not a failure) and unit-tested. Still unbounded in the
  pathological case: a prediction that is neither confirmed nor diverged keeps
  its bytes until the stale timer fires, which is bounded at 900 ms.
- **Interaction with line editing.** readline redraws a line on completion and
  on history navigation; those are "unpredictable" by the gate, so the risk is
  bounded, but a completion menu is exactly where a prediction would look
  wrong.
- **Where the gate lives.** It must be shared (KMP core), because iOS needs the
  identical behaviour; "never split engines" applies to input policy too.

## 8. Sources

- `dooboxes/docs/4_RESEARCH/current/mobile/PREDICTIVE_ECHO_AND_TERMINAL_RENDERING_RESEARCH_FEBRUARY_2026.md`
  — algorithm (`:107-125`), rules (`:127-142`), `isPredictable` (`:186-215`),
  mosh table (`:96-104`), sshx zoom patch (`:210-245`).
- `dooboxes/docs/1_CORE_SPECIFICATIONS/TESTING_STRATEGY.md:535-566` — latency SLA.
- `dooboxes/docs/1_CORE_SPECIFICATIONS/MOBILE_TERMINAL_PERFORMANCE.md:103,186-196`
  — P2 scope, adaptive frame rate.
- `dooboxes/docs/4_RESEARCH/current/misc/SSHX_COMPREHENSIVE_TECHNICAL_ANALYSIS_FEBRUARY_2026.md:885-903`
  — client-sends-winsize.