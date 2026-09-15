package dev.ldmux.mobile.term

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The frame format is the contract between the JNI shim (native/ldmux_terminal.c)
 * and the renderer. It is hand-rolled on both sides, so the two can drift
 * silently: these tests pin the exact byte layout, including the header's
 * default colours, which the surface uses to paint the letterbox.
 */
class TerminalFrameTest {

    private class Builder {
        private val out = ArrayList<Byte>()

        fun u16(v: Int) {
            out.add((v and 0xff).toByte())
            out.add(((v shr 8) and 0xff).toByte())
        }

        fun rgb(v: Int) {
            out.add(((v shr 16) and 0xff).toByte())
            out.add(((v shr 8) and 0xff).toByte())
            out.add((v and 0xff).toByte())
        }

        fun u32(v: Long) {
            out.add((v and 0xff).toByte())
            out.add(((v shr 8) and 0xff).toByte())
            out.add(((v shr 16) and 0xff).toByte())
            out.add(((v shr 24) and 0xff).toByte())
        }

        fun byte(v: Int) = out.add(v.toByte())

        fun cell(text: String, fg: Int, bg: Int, flags: Int = 0) {
            val bytes = text.encodeToByteArray()
            byte(bytes.size)
            byte(flags and 0xff)
            byte((flags shr 8) and 0xff)
            rgb(fg)
            rgb(bg)
            for (b in bytes) out.add(b)
        }

        fun build(): ByteArray = out.toByteArray()
    }

    /** Build a frame exactly as the C encoder does (little-endian). */
    private fun frame(
        cols: Int,
        rows: Int,
        cells: List<Triple<String, Int, Int>>,
        cursorX: Int = 0,
        cursorY: Int = 0,
        visible: Boolean = true,
        style: Int = 1,
        defFg: Int = 0xE6E6E6,
        defBg: Int = 0x1E1E1E,
        passwordInput: Boolean = false,
        viewportActive: Boolean = true,
        mouseTracking: Boolean = false,
        mouseSgr: Boolean = false,
        alternateOn: Boolean = false,
        scrollTotal: Long = 0,
        scrollOffset: Long = 0,
        scrollLen: Long = 0,
    ): ByteArray = Builder().apply {
        u16(cols)
        u16(rows)
        u16(cursorX)
        u16(cursorY)
        byte(if (visible) 1 else 0)
        byte(style)
        rgb(defFg)
        rgb(defBg)
        byte(if (passwordInput) 1 else 0)
        byte(if (viewportActive) 1 else 0)
        byte(if (mouseTracking) 1 else 0)
        u32(scrollTotal)
        u32(scrollOffset)
        u32(scrollLen)
        byte(if (mouseSgr) 1 else 0)
        byte(if (alternateOn) 1 else 0)
        repeat(7) { byte(0) } // reserved
        for ((text, fg, bg) in cells) cell(text, fg, bg)
    }.build()

    @Test
    fun `decodes the header, including the default colours`() {
        val f = TerminalFrame.decode(frame(2, 1, listOf(Triple("a", 0x111111, 0x222222), Triple("", 0x333333, 0x444444))))
        assertEquals(2, f.cols)
        assertEquals(1, f.rows)
        assertEquals(0xE6E6E6, f.defaultFg)
        assertEquals(0x1E1E1E, f.defaultBg)
        // The C side and this decoder must agree on where the cells start.
        assertEquals(40, TerminalFrame.HEADER_SIZE)
        assertEquals(TerminalFrame.HEADER_SIZE, frame(1, 1, listOf(Triple("x", 1, 2))).size - 10)
    }

    @Test
    fun `decodes the password flag and the scroll geometry`() {
        val f = TerminalFrame.decode(
            frame(
                1,
                1,
                listOf(Triple("x", 1, 2)),
                passwordInput = true,
                viewportActive = false,
                scrollTotal = 5000,
                scrollOffset = 1234,
                scrollLen = 29,
            ),
        )
        assertTrue(f.passwordInput, "the emulator knows, so nothing has to guess")
        assertFalse(f.viewportActive)
        assertEquals(5000L, f.scrollTotal)
        assertEquals(1234L, f.scrollOffset)
        assertEquals(29L, f.scrollLen)
    }

