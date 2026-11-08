package io.ldmux.mobile

import androidx.compose.ui.graphics.Color
import io.ldmux.mobile.mesh.MeshLinkState
import kotlin.math.max
import kotlin.math.min
import kotlin.math.pow
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNotSame
import kotlin.test.assertTrue

/** WCAG 2.x relative luminance, the definition the contrast ratio is built from. */
private fun luminance(c: Color): Double {
    fun channel(v: Float): Double {
        val s = v.toDouble()
        return if (s <= 0.03928) s / 12.92 else ((s + 0.055) / 1.055).pow(2.4)
    }
    return 0.2126 * channel(c.red) + 0.7152 * channel(c.green) + 0.0722 * channel(c.blue)
}

/**
 * The contrast ratio between two opaque colours, **unrounded**.
 *
 * Unrounded on purpose: WCAG says a computed 4.499:1 does not meet 4.5:1, and rounding first is how
 * a palette passes a test it fails on screen.
 */
private fun contrast(a: Color, b: Color): Double {
    val la = luminance(a)
    val lb = luminance(b)
    return (max(la, lb) + 0.05) / (min(la, lb) + 0.05)
}

private fun need(ratio: Double, floor: Double, what: String) {
    assertTrue(ratio >= floor, "$what measures ${"%.2f".format(ratio)}:1, floor is $floor:1")
}

/**
 * The palette contract, held against every theme.
 *
 * These are the numbers `Palette`'s own documentation promises, recomputed from the literals rather
 * than trusted: a theme is a table of colours, and the failure that matters is not an ugly theme but
 * a theme whose explanatory text cannot be read in daylight.
 */
class ThemePaletteTest {
    @Test
    fun `body and secondary text clear 4_5 to 1 on every ground it is drawn on`() {
        for (theme in LdmuxTheme.entries) {
            val p = theme.palette
            for (ground in listOf("base" to p.base, "surface" to p.surface)) {
                need(contrast(p.fg, ground.second), 4.5, "${theme.name} fg on ${ground.first}")
                need(contrast(p.dim, ground.second), 4.5, "${theme.name} dim on ${ground.first}")
                need(contrast(p.muted, ground.second), 4.5, "${theme.name} muted on ${ground.first}")
                need(contrast(p.accent, ground.second), 4.5, "${theme.name} accent on ${ground.first}")
                need(contrast(p.danger, ground.second), 4.5, "${theme.name} danger on ${ground.first}")
                need(contrast(p.warn, ground.second), 4.5, "${theme.name} warn on ${ground.first}")
            }
            // Body text on the raised fill too: a control that is *on* carries a label.
            need(contrast(p.fg, p.raised), 4.5, "${theme.name} fg on raised")
        }
    }

    @Test
    fun `anything that is not text clears 3 to 1 where it is drawn`() {
        for (theme in LdmuxTheme.entries) {
            val p = theme.palette
            for (ground in listOf("base" to p.base, "surface" to p.surface)) {
                // A chosen state's outline, and the edge of a control: both are boundaries, which
                // WCAG 1.4.11 holds to 3:1 like any other non-text information.
                need(contrast(p.chosen, ground.second), 3.0, "${theme.name} chosen on ${ground.first}")
                need(contrast(p.edge, ground.second), 3.0, "${theme.name} edge on ${ground.first}")
            }
        }
    }

    @Test
    fun `a fill's label is legible on the fill`() {
        for (theme in LdmuxTheme.entries) {
            val p = theme.palette
            need(contrast(p.onAccent, p.accent), 4.5, "${theme.name} onAccent on accent")
        }
    }

    @Test
    fun `the documented exception stays inside its own bound`() {
        // `Palette` says muted is the one token that may sit under 4.5:1 on `raised`, and that
        // nothing draws it there. That claim is only honest while the number stays near the floor:
        // a theme that drifted to 3:1 would make the sentence false rather than exceptional.
        for (theme in LdmuxTheme.entries) {
            val p = theme.palette
            val r = contrast(p.muted, p.raised)
            assertTrue(r >= 3.7, "${theme.name} muted on raised fell to ${"%.2f".format(r)}:1")
        }
    }

