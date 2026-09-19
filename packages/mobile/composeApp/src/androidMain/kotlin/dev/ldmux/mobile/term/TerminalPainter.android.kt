package dev.ldmux.mobile.term


import android.graphics.Paint
import android.graphics.Typeface
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

/** Android renderer: draws the grid straight onto the Compose canvas. */
actual class TerminalPainter actual constructor(private val fontSizePx: Float) {

    private val measuring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Typeface.MONOSPACE
        textSize = fontSizePx
    }
    private val bgPaint = Paint()
    private val selPaint = Paint()
    private val cursorPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val fgPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Typeface.MONOSPACE
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
                val bg = argb(cell.bg)
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
                bgPaint.color = bg
                nc.drawRect(x, y, x + m.width, y + m.height, bgPaint)
                if (cell.selected) {
                    selPaint.color = SELECTION_WASH
                    nc.drawRect(x, y, x + m.width, y + m.height, selPaint)
                }
                if (cell.text.isBlank()) continue
                fgPaint.color = argb(cell.fg)
                fgPaint.isFakeBoldText = cell.bold
                fgPaint.textSkewX = if (cell.italic) -0.25f else 0f
                fgPaint.isUnderlineText = cell.underline
                val baseline = y + m.baseline
                nc.drawText(cell.text, x, baseline, fgPaint)
            }
        }
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
        cursorPaint.color = argb(frame.defaultFg)
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
                val i = frame.cursorY * frame.cols + frame.cursorX
                val cell = frame.cells.getOrNull(i)
                if (cell != null && cell.text.isNotBlank()) {
                    fgPaint.color = argb(cell.bg)
                    fgPaint.isFakeBoldText = cell.bold
                    fgPaint.textSkewX = if (cell.italic) -0.25f else 0f
                    fgPaint.isUnderlineText = false
                    nc.drawText(cell.text, x, y + m.baseline, fgPaint)
                }
            }
        }
    }

    private fun argb(rgb: Int): Int = 0xFF000000.toInt() or (rgb and 0xFFFFFF)
}