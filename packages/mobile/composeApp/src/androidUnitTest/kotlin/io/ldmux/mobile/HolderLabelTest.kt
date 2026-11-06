package io.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * The holder words the daemon sends are kinds, not sentences.
 *
 * The radio route sends `"mesh"` as a holder, which the notice rendered literally until
 * 2026-11-06: the operator saw "in use on mesh" and had no idea what a mesh was doing holding a
 * shell. The kind is still the wire's identity (the daemon compares it in several places), so the
 * mapping lives here, where it is displayed.
 */
class HolderLabelTest {
    @Test
    fun theKindWordsReadAsEnglish() {
        assertEquals("the radio link", holderLabel("mesh"))
        assertEquals("the desktop", holderLabel("desktop"))
        assertEquals("another phone", holderLabel("phone"))
        assertEquals("this machine", holderLabel("local"))
    }

    @Test
    fun aDeviceNameIsAlreadyASentence() {
        assertEquals("Pixel 9", holderLabel("Pixel 9"))
        assertEquals("debian", holderLabel("debian"))
    }

    @Test
    fun anUnknownKindIsNotInvented() {
        // A word this build does not know is shown as it arrived rather than guessed at: naming a
        // device "somewhere" would be inventing a fact.
        assertEquals("repeater", holderLabel("repeater"))
        assertEquals("", holderLabel(""))
    }
}