    @Test
    fun `the shell's own colours are legible, and its selection is a wash`() {
        for (theme in LdmuxTheme.entries) {
            val t = theme.palette.terminal
            need(contrast(t.foreground, t.background), 4.5, "${theme.name} terminal fg on bg")
            need(contrast(t.cursor, t.background), 3.0, "${theme.name} terminal cursor on bg")
            // A selection that is opaque would hide the text it is selecting.
            assertTrue(t.selection.alpha < 0.5f, "${theme.name} terminal selection is not a wash")
        }
    }

    /**
     * **The tone has to be a hierarchy, not a set of nearby numbers.**
     *
     * Each token was chosen for its own contrast ratio and the set was never compared to itself,
     * which is how Air ended up with a secondary tier at 5.94:1: four points under the body and one
     * over a note, so "dim" read as "slightly dimmer body" and the page went flat. These bands are
     * the balance stated as a fact: the body leads, the second voice is genuinely second, a note is
     * legible and quiet, and each surface is a step you can see rather than a shade you cannot.
     */
    @Test
    fun `the tiers are in proportion, and every surface is a visible step`() {
        for (theme in LdmuxTheme.entries) {
            val p = theme.palette
            val body = contrast(p.fg, p.base)
            val second = contrast(p.dim, p.base)
            val note = contrast(p.muted, p.base)
            assertTrue(body >= 9.0, "${theme.name} body is only ${"%.2f".format(body)}:1")
            // A second voice, not a slightly dimmer first one, and not a note either.
            assertTrue(second >= 6.0, "${theme.name} dim is ${"%.2f".format(second)}:1, too quiet")
            assertTrue(
                second >= note * 1.3,
                "${theme.name} dim ${"%.2f".format(second)} is not a tier above muted ${"%.2f".format(note)}",
            )
            assertTrue(note >= 4.5 && note <= 6.6, "${theme.name} muted is ${"%.2f".format(note)}:1")
            // Clarity without shouting: the order is body, second voice, note, hairline.
            assertTrue(body > second && second > note, "${theme.name} tiers are out of order")
            // A panel has to be a step, and a hairline has to be a hairline.
            val panel = contrast(p.surface, p.base)
            val raised = contrast(p.raised, p.surface)
            assertTrue(panel >= 1.06, "${theme.name} surface is ${"%.3f".format(panel)}:1 off the page")
            assertTrue(raised >= 1.05, "${theme.name} raised is ${"%.3f".format(raised)}:1 off the panel")
            assertTrue(
                contrast(p.line, p.raised) <= 2.6,
                "${theme.name} line is a rule rather than a hairline",
            )
        }
    }

    @Test
    fun `the themes are different looks`() {
        assertEquals(6, LdmuxTheme.entries.size)
        // Six distinct palettes. Two of them share a *white ground* on purpose (Air and
        // Daybreak are both pages), so the test is over the whole palette rather than one token.
        val palettes = LdmuxTheme.entries.map { it.palette }
        assertEquals(palettes.size, palettes.toSet().size, "two themes are the same palette")
        val accents = palettes.map { it.accent }
        assertEquals(accents.size, accents.toSet().size, "two themes share an accent")
        // And the two dark ones differ where it matters, or "dark" would be two names for one look.
        assertNotEquals(LdmuxTheme.Dark.palette.base, LdmuxTheme.Forest.palette.base)
        assertNotEquals(LdmuxTheme.Dark.palette.base, LdmuxTheme.Nerv.palette.base)
    }

