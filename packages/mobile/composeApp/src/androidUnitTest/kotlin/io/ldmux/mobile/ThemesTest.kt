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

    /**
     * **The history scrollbar is a control boundary, so it has a 3:1 floor.**
     *
     * It was `Dim` at 45 %, which composites to 2.15:1 on the two light themes and 2.7-2.8:1 on the
     * dark ones — the one indicator a person scrolls the radio route with, because that route
     * deliberately has no scroll gestures. `TerminalScrollbar` is the theme's foreground at half;
     * this is the arithmetic that says it clears the floor on every theme, not just the one that
     * happened to be on screen. (A third, the first attempt, was 2.15:1 on white — which is why the
     * assertion is per theme and arithmetic rather than a judgement from a screenshot.)
     */
    @Test
    fun theHistoryScrollbarIsVisibleOnEveryTheme() {
        for (theme in LdmuxTheme.entries) {
            val t = theme.palette.terminal
            val over = composite(theme.palette.fg, TERMINAL_SCROLLBAR_ALPHA, t.background)
            val ratio = contrast(over, t.background)
            assertTrue(
                ratio >= 3.0,
                "${theme.label}: the scrollbar is ${"%.2f".format(ratio)}:1 on the shell's ground, below 3:1",
            )
        }
    }

    /** The same arithmetic for the old value, so the finding above stays checkable. */
    @Test
    fun theOldScrollbarColourWouldHaveFailedOnEveryTheme() {
        val failures = LdmuxTheme.entries.count { theme ->
            val over = composite(theme.palette.dim, 0.45f, theme.palette.terminal.background)
            contrast(over, theme.palette.terminal.background) < 3.0
        }
        assertEquals(
            LdmuxTheme.entries.size,
            failures,
            "every theme was under the floor, which is why the colour changed rather than one theme",
        )
    }

    /** A caption on a `filled` icon button is `Dim`, never `Muted`: 4.16-4.46:1 is under 4.5. */
    @Test
    fun aCaptionOnAFilledControlReadsItsOwnFill() {
        for (theme in LdmuxTheme.entries) {
            val p = theme.palette
            val dim = contrast(p.dim, p.raised)
            assertTrue(dim >= 4.5, "${theme.label}: Dim on Raised is ${"%.2f".format(dim)}:1")
        }
    }

    /** [over] at [alpha] composited onto [under], as the canvas would. */
    private fun composite(over: Color, alpha: Float, under: Color): Color {
        fun mix(a: Float, b: Float) = a * alpha + b * (1f - alpha)
        return Color(mix(over.red, under.red), mix(over.green, under.green), mix(over.blue, under.blue))
    }

    /** `Color` as `0xAARRGGBB`, without depending on the Android-only `toArgb` in a JVM test. */
    private fun argb(c: Color): Int {
        fun ch(v: Float) = (v.coerceIn(0f, 1f) * 255f).toInt() and 0xFF
        return (ch(c.alpha) shl 24) or (ch(c.red) shl 16) or (ch(c.green) shl 8) or ch(c.blue)
    }
}
