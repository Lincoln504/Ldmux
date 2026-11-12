package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * **One radio, one engine, two screens.**
 *
 * The terminal and the settings pages both hold the engine, so closing one must not close the
 * GATT link the other is still drawing on (`docs/RADIO-PLAN.md` §12.7d: a second owner sending
 * commands is `RC-2`'s shape). This is the rule, tested without a radio: it needs no adapter, no
 * native library and no device, and it is the part of the sharing that can be wrong.
 */
class HoldersTest {
    @Test
    fun `the first release does not close an engine a second screen holds`() {
        val holders = Holders()
        holders.retain() // the settings page joined
        assertFalse(holders.release(), "the terminal's release closed a shared engine")
        assertTrue(holders.release(), "the last release did not close the engine")
    }

    @Test
    fun `an engine with one holder closes on its one release`() {
        assertTrue(Holders().release())
    }

    @Test
    fun `a double release never goes negative and keeps answering closed`() {
        val holders = Holders()
        assertTrue(holders.release())
        // A caller that released twice is a bug, and the bug must not reopen the engine by taking
        // the count below zero.
        assertTrue(holders.release(), "a second release answered 'still open'")
        holders.retain()
        assertTrue(holders.retain() == Unit)
        assertFalse(holders.release(), "a retain after two releases left the count at zero")
        assertTrue(holders.release())
    }
}
