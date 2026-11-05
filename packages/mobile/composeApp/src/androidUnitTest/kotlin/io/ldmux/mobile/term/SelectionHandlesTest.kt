package io.ldmux.mobile.term

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.IntSize
import io.ldmux.mobile.term.TerminalFrame.SelectionBounds
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The handles are drawn from one geometry and hit-tested from the same one, because a
 * hit test that is not the exact inverse of the draw is the recorded failure mode: a
 * handle you can see and cannot grab. These tests pin the two together and pin the
 * arithmetic that makes a dragged end feel attached to the finger.
 */
class SelectionHandlesTest {

    // 80 columns of 10x20 px cells, grid flush at the origin.
    private val placement = GridPlacement(
        dx = 0f,
        dy = 0f,
        cellWidth = 10f,
        cellHeight = 20f,
        cols = 80,
        rows = 24,
    )

    private val oneWord = SelectionBounds(startCol = 2, startRow = 1, endCol = 6, endRow = 1)

    @Test
    fun `the anchors are the corners of the range`() {
        val (start, end) = selectionHandleAnchors(oneWord, placement)
        // Bottom-left of the first cell, bottom-right of the last — one column past
        // the end, because the end cell is selected and the corner is after it.
        assertEquals(Offset(2f * 10f, 2f * 20f), start)
        assertEquals(Offset(7f * 10f, 2f * 20f), end)
    }

    // The platform's handles, at the size the framework's own drawables are: 48dp by
    // 32dp on a 2x-density screen.
    private val handleSize = IntSize(96, 64)

    private fun anchorOf(start: Boolean) =
        if (start) {
            selectionHandleAnchors(oneWord, placement).first
        } else {
            selectionHandleAnchors(oneWord, placement).second
        }

    @Test
    fun `the platform hotspots lean inwards at the text`() {
        // The framework's own convention: three quarters of the width for the left
        // handle, a quarter for the right. This is what makes the teardrop's tip -
        // not its middle - land on the corner of the selection.
        val start = selectionHandleTopLeft(anchorOf(true), handleSize, start = true)
        assertEquals(2f * 10f - 96f * 0.75f, start.x, 0.001f)
        assertTrue(start.x < 2f * 10f, "the left handle hangs to the left of its tip")
        val end = selectionHandleTopLeft(anchorOf(false), handleSize, start = false)
        assertEquals(7f * 10f - 96f * 0.25f, end.x, 0.001f)
        assertTrue(end.x + 96f > 7f * 10f, "the right handle hangs to the right of its tip")
        // Both hang *below* the line they describe.
        assertEquals(2f * 20f, start.y, 0.001f)
        assertEquals(2f * 20f, end.y, 0.001f)
    }

    @Test
    fun `a finger on the start handle grabs the start`() {
        val grab = handleHit(anchorOf(true), oneWord, placement, handleSize, touchPx = 48f)
        assertNotNull(grab)
        assertTrue(grab.start)
    }

    @Test
    fun `a finger on the end handle grabs the end`() {
        val grab = handleHit(anchorOf(false), oneWord, placement, handleSize, touchPx = 48f)
        assertNotNull(grab)
        assertTrue(!grab.start)
    }

    @Test
    fun `a finger in the middle of the range grabs nothing`() {
        // Mid-range, and vertically where the text is rather than where the handles
        // hang: dragging from inside a selection must extend it, not move an end.
        assertNull(handleHit(Offset(45f, 30f), oneWord, placement, handleSize, touchPx = 48f))
    }

    @Test
    fun `the grab offset puts the anchor back under the anchor`() {
        // The property that makes a drag feel attached: whatever the finger lands on,
        // finger plus offset is the anchor, so the end does not jump on the first move
        // event.
        for (start in listOf(true, false)) {
            val anchor = anchorOf(start)
            // A finger on the drawn handle, a few pixels off the tip.
            val finger = anchor + Offset(4f, 6f)
            val grab = handleHit(finger, oneWord, placement, handleSize, touchPx = 48f)
            assertNotNull(grab, "start=$start")
            assertEquals(anchor.x, finger.x + grab.offset.x, 0.001f)
            assertEquals(anchor.y, finger.y + grab.offset.y, 0.001f)
        }
    }

    @Test
    fun `a tiny grid cannot produce a handle or a crash`() {
        // Before the first measurement the placement has no size. Nothing may divide by
        // it, and no handle may be reported where none can be drawn.
        val empty = GridPlacement(0f, 0f, 0f, 0f, 0, 0)
        assertNull(handleHit(Offset(0f, 0f), oneWord, empty, handleSize, touchPx = 48f))
    }
}
