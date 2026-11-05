package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshRoute
import kotlin.test.Test
import kotlin.test.assertEquals

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
            listOf(SettingsGroup.Link, SettingsGroup.Pairing, SettingsGroup.Display),
            settingsGroups(hasMesh = true),
        )
    }

    @Test
    fun `a platform with one route has no link section`() {
        // iOS: the tailnet only. A choice of one is not a choice, and a row that cannot
        // change anything is worse than an absent row.
        assertEquals(
            listOf(SettingsGroup.Pairing, SettingsGroup.Display),
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
        assertEquals(listOf("link", "pairing", "display"), groups.map { it.title })
    }

    @Test
    fun `each route's consequence names the thing the row says`() {
        // The row's options are `tailnet` and `radio`; the line under them must not
        // reintroduce the word `lora`, which names a modulation nobody chose.
        assertEquals("over Tailscale", routeConsequence(MeshRoute.Tailnet))
        assertEquals("the phone's radio", routeConsequence(MeshRoute.Mesh))
        for (r in MeshRoute.entries) {
            val line = routeConsequence(r)
            assertEquals(false, line.contains("lora", ignoreCase = true), "route $r says lora")
        }
    }
}
