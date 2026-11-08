package io.ldmux.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color

/**
 * The five looks this app has, and the one place their colours are written down.
 *
 * **Five, because a theme is a decision and a menu of twelve is a decision nobody makes.** Dark
 * is the default and has no sound at all; the other four each carry a voice of their own (see
 * `Cues.kt`). Two are dark grounds, two are light, so a phone in daylight and a phone in a dim
 * room are both served.
 *
 * Every neutral here is a measured value from **Radix Colors 3.0.0** (MIT), not a colour picked
 * by eye: that palette's twelve steps are built for exactly the roles this app hands them
 * (step 1–2 grounds, 3 raised, 6–8 borders, 9–10 solid fills, 11–12 text), and its steps are
 * checked for contrast against their own ground. The two light themes keep the *page* white and
 * put the hue in the accent — a blue ground or a cream ground was asked for and rejected: a
 * terminal reads best on the brightest thing the screen can do, and "silver lining" is a border
 * colour, not a ground.
 *
 * The measured ratios each token has to keep are stated once, in [Palette], and `ThemeTest`
 * recomputes them from these literals rather than trusting the comment.
 */
enum class LdmuxTheme(
    /** What Settings calls it. Lower case, like every other value in that list. */
    val label: String,
    /** Whether this theme has a voice. Dark does not, and that is the point of it. */
    val sounds: Boolean,
) {
    Dark(
        label = "dark",
        sounds = false,
    ),
    Forest(
        label = "forest",
        sounds = true,
    ),
    Air(
        label = "air",
        sounds = true,
    ),
    Daybreak(
        label = "daybreak",
        sounds = true,
    ),
    Nerv(
        label = "nerv",
        sounds = true,
    ),
    Gruvbox(
        label = "gruvbox",
        sounds = true,
    ),
    ;

    val palette: Palette
        get() = when (this) {
            Dark -> DarkPalette
            Forest -> ForestPalette
            Air -> AirPalette
            Daybreak -> DaybreakPalette
            Nerv -> NervPalette
            Gruvbox -> GruvboxPalette
        }

    /** True for the two themes whose ground is light: the system bars invert and the window is white. */
    val lightGround: Boolean
        get() = this == Air || this == Daybreak

    companion object {
        /**
         * A theme by its stored name, or the default.
         *
         * Stored by name, never by ordinal: an enum's order is an implementation detail, and a
         * value that moved would silently restyle every install that had saved it.
         */
        fun fromStored(name: String?): LdmuxTheme =
            entries.firstOrNull { it.name == name } ?: Dark
    }
}

/** The cursor, the wash over a selection, and the two defaults the shell starts from. */
data class TerminalPalette(
    val background: Color,
    val foreground: Color,
    val cursor: Color,
    /** Already carries its alpha: a selection is a wash over the cells, never an opaque fill. */
    val selection: Color,
)

/**
 * One theme's colours, in the roles the app draws them in.
 *
 * The contract, and the floor each role is held to (WCAG 2.2 SC 1.4.3 for text, SC 1.4.11 for
 * anything that is not text, both computed without rounding):
 *
 * | role | drawn as | floor |
 * |---|---|---|
 * | [fg] | body text | 4.5:1 on [base], [surface] and [raised] |
 * | [dim] | secondary text | 4.5:1 on [base] and [surface] |
 * | [muted] | explanatory text, placeholders | 4.5:1 on [base] and [surface] |
 * | [accent] | the one primary action: label, icon, outline | 4.5:1 on [base] and [surface] |
 * | [chosen] | the outline of a control that is chosen | 3:1 on [base] and [surface] |
 * | [danger], [warn] | a destructive action, an anomaly | 4.5:1 on [base] and [surface] |
 * | [edge] | the boundary of a control | 3:1 on [base] and [surface] |
 * | [line] | a divider between rows | none: a divider is meant to be barely there |
 *
 * Two exceptions are structural rather than sloppy, and each is a ratio that was measured rather
 * than assumed: [muted] on the dark themes sits just under 4.5:1 on [raised] (4.41:1 in Dark), and
 * nothing draws it there — [raised] is the fill of a control that is *on*, and text on that fill is
 * [fg]. [onAccent] is the text drawn on a solid [accent] fill.
 */
