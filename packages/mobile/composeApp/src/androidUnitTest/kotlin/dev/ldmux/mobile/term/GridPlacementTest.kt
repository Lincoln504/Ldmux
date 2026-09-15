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
    fun `a grid that does not fill the viewport is centred`() {
        // 10 cols x 10px = 100 wide in a 120 viewport: 10px margin each side.
        // 5 rows x 20px = 100 tall in a 140 viewport: 20px margin each side.
        val p = place(cols = 10, rows = 5, vw = 120f, vh = 140f)
        assertEquals(10f, p.dx)
        assertEquals(20f, p.dy)
    }

    @Test
    fun `an oversized grid is clipped, not squashed, and pinned to the bottom`() {
        // 20 rows x 20px = 400 tall in a 300 viewport. Nothing is scaled: the
        // cells stay 20px and the overflow goes off the *top*, so the bottom row
        // (the prompt) stays visible.
        val p = place(cols = 10, rows = 20, vw = 120f, vh = 300f)
        assertEquals(10f, p.dx, "still centred, because it is narrow enough")
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
    fun `a tap in the margin beside the grid is not a cell`() {
        val p = place(cols = 10, rows = 5, vw = 120f, vh = 140f)
        assertNull(p.cellAt(0f, 0f), "left of the 10px margin")
        assertNull(p.cellAt(119f, 139f), "right margin is 10px wide")
        assertNull(p.cellAt(-1f, 70f))
        assertNull(p.cellAt(60f, -1f))
    }

    @Test
    fun `a tap above a clipped grid maps to the row actually drawn there`() {
        // 20 rows in a 300px viewport, so the top 5 rows are off-screen.
        val p = place(cols = 10, rows = 20, vw = 120f, vh = 300f)
        // The top edge of the viewport is inside row 5 (rows -100..0 are rows 0-4).
        assertEquals(5 * 10 + 0, p.cellAt(10f, 0f))
        // Above the grid's own top edge (dy = -100) there is no cell at all.
        assertNull(p.cellAt(10f, -100.5f))
        // The bottom of the viewport is the bottom of row 19.
        assertEquals(19 * 10 + 9, p.cellAt(105f, 299f))
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