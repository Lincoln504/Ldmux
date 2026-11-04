package dev.ldmux.mobile

import androidx.compose.material3.darkColorScheme
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
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

/** The whole palette. Minimal, dark, one accent — no decoration. */
// Greyscale, deliberately. There is no accent *colour*: the only thing a hue
// could mean here is "this is the important one", and on a screen that is
// otherwise a terminal's own output, that is a claim worth making in weight and
// contrast instead. The one exception is the close confirmation, where the red is
// a warning rather than decoration.
val Bg = Color(0xFF0A0A0A)

val Fg = Color(0xFFEDEDED)

val Dim = Color(0xFF8C8C8C)

/**
 * The quiet end of the palette, and it is a **colour, not [Dim] with an alpha**.
 *
 * Explanatory lines and field placeholders used `Dim.copy(alpha = 0.6f)` and `0.7f`,
 * which over `Bg` works out to about 2.8:1 and 3.6:1 — under the 4.5:1 that body text
 * needs, on a screen that is otherwise high contrast. A faded colour is also not a
 * value anyone can look up. `#787878` on `#0A0A0A` is 4.5:1, which is the floor, and
 * it still reads as quieter than [Dim].
 */
val Muted = Color(0xFF787878)

/**
 * The one destructive colour: ending something.
 *
 * It lived as a literal in the close dialog, which is how a palette quietly grows a
 * second definition of its own warning.
 */
val Danger = Color(0xFFE08A8A)

/**
 * An anomaly, not a mood: the amber the app uses to say something is wrong but
 * recoverable. A shell with no window is the one such state a row can show, and
 * it should be rare enough that the eye stops on it.
 */
val Warn = Color(0xFFE0B080)

/** Nothing is tinted; "accent" is simply the lightest thing on screen. */
val Accent = Color(0xFFD0D0D0)

/**
 * The outline of a control that is *chosen*.
 *
 * Deliberately not [Accent]. A checkmark outline is a state, not an action, and at
 * `#D0D0D0` it read as the brightest thing on the launcher — brighter than the rows
 * it was describing. This is one clear step down: still unambiguous, no longer
 * competing with the one primary action on the screen.
 */
val Chosen = Color(0xFF9A9A9A)

/**
 * The fill for a control that is *on*: the menu button that says you are in a shell, and
 * the mark that says a window is out on the desktop.
 *
 * It used to be [Accent], the brightest value in the palette, so the menu button was a
 * near-white block in an app whose whole idea is that importance lives in weight and
 * contrast rather than in a hue. That reads as a light rather than as a state, and it
 * put the brightest thing in sight in the corner of every screen.
 *
 * A raised tone says the same thing without shouting. Deliberately not *barely* a
 * difference: `Bg` is #0A0A0A and this is #2E2E2E, which is better than four times the
 * luminance of the button beside it, so it reads as raised at a glance and still leaves
 * [Accent] meaning the one primary action on a screen.
 */
val Raised = Color(0xFF2E2E2E)

val Surface = Color(0xFF1A1A1A)

/**
 * The palette, as Material sees it.
 *
 * A scheme existed before this, but it was built inline inside the overflow menu and
 * wrapped only that menu (deleted 2026-11-04: it had one item), so it was the single
 * place in the app where Material's defaults applied and everywhere else took a
 * platform fallback. The same scheme, named once and hoisted to the root.
 */
internal val LdmuxColors = darkColorScheme(
    surface = Surface,
    onSurface = Fg,
    onSurfaceVariant = Dim,
    background = Bg,
    onBackground = Fg,
    primary = Accent,
)

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
 * 24sp of vertical space, plus Android's font padding of about 4dp per line, which
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
 * about 44dp across, so the target stays comfortable while the row stops competing
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

/** Rules and unselected outlines. */
val Line = Color(0xFF2A2A2A)
