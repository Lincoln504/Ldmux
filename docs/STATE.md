# ldmux — State of the project

**As of 2026-09-20.** This file answers one question: *where does everything stand
right now?* It is the register, not the specification. `docs/SPEC.md` says what the
product is meant to be, the ADRs say why the decisions were made, and
`docs/INVESTIGATION.md` is the historical research log. This is the current position.

It is written to be checkable. Every claim here is either a `file:line`, a command with
its output, or a measurement with the conditions it was taken under. Where something is
not known, it says so and says how to find out.

---

## 1. Verification vocabulary

Four distinct things, and conflating them is how a project lies to itself.

| Word | Means |
|---|---|
| **Device-verified** | Seen working on the phone (`ZD222W34LJ`), by pixel, pane read-back, or logcat |
| **Test-covered** | Asserted by the suite, and the suite fails when the behaviour is removed |
| **Measured** | A number taken from a real run, with the invocation that produced it |
| **Unverified** | Built, compiles, no evidence it works |

A thing can be test-covered and still unverified on the device. A thing can be
device-verified once and broken the next day — dates matter, and they are given.

---

## 2. What the product is, in one paragraph

A tmux-native remote terminal. A daemon on a Linux host owns sessions in **its own tmux
server**, a CLI drives it, and mobile clients (KMP + Compose Multiplatform, Android
first) attach over a WebSocket on the tailnet or the LAN. The client renders with
**libghostty-vt** behind a narrow JNI seam, so the terminal is native, not a WebView.
The wire carries **raw pty bytes**, not a grid.

---

## 3. Components and their state

### 3.1 `packages/protocol` — protocol library

- **State: done, test-covered.**
- One canonical frame codec (`frames.ts`), control message types, runtime path
  resolution, typed daemon RPC (`rpc.ts`) and a socket client shared by every consumer.
- `frames.ts:15` records the deliberate limitation: *only `FrameType.Output` is emitted
  by the shipped code* — a frame is an 8-byte header plus raw pty bytes.
- The remote frame header is **version 5, 40 bytes**, documented in
  `packages/mobile/native/ldmux_terminal.h`. Bytes 33–39 were reserved and now carry the
  selection endpoints; byte 8 is cursor visibility; byte 32 is the alternate screen.

### 3.2 `packages/daemon` — the daemon and CLI

- **State: done, test-covered, running as `ldmux.service`.**
- tmux control-mode parser + client; control plane; output sanitizer; JSON session
  registry with reconciliation; Unix-socket JSON daemon (pid lockfile, owner-only
  socket); `PaneStream` fan-out with coalescing; loopback WebSocket remote transport
  with bearer-token auth.
- Stream modes: `control` (raw `%output`) with automatic fallback to `capture-pane`
  **polling** at ~4/s (`stream.ts:35`, `:39`, `:234`), and **`pipe`** — `pipe-pane`
  into a FIFO, read with no protocol at all (`tmux/pipe-client.ts`). Selected at
  runtime with `LDMUX_STREAM_TRANSPORT=pipe`; `control` is the default.
  Measured A/B in `docs/TMUX-DEPENDENCY.md` §10: the two carry the same bytes to
  within **0.015 %**, and the pipe costs ~7 ms of local echo latency.
- CLI verbs: `ping list new run shell send key desktop live unlive clipboard-write
  copy capture snapshot kill forget adopt remote mirror pair expose` — and `ldmux
  --help` prints the same list, so it cannot drift from this one.
- **The tmux surface, enumerated from the code** (this is the dependency in full):
  - *Commands*: `set-option` (20 sites), `set-environment` (7), `send-keys` (6),
    `attach` (5), `refresh-client` (2), `list-panes` (2), and one each of
    `new-session`, `list-sessions`, `list-clients`, `kill-session`, `has-session`,
    `capture-pane`, `display-message`, `resize-window`, `start-server`.
  - *Control-mode notifications parsed*: `%begin %end %error %exit %output
    %extended-output %pause %continue`.
  - *Public methods of the plane*: `ensureServer listSessions paneCommands listPanes
    hasSession listClientTtys setSessionOption getSessionOption newSession killSession
    sendText sendKey copyMode sendBytes sendMouseClick setWindowSize resizeWindow
    capturePane displayMessage snapshotState`.
