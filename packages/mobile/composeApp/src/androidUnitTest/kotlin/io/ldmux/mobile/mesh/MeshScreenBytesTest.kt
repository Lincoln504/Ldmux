package io.ldmux.mobile.mesh

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
    fun `characters outside the basic plane are kept whole and count once toward the grid`() {
        assertEquals("\uD83D\uDE00\uD83D\uDE00b", cleanRow("\uD83D\uDE00\uD83D\uDE00bcd", 3))
    }
}
