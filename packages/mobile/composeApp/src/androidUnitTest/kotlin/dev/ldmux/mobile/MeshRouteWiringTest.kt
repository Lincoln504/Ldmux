package dev.ldmux.mobile

import dev.ldmux.mobile.mesh.MeshLinkState
import dev.ldmux.mobile.mesh.MeshRoute
import dev.ldmux.mobile.mesh.MeshRouteProblem
import dev.ldmux.mobile.mesh.MeshRouteResolution
import dev.ldmux.mobile.mesh.MobilePlatform
import dev.ldmux.mobile.mesh.availableRoutes
import dev.ldmux.mobile.mesh.meshLinkLabel
import dev.ldmux.mobile.mesh.meshRouteFromStored
import dev.ldmux.mobile.mesh.resolveRoute
import dev.ldmux.mobile.mesh.selectableRoutes
import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The mesh route as the rest of the app sees it: config, transport choice, link state, gating. */
class MeshRouteWiringTest {
    private val token = "e".repeat(64)
    private val tailnet = LdmuxConfig(host = "100.100.0.1:7625", token = token, deviceId = "d1", deviceLabel = "Moto")
    private val mesh = tailnet.copy(route = MeshRoute.Mesh)

    @Test
    fun `a device that never chose mesh behaves exactly as before`() {
        assertEquals(MeshRoute.Tailnet, LdmuxConfig.DEFAULT.route)
        assertEquals(MeshRoute.Tailnet, tailnet.route)
        assertNotNull(tailnet.endpoint)
        // The reconnect key is byte-for-byte what it was before the route existed.
        assertEquals("100.100.0.1:7625|$token|d1|Moto", tailnet.connectionKey)
        assertNull(tailnet.routeProblem())
        assertEquals(LinkState.Connecting, initialLink(tailnet))
        assertEquals(LinkState.Unpaired, initialLink(LdmuxConfig.DEFAULT))
    }

    private fun <T : Any> assertNotNull(v: T?): T = kotlin.test.assertNotNull(v)

    @Test
    fun `a saved mesh route opens nothing and says why, and never falls back to the tailnet`() {
        assertTrue(mesh.isPaired, "still paired with the machine")
        assertNull(mesh.endpoint, "the single choke point every socket is built from")
        assertEquals(LinkState.RouteUnavailable, initialLink(mesh))
        val problem = mesh.routeProblem()
        // Shells over mesh are not built, so that is the first thing the user is told.
        assertEquals(MeshRouteProblem.TerminalNotBuilt, problem)
        val label = linkLabel(LinkState.RouteUnavailable, mesh)
        assertTrue(label.startsWith(mesh.pairedName), label)
        assertTrue("mesh" in label && "not built" in label, label)
    }

    @Test
    fun `the route is a choice beside the pairing and not a second credential`() {
        val unpaired = mesh.unpaired()
        assertEquals(MeshRoute.Mesh, unpaired.route, "unpairing forgets the host, not the preference")
        assertEquals("", unpaired.token)
        assertEquals(LinkState.Unpaired, initialLink(unpaired))
        val store = ConfigStore(tailnet) { }
        store.update(store.config.value.copy(route = MeshRoute.Mesh))
        assertEquals(MeshRoute.Mesh, store.config.value.route)
    }

    @Test
    fun `choosing a route rebuilds the page and a display setting still does not`() {
        assertNotEquals(tailnet.connectionKey, mesh.connectionKey)
        assertEquals(mesh.connectionKey, mesh.copy(fontSp = 20f, side = "right", compact = false).connectionKey)
        assertEquals(tailnet.connectionKey, tailnet.copy(route = MeshRoute.Tailnet).connectionKey)
    }