- See **`docs/TMUX-DEPENDENCY.md`** for what all of that costs and what replacing it
  would take.

### 3.3 `packages/mobile` — the client

- **State: done for Android, device-verified in the areas listed in §4.**
- KMP + Compose Multiplatform, `minSdk 28`, `targetSdk 36`, `compileSdk 37.2`.
- Native terminal core: libghostty-vt behind `native/ldmux_terminal.{h,c}` (portable, no
  `jni.h`) and `native/ldmux_jni.c`. A fourth file, `native/test_engine.c`, is a
  dependency-free host test program.
- **`assembleDebug` does not rebuild the native library.** It is built by
  `scripts/build-libghostty-vt.sh` into `jniLibs/`, so invoking Gradle directly ships a
  stale `.so`. `packages/mobile/test/native-freshness.test.ts` guards this by comparing a
  sha256 stamp the build writes (`native/prebuilt/sources.sha256`, gitignored) against
  the sources on disk.
- **`assembleDebug` does not rebuild the native library** — repeated because it is the
  single easiest way to waste an afternoon.

### 3.4 `packages/testing`

- **State: done.** The assertion vocabulary (`test`/`expect`) over `node:test`.

### 3.5 `packages/web` — **deleted**

- The buildless xterm.js browser client is gone. Native is the client (ADR-0007).

### 3.6 `scripts/ab-transport.ts` — the transport A/B harness

- Drives both live transports identically and reports attach size, echo latency, burst
  bytes and chunking. Its own scratch tmux server per run, killed afterwards. Results
  in `docs/TMUX-DEPENDENCY.md` §10.

---

## 4. Verification ledger

### 4.1 Device-verified (Android, `ZD222W34LJ`, 2026-09-19)

| Behaviour | How it was checked |
|---|---|
| Real daemon serves the phone over the tailnet | `ldmux ping` with no environment; listener on `100.127.119.54:7625`; app connects and lists sessions |
| `~` in `+ shell` means home | The field's pre-filled `~/` created a shell with `cwd=/home/ldeen` |
| Landscape typing | `QRS` typed in landscape arrives in the pane |
| Selection, whole round trip | `xyzzy` long-press dragged on the phone, copied, pasted back into the shell |
| Dragging a handle moves its own end | End handle dragged right: the end moved, the start stayed |
| Handles are mirrored and proportionate | Screenshot: start leans left, end leans right, both hang below the line |
| No magnifier | Screenshot mid-drag: wash and handles only |
| Rotation is the device's | `user_rotation` changes flip the app; no control involved |
| Closing the keyboard does not rotate | `mInputShown=false`, `cur=1080x2400` unchanged |
| The keyboard returns by rotating through portrait | landscape → portrait → `mInputShown=true` |
| A desk shell has no per-shell menu | The launcher shows the phone glyph and actions only on the phone-opened row |
| A row that cannot be opened says so | The desktop `pi` row reads `~/Documents/ldmux · not in ldmux`, and selecting it turns the bottom button into `new shell (1)` rather than `open` |
| A phone-opened shell wears the phone glyph | `RCNT` and `nocira` show it and their per-shell menu; the desk shells show neither |
| The launcher needs the tailnet, and says why when it has none | Wi-Fi off and Tailscale down: `connect failed: SocketTimeoutException … after 10000ms` in logcat. Tailscale up: connected in 305 ms, 6 sessions parsed, 5 attachable |
| A selection survives a new snapshot | Selected rows `34`/`35` of `seq 1 40`, pressed HOME, came back: the same two rows were still selected, both handles and the platform Copy toolbar intact. Every snapshot replaces the engine, so before this the selection was discarded outright |

### 4.2 Suites (2026-09-20, all green)

| Suite | Count | Command |
|---|---|---|
| JS/TS | **303 tests, 303 pass** | `node --run test` (`--test-concurrency=4`) |
| Typecheck | clean | `node --run typecheck` |
| Native C (host) | **147 checks, 0 failures** | `just native-test` / `scripts/test-native.sh` |
| Kotlin (Android unit) | **143 `@Test` methods** | `./gradlew :composeApp:testDebugUnitTest` |
| Architecture guard | 7 tests, part of the 300 | `packages/mobile/test/architecture.test.ts` |

