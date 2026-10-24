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
        // 40 ms assumed, halved, is under the ceiling.
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
        assertEquals(FramePacing.MIN_INTERVAL_MS, p.intervalMs)
        p.onRttSample(600)
        assertTrue(p.intervalMs < FramePacing.MAX_INTERVAL_MS, "one slow pong should not go straight to the floor")
        p.onRttSample(40)
        assertEquals(FramePacing.MIN_INTERVAL_MS, p.intervalMs, "a fast sample is adopted whole")
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

    /**
     * Prediction is a fact about the link, not a setting: it comes on when the
     * round trip is slow enough that painted text beats waiting for the echo.
     */
    @Test
    fun `prediction stays off while the link is unmeasured or fast`() {
        val p = FramePacing()
        assertFalse(p.predict, "an assumed round trip is not evidence of a slow link")
        p.onRttSample(40)
        assertFalse(p.predict)
    }

    @Test
    fun `prediction turns on above the threshold`() {
        val p = FramePacing()
        p.onRttSample((FramePacing.PREDICT_ON_MS + 30).toLong())
        assertTrue(p.predict)
    }

    @Test
    fun `prediction has hysteresis so a link on the boundary does not flap`() {
        val p = FramePacing()
        p.onRttSample((FramePacing.PREDICT_ON_MS + 10).toLong())
        assertTrue(p.predict)
        // Between the two thresholds: neither on nor off. Still on.
        val between = ((FramePacing.PREDICT_ON_MS + FramePacing.PREDICT_OFF_MS) / 2).toLong()
        p.onRttSample(between)
        assertTrue(p.predict, "between the thresholds the current answer stands")
        // Below the lower threshold it goes off, and stays off until the upper one.
        p.onRttSample((FramePacing.PREDICT_OFF_MS - 10).toLong())
        assertFalse(p.predict)
        p.onRttSample(between)
        assertFalse(p.predict, "between the thresholds the current answer stands, off too")
    }

    @Test
    fun `a dropped connection drops the prediction`() {
        val p = FramePacing()
        p.onRttSample(600)
        assertTrue(p.predict)
        p.onDisconnected()
        assertFalse(p.predict, "there is nothing to predict against on a dead link")
    }

    /**
     * Volume is sized by the link: a history page on a slow link is the largest
     * message the protocol carries, and it is followed by a full engine rebuild.
     */
    @Test
    fun `a slow link asks for a smaller history page`() {
        val p = FramePacing()
        assertFalse(p.slow, "an unmeasured link is not called slow")
        assertEquals(FramePacing.HISTORY_FIRST_LINES, p.historyPageLines())
        assertEquals(FramePacing.HISTORY_MAX_LINES, p.historyMaxLines())

        p.onRttSample((FramePacing.SLOW_ON_MS + 20).toLong())
        assertTrue(p.slow)
        assertEquals(FramePacing.HISTORY_FIRST_SLOW_LINES, p.historyPageLines())
        assertEquals(FramePacing.HISTORY_MAX_SLOW_LINES, p.historyMaxLines())
        assertTrue(
            FramePacing.HISTORY_FIRST_SLOW_LINES < FramePacing.HISTORY_FIRST_LINES,
            "the slow page must actually be smaller",
        )
    }

    @Test
    fun `the slow class has hysteresis too`() {
        val p = FramePacing()
        p.onRttSample((FramePacing.SLOW_ON_MS + 10).toLong())
        assertTrue(p.slow)
        val between = ((FramePacing.SLOW_ON_MS + FramePacing.SLOW_OFF_MS) / 2).toLong()
        p.onRttSample(between)
        assertTrue(p.slow, "between the thresholds the answer stands")
        p.onRttSample((FramePacing.SLOW_OFF_MS - 10).toLong())
        assertFalse(p.slow)
        p.onRttSample(between)
        assertFalse(p.slow)
    }
}
