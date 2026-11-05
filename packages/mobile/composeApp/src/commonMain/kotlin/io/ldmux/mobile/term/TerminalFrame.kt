package io.ldmux.mobile.term

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
 *                      u8 cursorKnown, u8 frameVersion, u32 extLen,
 *                      u8 selStartRowHi, u8 selEndRowHi
 *   then cols*rows cell records, row-major:
 *                      u8 utf8Len, u8 flagsLo, u8 flagsHi,
 *                      u8 fgR, u8 fgG, u8 fgB, u8 bgR, u8 bgG, u8 bgB,
 *                      u8 utf8[utf8Len]            (utf8Len <= CELL_MAX_UTF8)
 *   then extLen bytes of extension sections (absent when extLen = 0):
 *                      u8 kind, u32 len, u8 payload[len]
 *                      kind 1 underline colour: (u32 cell, u8 r, u8 g, u8 b)*
 *                      kind 2 hyperlinks: u16 n, n*(u16 len, uri bytes),
 *                                         u32 m, m*(u32 cell, u16 uriIndex)
 *   flags: bit0 bold, bit1 italic, bit2 underline (any style), bit3 faint,
 *          bit4 blink, bit5 inverse, bit6 invisible, bit7 strikethrough,
 *          bit8 overline, bit9 selected, bit10 wide, bit11 spacerTail,
 *          bits12..14 underline style (0 none, 1 single, 2 double, 3 curly,
 *          4 dotted, 5 dashed), bit15 hyperlink
 *
 * Compatibility, which is the point of the version byte: this decoder ignores flag
 * bits it does not know, ignores extension sections whose kind it does not know,
 * and ignores the whole extension block (still reading the cells) when
 * `frameVersion` is above [FRAME_VERSION]. A frame from before the extension has
 * version 0 and zeros in the bytes the extension now uses, and decodes as having
 * none. The authoritative description is `native/ldmux_terminal.h`.
 */
object TerminalFrame {
    /** Bytes of frame header preceding the cell records. */
    const val HEADER_SIZE = 48

    /** The newest frame version this decoder understands (see the class comment). */
    const val FRAME_VERSION = 1

    /** The fixed part of a cell record, before its text. */
    const val CELL_FIXED = 9

    /** The longest cell text the encoder emits, in bytes (the wire length is a u8). */
    const val CELL_MAX_UTF8 = 64
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

    /** Head of a double-width glyph; the next cell is its [FLAG_SPACER_TAIL]. */
    const val FLAG_WIDE = 1 shl 10

    /**
     * The tail of a double-width glyph: no text, and not a blank to draw into. The
     * head's glyph already covers this column.
     */
    const val FLAG_SPACER_TAIL = 1 shl 11

    /** The underline style is a 3-bit field at bits 12..14. */
    const val UNDERLINE_STYLE_SHIFT = 12
    const val UNDERLINE_STYLE_MASK = 7

    /** The cell is inside an OSC 8 hyperlink and [Cell.hyperlink] has its URI. */
    const val FLAG_HYPERLINK = 1 shl 15

    /** Values of [Cell.underlineStyle]; the same numbers as the engine's. */
    const val UNDERLINE_NONE = 0
    const val UNDERLINE_SINGLE = 1
    const val UNDERLINE_DOUBLE = 2
    const val UNDERLINE_CURLY = 3
    const val UNDERLINE_DOTTED = 4
    const val UNDERLINE_DASHED = 5