    @Test
    fun `the light themes are actually light, and the dark ones are not`() {
        for (theme in LdmuxTheme.entries) {
            val light = luminance(theme.palette.base) > 0.5
            assertEquals(theme.lightGround, light, "${theme.name} disagrees with its own lightGround")
        }
        // White, not cream: a light theme's page is within a few points of the brightest value.
        for (theme in LdmuxTheme.entries.filter { it.lightGround }) {
            need(contrast(theme.palette.base, Color.White), 1.0, "${theme.name} ground is white")
            assertTrue(luminance(theme.palette.base) >= 0.97, "${theme.name} ground is not near-white")
        }
    }
}

/**
 * The theme as a value: defaulted, persisted by name, and live.
 */
class ThemeSelectionTest {
    @Test
    fun `dark is the default, and a name is the stored form`() {
        assertEquals(LdmuxTheme.Dark, LdmuxConfig.DEFAULT.theme)
        assertTrue(LdmuxConfig.DEFAULT.sounds)
        assertEquals(LdmuxTheme.Dark, LdmuxTheme.fromStored(null))
        assertEquals(LdmuxTheme.Dark, LdmuxTheme.fromStored(""))
        assertEquals(LdmuxTheme.Dark, LdmuxTheme.fromStored("A theme we removed"))
        for (theme in LdmuxTheme.entries) {
            assertEquals(theme, LdmuxTheme.fromStored(theme.name))
        }
    }

    @Test
    fun `the config round-trips a theme and a sound switch`() {
        val config = LdmuxConfig.DEFAULT.copy(theme = LdmuxTheme.Air, sounds = false)
        assertEquals(LdmuxTheme.Air, config.theme)
        assertFalse(config.sounds)
        // Neither is a connection fact: changing the look must not rebuild a page and re-handshake.
        val dark = LdmuxConfig.DEFAULT.copy(host = "100.100.0.1:7625", token = "t", deviceId = "d")
        assertEquals(dark.connectionKey, dark.copy(theme = LdmuxTheme.Nerv, sounds = false).connectionKey)
    }

    @Test
    fun `switching the theme changes the tokens`() {
        val previous = ThemeState.theme
        try {
            ThemeState.theme = LdmuxTheme.Dark
            val darkBg = Bg
            val darkFg = Fg
            ThemeState.theme = LdmuxTheme.Air
            assertNotEquals(darkBg, Bg, "the ground did not change with the theme")
            assertNotEquals(darkFg, Fg, "the text colour did not change with the theme")
            assertEquals(LdmuxTheme.Air.palette.base, Bg)
            assertEquals(LdmuxTheme.Air.palette.fg, Fg)
        } finally {
            ThemeState.theme = previous
        }
    }

    @Test
    fun `every theme names its own two faces`() {
        for (theme in LdmuxTheme.entries) {
            assertNotSame(monoFontFor(theme), sansFontFor(theme), "${theme.name} uses one face twice")
        }
        // Not one shared family with a colour change: the mono face differs per theme.
        assertNotSame(monoFontFor(LdmuxTheme.Dark), monoFontFor(LdmuxTheme.Forest))
        assertNotSame(monoFontFor(LdmuxTheme.Air), monoFontFor(LdmuxTheme.Nerv))
    }
}

/**
 * The cues: who has a voice, and what a link change is worth saying.
 */
class CueTest {
    @Test
    fun `dark is silent and the other four are not`() {
        assertFalse(cueAudible(LdmuxTheme.Dark, sounds = true), "dark must never sound")
        for (theme in LdmuxTheme.entries.filter { it != LdmuxTheme.Dark }) {
            assertTrue(cueAudible(theme, sounds = true), "${theme.name} has no cue")
            assertFalse(cueAudible(theme, sounds = false), "${theme.name} ignored the switch")
        }
    }

