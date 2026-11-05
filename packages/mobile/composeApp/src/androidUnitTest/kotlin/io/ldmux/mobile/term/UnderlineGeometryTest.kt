package io.ldmux.mobile.term

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class UnderlineGeometryTest {
    // A 10 x 20 cell with the baseline 16 down, like a small monospace font.
    private val w = 10f
    private val h = 20f
    private val baseline = 16f

    private fun segs(style: Int, x: Float = 0f, y: Float = 0f, width: Float = w): Pair<Int, FloatArray> {
        val buf = FloatArray(UnderlineGeometry.MAX_SEGMENTS * 4)
        return UnderlineGeometry.segments(style, x, y, width, h, baseline, buf) to buf
    }

    @Test
    fun `none and unknown styles draw nothing`() {
        assertEquals(0, segs(TerminalFrame.UNDERLINE_NONE).first)
        assertEquals(0, segs(99).first)
    }

    @Test
    fun `a degenerate cell or a short buffer draws nothing and does not throw`() {
        val buf = FloatArray(UnderlineGeometry.MAX_SEGMENTS * 4)
        assertEquals(0, UnderlineGeometry.segments(1, 0f, 0f, 0f, h, baseline, buf))
        assertEquals(0, UnderlineGeometry.segments(1, 0f, 0f, w, 0f, baseline, buf))
        assertEquals(0, UnderlineGeometry.segments(1, 0f, 0f, w, h, baseline, FloatArray(3)))
    }

    @Test
    fun `single is one full-width line below the baseline`() {
        val (n, b) = segs(TerminalFrame.UNDERLINE_SINGLE)
        assertEquals(1, n)
        assertEquals(0f, b[0]); assertEquals(w, b[2])
        assertEquals(b[1], b[3])
        assertTrue(b[1] > baseline && b[1] < h)
    }

    @Test
    fun `double is two distinct parallel lines`() {
        val (n, b) = segs(TerminalFrame.UNDERLINE_DOUBLE)
        assertEquals(2, n)
        assertTrue(b[1] != b[5], "two different heights")
        assertEquals(w, b[2]); assertEquals(w, b[6])
        assertTrue(b[5] - b[1] >= UnderlineGeometry.thickness(h), "the lines do not merge")
    }

    @Test
    fun `dotted and dashed are several separate pieces with gaps`() {
        for (style in listOf(TerminalFrame.UNDERLINE_DOTTED, TerminalFrame.UNDERLINE_DASHED)) {
            val (n, b) = segs(style)
            assertTrue(n >= 2, "style $style has $n pieces")
            for (i in 0 until n - 1) {
                assertTrue(b[i * 4 + 2] < b[(i + 1) * 4], "gap between piece $i and the next")
            }
        }
    }

    @Test
    fun `curly is a continuous wave that starts and ends on the centre line`() {
        val (n, b) = segs(TerminalFrame.UNDERLINE_CURLY)
        assertTrue(n >= 4)
        for (i in 0 until n - 1) {
            assertEquals(b[i * 4 + 2], b[(i + 1) * 4], "joined in x")
            assertEquals(b[i * 4 + 3], b[(i + 1) * 4 + 1], "joined in y")
        }
        assertEquals(0f, b[0]); assertEquals(w, b[(n - 1) * 4 + 2], 1e-4f)
        assertEquals(b[1], b[(n - 1) * 4 + 3], 1e-3f, "tiles into the next cell")
        val ys = (0 until n).map { b[it * 4 + 3] }
        assertTrue(ys.max() > ys.min(), "it actually waves")
    }

    @Test
    fun `every style stays inside the cell, thickness included`() {
        for (style in TerminalFrame.UNDERLINE_SINGLE..TerminalFrame.UNDERLINE_DASHED) {
            for (cellH in listOf(8f, 14f, 20f, 40f)) {
                for (cellW in listOf(5f, 10f, 20f, 40f)) {
                    val base = cellH * 0.8f
                    val buf = FloatArray(UnderlineGeometry.MAX_SEGMENTS * 4)
                    val n = UnderlineGeometry.segments(style, 100f, 200f, cellW, cellH, base, buf)
                    val half = UnderlineGeometry.thickness(cellH) / 2f
                    assertTrue(n in 1..UnderlineGeometry.MAX_SEGMENTS, "style $style")
                    for (i in 0 until n) {
                        for (k in intArrayOf(0, 2)) {
                            assertTrue(buf[i * 4 + k] >= 100f - 1e-3f && buf[i * 4 + k] <= 100f + cellW + 1e-3f,
                                "x in span: style $style $cellW x $cellH")
                        }
                        for (k in intArrayOf(1, 3)) {
                            val y = buf[i * 4 + k]
                            assertTrue(y - half >= 200f - 1e-3f && y + half <= 200f + cellH + 1e-3f,
                                "y in cell: style $style $cellW x $cellH y=$y")
                        }
                    }
                }
            }
        }
    }

    @Test
    fun `a two-cell span tiles the same pattern and offsets move it`() {
        val (one, a) = segs(TerminalFrame.UNDERLINE_DOTTED)
        val (two, b) = segs(TerminalFrame.UNDERLINE_DOTTED, width = 2 * w)
        assertEquals(2 * one, two)
        // The pitch is the same, so dots line up with two adjacent single cells.
        assertEquals(a[4] - a[0], b[4] - b[0], 1e-3f)
        val (_, moved) = segs(TerminalFrame.UNDERLINE_SINGLE, x = 50f, y = 100f)
        assertEquals(50f, moved[0]); assertEquals(60f, moved[2])
        assertTrue(moved[1] > 100f + baseline)
    }

    @Test
    fun `the style and colour a cell reports are the ones the painter uses`() {
        val single = TerminalFrame.FLAG_UNDERLINE or (TerminalFrame.UNDERLINE_CURLY shl TerminalFrame.UNDERLINE_STYLE_SHIFT)
        val c = TerminalFrame.Cell("a", 0xFFFFFF, 0, single, underlineColor = 0xFF0000)
        assertEquals(TerminalFrame.UNDERLINE_CURLY, c.underlineStyle)
        assertEquals(0xFF0000, c.drawnUnderlineColor)
        val plain = TerminalFrame.Cell("a", 0x112233, 0, TerminalFrame.FLAG_UNDERLINE)
        assertEquals(TerminalFrame.UNDERLINE_SINGLE, plain.underlineStyle)
        assertEquals(0x112233, plain.drawnUnderlineColor)
    }
}
