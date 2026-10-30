package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The mesh screen's body under the header: no state may leave it empty. */
class MeshBodyTest {
    private val live = MeshTermView.Live(
        screen = MeshScreen(emptyList(), 0, 0, false, false, 0),
        absent = emptyList(), budgetLeft = null, stalled = false, resyncing = false, painted = true,
    )

    @Test fun `every link state says something`() {
        for (link in MeshLinkState.entries) {
            val body = meshBodyFor(MeshTermView.LinkDown(link))
            assertNotNull(body, link.name)
            assertTrue(body.line.isNotBlank(), link.name)
        }
    }

    @Test fun `every close reason says something, including one nobody has met`() {
        for (reason in listOf(null, MeshTerm.Closed.NOT_FOUND, MeshTerm.Closed.AMBIGUOUS, MeshTerm.Closed.EXITED, MeshTerm.Closed.UNAVAILABLE, MeshTerm.Closed.VERSION, 99)) {
            assertTrue(meshBodyFor(MeshTermView.HostClosed(reason))!!.line.isNotBlank(), "$reason")
        }
    }

    @Test fun `no shells and an empty list are sentences`() {
        assertTrue(meshBodyFor(MeshTermView.NoShells)!!.line.isNotBlank())
        assertTrue(meshBodyFor(MeshTermView.Choose(emptyList()))!!.line.isNotBlank())
    }

    @Test fun `waits and populated lists draw their own thing`() {
        for (v in listOf(MeshTermView.Idle, MeshTermView.Handshaking, MeshTermView.ProfileMissing, MeshTermView.Listing, live)) {
            assertNull(meshBodyFor(v), v.toString())
        }
        assertNull(meshBodyFor(MeshTermView.Choose(listOf(SessionEntry("a", "bash")))))
    }

    @Test fun `a token is offered for exactly the links a token cures`() {
        for (link in MeshLinkState.entries) {
            assertEquals(meshLinkWantsHandoff(link), meshBodyFor(MeshTermView.LinkDown(link))!!.handoff, link.name)
        }
        assertTrue(!meshBodyFor(MeshTermView.HostClosed(null))!!.handoff)
    }

    @Test fun `the unreachable line names the address it tried`() {
        val body = meshBodyFor(MeshTermView.LinkDown(MeshLinkState.Unreachable), MeshGatewayEndpoint("127.0.0.1", 38211))!!
        assertTrue("127.0.0.1:38211" in body.line)
    }

    @Test fun `every route problem has a line and only a missing token offers the handoff`() {
        for (p in MeshRouteProblem.entries) {
            val body = meshRouteBodyFor(p)!!
            assertTrue(body.line.isNotBlank(), p.name)
            assertEquals(p == MeshRouteProblem.NeedsToken, body.handoff, p.name)
        }
        assertNull(meshRouteBodyFor(null))
    }
}
