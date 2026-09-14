# Native terminal rendering (replacing the WebView)

Status: **research / decision pending** (2026-09-14). The Android app currently
renders each session with the shared web client (`packages/web/public`) inside a
`WebView` — one terminal engine, per ADR-0006. This note records what it would
take to replace that with a native renderer, as requested.

## What we need

A terminal **core** (VT parser + screen state: cells, attributes, cursor,
scrollback, reflow) plus a **renderer** that draws the grid with Compose/Skia,
drivable from Kotlin Multiplatform on Android (iOS is a target, not v1).

## libghostty / libghostty-vt — CORRECTED (verified 2026-09-14)

An earlier revision of this note claimed libghostty-vt was unreleased, with no
mobile build and no Kotlin bindings. **That was wrong.** Verified directly
against crates.io, npm, and Ghostty's own `build.zig`:

- The Ghostty terminal core is published as **libghostty-vt**, under its **own
  version** (`lib_version = "0.1.0-dev"` in `build.zig`, a separate release cycle
  from the app). The C API header is `include/ghostty/vt.h`. It still carries a
  "incomplete, work-in-progress, not yet stable" warning, and the Zig module says
  the *API* may change while the *functionality* is production-proven. So the
  caveat is API stability, not maturity.
- **Ghostty's own `build.zig` builds all of**: a static lib (`libghostty-vt.a`),
  a shared lib, a **WASM** module (`ghostty-vt.wasm` — what ghostty-web uses),
  and an **Apple universal `.xcframework`** (macOS + iOS + iOS-simulator, via
  `xcodebuild`, macOS host). Minimum **Zig 0.16.0**. Its CI cross-compiles for
  Android. `zig build -Demit-lib-vt -Demit-xcframework=true` emits the Apple one.
- **Rust bindings are published**: `libghostty-vt` 0.2.1 and `libghostty-vt-sys`
  0.2.1 (`uzaaft/libghostty-rs`, MIT, ~113 k downloads; vendored Zig build by
  default, `link-dynamic`/`pkg-config`/`vendored` features; iOS targets build
  through the xcframework emit and require a macOS host). A sibling wrapper,
  `mnml-libghostty-vt` 0.2.3, exists too.
- **Real Android consumers exist** — this is the important one:
  `tapthaker/ghostty-android`, `SagerNet/libghostty-android` (published to Maven
  Central as `io.github.sagernet:libghostty-android:0.1.0-alpha01`, with Compose
  UI), and `pinhole-io/libghostty-android`. All cross-compile libghostty-vt with
  Zig for `arm64-v8a` (+ `x86_64`), render with **Compose Canvas** over a JNI
  bridge, and need NDK r29+. The VT core itself is libc-free; only a PTY wrapper
  needs Android libc.
- **A WASM drop-in for our web client exists today**: `coder/ghostty-web` (MIT,
  npm `ghostty-web` 0.4.0) compiles the Ghostty core to a ~423 KB
  `ghostty-vt.wasm` behind an **xterm.js-compatible `Terminal` API**, with zero
  runtime dependencies. Canvas-only (no WebGL, ligatures, or image protocols) and
  requires `await init()`. Its `buffer.active.viewportY` / `baseY` / `onData` /
  `onScroll` / `scrollToLine` surface matches exactly what ldmux already uses.

Net: the earlier "revisit trigger" (stable ABI **and** shipped iOS/Android
builds) is now **substantially met** — an iOS xcframework and Android builds both
exist, and a web path exists now. The single remaining caveat is the unstable C
API (pinned as `0.1.0-dev`).

## Verified build (2026-09-14)

`libghostty-vt` **builds for Android on this machine today.** From Ghostty
`d5eba8d` with Zig `0.16.0` (from the pinned nixpkgs) and
`ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/android-ndk-r27c`:

```
nix shell nixpkgs#zig -c zig build -Demit-lib-vt \
  -Dtarget=aarch64-linux-android -Doptimize=ReleaseSafe
```

