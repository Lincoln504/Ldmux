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

    /** A clock the test moves by hand, so "now" is a fact rather than a sleep. */
    private class TestClock { var now: Long = 1_000 }
    private var clock = TestClock()

    private fun source(): Triple<MeshTerminalSource, MeshTerminalHandle, MeshInputBatcher> {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, clock = { clock.now })
        return Triple(MeshTerminalSource(h, now = { clock.now }), h, b)
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

    /// **Leaving the page drops the screens the hold kept back.** They are the answer to a sync made
    /// on that page; returning to it must not resume a count of "view N behind" from a visit that is
    /// over, for the same reason the typing that caused it is dropped.
    @Test
    fun `discarding held screens clears the count and the pending screen`() {
        val (s, _, _) = source()
        s.setPublicChannel(true)
        s.show(MeshScreen(listOf("first"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 20), "aa11")
        s.show(MeshScreen(listOf("held"), 0, 0, alternate = false, dead = false, epoch = 2, cols = 20), "aa11")
        assertTrue(s.heldScreenWaiting, "the frozen view holds the second screen")
        assertTrue(s.heldUpdates >= 1)
        s.discardHeldScreens()
        assertFalse(s.heldScreenWaiting, "nothing is waiting once the page is left")
        assertEquals(0, s.heldUpdates, "and the bar has nothing to count")
    }

    /// **A sync opens the view just long enough to see the answer.** The wait ends on the first
    /// screen that arrives — normally the host echoing what was sent — and the answer follows in
    /// more screens, which were frozen. Measured on the device: `pwd`, enter, sync, and the bar said
    /// "view 2 behind" over a screen with no output; the user had to press `show` for the result of
    /// a command they had already sent.
    @Test
    fun `the screens after a sync are drawn, and the freeze returns once the shell goes quiet`() {
        val (s, _, b) = source()
        s.setPublicChannel(true)
        s.show(MeshScreen(listOf("first"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 20), "aa11")
        s.show(MeshScreen(listOf("frozen"), 0, 0, alternate = false, dead = false, epoch = 2, cols = 20), "aa11")
        assertTrue(s.heldScreenWaiting, "the view is frozen before the sync")
        // Typing makes `sync` a send rather than a show, which arms the window.
        b.appendText("pwd".encodeToByteArray(), clock.now)
        assertTrue(s.sync(), "there was text to send")
        // The drain finishes, which is what opens the window: mid-send a screen is the pty's
        // reaction to a partial command and is held on purpose.
        s.syncDrained()
        s.show(MeshScreen(listOf("echo"), 0, 0, alternate = false, dead = false, epoch = 3, cols = 20), "aa11")
        assertFalse(s.heldScreenWaiting, "the answer must be drawn, not held")
        s.show(MeshScreen(listOf("/tmp"), 0, 0, alternate = false, dead = false, epoch = 4, cols = 20), "aa11")
        assertFalse(s.heldScreenWaiting, "and so must the rest of the answer")
        // **The wait ends when the shell is quiet, not when the first screen lands.** The echo, the
        // output and the prompt after it all draw; then, with nothing arriving for a round trip,
        // the view freezes again — which is what keeps batch mode a mode rather than live.
        clock.now += PublicChannelHold.QUIET_MS
        assertTrue(s.settleIfQuiet(), "the shell has gone quiet, so the answer is finished")
        s.show(MeshScreen(listOf("later"), 0, 0, alternate = false, dead = false, epoch = 5, cols = 20), "aa11")
        assertTrue(s.heldScreenWaiting, "after the answer the view freezes again")
    }
}
