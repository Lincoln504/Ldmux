# ldmux mobile (KMP + Compose Multiplatform)

The native Android client. Landscape-only, minimal chrome, with the terminal
rendered **natively**: `libghostty-vt` (the Ghostty terminal core) behind a
Kotlin seam, painted by a Compose `Canvas`. No WebView, no WASM (ADR-0007).

This is the app `docs/SPEC.md` calls for (§4.2, §7). It is the only client: the
daemon-served browser client (`packages/web`) was deleted, because two renderers
meant two copies of the snapshot and mode-sequence logic. This package is
the native app with its own engine.

## Stack

- Kotlin Multiplatform + Compose Multiplatform (`composeApp`, `androidTarget`).
- `expect`/`actual` for the platform seams: `TerminalEngine` (libghostty-vt via
  JNI on Android), `TerminalPainter` (native text rasteriser), settings
  (SharedPreferences).
- Landscape (`sensorLandscape`), dark, one accent colour.

## Native terminal core

```bash
scripts/android.sh native     # libghostty-vt + JNI shim for arm64-v8a + x86_64
```

`scripts/build-libghostty-vt.sh` cross-compiles the Ghostty commit pinned in
`native/ghostty.pin` (the same pin the Rust build uses, via nix) with
Zig (from the devShell; the build asserts the checkout is exactly the pin) and the NDK, then links `native/ldmux_terminal.c` into
`libldmux_terminal.so` per ABI under `composeApp/src/androidMain/jniLibs/`.
Those libraries are build outputs (git-ignored); `scripts/android.sh build`
rebuilds them first so they are always current.

Data path: daemon `snapshot`/binary `Output` frames -> `AttachClient` ->
`TerminalEngine.write` -> `NativeTerminal.render` (a fixed `cols*rows` cell
frame) -> `TerminalFrame.decode` -> `TerminalPainter` -> Compose `Canvas`.

Two things worth knowing (both live gotchas):

- **A screen snapshot may separate rows with a bare LF.** A VT treats
  LF as a line feed only (column preserved), so a snapshot must be handed
  CR+LF or the screen staircases. `TerminalController` normalises it.
- **The native row/cell iterators may skip trailing blank cells.** The encoder
  must therefore emit exactly `cols*rows` records, padding blanks, or the
  client's fixed-stride decode desynchronises.

## Build

The JDK + Gradle live in a dedicated devShell so the main shell stays lean:

```bash
# `scripts/android.sh build` rebuilds the native core first. Gradle alone does not: it
# packages whatever .so is already in jniLibs/, so invoking gradle directly can
# ship a stale library. See "The native library is a build output" below.
scripts/android.sh build
# APK: packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
adb install -r packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
```

`local.properties` points Gradle at the Android SDK and is git-ignored; on this
machine the SDK is `/home/ldeen/Android/Sdk`. `ANDROID_HOME` is exported by the
`mobile` devShell as a fallback.

### The native library is a build output

`libldmux_terminal.so` is produced by `scripts/build-libghostty-vt.sh` (pinned in
`native/ghostty.pin`) into `composeApp/src/androidMain/jniLibs/<abi>/` and is
git-ignored. The build script resolves the NDK by each directory's
`Pkg.Revision`, never by sorting names — `sort -V` orders `30.0.16248370` before
`android-ndk-r27c`, which silently picks the *older* toolkit.

**Gradle does not compile C and does not know the shim changed.** It packages
whatever `.so` is already sitting in `jniLibs/`, so `gradle :composeApp:assembleDebug`
on its own ships the last library that was built, however old — which is how a
day-stale `.so` once reached a phone, where the only symptom was a program's copy
going nowhere and the device reporting silence rather than an error. `scripts/android.sh build`
solves it by building the native core first. `test/native-freshness.test.ts`
enforces it: a packaged `.so` older than any shim source fails the suite, and so
does a `.so` missing a JNI symbol Kotlin declares.

