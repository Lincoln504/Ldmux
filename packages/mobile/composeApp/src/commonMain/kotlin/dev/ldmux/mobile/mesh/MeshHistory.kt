package dev.ldmux.mobile.mesh

/**
 * The mesh terminal's own scrollback.
 *
 * The link carries one screen and no history, so this is built on the client from the row
 * identity the host does send: each screen names the absolute line at its top (`firstLine`), and
 * the difference between two of those numbers is exactly the rows that left the top. Nothing is
 * guessed from timing, nothing is invented when the host cannot number a screen, and the buffer
 * is bounded.
 *
 * The view window is a position above the live bottom, not a copy: while the user reads history,
 * new output still accumulates (so scrolling back down shows it) and does not drag the view away.
 *
 * Pure, so the arithmetic is testable without an engine or a radio.
 */
internal class MeshHistory(private val max: Int = 2_000) {

    private companion object {
        /**
         * The fewest real rows an inferred shift may rest on. Two is the smallest overlap that is
         * evidence rather than coincidence; with one row in common, a screen of blanks would agree
         * with every shift, and a guess from blanks is how history acquires rows that never
         * existed. A shift of "all but one row" leaves one row of overlap and is refused by the
         * same rule, which is the honest answer: one row cannot say where the rest went.
         */
        const val MIN_MATCHED = 2
    }

    private val rows = ArrayDeque<String>()

    /** Rows above the live bottom the view is parked on. 0 is live. */
    var offset = 0
        private set

    /** How many rows of history exist. The furthest the view can go up. */
    val size: Int get() = rows.size

    /**
     * Take the rows that left between [previous] and [next].
     *
     * A reflow (the grid changed) and a jump backwards are not scrolls: both can move `firstLine`
     * in ways the rows do not explain, so neither adds anything.
     */
    fun note(previous: MeshScreen?, next: MeshScreen) {
        if (previous == null) return
        if (previous.firstLine == 0 || next.firstLine == 0) {
            noteShift(previous, next)
            return
        }
        if (next.firstLine <= previous.firstLine) return
        val moved = (next.firstLine - previous.firstLine).toInt().coerceAtMost(previous.lines.size)
        add(previous, moved)
    }

    /**
     * The fallback for a host that cannot number its lines.
     *
     * A keeper older than the line-numbering change reports `totalRows = 0`, so `firstLine` is 0
     * on every screen and the exact rule above refuses everything — the pad then draws and does
     * nothing, which on a device reads as a broken button (measured 2026-11-03: the running
     * keeper predated the numbering commit). The screen itself still shows a scroll happened: the
     * rows below the top move up by exactly the number that left. So infer that shift, and only
     * that shift.
     *
     * Deliberately conservative, because inventing history is worse than having none:
     * - only when neither screen is on the alternate screen and both have the same height,
     * - at least [MIN_MATCHED] non-blank rows must sit in the overlap, so a screen of blanks
     *   cannot agree with every shift, and a shift decided from blanks is a guess,
     * - **every** non-blank row in the overlap must be the same row in the new screen, except at
     *   most one: the row a program is still writing (a spinner, a progress bar) is a row that
     *   legitimately changed without scrolling,
     * - a screen that does not match any shift adds nothing.
     *
     * **Why not "at most half a screen".** That was the first rule, and it refuses exactly the
     * scrolls that matter most: a build log, a `cat`, or anything that fills a screenful between
     * two snapshots moves the whole screen at once. Measured against the alternative — keep
     * nothing above the live screen — a rule that accepts a near-full-screen shift and refuses a
     * replacement is the honest middle: the overlap still has to match row for row.
     *
     * A screen that scrolls by more than a screen's worth, or that is replaced outright (a new
     * program's first screen), has no overlap and therefore adds nothing: a repaint is not a
     * scroll, and guessing one is how history acquires rows that never existed.
     */
    private fun noteShift(previous: MeshScreen, next: MeshScreen) {
        if (previous.alternate || next.alternate) return
        val height = previous.lines.size
        if (height < 2 || next.lines.size != height) return
        var best = 0
        var bestScore = 0
        for (moved in 1 until height) {
            val overlap = height - moved
            var matched = 0
            var nonBlank = 0
            for (i in 0 until overlap) {
                val p = previous.lines[i + moved]
                if (p.isBlank()) continue
                nonBlank++
                if (p == next.lines[i]) matched++
            }
            // At most one changed row: the row a program is still writing. Everything else in the
            // overlap must be the same row, or this shift is a guess about which rows left.
            if (nonBlank < MIN_MATCHED || matched < nonBlank - 1) continue
            // **The score falls as the shift grows** — a bigger shift leaves fewer shared rows —
            // so repeated content (a log of identical lines) yields the smallest shift, which is
            // the safe reading. A tie happens when the row just above the new bottom changed: the
            // smaller shift counts one more row as "shared" by allowing that one mismatch, and the
            // larger one explains the same rows without it. The larger shift is the honest answer,
            // because it does not need the excuse.
            if (matched >= bestScore) {
                best = moved
                bestScore = matched
            }
        }
        if (best == 0) return
        add(previous, best)
    }

    private fun add(previous: MeshScreen, moved: Int) {
        for (i in 0 until moved.coerceAtMost(previous.lines.size)) rows.addLast(previous.lines[i])
        while (rows.size > max) rows.removeFirst()
        // The view can never be further up than there is history.
        if (offset > rows.size) offset = rows.size
    }

    /**
     * Move the view by [delta] rows (negative is up) and answer with what it actually moved, in
     * the same sign as the request: -1 means it went one row up.
     */
    fun scroll(delta: Int): Int {
        val before = offset
        // The offset counts rows *above* the live bottom, so moving up is a bigger offset.
        offset = (offset - delta).coerceIn(0, rows.size)
        return before - offset
    }

    fun toBottom() {
        offset = 0
    }

    fun clear() {
        rows.clear()
        offset = 0
    }

    /**
     * The [screen]'s [screenRows] rows as the view should show them: the live screen while the
     * offset is zero, and otherwise a window that ends [offset] rows above the live bottom.
     */
    fun window(screen: MeshScreen, screenRows: Int): List<String> {
        if (offset == 0) return screen.lines
        val full: List<String> = rows.toList() + screen.lines
        val end = (full.size - offset).coerceIn(screenRows.coerceAtMost(full.size), full.size)
        val start = (end - screenRows).coerceAtLeast(0)
        return full.subList(start, end)
    }
}
