package dev.ldmux.mobile.mesh

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

/** [text] with every control character made a space, and at most [cols] code points. */
internal fun cleanRow(text: String, cols: Int): String {
    val sb = StringBuilder()
    var i = 0
    var n = 0
    while (i < text.length && n < cols) {
        val c = text[i]
        when {
            c.code < 0x20 || c.code == 0x7f || c.code in 0x80..0x9f -> { sb.append(' '); i++ }
            c.isHighSurrogate() && i + 1 < text.length && text[i + 1].isLowSurrogate() -> {
                sb.append(c).append(text[i + 1]); i += 2
            }
            else -> { sb.append(c); i++ }
        }
        n++
    }
    return sb.toString()
}
