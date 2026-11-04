package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The public-channel rule: when a screen may be drawn, when a sync starts and ends, and what a
 * wait that nobody answers does. The engine and the radio are not here, because the rule has no
 * engine and no radio in it.
 */
class PublicChannelHoldTest {
    @Test
    fun `off, every screen is drawn and nothing counts`() {
        val h = PublicChannelHold()
        assertTrue(h.acceptScreen(painted = true))
        assertTrue(h.acceptScreen(painted = false))
        assertEquals(0, h.updates)
        assertFalse(h.sync(pending = true), "the sync button only means something in the mode")
        assertFalse(h.awaiting)
    }

    @Test
    fun `on, the first screen is drawn and later ones are kept`() {
        val h = PublicChannelHold()
        h.set(true)
        // A frozen view before anything has been painted would be a blank terminal with no reason
        // on it, so the first screen always goes through.
        assertTrue(h.acceptScreen(painted = false))
        h.painted()
        // After that, screens are kept rather than drawn.
        assertFalse(h.acceptScreen(painted = true))
        assertFalse(h.acceptScreen(painted = true))
        assertEquals(2, h.updates, "the bar says how far behind the view is")
    }

    @Test
    fun `a sync with nothing waiting asks for the newest screen instead of sending`() {
        val h = PublicChannelHold()
        h.set(true)
        assertFalse(h.sync(pending = false), "nothing to send: the caller shows what it holds")
        assertTrue(h.awaiting, "but the view is still unfrozen, so the next screen draws")
    }

    @Test
    fun `a sync with something waiting starts the wait and the next screen ends it`() {
        val h = PublicChannelHold()
        h.set(true)
        assertTrue(h.sync(pending = true))
        assertTrue(h.awaiting)
        assertTrue(h.acceptScreen(painted = true), "the answer to a sync is drawn")
        h.painted()
        assertFalse(h.awaiting)
        assertEquals(0, h.updates)
    }

    @Test
    fun `a wait nobody answers times out once, and the view can be drawn`() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        assertTrue(h.timedOut())
        assertFalse(h.awaiting)
        assertFalse(h.timedOut(), "one timeout per wait: a second would clear a later wait's clock")
        // The mode is still on, so the next screen is kept again rather than drawn. The source
        // draws what it already holds when the wait times out (`pullHeld`), which is what ends the
        // freeze for this moment — not a second exemption here.
        assertFalse(h.acceptScreen(painted = true))
        assertEquals(1, h.updates)
        h.painted()
        assertEquals(0, h.updates)
    }

    @Test
    fun `the drain ends when the buffer is empty, not when the answer arrives`() {
        // The separation that keeps the promise: a sync sends, the answer can take seconds, and
        // typing during that wait must still stay on the phone. One flag for both would leak
        // every character typed while the answer was on its way.
        val h = PublicChannelHold()
        h.set(true)
        assertTrue(h.sync(pending = true))
        assertTrue(h.flushing)
        h.drained()
        assertFalse(h.flushing, "nothing left to send")
        assertTrue(h.awaiting, "but the answer is still on its way")
    }

    @Test
    fun `answering clears the drain as well as the wait`() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        h.painted()
        assertFalse(h.flushing)
        assertFalse(h.awaiting)
    }

    @Test
    fun `a second press while waiting is allowed, and still means send`() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        assertTrue(h.sync(pending = true), "the buffer refilled while waiting: send it too")
        assertTrue(h.awaiting)
    }

    @Test
    fun `turning the mode off clears the wait and the count`() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        h.set(false)
        assertFalse(h.on)
        assertFalse(h.awaiting)
        assertEquals(0, h.updates)
        assertTrue(h.acceptScreen(painted = true), "off means the view follows the host again")
    }

    @Test
    fun `setting the same value twice changes nothing`() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        h.set(true)
        assertTrue(h.awaiting, "a repeated set(true) must not clear a wait in flight")
    }
}
