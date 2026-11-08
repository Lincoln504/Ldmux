package io.ldmux.mobile

import io.ldmux.mobile.term.TerminalFrame

/**
 * How a snapshot from the daemon is applied to the engine the client already holds.
 *
 * **Scroll fidelity is the product (SPEC §7.4).** Every snapshot used to rebuild the
 * engine except a resize, and a rebuilt engine starts parked at its bottom with no
 * scrollback at all. A user reading history who lost the link for a second — or
 * whose pane the daemon resynced — came back to the live bottom with the history
 * gone. Only explicit user intent may change the view, and a reconnect is not intent.
 */
enum class SnapshotApply {
    /**
     * A fresh engine: clean parser state, viewport at the bottom, no scrollback.
     * Right when nobody is reading history, because a snapshot also means "the
     * parser state may be wrong" and only a fresh engine is certain to fix that.
     */
    Rebuild,

    /**
     * Replay the screen into the engine that is already here (resizing it first when
     * the grid changed). The scrollback, the viewport anchor and the selection survive.
     */
    ReplayInPlace,
}

/**
 * Decide how to apply a snapshot.
 *
 * In order:
 *
 * - **No frame yet** — a rebuild; there is nothing to preserve.
 * - **A history page-in reply** — a rebuild, deliberately: it *replaces* the scrollback
 *   with a deeper one. This is checked **before** the grid, because an in-place replay
 *   only re-establishes the visible screen; the history rows above it would be
 *   dropped, leaving the user at the top of the same shallow scrollback with no way to
 *   ask again. The reply is the deepest thing a snapshot carries, so it wins.
 * - **The screen changed** (primary <-> alternate since the last frame) — a rebuild:
 *   `?1049` cannot be sent into an engine that already holds both screens, because it
 *   switches them (see [snapshotBytes]).
 * - **The grid changed** — in place, the existing resize path.
 * - **The viewport is anchored in history** — in place. The user is reading; a
 *   reconnect, a server resync or the stale-prediction check must not move them.
 * - **Otherwise** (the viewport follows live output) — a rebuild for the clean state.
 *
 * "Anchored" is the emulator's own answer (`viewportActive == false`), read from the
 * last rendered frame: the one source of truth for where the view is.
 */
fun planSnapshot(
    current: TerminalFrame.Frame?,
    cols: Int,
    rows: Int,
    alternateOn: Boolean,
    history: Boolean,
): SnapshotApply {
    if (current == null) return SnapshotApply.Rebuild
    if (history) return SnapshotApply.Rebuild
    if (current.alternateOn != alternateOn) return SnapshotApply.Rebuild
    if (current.cols != cols || current.rows != rows) return SnapshotApply.ReplayInPlace
    if (!current.viewportActive) return SnapshotApply.ReplayInPlace
    return SnapshotApply.Rebuild
}

/**
 * Rows above the live bottom that a history page-in must leave the viewport on.
 *
 * **A page-in only prepends rows.** Everything the client already held keeps its
 * distance from the live bottom and gains older rows above it, so the bottom is the
 * one anchor that survives the round trip — and it is exact, because output that
 * arrived while the page was on its way appends *below* the viewport rather than
 * moving it.
 *
 * [owed] is the part of the gesture the viewport refused to move: rows the user asked
 * for beyond what was loaded. The page that arrives is where they were going, so the
 * owed rows continue there instead of being dropped.
 *
 * **The top is not an anchor, and treating it as one is a bug that shipped.** A freshly
 * attached client holds a single screenful, where offset 0 is both the oldest row and
 * the live bottom; the first swipe therefore asked for a page, and the reply parked
 * the viewport on row 0 of it — with a 4,000-row answer, 4,000 rows above the prompt
 * the finger was on. Measured 2026-11-08 on a phone against a tailnet shell.
 */
internal fun historyRowsAboveBottom(frame: TerminalFrame.Frame, owed: Int): Int {
    val below = (frame.scrollTotal - frame.scrollLen - frame.scrollOffset).coerceAtLeast(0L)
    return (below + owed.coerceAtLeast(0)).coerceIn(0L, Int.MAX_VALUE.toLong()).toInt()
}

/**
 * The rows of a gesture the viewport refused to move.
 *
 * [moved] is what the emulator's scrollbar actually did — negative upwards, as the
 * controller's scroll reads it before and after — and [rows] is what the gesture asked
 * for. Only the shortfall is owed to the page-in: the part that did move is already
 * counted in the distance from the bottom, so owing it as well would scroll the page that
 * arrives further than the finger travelled.
 */
internal fun historyOwed(rows: Int, moved: Int): Int = (moved - rows).coerceAtLeast(0)
