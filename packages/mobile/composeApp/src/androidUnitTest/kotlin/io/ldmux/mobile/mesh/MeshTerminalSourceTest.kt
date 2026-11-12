package io.ldmux.mobile.mesh

import io.ldmux.mobile.StickyKey
import io.ldmux.mobile.term.TerminalFrame
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
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

    private fun source(): Triple<MeshTerminalSource, MeshTerminalHandle, MeshDraft> {
        val d = MeshDraft()
        val h = MeshTerminalHandle(d, clock = { clock.now })
        return Triple(MeshTerminalSource(h, now = { clock.now }), h, d)
    }

    @Test
    fun `a tap on the cursor's line walks the cursor with named keys, into the draft`() {
        val (s, _, b) = source()
        assertTrue(s.tapOn(frame(cursorX = 10, cursorY = 5), col = 7, row = 5))
        val keys = b.peek().filterIsInstance<DraftItem.Keys>().flatMap { it.names }
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
        s.prependHistory(page(from = 96, lines = listOf("w", "x", "y", "z"), older = true))
        assertEquals(1, s.scrolledRows, "the press moved one row, into the fetched page")
        asked = null
        assertEquals(-2, s.scrollBy(-2), "two more rows, still inside the page")
        assertEquals(null, asked, "no request for a row the page already covers")
    }

    /// A page as the host sends it: `from` is the absolute line of its first row.
    private fun page(
        from: Int,
        lines: List<String>,
        last: Boolean = true,
        older: Boolean = false,
    ) = TermMessage.HistoryPage(from, last, older, lines)

    private fun screenOf(first: Int, vararg lines: String) = MeshScreen(
        lines.toList(), 0, lines.lastIndex,
        alternate = false, dead = false, epoch = 1, cols = 20, firstLine = first,
    )

    /// **A page is placed by the numbering it carries, and all three cases happen.**
    ///
    /// Contiguous is the normal answer. An *overlap* is the host serving from a line this client
    /// already holds (a request made from a stale top), and showing those rows twice is worse than
    /// showing them once. A *gap* is the pane having dropped rows since the request; the operator
    /// is told, because splicing the page onto the older rows would quietly invent history.
    @Test
    fun `a page is trimmed where it overlaps and named where it leaves a gap`() {
        val (s, _, _) = source()
        s.show(screenOf(100, "a", "b", "c", "d"), "aa11")

        // Contiguous: lines 96..99 end exactly at the live screen's first line, and the press that
        // asked for them moves one row in.
        s.prependHistory(page(96, listOf("w", "x", "y", "z"), older = true))
        assertEquals(1, s.scrolledRows, "the press moved one row into the page")
        assertNull(s.scrollNotice, "nothing to warn about, and more history above")

        // Overlap: 94..97 straddles the oldest row held (96), so only 94 and 95 are new. The view
        // is not at the top here, so it does not move: the two rows are added above it.
        s.prependHistory(page(94, listOf("m", "n", "o", "p"), older = true))
        assertEquals(3, s.scrolledRows, "the view stayed on its row, two rows added above it")

        // A page this client already holds entirely adds nothing and moves nothing.
        s.prependHistory(page(97, listOf("q", "r"), older = true))
        assertEquals(3, s.scrolledRows, "nothing was added, so nothing moved")

        // Gap: the oldest row held is 94 and the page ends at 90, so 86..89 join across a hole the
        // pane has already dropped.
        s.prependHistory(page(86, listOf("e", "f", "g", "h"), older = true))
        assertEquals("history has a gap here", s.scrollNotice)
    }

    /// **A lost request expires, so the up key never goes dead.**
    ///
    /// One request in flight at a time, or a fast press would queue a record per press. But a
    /// request that never arrives — a lost record, a host that dropped it — must not leave the key
    /// doing nothing for the rest of the session: past a deadline it may ask again.
    @Test
    fun `an unanswered history request expires and the key asks again`() {
        val (s, _, _) = source()
        var asks = 0
        var lastAsked: Pair<Int, Int>? = null
        s.onWantHistory = { from, rows ->
            asks++
            lastAsked = from to rows
        }
        s.show(screenOf(100, "a", "b", "c", "d"), "aa11")
        assertEquals(0, s.scrollBy(-1))
        assertEquals(1, asks, "the first press asks")
        assertEquals(96 to 4, lastAsked)

        assertEquals(0, s.scrollBy(-1))
        assertEquals(1, asks, "a second press does not queue another while one is in flight")

        // Past the deadline: the request is treated as lost and the key works again.
        clock.now += 11_000
        assertEquals(0, s.scrollBy(-1))
        assertEquals(2, asks, "the key asks again once the wait is up")
    }

    /// **`older` is how the operator learns the top is reached, without spending a record.**
    @Test
    fun `a page with nothing above it says so`() {
        val (s, _, _) = source()
        s.show(screenOf(100, "a", "b", "c", "d"), "aa11")
        s.prependHistory(page(96, listOf("w", "x", "y", "z"), older = false))
        assertEquals("start of history", s.scrollNotice)
    }

    /// **An empty answer above the live screen is still placed, not drawn as silence.**
    @Test
    fun `an empty page says whether history exists above it`() {
        val (s, _, _) = source()
        s.show(screenOf(100, "a", "b", "c", "d"), "aa11")
        s.prependHistory(page(100, emptyList(), older = true))
        assertEquals("history is above this", s.scrollNotice, "the request was clamped to the live screen")
        s.prependHistory(page(0, emptyList(), older = false))
        assertEquals("start of history", s.scrollNotice)
        assertEquals(0, s.scrolledRows, "an empty answer moves nothing")
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
     * to call the page's input directly, so Esc/Tab/Ctrl-c/arrows went to the live prompt while
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
        assertFalse(b.isEmpty, "the key still reaches the draft")
    }

    // ── the v15 batch model: one draft, one command, two controls ───────────────────────────

    /**
     * **A send is one command, and the draft leaves whole.** The old model queued pieces and let a
     * pump send them one record at a time; v15 puts the entire draft on the air in one message, so
     * what matters is that the source hands the items over in composition order and clears them
     * only after the engine took them (`docs/RADIO-PLAN.md` §6.1, `RC-8`).
     */
    @Test
    fun `send hands the whole draft over once and takes the state to waiting`() = kotlinx.coroutines.runBlocking {
        val (s, h, d) = source()
        h.sendText("ls -la")
        h.sendKey("Enter")
        var sent: List<DraftItem>? = null
        s.onSend = { items -> sent = items; true }
        assertTrue(s.send())
        val items = assertNotNull(sent)
        assertEquals(2, items.size, "the text and its Enter are one command: {items}")
        assertTrue(items[0] is DraftItem.Text)
        assertTrue(items[1] is DraftItem.Keys)
        assertTrue(d.isEmpty, "the draft is spent once the engine took it")
        assertEquals(BatchState.Waiting, s.batchState)
    }

    /** A refused send leaves the draft alone: a failure must not look like a sent command. */
    @Test
    fun `a refused send keeps every keystroke`() = kotlinx.coroutines.runBlocking {
        val (s, h, d) = source()
        h.sendText("rm -rf /tmp/x")
        s.onSend = { false }
        assertFalse(s.send())
        assertFalse(d.isEmpty, "the operator's text survives a refusal")
        assertEquals(BatchState.Idle, s.batchState)
    }

    /** **`refresh` never spends the draft.** Looking at a shell cannot run what is typed in it. */
    @Test
    fun `refresh asks for the screen and leaves the draft untouched`() {
        val (s, h, d) = source()
        h.sendText("pass")
        var asked = 0
        s.onRefresh = { asked++ }
        s.refresh()
        assertEquals(1, asked)
        assertFalse(d.isEmpty, "a refresh must not spend what is typed")
        assertEquals(BatchState.Idle, s.batchState, "and it is not a send, so nothing is owed")
    }

    /**
     * **The answer ends the wait, and "done" is not for ever.** The host's own `Settled` is the only
     * honest end: a timer that decided would either cut a slow answer off or leave "waiting" over a
     * screen the operator is already reading.
     */
    @Test
    fun `the answer settles the bar and the clock returns it to the draft`() = kotlinx.coroutines.runBlocking {
        val (s, h, _) = source()
        h.sendText("pwd")
        s.onSend = { true }
        assertTrue(s.send())
        assertEquals(BatchState.Waiting, s.batchState)
        s.answered()
        assertEquals(BatchState.Settled, s.batchState)
        clock.now += SETTLED_SHOW_MS + 1
        s.batchTick()
        assertEquals(BatchState.Idle, s.batchState, "done is a moment, not a state")
    }

    /**
     * **A wait that crosses repeaters is sized by the route.** A flat deadline declared an answer
     * lost while it was still travelling, so the deadline grows with the hops asked of the source.
     */
    @Test
    fun `a wait with no answer goes late, and the route buys it time`() = kotlinx.coroutines.runBlocking {
        var hops: Int? = null
        val d = MeshDraft()
        val h = MeshTerminalHandle(d, clock = { clock.now })
        val s = MeshTerminalSource(h, now = { clock.now }, hops = { hops })
        h.sendText("ls")
        s.onSend = { true }
        assertTrue(s.send())
        // Past the base deadline, but a twenty-hop route buys forty more seconds.
        hops = 20
        clock.now += ANSWER_WAIT_MS + 1
        s.batchTick()
        assertEquals(BatchState.Waiting, s.batchState, "twenty hops is forty more seconds of wait")
        clock.now += 20 * 2_000L
        s.batchTick()
        assertEquals(BatchState.NoAnswer, s.batchState, "and past that it is late")
    }
}
