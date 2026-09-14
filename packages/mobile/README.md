# ldmux mobile (KMP + Compose Multiplatform)

The native Android client. Landscape-only, minimal chrome, with the terminal
rendered by the **same** `packages/web/public` client inside a `WebView` (the
`TerminalSurface` seam) — one terminal engine, never two (ADR-0006).

This is the app `docs/SPEC.md` calls for (§4.2, §7). The PWA in
`packages/web/public` remains the daemon-served client and the renderer; this
package is the native shell around it.

## Stack

- Kotlin Multiplatform + Compose Multiplatform (`composeApp`, `androidTarget`).
- `expect`/`actual` for the two platform things: terminal surface (WebView) and
  settings persistence (SharedPreferences).
- Landscape (`sensorLandscape`), dark, one accent colour.

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

- [x] KMP/Compose shell that builds, installs and runs; landscape; WebView terminal.
- [x] Settings (host + token + panel side) persisted on device.
- [x] **No top bar**: a slim side panel (left or right, a setting) carries
      back/forward between open shells, the extra keys, `+shell` and settings.
- [x] Session list + `new`/`kill` over the daemon's WebSocket; one page per live
      tmux session in a `HorizontalPager` (swipe, or the side arrows).
- [x] Sidebar keys drive the terminal through a small JS bridge
      (`window.ldmuxSendKey`); verified on device (a tapped `|` reached the pane).
- [ ] Native terminal rendering (libghostty or an alternative) to replace the
      WebView — see the research note in `docs/`. Until then the WebView is the
      only terminal engine (ADR-0006).
- [ ] Foreground service so the heartbeat survives Doze (SPEC §6.3).

Verified on a real Motorola Edge 2024 (2026-09-14): landscape, panel left/right,
back/forward, `+sh`, sidebar key reaching the shell, and `chrome=0` removing the
web client's own top bar so there is exactly one chrome.
