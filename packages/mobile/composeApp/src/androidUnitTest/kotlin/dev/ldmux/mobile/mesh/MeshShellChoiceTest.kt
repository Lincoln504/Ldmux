package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.openLabel
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull

/** The mesh shell list follows the tailnet's rule: tick, then open, and only what was ticked is a page. */
class MeshShellChoiceTest {
    private val a = SessionEntry("aaaa", "alpha")
    private val b = SessionEntry("bbbb", "beta")
    private val c = SessionEntry("cccc", "")

    @Test
    fun `the pages are the ticked shells in the order ticked, not every shell the host lists`() {
        val shells = MeshShells(listOf(a, b, c), current = "cccc")
        assertEquals(listOf("cccc", "aaaa"), meshPagesFor(shells, listOf("cccc", "aaaa")).map { it.id })
    }

    @Test
    fun `a shell attached with nothing ticked is the only page, however many the host runs`() {
        val shells = MeshShells(listOf(a, b), current = "bbbb")
        assertEquals(listOf("bbbb"), meshPagesFor(shells, emptyList()).map { it.id })
    }

    @Test
    fun `a ticked shell that left the host's list takes its page with it`() {
        val shells = MeshShells(listOf(a), current = "aaaa")
        assertEquals(listOf("aaaa"), meshPagesFor(shells, listOf("bbbb", "aaaa")).map { it.id })
    }

    @Test
    fun `before the host has listed anything the header still has the one shell we are on`() {
        val pages = meshPagesFor(MeshShells(emptyList(), current = null), emptyList())
        assertEquals(1, pages.size)
    }

    @Test
    fun `an unlabelled shell is named by the start of its id`() {
        val pages = meshPagesFor(MeshShells(listOf(c), current = "cccc"), listOf("cccc"))
        assertEquals("cccc", pages.single().title)
    }

    @Test
    fun `the open button counts what is ticked, exactly as the launcher's does`() {
        assertEquals("open", openLabel(0))
        assertEquals("open (2)", openLabel(2))
    }

    private fun live(painted: Boolean) = MeshTermView.Live(
        screen = MeshScreen(listOf("x"), 0, 0, alternate = false, dead = false, epoch = 1, cols = 80),
        absent = emptyList(), budgetLeft = null, stalled = false, resyncing = false, painted = painted,
    )

    @Test
    fun `a screen the host has not committed is never drawn, so a profile cannot black the terminal`() {
        assertNull(meshScreenToShow(live(painted = false)))
        assertNotNull(meshScreenToShow(live(painted = true)))
        assertNull(meshScreenToShow(MeshTermView.ProfileMissing))
    }
}
