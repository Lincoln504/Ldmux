package dev.ldmux.mobile.term

import androidx.compose.ui.graphics.drawscope.DrawScope

/** The cell geometry for a given font size. */
class CellMetrics(val width: Float, val height: Float, val baseline: Float)

/**
 * Draws a decoded frame as a cell grid. Platform-specific because it needs the
 * native text rasteriser (android.graphics.Paint on Android, Skia on iOS).
 */
expect class TerminalPainter(fontSizePx: Float) {
    val metrics: CellMetrics
    fun paint(scope: DrawScope, frame: TerminalFrame.Frame)
}