package io.ldmux.mobile

import io.ldmux.mobile.term.TerminalFrame
import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * Where a history page-in leaves the viewport.
 *
 * The bug this pins, as the user reported it on 2026-11-08: "in tailnet mode, on a new
 * shell, when I start scrolling it jumps to the top of scrollback instead of staying at
 * the bottom". A freshly attached client holds one screenful, so its scroll offset 0 is
 * the oldest row *and* the live bottom at once; the first upward gesture asked for a
 * page, and the reply restored the viewport to "the top" — row 0 of a 4,000-row answer,
 * four thousand rows above the prompt the finger was on.
 *
 * The rule that replaces it: a page-in only prepends rows, so the anchor that survives
 * the round trip is the distance from the **bottom**, and the part of the gesture the
 * viewport refused is owed to the page that arrives.
 */
class HistoryRestoreTest {
    /** A frame whose scrollable area is `total` rows with a `len`-row viewport at `offset`. */
    private fun frame(total: Long, len: Long, offset: Long, viewportActive: Boolean) = TerminalFrame.Frame(
        cols = 80, rows = len.toInt(), cursorX = 0, cursorY = 0, cursorVisible = true,
        cursorKnown = true, cursorStyle = 1, defaultFg = 0, defaultBg = 0, passwordInput = false,
        viewportActive = viewportActive, mouseTracking = false, mouseSgr = false,
        alternateOn = false, selection = null,
        scrollTotal = total, scrollOffset = offset, scrollLen = len,
        cells = emptyList(),
    )

    @Test
    fun `the first swipe on a fresh shell lands one step above the live bottom`() {
        // What attach leaves behind: one screenful, so total == len and offset 0 is both
        // ends. The keeper's answer is thousands of rows deep.
        val fresh = frame(total = 24, len = 24, offset = 0, viewportActive = true)
        val swipe = 5
        val landing = historyRowsAboveBottom(fresh, owed = swipe)
        assertEquals(5, landing)

        // As rows of the page that answers: 4,024 rows with the same 24-row viewport, so
        // the landing is row 3,995 — five above the live bottom. The old policy restored
        // "the top", which is row 0 of that page: 4,000 rows above the prompt the finger
        // was on, for a swipe of five.
        val pageTotal = 4_024L
        val pageLen = 24L
        val landingRow = pageTotal - pageLen - landing
        assertEquals(3_995L, landingRow, "five rows above the live bottom, not the oldest row")
        // The jump, measured: the old policy's landing was row 0, which is 3,995 rows
        // above where this one puts the same gesture.
        assertEquals(3_995L, landingRow - 0L)
    }

    @Test
    fun `reading history keeps the rows being read, not the top of the page`() {
        // 5,000 rows loaded, the viewport at the very top: 4,976 rows sit below it, and a
        // page-in prepends above, so that distance is where the reader stays.
        val reading = frame(total = 5_000, len = 24, offset = 0, viewportActive = false)
        assertEquals(4_976, historyRowsAboveBottom(reading, owed = 0))
    }

    @Test
    fun `a page tap that had to ask lands a page above the bottom`() {
        // 30 rows loaded, the viewport 3 above the bottom, and a 19-row page asked for:
        // the tap was aimed 22 rows above the bottom, and that is where it lands.
        val nearBottom = frame(total = 30, len = 24, offset = 3, viewportActive = false)
        assertEquals(3, historyRowsAboveBottom(nearBottom, owed = 0))
        assertEquals(22, historyRowsAboveBottom(nearBottom, owed = 19))
    }

    @Test
    fun `only the part of a gesture the viewport refused is owed`() {
        // A 19-row page asked for from 3 rows above the bottom moved 3 and was refused the
        // rest; 16 is what the page owes. The 3 that moved are already in the distance.
        assertEquals(16, historyOwed(rows = -19, moved = -3))
        // A gesture that moved exactly what it asked for owes nothing.
        assertEquals(0, historyOwed(rows = -5, moved = -5))
        // A viewport with no history at all refuses the whole gesture.
        assertEquals(5, historyOwed(rows = -5, moved = 0))
    }

    @Test
    fun `nonsense scroll figures stay in range`() {
        // A frame that claims to be scrolled past its own bottom (impossible, but the
        // arithmetic must not go negative) lands at the bottom, not at a negative row.
        assertEquals(0, historyRowsAboveBottom(frame(total = 10, len = 24, offset = 40, viewportActive = true), owed = 0))
        // A page deeper than an Int is clamped rather than wrapped.
        assertEquals(
            Int.MAX_VALUE,
            historyRowsAboveBottom(frame(total = Long.MAX_VALUE / 2, len = 1, offset = 0, viewportActive = true), owed = 0),
        )
    }
}
