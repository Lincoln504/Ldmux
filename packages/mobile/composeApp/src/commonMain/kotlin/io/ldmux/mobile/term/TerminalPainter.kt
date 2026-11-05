package io.ldmux.mobile.term

import androidx.compose.ui.graphics.drawscope.DrawScope

/** The cell geometry for a given font size. */
class CellMetrics(val width: Float, val height: Float, val baseline: Float)

/**
 * Draws a decoded frame as a cell grid. Platform-specific because it needs the
 * native text rasteriser (android.graphics.Paint on Android, Skia on iOS).
 */
expect class TerminalPainter(fontSizePx: Float) {
    val metrics: CellMetrics

    /** Paint the grid. The cursor is drawn separately (see [paintCursor]). */
    fun paint(scope: DrawScope, frame: TerminalFrame.Frame)

    /**
     * Paint just the cursor, at the canvas origin.
     *
     * The caller puts a one-cell canvas on the cursor's cell, so a blinking
     * cursor costs one cell, not a whole grid.
     */
    fun paintCursor(scope: DrawScope, frame: TerminalFrame.Frame)
}