Output: `zig-out/lib/libghostty-vt.a` (21 MB, static),
`libghostty-vt.so.0.1.0` (12.8 MB, shared), and the full
`include/ghostty/vt/*.h` header set. The build's own `pkg/android-ndk/build.zig`
needs the NDK only for bionic libc include/lib paths — the VT core itself is
libc-free — so a *newer* NDK is not strictly required for the engine, though we
will take the latest. This proves the native Android path end to end at the
**engine** level; what remains is the JNI wrapper, the shared Kotlin core, and
the Compose Canvas renderer.

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
- **libghostty was rejected twice, with a revisit trigger of stable ABI
  **and** shipped iOS/Android targets.** (Their rejection predates the 2026-08
  Android projects and coder/ghostty-web; see the corrected section above — the
  trigger is now substantially met, leaving only API stability.)
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

## Recommendation (revised 2026-09-14)

The goal is a **native** terminal on both phones, from **one shared Kotlin
Multiplatform core** — not a web engine. WASM was considered and **rejected**: it
would be a second engine, it parses on the browser main thread, it is Canvas-only
with no WebGL, and it buys nothing over the native library we would already link.

1. **Engine: libghostty-vt, linked natively.** It is the Ghostty emulation core,
   builds a static/shared lib, and is the reason the Android projects listed
   above exist. Zig **0.16.0** is the exact minimum and is available in the pinned
   nixpkgs, so the toolchain is reproducible via Nix. The C API is `0.1.0-dev`
   (unstable), so we pin the upstream commit and keep all C calls behind one
   narrow Kotlin seam.
2. **Shared KMP core ("shared libraries").** One Kotlin Multiplatform module owns
   the terminal:
   - an `expect`/`actual` native seam (`TerminalCore`) that loads the Zig-built
     `libghostty-vt` — Android via **JNI** (`System.loadLibrary`), iOS via a
     **cinterop**/xcframework — exposing `advance(bytes)`, styled cell grid +
     cursor, and key/mouse encoding;
   - the **remote protocol client** (attach/caps/snapshot, binary OUTPUT,
     input/key/follow/scrollback) in shared Kotlin (a port of the web client's
     `core.js` protocol half), unit-tested on the JVM;
   - the **renderer**: a Compose Multiplatform `Canvas` over the cell grid, so the
     same drawing code runs on Android and iOS.
3. **Android first, iOS second, independently.** Android: cross-compile
   libghostty-vt with Zig for `arm64-v8a`/`x86_64` against the NDK, bridge via JNI,
   render with Compose. iOS: consume the upstream `.xcframework` (built on macOS)
   and render with the *same* Compose Canvas. The shared module and the protocol
   code are written once; only the tiny native-loading `actual` and the app shells
   differ.
4. **Never split engines** (inherited from dooboxes): the web client keeps its
   engine, but the native apps share exactly one core. Do not ship libghostty on
   Android and a different core on iOS.
5. **Fallback if the Zig/native path stalls:** `alacritty_terminal` (Rust,
   dependency-light, cross-compiles to both) behind the same seam, or Termux
   `terminal-view` (fast, but **GPL-3.0** makes the app GPL).

## What "native" also requires (independent of the core)

The native shell currently gets its session list and `new`/`kill` over the
daemon's WebSocket (`LdmuxClient`), but the **terminal itself** is the web client.
A native renderer means the app also speaks the rest of the protocol directly:
`attach` → `caps`/`snapshot`, binary `OUTPUT` frames, `input`/`key`, `follow`,
`scrollback` — i.e. a native port of `packages/web/public/core.js` + `app.js`'s
protocol half. That is the bulk of the work, and it is testable on the JVM
without a device.

## Implementation notes (2026-09-14)

The first working native slice (JNI shim + Compose Canvas) surfaced two
non-obvious facts, both confirmed on a real device:

1. **Snapshots need CR+LF.** `tmux capture-pane` separates rows with a bare LF.
   A VT treats LF as a line feed only — the column is preserved — so feeding the
   raw snapshot to a fresh `libghostty-vt` terminal staircases the screen
   diagonally. The snapshot must be normalised to CR+LF first. (Live PTY output
   already carries CR+LF and must not be rewritten.)

2. **The render-state row/cell iterators skip trailing blank cells.** The JNI
   encoder must therefore emit exactly `cols*rows` cell records, padding the
   missing ones with blanks. A variable count per row desynchronises the
   client's fixed-stride decode, which also shows up as diagonal drift.

Also confirmed: the render state must be populated into the pre-allocated row
iterator with `ghostty_render_state_get(..., GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR,
...)` after every update, or iteration yields nothing (a header-only 10-byte
frame).
