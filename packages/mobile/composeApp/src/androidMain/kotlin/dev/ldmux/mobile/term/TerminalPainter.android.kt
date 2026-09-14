package dev.ldmux.mobile.term

import android.graphics.Paint
import android.graphics.Typeface
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.drawIntoCanvas
import androidx.compose.ui.graphics.nativeCanvas

/** Android renderer: draws the grid straight onto the Compose canvas. */
actual class TerminalPainter actual constructor(private val fontSizePx: Float) {

    private val measuring = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        typeface = Typeface.MONOSPACE
        textSize = fontSizePx
    }
    private val bgPaint = Paint()
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
                bgPaint.color = argb(cell.bg)
                nc.drawRect(x, y, x + m.width, y + m.height, bgPaint)
                if (cell.text.isBlank()) continue
                fgPaint.color = argb(cell.fg)
                fgPaint.isFakeBoldText = cell.bold
                fgPaint.textSkewX = if (cell.italic) -0.25f else 0f
                fgPaint.isUnderlineText = cell.underline
                val baseline = y + m.baseline
                nc.drawText(cell.text, x, baseline, fgPaint)
            }
            drawCursor(nc, frame, m)
        }
    }

    /**
     * The cursor is drawn last, over the grid. Shapes follow the VT styles:
     * BAR=0, BLOCK=1, UNDERLINE=2, BLOCK_HOLLOW=3. A block inverts its cell by
     * filling with the foreground colour, so the glyph underneath has to be
     * repainted in the background colour to stay readable.
     */
    private fun drawCursor(nc: android.graphics.Canvas, frame: TerminalFrame.Frame, m: CellMetrics) {
        if (!frame.cursorVisible) return
        if (frame.cursorX < 0 || frame.cursorY < 0) return
        if (frame.cursorX >= frame.cols || frame.cursorY >= frame.rows) return
        val x = frame.cursorX * m.width
        val y = frame.cursorY * m.height
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