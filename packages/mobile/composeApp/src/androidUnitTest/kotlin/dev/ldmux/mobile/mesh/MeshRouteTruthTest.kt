package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals

/** Every combination `resolveRoute` is asked about, with everything built (the shipped state). */
class MeshRouteTruthTest {
    private fun resolve(
        saved: MeshRoute = MeshRoute.Mesh,
        platform: MobilePlatform = MobilePlatform.Android,
        keys: Boolean = true, token: Boolean = true, peer: Boolean = true,
        session: Boolean = true, terminal: Boolean = true,
    ) = resolveRoute(saved, platform, hasToken = token, hasPeer = peer, sessionBuilt = session, terminalBuilt = terminal, hasSessionKeys = keys)

    private fun unusable(p: MeshRouteProblem) = MeshRouteResolution.Unusable(MeshRoute.Mesh, p)

    @Test fun `the shipped flags say both layers are built`() {
        assertEquals(true, MESH_SESSION_BUILT)
        assertEquals(true, MESH_TERMINAL_BUILT)
    }

    @Test fun `tailnet is always usable, whatever else is missing`() {
        assertEquals(MeshRouteResolution.Use(MeshRoute.Tailnet), resolve(saved = MeshRoute.Tailnet, keys = false, token = false, peer = false, session = false))
        assertEquals(MeshRouteResolution.Use(MeshRoute.Tailnet), resolve(saved = MeshRoute.Tailnet, platform = MobilePlatform.Ios))
    }

    @Test fun `mesh built and paired is usable`() = assertEquals(MeshRouteResolution.Use(MeshRoute.Mesh), resolve())

    @Test fun `mesh unpaired says what is missing, in a fixed order`() {
        assertEquals(unusable(MeshRouteProblem.NeedsSessionKeys), resolve(keys = false, token = false, peer = false))
        assertEquals(unusable(MeshRouteProblem.NeedsToken), resolve(token = false, peer = false))
        assertEquals(unusable(MeshRouteProblem.NeedsPeer), resolve(peer = false))
    }

    @Test fun `mesh on iOS is not available even with everything present`() =
        assertEquals(unusable(MeshRouteProblem.NotOnThisPlatform), resolve(platform = MobilePlatform.Ios))

    @Test fun `mesh with no library on this device says so before asking for credentials`() =
        assertEquals(unusable(MeshRouteProblem.SessionNotBuilt), resolve(session = false, keys = false, token = false))

    @Test fun `terminal not built is reported before credentials too`() =
        assertEquals(unusable(MeshRouteProblem.TerminalNotBuilt), resolve(terminal = false, keys = false, token = false))

    @Test fun `mesh is selectable only where it can work`() {
        assertEquals(listOf(MeshRoute.Tailnet, MeshRoute.Mesh), selectableRoutes(MobilePlatform.Android, sessionBuilt = true, terminalBuilt = true))
        assertEquals(listOf(MeshRoute.Tailnet), selectableRoutes(MobilePlatform.Android, sessionBuilt = false, terminalBuilt = true))
        assertEquals(listOf(MeshRoute.Tailnet), selectableRoutes(MobilePlatform.Ios, sessionBuilt = true, terminalBuilt = true))
    }
}
