package dev.ldmux.mobile.term

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The render transform and the touch hit-test are the same geometry seen from
 * two sides. If they drift, tapping puts the cursor somewhere other than where
 * the user pointed — the exact bug this test exists to prevent, because it is
 * invisible in code review and obvious only on a device.
 */
class GridPlacementTest {

    private fun place(
        cols: Int,
        rows: Int,
        vw: Float,
        vh: Float,
        cell: Float = 10f,
    ) = placeGrid(cell, cell * 2f, cols, rows, vw, vh)

    @Test
    fun `a grid that does not fill the viewport is flush to the top-left`() {
        // Centring split the leftover into a band on *both* edges — a measurable
        // black strip beside the first column and above the first row — for a
        // leftover that is under one cell. A terminal's text starts where its box
        // starts. The leftover goes to the right and the bottom, which is where
        // nothing is reading.
        val p = place(cols = 10, rows = 5, vw = 120f, vh = 140f)
        assertEquals(0f, p.dx, "no band on the left")
        assertEquals(0f, p.dy, "no band on the top")
        assertEquals(100f, p.gridWidth, "the leftover is on the right")
        assertEquals(100f, p.gridHeight, "the leftover is on the bottom")
    }

    @Test
    fun `an oversized grid is clipped, not squashed, and pinned to the bottom`() {
        // 20 rows x 20px = 400 tall in a 300 viewport. Nothing is scaled: the
        // cells stay 20px and the overflow goes off the *top*, so the bottom row
        // (the prompt) stays visible.
        val p = place(cols = 10, rows = 20, vw = 120f, vh = 300f)
        assertEquals(0f, p.dx, "flush left, and narrow enough not to clip")
        assertEquals(-100f, p.dy, "the overflow goes off the top")
        assertEquals(20f, p.cellHeight, "cells are never resampled to fit")
    }

    @Test
    fun `a point at the origin of a cell maps back to that cell`() {
        val p = place(cols = 10, rows = 5, vw = 120f, vh = 140f)
        for (col in 0 until 10) {
            for (row in 0 until 5) {
                val (x, y) = p.cellOrigin(col, row)
                assertEquals(row * 10 + col, p.cellAt(x, y), "cell $col,$row")
                // Anywhere inside the cell, not just its exact corner.
                assertEquals(row * 10 + col, p.cellAt(x + 9f, y + 19f), "inside $col,$row")
            }
        }
    }

    @Test
    fun `the last pixel of a cell still belongs to it`() {
        val p = place(cols = 10, rows = 5, vw = 120f, vh = 140f)
        val (x, y) = p.cellOrigin(3, 2)
        assertEquals(23, p.cellAt(x + 9.99f, y + 19.99f))
    }

    @Test
    fun `a tap in the leftover beside the grid is not a cell`() {
        // Flush to the top-left means the only leftover is on the right and the
        // bottom. Tapping it must not invent a cell: the cursor would land in a
        // column the shell does not have.
        val p = place(cols = 10, rows = 5, vw = 120f, vh = 140f)
        assertNull(p.cellAt(119f, 139f), "right of the last column")
        assertNull(p.cellAt(5f, 139f), "below the last row")
        assertNull(p.cellAt(-1f, 70f), "before the first column")
        assertNull(p.cellAt(60f, -1f), "above the first row")
        assertEquals(0, p.cellAt(0f, 0f), "the corner *is* the first cell")
    }

    @Test
    fun `a tap above a clipped grid maps to the row actually drawn there`() {
        // 20 rows in a 300px viewport, so the top 5 rows are off-screen.
        val p = place(cols = 10, rows = 20, vw = 120f, vh = 300f)
        // The top edge of the viewport is inside row 5 (rows -100..0 are rows 0-4).
        assertEquals(5 * 10 + 0, p.cellAt(5f, 0f))
        // Above the grid's own top edge (dy = -100) there is no cell at all.
        assertNull(p.cellAt(5f, -100.5f))
        // The bottom of the viewport is the bottom of row 19.
        assertEquals(19 * 10 + 9, p.cellAt(95f, 299f))
    }

    @Test
    fun `a degenerate viewport has no cells`() {
        val p = place(cols = 0, rows = 0, vw = 0f, vh = 0f)
        assertNull(p.cellAt(0f, 0f))
        assertNull(placeGrid(0f, 0f, 10, 10, 100f, 100f).cellAt(5f, 5f))
    }

    @Test
    fun `the grid fills the viewport once the resize lands`() {
        // This is the invariant the whole design rests on: after the client has
        // told the daemon the grid it can paint, what is left over is less than
        // one cell, so nothing has to be scaled.
        for ((vw, vh) in listOf(1080f to 1920f, 2340f to 1080f, 720f to 1280f)) {
            val cell = 12f
            val cols = (vw / cell).toInt()
            val rows = (vh / (cell * 2f)).toInt()
            val p = placeGrid(cell, cell * 2f, cols, rows, vw, vh)
            assertTrue(p.gridWidth <= vw && p.gridHeight <= vh, "grid must fit for $vw x $vh")
            assertEquals(1f, p.cellWidth / cell)
        }
    }
}