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
        }
    }

    private fun argb(rgb: Int): Int = 0xFF000000.toInt() or (rgb and 0xFFFFFF)
}