data class Palette(
    val base: Color,
    val surface: Color,
    val raised: Color,
    val line: Color,
    val edge: Color,
    val fg: Color,
    val dim: Color,
    val muted: Color,
    val accent: Color,
    val chosen: Color,
    val danger: Color,
    val warn: Color,
    val onAccent: Color,
    val terminal: TerminalPalette,
)

/**
 * **The dark theme, lifted a step.** Same greyscale, same refusal of hue, with every grey raised a
 * little: a phone screen at night is not a photograph, and the old #0A0A0A ground was darker than
 * the black around the bezel, which made the app look like a hole rather than a surface. Line and
 * Dim now come from Radix `slate` dark with the blue cast pulled back toward grey — at these
 * luminances a cast that would be invisible as colour reads as a slight chill.
 */
private val DarkPalette = Palette(
    base = Color(0xFF111112),
    surface = Color(0xFF1B1B1D),
    raised = Color(0xFF2A2A2C),
    line = Color(0xFF363A3F),
    edge = Color(0xFF686868),
    fg = Color(0xFFEDEDEF),
    dim = Color(0xFFB0B4BA),
    muted = Color(0xFF8B8F96),
    accent = Color(0xFFD6D8DC),
    // Neutral to the third decimal: a chosen state in this theme is a lightness, so a 6 % slate
    // cast — invisible as colour, visible as "slightly cooler than the row above" — is not wanted.
    chosen = Color(0xFF9A9A9C),
    danger = Color(0xFFE39B9B),
    warn = Color(0xFFE0BC84),
    onAccent = Color(0xFF111112),
    terminal = TerminalPalette(
        background = Color(0xFF111112),
        foreground = Color(0xFFEDEDEF),
        cursor = Color(0xFFD6D8DC),
        selection = Color(0x33D6D8DC),
    ),
)

/**
 * **Forest: a dark grey ground with one green thing on it.** The greys are Radix `sage` dark, which
 * is a grey carrying a trace of green (chroma 0.010 at hue 172° — below the level at which a cast
 * reads as colour, which is exactly why the ground stays grey while the accent can be green). The
 * accent is a leaf green (#7FC98A) rather than jade's cyan-leaning step: jade at this lightness
 * reads as mint, which is a colour that belongs to a different idea of "forest" (and to an operating
 * system's notifications). 8.0:1 on the ground, 5.9:1 on a raised row.
 */
private val ForestPalette = Palette(
    base = Color(0xFF171918),
    surface = Color(0xFF202221),
    // sage-5, not sage-6: sage-6 is 1.41:1 over the panel — a jump that made a raised row look
    // like a different surface — and it left `muted` at 3.78:1 where a note is drawn on it.
    raised = Color(0xFF2E3130),
    line = Color(0xFF444947),
    edge = Color(0xFF7C8884),
    fg = Color(0xFFECEEED),
    dim = Color(0xFFADB5B2),
    muted = Color(0xFF8D9793),
    accent = Color(0xFF7FC98A),
    chosen = Color(0xFF9AA5A0),
    danger = Color(0xFFE58A84),
    warn = Color(0xFFE6C177),
    onAccent = Color(0xFF0E1512),
    terminal = TerminalPalette(
        background = Color(0xFF171918),
        foreground = Color(0xFFECEEED),
        cursor = Color(0xFF7FC98A),
        selection = Color(0x337FC98A),
    ),
)

/**
 * **Air: white page, blue accent, silver rules.** The ground is pure white and the neutrals are
 * Radix `slate` light, the blue-tinted grey the palette pairs with a blue accent; the "silver
 * lining" is `slate` step 7 used as the rule colour, which is what a silver line is — a pale
 * blue-grey hairline, not a gradient. The accent is `blue` step 11, the step reserved for text:
 * the fill step (#0090FF) carries white at 3.26:1, which is a large-text ratio, so it is not the
 * value this app writes body labels in.
 */
