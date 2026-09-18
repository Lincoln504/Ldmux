# ADR-0014 — Text selection, and the two-point handles

**Status:** accepted (design). Implementation not started.
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

### 4.4 The engine is destroyed and rebuilt on every snapshot

`TerminalController` closes and recreates `TerminalEngine` when history arrives, and
history arrives **when the user scrolls to the top** — that is, during an upward
selection drag. Any engine-owned selection dies there.

The existing code already states the invariant that saves it: the scroll offset is
measured from the live bottom, so restoring the offset restores the view. A selection
must be kept as **viewport endpoints** and re-seeded after a rebuild, and
`maybeExtendHistory()` must be suppressed while a gesture is live, so autoscroll stops
at the top of what is loaded rather than destroying the selection it is extending.

## 5. Rendering the highlight costs nothing structurally

The render state exposes `GHOSTTY_RENDER_STATE_ROW_DATA_SELECTION` returning
`{size, start_x, end_x}` per row, and its header advises querying it **once per row**
rather than once per cell — about 40 calls on a phone's grid.

The frame's flags field is read as a full 16 bits and the shim sets bits 0-8 only. So
`FLAG_SELECTED = 1 shl 9` is free: **no header change, no format version bump, no change
to `CELL_RECORD_MAX`.** The painter gains one branch where it already chooses a
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

`copy` toggles tmux copy-mode, and its documented reason to exist is that it is the only
way to look at the history of a **full-screen** program. That reason survives local
selection, because a program on the alternate screen has no local scrollback to select
from. So the two are not duplicates: **local selection copies, copy-mode pages a
full-screen program's history.** Its label should say what it does to the pane rather
than what it does to the clipboard, since "copy" now names the gesture.

## 8. Clipboard, and a bug next to it

The app has **no clipboard code at all** — `grep -i clipboard` over the whole source
tree returns nothing.

Separately: tmux is configured `set-clipboard on`, and the engine parses OSC 52 / 5522 /
1337 and will invoke a clipboard-write callback. **No callback is installed in
`ldmux_jni.c`**, so a copy performed *inside* tmux or vim is silently discarded today.
Same seam, different bug, and worth fixing in the same change.

On Android 13+ the platform shows its own "copied" confirmation; the app must not add a
second one.

## 9. Order of work

1. C shim: pointer down/drag/release/autoscroll, clear, selection text. Every ghostty
   symbol stays inside `ldmux_terminal.c`, which is what that file already promises.
2. `FLAG_SELECTED` bit, decoder, painter.
3. Double-tap, drag, autoscroll timer.
4. Handle mode, including the reversed-endpoint trick (§4.1).
5. Clipboard seam and OSC 52 callback.
6. Tests, including the scrolled-screen one (§4.2).

## 10. Known limits

- Alternate screen has no scrollback, so local selection reaches only what is on screen.
- The engine's C API is versioned `0.1.0-dev`; every new call stays inside
  `ldmux_terminal.c` so a `GHOSTTY_COMMIT` bump stays a contained change.
- `TerminalFrame.kt`'s header comment duplicates a `reserved[7]` fragment and does not
  match the 40-byte layout in `ldmux_terminal.h`. The decoder is correct; the comment
  is wrong.
