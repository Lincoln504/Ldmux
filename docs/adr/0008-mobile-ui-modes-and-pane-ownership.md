# ADR-0008 — Mobile UI modes, and who owns the pane's geometry

**Status:** Accepted (2026-09-14); §1 superseded by [ADR-0011](0011-screen-orientation.md)

> **Amended (2026-10-04, D4).** The tmux session layer this ADR assumed is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018.

> The ownership rule (ADR-0010) survives unchanged in shape; what changed is who
> sizes the pane: the keeper applies the holder's size, not tmux's `window-size`.

## Context

Three things were being decided implicitly by the first native slice, and all
three had visible consequences on the device:

1. **Rotation.** The activity was `sensorLandscape`. The soft keyboard on a
   phone in landscape covers most of the screen (and, on this device, most of
   the side panel too), which makes landscape the *wrong* place to type.
2. **Who owns the pane size.** The daemon creates sessions at a fixed
   `120x40`. With `window-size manual` (set per session at creation), nothing ever
   resized a pane afterwards, so a phone rendered a 120-column grid scaled down
   to ~12 px cells. ADR-0002 fixed the *canonical* geometry (cols/rows, never
   pixels); it did not say who gets to choose it at runtime.
3. **Chrome while typing.** The side panel is the only chrome (ADR-0006), and it
   was rendering *during* typing, competing with the keyboard for a 1080 px-tall
   landscape window.

The prior art offered two relevant precedents and no answers to (1): client-driven
winsize with server-side bounds validation (one to two hundred rows, one to five
hundred columns), and a recorded warning that
scale-to-fit breaks pointer→cell mapping if coordinates are not scaled with it
(`PREDICTIVE_ECHO…:210-245`). Their notes contain **no** soft-keyboard
orientation or scale-vs-resize policy, so that part is a new decision.

## Decision

### 1. Two explicit modes, chosen by a button. The sensor rotates nothing.

- **Landscape** — side panel (left or right, a setting), key matrix, `+sh`,
  settings. The keyboard is **never** up. `requestedOrientation =
  SCREEN_ORIENTATION_LANDSCAPE`.
- **Portrait (typing)** — no side panel; the terminal fills the width; the
  keyboard is **always** up; a **thin single-row bar rides directly above the
  keyboard** carrying the way back (`⤢`) plus only the keys a soft keyboard
  cannot produce (Esc, Tab, arrows, C-c) and the microphone.
  `requestedOrientation = SCREEN_ORIENTATION_PORTRAIT`.

### 1. ~~Rotation.~~ **Superseded by ADR-0011.**

