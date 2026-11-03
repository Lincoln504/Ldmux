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
     * - at most half a screen moves (a full replacement is a repaint, not a scroll),
     * - the overlap must match on at least half its rows, counting only non-blank ones, so two
     *   mostly-empty screens cannot agree by accident,
     * - a screen that does not match any shift adds nothing.
     */
    private fun noteShift(previous: MeshScreen, next: MeshScreen) {
        if (previous.alternate || next.alternate) return
        val height = previous.lines.size
        if (height == 0 || next.lines.size != height) return
        var best = 0
        var bestScore = 0
        for (moved in 1..(height / 2)) {
            val overlap = height - moved
            var matched = 0
            var nonBlank = 0
            for (i in 0 until overlap) {
                val p = previous.lines[i + moved]
                if (p.isNotBlank()) nonBlank++
                if (p == next.lines[i]) matched++
            }
            // Half the overlap, and at least one real row inside it: a screen of blanks matches
            // every shift, and a shift decided from blanks is a guess.
            if (nonBlank > 0 && matched * 2 >= overlap && matched > bestScore) {
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