Counts move; the commands do not. Re-run them rather than trusting this table's numbers,
and note that a green suite is only evidence of what it asserts — several of the tests in
this project were once green while asserting nothing, which is why §8 exists.

### 4.3 Measured, with conditions

- **`GhosttyGridRef.y` is an absolute grid row, not a viewport row.** The same selection
  answers `y=0` on screen and `y=7` with seven rows of scrollback above it. Found by
  probing the engine (`ldmux_terminal.c`, `selection_endpoints`), and it is why endpoints
  are located by comparing the engine's own projection with the engine's own projection
  rather than by arithmetic.
- **`set-option` does not start a tmux server.** `tmux -L <sock> set-option -g x y`
  against a missing server → `error connecting to … (No such file or directory)`, exit 1.
  `new-session` and `start-server` do start one.
- **A tmux server with no sessions exits.** Options applied to an empty server are gone
  before the first session exists, and that session is created by a *new* server.
- **tmux reads its configuration only when a server starts.** A long-lived server keeps
  the profile it was born with. This is why the phone's server had new options applied
  by hand rather than by restarting the daemon (the sessions would go with it).
- **The framework's selection-handle hotspots** are at three quarters of the left
  handle's width and a quarter of the right handle's, with the tip at the top and the
  body hanging below the line (`android.widget.Editor`'s convention).
- **`Modifier.magnifier` magnifies a surface.** The terminal is a canvas in a Compose
  layer, so it magnified a slab of nothing and ignored the size it was given.
- **Rotation timings**: `finishDrawing of orientation change` 58/125/168 ms, which is
  where `PANEL_FLIP_MS = 200L` comes from.
- **Wire cost (80×24, tailnet)**: attach snapshot 1985 B (25.3 % of it JSON envelope);
  keystroke echo 9 B; `ls -la` 1254 B; `ls -laR` 514,758 B; deflate 14.83× on the large
  case, 2.5× on small ones. `docs/WIRE-COST.md`.

---

## 5. Open work, in priority order

Each item says what it is, why it matters, and what the next concrete step is.

### 5.1 Selection: fine-tuning (the feature works; these are the rough edges)

**Double-tap-for-a-word is dropped** — the user does not want it, and long press already
selects. Three items were open here. Two are closed and one remains:

1. **Auto-scroll while dragging past an edge — done, both paths.** The handle drag had no
auto-scroll at all (`40d1d9e`), and the sign was inverted in both. The remaining half was
that neither path was *skipped on the alternate screen*, which §5.1 used to state as
already implemented: `scrollBy` was a `Unit` return, so the callers moved the anchored end
whenever they asked for a scroll rather than when one happened — and on the alternate
screen, where there is no scrollback, the engine refuses while the request is still
non-zero. In `vim`, in `htop`, in `less`, a drag past an edge walked the selection away
from its text while the content stood still. `scrollBy` now returns the rows the viewport
**actually moved**, read from the frame's own `scrollOffset`, so a refused scroll moves
nothing and the rule holds by construction rather than by a flag that can disagree with
the viewport. No protocol change: `scrollOffset` already rides every frame. Proved in
`test_engine.c` ("the scroll offset is what actually moved").
2. **A selection is lost when the engine is rebuilt — done.** The engine is replaced on
every snapshot (ADR-0014 §4 trap 4), so output arriving during a copy discarded the
selection, which on an active shell is most of the time. The controller now **re-projects**
the selection's rows by the change in the viewport's scroll offset — the same rule the drag
uses, and the reason is the same: a selection is anchored to text and its viewport rows are
a projection of that, so new output moves the text up the screen and the rows have to
follow. The selected text is then the test: if the shifted range holds what the user chose,
the selection is still about it. Restoring at the same rows looks like it works and is
wrong in the common case; §8 records that mistake. Device-verified: `seq 1 40`, select
34/35, HOME, return — the selection is still there.
3. **Snapping out of the middle of a wide glyph** — still open, and small.

### 5.2 Tap-to-cursor — **the Home shortcut is fixed; the verdict is not**

The `Home` shortcut is gone (it was wrong for a wrapped input, which is the case this
app exists for) and `cursorVisible` is now read as a negative signal. Both are done and
tested.

Still open:
- **The cursor-did-not-move verdict.** After a click, if the next frame shows the cursor
  unmoved, fall back to keys. The delivery acknowledgement (`mousedelivered`) means
  "written to the pane", not "the cursor moved". This is the honest general mechanism,
  and it is the remaining half of the device report about taps.
- **`CURSOR_PENDING_WRAP` does not solve the Home threshold** — it reports that the
  cursor sits at a wrap boundary, which says nothing about how many rows the input
  spans, and byte 9 is the cursor's visual style so it would need a new wire field.
  That is why the shortcut was removed instead. Recorded so it is not re-attempted.
- Known and unavoidable: `micro` and `pi` are indistinguishable by every signal the
  client has, so any rule that clicks for one clicks for the other.
  `docs/NATIVE-TERMINAL.md` has the measured table.

### 5.3 Clipboard write policy: device end-to-end not done

- The policy exists and is fail-closed: `packages/daemon/src/clipboard-policy.ts`, read
  when a stream is created, flipped with `ldmux clipboard-write on|off`, default off.
- The client's OSC 52 callback is implemented (`OPT_CLIPBOARD_WRITE` installed,
  `OPT_CLIPBOARD_READ` deliberately not).