The portable half of the shim has its own tests, which need no device at all:

```bash
scripts/android.sh native-test   # runs native/test_engine.c against the nix-built host engine
```

## A test daemon, and the way it lies

The daemon talks to a **keeper** (`ldmux-keeper`) that it auto-spawns by name from
`PATH`. A dev daemon started outside the devShell can pick up an *older installed
keeper* from `~/.local/bin` than the daemon it was built with, and the two can
disagree about the keeper protocol: the daemon starts, listens, accepts a
WebSocket and serves a degraded answer. The app shows a healthy `link 20 ms`
readout over a wrong screen, which is indistinguishable from a rendering bug.
Run the dev daemon with the workspace's own binaries first on `PATH`, and a
scratch state directory so it cannot touch the real sessions:

```bash
D=$(mktemp -d /tmp/ldmux-test-XXXX)
LDMUX_STATE_DIR=$D LDMUX_TOKEN_FILE=$D/token LDMUX_REMOTE_PORT=7630 \
  PATH="$PWD/target/debug:$PATH" target/debug/ldmuxd
```

A transient user unit also works and is how the daemon runs in production:

```bash
systemd-run --user --unit=ldmux-test-daemon --collect /tmp/ldmux-test/run.sh
journalctl --user -u ldmux-test-daemon -n 20 --no-pager
```

To inspect the scratch sessions, point the CLI at the same state directory:
`LDMUX_STATE_DIR=$D target/debug/ldmux list`. When done, stop the daemon and kill
only the keeper whose socket is `$D/keeper.sock` (never a broad `pkill`).

## Pointing it at a daemon

The app's `settings` opens a dialog for `host[:port]` and the bearer token. For a
USB-attached phone with no tailnet, forward the daemon's loopback port and use
`localhost`:

```bash
adb reverse tcp:7629 tcp:7629
# settings -> host: localhost:7629, token: <cat <state-dir>/token>
```

## Backgrounding and the network

- **The shell survives; the attachment does not.** The shell lives in the keeper on the
  host. Off screen, a foreground service keeps the sockets up for a 45 s grace
  (`BackgroundSession.releaseGraceMs`, ADR-0012); after that the app drops its pages,
  closes the sockets, releases its claims, stops the service and the wake lock (which
  also has a 10-minute timeout as a backstop) and lands on the launcher. The release is
  an explicit call from the platform lifecycle (`AppLifecycle.set` ->
  `BackgroundRelease`), not a Compose effect, so it does not depend on a frame clock
  (iOS answers a grace of 0 and releases synchronously).
- **Scroll fidelity across reconnects.** A snapshot rebuilds the engine only while the
  view follows live output; while the user is reading history it is replayed into the
  existing engine (`planSnapshot`), so the anchor and scrollback survive.
- **Input is refused while disconnected** (no prediction, no silent drop); the page says
  "offline". A refused token shows "pairing rejected".
- **Network changes** make every open socket ping at once and reconnect if no pong
  arrives within 2.5 s, instead of waiting out three missed heartbeats.
- **Cleartext.** Transport is `ws://` over WireGuard. Android's network security config
  cannot express `100.64.0.0/10`, so it permits cleartext and the tailnet/loopback rule is
  enforced in code where sockets are built (`LdmuxConfig.isPaired`); see
  `res/xml/network_security_config.xml`. Backup and device transfer exclude the token.
- Dictation prefers on-device recognition and says so on screen when it falls back to cloud.

## Modes

**The device owns the orientation in the shell view. The app owns the keyboard.** They
are separate decisions and they no longer share a variable (ADR-0011). The launcher and
its dialogs are the one exception: they hold portrait while shown and hand the device
back when a shell opens (`HoldOrientation`, ADR-0011 amendment).

- The manifest declares no `screenOrientation` and does declare `configChanges`, so the
  activity is not recreated when the device turns. Whether the app is portrait or
  landscape is read from `LocalWindowInfo.current.containerSize` — the window, not the
  sensor and not the display — so auto-rotate-off, a foldable, split screen and a
  floating window are all answered correctly with no second opinion.
