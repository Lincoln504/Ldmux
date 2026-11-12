package io.ldmux.mobile.mesh

import io.ldmux.mobile.term.charCountOf
import io.ldmux.mobile.term.codePointAt
import io.ldmux.mobile.term.displayWidthOf

/**
 * A mesh [MeshScreen] as the bytes that put it on the terminal engine's grid, so the mesh route
 * draws through the same surface as the tailnet route instead of its own rows.
 *
 * The link sends rows and a cursor, never a byte stream, so this is a **local** reconstruction:
 * nothing about it goes on the air, and nothing in it came from the host except text.
 *
 * The sequence, and why each part is there:
 *  - `ESC[0m`: a clean pen. Nothing here sets a style, but the engine is long-lived.
 *  - `ESC[?7l`: autowrap off. A row that fills the grid leaves the cursor in the last column,
 *    and a wide glyph can overhang it; with wrap on, either would push a line onto the next row
 *    (or scroll the screen when it is the last). Off, the overhang overwrites the last cell.
 *  - per row: `ESC[{y+1};1H`, the text, `ESC[K`. Addressing every row absolutely means no row
 *    depends on where the last one ended, and the erase clears whatever the previous screen
 *    left past this row's text. No full clear: a clear is the one operation that differs between
 *    emulators in whether it feeds scrollback, and this link has none.
 *  - `ESC[?7h`: autowrap back on, so the engine is not left in a mode the host never chose.
 *  - the cursor: shown or hidden (a dead shell has no cursor to place), then `ESC[{y+1};{x+1}H`,
 *    clamped into the grid. It goes last because writing text moves it.
 *
 * **The rows are untrusted text.** A control character in a row would be interpreted by the
 * engine, and an OSC 52 in one is a clipboard write. Every C0 control, DEL and C1 control is
 * replaced by a space, so the only escapes in the output are the ones above.
 */
internal fun meshScreenBytes(screen: MeshScreen, cols: Int, rows: Int, showCursor: Boolean = true): ByteArray {
    if (cols <= 0 || rows <= 0) return ByteArray(0)
    val out = StringBuilder()
    out.append("\u001b[0m\u001b[?7l")
    for (y in 0 until rows) {
        out.append("\u001b[").append(y + 1).append(";1H")
        out.append(cleanRow(screen.lines.getOrNull(y).orEmpty(), cols))
        out.append("\u001b[K")
    }
    out.append("\u001b[?7h")
    out.append(if (screen.dead || !showCursor) "\u001b[?25l" else "\u001b[?25h")
    val cx = screen.cursorX.coerceIn(0, cols - 1)
    val cy = screen.cursorY.coerceIn(0, rows - 1)
    out.append("\u001b[").append(cy + 1).append(';').append(cx + 1).append('H')
    return out.toString().encodeToByteArray()
}

/**
 * [text] with every control character made a space, and at most [cols] **cells**.
 *
 * Cells, not code points: a CJK ideograph or an emoji is two columns wide, so counting it as one
 * made the row a column wider than the grid and, with autowrap off, the last cell was overwritten
 * on top of itself. A combining mark is no columns at all, so counting it as one cut the row short.
 *
 * A wide character is either drawn whole or not at all: when it does not fit in the column that is
 * left, the row simply ends there and `ESC[K` clears the remainder.
 */
internal fun cleanRow(text: String, cols: Int): String {
    val sb = StringBuilder()
    var cells = 0
    var i = 0
    while (i < text.length) {
        val cp = codePointAt(text, i)
        val width = displayWidthOf(cp)
        if (cells + width > cols) break
        if (cp < 0x20 || cp == 0x7f || cp in 0x80..0x9f) {
            sb.append(' ')
            cells += 1
        } else {
            sb.appendRange(text, i, i + charCountOf(cp))
            cells += width
        }
        i += charCountOf(cp)
    }
    return sb.toString()
}

/**
 * The unsent draft, as the cells that draw it on the row where the cursor already is.
 *
 * This is the local echo of what the user has typed but not sent, written into the engine after the
 * host's own screen. It used to be the draft's raw text, which had two failures that both come
 * from the engine being a real terminal:
 *
 *  - **A line end scrolls.** Enter was echoed as `\r\n`, and a `\n` on the last row scrolls the whole
 *    engine up by a line — `ESC[?7l` turns off autowrap, not line feed — so the live screen shifted
 *    out from under itself before the command had even been sent.
 *  - **A long draft writes past the row.** With autowrap off, every character past the last column
 *    overwrote the final cell, so the user could not read what they were typing.
 *
 * So a key is drawn as a glyph (`⏎` for Enter, `⇥` for Tab) and the draft is **windowed** to the
 * cells left of the last column on this row. Windowing keeps the **end** of the draft, which is what
 * the user just typed, behind a leading `…` when anything was cut off.
 *
 * A backspace is echoed as `\b \b`, which erases the cell before it without moving to another row.
 * Every other control byte is dropped: the echo must never be able to issue an escape of its own.
 */
internal fun draftEcho(text: String, cursorX: Int, cols: Int): String {
    if (text.isEmpty() || cols <= 0) return ""
    // One entry per cell the draft occupies, so the window can be cut by cells and a wide glyph is
    // never split. A `\b` removes the previous cell, which is what it does on screen.
    val cells = ArrayList<String>()
    var i = 0
    while (i < text.length) {
        val cp = codePointAt(text, i)
        when {
            cp == '\b'.code -> { if (cells.isNotEmpty()) cells.removeAt(cells.lastIndex) }
            // One glyph per line end, whichever ending it is: `\r\n` is one line end and must not
            // draw as two, and a bare `\r` (what the shell's own Enter produces here) is the same.
            cp == '\r'.code -> {
                cells += ENTER_GLYPH
                if (i + 1 < text.length && text[i + 1] == '\n') i += 1
            }
            cp == '\n'.code -> cells += ENTER_GLYPH
            cp == '\t'.code -> cells += TAB_GLYPH
            cp < 0x20 || cp == 0x7f || cp in 0x80..0x9f -> {}
            else -> {
                cells += text.substring(i, i + charCountOf(cp))
                // A wide glyph covers two columns; the second is a blank the glyph itself draws
                // over, so nothing further is appended for it but the space has to be counted.
                repeat(displayWidthOf(cp) - 1) { cells += "" }
            }
        }
        i += charCountOf(cp)
    }
    val room = (cols - cursorX.coerceIn(0, cols - 1)).coerceAtLeast(1)
    if (cells.size <= room) return cells.joinToString("")
    val kept = cells.subList(cells.size - (room - 1), cells.size)
    return ELLIPSIS + kept.joinToString("")
}

/** The draft's visible stand-in for Enter. */
internal const val ENTER_GLYPH = "\u23CE"

/** The draft's visible stand-in for Tab. */
internal const val TAB_GLYPH = "\u21E5"

/** What says the front of a draft is not shown. */
internal const val ELLIPSIS = "\u2026"
