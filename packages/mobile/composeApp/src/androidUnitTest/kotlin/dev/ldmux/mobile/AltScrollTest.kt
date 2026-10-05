package dev.ldmux.mobile

import dev.ldmux.mobile.term.TerminalFrame
import kotlin.test.Test
import kotlin.test.assertEquals

/** A swipe on the alternate screen reaches the program as wheel events or arrows. */
class AltScrollTest {
    private fun frame(
        alternateOn: Boolean = true,
        mouseTracking: Boolean = false,
        mouseSgr: Boolean = false,
        viewportActive: Boolean = true,
        passwordInput: Boolean = false,
    ) = TerminalFrame.Frame(
        cols = 80, rows = 24, cursorX = 0, cursorY = 0, cursorVisible = true, cursorKnown = true,
        cursorStyle = 1, defaultFg = 0, defaultBg = 0, passwordInput = passwordInput,
        viewportActive = viewportActive, mouseTracking = mouseTracking, mouseSgr = mouseSgr,
        alternateOn = alternateOn, selection = null, scrollTotal = 0, scrollOffset = 0, scrollLen = 0,
        cells = emptyList(),
    )

    @Test
    fun `mouse reporting gets SGR wheel events`() {
        val f = frame(mouseTracking = true, mouseSgr = true)
        assertEquals(AltScroll.Wheel("\u001b[<64;41;13M"), planAltScroll(-1, f, false))
        assertEquals(AltScroll.Wheel("\u001b[<65;41;13M\u001b[<65;41;13M"), planAltScroll(2, f, false))
    }

    @Test
    fun `without mouse reporting a swipe is arrow keys`() {
        assertEquals(AltScroll.Keys(listOf("Up")), planAltScroll(-1, frame(), false))
        assertEquals(AltScroll.Keys(listOf("Down", "Down", "Down")), planAltScroll(3, frame(), false))
    }

    @Test
    fun `a refused mouse falls back to arrows`() {
        val f = frame(mouseTracking = true, mouseSgr = true)
        assertEquals(AltScroll.Keys(listOf("Up")), planAltScroll(-1, f, true))
    }

    @Test
    fun `a large swipe moves as many rows as it travelled`() {
        // The cap is 5 no longer: a flick that crosses the screen must scroll the
        // screen, not five lines.
        val k = planAltScroll(-20, frame(), false) as AltScroll.Keys
        assertEquals(20, k.keys.size)
        assertEquals(List(20) { "Up" }, k.keys)
    }

    @Test
    fun `the event count is capped`() {
        val a = planAltScroll(-100, frame(), false) as AltScroll.Keys
        assertEquals(ALT_SCROLL_MAX_EVENTS, a.keys.size)
    }

    @Test
    fun `nothing off the alternate screen or at a password prompt`() {
        assertEquals(AltScroll.None, planAltScroll(-1, frame(alternateOn = false), false))
        assertEquals(AltScroll.None, planAltScroll(-1, frame(passwordInput = true), false))
        assertEquals(AltScroll.None, planAltScroll(-1, frame(viewportActive = false), false))
        assertEquals(AltScroll.None, planAltScroll(0, frame(), false))
    }
}