- **There is no flip control anywhere.** Not in the menu, not in the bar, not in the
  panel.
- **The keyboard exists only in portrait.** In landscape there is no text field at all,
  so typing there is a key-event path (see below), not an IME.
- **Closing the keyboard does not rotate anything.** The portrait bar's second row leads
  with *hide keyboard*; the screen stays exactly where it is. Rotating out to landscape
  and back is the only way to bring the keyboard back, which is the platform's own
  gesture for it.

### Two shapes, one terminal

- **Landscape** — side panel (side is a setting), key matrix, `+ shell`, settings; the
  keyboard is never up. The panel is `paste` and the microphone at the top, then the key
  rows. (The microphone used to have a bottom bar of its own; it joined `paste`, because
  two bars were two things to learn for one job.)
- **Portrait** — no panel, terminal fills the width, thin bar above the keyboard with
  `[menu]` on the first row and `[hide keyboard]` on the second, one row per key group.

**One line per shell in the launcher.** The row is the process name, then the directory
small and grey beside it, at the thickness the two-line row had. A desk window says
nothing in words — the row implies it; a shell this phone opened carries the phone glyph.
The only state put into words is **"no window"**, in amber: it is an anomaly, not a state.

**Every row is one height, whatever it carries.** The per-shell controls (send to desktop,
close) are the tallest thing a row can hold — a control has a minimum touch height and the
text has none — so laying them out beside the name made the rows that carry them thicker
bars than the rest: measured on the phone, 64 dp against 53 dp (2026-10-05). They are drawn
over the row's right end now, matched to the row's height (`Modifier.matchParentSize`, so
they are measured after it and cannot set it), with their width reserved out of the address.

**A page is connected only while it is the one on screen *and* the app is on screen**
(`pageIsConnected`). An app in the background is not a reader, and holding the claim while
nothing is looking at it left a desk window on "Press r to take it back" with no shell open
anywhere. The shell is safe in the keeper, the page keeps its last frame, and coming back
re-attaches — which takes the shell over, because arriving is the consent (ADR-0010).

The terminal is resized to the viewport in both shapes: the client owns the geometry and
the daemon sizes the session's pty to the grid the surface can actually paint (ADR-0008).

### Typing

Two paths, and which one runs depends on the shape.

- **Portrait** has a text field that exists only to be focusable for the IME. Text
  arrives through it, and the delta between two field values is the input.
- **Landscape** has no text field, so characters arrive as key events and are turned into
  input directly. This was missing for a long time: the key handler mapped terminal
  *keys* (Enter, Tab, Escape, arrows) and let every other key fall through, which is right
  in portrait and meant letters typed in landscape reached nothing at all. Characters are
  handled on the same path now, skipped while the soft keyboard is up so they are not sent
  twice, and skipped under Ctrl/Alt/Meta so a chord cannot become its letter.
- **`onPreviewKeyEvent` is delivered from the root down to the focused node**, so the
  surface holds focus whenever the text field does not. Clearing focus hides the keyboard;
  it must not also be the thing that mutes it.

## Behaviour contract

Rules that came from a decision and are not preferences. Breaking one of these is a
regression even if it looks like an improvement.

