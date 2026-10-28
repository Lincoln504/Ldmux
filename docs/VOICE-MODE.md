# Live voice mode: hands-free conversation with a coding agent

> **Status, 2026-10-28: partly implemented.** Built and tested: the daemon `speech` relay and
> `ldmux speak`, the phone's OSC 1338 parser and `speech` message parser, `VoiceMode`, the
> chunker, `VoiceDriver`, an Android `TextToSpeech` speaker, and the pi extension
> (`packages/pi-voice-extension`, ADR-0024). **Not built:** the UI toggle that creates a
> `VoiceDriver`, a separate voice-activity detector (Android's `SpeechRecognizer` endpoints on its
> own), iOS, on-device verification of any of it, and real-pi verification of the extension
> beyond unit tests with a faked pi API. See section 15. Written 2026-10-06. This is the plan of record for a future feature; it is deliberately
> not an ADR, because no decision has been taken. Before implementation it needs
> an ADR (a new one, plus an amendment to ADR-0009's "no pi extension" half) and
> a `docs/SPEC.md` section. Every claim about the current code below names how it
> was checked.

This is the "talk to the agent and hear it answer" document. It covers the whole
loop: what the agent says, how that text reaches the phone, how the phone listens
and speaks, how it behaves across several open shells, and what must not be
sacrificed (battery, privacy, scroll fidelity, one renderer).

## 0. The shape in one screen

- **The phone is the voice.** Microphone, voice-activity detection, speech
  recognition, text-to-speech, barge-in and audio focus are all platform
  concerns, behind shared seams (ADR-0013). None of it goes on the host.
- **The agent decides what is said.** A pi extension keeps the written answer
  verbose and adds a short spoken summary, and hands the exact text to the
  daemon. The phone never scrapes the terminal for it.
- **The daemon is a relay.** One new RPC method and one new server-to-client
  message. No speech logic on the host.
- **The phone drives.** Voice mode is an app-level mode bound to one shell at a
  time. Switching shells rebinds it. The agent does not have to know the phone
  exists beyond publishing speech text.
- **Android first, iOS-shaped.** Android is the only target now. Every platform
  piece is a leaf behind a seam whose declaration is the shared concept, so iOS
  is an `actual`, not a port (ADR-0013).

## 1. Goals and non-goals

### 1.1 Goals

- **Hands-free, robust, two-way.** Speak a request, hear the answer, and the
  microphone comes back on its own. No button between turns in the common case.
- **The spoken form is not the written form.** The transcript stays detailed;
  the voice channel is a short, natural summary. A spoken monologue of a diff is
  worthless.
- **Exact text, not a screen scrape.** What is spoken is the assistant's own
  words, extracted from the message, not re-derived from a terminal grid.
- **Works across the open shells.** Voice mode is bound to the shell on screen
  and follows the user when they switch; a finished turn in another shell can be
  announced without stealing the microphone.
- **Battery-honest.** The recogniser and the microphone are gated by
  voice-activity detection and released while the agent is thinking.
- **One renderer, one seam.** libghostty-vt stays the only terminal renderer
  (AGENTS.md). Voice adds no second engine.

### 1.2 Non-goals

- Not a wake word. Voice mode is explicit; it does not listen for "hey ldmux"
  while the app is closed.
- Not a cloud assistant. On-device recognition and synthesis are the default;
  cloud recognition is a fallback that the user is told about, never a silent
  default.
- Not a replacement for the terminal. Voice is a mode over the same sessions,
  and the written transcript remains the record.
- Not a second pi integration path. This is the pi extension the project removed
  in ADR-0009, coming back for a different reason and a different job.

## 2. What exists today (ground truth)

- **A one-shot dictation seam, input only.** `voice/Voice.kt` (`VoiceState`,
  `expect object Dictation`, `VoiceBanner`) and `voice/Voice.android.kt`
  (`SpeechRecognizer`, on-device where the platform has it). The mic button is in
  the landscape side panel only (`SidePanel.kt:211`), wired at
  `AppContent.kt:726-734`, and it types text into the PTY without pressing Enter.
  Portrait has no mic on purpose (`KeyBars.kt:137`).
- **No TTS, no VAD, no continuous listening, no `AudioRecord`.** A grep for
  `TextToSpeech|AVSpeechSynthesizer|Silero|VAD|AudioRecord|SilenceDetector` over
  `packages/mobile/composeApp/src` finds no implementation. The output direction
  is greenfield.
- **The phone sees raw pre-emulator PTY bytes** at `TerminalController.feed`
  (`TerminalController.kt:359`) before `engine.write`, and it also has the
  rendered cell grid.
- **OSC 52 is the template for an in-band channel**, and it is emulator-mediated:
  a native callback fills a slot (`ldmux_terminal.c:169-213`) that Kotlin drains
  (`TerminalController.kt:363-378`). The generic
  `GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE` callback exists and is unused
  (`terminal.h:1479-1497`).
- **The daemon sanitizer passes every OSC except 52 and colour queries**
  (`sanitize.rs:216-236`), so a private OSC already reaches the phone with no
  wire change.
- **Every session exports `LDMUX_SESSION_ID`, `LDMUX_SOCKET`,
  `LDMUX_KEEPER_SOCK`, `LDMUX=1`** (`daemon.rs:2436-2454`,
  `keeper/src/lib.rs:294-320`). A pi extension already knows its session and how
  to reach the daemon.
- **The daemon can push JSON to attached clients** (`Out::Json`, `send_json`,
  the `Shared.conns` table in `remote.rs:276,286,318`), as `session_ended` and
  `broadcast_sessions` already do. A new server-to-client message is a contained
  addition.
- **The daemon RPC is one JSON line over a Unix socket** (`protocol/src/socket.rs`),
  so an extension can speak it directly with Node `net`, no CLI dependency.
- **SPEC §4.5 already reserves a semantic pi-RPC `SessionDriver`** and says v1 is
  terminal-first; it is unimplemented.
- **ADR-0009 removed the pi extension** ("the no-pi-extension half still
  stands"). A voice extension is a different thing (a pi-side capability, not
  ldmux registering sessions), but the ADR must be amended rather than ignored.

## 3. The channel problem, and why it decides everything

The hard part is not the audio. It is: **how does the agent's spoken text get to
the phone, exactly?** Three channels exist. Only one is both robust and emittable
from pi today.

### 3.1 Channel A — a semantic side channel through the daemon (recommended)

```
pi extension --(JSON line over LDMUX_SOCKET)--> ldmux daemon --(new `speech` WS message)--> phone
```

- The extension has the exact assistant message (`message_end`), extracts the
  spoken part, and sends `{sessionId, text, id, final, seq}` to the daemon.
- The daemon forwards a `{"type":"speech", ...}` message to every connection
  attached to that session, behind a client feature flag (`speechChannel`), the
  same additive rule as `sessionEnded` (SPEC §6.2).
- Exact, typed, testable, survives any TUI redraw, and needs no terminal parsing.
- Cost: a new RPC arm, a new outbound message, a new client parse, and the
  extension knowing ldmux exists.

### 3.2 Channel B — a private OSC in the byte stream (universal, but not from pi)

- A program prints e.g. `ESC ] 1338 ; {"v":1,"kind":"say","text":"..."} BEL`. The
  daemon sanitizer passes it, the phone sees it in `feed`, and a small parser in
  Kotlin surfaces it.
- It needs **no wire change and no daemon change**, and it works for *anything*
  in a shell that prints the sequence, which is the "works with anything" wish.
- **It cannot be emitted by a pi extension.** pi's extension API has no raw
  terminal write; the TUI owns stdout and a raw write risks desyncing its diffing
  renderer. So Channel B is a bonus for other programs, not the pi path.
- Choose an OSC number libghostty does not already claim (52/1337/5522 are
  clipboard; 0/2 title; 7 pwd; 8 hyperlink; 9/777 notification; 9;4 progress).

**Corrected 2026-10-28: the mechanism.** This document previously said the
emulator's `GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE` callback would surface a
private OSC. It would not. `terminal.h:345-356` states that **only APC sequences
are reported**, and OSC 52 has its own dedicated callback
(`GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE`). The real mechanism is the raw byte
stream: `TerminalController.feed` receives the bytes before `engine.write`
(`TerminalController.kt:397-398`), so a Kotlin parser sees the sequence with no
native change at all.

**And APC is not an alternative.** It is the sequence type the engine does
report, and it is tempting for exactly that reason, but the daemon drops every
DCS/SOS/PM/APC string whole (`crates/daemon/src/sanitize.rs:12,163`; test
`drops_dcs_sos_pm_apc_strings`). Whitelisting one APC would reopen the class of
hole that policy closes, and APC is how kitty sends terminal *commands*. So the
in-band channel is an OSC, and the phone parses it, or it does not exist.

**Guards, because the byte stream is arbitrary code's output:**

- Speak only when voice mode is on, the session is bound, and it is on screen.
- Cap the payload; drop an over-long or malformed sequence rather than truncating
  it into something that gets spoken.
- Handle a sequence split across two `feed` chunks (the sanitizer already keeps
  per-stream state for exactly this reason).
- Version the payload; ignore an unknown `v` or `kind`.
- Never feed speech text back as input.

### 3.3 Channel C — a second stream on the host (does not exist)

- A PTY has exactly one output. There is no fd 3 to read: the keeper allocates a
  pty pair and gives the child a single slave. A second stream means a second
  socket, which is Channel A with more moving parts.

### 3.4 Rejected channels

- **Grid scraping.** Read the rendered cells for a sentinel. Wrapping, scrollback,
  differential redraws and escape interleaving make this a reimplementation of
  transcript extraction that rots against pi's TUI. Rejected.
- **The window title as a data channel.** `ctx.ui.setTitle` is the only raw-ish
  emitter an extension has, but it is a single value, truncated in practice, and
  clobbered by the shell and by pi. Rejected.
- **pi RPC only (SPEC §4.5).** Fully semantic and the best long-term answer, but
  it is a new session kind with no TUI and a large subsystem. It is Phase 4, not
  Phase 0.
- **`process.stdout.write` from the extension.** Undocumented, interleaves with
  the renderer, and is exactly the fragility this document exists to avoid.

### 3.4a A and B in plain terms

| | Channel A (daemon message) | Channel B (private OSC) |
|---|---|---|
| Who can emit it | the pi extension, exactly | any program in the shell |
| Reaches the phone via | a new typed WS message | the raw byte stream |
| Host work | new RPC + outbound message | none |
| Phone work | parse a JSON message | parse an escape sequence |
| Robust against TUI redraws | yes | yes (it is not rendered) |
| Exact text | yes | yes |
| Works for non-pi programs | no | yes |
| Emittable by a pi extension today | yes | **no** |

### 3.5 Decision, 2026-10-28

- **The pi path is Channel A.** pi is the agent by decision, and pi cannot emit
  an OSC, so A is the only path that can carry its speech. One RPC arm, one
  server-to-client message, behind a client capability.
- **Channel B is built as well, and it is the same event.** The phone has one
  handler with two producers: the typed message from the daemon, and an OSC parsed
  out of the byte stream. Any program in the shell can therefore be voiced, and
  an own-CLI agent later needs no protocol work at all.
- **They are not redundant.** A covers the agent, which is the product. B covers
  everything else, costs no host code, and is the only path that survives
  replacing pi with a program we write.
- **Not a protocol field for speech.** The transport should carry what the
  transport must interpret. Speech needs no ordering, priority or delivery
  guarantee beyond the stream it is already in, and a field would make speech
  impossible for any program that is not speaking the RPC.

## 4. Architecture and separation of concerns

```
┌───────────────────────── host ──────────────────────────┐
│  pi  +  voice extension                                 │
│    decides what is said; emits exact text               │
│    → one JSON line on LDMUX_SOCKET                      │
│  ldmuxd  (relay only)                                   │
│    speech RPC → speech message to attached clients      │
│    existing input path ← the user's dictated words      │
└─────────────────────────────────────────────────────────┘
                          │ tailnet WS
┌──────────────────────── phone ──────────────────────────┐
│  commonMain  (the brain, testable)                      │
│    VoiceMode state machine, endpointing, barge-in,      │
│    chunking, multi-session binding, submit policy       │
│  androidMain / iosMain  (thin leaves)                   │
│    VoiceCapture  mic + VAD + ASR                        │
│    VoiceOutput   TTS                                    │
│    VoiceAudio    focus, AEC, routing                    │
└─────────────────────────────────────────────────────────┘
```

- **The extension owns meaning.** It is the only component that knows what a
  spoken summary is.
- **The daemon owns transport.** It must not learn what speech is.
- **`commonMain` owns policy.** Everything decidable once lives there (ADR-0013):
  thresholds, timeouts, when to submit, when to reopen the mic, which shell is
  bound, how to chunk.
- **Platform owns the OS.** Each platform half should be a five-line delegation.

## 5. The pi extension

- **Location.** A new `packages/pi-voice-extension/` (or an installed extension
  under `~/.pi/agent/extensions/`); the repo should own the source either way.
- **Toggle.** `pi.registerCommand("voice", ...)` for `/voice on|off`, with the
  flag persisted through `pi.appendEntry` and reconstructed from
  `ctx.sessionManager.getBranch()` on `session_start` (the documented pattern).
- **Instructions.** When on, a `before_agent_start` handler appends a prompt
  section: the written answer stays detailed, and the response must end with one
  fenced ` ```say ` block holding a short conversational summary. This is the
  "surrounded by certain characters" the user asked for, parsed **by the
  extension**, not by the phone.
- **Extraction.** A `message_end` handler takes the exact assistant text, pulls
  the fenced block, and sends it to the daemon. A fence is preferred to a
  `speak` tool because a tool call costs a whole extra model round trip per turn,
  which is latency the spoken loop cannot afford.
- **Cleanliness.** `pi.registerMarkdownTransformer` can strip the fence from the
  rendered transcript, so the screen does not fill with the spoken copy.
- **Transport.** Prefer speaking the daemon socket directly (Node `net`, one JSON
  line, one response line) using `LDMUX_SOCKET`, with `LDMUX_SESSION_ID` as the
  session. Fall back to a `ldmux speak` CLI for scripting and debugging.
- **Guard.** Publish only when `LDMUX=1`, so a pi outside ldmux is unaffected.

## 6. Daemon protocol additions

- **RPC:** `handle` gains `"speech"` with `{sessionId, text, id?, final?}`,
  validated (session exists, text non-empty, size cap).
- **CLI:** `ldmux speak <id> [--final] [--stdin]`.
- **Server-to-client:** `{"type":"speech","sessionId","id","text","final","seq"}`,
  sent to every connection attached to that session through the existing
  `Out::Json` path, and only to connections that advertised the `speechChannel`
  feature. A client that says nothing gets nothing, per the additive rule.
- **Ownership:** speech is delivered to viewers of the session; only the holder
  may open a microphone or submit on it (ADR-0010).
- **No daemon speech logic.** No chunking, no summarising, no voice state.

## 7. Mobile shared core

- **A `VoiceMode` state machine in `commonMain`:**
  `Idle → Listening → Capturing → Submitting → Thinking → Speaking → Listening`,
  with `Interrupted`, `Denied`, `Unavailable` and `Failed` terminal states, and
  the existing `ListeningCloud` honesty state reused.
- **Endpointing policy:** VAD start opens recognition, silence for a tuned hangover
  finalises, an empty or stop-phrase transcript is discarded, a long silence
  leaves the mode.
- **Submit policy:** dictated text is sent as `input` and then `Enter` (the
  existing seam types without submitting), gated on holding the session.
- **Mic policy:** released while the agent is thinking (the user's explicit
  requirement), reopened on the speech event or on a timeout, and muted while TTS
  plays unless barge-in is active.
- **Multi-session binding:** voice mode is app-scoped (like the existing `voice`
  state at `AppContent.kt:530`) and bound to the shell on screen. A `speech`
  event for the bound shell is spoken; one for another open shell is queued and
  announced when the user switches to it (an optional "check in on other shells"
  behaviour, off by default). The phone is the driver; the agent is not asked
  which shell is "active".
- **Chunking:** split on sentence boundaries for TTS queueing, and cap the
  spoken length so a runaway answer cannot monologue.
- **Safety:** speech text is untrusted agent output. It is spoken, never executed,
  and never fed back as input.

## 8. Platform seams (Android now, iOS-shaped)

| Seam | Shared concept | Android actual | iOS actual (later) |
|---|---|---|---|
| `VoiceCapture` | "turn microphone audio into partial and final text, gated by speech activity" | `AudioRecord` + Silero VAD ONNX, driving a restart loop of the on-device `SpeechRecognizer` | `AVAudioEngine` + `SpeechAnalyzer`/`SpeechTranscriber` + `SpeechDetector` |
| `VoiceOutput` | "speak this text, tell me when it starts and ends" | `TextToSpeech`, on-device voice, `QUEUE_ADD`, `UtteranceProgressListener` | `AVSpeechSynthesizer` |
| `VoiceAudio` | "own the audio session and cancel the speaker from the microphone" | audio focus, `AcousticEchoCanceler` where available | `AVAudioSession` voice-processing |

- Android has **no built-in VAD and no continuous ASR** (confirmed for API 36).
  `SpeechRecognizer` is one-shot with timeouts, so continuous use is a restart
  loop; the local VAD supplies the endpoints and gates the restarts.
- Silero VAD is small (about 2.2 MB fp32, about 0.6 MB int8; under a millisecond
  per 30 ms frame on a desktop CPU), which is why it is the default gate.
- TTS prefers a `Voice` with `isNetworkConnectionRequired == false`, and reports
  when only a cloud voice exists.
- iOS 26's `SpeechAnalyzer`/`SpeechTranscriber`/`SpeechDetector` is the better
  stack (on-device, no duration cap, about 2.12% WER in published benchmarks),
  which is why the seams are shaped now even though iOS is not built.

## 9. Turn-taking and hands-free behaviour

- **Robust hands-free is the goal**, so the design targets full duplex with
  barge-in, and ships half duplex first.
- **Half duplex (Phase 2, the reliable default):** the microphone is released
  while TTS plays and reopened on `onDone`. No echo cancellation is needed, and
  it works on every device.
- **Full duplex with barge-in (Phase 3):** capture stays live during TTS, the
  platform echo canceller (or a WebRTC AEC chain where the platform one is
  absent) cancels the speaker, and VAD firing during playback stops TTS and
  starts a new turn. Device-gated, because `AcousticEchoCanceler` is unreliable
  on mid-range hardware.
- **Thinking window:** the microphone is off while the agent works; a chime or a
  status line says the turn was submitted. An optional short "cancel" listener is
  a later refinement, not a default.

## 10. Battery, foreground, background

- **Foreground is enough for v1** (the user's call). Voice mode requires the app
  to be visible, which sidesteps Android 14-16 foreground-service microphone
  rules, the persistent notification, and Play's justification policy.
- **VAD is the battery lever.** The expensive parts (recogniser, and any local
  ASR) run only while speech is present; the microphone itself is released during
  thinking and, in half duplex, during playback.
- **Background listening is explicitly out of scope for v1.** It would need the
  `FOREGROUND_SERVICE_MICROPHONE` type, a matching notification, and the
  while-in-use rules; the existing `FOREGROUND_SERVICE_SPECIAL_USE` service is
  for link keep-alive only (`AndroidManifest.xml`).

## 11. Security and privacy

- **Audio stays on the device by default.** When only a cloud recogniser exists,
  the user is told before speaking (the existing `VoiceState.ListeningCloud`).
- **Speech text is untrusted.** It is rendered and spoken, never executed and
  never re-submitted as a command.
- **The extension chooses what is spoken**, not raw tool output, so a hostile
  tool result cannot read itself aloud.
- **Ownership gates the microphone.** Only the holder of a session may open the
  mic or submit on it (ADR-0010).
- **The daemon stays dumb.** It relays text it cannot interpret.

## 12. Testing

- **Shared policy is unit-tested on the JVM** (the existing pattern:
  `androidUnitTest`): state transitions, endpointing thresholds, chunking,
  binding and rebinding across shells, discard rules.
- **The daemon gets protocol tests** beside the existing remote tests: a `speech`
  RPC produces a `speech` message for attached clients, and not for clients that
  did not advertise `speechChannel`.
- **The extension gets a test** that a fenced ` ```say ` block is extracted
  exactly and sent, including the no-block and multi-block cases.
- **A live loop test** should use a scratch session and prove text in, speech
  out, without touching the deployed keeper (the "live tests clean up after
  themselves" rule in AGENTS.md).
- **Cross-component contract** (extension text shape ↔ daemon message ↔ client
  parse) is pinned by one end-to-end test in the same commit.

## 13. Phasing

- **Phase 0 — prove the channel.** Daemon `speech` RPC + `speech` message + CLI;
  extension emits a fixed line per turn; the phone speaks it with Android TTS. No
  VAD, no ASR. This de-risks the only genuinely new plumbing.
- **Phase 1 — spoken-text convention and TTS.** `/voice` toggle, the fence, the
  extension parse, sentence chunking, on-device voice selection, half duplex.
- **Phase 2 — continuous input.** `VoiceCapture`: `AudioRecord` + Silero VAD +
  restart-loop recogniser; the full state machine; auto-submit; mic released
  during thinking.
- **Phase 3 — hands-free robustness.** Barge-in with AEC where available,
  streaming speech deltas from `message_update`, cancel, cross-session
  announcements, iOS via `SpeechAnalyzer`/`SpeechDetector`.
- **Phase 4 — strategic.** The SPEC §4.5 pi-RPC `SessionDriver` as a true
  semantic session kind, which removes the fence convention entirely.

## 14. Open questions

- Does the fence live in the written answer, or does the extension derive the
  spoken summary itself and keep the fence out of the transcript entirely?
- Should the phone speak a shell that is not on screen (a "done in another
  shell" announcement), and if so, how is it not annoying?
- Is a bundled streaming ASR (sherpa-onnx Zipformer) needed, or is the platform
  on-device recogniser plus VAD good enough? Measure before bundling a model.
- How does voice mode interact with pane geometry when the phone is also a
  viewing client (see the multi-client geometry conflict below)?

## 15. Rejected alternatives (recorded)

- Wake word. Battery and policy cost with no product need; voice mode is explicit.
- Cloud STT by default. The audio is shell commands; it stays on the device.
- A phone-side sentinel scrape. Fragile by construction; see §3.3.
- A `speak` tool instead of a fence. Costs a model round trip per turn.
- Putting the speech channel on the daemon's byte fanout as a synthetic OSC.
  Cute, but it makes the wire carry a lie and a reattach loses it.

## 15. What exists, 2026-10-28

| Piece | Where | State |
|---|---|---|
| Daemon `speech` RPC, `speechChannel` feature, `ldmux speak` | `crates/daemon/src/{daemon,remote}.rs`, `bin/ldmux.rs` | Built. `tests/speech_relay.rs` (4) and the extended `cli_full_lifecycle`. |
| OSC 1338 parser | `voice/SpeechOsc.kt` | Built. 12 tests: every split point, oversize, malformed, unknown version, other OSCs. 1338 is not claimed by the engine (ghostty `d5eba8d`, `osc.zig`, the `.@"133"` arm marks the next digit invalid). |
| `speech` message on the page socket | `AttachClient.kt`, `WireJson.kt` | Built. The page advertises `speechChannel` always, because features are fixed at `hello`. |
| `VoiceMode`, `chunkForSpeech`, `VoiceDriver` | `voice/` | Built, pure. 28 tests. |
| `SpeechOutput` (TTS + audio focus) | `androidMain/.../SpeechOutput.android.kt` | Built, compiles, **not run on a device**. |
| pi extension | `packages/pi-voice-extension` | Built. 11 `node --test` tests against a faked pi API and a fake daemon socket. Loads in pi 1.1 without error output; the `/voice` round trip inside a real session was not observed. |

**Corrections to this document made while building.** Section 3.2 named
`GHOSTTY_TERMINAL_OPT_UNKNOWN_SEQUENCE` as the mechanism (wrong, see section 3.2's correction).
Section 5 said `pi.registerMarkdownTransformer` strips the fence: it exists in pi's typings
(`types.d.ts:1252`) and is used, but it only changes what the interactive transcript renders, not
what the model sees.

**Remaining.** The in-app toggle and indicator (a `VoiceDriver` per app, `bind` on page change
with the holder flag), the microphone permission flow around `Dictation`, a measured
thinking-timeout, and the multi-shell announcement behaviour (dropped and counted today, off by
design).
