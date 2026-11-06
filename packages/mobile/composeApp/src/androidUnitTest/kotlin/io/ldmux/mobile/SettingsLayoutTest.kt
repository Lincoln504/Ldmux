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
                SettingsGroup.Display,
            ),
            settingsGroups(hasMesh = true),
        )
    }

    @Test
    fun `a platform with one route has no link section`() {
        // iOS: the tailnet only. A choice of one is not a choice, and a row that cannot
        // change anything is worse than an absent row.
        assertEquals(
            listOf(SettingsGroup.Pairing, SettingsGroup.Diagnostics, SettingsGroup.Display),
            settingsGroups(hasMesh = false),
        )
    }

    @Test
    fun `pairing is one section, so both pairings are the same kind of thing`() {
        val groups = settingsGroups(hasMesh = true)
        assertEquals(1, groups.count { it == SettingsGroup.Pairing })
        // And it is the section both pairings are drawn in: nothing else may claim the
        // title, or the screen grows a second place to pair.
        assertEquals("pairing", SettingsGroup.Pairing.title)
        assertEquals(listOf("link", "pairing", "diagnostics", "display"), groups.map { it.title })
    }

    @Test
    fun `diagnostics sits after pairing and before display`() {
        // A pairing raises the question this section answers ("what am I paired with, and is it
        // working?"), and a support question outranks a font size. It is also read-only, so it
        // cannot become a second place to configure a link.
        val groups = settingsGroups(hasMesh = true)
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
        assertEquals("the phone's LoRa radio", routeConsequence(MeshRoute.Mesh))
        assertTrue(routeConsequence(MeshRoute.Mesh).contains("LoRa radio"))
        assertFalse(routeConsequence(MeshRoute.Tailnet).contains("LoRa"))
    }
}