| Rule | Why |
|---|---|
| No blue, no accent hue, anywhere | Greyscale only; importance lives in weight and contrast. The one exception is the desaturated red of the close confirmation. |
| The menu/home button is a raised tone, not the brightest thing | A token must not be both the emphasis and the primary fill; `Accent` is reserved for the one primary action. |
| A key is **one** composable in both shapes | Two copies of a control drift. |
| `del` sits to the left of `⏎`, `⏎` rightmost | Thumb reach. |
| Navigation at one end, keys in the middle, actions at the other | The panel's middle scrolls and holds keys; both ends are fixed. |
| A shell is closed only from the launcher | Never from the shell it belongs to. A desk-started shell keeps its window, always. |
| Only a phone-opened shell has a per-shell menu | Origin is a fact about the shell, not a label. |
| `+ shell` asks for a **path only** | No command, no taxonomy, no capability probing. Pre-filled with `~/`, caret after it, and empty means the home directory — never the launcher row's cwd. |
| Opening a shell never rotates the device | The app does not hold opinions about hardware. |
| `thin` is the default density | `Touch(min, dense, pad, rowPad, icon, check)` in one place; a minimum height only binds when the content is smaller than it. |
| Every drawn control has a label | A mark with no label has no name anywhere for a screen reader. |
| A control that can produce no visible change is disabled, not silent | Includes the settings steppers at their ends. |
| One microphone for the app, bound to the shell on screen | Dictation is for the shell you were looking at when you spoke. |
| A dead session is never a page | It is removed from the pager. |
| No magnifier, and no selection handles drawn from scratch | Handles are the platform's own drawables; a magnifier was built, worked, and was removed by decision (ADR-0014 §9). |

## Rendering

**Nothing is scaled to fit.** The grid the surface reports *is* the grid the
pane is resized to, so after a resize the leftover is under one cell and the
draw is 1:1 (`term/GridPlacement.kt`). An oversized grid — a font change or a
rotation whose resize has not landed yet — is clipped, anchored bottom-left,
rather than squashed. The render transform and the touch hit-test read the same
`GridPlacement`, which is what keeps tap-to-cursor pointing where the user
pointed.

- **Text size**: a setting, or a two-finger pinch. A bigger font means fewer
  columns, and the shell is reflowed to match — the client owns the geometry
  (ADR-0008).
- **Cursor**: drawn in its own one-cell canvas so the blink repaints one cell
  instead of the whole grid two or three times a second.
- **Frame rate**: follows the measured round trip — `RTT/2`, clamped to
  50–200 ms (`FramePacing.kt`). The sidebar shows `link 42 ms · 20 fps`.
- **Volume**: a history page is sized by the link (`FramePacing.historyPageLines`:
  5,000 rows fast, 1,000 slow), and the page advertises `inplaceResync` so a client
  that falls behind is repaired with a snapshot instead of a reconnect
  (`docs/WIRE-COST.md`).
- **Page order and numbering**: a page is a shell you opened, in the order you
  opened it (`PageOrder.kt`), never the daemon's last-activity order — that order
  changes whenever any shell prints, and following it made the screen slide between
  shells and come back on its own (2026-10-25). Opening from the launcher lands on
  page 1 (the first row ticked). The header reads `3 / 5 shells`, and the launcher's
  tick box shows the page a chosen row will become.
- **Key bar**: portrait is three rows — navigation (`Esc Tab Home End PgUp PgDn`),
  modifiers (`Ctrl Alt ⇧ C/c`), actions (`⌨ paste ⏎`) — with a 3x2 arrow pad
  (`↑` over `← ↓ →`, no empty cells: `↑` is one full-width key) at the trailing edge. Landscape uses the same `ArrowPad` in the
  side panel. Modifiers latch for one following key; `⇧` then `⏎` is `Shift+Enter`
  (a newline in `pi`, ADR-0024).

## Status

- [x] KMP/Compose shell that builds, installs and runs.
- [x] Settings persisted on device: one pairing row (`pair` opens the code
      scanner; paired shows the machine and `unpair`, ADR-0017), panel side,
      control size, text size. Predictive echo has no setting: it follows the
      measured round trip (`FramePacing.predict`, on above 120 ms / off below 80 ms).
- [x] **No top bar**: a slim side panel carries back/forward between open
      shells, the extra keys, `+shell` and settings.
- [x] Session list + `new`/`kill` over the daemon's WebSocket; one page per live
      session in a `HorizontalPager` (swipe, or the panel arrows).
