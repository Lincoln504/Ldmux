package io.ldmux.mobile

import androidx.compose.material3.ColorScheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.material3.LocalTextStyle
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/**
 * The palette, as every screen reads it: **five themes, one set of names.**
 *
 * These were five literals and are now five lookups into [`ThemeState`]. The names are unchanged
 * because the names are the API — about two hundred call sites say `color = Bg` or
 * `border(1.dp, Line)`, and a theme that needed each of them rewritten is a theme that would have
 * reached most of the app and not all of it. The colour each name resolves to, and the contrast
 * floor it is held to, is in [`Palette`]; the literals are in `Themes.kt`, with the Radix steps
 * they were taken from.
 *
 * Reading snapshot state from a top-level property is what makes the switch live: a read during
 * composition is tracked exactly like a `CompositionLocal`, so every screen that touched a token
 * recomposes when the theme changes.
 */
val Bg: Color get() = ThemeState.theme.palette.base

/**
 * Body text: the app's one type colour, and the thing every other token is a relationship to.
 *
 * 16.14:1 on [Bg] in Dark and 16.39:1 in Air — the floor is 4.5:1, and the headroom is the point:
 * a terminal is read at arm's length in bad light, and this is the colour that does most of the
 * reading.
 */
/**
 * The history scrollbar's colour: **[Fg] at a third of its strength, over the terminal's ground.**
 *
 * **A 4 px bar is a control boundary, and the floor for one is 3:1.** This was `Dim` at 45 % alpha,
 * which composites to 2.15:1 on the two light themes and 2.7-2.8:1 on the dark ones — the one
 * indicator a person scrolls the radio route with (the route deliberately has no scroll gestures)
 * was the faintest thing on the screen. `Fg` at **half** is **3.2:1 on the light themes and 3.9-4.8:1
 * on the dark ones** (measured per theme by `ThemesTest`; a third was still 2.15:1 on white), and it
 * still reads as a quiet overlay rather than as a bar of text.
 *
 * A named accessor rather than an expression at the draw call, because `ThemesTest` has to be able
 * to hold every theme to the floor, and a colour picked inline cannot be checked.
 */
val TerminalScrollbar: Color get() = Fg.copy(alpha = TERMINAL_SCROLLBAR_ALPHA)

/** The scrollbar's alpha, named so `ThemesTest` can composite the same value it draws. */
const val TERMINAL_SCROLLBAR_ALPHA = 0.5f

val Fg: Color get() = ThemeState.theme.palette.fg

/**
 * The page's own surface: a card, a modal, a row that sits above the ground.
 *
 * Body text on it ([Fg]) clears 14.7:1 in Dark and 15.6:1 in Air; the floor is 4.5:1.
 */
val Surface: Color get() = ThemeState.theme.palette.surface

/**
 * The fill for a control that is *on*: the menu button that says you are in a shell, and the mark
 * that says a window is out on the desktop.
 *
 * It used to be [Accent], the brightest value in the palette, so the menu button was a near-white
 * block in an app whose whole idea is that importance lives in weight and contrast rather than in
 * a hue. A tone above [Surface] says the same thing without shouting, and leaves [Accent] meaning
 * the one primary action on a screen. On the light themes it is the *lower* step instead — a light
 * ground raises a control by cutting into it, not out of it.
 */
val Raised: Color get() = ThemeState.theme.palette.raised

/**
 * [Fg]'s quieter sibling: labels, secondary text, glyphs.
 *
 * "Dim" is a relationship rather than a risk — 8.26:1 on [Surface] in Dark, 5.65:1 in Air, both
 * well clear of the 4.5:1 floor — and it stays visibly below [Fg] so that the word means something
 * on a screen where everything is compared with everything.
 */
val Dim: Color get() = ThemeState.theme.palette.dim

/**
 * The quiet end of the palette, and it is a **colour, not [Dim] with an alpha**.
 *
 * Explanatory lines and field placeholders used `Dim.copy(alpha = 0.6f)` and `0.7f`, which works
 * out to under 4.5:1 on a ground this dark — on a screen that is otherwise high contrast. A faded
 * colour is also not a value anyone can look up. This one is the darker second grey: 5.30:1 on
 * [Surface] in Dark, 4.58:1 in Air, and 4.41:1 on [Raised] in Dark, which is why **nothing draws it
 * on [Raised]** — that fill carries [Fg].
 */
val Muted: Color get() = ThemeState.theme.palette.muted

/**
 * The one destructive colour: ending something.
 *
 * It lived as a literal in the close dialog, which is how a palette quietly grows a second
 * definition of its own warning. 7.72:1 on [Surface] in Dark (5.0:1 in Air) — a destructive control
 * is a thing to read clearly, not a mood.
 */
val Danger: Color get() = ThemeState.theme.palette.danger

/**
 * An anomaly, not a mood: the amber the app uses to say something is wrong but recoverable. A shell
 * with no window is the one such state a row can show, and it should be rare enough that the eye
 * stops on it.
 */
