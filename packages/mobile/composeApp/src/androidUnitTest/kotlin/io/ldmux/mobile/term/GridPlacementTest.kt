package io.ldmux.mobile.term

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
        cursorRow: Int = rows - 1,
    ) = placeGrid(cell, cell * 2f, cols, rows, vw, vh, cursorRow)

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
        assertNull(placeGrid(0f, 0f, 10, 10, 100f, 100f, 0).cellAt(5f, 5f))
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
            val p = placeGrid(cell, cell * 2f, cols, rows, vw, vh, rows - 1)
            assertTrue(p.gridWidth <= vw && p.gridHeight <= vh, "grid must fit for $vw x $vh")
            // Flush to the top-left, which is the rule and not centring: the leftover is
            // under one cell, and splitting it in two puts a black band on *both* edges
            // instead of one and moves the text off the corner it starts at.
            //
            // This line used to read `assertEquals(1f, p.cellWidth / cell)` — a fact the
            // assertion above already implied, because `gridWidth` is `cellWidth * cols`
            // and the grid was built from the cell just handed in. It could never be the
            // thing that failed.
            assertEquals(0f, p.dx, "the grid must start at the left edge for $vw x $vh")
            assertEquals(0f, p.dy, "the grid must start at the top edge for $vw x $vh")
        }
    }

    @Test
    fun `a grid larger than the viewport is pinned to the bottom, not the top`() {
        // The overflow case is a font change or a rotation whose resize has not landed
        // yet, and the row you cannot afford to lose is the one with the cursor in it.
        // Pinning to the top would scroll the prompt off the bottom of the screen.
        val p = placeGrid(10f, 20f, 200, 100, 100f, 100f, 99)
        assertEquals(100f - 20f * 100f, p.dy)
        assertEquals(0f, p.dx, "overflow is still flush to the left; only the vertical moves")
        // The last row's bottom edge is the bottom of the viewport, so the prompt is on
        // screen, and the first row is off the top where it can be scrolled back to.
        assertEquals(100f, p.dy + p.gridHeight)
        assertTrue(p.dy < 0f)
    }

    // ── dragging past an edge ───────────────────────────────────────────────
    //
    // A selection reaches into the scrollback by dragging past the top of the
    // grid. Three facts decide whether that works, and all three were wrong or
    // missing: which cell a finger outside the grid means, which way the viewport
    // should move, and where a text-anchored endpoint ends up when it does.

    private fun grid() = placeGrid(8f, 16f, 80, 24, 640f, 384f, 23)

    // ── the cursor's row is the one that cannot be lost ─────────────────────
    //
    // A grid taller than the viewport is clipped rather than scaled, and the anchor used to be the
    // bottom and nothing else. That is right for a shell prompt, which sits on the last row, and
    // wrong for everything else: a full-screen program puts the cursor wherever it likes, and a
    // 46-row pane drawn in a 19-row viewport with the bottom anchor shows rows 27..45 — a cursor on
    // row 5 is not on the screen at all.

    @Test
    fun `a clipped grid keeps the cursor row and the prompt row on screen`() {
        // The reported case: a 46-row pane, a 19-row viewport, the cursor on the last row.
        val p = placeGrid(10f, 20f, 46, 46, 460f, 19 * 20f, cursorRow = 45)
        val firstVisible = (-p.dy / 20f).toInt()
        assertEquals(27, firstVisible, "the bottom anchor, so the prompt row is the last one drawn")
        assertTrue(p.dy + 45 * 20f >= 0f, "the cursor row starts inside the viewport")
        assertTrue(p.dy + 46 * 20f <= 19 * 20f, "the cursor row ends inside the viewport")
        assertEquals(
            45 - 18,
            firstVisible,
            "eighteen rows below the first visible one is the cursor's, which is the whole viewport",
        )
    }

    @Test
    fun `a cursor above the bottom anchor slides the window down to it`() {
        val p = placeGrid(10f, 20f, 46, 46, 460f, 19 * 20f, cursorRow = 5)
        // Row 5 becomes the first row drawn, so the window moved up six rows from the anchor and
        // the grid's own top edge (row 0) is now off the screen instead.
        assertEquals(-5 * 20f, p.dy, "row 5 is the first visible row")
        assertEquals(0f, p.dy + 5 * 20f, "and it starts at the top of the viewport")
        assertTrue(p.dy + 6 * 20f <= 19 * 20f, "the cursor row is wholly inside the viewport")
        assertEquals(5, (-p.dy / 20f).toInt())
    }

    @Test
    fun `a cursor inside the bottom anchor leaves the placement exactly as it was`() {
        // The prompt case and every full-screen program that keeps its cursor low: nothing moves,
        // so a program's own drawing is not shifted by the renderer.
        val anchored = placeGrid(10f, 20f, 46, 46, 460f, 19 * 20f, cursorRow = 45)
        for (row in 27..45) {
            val p = placeGrid(10f, 20f, 46, 46, 460f, 19 * 20f, cursorRow = row)
            assertEquals(anchored.dy, p.dy, "row $row is already inside the anchor")
        }
    }

    @Test
    fun `a cursor row outside the grid is clamped rather than trusting it`() {
        // A frame's cursor is host data. A stale one must not place the grid anywhere but inside
        // the two ends it can be: the top, and the bottom anchor.
        val low = placeGrid(10f, 20f, 46, 46, 460f, 19 * 20f, cursorRow = -3)
        val high = placeGrid(10f, 20f, 46, 46, 460f, 19 * 20f, cursorRow = 900)
        assertEquals(0f, low.dy, "clamped to row 0")
        assertEquals(19 * 20f - 46 * 20f, high.dy, "clamped to the last row, which is the anchor")
    }

    @Test
    fun `a viewport shorter than one row still shows the cursor's row`() {
        // Degenerate but reachable for a frame: the keyboard is up and the terminal has been
        // squeezed to less than a cell. One row visible, and it is the cursor's.
        val p = placeGrid(10f, 20f, 46, 46, 460f, 10f, cursorRow = 30)
        assertEquals(-30 * 20f + 10f - 20f, p.dy, "row 30's top is one cell above the viewport's top")
        assertTrue(p.dy + 30 * 20f <= 10f, "the cursor row's top is inside a viewport shorter than it")
    }

    @Test
    fun `a finger above the grid means the cell at the top edge`() {
        val p = grid()
        // Column comes from where the finger actually is, so dragging diagonally
        // out of the top-left corner still selects to column 1, not column 0.
        val idx = p.dragCell(p.dx + p.cellWidth * 1.5f, p.dy - 200f)!!
        assertEquals(0, idx / p.cols)
        assertEquals(1, idx % p.cols)
    }

    @Test
    fun `a finger below the grid means the cell at the bottom edge`() {
        val p = grid()
        val idx = p.dragCell(p.dx + p.cellWidth * 3.5f, p.dy + p.gridHeight + 500f)!!
        assertEquals(p.rows - 1, idx / p.cols)
        assertEquals(3, idx % p.cols)
    }

    @Test
    fun `a finger inside the grid is its own cell`() {
        val p = grid()
        val idx = p.dragCell(p.dx + p.cellWidth * 2.5f, p.dy + p.cellHeight * 4.5f)!!
        assertEquals(4, idx / p.cols)
        assertEquals(2, idx % p.cols)
    }

    @Test
    fun `a finger far past the far corner is still on the grid`() {
        val p = grid()
        // The boundary case `cellAt` gets wrong on its own: an x exactly on the
        // right edge floors to column `cols`, which is outside. dragCell clamps to
        // the last cell's centre instead, so it is never null.
        val idx = p.dragCell(p.dx + p.gridWidth * 4f, p.dy + p.gridHeight * 4f)!!
        assertEquals(p.rows - 1, idx / p.cols)
        assertEquals(p.cols - 1, idx % p.cols)
    }

    @Test
    fun `the viewport follows a finger that leaves the grid`() {
        val p = grid()
        assertEquals(0, p.scrollFor(p.dy + p.cellHeight))
        // The edge itself is inside. Only strictly past it asks for history —
        // otherwise a finger resting on the top row would scroll forever.
        assertEquals(0, p.scrollFor(p.dy))
        assertEquals(0, p.scrollFor(p.dy + p.gridHeight))
        assertEquals(-1, p.scrollFor(p.dy - 1f))
        assertEquals(1, p.scrollFor(p.dy + p.gridHeight + 1f))
    }

    @Test
    fun `scrolling up moves a text-anchored row down the screen`() {
        // THE sign, in one assertion. Ghostty's delta is "up is negative", and
        // scrolling up shows older lines, so content moves DOWN and a row grows.
        // The gesture used to add the delta, which slid the anchor by two rows per
        // row scrolled. Backed by the engine itself in test_engine.c's "a selection
        // follows its text when the viewport moves".
        assertEquals(1, rowAfterScroll(0, -1))
        assertEquals(0, rowAfterScroll(1, 1))
        assertEquals(5, rowAfterScroll(3, -2))
    }
}
