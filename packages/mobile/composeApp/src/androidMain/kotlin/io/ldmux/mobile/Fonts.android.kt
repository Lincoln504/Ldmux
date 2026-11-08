package io.ldmux.mobile

import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight

/**
 * The theme's two faces, built once each.
 *
 * **One monospace face for all of them: Fira Mono.** The monospace face is the terminal's cell
 * grid and the app's own labels, so a theme that changed it changed the *advance* of every column —
 * a terminal re-gridded on a theme switch, and two themes' worth of metrics to keep honest. The
 * personality belongs in the proportional face and the palette; the terminal's type does not need a
 * costume. Fira Mono is the face this app measured its layouts against, and it is the one the
 * operator asked to keep.
 *
 * A face is five `FontFamily` objects per theme at most, and a screen can ask for one on every
 * recomposition, so they are built lazily and kept rather than remade: `FontFamily(...)` allocates,
 * and a terminal at 20 fps that rebuilt its family per frame would be allocating for a picture that
 * did not change.
 *
 * The pair is *total* over [LdmuxTheme] — a `when` with no `else`, so adding a theme without
 * deciding its faces is a compile error rather than a silent fall back to the platform's own.
 */
private class Faces(val mono: FontFamily, val sans: FontFamily)

private val facesByTheme: Map<LdmuxTheme, Faces> by lazy {
    LdmuxTheme.entries.associateWith { theme ->
        when (theme) {
            LdmuxTheme.Dark -> Faces(
                mono = FontFamily(
                    Font(R.font.fira_mono_regular, FontWeight.Normal),
                    Font(R.font.fira_mono_bold, FontWeight.Bold),
                ),
                // Inter for the chrome: the quiet, neutral UI face (Blender moved to it for the same
                // reason). Fira Sans has a distinct voice that competes with the mono on a screen
                // that is mostly a terminal.
                sans = FontFamily(
                    Font(R.font.inter_regular, FontWeight.Normal),
                    Font(R.font.inter_semibold, FontWeight.SemiBold),
                ),
            )
            LdmuxTheme.Forest -> Faces(
                mono = FontFamily(
                    Font(R.font.fira_mono_regular, FontWeight.Normal),
                    Font(R.font.fira_mono_bold, FontWeight.Bold),
                ),
                sans = FontFamily(
                    Font(R.font.pt_sans_regular, FontWeight.Normal),
                    Font(R.font.pt_sans_bold, FontWeight.Bold),
                ),
            )
            LdmuxTheme.Air -> Faces(
                // JetBrains Mono: the tall x-height and open apertures that make it the most
                // legible of the favourites at 11sp, which is what a phone terminal is.
                mono = FontFamily(
                    Font(R.font.jetbrains_mono_regular, FontWeight.Normal),
                    Font(R.font.jetbrains_mono_bold, FontWeight.Bold),
                ),
                sans = FontFamily(
                    Font(R.font.lato_light, FontWeight.Light),
                    Font(R.font.lato_regular, FontWeight.Normal),
                ),
            )
            LdmuxTheme.Daybreak -> Faces(
                // Source Code Pro: Adobe's neutral, slightly narrow mono, which keeps a light
                // page from looking like a type specimen.
                mono = FontFamily(
                    Font(R.font.source_code_pro_regular, FontWeight.Normal),
                    Font(R.font.source_code_pro_bold, FontWeight.Bold),
                ),
                sans = FontFamily(
                    Font(R.font.chakra_petch_regular, FontWeight.Normal),
                    Font(R.font.chakra_petch_bold, FontWeight.Bold),
                ),
            )
            LdmuxTheme.Gruvbox -> Faces(
                // Geist Mono: clean geometric strokes that sit well beside gruvbox's flat,
                // warm surfaces without imitating the scheme's own paper-like fonts.
                mono = FontFamily(
                    Font(R.font.geist_mono_regular, FontWeight.Normal),
                    Font(R.font.geist_mono_bold, FontWeight.Bold),
                ),
                sans = FontFamily(
                    Font(R.font.oxygen_regular, FontWeight.Normal),
                    Font(R.font.oxygen_bold, FontWeight.Bold),
                ),
            )
            LdmuxTheme.Nerv -> Faces(
                // Cascadia Code: the terminal face, drawn for a terminal, with the rounded
                // terminals that make it read as an instrument rather than as a document.
                mono = FontFamily(
                    Font(R.font.cascadia_code_regular, FontWeight.Normal),
                    Font(R.font.cascadia_code_bold, FontWeight.Bold),
                ),
                sans = FontFamily(
                    Font(R.font.rajdhani_medium, FontWeight.Normal),
                    Font(R.font.rajdhani_bold, FontWeight.Bold),
                ),
            )
        }
    }
}

private fun facesFor(theme: LdmuxTheme): Faces = facesByTheme.getValue(theme)

/**
 * The theme's monospace face.
 *
 * A getter rather than a `val`: it is read inside composition, where the read of [ThemeState] is
 * tracked like a composition local, so switching theme re-letters every key row, id and the header
 * without a call site knowing a theme exists.
 */
actual val MonoFont: FontFamily get() = facesFor(ThemeState.theme).mono

/** The theme's proportional face: what the root installs as the app's default text style. */
actual val SansFont: FontFamily get() = facesFor(ThemeState.theme).sans

/** A named theme's monospace face — see the expect declaration. */
actual fun monoFontFor(theme: LdmuxTheme): FontFamily = facesFor(theme).mono

/** A named theme's proportional face — see the expect declaration. */
actual fun sansFontFor(theme: LdmuxTheme): FontFamily = facesFor(theme).sans

/**
 * The monospace face as a platform typeface, for the terminal painter.
 *
 * The painter draws through `android.graphics.Paint` rather than Compose text, so it needs the
 * `Typeface` and not the `FontFamily` — same face, same advance, one lookup. `getFont` needs an
 * asset manager, which the painter already has by way of `AndroidApp.context`.
 */
internal fun monoTypeface(theme: LdmuxTheme, bold: Boolean): android.graphics.Typeface =
    typefaceCache.getOrPut(theme to bold) { loadTypeface(theme, bold) }

/** One load per face and weight: `getFont` parses a resource, and the painter asks per frame. */
private val typefaceCache = HashMap<Pair<LdmuxTheme, Boolean>, android.graphics.Typeface>()

private fun loadTypeface(theme: LdmuxTheme, bold: Boolean): android.graphics.Typeface {
    // The same face the Compose side uses, so a bold cell is the bold cut of the theme's mono rather
    // than a synthesised slant of its regular one.
    val res = when (theme) {
        LdmuxTheme.Dark -> if (bold) R.font.fira_mono_bold else R.font.fira_mono_regular
        LdmuxTheme.Forest -> if (bold) R.font.ibm_plex_mono_bold else R.font.ibm_plex_mono_regular
        LdmuxTheme.Air -> if (bold) R.font.jetbrains_mono_bold else R.font.jetbrains_mono_regular
        LdmuxTheme.Daybreak -> {
            if (bold) R.font.source_code_pro_bold else R.font.source_code_pro_regular
        }
        LdmuxTheme.Nerv -> if (bold) R.font.cascadia_code_bold else R.font.cascadia_code_regular
        LdmuxTheme.Gruvbox -> if (bold) R.font.geist_mono_bold else R.font.geist_mono_regular
    }
    return AndroidApp.context.resources.getFont(res)
}
