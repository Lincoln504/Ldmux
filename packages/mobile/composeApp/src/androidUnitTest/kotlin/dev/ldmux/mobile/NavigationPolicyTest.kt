package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * "Go to the shell I just created" must not outlive the shell. An outstanding request
 * blocks the rule that sends the user back to the list when none of their shells is
 * left, so one that could never arrive stalled the app on an empty terminal screen.
 */
class NavigationPolicyTest {
    private fun session(id: String, status: String = "running", keeper: String = "k-$id") =
        Session(id = id, kind = "shell", title = "", cwd = "/", status = status, keeperName = keeper)

    @Test
    fun `a created shell that became a page is arrived at`() {
        assertEquals(GotoState.Arrive, gotoState("a", listOf(session("a")), listOf("a"), connected = true))
    }

    @Test
    fun `a shell the list has not carried yet is waited for`() {
        assertEquals(GotoState.Wait, gotoState("a", emptyList(), emptyList(), connected = true))
    }

    @Test
    fun `a shell that is still starting is waited for, not abandoned`() {
        // No keeper name yet means not attachable *yet*; that is not a dead shell.
        val starting = session("a", status = "starting", keeper = "")
        assertEquals(GotoState.Wait, gotoState("a", listOf(starting), emptyList(), connected = true))
    }

    @Test
    fun `a shell that died before it arrived is abandoned`() {
        assertEquals(GotoState.Abandon, gotoState("a", listOf(session("a", status = "dead")), emptyList(), connected = true))
    }

    @Test
    fun `a dropped link abandons the request`() {
        assertEquals(GotoState.Abandon, gotoState("a", emptyList(), emptyList(), connected = false))
    }

    @Test
    fun `arriving wins over a dropped link`() {
        // The page exists; going to it is still right.
        assertEquals(GotoState.Arrive, gotoState("a", listOf(session("a")), listOf("a"), connected = false))
    }
}
