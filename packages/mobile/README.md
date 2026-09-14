# ldmux mobile (KMP + Compose Multiplatform)

The native Android client. Landscape-only, minimal chrome, with the terminal
rendered **natively**: `libghostty-vt` (the Ghostty terminal core) behind a
Kotlin seam, painted by a Compose `Canvas`. No WebView, no WASM (ADR-0007).

This is the app `docs/SPEC.md` calls for (§4.2, §7). The PWA in
`packages/web/public` remains the daemon-served browser client; this package is
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

## Pointing it at a daemon

The app's `settings` opens a dialog for `host[:port]` and the bearer token. For a
USB-attached phone with no tailnet, forward the daemon's loopback port and use
`localhost`:

```bash
adb reverse tcp:7629 tcp:7629
# settings -> host: localhost:7629, token: <cat <state-dir>/token>
```

## Status

- [x] KMP/Compose shell that builds, installs and runs; landscape.
- [x] Settings (host + token + panel side) persisted on device.
- [x] **No top bar**: a slim side panel (left or right, a setting) carries
      back/forward between open shells, the extra keys, `+shell` and settings.
- [x] Session list + `new`/`kill` over the daemon's WebSocket; one page per live
      tmux session in a `HorizontalPager` (swipe, or the side arrows).
- [x] **Native terminal rendering**: libghostty-vt via JNI, Compose Canvas
      renderer, live attach stream. Verified on device.
- [x] Sidebar keys drive the native terminal directly (verified on device: a
      tapped `|` reached the pane, and the view updated live).
- [ ] On-screen keyboard for arbitrary text input (sidebar keys are not enough).
- [ ] Cursor rendering, touch scrollback, and a fill-the-screen font size.
- [ ] Foreground service so the heartbeat survives Doze (SPEC §6.3).

Verified on a real Motorola Edge 2024 (2026-09-14): landscape, panel left/right,
back/forward, `+sh`, and the full native pipeline — attach -> snapshot ->
libghostty-vt -> Canvas, with ANSI colours, and live output after a sidebar key.
