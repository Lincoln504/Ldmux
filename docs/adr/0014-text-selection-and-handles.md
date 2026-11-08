# ADR-0014 — Text selection, and the two-point handles

**Status:** Accepted; implemented on Android (`SelectionHandles.kt`, `SystemSelectionHandles.kt`, `SelectionHandlesTest.kt`); iOS not started

> **Amended (2026-10-04, D4).** The tmux session layer this ADR assumed is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018.

> The selection format and the two-point handles are unchanged (they are the
> client's, not the session layer's). Only its `capture-pane` provenance changed.

**Supersedes:** nothing. **Relates to:** ADR-0007 (native engine), ADR-0013 (seams).

## 1. The engine already implements this, which is the whole finding

`libghostty-vt` does not merely have selection internals. It ships the **interaction**:

```
ghostty_selection_gesture_new / _reset / _free
ghostty_selection_gesture_event_new / _set / _event / _get / _get_multi
ghostty_terminal_select_word / _select_word_between / _select_line / _select_all / _select_output
ghostty_terminal_selection_adjust / _order / _ordered / _contains / _equal
ghostty_terminal_selection_format_buf / _format_alloc
```

Verified present in the tree the app builds against, not inferred: 21 exported
declarations in `include/ghostty/vt/selection.h`, re-exported through `src/lib_vt.zig`'s
`@export` list, and gated on by default in `src/terminal/build_options.zig`. The build
script pins `GHOSTTY_COMMIT=d5eba8d` and `~/build/ghostty/.git/HEAD` is that same SHA, so
what is documented here is what is compiled.

The gesture machine covers the interaction asked for by name: a press, a drag, a release,
an **autoscroll tick**, and a deep press. Word boundaries are the engine's, from
`src/terminal/selection_codepoints.zig` — a terminal's idea of a word, which is not a
text editor's.

**Consequence: no engine work, and nothing new on the wire.** Selection, hit-testing,
word boundaries, reflow tracking and formatting are all local. Zero bytes. The standing
"light on the network always" rule is satisfied by construction rather than by effort.

## 2. The interaction

1. **Double-tap** a cell selects the word at it.
2. **Drag** extends the selection, cell by cell.
3. **Drag past the top or bottom edge** autoscrolls and keeps extending.
4. **On release**, the gesture becomes the **two-point affordance**: two handles, one at
   each endpoint, each independently draggable, with a loupe while dragging.
5. Copy puts the text on the system clipboard.

Step 4 is the requirement, and it is the reason this ADR exists rather than a commit:
a selection that ends when the finger lifts is a gesture, not a selection.

## 3. What is the platform's and what is ours

Investigated on both platforms, and the answer is the same on both:

| | Android | iOS |
|---|---|---|
| the two handle shapes | **no public API.** `android/widget/Editor` is absent from `api-versions.xml`, i.e. `@hide`; Compose's own handle drawing is `internal` (mangled name in its ABI jar) | **no public API**; the system's is `UITextInteraction`, which requires adopting the whole `UITextInput` protocol — text ranges, geometry mapping, a tokenizer, an input delegate — for a cell grid it does not model |
| the loupe | `android.widget.Magnifier`, API **28** (constructor) / **29** (`Builder`), constructor deprecated at 29 | **no public API at all** |
| the edit menu | `TextToolbar` via `LocalTextToolbar`, addressed by a `Rect`, not by a widget | `UIEditMenuInteraction` (iOS 16+), needs only a target rect |
| handle haptics | `HapticFeedbackType.TextHandleMove` — already in **common** Compose | same |

So: **the handles and the loupe are drawn by this app on both platforms**, and the menu
and haptics come from the platform on both. That is a good outcome, not a compromise —
it means the handle geometry lives in `commonMain` beside the grid that owns it, and the
"platform-specific" part of the requirement is satisfied where a platform actually
offers something.

