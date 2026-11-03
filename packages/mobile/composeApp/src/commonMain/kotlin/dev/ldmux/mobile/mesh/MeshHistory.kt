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
        if (previous.firstLine == 0 || next.firstLine == 0) return
        if (next.firstLine <= previous.firstLine) return
        val moved = (next.firstLine - previous.firstLine).toInt().coerceAtMost(previous.lines.size)
        for (i in 0 until moved) rows.addLast(previous.lines[i])
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
