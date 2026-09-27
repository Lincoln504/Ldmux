package dev.ldmux.mobile

import dev.ldmux.mobile.term.TerminalFrame
import kotlin.math.abs

/**
 * What a tap on the terminal should do.
 *
 * This is pure so it can be tested without a device, because the rules here are
 * the product: a wrong verdict is a cursor that jumps somewhere strange, or —
 * as happened on device — a shell printing `^[[<0;42;8M` because we wrote a
 * mouse click into a program that had not asked for one.
 */
sealed interface TapAction {
    /** Deliver a synthesized SGR click; only the server can actually do it. */
    data class Click(val col: Int, val row: Int) : TapAction

    /** Send these named keys, in order, to move the cursor. */
    data class Keys(val keys: List<String>) : TapAction

    /** Nothing sensible to do (reading history, at a password prompt, same cell). */
    data object Ignore : TapAction
}

/**
 * Decide what a tap at cell ([col], [row]) means.
 *
 * There is no "set cursor here" in a terminal, so this is built from what a
 * terminal actually has. The choice is not made by recognising the application —
 * there is no app blocklist and no setting — it is made from the shape of the
 * input surface, which is the thing that actually differs:
 *
 *  - **A program that accepts pointer events** (`vim` with SGR mouse, a TUI with
 *    mouse reporting) gets a real click and its own click-to-position logic runs.
 *    Exact, one round trip, and the program decides the meaning.
 *  - **A text prompt that wraps** (a shell, and the composers in `pi`, Claude
 *    Code and Codex) is one continuous line, so the screen is treated as a strip:
 *    a tap on a wrapped segment lands where it looks like it should.
 *  - **A full-screen program that did not ask for the mouse** (`vim`, `less`,
 *    `htop`, `nano`) is a grid: it has its own cursor with its own idea of rows.
 *    Measured on this box, 40 `Right` keys in vim moved the cursor 5 columns and
 *    stopped at the end of the line — `l` does not wrap — while `Down` moved
 *    whole lines. So the row delta must be sent as Up/Down and the column delta
 *    as Left/Right, and then a tap lands on the cell that was tapped.
 *
 * [mouseRefused] is the server having already said this pane will not take a
 * click. It matters because the mouse mode is **sticky**: a program that enables
 * reporting and then exits leaves the mode enabled, so the frame still says
 * "mouse" while a plain shell is what would receive the bytes — and a shell
 * prints them. The first tap teaches us that; every tap after it skips the round
 * trip.
 */
fun planTap(
    col: Int,
    row: Int,
    frame: TerminalFrame.Frame,
    mouseRefused: Boolean = false,
): TapAction {
    // Not while reading history (there is no live cursor up there), and never at
    // a password prompt.
    if (!frame.viewportActive || frame.passwordInput) return TapAction.Ignore
    if (col < 0 || row < 0 || col >= frame.cols || row >= frame.rows) return TapAction.Ignore

    if (frame.mouseTracking && frame.mouseSgr && !mouseRefused) return TapAction.Click(col, row)

    if (col == frame.cursorX && row == frame.cursorY) return TapAction.Ignore

    // A grid: rows are rows. Up/Down first, so the horizontal part clamps on the
    // line that was actually tapped rather than on the one we started from.
    if (frame.alternateOn) {
        // The delta below is measured FROM the cursor, so what it needs is that the
        // cursor's position is *real* — not that the program wants one drawn. Those
        // are two different facts in two different bytes, and this asked the wrong
        // one: `pi` hides the hardware cursor, so every tap in it was refused even
        // though the emulator knew the input position exactly.
        if (!frame.cursorKnown) return TapAction.Ignore
        val rows = row - frame.cursorY
        val cols = col - frame.cursorX
        val keys = ArrayList<String>(abs(rows) + abs(cols))
        repeat(abs(rows)) { keys.add(if (rows > 0) "Down" else "Up") }
        repeat(abs(cols)) { keys.add(if (cols > 0) "Right" else "Left") }
        return if (keys.isEmpty()) TapAction.Ignore else TapAction.Keys(keys.take(MAX_TAP_KEYS))
    }

    // The same question as the alternate-screen branch above, and the same answer:
    // whether the position is real, not whether a cursor was asked for.
    if (!frame.cursorKnown) return TapAction.Ignore

    // The screen is a continuous strip, and the strip arithmetic is exact for a
    // WRAPPED input line: an input spanning three rows is reached by walking the
    // strip, not by guessing which row the input starts on.
    //
    // There used to be a `Home` shortcut here for a tap more than one row above
    // the cursor — one key instead of up to 120, which is a real cost on a metered
    // link. It was removed because it is only correct when the input is a single
    // line, and it is wrong in exactly the case this app is for: the composers in
    // `pi`, Claude Code and Codex wrap, and a tap on the second row of a wrapped
    // command was being turned into "jump to the start" instead of "go to the cell
    // that was tapped". A shortcut that silently lands somewhere else is worse than
    // the bytes it saves.
    //
    // Nothing is lost by relying on the strip: readline clamps Left at the line
    // start, so on a single-line input this still lands at the beginning — it just
    // is not free. The bound below is what keeps the cost finite, and the honest
    // fix if that cost ever proves too high on a real link is a server-side
    // "put the cursor on this cell" primitive, not another client-side heuristic.
    val delta = (row - frame.cursorY) * frame.cols + (col - frame.cursorX)
    if (delta == 0) return TapAction.Ignore
    val key = if (delta < 0) "Left" else "Right"
    val count = minOf(abs(delta), MAX_TAP_KEYS)
    return TapAction.Keys(List(count) { key })
}

/**
 * Upper bound on the keys one tap may generate, so a stray tap at the edge of a
 * wide pane cannot become a key storm.
 */
const val MAX_TAP_KEYS = 120

/**
 * The wire cap: the daemon accepts at most 32 key names per message, so a long
 * move becomes a couple of frames rather than one per character.
 */
const val KEYS_PER_MESSAGE = 32

/** Split a key list into message-sized batches. */
fun batchKeys(keys: List<String>): List<List<String>> {
    if (keys.isEmpty()) return emptyList()
    val out = ArrayList<List<String>>(keys.size / KEYS_PER_MESSAGE + 1)
    var i = 0
    while (i < keys.size) {
        val end = minOf(i + KEYS_PER_MESSAGE, keys.size)
        out.add(keys.subList(i, end))
        i = end
    }
    return out
}