package dev.ldmux.mobile.term

/**
 * Decoder for the frame produced by libghostty-vt (native/ldmux_terminal.c).
 * Pure Kotlin so it is unit-testable without a device.
 *
 * Layout (little-endian):
 *   header (48 bytes): u16 cols, u16 rows, u16 cursorX, u16 cursorY,
 *                      u8 cursorVisible, u8 cursorStyle,
 *                      u8 defFgR, u8 defFgG, u8 defFgB,
 *                      u8 defBgR, u8 defBgG, u8 defBgB,
 *                      u8 passwordInput, u8 viewportActive, u8 mouseTracking,
 *                      u32 scrollTotal, u32 scrollOffset, u32 scrollLen,
 *                      u8 mouseSgr, u8 alternateOn,
 *                      u8 selectionOn, u16 selStartCol, u8 selStartRow,
 *                      u16 selEndCol, u8 selEndRow,
 *                      u8 cursorKnown, u8 reserved[7]
 *   then cols*rows cell records, row-major:
 *                      u8 utf8Len, u8 flagsLo, u8 flagsHi,
 *                      u8 fgR, u8 fgG, u8 fgB, u8 bgR, u8 bgG, u8 bgB,
 *                      u8 utf8[utf8Len]
 *   flags: bit0 bold, bit1 italic, bit2 underline, bit3 faint, bit4 blink,
 *          bit5 inverse, bit6 invisible, bit7 strikethrough, bit8 overline,
 *          bit9 selected
 */
object TerminalFrame {
    /** Bytes of frame header preceding the cell records. */
    const val HEADER_SIZE = 48
    const val FLAG_BOLD = 1 shl 0
    const val FLAG_ITALIC = 1 shl 1
    const val FLAG_UNDERLINE = 1 shl 2
    const val FLAG_FAINT = 1 shl 3
    const val FLAG_BLINK = 1 shl 4
    const val FLAG_INVERSE = 1 shl 5
    const val FLAG_INVISIBLE = 1 shl 6
    const val FLAG_STRIKETHROUGH = 1 shl 7
    const val FLAG_OVERLINE = 1 shl 8

    /**
     * The cell is inside the selection.
     *
     * The ninth flag bit, which was free, rather than a new header field: a
     * selection is a property of cells, the client already decodes this word for
     * every cell it draws, and an older client ignores the bit instead of
     * misreading the frame. No version bump and nothing on the wire.
     */
    const val FLAG_SELECTED = 1 shl 9

    class Cell(
        /** The cell's text (a grapheme, may be empty for a blank cell). */
        val text: String,
        val fg: Int,
        val bg: Int,
        val flags: Int,
    ) {
        val selected get() = flags and FLAG_SELECTED != 0
        val bold get() = flags and FLAG_BOLD != 0
        val italic get() = flags and FLAG_ITALIC != 0
        val underline get() = flags and FLAG_UNDERLINE != 0

        /**
         * Reverse video (`ESC[7m`): draw the cell foreground on the cell
         * background, swapped.
         *
         * The flag was in the wire format from the first version and read by
         * nobody, which is why `pi`'s cursor was invisible: `pi` draws its own
         * cursor as an inverse-video cell (`ESC[7m` + the character under it +
         * `ESC[27m`) rather than showing the hardware cursor, and an unrendered
         * inverse cell is indistinguishable from a plain one. On a blank cell —
         * the cursor at the end of typed text — it vanished completely, because
         * the painter skips a cell with no text.
         */
        val inverse get() = flags and FLAG_INVERSE != 0

        /**
         * The colour this cell's background is actually drawn in.
         *
         * Reverse video swaps the two channels, so this is the only correct way
         * to ask — reading [bg] directly is right for every cell except the ones
         * the swap exists for.
         */
        val drawnBg: Int get() = if (inverse) fg else bg

        /**
         * The colour this cell's text is actually drawn in.
         *
         * Two operations, and they are not the same one: reverse video swaps,
         * concealment paints the text in whatever the background is so the
         * character is in the buffer and not on the screen. Treating them as one
         * (which the first version did) drew reverse video as a solid bar with its
         * own text hidden inside it.
         */
        val drawnFg: Int
            get() = when {
                invisible -> drawnBg
                inverse -> bg
                else -> fg
            }

        /**
         * Concealed text (`ESC[8m`): the character is in the buffer and must not
         * be shown. Not the same operation as [inverse], which swaps rather than
         * hides — see [drawnFg].
         */
        val invisible get() = flags and FLAG_INVISIBLE != 0
    }