Neither platform's stock teardrops are reachable, so "use the platform's handles" was
never available. What is available is the platform's *feel*: handle-move haptics, a
loupe, and — on Android — the system floating toolbar.

**Before adding a seam, check `Modifier.magnifier`.** Compose Foundation 1.12.0 already
factors the loupe behind a `platformMagnifierFactory` parameter, and the artifact
publishes `iosArm64` variants. The shared concept ("the platform's loupe over a rect of
this app's pixels") is therefore already expressed, and an `expect object Loupe` should
only be introduced if the iOS factory turns out to be absent. Haptics and clipboard are
already common Compose (`HapticFeedbackType.TextHandleMove`, `ClipboardManager`).

## 4. Four traps, written down because each one costs an afternoon

### 4.1 `selection_adjust` moves the end endpoint, always — and the start handle still works

`selection.h` says the logical **end** is always the one moved, "regardless of whether
the selection is forward or reversed visually". Read naively that means the visual start
handle cannot be dragged, which would break the two-point affordance outright.

It does not. `GhosttySelection` is a plain caller-owned snapshot of two grid refs plus a
rect flag. To move the visual start: build a temporary selection whose `end` is the
visual start, call `adjust`, then swap the refs back. No private state, no engine change.

Anyone who reads the header first and the requirement second will conclude it is
impossible. It is not, and it is four lines.

### 4.2 The coordinate tag is `VIEWPORT`, not `ACTIVE`

`include/ghostty/vt/point.h` has four tags. The shipped example uses `ACTIVE`, which is
correct **only because it never scrolls**. The rendered grid is the viewport. Getting
this wrong selects the right cell on a fresh screen and the wrong one the moment the user
has scrolled — so **a test that does not scroll first passes either way and proves
nothing.** The test must scroll back, then select, then compare text.

### 4.3 The autoscroll scrolls the terminal itself

`SelectionGesture.zig` decides autoscroll from a **1-pixel** edge test against the
screen height, and `autoscrollTick` scrolls the viewport itself before continuing the
drag. The app supplies the geometry and a ~15 ms timer and **must not scroll as well**,
or it advances two rows per tick.

The app must also pass raw pointer y in surface pixels, plus the surface height: the edge
test cannot be expressed in cells, because a cell index cannot go past the edge.

### 4.4 The engine is destroyed and rebuilt on a snapshot — but not on a resize

`TerminalController` closes and recreates `TerminalEngine` when a snapshot arrives,
and history arrives **when the user scrolls to the top** — that is, during an upward
selection drag. Any engine-owned selection dies there.

The scroll offset counts into the scrollback **from the top**: `0` is the oldest row
and `total - len` is the live bottom. This section used to say the offset was
measured from the live bottom, and that mistake is what broke the page-in — the
request was gated on `scrollOffset < scrollTotal`, which is always true, so the
daemon was never asked (amended 2026-09-27). The guard is `scrollOffset == 0`, the
only place there is older output to ask for.

Two things keep the selection across a rebuild, and a third keeps it out of the
rebuild in the first place:

- A selection is kept as its **range plus its text**, and the text is the test: the range
is put back at the same rows and, if those rows no longer hold the text, the screen the
selection was made on is gone and it is dropped rather than mis-copied. The **offset-based
re-projection** this section used to describe is not implemented and is open: a rebuild
hands the new engine a *fresh* coordinate system (the captured screen at its bottom), so
the difference between the old and new offsets is not a count of lines anything moved by —
and the post-rebuild offset was in any case read before the re-render, making the delta
always zero (archive/STATE.md §8, 2026-09-27).
- `maybeExtendHistory()` is **suppressed while a gesture is live**, which the caller
says on the call (`scrollBy(rows, pageInHistory = false)`): a per-call answer cannot
be left set by a cancelled gesture, which is what a flag would do.
- A **resize** (rotation, font change) no longer rebuilds at all: the captured screen
is replayed into the live engine (`replayResize`/`resizeReplayBytes`), so its
scrollback and its selection both survive. Only attach, resync and a history page-in
rebuild, because only those are asking for a clean parser state.

## 5. Rendering the highlight costs nothing structurally

The render state exposes `GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION` returning
`{size, start_x, end_x}` per row, and its header advises querying it **once per row**
rather than once per cell — ≈40 calls on a phone's grid.

The frame's flags field is read as a full 16 bits and the shim sets bits 0-8 only. So
`FLAG_SELECTED = 1 shl 9` is free: **no header change, no format version bump, no change
to `LDMUX_CELL_RECORD_MAX`.** The painter gains one branch where it already chooses a
background colour.

## 6. Double-tap versus tap-to-cursor

Tap-to-cursor sends a **real SGR click immediately** when the program is tracking the
mouse. A double-tap would therefore deliver two real clicks into the program before any
word is selected — two cursor placements in vim, possibly two button presses in a TUI —
and the first click cannot be un-sent. Deferring every tap by a ~300 ms double-tap
timeout would tax the case where tapping is most useful.

**Decision: the existing mouse-tracking flag decides it, exactly as `TapPolicy` already
does.** If the program is tracking the mouse, a double-tap is two clicks, which is what a
mouse does. If it is not, a double-tap is a word selection. No deferral, no new latency.

## 7. What it means for the `copy` button

> **Removed 2026-10-05.** The copy-mode toggle (and the `copymode` message and `paneInMode` flag it drove) no longer exist; nothing in the client called the toggle. This section is the record of the tmux-era reasoning.

`copy` toggles tmux copy-mode, and its documented reason to exist is that it is the only
way to look at the history of a **full-screen** program. That reason survives local
selection, because a program on the alternate screen has no local scrollback to select
from. So the two are not duplicates: **local selection copies, copy-mode pages a
full-screen program's history.** Its label should say what it does to the pane rather
than what it does to the clipboard, since "copy" now names the gesture.

## 8. Clipboard, and a bug next to it

The app had **no clipboard code at all** — `grep -i clipboard` over the whole source
tree returned nothing.

CORRECTION (2026-09-19). The paragraph that used to be here said the missing callback
was "a bug" and that "a copy performed inside tmux or vim is silently discarded today".
The first half was right and the second was wrong, and the difference matters: the
discard is not an accident, it is `crates/daemon/src/sanitize.rs` doing its job. The
daemon drops OSC 52 from pane output **deliberately**, because the pane is arbitrary
code and the clipboard is shared with every other app on the device — a program inside
a shell silently replacing what the user copies elsewhere is a real attack, not a
feature. The client-side callback was genuinely missing, but it was unreachable by
design, so installing it changed nothing while the policy stood.

Both halves exist now:

- The shim installs `OPT_CLIPBOARD_WRITE` and hands the value across the seam
  (ADR-0013 §2). `OPT_CLIPBOARD_READ` stays **unset**: a program may not ask what is
  on the clipboard even when writes are allowed, because exfiltrating the user's
  clipboard is worse than a copy that does not arrive.
- The daemon keeps dropping OSC 52 unless the host says otherwise. `ldmux
  clipboard-write on|off` writes a policy file in the state directory, read when a
  stream is created, so it can be opened without restarting a daemon that is holding
  live shells. Default: **off**.

On Android 13+ the platform shows its own "copied" confirmation; the app must not add a
second one. A program that reports its own success (pi prints "Copied!" whether or not
anything received the write) is saying what it sent, not what arrived.

## 9. Order of work

1. C shim: pointer down/drag/release/autoscroll, clear, selection text. Every ghostty
   symbol stays inside `ldmux_terminal.c`, which is what that file already promises.
2. `FLAG_SELECTED` bit, decoder, painter.
3. Double-tap, drag, autoscroll timer.
4. Handle mode, including the reversed-endpoint trick (§4.1).
5. Clipboard seam and OSC 52 callback. **Done** (`ldmux_jni.c` +
   `OPT_CLIPBOARD_WRITE`, and paste via `TerminalHandle.paste`). Note the policy: the
   daemon drops OSC 52 unless `ldmux clipboard-write on` (§8).
6. Tests, including the scrolled-screen one (§4.2).

## 10. Known limits

- Alternate screen has no scrollback, so local selection reaches only what is on screen.
- The engine's C API is versioned `0.1.0-dev`; every new call stays inside
  `ldmux_terminal.c` so a `GHOSTTY_COMMIT` bump stays a contained change.
- `TerminalFrame.kt`'s header comment duplicates a `reserved[7]` fragment and does not
  match the 40-byte layout in `ldmux_terminal.h`. The decoder is correct; the comment
  is wrong.

## 11. The handles are the platform's — and the loupe is not there on purpose

**Status:** Accepted. This replaces the "drawn by the app" plan in §1 for the handle
*art*.

### The handles

The app draws every pixel of its terminal, so it has no `TextView` and none of the
selection furniture that comes with one. The framework's handles live inside
`android.widget.Editor`, which is `@hide` and unreachable from the SDK. What *is*
public is the art:

- `android.R.attr.textSelectHandle`, `textSelectHandleLeft`, `textSelectHandleRight`
  are theme attributes. Resolving them yields the same drawables the system's own text
  fields show, at the platform's own shape.

Three things about using them were wrong first, and each is worth recording:

1. **The aspect ratio is the asset's.** They were drawn at a fixed 48x32 dp, which
   squashed a ≈2:1 teardrop into 3:2 and read as a fat blob. The height is now
   chosen (24 dp, the framework's own design height) and the width is derived from the
   drawable.
2. **A modern theme has no separate left and right asset.** It ships one teardrop and
   `Editor` mirrors it for the start. Drawing it twice unmirrored produced two handles
   leaning the same way. So: use the left/right pair when the theme has them, and
   otherwise mirror one for the start handle.
3. **The hotspots are the framework's convention, not ours to improve on.** The left
   handle's hotspot is three quarters of its width and the right handle's a quarter, so
   each teardrop leans inwards with its tip on the corner of the selection and its body
   hanging below the line. `selectionHandleTopLeft` is used by the drawing *and* the hit
   test, so what is visible and what is grabbable cannot drift apart.

The tint is ours: the framework assets arrive tinted with `?attr/colorControlActivated`,
which is blue. The shape is the platform's and the colour is the app's, which is the one
arrangement that gives a native handle in a greyscale UI.

### The handles are dragged immediately

A handle is a grab affordance: the finger lands on it and moves, with no hold first. It
required a long press at first, because one detector was serving both "choose a range"
and "adjust one end", and the practical result was handles that could not be dragged at
all. There is now a gesture that consumes the pointer *only* when the touch begins on a
handle, and it is innermost in the modifier chain so it sees the event before the tap
and scroll gestures — a finger that misses a handle falls straight through and behaves
exactly as before.

### No magnifier. Deliberately.

A loupe was built and then removed, and the reasons are worth keeping:

- Compose's `Modifier.magnifier` magnifies a *surface*. There is no surface here, only a
  canvas inside a Compose layer, so what it magnified was a slab of nothing much, and it
  ignored the size and corner radius it was given.
- Drawing our own lens from the frame works (the grid repainted scaled about the finger
  and clipped to a circle), and it was correct — it magnified real glyphs, flipped below
  the finger when there was no room above, and clamped to the surface. It was removed on
  the user's instruction: **no magnification area of any kind.**
- The precedent is not a gap in the platform. `termux-app` — the reference terminal for
  Android, and the implementation this project read for selection — has no magnifier at
  all: a search of its tree for `Magnifier` returns nothing.

So the absence is a decision, not an omission. Should it ever come back, the machinery
is one function long (`loupeCentre`, deleted with its tests) and the drawing is ten
lines inside the existing canvas.
