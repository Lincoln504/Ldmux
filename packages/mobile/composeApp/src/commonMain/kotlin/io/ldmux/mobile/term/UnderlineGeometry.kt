package io.ldmux.mobile.term

import kotlin.math.PI
import kotlin.math.max
import kotlin.math.min
import kotlin.math.roundToInt
import kotlin.math.sin

/**
 * Which line segments make up an underline of a given style over a span of cells.
 *
 * Pure arithmetic with no graphics types, so the shapes are unit-tested on the JVM
 * and the painter only strokes what this returns. Everything is a stroked segment
 * (`x0, y0, x1, y1`) of width [thickness], written into a caller-owned array: the
 * painter calls this for every underlined cell at 20 fps and must not allocate.
 *
 * Every pattern is anchored to the left edge of the span and repeats a whole number
 * of times across it, so adjacent cells join into one continuous dotted, dashed or
 * wavy line instead of restarting visibly at each column.
 */
object UnderlineGeometry {
    /** The most segments one call writes; size the output array `MAX_SEGMENTS * 4`. */
    const val MAX_SEGMENTS = 16

    /** Samples per curly period (a period is one span width). */
    private const val CURLY_STEPS = 8

    /** Stroke width for a cell of height [cellHeight]: at least one pixel. */
    fun thickness(cellHeight: Float): Float = max(1f, cellHeight / 16f)

    /**
     * Write the segments for [style] over the span starting at ([x], [y]) that is
     * [w] wide and [h] tall (the cell, or several cells for a wide glyph);
     * [baseline] is the distance from the cell top to the text baseline. Returns
     * the number of segments written (0 for none or an unknown style). Every
     * segment, including its thickness, stays inside the span.
     */
    fun segments(
        style: Int,
        x: Float,
        y: Float,
        w: Float,
        h: Float,
        baseline: Float,
        out: FloatArray,
    ): Int {
        if (w <= 0f || h <= 0f || out.size < MAX_SEGMENTS * 4) return 0
        val t = thickness(h)
        val half = t / 2f
        // The lowest the centre of a stroke may sit and still be fully in the cell.
        val floor = h - half
        // A single line sits halfway between the baseline and the cell bottom.
        val single = min(floor, baseline + (h - baseline) / 2f)
        return when (style) {
            TerminalFrame.UNDERLINE_SINGLE -> line(out, 0, x, x + w, y + single)
            TerminalFrame.UNDERLINE_DOUBLE -> {
                val lower = floor
                val upper = max(half, lower - 2f * t)
                line(out, 0, x, x + w, y + upper)
                line(out, 1, x, x + w, y + lower)
                2
            }
            TerminalFrame.UNDERLINE_DOTTED -> {
                val n = min(MAX_SEGMENTS, max(1, (w / (2f * t)).roundToInt()))
                val pitch = w / n
                val dot = min(t, pitch / 2f)
                for (i in 0 until n) line(out, i, x + i * pitch, x + i * pitch + dot, y + single)
                n
            }
            TerminalFrame.UNDERLINE_DASHED -> {
                // Two dashes per cell width, 70% filled.
                val n = min(MAX_SEGMENTS, max(1, (w / (h / 2f)).roundToInt() * 2).coerceAtLeast(2))
                val pitch = w / n
                val dash = pitch * 0.7f
                for (i in 0 until n) line(out, i, x + i * pitch, x + i * pitch + dash, y + single)
                n
            }
            TerminalFrame.UNDERLINE_CURLY -> {
                // One sine period per span, amplitude limited by the room under the
                // baseline; the wave's lowest point still clears the cell bottom.
                val amp = max(0f, min((h - baseline) / 2f, h / 6f) - half).coerceAtMost(floor / 2f)
                val centre = max(half + amp, floor - amp)
                var px = x
                var py = y + centre
                for (k in 1..CURLY_STEPS) {
                    val cx = x + w * k / CURLY_STEPS
                    val cy = y + centre + amp * sin(2.0 * PI * k / CURLY_STEPS).toFloat()
                    val o = (k - 1) * 4
                    out[o] = px
                    out[o + 1] = py
                    out[o + 2] = cx
                    out[o + 3] = cy
                    px = cx
                    py = cy
                }
                CURLY_STEPS
            }
            else -> 0
        }
    }

    private fun line(out: FloatArray, i: Int, x0: Float, x1: Float, y: Float): Int {
        val o = i * 4
        out[o] = x0
        out[o + 1] = y
        out[o + 2] = x1
        out[o + 3] = y
        return 1
    }
}