    class Frame(
        val cols: Int,
        val rows: Int,
        val cursorX: Int,
        val cursorY: Int,
        val cursorVisible: Boolean,
        val cursorStyle: Int,
        /**
         * Whether [cursorX]/[cursorY] are real.
         *
         * Separate from [cursorVisible] because they are separate facts: the
         * program asks for a cursor or not, and the emulator either knows where
         * it is or reports coordinates that "must not be read". `pi` hides the
         * hardware cursor — so [cursorVisible] is false — while still tracking
         * its own input position exactly, which is what makes tap-to-cursor
         * meaningful there and why the tap policy asks this, not that.
         */
        val cursorKnown: Boolean,
        /** The terminal's default foreground, as 0xRRGGBB. */
        val defaultFg: Int,
        /** The terminal's default background, as 0xRRGGBB. */
        val defaultBg: Int,
        /**
         * The cursor is at a field that will not echo (a password prompt).
         * Input must not be shown before the server confirms it.
         */
        val passwordInput: Boolean,
        /** True when the viewport is following the live output, not history. */
        val viewportActive: Boolean,
        /**
         * The program is tracking the mouse (mode 1000/1002/1003).
         *
         * Read from the emulator, not from a server snapshot, because a program
         * can enable this at any moment and the emulator is what saw the escape
         * sequence — so it is current on the frame right after it. Waiting for a
         * snapshot meant a program that had just asked for the mouse was still
         * sent cursor keys.
         */
        val mouseTracking: Boolean,
        /** SGR mouse encoding (1006) is on: the only form we synthesize. */
        val mouseSgr: Boolean,
        /**
         * The alternate screen is active — `vim`, `less`, `htop`, `nano`, `top`.
         *
         * This doc has now claimed two different things and neither survived
         * measurement. It first said the opposite of the paragraph above; it then
         * claimed `pi` emits `ESC[?1000h ?1002h ?1003h ?1004h ?1006h ?1049h`.
         * Captured from a real pane on 2026-09-27, `pi` 0.87.1 in its default
         * `regular` mode emits **none** of them: `alternate_on`,
         * `mouse_any_flag` and `mouse_sgr_flag` all read 0, and it draws its
         * composer on the primary screen. It takes the alternate screen only in
         * its `fullscreen` `tuiMode`. So in the default configuration this bit
         * does separate a composer from `vim`/`less`/`htop`/`nano` — but nothing
         * should be built on it in either direction without asking the emulator.
         * Read from the emulator for the same freshness reason as
         * [mouseTracking].
         */
        val alternateOn: Boolean,
        /**
         * Where the selection is, in viewport cells, inclusive on both ends — or
         * null when nothing is selected.
         *
         * It rides in bytes that were reserved, so a client that does not know
         * about it reads a zero flag and draws no handles rather than misreading the
         * frame. The engine reports the rows, rather than the client remembering
         * what it asked for, because a long press turns a touch into a *word* and
         * only the engine knows where that word ended.
         */
        val selection: SelectionBounds?,
        /** Scrollable area in rows: total, the viewport's offset, and its size. */
        val scrollTotal: Long,
        val scrollOffset: Long,
        val scrollLen: Long,
        /** Row-major, [rows] * [cols] cells. */
        val cells: List<Cell>,
    )

