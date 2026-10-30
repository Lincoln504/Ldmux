package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.LdmuxConfig

/**
 * The text size at which the host's whole grid fits the width this route has.
 *
 * The tailnet route asks the host to resize its terminal to the phone, so a line is never wider
 * than the screen. This link has no resize (ADR-0025 lists it as absent), so the host's grid is
 * fixed and the phone has to fit to it: at the user's own text size an 80-column shell is wider
 * than a phone and its right edge was clipped, with the prompt and the cursor beyond it.
 *
 * Never larger than what the user chose, and never below the app's own minimum: past that the
 * text is unreadable, and clipping is the honest result.
 *
 * A monospaced advance is about [ADVANCE_EM] of the font size; [widthDp] is divided by the font
 * scale because the size is in sp, which the system scales.
 */
internal fun meshFitFontSp(widthDp: Float, cols: Int, fontScale: Float, wanted: Float): Float {
    if (cols <= 0 || widthDp <= 0f || fontScale <= 0f) return wanted
    val fits = widthDp / (cols * ADVANCE_EM * fontScale)
    return minOf(wanted, fits).coerceAtLeast(LdmuxConfig.MIN_FONT_SP).coerceAtMost(maxOf(wanted, LdmuxConfig.MIN_FONT_SP))
}

private const val ADVANCE_EM = 0.62f
