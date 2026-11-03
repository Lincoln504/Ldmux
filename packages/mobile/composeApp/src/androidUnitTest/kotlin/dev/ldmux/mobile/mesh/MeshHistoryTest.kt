package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals

/** The mesh view's own scrollback: row identity in, a window out. */
class MeshHistoryTest {

    private fun screen(first: Int, vararg lines: String) = MeshScreen(
        lines = lines.toList(),
        cursorX = 0,
        cursorY = lines.lastIndex,
        alternate = false,
        dead = false,
        epoch = 0,
        cols = 80,
        firstLine = first,
    )

    @Test
    fun `the rows a scroll pushed off are the history, oldest first`() {
        val h = MeshHistory()
        h.note(null, screen(1, "a", "b"))
        h.note(screen(1, "a", "b"), screen(2, "b", "c"))
        h.note(screen(2, "b", "c"), screen(4, "d", "e"))
        // Three rows left the top: `a`, then `b` and `c`.
        assertEquals(3, h.size)
        // All the way up: the window shows the oldest screen, and the live one is below it.
        h.scroll(-3)
        assertEquals(listOf("a", "b"), h.window(screen(4, "d", "e"), 2))
        // One row up: the last history row, then the live screen without its top.
        h.toBottom()
        h.scroll(-1)
        assertEquals(listOf("c", "d"), h.window(screen(4, "d", "e"), 2))
    }

    @Test
    fun `the window ends at the live bottom and moves by whole rows`() {
        val h = MeshHistory()
        h.note(null, screen(1, "a", "b", "c"))
        h.note(screen(1, "a", "b", "c"), screen(2, "b", "c", "d"))
        val live = screen(2, "b", "c", "d")
        assertEquals(listOf("b", "c", "d"), h.window(live, 3))
        assertEquals(-1, h.scroll(-1), "one row up")
        assertEquals(listOf("a", "b", "c"), h.window(live, 3))
        assertEquals(1, h.scroll(1), "one row back down")
        assertEquals(listOf("b", "c", "d"), h.window(live, 3))
    }

    @Test
    fun `the view cannot scroll past the history or the bottom`() {
        val h = MeshHistory()
        h.note(null, screen(1, "a", "b"))
        h.note(screen(1, "a", "b"), screen(2, "b", "c"))
        assertEquals(-1, h.scroll(-50), "clamped to the one row of history")
        assertEquals(0, h.scroll(-1), "nothing above the oldest row")
        assertEquals(1, h.scroll(50))
        assertEquals(0, h.offset)
    }

    @Test
    fun `a reflow or a jump backwards adds nothing`() {
        val h = MeshHistory()
        h.note(null, screen(10, "a", "b"))
        h.note(screen(10, "a", "b"), screen(10, "a", "b"))
        h.note(screen(10, "a", "b"), screen(2, "x", "y"))
        assertEquals(0, h.size)
    }

    @Test
    fun `an unnumbered screen accumulates nothing rather than guessing`() {
        val h = MeshHistory()
        h.note(null, screen(0, "a"))
        h.note(screen(0, "a"), screen(0, "b"))
        assertEquals(0, h.size)
    }

    @Test
    fun `the buffer is bounded and keeps the newest rows`() {
        val h = MeshHistory(max = 3)
        h.note(null, screen(1, "a", "b", "c", "d"))
        h.note(screen(1, "a", "b", "c", "d"), screen(5, "e", "f"))
        h.note(screen(5, "e", "f"), screen(7, "g", "h"))
        assertEquals(3, h.size)
        h.scroll(-3)
        // Only the newest three rows are kept: `d`, `e`, `f`.
        assertEquals(listOf("d", "e"), h.window(screen(7, "g", "h"), 2))
    }
}

/**
 * A host that cannot number its lines still scrolls, and the view still has history.
 *
 * The running keeper on the test host predates line numbering and reports `totalRows = 0`, so
 * every screen arrives with `firstLine = 0`. Refusing those left the pad drawing and doing
 * nothing; the fallback reads the shift the screen itself shows.
 */
class MeshHistoryShiftTest {

    private fun screen(vararg lines: String) = MeshScreen(
        lines = lines.toList(),
        cursorX = 0,
        cursorY = lines.lastIndex,
        alternate = false,
        dead = false,
        epoch = 0,
        cols = 80,
        firstLine = 0,
    )

    @Test
    fun `a shifted screen gives up the rows that left the top`() {
        val h = MeshHistory()
        val a = screen("one", "two", "three", "four")
        val b = screen("two", "three", "four", "five")
        h.note(a, b)
        assertEquals(1, h.size, "the top row left")
        h.scroll(-1)
        // One row above live: the row that left, then the live screen minus its last row.
        assertEquals(listOf("one", "two", "three", "four"), h.window(b, 4))
    }

    @Test
    fun `two rows leaving is two rows of history`() {
        val h = MeshHistory()
        h.note(
            screen("one", "two", "three", "four", "five", "six"),
            screen("three", "four", "five", "six", "seven", "eight"),
        )
        assertEquals(2, h.size)
    }

    @Test
    fun `a repaint that shares nothing is not a scroll`() {
        val h = MeshHistory()
        h.note(
            screen("alpha", "beta", "gamma", "delta"),
            screen("one", "two", "three", "four"),
        )
        assertEquals(0, h.size)
    }

    @Test
    fun `blank screens cannot agree by accident`() {
        val h = MeshHistory()
        h.note(screen("", "", "", ""), screen("", "", "", ""))
        assertEquals(0, h.size)
    }

    @Test
    fun `the alternate screen never contributes history`() {
        val h = MeshHistory()
        val alt = screen("one", "two", "three", "four").copy(alternate = true)
        val next = screen("two", "three", "four", "five").copy(alternate = true)
        h.note(alt, next)
        assertEquals(0, h.size)
    }
}