- [x] **Native terminal rendering**: libghostty-vt via JNI, Compose Canvas
      renderer, live attach stream, 1:1 with the pane. Verified on device.
- [x] **Client-driven pane resize**: the phone's viewport is the authority;
      the daemon clamps it and resizes the keeper session.
- [x] **Landscape/portrait by device rotation** in the shell view (ADR-0011), keyboard
      portrait-only and dismissable without moving the screen. The launcher and its
      dialogs hold portrait (ADR-0011 amendment).
- [x] Panel and bar keys drive the native terminal directly.
- [x] **Headless dictation** (Android `SpeechRecognizer`) — no keyboard needed.
- [x] **Scrollback**: the emulator's own viewport is scrolled (drag, or `⤒`/
      `⤓`), with a scrollbar and the frame carrying total/offset. History is
      paged in once, then scrolled locally with no round trip per swipe.
- [x] **Predictive local echo**, off by default (settings). The emulator tells
      us when the cursor is at a password prompt, so prediction is refused there
      rather than guessed at.
- [x] Frame coalescing + **RTT-adaptive frame rate** (`RTT/2`, 33–150 ms), with
      the measured link shown in the sidebar.
- [x] **Cursor blink** (~2 Hz), drawn in its own one-cell canvas.
- [x] **Pinch/text-size zoom**: font size is a client setting, and a bigger font
      reflows the pane instead of scaling the grid.
- [x] **Tap-to-cursor**, in two regimes chosen from the pane's own state — a real
      synthesized SGR click for a mouse-aware program, relative cursor motion for
      a plain shell.
- [ ] Selection polish: auto-scroll while dragging past an edge, a selection surviving
      new output, double-tap for a word (`docs/archive/STATE.md` §5.1).
