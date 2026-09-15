package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Frame pacing is the "stay light on the network" policy, so its clamps are the
 * product: too eager and a phone burns battery repainting text nobody can see,
 * too slow and the terminal stops looking live.
 */
class FramePacingTest {

    @Test
    fun `an unmeasured link assumes a middling mobile round trip`() {
        val p = FramePacing()
        assertFalse(p.measured)
        // 40 ms assumed, halved, is under the 33 ms ceiling.
        assertEquals(FramePacing.MIN_INTERVAL_MS, p.intervalMs)
    }

    @Test
    fun `a fast link is capped at the frame-rate ceiling, not painted at 60 Hz`() {
        val p = FramePacing()
        p.onRttSample(2)
        assertEquals(FramePacing.MIN_INTERVAL_MS, p.intervalMs)
    }

    @Test
    fun `a slow link slows the frame rate toward its floor`() {
        val p = FramePacing()
        repeat(20) { p.onRttSample(600) }
        assertEquals(FramePacing.MAX_INTERVAL_MS, p.intervalMs)
    }

    @Test
    fun `the interval tracks half the round trip in between`() {
        val p = FramePacing()
        repeat(20) { p.onRttSample(200) }
        assertEquals(100L, p.intervalMs)
    }

    @Test
    fun `a queueing delay does not slow the display for long`() {
        // The estimate is asymmetric on purpose: a pong can be delayed by a large
        // snapshot in front of it, but that says nothing about the link. One
        // spiked sample must not halve the frame rate, and one good sample must
        // restore it immediately.
        val p = FramePacing()
        p.onRttSample(40)
        assertEquals(33L, p.intervalMs)
        p.onRttSample(600)
        assertTrue(p.intervalMs < 150L, "one slow pong should not go straight to the floor")
        p.onRttSample(40)
        assertEquals(33L, p.intervalMs, "a fast sample is adopted whole")
    }

    @Test
    fun `a dropped connection forgets the measurement`() {
        val p = FramePacing()
        repeat(10) { p.onRttSample(600) }
        assertEquals(FramePacing.MAX_INTERVAL_MS, p.intervalMs)
        p.onDisconnected()
        assertFalse(p.measured)
        assertEquals(FramePacing.MIN_INTERVAL_MS, p.intervalMs)
    }

    @Test
    fun `a nonsense sample is ignored`() {
        val p = FramePacing()
        p.onRttSample(-5)
        assertFalse(p.measured, "a negative round trip is not evidence of anything")
    }
}