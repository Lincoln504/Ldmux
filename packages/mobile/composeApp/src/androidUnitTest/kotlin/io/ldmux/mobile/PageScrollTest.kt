package io.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * The scroll pad turns a page, not a nudge.
 *
 * It stepped three rows until 2026-11-05 — `SCROLL_STEP`, with a comment calling a page "a
 * swipe" — so on a 38-row phone screen the pad moved about a twelfth of what the reader asked
 * for. A page is now the pane's own height less a two-row overlap, and that arithmetic lives
 * here so it can be checked without a native engine.
 */
class PageScrollTest {

    @Test
    fun `a page is the pane less the overlap`() {
        assertEquals(22, pageStep(24))
        assertEquals(36, pageStep(38))
        assertEquals(10, pageStep(12))
    }

    @Test
    fun `a pane shorter than the overlap still moves one row`() {
        // A three-row pane: two less two is one, and one less two would be a refusal.
        assertEquals(1, pageStep(3))
        assertEquals(1, pageStep(2))
        assertEquals(1, pageStep(1))
    }

    @Test
    fun `an unmeasured pane still moves`() {
        // The buttons are drawn before a frame exists; a step of zero is a button that looks
        // live and does nothing, which is the failure this whole screen keeps trying not to be.
        assertEquals(1, pageStep(0))
        assertEquals(1, pageStep(-5))
    }

    @Test
    fun `the overlap is smaller than any plausible pane`() {
        // If the overlap ever reached a phone's row count, every page turn would be one row and
        // nothing would say so.
        assertEquals(true, SCROLL_OVERLAP < 10)
    }
}
