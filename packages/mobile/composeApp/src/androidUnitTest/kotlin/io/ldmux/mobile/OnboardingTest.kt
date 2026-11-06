package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshRoute
import io.ldmux.mobile.mesh.canUnpair
import io.ldmux.mobile.mesh.pairingNote
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The three decisions a first run makes, as values rather than as a body of composables.
 *
 * Each of these was wrong before 2026-11-06, and each was wrong silently: a radio-only
 * install could not be unpaired, a mesh-only pairing left the screen byte-identical to a
 * scan that did nothing, and the unpaired screen said two contradictory things at once.
 * A screenshot cannot hold any of that — it says what one screen looked like in one font
 * size — so the rules are functions and the tests hold them.
 */
class OnboardingTest {

    private fun tailnetPairing() = Pairing("100.100.0.1:7625", "ab".repeat(32), "debian")

    private fun meshOnlyPairing() = parseScannedPairing(
        "ldmux://pair?v=1&name=debian&mv=1&mk=" + "00".repeat(32) + "&psk=" + "a0".repeat(32),
    )!!

    private fun meshKeysPairing() = parseScannedPairing(
        "ldmux://pair?v=1&host=100.100.0.1:7625&token=" + "cd".repeat(32) + "&name=debian" +
            "&mv=1&mk=" + "00".repeat(32) + "&psk=" + "a0".repeat(32),
    )!!

    // ── what a pairing says afterwards ────────────────────────────────────────

    @Test
    fun `a mesh-only pairing says the radio is paired and the route is still the tailnet`() {
        // The case that looked like failure: the keys were stored, no tailnet fact changed, so
        // `connectionKey` did not change and the screen did not either.
        val note = pairingNote(meshOnlyPairing(), radioKeysStored = true, route = MeshRoute.Tailnet, hasMeshRoute = true)
        assertEquals("radio paired; the app is still on the tailnet", note?.text)
        assertTrue(note!!.useRadio, "the note comes with the one press that makes it usable")
    }

    @Test
    fun `a mesh-only pairing on the radio route is just a confirmation`() {
        val note = pairingNote(meshOnlyPairing(), radioKeysStored = true, route = MeshRoute.Mesh, hasMeshRoute = true)
        assertEquals("radio paired", note?.text)
        assertFalse(note!!.useRadio)
    }

    @Test
    fun `keys that could not be stored are said, not counted as paired`() {
        // A Keystore refusal leaves the tailnet half working and the radio half empty; a bare
        // "paired" would promise a route whose keys are not there.
        val note = pairingNote(meshKeysPairing(), radioKeysStored = false, route = MeshRoute.Tailnet, hasMeshRoute = true)
        assertTrue(note!!.text.contains("were not stored"), note.text)
    }

    @Test
    fun `a tailnet pairing that changed the config needs no line`() {
        // The header and the session list change by themselves; a note here would be a third
        // copy of the same fact.
        assertNull(pairingNote(tailnetPairing(), radioKeysStored = null, route = MeshRoute.Tailnet, hasMeshRoute = true))
    }

    @Test
    fun `a mesh-only code on a build with no radio route is refused in words`() {
        val note = pairingNote(meshOnlyPairing(), radioKeysStored = null, route = MeshRoute.Tailnet, hasMeshRoute = false)
        assertTrue(note!!.text.contains("no radio route"), note.text)
    }

    @Test
    fun `a mesh code whose keys stored is not a problem on the tailnet route`() {
        // `ldmux code --mesh` pairs both halves; the tailnet changed, so there is nothing left
        // to explain even though the radio keys also landed.
        assertNull(pairingNote(meshKeysPairing(), radioKeysStored = true, route = MeshRoute.Tailnet, hasMeshRoute = true))
    }

    // ── one unpair verb, two pairings ─────────────────────────────────────────

    @Test
    fun `a radio-only install has something to unpair`() {
        // `ldmux code --mesh-only`: no host, no token, keys in the Keystore. `isPaired` is false
        // and the row used to offer only `pair`, so the radio could never be forgotten.
        assertTrue(canUnpair(isPaired = false, radioPaired = true))
    }

    @Test
    fun `nothing paired means no unpair verb`() {
        assertFalse(canUnpair(isPaired = false, radioPaired = false))
        assertTrue(canUnpair(isPaired = true, radioPaired = false))
        assertTrue(canUnpair(isPaired = true, radioPaired = true))
    }

    @Test
    fun `the pairing row names a radio-only install as paired with a radio`() {
        val config = LdmuxConfig.DEFAULT
        assertEquals("unpaired", pairingRowSummary(config, radioPaired = false, meshPresent = true))
        assertEquals("radio only", pairingRowSummary(config, radioPaired = true, meshPresent = true))
        assertEquals(
            "debian · radio",
            pairingRowSummary(config.copy(host = "100.100.0.1:7625", token = "ab".repeat(32), pairName = "debian"), true, true),
        )
        assertEquals(
            "debian · radio not paired",
            pairingRowSummary(config.copy(host = "100.100.0.1:7625", token = "ab".repeat(32), pairName = "debian"), false, true),
        )
    }

    // ── the first screen ─────────────────────────────────────────────────────

    @Test
    fun `the unpaired screen names the host, both routes, and the command`() {
        val lines = unpairedSteps()
        assertEquals(3, lines.size)
        assertTrue(lines[0].contains("ldmuxd"), "what this needs: ${lines[0]}")
        assertTrue(lines[0].contains("Linux"), "what this needs: ${lines[0]}")
        // The two routes are a settings row; a blank screen cannot be where the user learns
        // that a radio is an option at all.
        assertTrue(lines[1].contains("tailnet") && lines[1].contains("LoRa"), "both routes: ${lines[1]}")
        // The one command onboarding needs. Losing it here makes the app look unfixable.
        assertTrue(lines[2].contains("ldmux code"), "the command: ${lines[2]}")
    }
}
