package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import io.ldmux.mobile.parseHostPreset
import kotlin.test.assertTrue
import kotlin.test.assertFalse

/**
 * A tuple typed as the radio prints it. The order is the firmware's, because the one place a user
 * reads a tuple is a radio's own output, and copying four numbers in a different order than the
 * screen they came from is how a frequency ends up in the bandwidth field.
 */
class RadioTupleTest {
    private val base = RadioPreset.BENCH

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

    /// **The desktop's tuple, as the pairing code carries it.** Four numbers inside the firmware's
    /// own bounds, or nothing: a code that asks for a tuple the radio would refuse is not a tuple
    /// this app should apply.
    @Test
    fun `the desktop's tuple parses, and anything the radio would refuse does not`() {
        val p = parseHostPreset("910525,500000,7,5")!!
        assertEquals(910.525, p.frequencyMhz, 1e-9)
        assertEquals(500.0, p.bandwidthKhz, 1e-9)
        assertEquals(7, p.spreadingFactor)
        assertEquals(5, p.codingRate)
        // Bounds are the firmware's: frequency kHz, bandwidth Hz, SF 5..12, CR 5..8.
        assertNull(parseHostPreset("1000,500000,7,5"), "below the band")
        assertNull(parseHostPreset("910525,1000,7,5"), "bandwidth below the firmware's floor")
        assertNull(parseHostPreset("910525,500000,13,5"), "no such spreading factor")
        assertNull(parseHostPreset("910525,500000,7"), "three numbers is not a tuple")
        assertNull(parseHostPreset("910525,500000,7,5,9"), "five numbers either")
        assertNull(parseHostPreset("910525,500000,x,5"), "not a number")
    }

    /// A mismatch in power or path width does not stop two radios hearing each other, so it is not
    /// a mismatch this row reports.
    @Test
    fun `two tuples match on the four numbers and the path width, not on power`() {
        val a = RadioPreset(910.525, 500.0, 7, 5, 22, 1)
        // Power is the radio's own business (a regional limit), so it is not part of the channel.
        assertTrue(sameTuple(a, a.copy(txPowerDbm = 14)))
        assertFalse(sameTuple(a, a.copy(frequencyMhz = 914.25)))
        assertFalse(sameTuple(a, a.copy(bandwidthKhz = 62.5)))
        assertFalse(sameTuple(a, a.copy(spreadingFactor = 8)))
        assertFalse(sameTuple(a, a.copy(codingRate = 8)))
        // **The width is** part of the *row's* comparison: a phone on two bytes cannot follow a
        // route a one-byte public repeater built, so a host of one byte and a phone of two is a
        // mismatch the row must name.
        assertFalse(sameTuple(a, a.copy(pathHashSize = 2)))
        assertFalse(sameTuple(a, a.copy(pathHashSize = 3)))
        // ...but it is **not** a channel difference, and the panel must not say the link dies for
        // it: two radios on the same four numbers hear each other whatever their widths.
        assertTrue(sameChannel(a, a.copy(pathHashSize = 3)))
        assertFalse(sameChannel(a, a.copy(frequencyMhz = 914.25)))
    }
}
