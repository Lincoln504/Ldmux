# Native terminal rendering (replacing the WebView)

Status: **research / decision pending** (2026-09-14). The Android app currently
renders each session with the shared web client (`packages/web/public`) inside a
`WebView` — one terminal engine, per ADR-0006. This note records what it would
take to replace that with a native renderer, as requested.

## What we need

A terminal **core** (VT parser + screen state: cells, attributes, cursor,
scrollback, reflow) plus a **renderer** that draws the grid with Compose/Skia,
drivable from Kotlin Multiplatform on Android (iOS is a target, not v1).

## libghostty / libghostty-vt

The most attractive core on quality grounds, and **not yet usable here**:

- `libghostty-vt` (Zig) is **parser + state only** — no rendering, by design
  (embedders draw). Zero dependencies; SIMD parser; robust Unicode, Kitty
  graphics, tmux control mode.
- **Its public C API is still alpha and untagged** as of 2026-09 (the internal
  `ghostty.h` used by the macOS app is explicitly "not a good C API"). Ghostty
  the app is production-stable; the library is not.
- **No confirmed Android NDK or iOS build**, no `xcframework`, no Kotlin
  bindings, no shipped mobile app. Only Linux/macOS/Windows-x64 library builds
  are demonstrated (e.g. Paneflow links a pinned static lib). MIT licensed.
- Guidance from the project is to wait for the tagged C API.

Verdict: **revisit when libghostty-vt ships a tagged C ABI and mobile builds**;
it is the right long-term core.

## Viable alternatives (checked 2026-09-14)

| Option | Kind | Android | License | Notes |
|---|---|---|---|---|
| **Termux `terminal-view`** | parser + Android `View` | yes (Maven `com.termux:terminal-view`) | **GPL-3.0** | proven, battle-tested; it is a `View`, embeddable via `AndroidView`; GPL forces the app to be GPL-compatible (fine for personal use, a problem if distributed proprietary) |
| **libvterm** (Paul Evans) | parser only (C) | via NDK + JNI | **MIT** | the classic embeddable core; we render with Compose Canvas; no prebuilt Android artifact |
| **`alacritty_terminal` / `vte` / `termwiz`** (Rust) | parser (± surface) | via NDK + FFI | MIT / Apache-2.0 | solid cores, but Rust cross-compilation + C-ABI bridge is real work |
| **BossTerm** (`com.risaboss:bossterm-compose`) | parser + Compose Canvas | **no published Android artifact** (module metadata is desktop-JVM only) | LGPL-3.0 | attractive on paper; not usable as a dependency today |
| **Flutter `xterm3`** | parser + renderer | yes | **AGPL-3.0** (v6+) | requires Flutter; AGPL |
| **`termlib`** (ConnectBot) | parser + Canvas | yes | Apache-2.0 | older, Android-only, Java |

## Recommendation

1. **Now:** keep the WebView renderer. It is the working, tested engine and the
   smallest risk while the app's chrome/UX is being settled.
2. **Best native path with an acceptable license:** **libvterm (MIT) via the NDK**
   + a Compose Canvas grid renderer. This is the standard "C core + cinterop/JNI
   + Skia" architecture:
   - compile libvterm as a `.so` per ABI (arm64-v8a, x86_64) with CMake;
   - JNI bridge: opaque screen handle, a `TerminalFrame` snapshot (cells +
     attributes + cursor) copied out per frame;
   - render with `Canvas`/`drawIntoCanvas`, batching glyphs into `SkTextBlob`;
     only changed rows redraw;
   - Android IME via a custom `InputConnection` (or the pattern the web client
     already uses).
3. **If GPL is acceptable:** Termux `terminal-view` gets a native, proven
   terminal on screen fastest, via `AndroidView` — but it makes the app GPL.
4. **Long term:** swap the core to **libghostty-vt** once its C API is tagged and
   Android/iOS builds exist; the frame-copy seam above is core-agnostic, so the
   renderer would not change.

## What "native" also requires (independent of the core)

The native shell currently gets its session list and `new`/`kill` over the
daemon's WebSocket (`LdmuxClient`), but the **terminal itself** is the web client.
A native renderer means the app also speaks the rest of the protocol directly:
`attach` → `caps`/`snapshot`, binary `OUTPUT` frames, `input`/`key`, `follow`,
`scrollback` — i.e. a native port of `packages/web/public/core.js` + `app.js`'s
protocol half. That is the bulk of the work, and it is testable on the JVM
without a device.
