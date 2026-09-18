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
just native-android            # libghostty-vt + JNI shim for arm64-v8a + x86_64
```

`scripts/build-libghostty-vt.sh` cross-compiles a pinned Ghostty commit with
Zig (from the devShell) and the NDK, then links `native/ldmux_terminal.c` into
`libldmux_terminal.so` per ABI under `composeApp/src/androidMain/jniLibs/`.
Those libraries are build outputs (git-ignored); the `android` just recipe
depends on `native-android` so they are always current.

Data path: daemon `snapshot`/binary `Output` frames -> `AttachClient` ->
`TerminalEngine.write` -> `NativeTerminal.render` (a fixed `cols*rows` cell
frame) -> `TerminalFrame.decode` -> `TerminalPainter` -> Compose `Canvas`.

Two things worth knowing (both live gotchas):

- **A tmux `capture-pane` snapshot separates rows with a bare LF.** A VT treats
  LF as a line feed only (column preserved), so a snapshot must be handed
  CR+LF or the screen staircases. `TerminalController` normalises it.
- **The native row/cell iterators may skip trailing blank cells.** The encoder
  must therefore emit exactly `cols*rows` records, padding blanks, or the
  client's fixed-stride decode desynchronises.

## Build

The JDK + Gradle live in a dedicated devShell so the main shell stays lean:

```bash
nix develop .#mobile --command gradle -p packages/mobile :composeApp:assembleDebug
# APK: packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
adb install -r packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk
```

`local.properties` points Gradle at the Android SDK and is git-ignored; on this
machine the SDK is `/home/ldeen/Android/Sdk`. `ANDROID_HOME` is exported by the
`mobile` devShell as a fallback.

### The native library is a build output

`libldmux_terminal.so` is produced by `scripts/build-libghostty-vt.sh` (pinned
`GHOSTTY_COMMIT`) into `composeApp/src/androidMain/jniLibs/<abi>/` and is
git-ignored. The build script resolves the NDK by each directory's
`Pkg.Revision`, never by sorting names — `sort -V` orders `30.0.16248370` before
`android-ndk-r27c`, which silently picks the *older* toolkit.

## A test daemon, and the two ways it lies

The daemon talks to tmux in control mode. A dev daemon started outside the
devShell uses the **system tmux (3.5a)** while the server was created by the
devShell's **3.7b**, and the two cannot talk: the daemon starts, listens, accepts
a WebSocket and serves nothing at all. The app shows a healthy `link 20 ms`
readout over a black screen, which is indistinguishable from a rendering bug.
Run the dev daemon on the devShell's own `node` and `tmux`:

```bash
# resolve once, then paste the store paths into the launcher script
nix develop --command bash -c 'readlink -f "$(command -v node)" "$(command -v tmux)"'
```

`nix develop` itself cannot be used as the launcher: it does not survive being
backgrounded from an agent shell (`nix` is then not on the unit's `PATH` either).
A transient user unit does survive, and is how the daemon runs in production too:

```bash
systemd-run --user --unit=ldmux-test-daemon --collect /tmp/ldmux-test/run.sh
journalctl --user -u ldmux-test-daemon -n 20 --no-pager
```

One more trap, for inspecting that server: the system tmux cannot see it either,
so every query has to go through the devShell —
`nix develop --command bash -c 'tmux -L ldmux-test list-sessions'`.

## Pointing it at a daemon

The app's `settings` opens a dialog for `host[:port]` and the bearer token. For a
USB-attached phone with no tailnet, forward the daemon's loopback port and use
`localhost`:

```bash
adb reverse tcp:7629 tcp:7629
# settings -> host: localhost:7629, token: <cat <state-dir>/token>
```

## Modes

Two explicit modes, switched by a button — the sensor never rotates the app
(ADR-0008):

- **Landscape** — side panel (side is a setting), key matrix, `+sh`, settings;
  the keyboard is never up.
- **Portrait (typing)** — no panel, terminal fills the width, keyboard always
  up, and a thin bar rides directly above the keyboard with `⟳` (back to
  landscape) plus Esc, Tab, arrows, C-c. No microphone here: the keyboard is
  always up in portrait and has its own mic.

The terminal is resized to the viewport in both modes: the daemon sizes the tmux
pane to the grid the surface can actually paint, so the shell is always the
right size for the screen showing it.

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
  33–150 ms (`FramePacing.kt`). The sidebar shows `link 42 ms · 30 fps`.

## Status

- [x] KMP/Compose shell that builds, installs and runs.
- [x] Settings (host + token + panel side) persisted on device.
- [x] **No top bar**: a slim side panel carries back/forward between open
      shells, the extra keys, `+shell` and settings.
- [x] Session list + `new`/`kill` over the daemon's WebSocket; one page per live
      tmux session in a `HorizontalPager` (swipe, or the panel arrows).
- [x] **Native terminal rendering**: libghostty-vt via JNI, Compose Canvas
      renderer, live attach stream, 1:1 with the pane. Verified on device.
- [x] **Client-driven pane resize**: the phone's viewport is the authority;
      the daemon clamps and runs `tmux resize-window`.
- [x] **Landscape/portrait modes** with a thin bar above the keyboard.
- [x] Sidebar keys drive the native terminal directly; on-screen keyboard is
      opt-in through the `⌨` button (a tap on the grid never raises it).
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
- [ ] Foreground service so the heartbeat survives Doze (SPEC §6.3).
- [ ] iOS (needs a macOS host for the xcframework).

Verified on a real Motorola Edge 2024 (2026-09-14): landscape with the side
panel; `⌨` flips to portrait with the keyboard up and the key bar above it;
typing reaches the pane; `⟳` returns to landscape; the pane resizes per mode
(76x20 landscape, 46x29 portrait) and the grid draws 1:1 in the terminal's own
colours.

## Known issues

### The page you are on is lost when the mode flips

Reproduction, on device: open two or more shells, tap `▶` until the header reads
`n / n`, flip to portrait with the flip button. The header comes back as `1 / n`
and the terminal behind it is the first shell. Verified three times, every time.

The mechanism is understood and is not a race. `PagerState` holds its position as
a **pixel offset into a measured page**, not as a page number. On landscape the
terminal is `2400 - 636 = 1764` px wide, so page 2 is an offset of 1764 px into a
1764 px page. Mid-rotation the terminal is briefly the full 2400 px of the
still-landscape window, and `floor(1764 / 2400)` is `0`, which is page one. The
re-measure rewrites the pager's position and the page is gone.

What was tried, and why each failed — recorded so it is not re-tried:

- Hoisting the page index into `App()` and restoring it. The new pager reports its
  first page and the report overwrites the restore.
- Reporting only on a settled swipe (`isScrollInProgress`, first value dropped).
  Does not fire for `animateScrollToPage`, which is what the arrows use.
- Having the arrows set the index directly. Correct in itself and kept, but it does
  not survive the re-measure either.
- Gating on the display's orientation (`LocalConfiguration`). The configuration is
  delivered at the **start** of the rotation, not the end, so the gate never fired.
- Capturing the page at the last composition before the mode change and restoring
  it 820 ms later. Still wrong, so the re-measure lands after that too.

The live suspect is that a restore issued at any fixed delay is racing a re-measure
with no known bound. The likely correct fix is structural: one `HorizontalPager`
instance for both modes, with the chrome arranged around it, so the pager is never
re-created and never re-measured against a different width.

### Rotation timings, measured

`WindowManager: finishDrawing of orientation change` on the test device reports
58 ms, 125 ms and 168 ms. `PANEL_FLIP_MS` is set from that: the panel waits out the
turn rather than being drawn into the old window and carried round with it.

### `adb install -r` looks like a crash

`am force-stop` followed by an install kills the process with no crash record. It
was mistaken for an app crash during development. Check
`adb shell dumpsys activity exit-info dev.ldmux.mobile` before believing a crash
report: `reason=16 (PACKAGE UPDATED)` is a reinstall, `reason=5/6 (CRASH)` is not.