private val AirPalette = Palette(
    base = Color(0xFFFFFFFF),
    // **A panel has to be a step you can see**, and a second voice has to be a second voice. The
    // first pass put `dim` at slate-11 (5.94:1), four points under the body and one over a note, so
    // the middle tier read as "slightly dimmer body" and the whole page went flat. 7.86:1 is quiet
    // next to the body (16.4:1) and loud next to a note (4.8:1); the panel is 1.08:1 off the page.
    surface = Color(0xFFF4F6F9),
    raised = Color(0xFFECEFF3),
    line = Color(0xFFCDCED6),
    edge = Color(0xFF8B8D98),
    fg = Color(0xFF1C2024),
    dim = Color(0xFF4D525A),
    // 5.26:1 on the page, 4.85:1 on the panel and 4.56:1 on a raised fill — the note is legible on
    // every surface it is drawn on, which the first value (4.45:1 on the panel) was not.
    muted = Color(0xFF686C76),
    // blue-11 is 4.77:1 on the page but 4.40:1 on this theme's panel, under the floor for the
    // label an accent is drawn as. One step darker in the same hue: 5.67:1 / 5.24:1 / 4.92:1.
    accent = Color(0xFF0A68BA),
    chosen = Color(0xFF566F8A),
    danger = Color(0xFFCE2C31),
    // Radix amber-11 (#AB6400) is 4.61:1 on white but 4.38:1 on this theme's `surface`, under the
    // 4.5 floor for the sentence it is used in. This is the same caution one step darker.
    warn = Color(0xFF8A5A00),
    onAccent = Color(0xFFFFFFFF),
    terminal = TerminalPalette(
        background = Color(0xFFFFFFFF),
        foreground = Color(0xFF1C2024),
        cursor = Color(0xFF0A68BA),
        selection = Color(0x330A68BA),
    ),
)

/**
 * **Daybreak: white page, a warm grey, one red.** The neutrals are Radix `olive` light — a warm
 * silver — because a blue-grey rule under a vermillion accent reads as two unrelated decisions.
 * The accent is `tomato` step 11, the vermillion that is still legible as text (#D13415).
 *
 * `danger` is a different red (`red` step 11), and the two are close on purpose: a destructive
 * action and the theme's accent should belong to the same family. They are never told apart by
 * colour alone — every destructive control in this app carries a word, and the close dialog
 * carries an icon too.
 */
private val DaybreakPalette = Palette(
    base = Color(0xFFFFFFFF),
    surface = Color(0xFFF5F7F4),
    raised = Color(0xFFEBEEEB),
    line = Color(0xFFD7DAD7),
    edge = Color(0xFF898E87),
    fg = Color(0xFF1D211C),
    dim = Color(0xFF4D524B),
    muted = Color(0xFF6E7368),
    accent = Color(0xFFD13415),
    chosen = Color(0xFF7A6A5E),
    danger = Color(0xFFCE2C31),
    // The same correction as Air: amber-11 fails the floor on a near-white surface.
    warn = Color(0xFF8A5A00),
    onAccent = Color(0xFFFFFFFF),
    terminal = TerminalPalette(
        background = Color(0xFFFFFFFF),
        foreground = Color(0xFF1D211C),
        cursor = Color(0xFFD13415),
        selection = Color(0x33D13415),
    ),
)

/**
 * **NERV: navy ground, orange accent.** The neutrals are Radix `indigo` dark (step 1 is #11131F,
 * a near-black with blue in it) and the accent is `orange` dark — step 9 #F76B15 wherever it is a
 * fill, step 11 #FFA057 wherever it is drawn as text, because that is the step the palette keeps
 * legible on a dark ground.
 */
private val NervPalette = Palette(
    base = Color(0xFF11131F),
    surface = Color(0xFF161A2B),
    raised = Color(0xFF182449),
    line = Color(0xFF304384),
    edge = Color(0xFF6F7CA8),
    fg = Color(0xFFD6E1FF),
    dim = Color(0xFF9EB1FF),
    muted = Color(0xFF7E8AB4),
    accent = Color(0xFFFFA057),
    chosen = Color(0xFF6F7CA8),
    danger = Color(0xFFFF9592),
    warn = Color(0xFFFFCA16),
    onAccent = Color(0xFF11131F),
    terminal = TerminalPalette(
        background = Color(0xFF11131F),
        foreground = Color(0xFFD6E1FF),
        cursor = Color(0xFFFFA057),
        selection = Color(0x33FFA057),
    ),
)