    class Cell(
        /** The cell's text (a grapheme, may be empty for a blank cell). */
        val text: String,
        val fg: Int,
        val bg: Int,
        val flags: Int,
        /**
         * The underline colour as 0xRRGGBB, or null for "use the foreground". Only
         * ever set on an underlined cell. It arrives in the frame's extension block,
         * so a frame without one has none.
         */
        val underlineColor: Int? = null,
        /**
         * The OSC 8 link target, or null. Non-null exactly when [FLAG_HYPERLINK] is
         * set *and* the URI was delivered; a flag whose URI was dropped (too long,
         * table full) reads as no link. Untrusted program output: show it before
         * following it.
         */
        val hyperlink: String? = null,
    ) {
        val selected get() = flags and FLAG_SELECTED != 0

        /** Head of a double-width glyph. */
        val wide get() = flags and FLAG_WIDE != 0

        /** Tail of a double-width glyph: paint nothing here. */
        val spacerTail get() = flags and FLAG_SPACER_TAIL != 0

        /**
         * One of the `UNDERLINE_*` values. A cell from a writer that predates the
         * style bits has [underline] set and a zero style field, which reads as
         * single, and a reserved value (6, 7) also reads as single.
         */
        val underlineStyle: Int
            get() {
                if (!underline) return UNDERLINE_NONE
                val v = (flags shr UNDERLINE_STYLE_SHIFT) and UNDERLINE_STYLE_MASK
                return if (v in UNDERLINE_SINGLE..UNDERLINE_DASHED) v else UNDERLINE_SINGLE
            }

        /** The colour the underline is drawn in: its own, else the text's. */
        val drawnUnderlineColor: Int get() = underlineColor ?: drawnFg

        internal fun withExtras(underlineColor: Int?, hyperlink: String?) =
            Cell(text, fg, bg, flags, underlineColor ?: this.underlineColor, hyperlink ?: this.hyperlink)

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
            get() {
                if (invisible) return drawnBg
                val text = if (inverse) bg else fg
                // Faint (`ESC[2m`) is a *rendering* attribute, and the engine hands it over as a
                // flag with the colour untouched: the probe in `native/` shows a faint cell
                // arriving with the same 255,255,255 foreground as a normal one. So half of the
                // attribute is the client's to draw, and it was not drawn at all — every program
                // that de-emphasises text (`ls` extracts, a `fzf` list, a diff's context) was
                // shown at full strength. Half-intensity is a blend halfway to the cell's own
                // background, which also does the right thing on an inverse cell, where the two
                // channels are already swapped.
                return if (faint) dimTowards(text, drawnBg) else text
            }

        /**
         * Concealed text (`ESC[8m`): the character is in the buffer and must not
         * be shown. Not the same operation as [inverse], which swaps rather than
         * hides — see [drawnFg].
         */
        val invisible get() = flags and FLAG_INVISIBLE != 0

        /** Half-intensity text (`ESC[2m`). Drawn, not ignored — see [drawnFg]. */
        val faint get() = flags and FLAG_FAINT != 0

        /**
         * [text] blended halfway towards [back], per channel, in `0xRRGGBB`.
         *
         * The mean rather than `text + (back - text) / 2`: the two differ by one on odd sums, and
         * the mean gives the same grey whichever way round the channels are (a dimmed
         * white-on-black and a dimmed black-on-white are the same colour, which is the property
         * this is for).
         */
        private fun dimTowards(text: Int, back: Int): Int {
            val r = (((text shr 16) and 0xFF) + ((back shr 16) and 0xFF)) / 2
            val g = (((text shr 8) and 0xFF) + ((back shr 8) and 0xFF)) / 2
            val b = ((text and 0xFF) + (back and 0xFF)) / 2
            return (r shl 16) or (g shl 8) or b
        }
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
        /** The frame's own version byte (0 before the extension existed). */
        val version: Int = FRAME_VERSION,
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
        val version = b[41].toInt() and 0xff
        val extLen = u32(b, 42)
        val selection = if (b[33].toInt() != 0) {
            SelectionBounds(
                startCol = u16(b, 34),
                // The rows are a u8 at 36/39 and a high byte at 46/47, which is zero
                // in a frame from before the high bytes existed.
                startRow = (b[36].toInt() and 0xff) or ((b[46].toInt() and 0xff) shl 8),
                endCol = u16(b, 37),
                endRow = (b[39].toInt() and 0xff) or ((b[47].toInt() and 0xff) shl 8),
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
        val fits = (b.size - HEADER_SIZE) / CELL_FIXED
        val cellCount = minOf(cols * rows, fits)
        val cells = ArrayList<Cell>(cellCount.coerceAtLeast(0))
        var p = HEADER_SIZE
        for (i in 0 until cellCount) {
            if (p + CELL_FIXED > b.size) break
            val len = b[p].toInt() and 0xff
            val flags = (b[p + 1].toInt() and 0xff) or ((b[p + 2].toInt() and 0xff) shl 8)
            val fg = rgb(b, p + 3)
            val bg = rgb(b, p + 6)
            p += CELL_FIXED
            val text = if (len > 0 && p + len <= b.size) {
                b.decodeToString(p, p + len)
            } else {
                ""
            }
            p += len
            cells.add(Cell(text, fg, bg, flags))
        }
        // The extension block, only from a version this decoder understands and only
        // when every cell arrived (its indices are cell positions) and it fits.
        if (version in 1..FRAME_VERSION && extLen > 0 && cells.size == cols * rows &&
            extLen <= (b.size - p).toLong()
        ) {
            applyExtensions(b, p, p + extLen.toInt(), cells)
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
            version,
        )
    }

    /**
     * Fold the extension sections into [cells]. Every length and index is checked
     * against what is really there: the block comes from native code that a future
     * version may change, and a bad one must lose its decoration rather than throw
     * in the render path.
     */
    private fun applyExtensions(b: ByteArray, start: Int, end: Int, cells: MutableList<Cell>) {
        var p = start
        while (p + 5 <= end) {
            val kind = b[p].toInt() and 0xff
            val len = u32(b, p + 1)
            p += 5
            if (len > (end - p).toLong()) return
            val stop = p + len.toInt()
            when (kind) {
                1 -> {
                    var q = p
                    while (q + 7 <= stop) {
                        val idx = u32(b, q)
                        if (idx < cells.size) {
                            val i = idx.toInt()
                            cells[i] = cells[i].withExtras(rgb(b, q + 4), null)
                        }
                        q += 7
                    }
                }
                2 -> applyLinks(b, p, stop, cells)
                else -> Unit // a kind from the future: skipped by its length
            }
            p = stop
        }
    }

    private fun applyLinks(b: ByteArray, start: Int, end: Int, cells: MutableList<Cell>) {
        var q = start
        if (q + 2 > end) return
        val n = u16(b, q)
        q += 2
        val uris = ArrayList<String>(n)
        for (i in 0 until n) {
            if (q + 2 > end) return
            val l = u16(b, q)
            q += 2
            if (q + l > end) return
            uris.add(b.decodeToString(q, q + l))
            q += l
        }
        if (q + 4 > end) return
        val m = u32(b, q)
        q += 4
        var r = 0L
        while (r < m && q + 6 <= end) {
            val idx = u32(b, q)
            val u = u16(b, q + 4)
            if (idx < cells.size && u < uris.size) {
                val i = idx.toInt()
                cells[i] = cells[i].withExtras(null, uris[u])
            }
            q += 6
            r++
        }
    }
}