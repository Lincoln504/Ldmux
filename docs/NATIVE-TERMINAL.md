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

## Prior art: the dooboxes project (read 2026-09-14)

A sibling project (Rust) already worked this exact problem and its decisions are
worth inheriting. Its renderer decision changed **three times**, and the older
docs carry superseded banners — read the ADRs, not the specs.

| When | Artifact | Mobile renderer decision |
|---|---|---|
| 2026-06-29 | `MOBILE_TERMINAL_PERFORMANCE.md`, `MOBILE_TERMINAL_UX.md` | xterm.js in a WebView; libghostty a post-MVP swap |
| 2026-02-08 | `PREDICTIVE_ECHO_AND_TERMINAL_RENDERING_RESEARCH_FEBRUARY_2026.md` | keep xterm.js; native deferred |
| 2026-07-05 | `ADR_2026-07_TAILNET_AND_RUST_TERMINAL_STACK.md` | one engine = **`alacritty_terminal`**, replacing xterm.js; mobile stays WebView for now |
| 2026-07-14 | `ADR_2026-07-14_FLUTTER_MOBILE_AND_SEAMLESS_SESSIONS.md` | **authoritative:** Flutter + `flutter_rust_bridge` + the `alacritty_terminal` core + `CustomPainter` over the cell grid |

Findings that transfer directly:

- **Engine: `alacritty_terminal` v0.26, not libvterm.** Verified dependency-light —
  only `vte`, `base64`, `bitflags`, `unicode-width`, `serde`, `parking_lot` (plus a
  `tty` module they never use). It gives the **grid + scrollback + `Term` API**,
  whereas libvterm is parser-only and would leave us to build and maintain the
  grid. The infamous "Alacritty is hard on Android" is its *app* (winit/fontconfig/
  crossfont), not this crate. It cross-compiles to Android NDK and `aarch64-apple-ios`.
- **The reusable seam is proven in code** (`libs/dooboxes-terminal-core`):
  `Terminal::new(cols, rows)` → `advance(bytes)` in → `take_pty_output()` (query
  replies that must go back upstream or full-screen apps hang) → `cells()` /
  `visible_rows()` / `cursor()` out, plus `encode_key`/`encode_mouse`. A renderer
  only needs the styled-grid-out shape.
- **libghostty was rejected twice, with a testable revisit trigger:** stable ABI
  **and** shipped iOS/Android targets. Only one of those two conditions is not
  enough. (The web research above confirms neither has happened as of 2026-09.)
- **"Never split engines"** — one emulation core for every client. Pairing
  libvterm on mobile with xterm.js on the web would repeat the anti-pattern they
  forbade.
- **The engine is the cheap part; the renderer + input + FFI is the real cost, and
  it is platform-native regardless of engine.** Their effort estimate: WebView
  5–9 weeks vs native 19–28 weeks (~4×).
- **Framework-independent perf targets to reuse:** 30 FPS target (15 acceptable),
  input latency < 100 ms (< 200 acceptable), memory < 50 MB (< 100), battery
  < 3 %/h, frame rate capped at 50 Hz with an RTT-adaptive floor of `max(2 Hz,
  1/2 RTT)`, and 1000-line mobile scrollback.
- Mobile WebView measured (their research): 30–45 FPS iOS / 30–40 Android,
  200–400 ms first paint, 15–25 MB + 5–10 MB per 1000 scrollback lines, 3–5 %/h.
- Their `vte`-based sanitizer policy matches ldmux's existing one (drop device
  queries server-side, by state machine, not by string blocklist).
- Note: their `rendererType: 'canvas'` advice is stale for xterm.js 6 (no such
  option) — same finding ldmux already hit.

## Recommendation

1. **Now:** keep the WebView renderer. It is the working, tested engine and the
   smallest risk while the app's chrome/UX is being settled.
2. **Best native path, revised after reading dooboxes:** **`alacritty_terminal`
   (Rust, Apache-2.0/MIT) over FFI + a Compose Canvas grid renderer** — preferred
   over libvterm because it is a *complete* engine (grid, scrollback, reflow,
   key/mouse encoding) rather than a parser we would have to grow into one. The
   bridge is Rust→Kotlin: **UniFFI** (+ the UbiqueInnovation KMP plugin, which
   targets Android JNI and iOS cinterop from one definition) is the 2026 tool of
   choice; `cargo-ndk` builds the Android `.so`, `xcodebuild -create-xcframework`
   the iOS artifact. The consume shape is exactly dooboxes' proven one:
   `advance(bytes)` in, styled cell grid + cursor out. Cost: adds a Rust
   toolchain to a TypeScript/Bun repo.

   The older equivalent, if we would rather not add Rust: **libvterm (MIT) via the
   NDK** + the same Compose Canvas renderer — but we then own the grid/scrollback
   layer:
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
