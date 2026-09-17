package dev.ldmux.mobile

import dev.ldmux.mobile.term.TerminalFrame
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The tap policy decides what a finger on the terminal means. Every rule here
 * was either wrong on a real device at some point or prevents a bug that was:
 * a shell printing `^[[<0;42;8M` because a click was written to a program that
 * had not asked for one, and taps that silently did nothing.
 */
class TapPolicyTest {

    private fun frame(
        cols: Int = 80,
        rows: Int = 24,
        cursorX: Int = 10,
        cursorY: Int = 5,
        mouseTracking: Boolean = false,
        mouseSgr: Boolean = false,
        alternateOn: Boolean = false,
        viewportActive: Boolean = true,
        passwordInput: Boolean = false,
    ) = TerminalFrame.Frame(
        cols = cols,
        rows = rows,
        cursorX = cursorX,
        cursorY = cursorY,
        cursorVisible = true,
        cursorStyle = 1,
        defaultFg = 0,
        defaultBg = 0,
        passwordInput = passwordInput,
        viewportActive = viewportActive,
        mouseTracking = mouseTracking,
        mouseSgr = mouseSgr,
        alternateOn = alternateOn,
        scrollTotal = 0,
        scrollOffset = 0,
        scrollLen = 0,
        cells = emptyList(),
    )

    @Test
    fun `a full-screen program gets a grid-shaped tap, not a strip`() {
        // Measured: in vim, 40 Right keys moved the cursor 5 columns and clamped
        // at the line end (`l` does not wrap), while Down moved whole lines. So a
        // tap two rows up and 30 columns left must be two Ups and 30 Lefts — and
        // reaches the cell that was tapped, with no app blocklist and no setting.
        val a = planTap(10, 3, frame(cols = 80, rows = 24, cursorX = 40, cursorY = 5, alternateOn = true))
        val keys = (a as TapAction.Keys).keys
        assertEquals("Up", keys.first())
        assertEquals(2, keys.count { it == "Up" })
        assertEquals(0, keys.count { it == "Down" })
        assertEquals(30, keys.count { it == "Left" })
        assertEquals(0, keys.count { it == "Right" })
    }

    @Test
    fun `a tap below and right in a full-screen program goes down then right`() {
        // Vertical first, so the horizontal part clamps on the line actually
        // tapped rather than on the one we started from.
        val a = planTap(20, 8, frame(cols = 80, cursorX = 5, cursorY = 5, alternateOn = true))
        val keys = (a as TapAction.Keys).keys
        assertEquals(listOf("Down", "Down", "Down") + List(15) { "Right" }, keys)
    }

    @Test
    fun `a grid tap is capped too, and never uses Home`() {
        val a = planTap(0, 23, frame(cols = 500, rows = 24, cursorX = 499, cursorY = 0, alternateOn = true)) as TapAction.Keys
        assertEquals(MAX_TAP_KEYS, a.keys.size)
        assertEquals(0, a.keys.count { it == "Home" })
    }

    @Test
    fun `pi and Claude Code run on the primary screen, so taps still move their cursor`() {
        // Measured on this box: pi and claude report alternate_on=0 and no mouse
        // mode, unlike vim/htop which are alternate_on=1. They draw their own
        // composer on the primary screen, where arrows are ordinary cursor keys.
        val a = planTap(20, 5, frame(alternateOn = false))
        assertTrue(a is TapAction.Keys)
        assertEquals(List(10) { "Right" }, a.keys)
    }

    @Test
    fun `a program that asked for SGR clicks gets a click, on either screen`() {
        // An explicit mouse request outranks every heuristic.
        for (alt in listOf(false, true)) {
            assertEquals(
                TapAction.Click(20, 5),
                planTap(20, 5, frame(alternateOn = alt, mouseTracking = true, mouseSgr = true)),
            )
        }
    }

    @Test
    fun `a mouse-aware program gets a click`() {
        val a = planTap(20, 5, frame(mouseTracking = true, mouseSgr = true))
        assertEquals(TapAction.Click(20, 5), a)
    }

    @Test
    fun `a shell gets cursor motion, never a click`() {
        val a = planTap(20, 5, frame(mouseTracking = false, mouseSgr = false))
        assertTrue(a is TapAction.Keys)
        // cursor is at column 10, so this is 10 steps right.
        assertEquals(List(10) { "Right" }, a.keys)
    }

    @Test
    fun `tracking without SGR is not clickable, because we can only speak SGR`() {
        val a = planTap(20, 5, frame(mouseTracking = true, mouseSgr = false))
        assertTrue(a is TapAction.Keys, "legacy encodings cap coordinates at 223, so they are not synthesized")
    }

    @Test
    fun `a refused click falls back to cursor motion, which is what the tap meant`() {
        // This is the device bug: the mouse mode is sticky, so a program that
        // enabled reporting and then exited leaves the flags on while a shell is
        // what would receive the bytes and print them.
        val a = planTap(20, 5, frame(mouseTracking = true, mouseSgr = true), mouseRefused = true)
        assertTrue(a is TapAction.Keys, "the server said no; the tap must still do the obvious thing")
        assertEquals(List(10) { "Right" }, a.keys)
    }

