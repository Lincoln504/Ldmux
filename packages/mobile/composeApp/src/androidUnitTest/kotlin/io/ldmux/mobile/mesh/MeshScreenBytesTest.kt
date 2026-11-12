package io.ldmux.mobile.mesh

import io.ldmux.mobile.term.displayWidthOf
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The local byte stream that puts a committed mesh screen on the shared terminal engine. The engine
 * is native and not loadable in a JVM test, so these pin the stream itself: exact bytes for the
 * cases that decide how the screen looks, and an invariant that nothing from the host can add an
 * escape of its own.
 */
class MeshScreenBytesTest {
    private fun screen(vararg lines: String, x: Int = 0, y: Int = 0, dead: Boolean = false) =
        MeshScreen(lines.toList(), x, y, alternate = false, dead = dead, epoch = 1, cols = 80)

    private fun text(s: MeshScreen, cols: Int = 10, rows: Int = s.lines.size) =
        meshScreenBytes(s, cols, rows).decodeToString()

    private val head = "\u001b[0m\u001b[?7l"
    private val tail = "\u001b[?7h\u001b[?25h"

    @Test
    fun `an empty screen erases every row and puts the cursor home`() {
        assertEquals(
            head + "\u001b[1;1H\u001b[K" + "\u001b[2;1H\u001b[K" + tail + "\u001b[1;1H",
            text(screen("", "")),
        )
    }

    @Test
    fun `a row at column zero is addressed absolutely, written, then erased to its end`() {
        assertEquals(
            head + "\u001b[1;1H\$ ls\u001b[K" + tail + "\u001b[1;5H",
            text(screen("\$ ls", x = 4)),
        )
    }

    @Test
    fun `a row as wide as the grid is not wrapped and cannot scroll the last row`() {
        val full = "0123456789"
        val out = text(screen("a", full, x = 9, y = 1), cols = 10)
        assertTrue(out.startsWith(head), "autowrap is off before any text is written")
        assertTrue("\u001b[2;1H" + full + "\u001b[K" in out)
        assertTrue(out.indexOf("\u001b[?7l") < out.indexOf(full) && out.indexOf(full) < out.indexOf("\u001b[?7h"))
        assertTrue(out.endsWith("\u001b[2;10H"))
    }

    @Test
    fun `a row longer than the grid is cut to the grid`() {
        assertTrue("\u001b[1;1H0123456789\u001b[K" in text(screen("0123456789ABCDEF"), cols = 10))
    }

    @Test
    fun `a cursor past the end of the text or the grid is clamped into the grid, not dropped`() {
        assertTrue(text(screen("ab", x = 9), cols = 10).endsWith("\u001b[1;10H"))
        assertTrue(text(screen("ab", x = 500, y = 500), cols = 10, rows = 1).endsWith("\u001b[1;10H"))
        assertTrue(text(screen("ab", x = -4, y = -4), cols = 10).endsWith("\u001b[1;1H"))
    }

    @Test
    fun `a dead shell hides the cursor`() {
        assertTrue("\u001b[?25l" in text(screen("bye", dead = true)))
        assertTrue("\u001b[?25h" !in text(screen("bye", dead = true)))
    }

    @Test
    fun `a screen with fewer lines than the grid still erases the rest`() {
        val out = text(screen("only"), rows = 3)
        assertTrue("\u001b[2;1H\u001b[K" in out && "\u001b[3;1H\u001b[K" in out)
    }

    @Test
    fun `a grid of nothing is no bytes`() {
        assertEquals(0, meshScreenBytes(screen("x"), 0, 1).size)
        assertEquals(0, meshScreenBytes(screen("x"), 10, 0).size)
    }

    @Test
    fun `a row cannot carry an escape of its own, so the host cannot reach the clipboard`() {
        val hostile = "a\u001b]52;c;ZXZpbA==\u0007b\u009b31mc\u007fd\u0000e\r\n"
        val row = cleanRow(hostile, 80)
        assertTrue(row.none { it.code < 0x20 || it.code == 0x7f || it.code in 0x80..0x9f }, row)
        // The only ESC bytes in the whole stream are the sequences this file writes.
        val out = text(screen(hostile), cols = 80)
        val escapes = Regex("\u001b(?!\\[)").findAll(out).count()
        assertEquals(0, escapes, "every ESC is the start of a CSI this helper wrote")
        assertTrue("]52" in out, "the text survives as text, which is harmless")
    }