val Warn: Color get() = ThemeState.theme.palette.warn

/**
 * The one primary action on a screen.
 *
 * In Dark it is simply the lightest thing on screen, and there is no hue at all; in the four
 * coloured themes it is the theme's own accent, at the step that palette reserves for text so it is
 * legible as a label rather than only as a fill (4.77:1 on white in Air, 4.98:1 in Vermillion,
 * 8.70:1 in Forest, 8.83:1 in NERV). [OnAccent] is the text drawn on a solid [Accent] fill.
 */
val Accent: Color get() = ThemeState.theme.palette.accent

/**
 * The text drawn on a solid [Accent] fill.
 *
 * It exists because "the accent is a fill" and "the accent is a label" are two roles with two
 * answers: on a light theme the accent is dark and its text is white, and on a dark theme with a
 * bright accent the text is the ground. Drawing [Fg] there instead was invisible on exactly the
 * themes where the accent is weakest.
 */
val OnAccent: Color get() = ThemeState.theme.palette.onAccent

/**
 * The outline of a control that is *chosen*.
 *
 * Deliberately not [Accent]. A checkmark outline is a state, not an action, and at the accent's
 * value it read as the brightest thing on the launcher — brighter than the rows it was describing.
 * This is one clear step down: still unambiguous at 3:1 (the floor for a non-text boundary), no
 * longer competing with the one primary action on the screen.
 */
val Chosen: Color get() = ThemeState.theme.palette.chosen

/**
 * Dividers and rules: the subtle end, and **not** a control's edge.
 *
 * 1.50:1 against [Surface] in Dark and 1.49:1 in Air. That is right for a line between two rows — a
 * divider is meant to be barely there — and wrong for the outline of a control: WCAG 1.4.11 wants
 * 3:1 for the boundary of something you press. The theme used one value for both, so every
 * "outlined" button was outlined in a colour that is not visible; [Edge] is the second value that
 * fixes it without brightening every divider in the app.
 *
 * On the two light themes the rule is the *silver* in "silver lining": Air's is a pale blue-grey
 * (Radix slate step 7), Vermillion's a warm one (olive step 6).
 */
val Line: Color get() = ThemeState.theme.palette.line

/**
 * The edge of a control: an outline you can see.
 *
 * 3.39:1 against [Bg] and 3.09:1 against [Surface] in Dark — the floor for a non-text boundary, and
 * still quieter than [Chosen], which is the outline of a control that is *chosen* rather than
 * merely pressable. The light themes need a darker edge for the same 3:1, because there is no
 * lighter direction to spend.
 */
val Edge: Color get() = ThemeState.theme.palette.edge

/**
 * The shell's own colours: its ground, its default text, its cursor and the wash over a selection.
 *
 * **The terminal used to be black whatever the app was.** The cell grid arrives with the host's
 * default background as its default cells, and painting that made the shell a separate room from
 * the rest of the app: on a light theme the lower half of the screen went dark. The painter maps
 * the *default* cells to these values and leaves every cell a program coloured explicitly alone —
 * an explicit colour is the program's, a default is the terminal's, and this is the terminal's.
 */
val TerminalColors: TerminalPalette get() = ThemeState.theme.palette.terminal

/**
 * The palette, as Material sees it, for the theme that is active.
 *
 * A scheme existed before this, but it was built inline inside the overflow menu and wrapped only
 * that menu (deleted 2026-11-04: it had one item), so it was the single place in the app where
 * Material's defaults applied and everywhere else took a platform fallback. It is built from the
 * same tokens as everything else, and it is *light* or *dark* according to the theme's ground —
 * Material's own light scheme would otherwise put dark-on-dark where it computes a colour itself.
 */
@Composable
internal fun ldmuxColors(): ColorScheme {
    val p = ThemeState.theme.palette
    val scheme = if (ThemeState.theme.lightGround) lightColorScheme() else darkColorScheme()
    return scheme.copy(
        primary = p.accent,
        onPrimary = p.onAccent,
        surface = p.surface,
        onSurface = p.fg,
        onSurfaceVariant = p.dim,
        background = p.base,
        onBackground = p.fg,
        outline = p.edge,
        error = p.danger,
        onError = p.base,
    )
}

/**
 * The smallest a control may be, in either direction.
 *
 * Android's minimum touch target is 48dp and nothing in this app specified one: every
 * control's height was whatever its padding and its type happened to add up to, which
 * came to 24-34dp. That is small enough to miss, and it is the other half of why the
 * same control felt different in two places — the padding differed and nothing else
 * did. Stated once, applied by every control in this file.
 */