    @Test
    fun `every sounding theme has all three cues, as different files`() {
        assertTrue(cueResources(LdmuxTheme.Dark).isEmpty(), "dark has no cue resources")
        for (theme in LdmuxTheme.entries.filter { it != LdmuxTheme.Dark }) {
            val table = cueResources(theme)
            assertEquals(Cue.entries.toSet(), table.keys, "${theme.name} is missing a cue")
            assertEquals(table.size, table.values.toSet().size, "${theme.name} reuses one file for two cues")
        }
        // Two themes must not share a sample: the whole point is that they sound different.
        val all = LdmuxTheme.entries.flatMap { cueResources(it).values }
        assertEquals(all.size, all.toSet().size)
    }

    @Test
    fun `a link change is a cue only when it is news`() {
        // Coming up is news; leaving Online for something that needs the operator is news.
        assertEquals(Cue.LinkUp, cueForTailnet(LinkState.Connecting, LinkState.Online))
        assertEquals(Cue.LinkDown, cueForTailnet(LinkState.Online, LinkState.Unreachable))
        assertEquals(Cue.Attention, cueForTailnet(LinkState.Online, LinkState.Rejected))
        // The same state twice is not a change, and the first Connecting of an attempt is not news.
        assertEquals(null, cueForTailnet(LinkState.Online, LinkState.Online))
        assertEquals(null, cueForTailnet(LinkState.Unreachable, LinkState.Connecting))
        assertEquals(null, cueForTailnet(LinkState.Online, LinkState.Unpaired))

        // The radio route: a failed session is a drop, a refusal is something to look at, and the
        // radio binding (`Bound`) is deliberately silent — a handshake has not happened yet.
        assertEquals(Cue.LinkDown, cueForMesh(MeshLinkState.Bound, MeshLinkState.SessionFailed))
        assertEquals(Cue.LinkDown, cueForMesh(MeshLinkState.Bound, MeshLinkState.RadioUnavailable))
        assertEquals(Cue.Attention, cueForMesh(MeshLinkState.Bound, MeshLinkState.Refused))
        assertEquals(Cue.Attention, cueForMesh(MeshLinkState.Connecting, MeshLinkState.NoPath))
        assertEquals(null, cueForMesh(MeshLinkState.Connecting, MeshLinkState.Bound))
        assertEquals(null, cueForMesh(MeshLinkState.Bound, MeshLinkState.Closed))
    }
}

/**
 * **The dark theme selects with lightness, not with hue.**
 *
 * There is no accent colour in it on purpose: on a screen that is otherwise a terminal's own output,
 * the only thing a hue could mean is "this is the important one", and the app says that with weight
 * and contrast instead. Everything that marks a choice — the chosen state, the accent token, the
 * terminal's cursor and its selection wash — is achromatic, and the themes that do carry a hue carry
 * it in the same tokens.
 */
class ThemeHueTest {
    private fun saturation(c: Color): Float {
        val max = maxOf(c.red, c.green, c.blue)
        val min = minOf(c.red, c.green, c.blue)
        return if (max <= 0f) 0f else (max - min) / max
    }

    @Test
    fun `dark marks a choice with lightness`() {
        val p = LdmuxTheme.Dark.palette
        for ((name, colour) in listOf(
            "accent" to p.accent,
            "chosen" to p.chosen,
            "cursor" to p.terminal.cursor,
            "selection" to p.terminal.selection,
        )) {
            assertTrue(saturation(colour) < 0.06f, "dark $name carries a hue (${saturation(colour)})")
        }
    }

    @Test
    fun `a theme with a hue carries it in the same tokens`() {
        for (theme in LdmuxTheme.entries - LdmuxTheme.Dark) {
            val p = theme.palette
            assertTrue(saturation(p.accent) > 0.25f, "${theme.name} accent has no hue")
            // The terminal's cursor is a hue in every voiced theme, so the cell that says "you are
            // here" is not a grey one: gruvbox keeps its own bright yellow (the scheme's own cursor
            // colour), the rest use their accent.
            assertTrue(
                saturation(p.terminal.cursor) > 0.25f,
                "${theme.name} cursor has no hue",
            )
        }
    }
}