    @Test
    fun `a mismatched pairing fails visibly at every step instead of being replaced`() {
        // Every ingredient present: the first unbuilt layer is what is reported.
        fun r(session: Boolean, terminal: Boolean, token: Boolean, peer: Boolean) =
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Android, token, peer, session, terminal)
        assertEquals(MeshRouteProblem.SessionNotBuilt, (r(false, false, true, true) as MeshRouteResolution.Unusable).problem)
        assertEquals(MeshRouteProblem.TerminalNotBuilt, (r(true, false, true, true) as MeshRouteResolution.Unusable).problem)
        assertEquals(MeshRouteProblem.NeedsToken, (r(true, true, false, true) as MeshRouteResolution.Unusable).problem)
        assertEquals(MeshRouteProblem.NeedsPeer, (r(true, true, true, false) as MeshRouteResolution.Unusable).problem)
        assertEquals(MeshRouteResolution.Use(MeshRoute.Mesh), r(true, true, true, true))
        // And an Unusable always names the route the user chose.
        assertEquals(MeshRoute.Mesh, (r(true, false, true, true) as MeshRouteResolution.Unusable).route)
    }

    @Test
    fun `ios has no mesh route to choose, to save or to run`() {
        assertEquals(listOf(MeshRoute.Tailnet), availableRoutes(MobilePlatform.Ios))
        assertEquals(listOf(MeshRoute.Tailnet), selectableRoutes(MobilePlatform.Ios, sessionBuilt = true, terminalBuilt = true))
        // Even with every layer built and every credential present, iOS refuses it.
        assertEquals(
            MeshRouteResolution.Unusable(MeshRoute.Mesh, MeshRouteProblem.NotOnThisPlatform),
            resolveRoute(MeshRoute.Mesh, MobilePlatform.Ios, true, true, true, true),
        )
    }

    @Test
    fun `no ios source set references the mesh package or the session library`() {
        // The iOS decision is "tailnet only, no MeshCore". It is enforced here, not just stated:
        // when an iosMain exists, nothing in it may mention the mesh route or the native session.
        val ios = File("src/iosMain")
        if (!ios.exists()) return
        val offenders = ios.walkTopDown().filter { it.isFile && it.extension in setOf("kt", "swift", "def", "h") }
            .filter { f ->
                val t = f.readText()
                listOf("dev.ldmux.mobile.mesh", "ldmux_session", "SessionLib", "MeshSession", "MeshGateway")
                    .any { it in t }
            }.map { it.path }.toList()
        assertEquals(emptyList(), offenders)
    }

    @Test
    fun `a mesh route is not offered where it can only fail`() {
        assertEquals(listOf(MeshRoute.Tailnet), selectableRoutes(MobilePlatform.Android, true, false), "shells not built")
        assertEquals(listOf(MeshRoute.Tailnet), selectableRoutes(MobilePlatform.Android, false, true), "no library")
        assertEquals(listOf(MeshRoute.Tailnet, MeshRoute.Mesh), selectableRoutes(MobilePlatform.Android, true, true))
    }

    @Test
    fun `a stored route round trips and unknown text is the tailnet`() {
        assertEquals(MeshRoute.Mesh, meshRouteFromStored(MeshRoute.Mesh.name))
        assertEquals(MeshRoute.Tailnet, meshRouteFromStored(MeshRoute.Tailnet.name))
        assertEquals(MeshRoute.Tailnet, meshRouteFromStored(null))
        assertEquals(MeshRoute.Tailnet, meshRouteFromStored("carrier pigeon"))
    }

    @Test
    fun `a mesh link that is down cannot be mistaken for a tailnet link that is down`() {
        val tailnetLabels = LinkState.entries.map { linkLabel(it, tailnet).substringAfter("${tailnet.pairedName} · ") }.toSet()
        for (s in MeshLinkState.entries) {
            val label = meshLinkLabel(s)
            assertTrue(label.startsWith("mesh · "), label)
            assertFalse(label in tailnetLabels, label)
        }
        assertEquals("mesh · host not answering", meshLinkLabel(MeshLinkState.SessionFailed))
    }
}