/**
 * The text style every `Text` in the app starts from, and the reason the app looked
 * thick.
 *
 * This exists because of a measurement. A launcher row came out **71.7dp** tall for two
 * lines of text, and the arithmetic says why. Material's `LocalTextStyle` is
 * `bodyLarge`, whose line height is **24sp** -- and a `Text` that asks for `fontSize =
 * 11.sp` overrides the size and not the line height. So an 11sp subtitle was reserving
 * 24sp of vertical space, plus Android's font padding of ≈4dp per line, which
 * comes to 56dp for two lines before the row's own padding adds anything.
 *
 * That is the whole of it, and no control-height setting could have fixed it. A minimum
 * height only binds when the content is *smaller* than it, and this content was larger.
 * Every density control in the app was fighting a line height nobody chose, which is why
 * turning the setting on moved six call sites and nothing you could see.
 *
 * Both halves are fixed here and nowhere else:
 *
 * - `lineHeight` in `em`, so it is proportional to the size actually asked for rather
 *   than to a style nobody selected. `1.2` is above the ~1.17 a font's own ascent and
 *   descent usually take, so nothing clips, and far below 24sp.
 * - no font padding, which is a platform extra of a few dp per line that exists to
 *   leave room for a font's ascent and descent rather than for text a person reads.
 */
internal val BodyStyle = TextStyle(
    lineHeight = 1.2.em,
    platformStyle = PlatformTextStyle(includeFontPadding = false),
)

internal val MinTouch = 48.dp

/**
 * The one row that gets less than [MinTouch], and only in portrait.
 *
 * Portrait's header sits above two rows of keys that are themselves above the
 * keyboard, so it is the only chrome in the app stacked three deep. It is also
 * navigation rather than typing, and it is wide rather than tall: each arrow is
 * ≈44dp across, so the target stays comfortable while the row stops competing
 * with the keys below it. Landscape keeps [MinTouch], where there is room and the
 * same row sits at the top of a panel with nothing above it.
 */
internal val DenseTouch = 34.dp

/**
 * How tall a control is, decided once, for the whole app.
 *
 * Both settings are real. 48dp is the platform's minimum touch target and the app
 * went a long time without one, at 24-34dp, which is small enough to miss; it is
 * also a lot of a phone screen to spend on chrome. Only the person holding the
 * phone knows which they would rather have, so it is a setting rather than a
 * judgement call made in a commit.
 */
internal data class Touch(
    /**
     * Minimum height of a control.
     *
     * This only binds when a control's own content is smaller than it, and for a long
     * time nothing was: every `Text` in the app reserved Material's 24sp line height
     * whatever size it asked for, so every control was already taller than any floor
     * and this number was decoration. With the line height fixed, the floor finally
     * does the work it was written to do.
     */
    val min: Dp,
    /** The one row that may be shorter than [min]. */
    val dense: Dp,
    /**
     * Vertical padding inside a control.
     *
     * The second half of the same story. Padding generous enough to set the height by
     * itself makes [min] irrelevant, so it has to move with the density too, or the
     * thin setting is a setting that changes only the controls that did not need
     * changing.
     */
    val pad: Dp,
    /** Vertical padding inside a list row, which is where a row's thickness comes from. */
    val rowPad: Dp,
    /**
     * The drawn size of a glyph inside a control.
     *
     * A floor cannot shrink content that is already bigger than it, so the one way to
     * get a control below its floor is for the mark inside it to be smaller. This is
     * that lever, and it is why the portrait menu and flip buttons -- the two that
     * carry a 18dp mark rather than the 20-22dp defaults -- are the ones that move.
     */
    val icon: Dp,
    /** The launcher's checkbox. Its tick is drawn from it, so the two move together. */
    val check: Dp,
)

/** The two densities, named once so neither is a pair of literals in two places. */
private val ThinTouch = Touch(min = 36.dp, dense = 30.dp, pad = 6.dp, rowPad = 6.dp, icon = 18.dp, check = 16.dp)

private val TallTouch = Touch(min = MinTouch, dense = DenseTouch, pad = 8.dp, rowPad = 8.dp, icon = 22.dp, check = 18.dp)

internal fun touchFor(compact: Boolean): Touch = if (compact) ThinTouch else TallTouch

/**
 * Corner radii, in dp, named once.
 *
 * Small on purpose. A 2–3 dp radius reads as a crisp edge with the corner taken off,
 * which is what a dense terminal UI wants; the 8 and 12 dp corners it replaces read as
 * soft buttons, and at these sizes the curve is a bigger visual event than the border
 * it belongs to. Anything larger than these is a modal, and says so by name.
 */
internal val RadiusSmall = RoundedCornerShape(2.dp)

internal val RadiusMedium = RoundedCornerShape(3.dp)

internal val RadiusLarge = RoundedCornerShape(4.dp)

/**
 * The app's touch scale, installed once at the root and read by every control.
 *
 * A composition local rather than a parameter because the alternative is threading
 * a `Dp` through every composable between the setting and the button, and a
 * parameter on thirty call sites is how a setting ends up reaching most of the
 * controls and not all of them. `static` is deliberate: nothing reads this *while*
 * composing in a way that would need tracking, and the value changes about once a
 * year, so a full recomposition of the subtree on change is exactly right.
 */
internal val LocalTouch = staticCompositionLocalOf { ThinTouch }

