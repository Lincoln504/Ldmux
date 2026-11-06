package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshRoute
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * What the settings screen's `diagnostics` section says, as values.
 *
 * The failure this section exists to prevent is a value that looks authoritative and is not:
 * a "host" that is really a remembered address, a "route" that says tailnet while the radio is
 * selected, a pairing line that prints the token instead of a fingerprint. Each of those is one
 * string here, so each is one assertion rather than a screenshot.
 */
class DiagnosticsTest {

    private val token = "0123456789abcdef".repeat(4)

    private fun tailnet(route: MeshRoute = MeshRoute.Tailnet) = LdmuxConfig(
        host = "100.100.0.1:7625",
        token = token,
        pairName = "debian",
        deviceId = "d1",
        route = route,
    )

    private val meshOnly = LdmuxConfig(host = "", token = "", pairName = "debian", deviceId = "d1")

    @Test
    fun `a tailnet pairing shows the address it connects to`() {
        val s = connectionSummary(tailnet(), radioPaired = false)
        assertEquals("100.100.0.1:7625", s.host)
        assertEquals("tailnet", s.route)
        assertEquals("token 0123\u2026cdef", s.pairing)
        assertEquals("none chosen", s.radio)
        // A tailnet route's problem is never about missing radio keys.
        assertNull(s.problem)
    }

    @Test
    fun `a mesh-only install is radio only, not unpaired`() {
        // The screen must not send a radio-only user looking for a token they do not need.
        val s = connectionSummary(meshOnly.copy(route = MeshRoute.Mesh), radioPaired = true)
        assertEquals("radio only", s.host)
        assertEquals("LoRa radio", s.route)
        assertEquals("radio keys stored", s.pairing)
    }

    @Test
    fun `nothing paired says so, without inventing a fingerprint`() {
        val s = connectionSummary(LdmuxConfig(host = "", token = "", deviceId = "d1"), radioPaired = false)
        assertEquals("not paired", s.host)
        assertEquals("none", s.pairing)
    }

    @Test
    fun `the pairing line never prints the secret`() {
        val s = connectionSummary(tailnet(), radioPaired = false)
        assertTrue(!s.pairing.contains(token), s.pairing)
        assertTrue(s.pairing.contains("0123") && s.pairing.contains("cdef"), s.pairing)
    }

    @Test
    fun `both halves are named when both are held`() {
        val s = connectionSummary(tailnet(route = MeshRoute.Mesh), radioPaired = true)
        assertTrue(s.pairing.startsWith("token ") && s.pairing.endsWith("radio keys"), s.pairing)
        assertEquals("LoRa radio", s.route)
    }

    @Test
    fun `the radio row prefers the name it advertised, then the address`() {
        val named = tailnet().copy(meshRadio = "AA:BB:CC:DD:EE:FF", meshRadioName = "heltec-v3")
        assertEquals("heltec-v3", connectionSummary(named, radioPaired = true).radio)
        val bare = tailnet().copy(meshRadio = "AA:BB:CC:DD:EE:FF")
        assertEquals("AA:BB:CC:DD:EE:FF", connectionSummary(bare, radioPaired = false).radio)
    }

    @Test
    fun `a mesh route without its keys says why it cannot run`() {
        // The same question the launcher asks, so the two cannot disagree about a route that
        // is selected and unusable.
        val s = connectionSummary(meshOnly.copy(route = MeshRoute.Mesh), radioPaired = false)
        assertTrue(!s.problem.isNullOrBlank(), "a radio route with no keys must explain itself")
        val ok = connectionSummary(meshOnly.copy(route = MeshRoute.Mesh), radioPaired = true)
        assertNull(ok.problem)
    }

    // ── the log tail ──────────────────────────────────────────────────────────

    @Test
    fun `a tail under the limit is the whole list`() {
        assertEquals(listOf("a", "b"), logTail(listOf("a", "b"), 40))
    }

    @Test
    fun `a tail over the limit says how much it left out`() {
        val all = (1..10).map { "line $it" }
        val tail = logTail(all, 3)
        assertEquals(4, tail.size)
        assertEquals("\u2026 7 earlier lines", tail.first())
        assertEquals(listOf("line 8", "line 9", "line 10"), tail.drop(1))
    }

    @Test
    fun `one omitted line reads as one line`() {
        assertEquals("\u2026 1 earlier line", logTail(listOf("a", "b"), 1).first())
    }

    @Test
    fun `an empty or nonsensical limit yields nothing rather than everything`() {
        assertEquals(emptyList(), logTail(listOf("a"), 0))
        assertEquals(emptyList(), logTail(listOf("a"), -1))
        assertEquals(emptyList(), logTail(emptyList(), 40))
    }
}
