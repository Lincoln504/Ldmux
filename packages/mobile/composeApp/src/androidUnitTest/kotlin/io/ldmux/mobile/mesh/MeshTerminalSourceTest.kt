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

    /// **`show` on a frozen view releases the host instead of doing nothing.**
    ///
    /// With `Hold`, the host sends no screens while the view is frozen, so there is never a held
    /// screen to draw — and `show` used to return early on exactly that condition. The button did
    /// nothing at all on a batch shell (measured 2026-11-09), and the operator concluded the link
    /// was dead. Releasing the host is what makes the press mean "show me the shell now".
    /// **A screen with one row more or less does not throw the operator out of history.**
    ///
    /// The view used to return to the live bottom whenever the grid changed at all, so a shell
    /// whose last row came and went — a prompt, a status line — yanked a reader back to live two
    /// screens after they scrolled up (measured on the device 2026-11-09: offset 33, then offset 0).
    /// The offset counts rows above the live bottom and the window clamps it, so a height change
    /// keeps the position; only a *width* change is a reflow that invalidates it.
    @Test
    fun `a height change keeps the view where the operator put it`() {
        val (s, _, _) = source()
        s.show(
            MeshScreen(
                listOf("a", "b", "c", "d"), 0, 3,
                alternate = false, dead = false, epoch = 1, cols = 20, firstLine = 10,
            ),
            "aa11",
        )
        s.show(
            MeshScreen(
                listOf("c", "d", "e", "f"), 0, 3,
                alternate = false, dead = false, epoch = 2, cols = 20, firstLine = 12,
            ),
            "aa11",
        )
        assertEquals(-2, s.scrollBy(-2), "two rows of history to read")
        assertEquals(2, s.scrolledRows)
        // One row shorter, same width: a prompt line came or went.
        s.show(
            MeshScreen(
                listOf("d", "e", "f"), 0, 2,
                alternate = false, dead = false, epoch = 3, cols = 20, firstLine = 13,
            ),
            "aa11",
        )
        assertEquals(2, s.scrolledRows, "still reading history after a height change")
    }

    /// **A width change is a reflow, and the view starts again at the live screen.**
    @Test
    fun `a width change returns the view to live`() {
        val (s, _, _) = source()
        s.show(
            MeshScreen(
                listOf("a", "b", "c"), 0, 2,
                alternate = false, dead = false, epoch = 1, cols = 20, firstLine = 10,
            ),
            "aa11",
        )
        s.show(
            MeshScreen(
                listOf("c", "d", "e"), 0, 2,
                alternate = false, dead = false, epoch = 2, cols = 20, firstLine = 12,
            ),
            "aa11",
        )
        assertEquals(-2, s.scrollBy(-2))
        s.show(
            MeshScreen(
                listOf("d", "e", "f"), 0, 2,
                alternate = false, dead = false, epoch = 3, cols = 30, firstLine = 13,
            ),
            "aa11",
        )
        assertEquals(0, s.scrolledRows, "a reflow starts again at the live screen")
    }

    /// **An up key with no history asks the host for the page above.**
    ///
    /// The keeper holds the pane's history and the link carries only the live screen, so "no
    /// history yet on this link" was true of this client and useless to the operator. The request
    /// names the absolute line above the oldest row held here — with nothing held, that is the page
    /// immediately above the live screen. The page is then prepended, so the next up key asks for
    /// the page above *that* rather than the same one again.
    @Test
    fun `an up key with no history asks the host for a page`() {
        val (s, _, _) = source()
        var asked: Pair<Int, Int>? = null
        s.onWantHistory = { from, rows -> asked = from to rows }
        s.show(
            MeshScreen(
                listOf("a", "b", "c", "d"), 0, 3,
                alternate = false, dead = false, epoch = 1, cols = 20, firstLine = 100,
            ),
            "aa11",
        )
        assertEquals(0, s.scrollBy(-1), "nothing to scroll locally yet")
        // **A page is fetched, and the press decides the movement.** One request either way, so a
        // one-row key asks for the same page a page key does — the page above a screen whose top is
        // line 100, four rows because this screen is four rows tall.
        assertEquals(96 to 4, asked, "the page above a screen whose top is line 100")

        // The page is now in hand **and the press has been honoured**: the view sits one row up,
        // which is what the press asked for. The rest of the page costs nothing more.
        s.prependHistory(listOf("w", "x", "y", "z"))
        assertEquals(1, s.scrolledRows, "the press moved one row, into the fetched page")
        asked = null
        assertEquals(-2, s.scrollBy(-2), "two more rows, still inside the page")
        assertEquals(null, asked, "no request for a row the page already covers")
    }

    @Test
    fun `show on a frozen view unfreezes it so the host draws again`() {
        val (s, _, b) = source()
        s.setPublicChannel(true)
        s.show(MeshScreen(listOf("$ one", "$ "), 2, 1, alternate = false, dead = false, epoch = 1, cols = 20), "aa11")
        assertTrue(s.hasScreen, "the first screen paints")
        assertTrue(b.isEmpty, "nothing was typed")

        assertFalse(s.sync(), "nothing to send, so the press does not send")
        assertTrue(s.awaitingSync, "but the view follows the host again, which is what releases the hold")
        assertFalse(s.awaitingAnswer, "and the bar does not claim an answer is owed")
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

    /**
     * **A key sent while reading history returns the view to the live screen.** The key bars used
     * to call the page's batcher directly, so Esc/Tab/Ctrl-c/arrows went to the live prompt while
     * the user was looking at rows above it and saw nothing happen — the same defect fixed on the
     * tailnet (RELEASE-AUDIT 9.14). The bars now go through the source, whose `sendKey` follows the
     * input, and this is the difference between the two paths.
     */
    @Test
    fun `a key sent while reading history returns the view to the live bottom`() {
        val (s, _, b) = source()
        s.show(MeshScreen(listOf("one", "two", "three"), 0, 2, alternate = false, dead = false, epoch = 1, cols = 20), "aa11")
        s.show(MeshScreen(listOf("two", "three", "four"), 0, 2, alternate = false, dead = false, epoch = 2, cols = 20), "aa11")
        assertEquals(-1, s.scrollBy(-1), "one row up, into history")
        assertEquals(1, s.scrolledRows)
        s.sendKey("Escape")
        assertEquals(0, s.scrolledRows, "sending a key is typing, and typing returns to live")
        assertTrue(b.pending.isNotEmpty(), "the key still reaches the batcher")
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
    /// on that page; returning to it must not draw a screen kept by a visit that is over, for the
    /// same reason the typing that caused it is dropped.
    @Test
    fun `discarding held screens clears the pending screen`() {
        val (s, _, _) = source()
        s.setPublicChannel(true)
        s.show(MeshScreen(listOf("first"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 20), "aa11")
        s.show(MeshScreen(listOf("held"), 0, 0, alternate = false, dead = false, epoch = 2, cols = 20), "aa11")
        assertTrue(s.heldScreenWaiting, "the frozen view holds the second screen")
        s.discardHeldScreens()
        assertFalse(s.heldScreenWaiting, "nothing is waiting once the page is left")
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