- **Not verified on the device**: with the policy on, a program's copy reaching the
  Android clipboard. Next step: turn the policy on, run a program that copies, read the
  clipboard back.

### 5.4 iOS

- **Deferred as a build, not as a design.** `docs/IOS-DEVELOPMENT.md` holds the gating
  unknowns. The architecture is cross-platform-ready: the seams are
  `TerminalEngine`, `TerminalPainter`, `Voice`, `BackToLauncher`, `AppLifecycle`.
- **Two seams were deleted rather than kept**, and that is the current style:
  `Orientation` is gone because the shape now comes from the window size, which is
  common Compose.
- A macOS host exists (`~/vms/macos-sequoia-vm`, Sequoia 15.7.9 + Xcode 26.1.1, reachable
  as `ssh seqvm`). Bringing it up for iOS development is a decision, not a task.

### 5.5 `mesh` transport profile

- **Design only** (`docs/MESH-TRANSPORT.md` §9). Two profiles are specified, `direct`
  and `mesh`; only `direct` is built.

### 5.6 Smaller items

- `OutlinedTextField`, `AlertDialog` and `TextField` are deliberately **not** thinned:
  their internals clip when forced shorter. Open question: whether to restyle them.
- Commit `0851333`'s message describes one of its three changes. It is already pushed, so
  correcting it means `--amend` plus `--force-with-lease`.
- **Ahead of `origin/main`** (`git log --oneline origin/main..HEAD` is the current answer). The push is held for the user's word.

---

## 6. Pending decisions

| Decision | Options | Where the evidence is |
|---|---|---|
| **The tmux dependency** | (a) keep tmux as the PTY keeper, replace control mode with `pipe-pane`; (b) keep everything as it is; (c) build the session layer ourselves. **(c) is decided: not built** (ADR-0015). **(a) is built and measured but not switched to** — the default is still `control` | **`docs/TMUX-DEPENDENCY.md`** §7 and §10, **ADR-0015** |
| Whether to bring up the macOS VM for iOS | now / after Android is finished | §5.4, `docs/IOS-DEVELOPMENT.md` |
| Whether the login shell's own PS1 should be honoured or overridden | honoured today; `set-environment -gu PS1` ensures the value is the user's | `packages/daemon/src/tmux/control-plane.ts`, the `serverOptionArgs` block |
| Whether `~/.tmux.conf` should be read | **Decided: no.** ldmux drives its own server, and a shell whose behaviour depends on whose dotfiles are on the machine is not supportable | `writeServerProfile` |

---

## 7. Known unknowns

Things we do not know, each with the way to find out. Some of these are here because a
web-research pass failed mid-session and the question was never answered.

### 7.1 The tmux replacement questions — **answered by measurement**