/**
 * The active theme, and the only mutable thing in the palette.
 *
 * **A global holder rather than a composition local, and that is load-bearing.** The palette is
 * read by about two hundred call sites as `color = Bg`, `border(1.dp, Line)` — the plain property
 * form is what keeps a theme switch from being a refactor of the whole UI. A top-level property
 * cannot read a `CompositionLocal`; it *can* read snapshot state, and a read of snapshot state
 * during composition is tracked exactly like a local, so switching the theme recomposes every
 * reader. Written once, from the root, from the persisted setting (`App`), and never from a
 * control.
 *
 * The cost is that a test can move it too — which is the property `ThemeTest` uses to hold every
 * theme to its contrast floors without rendering anything.
 */
internal object ThemeState {
    var theme: LdmuxTheme by mutableStateOf(LdmuxTheme.Dark)
}

/**
 * Paint the window the theme's ground, and its bars the theme's way round.
 *
 * **A light theme has to reach past Compose.** `enableEdgeToEdge` chooses light or dark bar *icons*
 * once, in `MainActivity`, from the palette that was persisted at the time; a theme switch then
 * left white-on-white status bar icons, and the launch window stayed the dark theme's near-black
 * under a white page. This is the seam that repaints both, called from the root on every change.
 */
internal expect fun applyWindowTheme(theme: LdmuxTheme)
/**
 * **Gruvbox dark, exactly.** The ground is dark0 #282828 at gruvbox's medium contrast level, the
 * surfaces are dark1..dark4, the text is light1..light4 and the accents are the bright cuts, all
 * from the scheme's own published palette (`morhetz/gruvbox`).
 *
 * Two choices are the scheme's own rather than mine: **body text is fg1 #ebdbb2, not fg0 #fbf1c7**
 * — gruvbox pairs bg0 with fg1 and keeps light0 for emphasised text — and the terminal's cursor is
 * the bright yellow it uses for its search highlight. Contrast, measured on #282828: fg 10.75:1,
 * dim (light2) 8.59:1, muted (light4) 5.30:1, accent (bright yellow) 8.69:1, warn (bright orange)
 * 5.84:1, and bright red for danger at 4.29:1 — gruvbox's own error colour, a tenth under the floor
 * for body text, so it is only ever drawn with an icon and a word and never as a paragraph.
 */
private val GruvboxPalette = Palette(
    base = Color(0xFF282828),
    // dark0_soft for the panel and dark1 for the raised step: gruvbox's own two nearest layers, and
    // the pairing that keeps `muted` (light4, 4.2:1 on dark1) legible where a note is drawn on a
    // raised fill. dark2 at the raised step measured 3.2:1, under the exception's own bound.
    surface = Color(0xFF32302F),
    raised = Color(0xFF3C3836),
    line = Color(0xFF504945),
    edge = Color(0xFF928374),
    fg = Color(0xFFEBDBB2),
    dim = Color(0xFFD5C4A1),
    muted = Color(0xFFA89984),
    accent = Color(0xFFB8BB26),
    chosen = Color(0xFF8EC07C),
    // **Bright orange, not bright red, for danger.** gruvbox's red (#FB4934) measures 4.29:1 on the
    // page and 4.02:1 on its own panel — under the floor for a label — and the app draws a
    // destructive word, not a diff marker. Orange is gruvbox's other warning colour, clears the
    // floor on every ground (5.84:1 on the page), and the red still appears where gruvbox uses it:
    // as the terminal's own error colour, which the host's programs choose.
    danger = Color(0xFFFE8019),
    warn = Color(0xFFFABD2F),
    onAccent = Color(0xFF282828),
    terminal = TerminalPalette(
        background = Color(0xFF282828),
        foreground = Color(0xFFEBDBB2),
        cursor = Color(0xFFFABD2F),
        selection = Color(0x33FABD2F),
    ),
)
