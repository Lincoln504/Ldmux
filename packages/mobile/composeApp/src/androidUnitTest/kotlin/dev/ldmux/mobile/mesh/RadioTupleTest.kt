package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull

/**
 * A tuple typed as the radio prints it. The order is the firmware's, because the one place a user
 * reads a tuple is a radio's own output, and copying four numbers in a different order than the
 * screen they came from is how a frequency ends up in the bandwidth field.
 */
class RadioTupleTest {
    private val base = RadioPreset.LDMUX_PRIVATE

    @Test
    fun `the radio's own line, spaces and units, parses`() {
        val p = assertNotNull(parseRadioTuple("910.525 500 7 5", base))
        assertEquals(910.525, p.frequencyMhz)
        assertEquals(500.0, p.bandwidthKhz)
        assertEquals(7, p.spreadingFactor)
        assertEquals(5, p.codingRate)
    }

    @Test
    fun `units and commas are the same numbers to a person, so they are the same to the parser`() {
        val a = assertNotNull(parseRadioTuple("910.525 MHz, 500 kHz, SF7, CR5", base))
        val b = assertNotNull(parseRadioTuple("910.525,500,7,5", base))
        assertEquals(a.frequencyMhz, b.frequencyMhz)
        assertEquals(a.bandwidthKhz, b.bandwidthKhz)
        assertEquals(a.spreadingFactor, b.spreadingFactor)
        assertEquals(a.codingRate, b.codingRate)
    }

    @Test
    fun `power and path width are not part of which channel, and survive the edit`() {
        val p = assertNotNull(parseRadioTuple("910.525 500 7 5", base.copy(txPowerDbm = 14, pathHashSize = 3)))
        assertEquals(14, p.txPowerDbm)
        assertEquals(3, p.pathHashSize)
    }

    @Test
    fun `anything that is not four numbers in range is refused`() {
        // Refused rather than rounded: the caller is about to change a radio's frequency, and a
        // radio on "close enough" hears nobody.
        for (bad in listOf(
            "", "910.525", "910.525 500 7", "910.525 500 7 5 1",
            "abc 500 7 5", "910.525 500 7 x", "910.525 500 13 5", "910.525 500 7 9",
            "1020.1 500 7 5", "136.9 500 7 5", "910.525 501 7 5", "910.525 62 7 5",
            "910.525 500 4 5", "910.525 500 7 4",
        )) {
            assertNull(parseRadioTuple(bad, base), bad)
        }
    }

    @Test
    fun `a tuple printed and parsed again is the same tuple`() {
        val text = radioTupleText(base)
        val back = assertNotNull(parseRadioTuple(text, base))
        assertEquals(base.frequencyMhz, back.frequencyMhz)
        assertEquals(base.bandwidthKhz, back.bandwidthKhz)
        assertEquals(base.spreadingFactor, back.spreadingFactor)
        assertEquals(base.codingRate, back.codingRate)
    }
}