The answers are not in this file: they are in `docs/TMUX-DEPENDENCY.md` **§6**
(the questions and their resolutions), **§8** (the measurements in full), **§9** (the
keeper-pattern survey) and **§10** (the transport A/B). The recommendation that follows
is ADR-0015. This section used to cite "§8, §10" of *this* document, where §8 is the
corrections log — a pointer a reader could follow and land somewhere unrelated.

The four `pipe-pane` questions are settled: verbatim, lossless, bounded, ordered, and
with no flow control. The keeper-pattern research is complete and written up in
ADR-0015. **Nothing here is still unknown.** The one thing outstanding is the **device
A/B**: the numbers in §10 are host loopback, and the two behaviours that would decide
it — echo latency over the tailnet, and what each transport does when the phone stops
keeping up — need the phone.

- **Does `pipe-pane` deliver raw bytes equivalent to control mode's `%output`,
  including escape sequences and OSC?** Yes, completely, byte-for-byte, with no octal
  escaping. `docs/TMUX-DEPENDENCY.md` §6.1 and the numbers in §8.
- **What happens when a `pipe-pane` reader is slow?** Measured: lossless under a stalled
  reader, with a bounded buffer (RSS 8.4 → 26 MB then flat) and backpressure through to
  the pane. There is no flow-control substitute — `pause-after` is control mode only.
  Same section.
- **Whether control mode's `%output` ordering guarantee has an ordinary-command
  substitute.** Measured: the pipe is strictly ordered.
- **napi-rs, `node-pty` versus a Rust pty crate, and the keeper-process pattern.** All
  answered by the survey in §9: every one of dtach, abduco, mosh, Zellij, VS Code's pty
  host and WezTerm's mux server keeps the PTY in a long-lived supervisor and reattaches
  over a Unix socket, and **none uses SCM_RIGHTS descriptor passing**.

These were six open questions here until 2026-09-19 and were answered by measurement.
Leaving the questions in place under a paragraph saying nothing was unknown made the
section contradict itself, which is how a reader learns to stop trusting it. The full
answers are in `docs/TMUX-DEPENDENCY.md` §6 and §9, and the recommendation that follows
from them is ADR-0015.

### 7.2 Product unknowns

- **Whether `Modifier.magnifier` could ever be made to work over Compose content** — not
  pursued, because the magnifier was removed by decision (§8.6 of the decision log in
  `docs/adr/0014-text-selection-and-handles.md` §9).
- **iOS behaviour of every seam.** Unmeasured by definition until a macOS build exists.
- **Whether Node's HTTP server leaves Nagle on by default** in the exact configuration
  used here. `setNoDelay(true)` is called explicitly, so the answer does not change
  behaviour, but the default is unverified.
- **Whether 4 KB output coalescing delays small outputs** perceptibly, and whether
  `permessage-deflate` interacts with the ping/heartbeat. Asserted as safe; not measured.

### 7.3 Machine facts that are easy to get wrong

- `just` is **not** on the host PATH. `just android` outside nix fails rc=127 with no
  build. Use `nix develop .#mobile --command just android`.
- The host Node is **v25.8.2**; inside `nix develop` it is v26.8.2. The daemon's systemd
  unit pins the host path.
- The system tmux is **3.5a**; the devShell's is **3.7b**. The daemon uses the system one.
  `tmux -L <sock>` from the *system* tmux cannot talk to a *devShell* tmux server
  (`server exited unexpectedly`); read those through `nix develop --command tmux`.
- The phone's density override is 446 ⇒ factor 2.7875, so 48 dp = 133.8 px.
- `adb shell input swipe` interpolates from the first millisecond, so it leaves the
  long-press slop before the timer fires. Use `input motionevent DOWN/MOVE/UP` to test a
  long-press-drag.

---

### 7.3 A pipe reader that dies is not noticed — **measured, open, and recorded here**

The pipe transport's liveness is read from `#{pane_pipe}`, and
`packages/daemon/src/tmux/pipe-client.ts` says in its own docstring that tmux cannot answer
that question. Measured 2026-09-20 on a scratch server:

