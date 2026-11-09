package io.ldmux.mobile

import androidx.compose.ui.graphics.Color
import kotlin.math.pow
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * Every theme's terminal palette, checked as a set rather than by eye.
 *
 * **Two of these were wrong at once and neither was visible in a screenshot of a menu.** The
 * painter masks the theme's shell colours with `or 0xFF000000`, because a Compose `Color` may carry
 * alpha and `Paint.color` with alpha 0 draws nothing at all — so a theme whose terminal foreground
 * was translucent, or a painter that stripped the alpha, produced a pane with no text on it and a
 * cursor that still blinked. That is the empty terminal reported on 2026-11-09, on the mesh route
 * only (its rows are plain text and take the theme's colour; the tailnet route sends the program's
 * own SGR and never reaches that branch). This test is the reason it cannot come back in one theme
 * while the others look fine.
 */
class ThemesTest {
    /** WCAG 2.2 relative luminance, the one the contrast floors in `Themes.kt` are written in. */
    private fun luminance(c: Color): Double {
        fun channel(v: Float): Double {
            val s = v.toDouble()
            return if (s <= 0.03928) s / 12.92 else ((s + 0.055) / 1.055).pow(2.4)
        }
        return 0.2126 * channel(c.red) + 0.7152 * channel(c.green) + 0.0722 * channel(c.blue)
    }

    private fun contrast(a: Color, b: Color): Double {
        val la = luminance(a)
        val lb = luminance(b)
        return (maxOf(la, lb) + 0.05) / (minOf(la, lb) + 0.05)
    }

    @Test
    fun everyThemeHasAnOpaqueTerminalPalette() {
        for (theme in LdmuxTheme.entries) {
            val t = theme.palette.terminal
            assertEquals(
                1f,
                t.foreground.alpha,
                "${theme.label}: a translucent shell foreground draws nothing (Paint.color alpha 0)",
            )
            assertEquals(
                1f,
                t.background.alpha,
                "${theme.label}: a translucent shell background leaves the cell unpainted",
            )
            assertEquals(
                1f,
                t.cursor.alpha,
                "${theme.label}: the cursor is a block, and a translucent one is invisible on most grounds",
            )
            assertTrue(
                t.selection.alpha < 1f,
                "${theme.label}: the selection is a wash over the cells, never an opaque fill",
            )
        }
    }

    @Test
    fun everyThemeReadsItsOwnTerminalText() {
        for (theme in LdmuxTheme.entries) {
            val t = theme.palette.terminal
            val ratio = contrast(t.foreground, t.background)
            assertTrue(
                ratio >= 4.5,
                "${theme.label}: shell text is ${"%.2f".format(ratio)}:1 on its own ground, below the 4.5:1 floor",
            )
        }
    }

    /**
     * **The default-cell colour is opaque whatever the theme says.** The painter's two theme values
     * are `or 0xFF000000` for exactly this reason, and the faint branch blends them — a blend in
     * `0xRRGGBB` that is returned without its alpha is transparent again.
     */
    @Test
    fun aFaintDefaultCellIsStillOpaque() {
        for (theme in LdmuxTheme.entries) {
            val t = theme.palette.terminal
            val fg = argb(t.foreground) or 0xFF000000.toInt()
            val bg = argb(t.background) or 0xFF000000.toInt()
            val blended = io.ldmux.mobile.term.TerminalFrame.blendTowards(fg, bg)
            assertEquals(
                0,
                (blended ushr 24) and 0xFF,
                "${theme.label}: the blend itself carries no alpha (which is why the caller must add it)",
            )
            val faint = 0xFF000000.toInt() or blended
            assertEquals(0xFF, (faint ushr 24) and 0xFF, "${theme.label}: faint default text must be opaque")
        }
    }

    /** `Color` as `0xAARRGGBB`, without depending on the Android-only `toArgb` in a JVM test. */
    private fun argb(c: Color): Int {
        fun ch(v: Float) = (v.coerceIn(0f, 1f) * 255f).toInt() and 0xFF
        return (ch(c.alpha) shl 24) or (ch(c.red) shl 16) or (ch(c.green) shl 8) or ch(c.blue)
    }
}