> **Do not implement this.** Both orientations were locked and the IME was watched with a
> 600 ms debounce. ADR-0011 deleted all of it: the device owns the rotation, the app owns
> the keyboard, and there is no flip button — `grep -rn 'requestedOrientation\|SCREEN_ORIENTATION'`
> over `packages/mobile/composeApp/src` returns nothing. The paragraph is kept because the
> reasoning ("a terminal that reflows because the phone tilted is worse than one that
> stays put") is a real tension that ADR-0011 resolves the other way, by noticing that the
> user can simply not tilt it.

Both orientations are **locked** (`SCREEN_ORIENTATION_LANDSCAPE` /
`SCREEN_ORIENTATION_PORTRAIT`, not the `sensor*` variants). A terminal that
reflows because the phone tilted is worse than one that stays put: the user
turns it.

Because portrait *means* typing, dismissing the keyboard in portrait returns to
landscape rather than leaving a portrait layout with no keyboard. The IME inset
is watched (debounced 600 ms, because it passes through zero during the rotation
itself) rather than assumed.

### 2. The viewing client owns the pane geometry.

The client measures its own viewport, computes the grid that fits, and tells the
daemon (`Resize`, or `cols`/`rows` on `Attach`). The daemon clamps into
`MIN/MAX_PANE_COLS` (20–500) and `MIN/MAX_PANE_ROWS` (5–255), the same interval
the prior art recorded. (It was 300 until 2026-09-20 — a frame carries a selection's rows in
one byte each while its columns get two, so a taller pane wrapped its selection rows
modulo 256 — and runs `tmux resize-window -x -y`.) The daemon still
enforces the bound, so a buggy or hostile client cannot ask tmux for a window
whose capture would blow the stream's byte budget.

The pane is resized in **both** modes, from the *visible* viewport (in portrait
that viewport already excludes the keyboard). An earlier iteration skipped the
resize while typing and scaled the landscape grid instead; on the device that
produced a postage-stamp grid with large empty bands, which is precisely the
outcome the prior art warns about. Resizing is the clean answer; the grid then
draws at 1:1 and the scale transform only absorbs rounding.

Resizes are debounced 250 ms in the surface, so a rotation that emits two or
three intermediate viewport sizes results in one request, not three.

### 3. The terminal is painted in the terminal's own colours.

The JNI frame header was widened from 10 to 16 bytes to carry the emulator's
default foreground and background (`native/ldmux_terminal.c`,
`TerminalFrame.HEADER_SIZE`). The surface paints its whole area — including the
letterbox left by rounding — in the emulator's own background, so the terminal
does not sit in a differently-coloured rectangle. The first working slice
painted the letterbox in the app's background, which reads as a rendering bug.

### 4. Voice input is headless, and is not the keyboard's microphone.

Dictation goes through Android's `SpeechRecognizer` (on-device where available,
API 31+), **not** the IME's voice typing: the IME path requires a visible
keyboard and a live `InputConnection`, which is the thing a terminal wants to
avoid. `RECORD_AUDIO` is requested at first use, never at launch. iOS will use
`SFSpeechRecognizer` with `requiresOnDeviceRecognition` (or iOS 26
`SpeechAnalyzer`), which is the same shape.

## Consequences

- Rotation is a deliberate act with a visible control; the layout never fights
  the user.
- Every mode change costs one pane resize, i.e. one round trip and one reflow
  of the remote shell. This is acceptable because mode changes are explicit and
  rare; it would not be acceptable for the keyboard animating in and out, which
  is why landscape never raises the keyboard.
- Other clients viewing the same tmux session are *not* told about a resize yet
  (the resizing client gets a fresh snapshot; others will follow on their next
  output or resync). Correct multi-client geometry is still an open problem:
  with `window-size manual` there is exactly one window size, so two clients of
  different sizes cannot both be "exactly right". `window-size latest` or
  `aggressive-resize` are the levers (see ADR-0002 for the geometry contract).
- The scale-to-fit transform was **removed**. Nothing is scaled; the grid is
  drawn 1:1 and an oversized one (a rotation or font change whose resize has not
  landed yet) is clipped, anchored bottom-left, so the prompt stays on screen.
  Pointer→cell mapping is therefore a pure translate, and the warning below no
  longer applies — but if a scale ever returns, it returns here, and both the
  draw and the hit-test must keep reading one `GridPlacement`.
- Font size is a **client setting** (settings dialog, or pinch), and it feeds
  back into the geometry: a bigger font means fewer columns, so the shell is
  reflowed rather than the grid being scaled down. This is the same ownership
  rule as §2, applied to the font. It is per shell and it is **not persisted**
  (amended 2026-09-27): a pinch lives for as long as that shell's composition
  lives, and the dialog's text size is the default a new shell starts at —
  saving the pinch had made one shell's zoom re-size every other open one.
- Input latency is unchanged by this ADR. Predictive echo is designed in.

## Alternatives rejected

- **Sensor rotation.** Reflows the shell whenever the phone tilts, and makes
  "is the keyboard up?" a function of physical orientation.
- **Keyboard in landscape.** The keyboard occupies most of a 1080 px-tall
  landscape window; the mode split removes the conflict instead of managing it.
- **Scale the landscape grid in portrait** (keep the pane, shrink the pixels).
  Verified on device to look broken (tiny text, large dead bands).
- **Keep the side panel while typing.** It competes with the keyboard for the
  same axis and, on this device, got clipped. Replaced by the thin bar.
- **Resize on every viewport change including IME animation frames.**
  Thrashes the pane; debounced instead.