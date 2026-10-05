package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Which sessions become pages in the pager.
 *
 * A dead session keeps its keeper name in the registry until the record is pruned,
 * so "has a keeper name" was not the same question as "can be opened". The pager
 * spent pages on corpses, and a tap on one went nowhere with no explanation.
 */
class SessionAttachableTest {

    private fun session(status: String, keeperName: String = "ldmux-abc") =
        Session(id = "abc", kind = "shell", title = "shell", cwd = "/tmp", status = status, keeperName = keeperName)

    @Test
    fun `a running shell is a page`() {
        assertTrue(session("running").attachable)
    }

    @Test
    fun `a starting shell is a page, because it is on its way`() {
        assertTrue(session("starting").attachable)
    }

    @Test
    fun `a dead shell is never a page, however long the record lingers`() {
        assertFalse(session("dead").attachable)
    }

    @Test
    fun `a session with no terminal is not a page even while running`() {
        // An RPC-only session has nothing to attach to.
        assertFalse(session("running", keeperName = "").attachable)
    }

    @Test
    fun `the label falls back to the id when there is no title`() {
        assertEquals("abc", session("running").copy(title = "").label)
        assertEquals("shell", session("running").label)
    }
}