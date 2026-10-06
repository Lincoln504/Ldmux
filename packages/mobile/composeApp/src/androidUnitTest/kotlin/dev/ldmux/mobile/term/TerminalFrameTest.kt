package dev.ldmux.mobile.term

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
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
        known: Boolean = true,
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
        repeat(7) { byte(0) } // selection: flag + two 16-bit columns + two rows
        byte(if (known) 1 else 0)
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
        assertEquals(48, TerminalFrame.HEADER_SIZE)
        assertEquals(TerminalFrame.HEADER_SIZE, frame(1, 1, listOf(Triple("x", 1, 2))).size - 10)
    }

    @Test
    fun `cursorKnown is decoded separately from cursorVisible`() {
        // The measured `pi` case: a hidden cursor whose position is still real, so
        // the client must not draw a hardware cursor and must still be able to walk
        // to it. And the case that must not be walked to at all.
        val pi = TerminalFrame.decode(frame(1, 1, listOf(Triple(" ", 0, 0)), visible = false, known = true))
        assertFalse(pi.cursorVisible)
        assertTrue(pi.cursorKnown)
        val lost = TerminalFrame.decode(frame(1, 1, listOf(Triple(" ", 0, 0)), known = false))
        assertFalse(lost.cursorKnown)
    }

    @Test
    fun `reverse video swaps the two channels, and concealment hides the text`() {
        // Two operations, not one, and a client that collapses them draws reverse
        // video as a solid bar with its own text hidden inside it. This is the
        // difference between `pi`'s cursor being visible and being a white rectangle.
        val plain = TerminalFrame.Cell("x", fg = 0xFFFFFF, bg = 0x000000, flags = 0)
        assertEquals(0xFFFFFF, plain.drawnFg)
        assertEquals(0x000000, plain.drawnBg)

        val reversed = TerminalFrame.Cell("x", 0xFFFFFF, 0x000000, TerminalFrame.FLAG_INVERSE)
        assertEquals(0x000000, reversed.drawnFg)
        assertEquals(0xFFFFFF, reversed.drawnBg)

        val hidden = TerminalFrame.Cell("x", 0xFFFFFF, 0x000000, TerminalFrame.FLAG_INVISIBLE)
        assertEquals(0x000000, hidden.drawnFg)
        assertEquals(0x000000, hidden.drawnBg)

        // Concealment wins over the swap: a withheld character must not come back
        // because the cell was also reversed.
        val both = TerminalFrame.Cell(
            "x",
            0xFFFFFF,
            0x000000,
            TerminalFrame.FLAG_INVERSE or TerminalFrame.FLAG_INVISIBLE,
        )
        assertEquals(both.drawnBg, both.drawnFg)
    }

    @Test
    fun `a block cursor can always be told apart from the glyph it covers`() {
        // The cursor fills the cell with `drawnFg` and repaints the glyph in
        // `drawnBg`, so those two being equal is a cursor with the character inside
        // it erased — which is exactly what filling with the default foreground did
        // on a reverse-video cell. Concealed text is the one case where they are
        // deliberately the same.
        val plain = TerminalFrame.Cell("x", fg = 0xF0F0F0, bg = 0x101010, flags = 0)
        assertTrue(plain.drawnFg != plain.drawnBg)

        val reversed = TerminalFrame.Cell("x", 0xF0F0F0, 0x101010, TerminalFrame.FLAG_INVERSE)
        assertTrue(reversed.drawnFg != reversed.drawnBg)

        // The ordinary `pi` cursor: reverse video over the default colours.
        val cursor = TerminalFrame.Cell(" ", 0xF0F0F0, 0x101010, TerminalFrame.FLAG_INVERSE)
        assertTrue(cursor.inverse)
        assertEquals(0x101010, cursor.drawnFg)
        assertEquals(0xF0F0F0, cursor.drawnBg)

        val hidden = TerminalFrame.Cell("x", 0xF0F0F0, 0x101010, TerminalFrame.FLAG_INVISIBLE)
        assertEquals(hidden.drawnFg, hidden.drawnBg)
    }

    @Test
    fun `an inverse blank cell is a filled block, which is what pi's cursor is`() {
        // pi's cursor is reverse video over a space: no glyph, and the cell's own
        // foreground painted as the background. If `drawnBg` is not the foreground,
        // the cell is drawn as nothing at all and the cursor is invisible.
        val cursor = TerminalFrame.Cell(" ", fg = 0xFFFFFF, bg = 0x000000, TerminalFrame.FLAG_INVERSE)
        assertTrue(cursor.inverse)
        assertTrue(cursor.text.isBlank())
        assertEquals(0xFFFFFF, cursor.drawnBg)
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
            repeat(15) { byte(0) } // selection (7), cursorKnown, reserved
            cell("x", 1, 2, TerminalFrame.FLAG_BOLD or TerminalFrame.FLAG_UNDERLINE)
        }.build()
        val f = TerminalFrame.decode(bytes)
        assertTrue(f.cells[0].bold)
        assertTrue(f.cells[0].underline)
        assertFalse(f.cells[0].italic)
    }

    @Test
    fun `a selected cell is selected and not silently something else`() {
        val bytes = Builder().apply {
            u16(3); u16(1); u16(0); u16(0)
            byte(1); byte(1)
            rgb(0); rgb(0)
            byte(0); byte(1); byte(0)
            u32(0); u32(0); u32(0)
            byte(0)
            byte(0)
            repeat(15) { byte(0) } // selection (7), cursorKnown, reserved
            cell("x", 1, 2, TerminalFrame.FLAG_SELECTED)
            cell("y", 1, 2, TerminalFrame.FLAG_SELECTED or TerminalFrame.FLAG_BOLD)
            cell("z", 1, 2, TerminalFrame.FLAG_BOLD)
        }.build()
        val f = TerminalFrame.decode(bytes)
        assertTrue(f.cells[0].selected)
        assertFalse(f.cells[0].bold)
        assertTrue(f.cells[1].selected)
        assertTrue(f.cells[1].bold)
        assertFalse(f.cells[2].selected)
        assertTrue(f.cells[2].bold)
    }

    @Test
    fun `decodes the selection endpoints from the bytes that were reserved`() {
        val bytes = Builder().apply {
            u16(1); u16(1); u16(0); u16(0)
            byte(1); byte(1)
            rgb(0); rgb(0)
            byte(0); byte(1); byte(0)
            u32(0); u32(0); u32(0)
            byte(0)
            byte(0)
            byte(1) // selection on
            u16(3); byte(1) // start col 3, row 1
            u16(9); byte(2) // end col 9, row 2
            repeat(8) { byte(0) } // cursorKnown, then the rest of the reserved run
            cell("x", 1, 2)
        }.build()
        val f = TerminalFrame.decode(bytes)
        val sel = f.selection
        assertNotNull(sel)
        assertEquals(3, sel.startCol)
        assertEquals(1, sel.startRow)
        assertEquals(9, sel.endCol)
        assertEquals(2, sel.endRow)
        // Cells still decode correctly past the new bytes: the endpoints ride in the
        // header and nothing after it moved.
        assertEquals("x", f.cells[0].text)
    }

    @Test
    fun `no selection is null and not an empty range`() {
        // The difference matters: an empty range would draw two handles on top of each
        // other at (0,0), and a tap there would have to be distinguished from a tap
        // anywhere else on the screen.
        assertNull(TerminalFrame.decode(frame(1, 1, listOf(Triple("x", 1, 2)))).selection)
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

    // ---- frame version 1: the additive extension ----

    /** A version-1 frame: [cells] are (text, flags) with white-on-black colours. */
    private fun v1(
        cols: Int,
        rows: Int,
        cells: List<Pair<String, Int>>,
        ext: ByteArray = ByteArray(0),
        version: Int = 1,
        selRowHi: Pair<Int, Int> = 0 to 0,
        declaredExtLen: Int = ext.size,
    ): ByteArray = Builder().apply {
        u16(cols); u16(rows); u16(0); u16(0)
        byte(1); byte(1)
        rgb(0xFFFFFF); rgb(0)
        byte(0); byte(1); byte(0)
        u32(0); u32(0); u32(0)
        byte(0); byte(0)
        byte(0); u16(0); byte(0); u16(0); byte(0) // selection (7 bytes)
        byte(1)                                   // cursorKnown
        byte(version)
        u32(declaredExtLen.toLong())
        byte(selRowHi.first); byte(selRowHi.second)
        for ((t, f) in cells) cell(t, 0xFFFFFF, 0, f)
    }.build() + ext

    private fun le32(v: Int) = byteArrayOf(v.toByte(), (v shr 8).toByte(), (v shr 16).toByte(), (v shr 24).toByte())
    private fun le16(v: Int) = byteArrayOf(v.toByte(), (v shr 8).toByte())

    private fun section(kind: Int, payload: ByteArray) = byteArrayOf(kind.toByte()) + le32(payload.size) + payload

    private fun underlineColours(vararg e: Pair<Int, Int>): ByteArray =
        section(
            1,
            e.fold(ByteArray(0)) { acc, (idx, rgb) ->
                acc + le32(idx) + byteArrayOf((rgb shr 16).toByte(), (rgb shr 8).toByte(), rgb.toByte())
            },
        )

    private fun links(uris: List<String>, refs: List<Pair<Int, Int>>): ByteArray {
        var payload = le16(uris.size)
        for (u in uris) payload += le16(u.encodeToByteArray().size) + u.encodeToByteArray()
        payload += le32(refs.size)
        for ((cell, uri) in refs) payload += le32(cell) + le16(uri)
        return section(2, payload)
    }

    private fun style(n: Int) = TerminalFrame.FLAG_UNDERLINE or (n shl TerminalFrame.UNDERLINE_STYLE_SHIFT)

    @Test
    fun `the header sizes and the cluster cap are the ones the C core documents`() {
        assertEquals(48, TerminalFrame.HEADER_SIZE)
        assertEquals(9, TerminalFrame.CELL_FIXED)
        assertEquals(64, TerminalFrame.CELL_MAX_UTF8)
        assertEquals(1, TerminalFrame.FRAME_VERSION)
    }

    @Test
    fun `underline style is read from bits 12 to 14 and colour from the extension`() {
        val f = TerminalFrame.decode(
            v1(
                5, 1,
                listOf("a" to style(1), "b" to style(2), "c" to style(3), "d" to style(4), "e" to style(5)),
                ext = underlineColours(2 to 0xFF8800),
            ),
        )
        assertEquals(listOf(1, 2, 3, 4, 5), f.cells.map { it.underlineStyle })
        assertEquals(TerminalFrame.UNDERLINE_CURLY, f.cells[2].underlineStyle)
        assertEquals(0xFF8800, f.cells[2].underlineColor)
        assertEquals(0xFF8800, f.cells[2].drawnUnderlineColor)
        assertNull(f.cells[0].underlineColor)
        // No colour of its own: the line is drawn in the text's colour.
        assertEquals(f.cells[0].drawnFg, f.cells[0].drawnUnderlineColor)
    }

    @Test
    fun `a bare underline bit is a single underline and a reserved style degrades to single`() {
        // The old writer: bit 2 only. And a style value this decoder does not know.
        val f = TerminalFrame.decode(
            v1(3, 1, listOf("a" to TerminalFrame.FLAG_UNDERLINE, "b" to style(7), "c" to 0)),
        )
        assertEquals(TerminalFrame.UNDERLINE_SINGLE, f.cells[0].underlineStyle)
        assertEquals(TerminalFrame.UNDERLINE_SINGLE, f.cells[1].underlineStyle)
        assertEquals(TerminalFrame.UNDERLINE_NONE, f.cells[2].underlineStyle)
        assertTrue(f.cells[0].underline && f.cells[1].underline)
    }

    @Test
    fun `wide head and spacer tail are distinct from a blank`() {
        val f = TerminalFrame.decode(
            v1(
                3, 1,
                listOf(
                    "\u4f60" to TerminalFrame.FLAG_WIDE,
                    "" to TerminalFrame.FLAG_SPACER_TAIL,
                    "" to 0,
                ),
            ),
        )
        assertTrue(f.cells[0].wide)
        assertTrue(f.cells[1].spacerTail)
        assertFalse(f.cells[1].wide)
        assertFalse(f.cells[2].spacerTail, "a plain blank is not a tail")
    }

    @Test
    fun `hyperlinks resolve to their URIs, shared and per cell`() {
        val h = TerminalFrame.FLAG_HYPERLINK
        val f = TerminalFrame.decode(
            v1(
                4, 1,
                listOf("a" to h, "b" to h, "c" to 0, "d" to h),
                ext = links(listOf("https://a.example/", "https://b.example/\u00e9"), listOf(0 to 0, 1 to 0, 3 to 1)),
            ),
        )
        assertEquals("https://a.example/", f.cells[0].hyperlink)
        assertEquals("https://a.example/", f.cells[1].hyperlink)
        assertNull(f.cells[2].hyperlink)
        assertEquals("https://b.example/\u00e9", f.cells[3].hyperlink)
    }

    @Test
    fun `a hyperlink flag with no URI behind it is not a link`() {
        val f = TerminalFrame.decode(v1(1, 1, listOf("a" to TerminalFrame.FLAG_HYPERLINK)))
        assertNull(f.cells[0].hyperlink)
    }

    @Test
    fun `unknown flag bits and unknown extension sections are ignored`() {
        // Bits 6 and 12..14 are meaningful, so use a section kind from the future
        // ahead of a real one: the unknown one is skipped by its length.
        val ext = section(99, ByteArray(13) { 7 }) + underlineColours(0 to 0x123456)
        val f = TerminalFrame.decode(v1(1, 1, listOf("a" to style(3)), ext = ext))
        assertEquals(0x123456, f.cells[0].underlineColor)
    }

    @Test
    fun `a newer frame version keeps its cells and loses only the extension`() {
        val ext = underlineColours(0 to 0x123456)
        val f = TerminalFrame.decode(v1(1, 1, listOf("a" to style(3)), ext = ext, version = 2))
        assertEquals("a", f.cells[0].text)
        assertEquals(TerminalFrame.UNDERLINE_CURLY, f.cells[0].underlineStyle)
        assertNull(f.cells[0].underlineColor, "a version this decoder does not know is not parsed")
        assertEquals(2, f.version)
    }

    @Test
    fun `a legacy version 0 frame is decoded as having no extension`() {
        // The pre-extension writer left bytes 41..47 zero; garbage after the cells
        // must not be parsed as one.
        val f = TerminalFrame.decode(v1(1, 1, listOf("a" to 0), ext = byteArrayOf(1, 9, 9, 9, 9, 9), version = 0, declaredExtLen = 0))
        assertEquals(0, f.version)
        assertNull(f.cells[0].underlineColor)
    }

    @Test
    fun `malformed extensions never throw and never mis-colour`() {
        val cases = listOf(
            // a section longer than the block
            byteArrayOf(1) + le32(1000) + ByteArray(7),
            // a truncated section header
            byteArrayOf(1, 0, 0),
            // a cell index past the grid
            underlineColours(500 to 0xFF0000),
            // a link table claiming more URIs than exist
            section(2, le16(60000)),
            // a ref to a URI that is not there
            links(listOf("x"), listOf(0 to 5)),
        )
        for (ext in cases) {
            val f = TerminalFrame.decode(v1(1, 1, listOf("a" to style(1)), ext = ext))
            assertEquals("a", f.cells[0].text)
            assertNull(f.cells[0].underlineColor)
            assertNull(f.cells[0].hyperlink)
        }
        // And a declared length beyond the buffer disables the block altogether.
        val f = TerminalFrame.decode(v1(1, 1, listOf("a" to 0), ext = underlineColours(0 to 1), declaredExtLen = 99999))
        assertNull(f.cells[0].underlineColor)
    }

    @Test
    fun `a cluster up to the cap decodes whole`() {
        // ZWJ family: man, ZWJ, woman, ZWJ, girl, ZWJ, boy = 25 bytes, which the old
        // 16-byte cap cut in the middle of the third emoji.
        val family = "\uD83D\uDC68\u200D\uD83D\uDC69\u200D\uD83D\uDC67\u200D\uD83D\uDC66"
        assertEquals(25, family.encodeToByteArray().size)
        val f = TerminalFrame.decode(v1(1, 1, listOf(family to TerminalFrame.FLAG_WIDE)))
        assertEquals(family, f.cells[0].text)
    }

    @Test
    fun `selection rows above 255 use the high bytes`() {
        val bytes = v1(1, 1, listOf("x" to 0), selRowHi = 1 to 2)
        bytes[33] = 1                    // selection on
        bytes[36] = 5; bytes[39] = 7     // low bytes of the rows
        val sel = TerminalFrame.decode(bytes).selection
        assertNotNull(sel)
        assertEquals(256 + 5, sel.startRow)
        assertEquals(512 + 7, sel.endRow)
        // A frame that predates the high bytes has zeros there: rows are unchanged.
        val old = frame(1, 1, listOf(Triple("x", 1, 2)))
        old[33] = 1; old[36] = 9; old[39] = 11
        assertEquals(9, TerminalFrame.decode(old).selection!!.startRow)
    }
}
