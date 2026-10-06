package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * When a page is connected.
 *
 * A page exists to be looked at, so it is connected while it is the one on
 * screen — and while the app is on screen at all. The second half was missing:
 * backgrounding the app left the page attached for the whole release grace, and
 * the cost was paid somewhere else. A desk window went on saying "This shell is
 * open on another device. Press r to take it back" while nothing was using the
 * shell and no page was in front of anyone (2026-10-05).
 */
class PageConnectionTest {

    @Test
    fun `the page on screen in the app on screen is connected`() {
        assertTrue(pageIsConnected(shown = true, appOnScreen = true))
    }

    @Test
    fun `a neighbouring page is composed but not connected`() {
        assertFalse(pageIsConnected(shown = false, appOnScreen = true))
    }

    @Test
    fun `no page is connected while the app is in the background`() {
        // The one that mattered: the page on screen is still *the* page, and the
        // app is not on screen, so nothing is reading it and the shell goes back.
        assertFalse(pageIsConnected(shown = true, appOnScreen = false))
        assertFalse(pageIsConnected(shown = false, appOnScreen = false))
    }
}