    @Test
    fun `decodes the mouse modes, which are what decide click versus cursor keys`() {
        // Both, and independently: a program can turn mouse reporting on with a
        // legacy encoding we cannot synthesize, and a tap must fall back to
        // cursor keys then rather than write bytes the program will misread.
        val both = TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2)), mouseTracking = true, mouseSgr = true))
        assertTrue(both.mouseTracking)
        assertTrue(both.mouseSgr)

        val legacy = TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2)), mouseTracking = true))
        assertTrue(legacy.mouseTracking)
        assertFalse(legacy.mouseSgr, "tracking without SGR is not clickable")

        val none = TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2))))
        assertFalse(none.mouseTracking)
        assertFalse(none.mouseSgr)
    }

    @Test
    fun `decodes cells row-major with colour and text`() {
        val f = TerminalFrame.decode(
            frame(
                3,
                2,
                listOf(
                    Triple("a", 0x010203, 0x040506),
                    Triple("", 0x000000, 0x000000),
                    Triple("c", 0x0a0b0c, 0x0d0e0f),
                    Triple("d", 0x111111, 0x222222),
                    Triple("e", 0x333333, 0x444444),
                    Triple("f", 0x555555, 0x666666),
                ),
                cursorX = 2,
                cursorY = 1,
            ),
        )
        assertEquals(6, f.cells.size)
        assertEquals(2, f.cursorX)
        assertEquals(1, f.cursorY)
        assertEquals("a", f.cells[0].text)
        assertEquals(0x010203, f.cells[0].fg)
        assertEquals(0x040506, f.cells[0].bg)
        assertEquals("", f.cells[1].text)
        assertEquals("c", f.cells[2].text)
        assertEquals("f", f.cells[5].text)
        assertEquals(0x666666, f.cells[5].bg)
    }

    @Test
    fun `decodes multi-byte graphemes by byte length, not character count`() {
        // The C side stores a UTF-8 byte count; decoding by characters would
        // desynchronise the whole frame on the first non-ASCII glyph. The
        // palette/benchmark glyphs are exactly that case.
        val f = TerminalFrame.decode(frame(2, 1, listOf(Triple("█▓▒░", 1, 2), Triple("é", 3, 4))))
        assertEquals("█▓▒░", f.cells[0].text)
        assertEquals("é", f.cells[1].text)
    }

    @Test
    fun `decodes style flags`() {
        val bytes = Builder().apply {
            u16(1); u16(1); u16(0); u16(0)
            byte(1); byte(1)
            rgb(0); rgb(0)
            byte(0); byte(1); byte(0) // password, viewport-active, mouse-tracking
            u32(0); u32(0); u32(0)
            byte(0) // mouse-sgr
            byte(0) // alternate screen
            repeat(7) { byte(0) } // reserved
            cell("x", 1, 2, TerminalFrame.FLAG_BOLD or TerminalFrame.FLAG_UNDERLINE)
        }.build()
        val f = TerminalFrame.decode(bytes)
        assertTrue(f.cells[0].bold)
        assertTrue(f.cells[0].underline)
        assertFalse(f.cells[0].italic)
    }

    @Test
    fun `decodes the alternate-screen flag, which is what keeps taps out of vim`() {
        assertTrue(TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2)), alternateOn = true)).alternateOn)
        assertFalse(TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2)))).alternateOn)
    }

    @Test
    fun `carries cursor visibility and style`() {
        val f = TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2)), visible = false, style = 3))
        assertFalse(f.cursorVisible)
        assertEquals(3, f.cursorStyle)
    }

    @Test
    fun `rejects a frame shorter than the header`() {
        assertFailsWith<IllegalArgumentException> { TerminalFrame.decode(ByteArray(31)) }
    }
}