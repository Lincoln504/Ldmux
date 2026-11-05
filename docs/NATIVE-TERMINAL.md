# Native terminal rendering (replacing the WebView)

> **Note (2026-10-04, D4).** Unchanged in substance: libghostty-vt is still the one
> engine, now on both sides — the client renders with it and the keeper runs it.


Status: **research / decision pending** (2026-09-14). The Android app currently
renders each session with the native emulator inside a
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

`libghostty-vt` **builds for Android on this machine**, and the whole thing —
engine *and* our JNI bridge — is built by `packages/mobile/scripts/build-libghostty-vt.sh`
from Ghostty `d5eba8d` with Zig `0.16.0` (from the pinned nixpkgs) and the NDK
that `nix develop .#mobile` resolves into `ANDROID_NDK_HOME`:

```
nix develop .#mobile --command packages/mobile/scripts/build-libghostty-vt.sh arm64-v8a
```

The devShell resolves the NDK by the `Pkg.Revision` each installed directory
declares, **not** by sorting the directory names — SDK Manager uses two shapes at
once (`android-ndk-r27c`, `30.0.16248370`), and `sort -V` orders a digit-leading
name before a letter-leading one, which silently picks the *older* toolkit.

Re-verified 2026-09-14 against **NDK r30** (`30.0.16248370`), the current stable:
a `arm64-v8a` `libldmux_terminal.so` of 12,694,672 bytes, against 12.7 MB from the
previous r27c build — the toolkit change is not load-bearing for the engine. The
build's own `pkg/android-ndk/build.zig` needs the NDK only for bionic libc
include/lib paths, and the VT core itself is libc-free.

Before that, the engine alone: `zig build -Demit-lib-vt
-Dtarget=aarch64-linux-android -Doptimize=ReleaseSafe` produces
`zig-out/lib/libghostty-vt.a` (21 MB, static), `libghostty-vt.so.0.1.0` (12.8 MB,
shared) and the full `include/ghostty/vt/*.h` header set.

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

**The face is the app's, not the platform's.** `TerminalPainter` draws with Fira Mono, bundled at
`packages/mobile/composeApp/src/androidMain/res/font/`, and every monospace label in the mobile UI
uses the same family (`dev.ldmux.mobile.MonoFont`). `Typeface.MONOSPACE` is whatever the OEM build
calls monospace, and a terminal's grid is one advance per character, so a face that varies by device
varies the grid with it. Fira Mono ships Regular, Bold and Medium only, so italic cells are slanted
by the renderer (`textSkewX`), which changes no advance. Licence: SIL OFL 1.1, recorded in
`THIRD-PARTY-NOTICES.md`.


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
`scrollback` — i.e. a native port of the browser client's
protocol half. That is the bulk of the work, and it is testable on the JVM
without a device.

## Implementation notes (2026-09-14)

The first working native slice (JNI shim + Compose Canvas) surfaced two
non-obvious facts, both confirmed on a real device:

1. **Snapshots need CR+LF.** A screen dump separates rows with a bare LF (the tmux-era `capture-pane` did; the keeper's snapshot separates rows with CR+LF itself, and the client normalises either).
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

3. **Ask the engine when the question is about the terminal's own state.**
   Tap-to-cursor has three shapes, chosen from the shape of the input surface
   rather than from the application's name — a synthesized SGR click for a
   program that asked for the mouse, strip motion for a wrapping prompt, grid
   motion for a full-screen program. The click path was dead on device: a
   program that had *just* enabled mouse reporting still got arrow keys. The mode
   flags the daemon reports (mouse tracking, SGR mouse) are
   accurate but only arrive with a **snapshot**, so the client's copy was stale
   for as long as the user stayed attached. The emulator had already parsed the
   enabling escape sequence, so it can answer for free and immediately:
   `GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING` (any of X10/1000/1002/1003) and
   `GHOSTTY_TERMINAL_DATA_MODE` with `GHOSTTY_MODE_SGR_MOUSE` (1006, the only
   encoding we synthesize — the older ones cap coordinates at 223). The frame
   header carries both (byte 18 and byte 31), so a tap is a click on the very
   frame after the program asks for one.

   This paragraph used to claim that one flag, alone, separates the two cases:
   "`pi` and `claude` draw a composer of their own on the **primary** screen
   (`alternate_on=0`, no mouse)... while `vim`, `less`, `htop` and `nano` take
   the **alternate** screen." That is false. Measured directly: `pi` emits
   `ESC[?1000h ?1002h ?1003h ?1004h ?1006h ?1049h` — mode `1049` is the
   alternate screen, and `1000`/`1002`/`1003`/`1006` are mouse tracking with
   SGR encoding. So `pi` (and by extension Claude Code and Codex, the same
   class of program) sets **both** flags, exactly like `vim`/`less`/`htop`/
   `nano` — `alternate_on` does not separate them at all, and there is no mode
   anywhere meaning "a click should place the cursor". The engine is still the
   right and cheaper place to ask (`GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN`, byte
   32 — the header grew from 32 to 40 bytes for it) rather than a hand-kept
   list of program names, but telling a TUI composer from a full-screen program
   by this flag is not possible; whatever mechanism actually decides tap
   behaviour has to be something other than "alternate screen on/off".

   The daemon still re-checks before writing anything to the pty, where its view
   is fresh because it polls at click time — and it refuses a click outright when
   the pane's foreground command is a shell (`#{pane_current_command}`), because
   the mouse mode is *sticky*: a program that enables reporting and then exits
   leaves it latched, so the flags say "mouse" while a shell is what would
   receive the bytes, and a shell prints them.

   Same shape as the password-prompt signal (`docs/PREDICTIVE-ECHO.md` §7):
   when the question is about the terminal's own state, the engine is both
   better informed and cheaper to ask than the protocol.

   **Measured 2026-09-19, and the measurement itself was wrong once.** The claim
   "`micro` and `pi` are indistinguishable by every signal the client has or could
   cheaply get" is the conclusion this document already carried, and it holds — but
   an attempt to re-verify it first produced the opposite answer, because the probe
   started `micro -config-dir /tmp/none` and micro **disables mouse reporting when
   its config directory does not exist**. That run reported `alt=0 mouse_any=0
   mouse_sgr=0`, which looked like a genuine separation between micro and pi. With
   micro's normal configuration the answer is `alt=1 mouse_any=1 mouse_sgr=1
   mouse_button=1`, i.e. the same class as pi. The exact invocation is part of the
   measurement; a program's behaviour under a broken invocation is not evidence
   about its behaviour.

   Where the flags do differ, and why it does not help:

   | program | alt | mouse_any | mouse_sgr | mouse_button (1002) |
   |---|---|---|---|---|
   | shell (`bash --norc`) | 0 | 0 | 0 | 0 |
   | pi's own mode set (`?1000h ?1002h ?1003h ?1004h ?1006h ?1049h`) | 1 | 1 | 1 | 1 |
   | `micro` (normal config) | 1 | 1 | 1 | 1 |
   | `htop` | 1 | 1 | 0 | 0 |
   | `less` | 1 | 0 | 0 | 0 |

   `mouse_sgr_flag` (1006) separates the encodings, and byte 31 already carries it,
   which is what lets the client refuse to synthesize a click a pre-SGR program could
   not decode. Nothing separates *what the click will mean* — that is a property of
   the application's own event handling, not of the terminal protocol, and no mode
   bit in `ghostty/vt/modes.h` means "a click places the cursor". Verdict by
   observation is therefore the only mechanism, and it costs the thing it costs: the
   click has already been delivered by the time the answer arrives.

## The capture is a screen to replay, not text to read

`capture-pane -J` joins rows the terminal *wrapped*, so a long logical line comes
back as one row. That is right for a person reading `ldmux capture` and **wrong
for every client**, because a client does not read the rows — it replays them into
an emulator of the pane's width, where the wrap is a consequence of that width and
reproduces itself. Joining destroys that.

Measured on a device: the pane held a 105-column line wrapped over two rows at 76
columns. The capture returned it as **one** row. The client re-wrapped it into two,
so the capture had one row more than the pane has, the emulator scrolled by one to
fit it, and the top line was silently deleted. On a 20-row pane whose first row is
the shell prompt, the prompt was simply gone — the screen showed a cursor and
nothing else, which reads as a renderer bug and is not one.

So `joinWrapped` is **off by default** and only `ldmux capture` (text being read)
asks for it. This is the kind of option where the default is the whole bug: the
wrong one fails silently, produces a plausible-looking screen, and looks like a
problem somewhere else entirely.