| Step | Result |
|---|---|
| Kill the `cat` reader that tmux spawned for the pane's FIFO | `pgrep -f <fifo>` found it and `SIGKILL` was delivered |
| Ask tmux whether the pipe is still registered | `#{pane_pipe}` still reports **`1`** |
| Watch the FIFO's read end for EOF | **no `end` within 15 s** — the socket stays quiet |
| Watch for the client reporting death | **no `onExit`** |

Neither available signal notices. The client goes on believing it is streaming while nothing
arrives: a frozen terminal with no resync, which is the same failure the control transport
had, reached the same way — liveness read from a source that cannot report it.

**A fix was attempted and reverted.** It combined socket EOF with the tmux poll, on the
assumption that EOF fires when the last writer dies. It does not, so the code never fired
and would have shipped as machinery that looks like a guard and is not. What stands in its
place is the corrected comment and a **tripwire test** (`pipe-client.test.ts`, the one named
`GAP:`), which asserts `#{pane_pipe}` still says `1` and no exit is reported. The day either
changes, that test fails and the comment gets corrected rather than going on claiming
something untrue.

**Exposure is bounded, and that is the only reason this is not being built now.** The pipe
is opt-in (`LDMUX_STREAM_TRANSPORT=pipe`); the default is `control`, whose equivalent bug
was fixed. The mechanism that would actually answer the question is asking whether the
reader *process* still exists — direct, and already demonstrated by the tripwire's `pgrep` —
at the cost of one process check per pane per poll interval. Worth doing before the pipe
transport could ever become the default, and not worth doing speculatively before then.

## 8. Corrections log — kept visible

A wrong claim is retracted in place, with the reason. These are the ones worth keeping.

- **"The port-leak flake."** I diagnosed a port-reuse race from `EADDRINUSE` lines in the
  suite output. Those lines are *expected logging* from a test that deliberately takes a
  port. The real cause was my own staleness guard comparing mtimes.
- **"Selection is device-verified"** was true of the *round trip* and not of the handles:
  the handles were later found not draggable at all, because the gesture required a long
  press first.
- **"The handles are drawn by the app"** (an earlier plan): they are the *platform's* art.
  See `docs/adr/0014-text-selection-and-handles.md` §9.
- **"`micro` and `pi` are distinguishable"** — an early probe said so because it started
  `micro -config-dir /tmp/none`, which makes micro disable mouse reporting. The exact
  invocation is part of the measurement.
- **"A magnifier."** Built, worked, removed by instruction. Recorded as a decision.
- **"The wrapped-line test is a flake."** It was deterministic: a 40-character line on a
  40-column pane does not wrap, and the test asserted it must.
- **"Raising the burst makes the pause test reliable."** It does the opposite. A bigger
  burst starves the producing process under load, so it writes slower, so the client
  keeps up, so tmux correctly never pauses. Measured: 8 MB failed more often than 300 KB.
  The real cause was suite parallelism.
- **Two tests were passing on their own echo.** The shell echoes the command it was
  given, so a sentinel word and the literal text `\033` are both in the stream before
  any of it runs. A wait on a sentinel passed instantly and a negative assertion about
  escaping failed on the echo. Escaping is now asserted positively, which cannot be
  fooled.
- **A third test was passing on its own echo** (2026-09-20). Fixing the alternate-screen
  flake by waiting on the marker `ALTERNATE-MARKER` failed in 187 ms, because the shell
  *echoes the command* and the marker is on screen before `tput` has run. An echo trap is
  not a one-off in this project; it has now been sprung three times.
- **`waitFor` could not see an async predicate** (2026-09-20). It took `() => boolean`, so
  an `async` predicate returned a Promise, a Promise is truthy, and the first check passed
  and the wait did nothing. It bit twice before it was fixed at the helper.
- **"The process death that freezes the terminal is reachable by killing the tmux
  server"** — wrong, and the test that encoded it proved nothing (2026-09-20). A control
  client that loses its *server* prints `%exit` on the way out, so the graceful path covers
  it; the test passed with the fix removed. The reachable case is the client's own process
  being destroyed — an OOM kill or a crash — which is what the test now does. **A test that
  passes with the fix removed is not evidence, and this one shipped that way for one run
  before the probe caught it.**
- **"Auto-scroll is skipped on the alternate screen"** (2026-09-20). §5.1 stated it as
  implemented and it was not implemented at all. A status document's next-step text is a
  description of the *plan*; it is not evidence about the code.