    @Test
    fun `a tap left of the cursor moves left`() {
        val a = planTap(3, 5, frame())
        assertEquals(List(7) { "Left" }, (a as TapAction.Keys).keys)
    }

    @Test
    fun `a tap on another row counts the wrap, so a wrapped line lands where it looks`() {
        // One row down from a 80-column pane is +80 cells, not +1.
        val a = planTap(10, 6, frame(cols = 80, cursorX = 10, cursorY = 5))
        assertEquals(List(80) { "Right" }, (a as TapAction.Keys).keys)
    }

    @Test
    fun `a tap well above the input line is a single Home`() {
        // Walking there would be dozens of keys and dozens of readline redraws.
        val a = planTap(3, 1, frame(cursorX = 40, cursorY = 10))
        assertEquals(TapAction.Keys(listOf("Home")), a)
    }

    @Test
    fun `one row above is still relative motion, not Home`() {
        // readline only offers Home at the line start, so the threshold for the
        // one-key shortcut is "strictly more than one row up" — a wrapped input
        // can be reached by arrows. The screen is a continuous strip, so one row
        // up at 80 columns is 80 cells plus the 37 leftwards: 117 keys. On a
        // short single-line input readline clamps at the line start, so this
        // still lands at the beginning, just not for free.
        val a = planTap(3, 9, frame(cursorX = 40, cursorY = 10)) as TapAction.Keys
        assertEquals(117, a.keys.size)
        assertTrue(a.keys.all { it == "Left" })
    }

    @Test
    fun `a tap on the cursor does nothing`() {
        assertEquals(TapAction.Ignore, planTap(10, 5, frame(cursorX = 10, cursorY = 5)))
    }

    @Test
    fun `nothing is sent while reading history`() {
        // There is no live cursor up there, and scrollback is the emulator's.
        assertEquals(TapAction.Ignore, planTap(3, 5, frame(viewportActive = false)))
    }

    @Test
    fun `nothing is sent at a password prompt`() {
        assertEquals(TapAction.Ignore, planTap(3, 5, frame(passwordInput = true)))
    }

    @Test
    fun `a tap outside the grid does nothing`() {
        assertEquals(TapAction.Ignore, planTap(-1, 5, frame()))
        assertEquals(TapAction.Ignore, planTap(3, -1, frame()))
        assertEquals(TapAction.Ignore, planTap(80, 5, frame(cols = 80)))
        assertEquals(TapAction.Ignore, planTap(3, 24, frame(rows = 24)))
    }

    @Test
    fun `a long move is capped so a stray tap cannot become a key storm`() {
        val a = planTap(0, 23, frame(cols = 500, rows = 24, cursorX = 499, cursorY = 0)) as TapAction.Keys
        assertEquals(MAX_TAP_KEYS, a.keys.size, "11001 cells of travel must be capped")
        assertTrue(a.keys.all { it == "Right" }, "the cursor is at column 499, so the move is leftwards in cell order but forwards in key order")
    }

    @Test
    fun `the cursor is placed where the pane has it, not where the replay ended`() {
        // 1-based on the wire, and clamped: a pane resized between the snapshot
        // and the write must not send the cursor off the grid.
        assertEquals("\u001b[6;11H", cursorSequence(col = 10, row = 5, cols = 80, rows = 24))
        assertEquals("\u001b[1;1H", cursorSequence(col = 0, row = 0, cols = 80, rows = 24))
        assertEquals("\u001b[24;80H", cursorSequence(col = 999, row = 999, cols = 80, rows = 24))
    }

    @Test
    fun `batches respect the daemon message cap but keep every key in order`() {
        val keys = List(MAX_TAP_KEYS) { "Left" }
        val batches = batchKeys(keys)
        assertEquals(listOf(32, 32, 32, 24), batches.map { it.size })
        assertEquals(keys, batches.flatten())
        assertEquals(emptyList(), batchKeys(emptyList()))
        assertEquals(listOf(listOf("Home")), batchKeys(listOf("Home")))
    }

    @Test
    fun `a captured screen loses exactly one trailing newline before replay`() {
        // A 20-row pane comes back with 20 newlines: capture-pane terminates every
        // row, including the last. Replaying all of them moves the cursor past the
        // bottom, the emulator scrolls, and every line shifts up by one — while the
        // cursor is separately placed at the pane's real row, so it lands one row
        // *below* the text it belongs to.
        val twenty = List(20) { "row$it" }.joinToString("\n") + "\n"
        val replayed = snapshotReplay(twenty)
        assertEquals(19, Regex("\r\n").findAll(replayed).count())
        assertTrue(!replayed.endsWith("\r\n"))
    }

    @Test
    fun `rows are separated by CRLF, or the screen staircases`() {
        // A bare LF preserves the column, so each row would start one cell further
        // right than the last.
        val replayed = snapshotReplay("a\nb\n")
        assertEquals("a\r\nb", replayed)
    }

    @Test
    fun `an empty capture stays empty`() {
        assertEquals("", snapshotReplay(""))
    }
}
