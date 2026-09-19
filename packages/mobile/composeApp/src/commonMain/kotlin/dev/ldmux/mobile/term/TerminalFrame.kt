package dev.ldmux.mobile.term

/**
 * Decoder for the frame produced by libghostty-vt (native/ldmux_terminal.c).
 * Pure Kotlin so it is unit-testable without a device.
 *
 * Layout (little-endian):
 *   header (40 bytes): u16 cols, u16 rows, u16 cursorX, u16 cursorY,
 *                      u8 cursorVisible, u8 cursorStyle,
 *                      u8 defFgR, u8 defFgG, u8 defFgB,
 *                      u8 defBgR, u8 defBgG, u8 defBgB,
 *                      u8 passwordInput, u8 viewportActive, u8 mouseTracking,
 *                      u32 scrollTotal, u32 scrollOffset, u32 scrollLen,
 *                      u8 mouseSgr, u8 alternateOn, u8 reserved[7]
 *   then cols*rows cell records, row-major:
 *                      u8 utf8Len, u8 flagsLo, u8 flagsHi,
 *                      u8 fgR, u8 fgG, u8 fgB, u8 bgR, u8 bgG, u8 bgB,
 *                      u8 utf8[utf8Len]
 */
object TerminalFrame {
    /** Bytes of frame header preceding the cell records. */
    const val HEADER_SIZE = 40
    const val FLAG_BOLD = 1 shl 0
    const val FLAG_ITALIC = 1 shl 1
    const val FLAG_UNDERLINE = 1 shl 2
    const val FLAG_FAINT = 1 shl 3
    const val FLAG_BLINK = 1 shl 4
    const val FLAG_INVERSE = 1 shl 5
    const val FLAG_INVISIBLE = 1 shl 6
    const val FLAG_STRIKETHROUGH = 1 shl 7
    const val FLAG_OVERLINE = 1 shl 8

    class Cell(
        /** The cell's text (a grapheme, may be empty for a blank cell). */
        val text: String,
        val fg: Int,
        val bg: Int,
        val flags: Int,
    ) {
        val bold get() = flags and FLAG_BOLD != 0
        val italic get() = flags and FLAG_ITALIC != 0
        val underline get() = flags and FLAG_UNDERLINE != 0
    }

    class Frame(
        val cols: Int,
        val rows: Int,
        val cursorX: Int,
        val cursorY: Int,
        val cursorVisible: Boolean,
        val cursorStyle: Int,
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
         * This doc used to claim this flag also separates those from `pi`, Claude
         * Code and Codex, on the theory that the latter stay on the *primary*
         * screen. That is false, measured: `pi` emits
         * `ESC[?1000h ?1002h ?1003h ?1004h ?1006h ?1049h`, i.e. it turns the
         * alternate screen on too (`1049`), along with SGR mouse tracking
         * (`1000`/`1002`/`1003`/`1006`) — so this flag alone does not tell a TUI
         * composer apart from `vim`/`less`/`htop`/`nano`. Read from the emulator
         * for the same freshness reason as [mouseTracking]; what it means for tap
         * handling is a separate, still-open question from what this field reports.
         */
        val alternateOn: Boolean,
        /** Scrollable area in rows: total, the viewport's offset, and its size. */
        val scrollTotal: Long,
        val scrollOffset: Long,
        val scrollLen: Long,
        /** Row-major, [rows] * [cols] cells. */
        val cells: List<Cell>,
    )

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
        val scrollTotal = u32(b, 19)
        val scrollOffset = u32(b, 23)
        val scrollLen = u32(b, 27)

        val cells = ArrayList<Cell>(cols * rows)
        var p = HEADER_SIZE
        for (i in 0 until cols * rows) {
            if (p + 9 > b.size) break
            val len = b[p].toInt() and 0xff
            val flags = (b[p + 1].toInt() and 0xff) or ((b[p + 2].toInt() and 0xff) shl 8)
            val fg = rgb(b, p + 3)
            val bg = rgb(b, p + 6)
            p += 9
            val text = if (len > 0 && p + len <= b.size) {
                String(b, p, len, Charsets.UTF_8)
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
            defaultFg,
            defaultBg,
            passwordInput,
            viewportActive,
            mouseTracking,
            mouseSgr,
            alternateOn,
            scrollTotal,
            scrollOffset,
            scrollLen,
            cells,
        )
    }
}