    @Test
    fun `characters outside the basic plane are kept whole`() {
        // A surrogate pair is one character and must never be split: half of one is not text.
        val out = cleanRow("\uD83D\uDE00x", 4)
        assertEquals("\uD83D\uDE00x", out)
        assertEquals(3, out.length, "the pair survives as two UTF-16 units")
    }

    @Test
    fun `a wide character costs two cells, which is what keeps a row inside the grid`() {
        // Counting code points let a row of ideographs reach the grid one cell too wide, and with
        // autowrap off the last cell was then overwritten on top of itself. Cells, not characters:
        // four cells hold two ideographs, and the third does not fit and is left out whole.
        assertEquals("\u4F60\u597D", cleanRow("\u4F60\u597D\u4E16", 4))
        assertEquals("\u4F60", cleanRow("\u4F60\u597D\u4E16", 3), "a wide glyph is never cut in half")
        // An emoji takes two cells for the same reason.
        assertEquals("\uD83D\uDE00", cleanRow("\uD83D\uDE00\uD83D\uDE00bcd", 3))
    }

    @Test
    fun `a combining mark takes no cell at all`() {
        // "e" + combining acute is one cell, so a two-cell row holds two of them.
        val composed = "e\u0301e\u0301e\u0301"
        assertEquals("e\u0301e\u0301", cleanRow(composed, 2))
    }

    // ── the unsent draft, drawn on the row the cursor is on ────────────────

    @Test
    fun `a draft's line ends and tabs are glyphs, never control characters`() {
        // The reason this function exists: a `\n` written into the engine on the last row scrolls
        // the whole screen, so the operator's typing pushed the shell's own output up a line before
        // the command had been sent.
        assertEquals("ls${ENTER_GLYPH}", draftEcho("ls\r\n", 0, 20))
        assertEquals("5${TAB_GLYPH}x", draftEcho("5\tx", 0, 20))
        assertTrue(draftEcho("a\nb", 0, 20).none { it.code < 0x20 })
    }

    @Test
    fun `a draft longer than the row keeps its end and says that the front is cut`() {
        // 20 columns, cursor at column 0, so 20 cells are available and one of them is the ellipsis.
        val draft = (1..120).map { ('a'.code + it % 26).toChar() }.joinToString("")
        val drawn = draftEcho(draft, 0, 20)
        assertEquals(20, displayWidthOf(drawn), "exactly the room there is")
        assertTrue(drawn.startsWith(ELLIPSIS), "the front is what was dropped")
        assertEquals(draft.takeLast(19), drawn.drop(1), "the end of the draft is what stays visible")
    }

    @Test
    fun `the window starts at the cursor, not at column zero`() {
        // The echo is written where the engine's cursor already is, so a prompt of 12 cells leaves
        // 8 on the row. Short enough, and it is drawn as it is with no ellipsis at all.
        assertEquals("abcdef", draftEcho("abcdef", 12, 20))
        val drawn = draftEcho("abcdefghijkl", 12, 20)
        assertEquals(8, displayWidthOf(drawn), "eight cells are left, and the ellipsis spends one")
        assertEquals(ELLIPSIS + "fghijkl", drawn)
    }

    @Test
    fun `a window never splits a wide glyph, even when the room is odd`() {
        val drawn = draftEcho("\u4F60\u597D\u4E16\u754C", 0, 5)
        // 5 cells: the ellipsis plus two ideographs is 5, so it keeps the last two whole ones.
        assertEquals(ELLIPSIS + "\u4E16\u754C", drawn)
        assertEquals(5, displayWidthOf(drawn))
    }

    @Test
    fun `a backspace in the draft erases the cell before it`() {
        assertEquals("a", draftEcho("ab\b \b", 0, 20), "the b, the space and the second backspace")
        // And it cannot erase past the start of the draft.
        assertEquals("", draftEcho("\b\b", 0, 20))
    }

    @Test
    fun `a draft cannot carry an escape of its own`() {
        // The draft is a paste's text; a hostile `ESC[2J` in it would clear the phone's copy of the
        // host's screen, and an OSC 52 would reach the clipboard hook.
        val drawn = draftEcho("a\u001b]52;c;ZXZpbA==\u0007b", 0, 40)
        assertEquals("a]52;c;ZXZpbA==b", drawn)
    }

    @Test
    fun `nothing visible draws nothing at all`() {
        assertEquals("", draftEcho("", 0, 20))
        // A lone arrow key renders as nothing (MeshInput shares that decision), and an empty echo
        // must leave the engine's pen alone rather than open and close a faint run for no text.
        assertEquals("", draftEcho("\u0001\u0002", 0, 20))
    }
}
