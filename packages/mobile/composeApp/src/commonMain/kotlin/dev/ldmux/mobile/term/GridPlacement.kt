package dev.ldmux.mobile.term

import kotlin.math.floor

/**
 * Where the cell grid sits inside the viewport, in pixels — the one piece of
 * geometry the renderer and the touch hit-test must agree on, which is why they
 * both derive it from here rather than each computing their own.
 *
 * There is no scale factor, and that is deliberate. The grid is sized to the
 * screen (the client tells the daemon how many cells fit, so the pane is the
 * right size for the device), and it is drawn 1:1. The only reason the old code
 * had a scale was a transient: between a rotation — or a font change — and the
 * resize that answers it, the grid is briefly the wrong size for the viewport.
 * Scaling to fit made that moment *look* tidy and cost far more than it was
 * worth. The pointer mapping had to remember to divide by it (the recorded
 * failure mode), text was resampled to a size nobody asked for, and a font
 * change would have fought itself by shrinking the very thing the user was
 * enlarging.
 *
 * So an oversized grid is **clipped, not squashed**, anchored to the corner that
 * matters: the bottom-left, where the shell's prompt and column 0 are.
 */
data class GridPlacement(
    val dx: Float,
    val dy: Float,
    val cellWidth: Float,
    val cellHeight: Float,
    val cols: Int,
    val rows: Int,
) {
    val gridWidth: Float get() = cellWidth * cols
    val gridHeight: Float get() = cellHeight * rows

    /** Pixel position of the top-left of one cell. */
    fun cellOrigin(col: Int, row: Int): Pair<Float, Float> =
        (dx + col * cellWidth) to (dy + row * cellHeight)

    /**
     * The inverse of the draw: a touch point to a cell index, or null when the
     * point is outside the grid (a margin, or clipped off the top).
     *
     * Row-major, so `index / cols` is the row and `index % cols` the column.
     * [floor], not a truncating cast: a truncating cast maps a point just left of
     * the grid to column 0 instead of rejecting it.
     */
    fun cellAt(x: Float, y: Float): Int? {
        if (cellWidth <= 0f || cellHeight <= 0f || cols <= 0 || rows <= 0) return null
        val col = floor((x - dx) / cellWidth).toInt()
        val row = floor((y - dy) / cellHeight).toInt()
        if (col < 0 || row < 0 || col >= cols || row >= rows) return null
        return row * cols + col
    }
}

/**
 * Place a [cols]x[rows] grid of [cellWidth]x[cellHeight] cells inside a viewport.
 *
 * Centred when it fits (the leftover is under one cell, so this is about
 * symmetry, not letterboxing). When it does not fit it is pinned to the
 * bottom-left so the shell's prompt stays on screen — the overflowing case is a
 * font change or a rotation whose resize has not landed yet, and the bottom row
 * is the one holding the cursor.
 */
fun placeGrid(
    cellWidth: Float,
    cellHeight: Float,
    cols: Int,
    rows: Int,
    viewportWidth: Float,
    viewportHeight: Float,
): GridPlacement {
    val gridW = cellWidth * cols
    val gridH = cellHeight * rows
    // Flush to the top-left. Centring was inherited from a general-purpose placer
    // and it is wrong for a terminal in a way that is measurable: the leftover is
    // under one cell, but splitting it in two puts a black band on *both* edges
    // instead of one, and the text no longer starts where the corner is. When the
    // grid is larger than the viewport it is pinned to the bottom, because that is
    // where the prompt and the cursor are and they are what you cannot lose.
    val dx = 0f
    val dy = if (gridH <= viewportHeight) 0f else viewportHeight - gridH
    return GridPlacement(dx, dy, cellWidth, cellHeight, cols, rows)
}