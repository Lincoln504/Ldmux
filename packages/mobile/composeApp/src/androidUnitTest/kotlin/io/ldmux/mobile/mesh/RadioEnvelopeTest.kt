package io.ldmux.mobile.mesh

import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * Every private preset the randomiser can produce is inside its region's envelope.
 *
 * The randomiser is the one place this app invents an operating point, and an operating point is
 * a legal statement: a centre that puts the occupied band over an edge, a power above the region's
 * limit, or a bandwidth the power argument does not cover is a radio transmitting where it may
 * not. The values were plausible before this test existed (902.25 to 927.25 at 500 kHz, 125 kHz
 * inside 869.4-869.65) but "plausible" was never checked, and the EU row carried 22 dBm against a
 * table whose own rows use 14.
 *
 * The assertion is on the **occupied** band, not the centre: half a bandwidth of margin at each
 * edge is what makes a 500 kHz channel centred on 902.25 legal rather than straddling 902.0.
 */
class RadioEnvelopeTest {

    private fun presets(region: RadioRegion, count: Int = 500): List<RadioPreset> {
        val rng = Random(region.ordinal * 7_919 + 1)
        return List(count) { privatePreset(region, rng) }
    }

    @Test
    fun `every generated preset lies inside its region's band`() {
        for (region in RadioRegion.entries) {
            val env = envelopeOf(region)
            for (p in presets(region)) {
                val half = p.bandwidthKhz / 2000.0
                assertTrue(
                    p.frequencyMhz - half >= env.freqMinMhz - 1e-9,
                    "$region: ${p.frequencyMhz} - $half is below ${env.freqMinMhz}",
                )
                assertTrue(
                    p.frequencyMhz + half <= env.freqMaxMhz + 1e-9,
                    "$region: ${p.frequencyMhz} + $half is above ${env.freqMaxMhz}",
                )
            }
        }
    }

    @Test
    fun `the randomiser uses the envelope's width, rate and power`() {
        for (region in RadioRegion.entries) {
            val env = envelopeOf(region)
            for (p in presets(region, 50)) {
                assertEquals(env.bandwidthKhz, p.bandwidthKhz, "$region bandwidth")
                assertEquals(env.codingRate, p.codingRate, "$region coding rate")
                assertEquals(env.txPowerDbm, p.txPowerDbm, "$region power")
                assertTrue(p.spreadingFactor in env.spreadingFactors, "$region SF ${p.spreadingFactor}")
            }
        }
    }

    @Test
    fun `the european band carries a duty cycle and a low power`() {
        // 869.4-869.65 MHz at 500 mW ERP is conditioned on a 10 % duty cycle, and the reference
        // table's own EU rows are 14 dBm. Both are properties of the band, not of a preference.
        val eu = envelopeOf(RadioRegion.EuUk)
        assertTrue(eu.requiresDutyCycle)
        assertTrue(eu.txPowerDbm <= 14, "the EU envelope is ${eu.txPowerDbm} dBm")
        assertTrue(eu.freqMinMhz >= 869.4 && eu.freqMaxMhz <= 869.65)
    }

    @Test
    fun `the bands without a mandated duty cycle say so`() {
        for (region in listOf(RadioRegion.UsCanada, RadioRegion.Australia, RadioRegion.NewZealand)) {
            assertEquals(false, envelopeOf(region).requiresDutyCycle, "$region")
        }
    }

    @Test
    fun `a preset in the european sub-band is recognised as needing the meter`() {
        // The check is on the tuple, not on a stored region: a radio can be moved by its own
        // preset dialog, by a custom tuple, or by another app entirely.
        assertTrue(dutyCycleRequired(RadioPreset(869.525, 250.0, 11, 5, 14, 1)))
        assertEquals(false, dutyCycleRequired(RadioPreset(910.525, 62.5, 7, 5, 22, 1)))
        assertEquals(false, dutyCycleRequired(RadioPreset(915.8, 250.0, 10, 5, 20, 1)))
    }

    @Test
    fun `every public preset sits inside its own region's band`() {
        // The public table is the community's, so it is not required to obey ldmux's private
        // envelopes — but a row that names a region should at least be in that region's ISM band,
        // and a typo in a frequency would otherwise be indistinguishable from a deliberate variant.
        for (c in PublicPresets) {
            val band = when (c.id.substringBefore('_')) {
                "us" -> 902.0..928.0
                "eu", "ch", "cz", "pt" -> 863.0..870.0
                "au", "nz" -> 915.0..928.0
                "jp" -> 915.0..930.0
                "vn" -> 920.0..925.0
                else -> 0.0..2000.0
            }
            assertTrue(c.preset.frequencyMhz in band, "${c.id} is at ${c.preset.frequencyMhz}")
        }
    }
}