- [ ] `planTap` corrections for tap-to-cursor (`docs/archive/STATE.md` §5.2).
- [ ] Clipboard-write policy verified end to end on the device (`docs/archive/STATE.md` §5.3).
- [x] Foreground service for a *bounded* background window (see "Backgrounding and the
      network" below). The heartbeat does not survive Doze indefinitely, by design.
- [ ] iOS (needs a macOS host for the xcframework).

Verified on a real Motorola Edge 2024 (2026-09-19): rotating the device flips the app
with no control involved; closing the keyboard leaves the screen in portrait; the
keyboard returns by rotating through portrait; typing reaches the pane in **both**
shapes; a long-press drag selects past the cell it started on; dragging a handle moves
its own end and leaves the other; the pane resizes per shape and the grid draws 1:1 in
the terminal's own colours. See `docs/archive/STATE.md` §4 for the full ledger.

## Known issues

### The page you are on is lost when the mode flips — FIXED

This heading did not say "FIXED" even though the fix is described below and is
in the tree (see "The fix in the tree follows from the measurement"); the
sibling entry below (`### The four arrow keys draw nothing`) uses the
"— RETRACTED"/"— FIXED" convention and this one should have too. Kept as a
postmortem, not an open issue.

Reproduction, on device: open two or more shells, tap `▶` until the header reads
`n / n`, flip to portrait with the flip button. The header comes back as `1 / n`
and the terminal behind it is the first shell. Reproduced every time.

**Root cause, measured.** An instrumented run printed the page, the pager's
measured viewport width and the pager's *identity* at every change:

```
page=1 viewportW=1764 state=@7374ab1   <- after tapping the next arrow
page=1 viewportW=2294 state=@7374ab1   <- window mid-turn, page INTACT
page=0 viewportW=1080 state=@7374ab1   <- same state object, same composition
TerminalArea composed                  <- printed once, for the whole run
```

The identical `PagerState` re-derives its page when it is re-measured at a new
viewport width. It is not a rebuilt pager, not a new state object, and not which
branch the terminal is drawn in.

Two explanations were published here first and were **wrong**. They are recorded
so they are not revived:

- "Position is a pixel offset, so `floor(1764/2400)` is 0." Dead in both
  directions: at 2294 px the pager still reported page 1, where that arithmetic
  predicts 0, and at 1080 px it reported 0, where the arithmetic predicts 1.
- "A new `HorizontalPager` at a new call site resets it." The terminal was
  restructured to have exactly one call site and the reset still happened.

What was tried and failed, so it is not re-tried: hoisting the page index and
restoring it; reporting only on a settled swipe; having the arrows set the index
directly; gating on `LocalConfiguration`; capturing the page and restoring it
after a fixed delay. Every one of those varied the fix. None varied the
observation, which is why five were guessed at a mechanism nobody had measured.

The fix in the tree follows from the measurement: capture the page while the
viewport is **stable**, and re-assert it on the viewport **change itself** — the
event, rather than a timer, which is a bet on when re-measuring has finished.

### Rotation timings, measured

`WindowManager: finishDrawing of orientation change` on the test device reports
58 ms, 125 ms and 168 ms. `PANEL_FLIP_MS` is set from that: the panel waits out the
turn rather than being drawn into the old window and carried round with it.

### `adb install -r` looks like a crash

`am force-stop` followed by an install kills the process with no crash record. It
was mistaken for an app crash during development. Check
`adb shell dumpsys activity exit-info io.ldmux.mobile` before believing a crash
report: `reason=16 (PACKAGE UPDATED)` is a reinstall, `reason=5/6 (CRASH)` is not.

### The four arrow keys draw nothing — RETRACTED. All four render.

This section used to claim that `↓` and `→` did not draw, with a table of bright-pixel
counts per key of 462, 496, 1 and 0. **That was wrong.** Every one of the four renders.

Measured again, on a fresh capture, inside each glyph's own reported bounds:

| key | ink pixels brighter than 110 |
|---|---|
| `←` | 63 |
| `↑` | 46 |
| `↓` | 47 |
| `→` | 63 |

The mistake was mechanical, and it is worth more than the correction. The landscape
panel's middle section is a `verticalScroll`, so those rows *move* as history is added.
The y-band being measured was taken from an earlier `uiautomator` dump, by which time it
had drifted down onto the flip and microphone row. So "462 and 496" were the rotate icon
and the microphone, and the four glyphs were somewhere else on the screen the whole time.

The lesson is not "measure rather than eyeball". That lesson had already been learned
here and it was obeyed — this was a measurement, not a guess. The lesson is that **a
measurement is only as good as the coordinates it is anchored to**, and coordinates read
from an accessibility tree are true for the frame they were dumped from and for no
other. Take the bounds and the pixels from the same capture, or the number describes a
screen that no longer exists.

The user reported the arrows working before any of this was re-measured. They were right
and the instrument was wrong.

### An accessibility node's bounds are not the drawn bounds

Twice in one session, geometry was read from a `uiautomator` dump and was wrong.

- The menu button's node reports `x 114..228` while the square it draws is at
  `x 128..233` — **14 px to the right** of what the node claims.
- The same node reported a height of **133 px in both** the tall and the thin
  densities, while the drawn square measured **134 px** and **95 px** respectively.
  Believing the node meant concluding the new setting did not work, twice, and going
  looking for a wiring bug that was not there.

Node bounds are reliable for *what is on screen and what it is called*. They are not
reliable for *where it is or how big it is*. For geometry, count pixels in the
capture, and take the capture and the dump from the same moment.

### A byte-identical debug APK is not a stale one

`composeApp-debug.apk` has been **74070078 bytes** across every build for days,
including builds that changed behaviour, and the number has been used here as evidence
that a build was stale. It is not evidence either way: the zip is dominated by the
native library and Compose assets, and a small code change does not move the total.

To settle it, look inside: `unzip -Z1 …apk | grep '\.dex$'` and grep **every** dex for
a string the change introduced. Checking only `classes.dex` returns 0 for a string that
is present, because the app has seven of them and the value was in `classes5.dex`.
