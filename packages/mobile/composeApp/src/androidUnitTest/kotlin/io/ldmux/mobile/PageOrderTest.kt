package io.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The page order, and the number a row becomes on the launcher.
 *
 * Both exist because of one reported fault (2026-10-25): with several shells open,
 * the shell view slid between shells and came back on its own. The daemon orders
 * sessions by last activity, page order used to be the daemon's order, and every
 * shell that printed anything moved to the front of the pager.
 */
class PageOrderTest {

    private fun shell(id: String, cwd: String = "/tmp") =
        Session(
            id = id,
            kind = "shell",
            title = "",
            cwd = cwd,
            status = "running",
            keeperName = "ldmux-$id",
        )

    @Test
    fun `pages keep the order they were opened in, not the list's order`() {
        val opened = listOf("c", "a", "b")
        // The daemon sorts live sessions by activity, so the list it sends is not
        // the order the user opened them in — and it changes as shells print.
        val list = listOf(shell("a"), shell("b"), shell("c"))
        assertEquals(listOf("c", "a", "b"), pagesInOrder(opened, list).map { it.id })
        // The same three shells, with one of them having printed something.
        val churned = listOf(shell("b"), shell("c"), shell("a"))
        assertEquals(listOf("c", "a", "b"), pagesInOrder(opened, churned).map { it.id })
    }

    @Test
    fun `an id a stale list does not carry keeps its place`() {
        val opened = listOf("a", "b", "c")
        val missing = listOf(shell("a"), shell("c"))
        assertEquals(listOf("a", "c"), pagesInOrder(opened, missing).map { it.id })
        // When the list comes back, so does the page — at the index it had.
        val back = listOf(shell("c"), shell("b"), shell("a"))
        assertEquals(listOf("a", "b", "c"), pagesInOrder(opened, back).map { it.id })
    }

    @Test
    fun `a shell joins the order once, at the end`() {
        assertEquals(listOf("a", "b"), addToOrder(listOf("a"), listOf("b")))
        assertEquals(listOf("a", "b"), addToOrder(listOf("a", "b"), listOf("b")))
        assertEquals(listOf("a", "b", "c"), addToOrder(listOf("a"), listOf("b", "c")))
        // Two pages on one pty is two claims on one shell; the order cannot say it.
        assertEquals(listOf("a"), pagesInOrder(listOf("a", "a"), listOf(shell("a"))).map { it.id })
    }

    @Test
    fun `a chosen launcher row carries the page number it will get`() {
        val entries = launcherEntries(listOf(shell("a"), shell("b"), shell("c")))
        assertEquals(listOf("a", "b", "c"), entries.map { it.sessionId })
        val chosen = listOf("c", "a")
        assertEquals(1, ordinalIn(entries, chosen, "c"))
        assertEquals(2, ordinalIn(entries, chosen, "a"))
        assertNull(ordinalIn(entries, chosen, "b"))
        // A row that died after it was ticked must not keep its number taken:
        // `c` is gone from the list, so `a` is the first page now, not the second.
        val survived = launcherEntries(listOf(shell("a"), shell("b")))
        assertEquals(1, ordinalIn(survived, chosen, "a"))
        // And a ticked row the list does not carry has no number at all, rather
        // than the number it would have had.
        assertNull(ordinalIn(launcherEntries(listOf(shell("b"))), chosen, "c"))
    }
}