    /** The two ends of a selection, inclusive, in viewport cells. */
    data class SelectionBounds(
        val startCol: Int,
        val startRow: Int,
        val endCol: Int,
        val endRow: Int,
    ) {
        /** True when the range covers nothing, which is not a selection. */
        val isEmpty: Boolean
            get() = startRow == endRow && startCol == endCol
    }

    private fun u16(b: ByteArray, o: Int): Int =
        (b[o].toInt() and 0xff) or ((b[o + 1].toInt() and 0xff) shl 8)

    private fun u32(b: ByteArray, o: Int): Long =
        ((b[o].toInt() and 0xff).toLong()) or
            ((b[o + 1].toInt() and 0xff).toLong() shl 8) or
            ((b[o + 2].toInt() and 0xff).toLong() shl 16) or
            ((b[o + 3].toInt() and 0xff).toLong() shl 24)

    private fun rgb(b: ByteArray, o: Int): Int =
        ((b[o].toInt() and 0xff) shl 16) or
            ((b[o + 1].toInt() and 0xff) shl 8) or
            (b[o + 2].toInt() and 0xff)

    fun decode(b: ByteArray): Frame {
        require(b.size >= HEADER_SIZE) { "frame too short: ${b.size}" }
        val cols = u16(b, 0)
        val rows = u16(b, 2)
        val cursorX = u16(b, 4)
        val cursorY = u16(b, 6)
        val cursorVisible = b[8].toInt() != 0
        val cursorStyle = b[9].toInt() and 0xff
        val defaultFg = rgb(b, 10)
        val defaultBg = rgb(b, 13)
        val passwordInput = b[16].toInt() != 0
        val viewportActive = b[17].toInt() != 0
        val mouseTracking = b[18].toInt() != 0
        val mouseSgr = b[31].toInt() != 0
        val alternateOn = b[32].toInt() != 0
        // Bytes 33..39: the selection flag, then the two ends.
        // Byte 40: whether cursorX/cursorY may be read at all.
        val cursorKnown = b[40].toInt() != 0
        val selection = if (b[33].toInt() != 0) {
            SelectionBounds(
                startCol = u16(b, 34),
                startRow = b[36].toInt() and 0xff,
                endCol = u16(b, 37),
                endRow = b[39].toInt() and 0xff,
            )
        } else {
            null
        }
        val scrollTotal = u32(b, 19)
        val scrollOffset = u32(b, 23)
        val scrollLen = u32(b, 27)

        // How many cells the buffer can possibly describe. A cell record is at least
        // 9 bytes, so anything the header claims beyond that is a claim about a frame
        // that did not arrive — and without this the header's own numbers are the
        // array's *capacity*: `cols` and `rows` are `u16`, so their product overflows
        // an Int (65535 * 65535 is negative, and `ArrayList(negative)` throws) or
        // allocates for a frame that is nowhere in the buffer. The loop below already
        // refused to read past the end; it just did it after the allocation.
        val fits = (b.size - HEADER_SIZE) / 9
        val cellCount = minOf(cols * rows, fits)
        val cells = ArrayList<Cell>(cellCount.coerceAtLeast(0))
        var p = HEADER_SIZE
        for (i in 0 until cellCount) {
            if (p + 9 > b.size) break
            val len = b[p].toInt() and 0xff
            val flags = (b[p + 1].toInt() and 0xff) or ((b[p + 2].toInt() and 0xff) shl 8)
            val fg = rgb(b, p + 3)
            val bg = rgb(b, p + 6)
            p += 9
            val text = if (len > 0 && p + len <= b.size) {
                b.decodeToString(p, p + len)
            } else {
                ""
            }
            p += len
            cells.add(Cell(text, fg, bg, flags))
        }
        return Frame(
            cols,
            rows,
            cursorX,
            cursorY,
            cursorVisible,
            cursorStyle,
            cursorKnown,
            defaultFg,
            defaultBg,
            passwordInput,
            viewportActive,
            mouseTracking,
            mouseSgr,
            alternateOn,
            selection,
            scrollTotal,
            scrollOffset,
            scrollLen,
            cells,
        )
    }
}