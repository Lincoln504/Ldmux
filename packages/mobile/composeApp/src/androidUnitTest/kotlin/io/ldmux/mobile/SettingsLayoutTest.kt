package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshRoute
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The settings screen's shape, as values rather than as a body of composables.
 *
 * These are the two decisions in that screen that a later edit can silently undo:
 * **which section comes first**, and **whether the link's consequence line tells the
 * truth about the route**. Both were wrong before 2026-11-04 — `host` came first, so the
 * machine was read before the choice of how to reach it, and the mesh pairing lived in a
 * different section from the tailnet one.
 *
 * A screenshot would not hold either: it says what the order was on one device in one
 * font size. A list does.
 */
class SettingsLayoutTest {

    @Test
    fun `the link comes first, because it decides whether the rest applies`() {
        assertEquals(
            listOf(
                SettingsGroup.Link,
                SettingsGroup.Pairing,
                SettingsGroup.Diagnostics,
                SettingsGroup.Shells,
                SettingsGroup.Display,
            ),
            settingsGroups(),
        )
    }

    @Test
    fun `pairing is one section, so both pairings are the same kind of thing`() {
        val groups = settingsGroups()
        assertEquals(1, groups.count { it == SettingsGroup.Pairing })
        // And it is the section both pairings are drawn in: nothing else may claim the
        // title, or the screen grows a second place to pair.
        assertEquals("pairing", SettingsGroup.Pairing.title)
        assertEquals(listOf("link", "pairing", "diagnostics", "shells", "display"), groups.map { it.title })
    }

    @Test
    fun `diagnostics sits after pairing and before display`() {
        // A pairing raises the question this section answers ("what am I paired with, and is it
        // working?"), and a support question outranks a font size. It is also read-only, so it
        // cannot become a second place to configure a link.
        val groups = settingsGroups()
        assertEquals(
            groups.indexOf(SettingsGroup.Pairing) + 1,
            groups.indexOf(SettingsGroup.Diagnostics),
        )
        assertTrue(groups.indexOf(SettingsGroup.Diagnostics) < groups.indexOf(SettingsGroup.Display))
        assertEquals("diagnostics", SettingsGroup.Diagnostics.title)
    }

    @Test
    fun `each route's consequence names the thing the row says`() {
        // The row's options are `tailnet` and `LoRa radio`, and the consequence line has to
        // name the same thing: "radio" alone could be Bluetooth, and the phone's own voice.
        // The row's options are `tailnet` and `LoRa radio`, and the line under them has to
        // name the same thing. "radio" alone could be Bluetooth, and a line that says one noun
        // under buttons that say another is how the two drift apart.
        assertEquals("over Tailscale", routeConsequence(MeshRoute.Tailnet))
        assertEquals("phone's LoRa radio", routeConsequence(MeshRoute.Mesh))
        assertTrue(routeConsequence(MeshRoute.Mesh).contains("LoRa radio"))
        assertFalse(routeConsequence(MeshRoute.Tailnet).contains("LoRa"))
    }
}

/**
 * **The vertical rhythm is an ordering, not a set of numbers.**
 *
 * The screen used to run three schemes at once — a 16 dp column gap, an extra 20 dp spacer at every
 * section (36 dp, while its comment said 24), and 8 dp of row padding — so two labels were 32 dp
 * apart, a header sat 25 dp from its first row, and a note sat the same distance from the row it
 * described as from the row below it. These assertions are the rules that make a group read as a
 * group, and they hold at both densities because every value comes from the touch scale.
 */
class SettingsRhythmTest {
    private val thin = touchFor(compact = true)
    private val tall = touchFor(compact = false)

    @Test
    fun `a header is closer to its rows than two rows are to each other`() {
        for (t in listOf(thin, tall)) {
            assertTrue(Rhythm.headerToRows(t) < Rhythm.rowGap(t))
        }
    }

    @Test
    fun `a note attaches upward`() {
        for (t in listOf(thin, tall)) {
            assertTrue(
                Rhythm.noteAbove(t) < Rhythm.noteBelow(t),
                "a note must sit closer to the row above it than to the row below",
            )
        }
    }

    @Test
    fun `a section break is the largest space on the screen`() {
        for (t in listOf(thin, tall)) {
            assertTrue(Rhythm.sectionGap(t) > Rhythm.rowGap(t) * 2)
            assertTrue(
                Rhythm.sectionBreak(t).value > 0f,
                "the break has to add to the column gap",
            )
            assertTrue(Rhythm.contentPad(t) >= Rhythm.rowGap(t))
        }
    }

    @Test
    fun `the gaps grow with the density, so the setting is not half a setting`() {
        assertTrue(Rhythm.rowGap(tall) > Rhythm.rowGap(thin))
        assertTrue(Rhythm.sectionGap(tall) > Rhythm.sectionGap(thin))
        assertTrue(Rhythm.contentPad(tall) > Rhythm.contentPad(thin))
    }
}
