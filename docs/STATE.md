# ldmux — State of the project

**As of 2026-09-19.** This file answers one question: *where does everything stand
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
  **polling** at ~4/s (`stream.ts:35`, `:39`, `:234`).
- CLI verbs: `ping list desktop new shell run send key capture snapshot kill close
  forget adopt mirror live unlive remote pair expose clipboard-write`.
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

### 4.2 Suites (2026-09-19, all green)

| Suite | Count | Command |
|---|---|---|
| JS/TS | **288 tests, 288 pass** | `node --test "packages/*/test/*.test.ts"` (30 files) |
| Typecheck | clean | `node --run typecheck` |
| Native C (host) | **117 checks, 0 failures** | `just native-test` / `scripts/test-native.sh` |
| Kotlin (Android unit) | **127 `@Test` methods** | `./gradlew :composeApp:testDebugUnitTest` |
| Architecture guard | 6 tests, part of the 288 | `packages/mobile/test/architecture.test.ts` |

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

1. **Auto-scroll while dragging a handle past an edge.** The gesture path has it
   (`TerminalSurface.android.kt`, the `onDrag` branch that calls `controller.scrollBy`);
   the *handle* gesture does not. Next step: extract the "one row per move, clamped to
   the history depth, skipped on the alternate screen" behaviour into one place used by
   both, with the fixed endpoint shifting with the viewport.
2. **A selection is lost when the engine is rebuilt.** The engine is rebuilt on every
   snapshot (ADR-0014 §4 trap 4), so output arriving during a copy discards the
   selection. `termux-app` does the opposite: the view scrolls with the text and both
   endpoints shift by the scroll counter, and the selection is dropped only when it would
   leave the transcript. The frame carries `scrollTotal`/`scrollOffset`, so the delta is
   available. Next step: re-apply the selection in the controller after a snapshot,
   shifted by the delta, with a test.
3. **Double-tap for a word.** The engine's click-count path does not produce a word from
   two taps — pinned as a tripwire test in `test_engine.c`, so a future engine that
   honours it fails loudly rather than being assumed either way. Today long press picks
   the touched cell and the drag extends. Next step: either the engine gains it, or the
   client expands the range using the engine's word table.

### 5.2 Tap-to-cursor: the `planTap` corrections (not started)

Blocked on correcting `TapPolicyTest.kt` first, or the fix fails a stale green test.

- Read byte 8 (`cursorVisible`) as a *negative* signal — it is already decoded and
  `planTap` never reads it.
- Fix the `TapPolicy.kt:88` `Home` shortcut using `CURSOR_PENDING_WRAP`; today it
  pre-empts the strip arithmetic and is only correct when the cursor is on the input's
  first row.
- Add the cursor-did-not-move verdict: after a click, if the next frame shows the cursor
  unmoved, fall back to keys. The delivery acknowledgement (`mousedelivered`) means
  "written to the pane", not "the cursor moved".
- Known and unavoidable: `micro` and `pi` are indistinguishable by every signal the
  client has, so any rule that clicks for one clicks for the other. `docs/NATIVE-TERMINAL.md`
  has the measured table.

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
- **31 commits are ahead of `origin/main`.** The push is held for the user's word.

---

## 6. Pending decisions

| Decision | Options | Where the evidence is |
|---|---|---|
| **The tmux dependency** | (a) keep tmux as the PTY keeper, replace control mode with ordinary commands; (b) keep everything as it is; (c) build the session layer ourselves, planning the keeper process first | **`docs/TMUX-DEPENDENCY.md`** |
| Whether to bring up the macOS VM for iOS | now / after Android is finished | §5.4, `docs/IOS-DEVELOPMENT.md` |
| Whether the login shell's own PS1 should be honoured or overridden | honoured today; `set-environment -gu PS1` ensures the value is the user's | `packages/daemon/src/tmux/control-plane.ts`, the `serverOptionArgs` block |
| Whether `~/.tmux.conf` should be read | **Decided: no.** ldmux drives its own server, and a shell whose behaviour depends on whose dotfiles are on the machine is not supportable | `writeServerProfile` |

---

## 7. Known unknowns

Things we do not know, each with the way to find out. Some of these are here because a
web-research pass failed mid-session and the question was never answered.

### 7.1 The tmux replacement questions (research incomplete)

- **Does `pipe-pane` deliver raw bytes equivalent to control mode's `%output`,
  including escape sequences and OSC?** Not verified. `pipe-pane -o 'cat >> file'` is the
  obvious mechanism and it is not documented well enough from memory. *How to find out:*
  run it on a scratch socket, write a known sequence containing OSC 52 and a multi-byte
  character, compare the bytes.
- **What happens when a `pipe-pane` reader is slow?** Whether tmux blocks, drops, or
  buffers the pane's output, and therefore whether flow control can be built on it. Not
  known. The control-mode equivalent is `refresh-client -f pause-after` plus
  `%pause`/`%continue`, which we already use.
- **Whether control mode's `%output` ordering guarantee has an ordinary-command
  substitute.** Control mode uses a dual queue (`control_block`/`control_pane`) so a
  notification cannot overtake output. Unknown whether that is reproducible without it.
- **napi-rs in practice**: prebuilt binaries, N-API ABI stability across Node versions,
  async and threads, packaging. Not investigated.
- **`node-pty` versus a Rust PTY crate** (`portable-pty`, `nix::pty`): maturity, failure
  modes, who maintains them. Not investigated. The specific failure to look for is why a
  pty host fails with `openpty failed` on some hosts.
- **The keeper-process pattern**: how VS Code's pty host, WezTerm's mux server, Zellij's
  server, `abduco` and `dtach` keep ptys alive across a client-facing restart, whether
  output is buffered while nobody is attached, and whether SCM_RIGHTS descriptor passing
  is used. Not investigated.

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
- **`docs/SECURITY-CONTROL-REGISTER.md` does not exist in this repo.** It was the RCNT
  project. The clipboard policy is documented in `docs/adr/0014` §8 and `docs/TRANSPORT.md`.
