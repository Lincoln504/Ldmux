package io.ldmux.mobile.mesh

import io.ldmux.mobile.StickyKey
import io.ldmux.mobile.term.TerminalFrame
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** What the shared surface asks of the mesh route: a tap, a size, a scroll, a screen. */
class MeshTerminalSourceTest {
    private fun frame(cursorX: Int, cursorY: Int, cols: Int = 80, rows: Int = 24) = TerminalFrame.Frame(
        cols = cols, rows = rows, cursorX = cursorX, cursorY = cursorY, cursorVisible = true, cursorKnown = true,
        cursorStyle = 1, defaultFg = 0, defaultBg = 0, passwordInput = false, viewportActive = true,
        mouseTracking = false, mouseSgr = false, alternateOn = false, selection = null,
        scrollTotal = 0, scrollOffset = 0, scrollLen = 0, cells = emptyList(),
    )

    private fun source(): Triple<MeshTerminalSource, MeshTerminalHandle, MeshInputBatcher> {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        return Triple(MeshTerminalSource(h), h, b)
    }

    @Test
    fun `a tap on the cursor's line walks the cursor with named keys, through the batcher`() {
        val (s, _, b) = source()
        assertTrue(s.tapOn(frame(cursorX = 10, cursorY = 5), col = 7, row = 5))
        val keys = b.pending.filterIsInstance<MeshOut.Keys>().flatMap { it.names }
        assertEquals(List(3) { "Left" }, keys)
    }

    @Test
    fun `a tap on the cursor's own cell does nothing, so no outline is drawn`() {
        val (s, _, b) = source()
        assertFalse(s.tapOn(frame(cursorX = 10, cursorY = 5), col = 10, row = 5))
        assertTrue(b.isEmpty)
    }

    @Test
    fun `a tap never spends an armed Ctrl`() {
        val (s, h, _) = source()
        h.toggleModifier(StickyKey.Ctrl)
        s.tapOn(frame(cursorX = 10, cursorY = 5), col = 9, row = 5)
        assertTrue(h.modifiers.armed, "the user armed Ctrl for their next keystroke, not for a tap")
    }

    @Test
    fun `nothing to scroll before a screen arrives, and the measured grid goes out as a resize request`() {
        val (s, _, _) = source()
        assertEquals(0, s.scrollBy(-5))
        var asked: Pair<Int, Int>? = null
        s.onGrid = { c, r -> asked = c to r }
        s.setGrid(33, 7)
        assertEquals(33 to 7, asked)
        assertNull(s.frame.value)
    }

    @Test
    fun `a screen with no profile yet (no columns) is not shown`() {
        val (s, _, _) = source()
        s.show(MeshScreen(listOf("x"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 0))
        assertNull(s.frame.value)
        assertFalse(s.hasSelection)
    }

    /// **A newly opened shell paints its first screen, in batch mode, with no timer and no tap.**
    ///
    /// The hold draws a screen only when nothing has been painted; it used to judge that over the
    /// source's whole life, so a previous shell's last screen made the next shell's first screen
    /// "not the first" and it was held. On a public channel the first full screen takes longer than
    /// the 1.5 s timer that was papering over this, so the page stayed blank until a refresh.
    @Test
    fun `the first screen of a newly opened shell is drawn, not held`() {
        val (s, _, _) = source()
        s.setPublicChannel(true)
        s.show(MeshScreen(listOf("shell A"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 20), "aa11")
        // The frozen view holds the second screen of the shell already on screen.
        s.show(MeshScreen(listOf("shell A again"), 0, 0, alternate = false, dead = false, epoch = 2, cols = 20), "aa11")
        assertTrue(s.heldScreenWaiting, "a frozen view holds the shell it is already showing")
        // A different shell is a new screen, and it is the first screen of that shell.
        s.show(MeshScreen(listOf("shell B"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 20), "bb22")
        assertFalse(s.heldScreenWaiting, "the new shell must not inherit the old one's hold")
    }
}
