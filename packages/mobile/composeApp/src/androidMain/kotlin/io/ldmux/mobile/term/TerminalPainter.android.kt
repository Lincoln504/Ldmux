package io.ldmux.mobile.term


import android.graphics.Paint
import android.graphics.Typeface
import io.ldmux.mobile.AndroidApp
import io.ldmux.mobile.R
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas
/**
 * The selection wash.
 *
 * The system's own selection is its accent colour at partial alpha over the text; this
 * is the same idea with no hue, because the app has none. Deliberately light enough
 * that a glyph on top of it keeps its contrast, and light enough to read as "these
 * cells" rather than as a block of colour.
 */
private const val SELECTION_WASH = 0x44EDEDED

/** Alpha of the dotted underline that marks an OSC 8 link, in the top byte. */
private const val LINK_ALPHA = 0xA0 shl 24

/** Android renderer: draws the grid straight onto the Compose canvas. */
actual class TerminalPainter actual constructor(private val fontSizePx: Float) {

    /**
     * Fira Mono, the app's own face, not the platform's `MONOSPACE`.
     *
     * A terminal's grid is one advance per character, so a face that changes with the OEM build
     * changes the grid with it — and the bundled face is what every measured layout here assumes.
     * The regular cut is used for regular cells and the bold cut for bold ones; a failed load falls
     * back to the platform face (and to a synthesised bold) rather than to nothing.
     */
    private val regularFace: Typeface =
        AndroidApp.context.resources.getFont(R.font.fira_mono_regular)
    private val boldFace: Typeface =
        AndroidApp.context.resources.getFont(R.font.fira_mono_bold)

    private val measuring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = regularFace
        textSize = fontSizePx
    }
    private val bgPaint = Paint()
    private val selPaint = Paint()
    private val cursorPaint = Paint(Paint.ANTI_ALIAS_FLAG)

    // Underline strokes: one reusable segment buffer and paint, so the 20 fps grid
    // loop allocates nothing per cell (the shapes come from UnderlineGeometry).
    private val linePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.BUTT
    }
    private val lineBuf = FloatArray(UnderlineGeometry.MAX_SEGMENTS * 4)
    private val fgPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = regularFace
        textSize = fontSizePx
    }

    actual val metrics: CellMetrics
        get() {
            val fm = measuring.fontMetrics
            return CellMetrics(
                width = measuring.measureText("M"),
                height = fm.descent - fm.ascent,
                baseline = -fm.ascent,
            )
        }

    actual fun paint(scope: DrawScope, frame: TerminalFrame.Frame) {
        val m = metrics
        if (m.width <= 0f || m.height <= 0f) return
        scope.drawIntoCanvas { canvas ->
            val nc = canvas.nativeCanvas
            val cols = frame.cols
            for (i in frame.cells.indices) {
                val cell = frame.cells[i]
                val x = (i % cols) * m.width
                val y = (i / cols) * m.height
                // Reverse video and concealment are applied by the cell itself
                // (`drawnBg`/`drawnFg`), because getting the swap wrong is invisible
                // in a screenshot of plain text and the two operations are easy to
                // collapse into one. `pi` draws its cursor as reverse video
                // (`ESC[7m` + the character under it + `ESC[27m`) rather than showing
                // the hardware cursor, so before this the cursor cell was painted as
                // ordinary text — and on a blank cell, where the `isBlank` skip below
                // draws nothing at all, it vanished completely.
                val bg = argb(cell.drawnBg)
                // The cell's own background first, then the selection over it.
                //
                // This was an inversion, and an inversion of an *empty* cell's
                // background is opaque white: selecting a line of padding painted a
                // solid white bar across the terminal, which is what the phone showed.
                // A translucent wash is also what the platform does — the system's own
                // selection is its accent colour at partial alpha over the text, not a
                // reversal — so this looks like a selection instead of like damage. The
                // text keeps its own colour, which is the property that makes a
                // selection readable.
                val col = i % cols
                // A double-width glyph is one drawing over two columns. The head
                // paints a background two cells wide; the spacer tail then paints no
                // background (it would cover the right half of the glyph the head
                // just drew) and no text. A tail with no wide head before it is
                // malformed or scrolled-off context, and is painted as a plain cell.
                val tailOfWide = cell.spacerTail && col > 0 && frame.cells[i - 1].wide
                val span = if (cell.wide && col + 1 < cols) 2 else 1
                if (!tailOfWide) {
                    bgPaint.color = bg
                    nc.drawRect(x, y, x + span * m.width, y + m.height, bgPaint)
                }
                if (cell.selected) {
                    selPaint.color = SELECTION_WASH
                    nc.drawRect(x, y, x + m.width, y + m.height, selPaint)
                }
                if (tailOfWide) continue
                drawUnderline(nc, cell, x, y, span * m.width, m)
                if (cell.text.isBlank()) continue
                fgPaint.color = argb(cell.drawnFg)
                fgPaint.typeface = if (cell.bold) boldFace else regularFace
                fgPaint.textSkewX = if (cell.italic) -0.25f else 0f
                fgPaint.isUnderlineText = false
                val baseline = y + m.baseline
                nc.drawText(cell.text, x, baseline, fgPaint)
            }
        }
    }

    /**
     * The cell's underline, in its style and colour, or the link affordance.
     *
     * A cell with an underline style gets that style in its own underline colour
     * (else the text colour). A hyperlinked cell without an underline gets a dotted
     * underline in the text colour at reduced alpha: visibly "something here", quiet
     * enough not to read as an ordinary underline. Links are never followed from
     * here; the URI is untrusted program output.
     */
    private fun drawUnderline(
        nc: android.graphics.Canvas,
        cell: TerminalFrame.Cell,
        x: Float,
        y: Float,
        w: Float,
        m: CellMetrics,
    ) {
        var style = cell.underlineStyle
        var color = 0
        if (style != TerminalFrame.UNDERLINE_NONE) {
            color = argb(cell.drawnUnderlineColor)
        } else if (cell.hyperlink != null) {
            style = TerminalFrame.UNDERLINE_DOTTED
            color = (argb(cell.drawnFg) and 0x00FFFFFF) or LINK_ALPHA
        } else {
            return
        }
        val n = UnderlineGeometry.segments(style, x, y, w, m.height, m.baseline, lineBuf)
        if (n == 0) return
        linePaint.color = color
        linePaint.strokeWidth = UnderlineGeometry.thickness(m.height)
        nc.drawLines(lineBuf, 0, n * 4, linePaint)
    }

    /**
     * Draw the cursor at the canvas origin.
     *
     * This is a method of its own so the cursor can live in its own tiny canvas:
     * a terminal cursor blinks, and blinking it by repainting the whole grid two
     * or three times a second forever would be a permanent tax on a phone's
     * battery for no picture anyone asked for. The caller positions a one-cell
     * canvas at the cursor's cell, so this only ever draws one cell.
     */
    actual fun paintCursor(scope: DrawScope, frame: TerminalFrame.Frame) {
        scope.drawIntoCanvas { canvas -> drawCursor(canvas.nativeCanvas, frame, metrics) }
    }

    /**
     * The cursor, drawn over the grid. Shapes follow the VT styles: BAR=0,
     * BLOCK=1, UNDERLINE=2, BLOCK_HOLLOW=3. A block inverts its cell by filling
     * with the foreground colour, so the glyph underneath has to be repainted in
     * the background colour to stay readable.
     *
     * Coordinates are relative to the canvas, which the caller has already
     * positioned on the cursor's cell — hence the cell's own text, not the
     * text at `cursorX`/`cursorY` in a full-grid context.
     */
    private fun drawCursor(nc: android.graphics.Canvas, frame: TerminalFrame.Frame, m: CellMetrics) {
        if (!frame.cursorVisible) return
        if (frame.cursorX < 0 || frame.cursorY < 0) return
        if (frame.cursorX >= frame.cols || frame.cursorY >= frame.rows) return
        val x = 0f
        val y = 0f
        val i = frame.cursorY * frame.cols + frame.cursorX
        val cell = frame.cells.getOrNull(i)
        // A block cursor *inverts* the cell it sits on: it fills with what the text
        // was drawn in and repaints the glyph in what was behind it. Filling with the
        // default foreground instead broke exactly one case and broke it completely —
        // on a reverse-video cell whose foreground is the default (`ESC[7m` + text,
        // which is how `pi` draws its own cursor) the fill and the glyph came out the
        // same colour and the character under the cursor vanished. It also made the
        // cursor the default colour on a cell whose text is not.
        cursorPaint.color = argb(cell?.drawnFg ?: frame.defaultFg)
        when (frame.cursorStyle) {
            0 -> { // bar
                val w = (m.width / 8f).coerceAtLeast(1f)
                nc.drawRect(x, y, x + w, y + m.height, cursorPaint)
            }
            2 -> { // underline
                val h = (m.height / 12f).coerceAtLeast(1f)
                nc.drawRect(x, y + m.height - h, x + m.width, y + m.height, cursorPaint)
            }
            3 -> { // hollow block
                cursorPaint.style = Paint.Style.STROKE
                cursorPaint.strokeWidth = 1f
                nc.drawRect(x + 0.5f, y + 0.5f, x + m.width - 0.5f, y + m.height - 0.5f, cursorPaint)
                cursorPaint.style = Paint.Style.FILL
            }
            else -> { // block
                nc.drawRect(x, y, x + m.width, y + m.height, cursorPaint)
                if (cell != null && cell.text.isNotBlank()) {
                    // The colour actually behind the glyph, which reverse video has
                    // swapped — repainting it in `cell.bg` on an inverse cell would
                    // draw the glyph in the colour it is already on top of.
                    fgPaint.color = argb(cell.drawnBg)
                    fgPaint.typeface = if (cell.bold) boldFace else regularFace
                    fgPaint.textSkewX = if (cell.italic) -0.25f else 0f
                    fgPaint.isUnderlineText = false
                    nc.drawText(cell.text, x, y + m.baseline, fgPaint)
                }
            }
        }
    }

    private fun argb(rgb: Int): Int = 0xFF000000.toInt() or (rgb and 0xFFFFFF)
}