- **"Extracting `parseSessions` fixed the empty launcher"** — it fixed nothing
  (2026-09-20). The daemon's payload has no nulls and no objects, so the old code parsed it
  correctly. The extraction and its test are right on their own terms; the cause was that
  the phone had no route to the tailnet. A tidy story about a tidy fix is how a project
  lies to itself, which is why this is written down.
- **"The daemon was stale"** — wrong (2026-09-20). `ldmux list` and the phone disagreed
  about a session's own name because `list` returned raw records while the push path
  applied `sessionTitle`; two answers to one question, not an old process.
- **"The debug APK is stale because its size never changes"** (2026-09-20). The size did
  not move across a real source edit, and neither did the dex hash I was comparing —
  because I hashed `classes.dex`, and **this app's code is in `classes5.dex`**.
  `classes.dex` holds the Kotlin runtime and does not mention `TerminalController` at all.
  The build was fresh the whole time, and I nearly recorded a false alarm as a defect. The
  check that works: put a distinctive string literal in a source file, rebuild, and
  `unzip -p <apk> classes5.dex | grep` for it — probed both ways, present when it is in
  the source and absent when reverted, before believing a build is either fresh or stale.
- **"Restoring a selection at its own viewport rows keeps it across new output"** — my own
  first fix, and wrong (2026-09-20). New output moves content up the screen, so those rows
  hold different text and the text check would have cleared the selection in the common
  case while looking like it worked. The rows have to be **re-projected** by the change in
  the viewport's scroll offset, which is the same rule the drag already uses. Caught by
  asking what the code actually delivers rather than what the comment said.
- **`waitFor` in this suite takes a synchronous predicate.** An `async` one returns a
  Promise, a Promise is truthy, and the wait passes instantly without checking anything.
  A test that always passes is worse than a failing one.
- **The 2026-09-19 restart.** Moving the tail tmux server out of the dying unit's cgroup
  saved the server and not the panes, so the sessions died. The lesson is that a process
  tree has to move, not a parent.

---

## 9. Environment

- **Live daemon**: `ldmux.service` (user unit), active, `KillMode=process`,
  `LDMUX_STATE_DIR=%t/ldmux`, remote on `100.127.119.54:7625`, tmux socket `ldmux`.
  Its option profile is `/run/user/1000/ldmux/tmux-server.conf`.
- **The tail rig is retired**; `/tmp/ldmux-tail` is dead. The test rig is
  `ldmux-test-daemon` (`/tmp/ldmux-test`, port 7624).
- **Their sessions survive a daemon restart** because `KillMode=process` keeps the tmux
  server out of the signal path. Measured: three sessions, restart, three sessions.
- **Host artefacts**: `~/.local/bin/{ldmux,ldmuxd,ldmux-terminal}`,
  `~/.config/systemd/user/ldmux.service`. `$mod+Return` opens alacritty on ldmux via
  `ldmux-terminal`, which falls back to a plain shell when no daemon answers.
- **dooboxes is uninstalled** (move-not-delete): unit renamed `.uninstalled`, PATH
  wrappers moved to `~/.config/ldmux-uninstalled-dooboxes/`, project untouched.
- **gmux is still installed, still enabled, and still bound to the tailnet** — measured
  2026-09-20: `gmux.service` `active`+`enabled`, `gmux-prune.timer` `active`+`enabled`,
  and a `gmux.service.d/listen.conf` drop-in setting `GMUXD_LISTEN=100.127.119.54`.
  **Not a defect, and deliberately not removed yet:** the session this work was done in
  was running inside it, and it is `Restart=on-failure`, so stopping it would drop the
  working terminal. Recording it here because the dooboxes entry above is detailed
  enough to read as "the migration is done" — a place-holder for a second predecessor
  that is still running is exactly what that impression hides. ldmux's unit is gmux's
  shape with `KillMode=process` added, so the successor was written against it.
- **`docs/SECURITY-CONTROL-REGISTER.md` does not exist in this repo.** It was the RCNT
  project. The clipboard policy is documented in `docs/adr/0014` §8 and `docs/TRANSPORT.md`.
