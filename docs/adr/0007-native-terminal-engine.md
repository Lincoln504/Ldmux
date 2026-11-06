# ADR-0007 — Native terminal engine: libghostty-vt behind a shared KMP core

**Status:** Accepted (2026-09-14); supersedes the "native renderer is a later swap" stance of ADR-0006 §4

> **Later (2026-09-18):** the second client this decision tolerated — the
> buildless browser client in `packages/web` (`docs/WEB.md`) — has been
> **deleted**. Native KMP/Compose is the only renderer, so "never split
> engines" is no longer a rule about two live engines but a fact about one.
> The references below are left as written: an ADR records what was decided
> when it was decided.

## Context

- The phone apps must be **native** — a `WebView` hosting the web client was
  acceptable as an interim shell (ADR-0006) but is not the target. The interim
  shell survives only as `packages/mobile`'s current implementation, not as the
  architecture.
- We want **Kotlin Multiplatform + Compose Multiplatform**: Android and iOS are
  developed as **independent apps over a shared KMP core**, sharing the terminal,
  the protocol client, and the renderer, while each app owns its platform shell.
- Earlier research in `docs/ARCHITECTURE.md#1-components` wrongly recorded libghostty-vt as
  unreleased. **Corrected (verified 2026-09-14):** Ghostty's terminal core is
  published as `libghostty-vt` with its own version (`0.1.0-dev`); Ghostty's own
  `build.zig` produces a static lib, a shared lib, WASM, and an Apple
  `.xcframework` (macOS + iOS); Zig **0.16.0** is the minimum; and multiple real
  Android consumers exist (`tapthaker/ghostty-android`,
  `SagerNet/libghostty-android`, `pinhole-io/libghostty-android`), all rendering
  the core with Compose Canvas over JNI.

## Decision

**One native terminal engine, `libghostty-vt`, linked into one shared Kotlin
Multiplatform module, rendered with Compose Multiplatform `Canvas`.**

1. **Engine — libghostty-vt.** It is the Ghostty emulation core: grid, scrollback,
   reflow, Unicode/grapheme handling, and key/mouse encoding, all proven in the
   shipped Ghostty app. It owns the terminal; we do not reimplement any of it.
   The C API is `0.1.0-dev` and **unstable**, so the upstream commit is pinned and
   every C symbol is confined behind one narrow seam below.

2. **Shared KMP core.** A single module (`packages/terminal`, `TerminalCore`)
   contains:
   - the **native seam** — `expect`/`actual` over the Zig-built library: Android
     links it with **JNI** (`System.loadLibrary`), iOS via **cinterop** over the
     `.xcframework` — exposing `advance(bytes)`, a styled cell-grid + cursor
     snapshot, and key/mouse encoders;
   - the **protocol client** — attach/caps/snapshot, binary `OUTPUT`, `input`,
     `key`, `follow`, `scrollback` — in shared Kotlin, a port of the web client's
     protocol half (`packages/web/public/core.js`), unit-tested on the JVM;
   - the **renderer** — a Compose Multiplatform `Canvas` drawing the cell grid, so
     Android and iOS draw with the same code.

3. **Platforms are independent apps over the shared core.** Android is built
   first (cross-compile the core with Zig for `arm64-v8a`/`x86_64`, JNI, Compose);
   iOS second (consume the upstream `.xcframework` built on a macOS host, cinterop,
   the same Compose Canvas). Only the small native-loading `actual` and each app's
   shell differ.

4. **Never split engines.** Android and iOS link the *same* `libghostty-vt`
   version. Do not pair one engine on Android with another on iOS.

5. **Nix owns the toolchain.** The mobile devShell gains `zig` (0.16.0, the exact
   minimum) and the Android NDK, so the core's cross-compile is reproducible
   rather than hand-managed.

6. **Rejected:** WASM in the native apps (a second engine, main-thread parsing,
   Canvas-only, no benefit over linking the native library); a second off-the-shelf
   Kotlin terminal widget; and `xterm.dart`/Flutter (out of the KMP/Compose plan).

## Consequences

- The web client is unchanged and remains the reference implementation of the
  protocol; the shared Kotlin protocol client mirrors it (and both are tested).
- The unstable C API is the main risk. Mitigation: pin the upstream commit,
  isolate the C surface in one module, and cover it with a JVM/device test that
  feeds bytes and asserts the cell grid, so an upstream API change fails one test
  rather than the app.
- `packages/mobile`'s `WebView`/`TerminalSurface` shell was the interim
  implementation of the renderer seam; it has since been replaced by the
  Compose Canvas renderer over libghostty-vt (this bullet used to describe the
  replacement as pending — it has shipped, and the WebView path is gone). The
  `TerminalHandle` idea (a renderer-agnostic control surface) is preserved by
  the shared core.
- If the Zig/native path stalls, the seam is core-agnostic: `alacritty_terminal`
  (Rust) can be linked behind the same `actual`, which is the fallback recorded in
  `docs/ARCHITECTURE.md#1-components`.