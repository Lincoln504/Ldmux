package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class MeshRouteTest {
    @Test
    fun `iOS has no mesh route at all`() {
        assertEquals(listOf(MeshRoute.Tailnet), availableRoutes(MobilePlatform.Ios))
        assertEquals(listOf(MeshRoute.Tailnet, MeshRoute.Mesh), availableRoutes(MobilePlatform.Android))
    }

    @Test
    fun `tailnet always resolves, whatever the mesh state`() {
        for (p in MobilePlatform.entries) {
            assertEquals(MeshRouteResolution.Use(MeshRoute.Tailnet), resolveRoute(MeshRoute.Tailnet, p, false, false))
        }
    }

    @Test
    fun `a saved mesh route on iOS fails visibly and never becomes the tailnet`() {
        val r = resolveRoute(MeshRoute.Mesh, MobilePlatform.Ios, hasToken = true, hasPeer = true)
        assertEquals(MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NotOnThisPlatform), r)
    }

    @Test
    fun `mesh on Android needs a token first, then a peer, then it resolves`() {
        assertEquals(
            MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NeedsToken),
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, hasToken = false, hasPeer = false, sessionBuilt = true, terminalBuilt = true),
        )
        assertEquals(
            MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NeedsPeer),
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, hasToken = true, hasPeer = false, sessionBuilt = true, terminalBuilt = true),
        )
        assertEquals(
            MeshRouteResolution.Use(MeshRoute.Mesh),
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, hasToken = true, hasPeer = true, sessionBuilt = true, terminalBuilt = true),
        )
    }

    @Test
    fun `the session is built, the shell carrier is not, and mesh says so before asking for a token`() {
        assertEquals(true, MESH_SESSION_BUILT, "MeshSession exists and is tested against the real library")
        assertEquals(false, MESH_TERMINAL_BUILT, "flip with the compact terminal encoding and the shell bridge")
        val expected = MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.TerminalNotBuilt)
        assertEquals(expected, resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, hasToken = false, hasPeer = false, sessionBuilt = true))
        assertEquals(expected, resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, hasToken = true, hasPeer = true, sessionBuilt = true))
        // A device that could not load the library is told that first: it is the earlier layer.
        assertEquals(
            MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.SessionNotBuilt),
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, hasToken = true, hasPeer = true, sessionBuilt = false),
        )
        // iOS is still reported as its own, truer problem.
        assertEquals(
            MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NotOnThisPlatform),
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Ios, hasToken = true, hasPeer = true),
        )
    }

    @Test
    fun `this build knows which platform it is`() {
        assertEquals(MobilePlatform.Android, currentPlatform)
    }

    @Test
    fun `every link state has its own label and none claims delivery`() {
        val labels = MeshLinkState.entries.map { meshLinkLabel(it) }
        assertEquals(labels.size, labels.toSet().size, "states must be distinguishable on screen")
        assertTrue(labels.none { "deliver" in it.lowercase() })
        assertTrue(MeshRouteProblem.entries.map { meshProblemLabel(it) }.toSet().size == MeshRouteProblem.entries.size)
        assertFalse("deliver" in MESH_RATE_NOTICE.replace("not a delivered", "").lowercase())
    }

    @Test
    fun `the mesh route offers none of the terminal affordances`() {
        val m = capabilitiesOf(MeshRoute.Mesh)
        assertFalse(m.liveCursor); assertFalse(m.keystrokeEcho); assertFalse(m.streamingOutput)
        assertTrue(m.timestampReplies); assertTrue(m.budgetInOperations)
        val t = capabilitiesOf(MeshRoute.Tailnet)
        assertTrue(t.liveCursor && t.keystrokeEcho && t.streamingOutput)
    }
}
