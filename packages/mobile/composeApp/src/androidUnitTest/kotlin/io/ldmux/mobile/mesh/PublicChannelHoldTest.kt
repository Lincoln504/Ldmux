package io.ldmux.mobile.mesh

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
    fun `a screen arriving mid-drain is held, so the send is not cut in half`() {
        // `QueueFull` is the normal case on this link, so a send spanning several records is the
        // rule: a screen that arrives between two of them is the pty's reaction to a partial
        // command. Drawing it used to end the wait, which closed the drain and left the rest of the
        // typed text held with nothing in flight to send it — a two-line command sent only its
        // first line. The answer is only the answer when the send is complete.
        val h = PublicChannelHold()
        h.set(true)
        assertTrue(h.sync(pending = true))
        assertFalse(h.acceptScreen(painted = true), "mid-drain: kept, not drawn")
        assertEquals(1, h.updates)
        assertTrue(h.drained(), "the buffer emptied")
        assertFalse(h.flushing)
        assertTrue(h.awaiting, "the answer is still owed")
        assertTrue(h.acceptScreen(painted = true), "and now it is drawn")
        h.painted()
        assertFalse(h.awaiting)
        assertEquals(0, h.updates)
    }

    @Test
    fun `a wait nobody answers reports, and keeps waiting`() {
        // A congested channel answers slowly rather than never, so the deadline reports and stops a
        // send that is going nowhere — but it does not cancel the wait, because the answer may still
        // be coming and the press the user already made must not be spent on nothing.
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        assertTrue(h.timedOut(), "the deadline says nothing has come")
        assertTrue(h.awaiting, "a slow channel still owes an answer")
        assertTrue(h.reported, "and the bar is told to say so")
        assertFalse(h.flushing, "the drain is cancelled: a send going nowhere costs airtime")
        // The answer arrives late: it is drawn, not held, so the user does not press again.
        assertTrue(h.acceptScreen(painted = true))
        h.painted()
        assertEquals(0, h.updates)
        assertFalse(h.awaiting)
        assertFalse(h.reported, "the report is spent once the answer arrives")
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
    fun `a paint does not end a drain, only the empty buffer or the deadline does`() {
        // This is the bug `painted` used to have: it cleared `flushing` too, contradicting its own
        // neighbour's doc ("clears when the buffer is empty, not when the answer arrives"), so any
        // paint during a drain parked the pump with text still held and nothing in flight.
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        h.painted()
        assertTrue(h.flushing, "a paint is not the drain ending")
        assertFalse(h.awaiting, "but the wait for the answer is over")
        assertTrue(h.drained())
        assertFalse(h.flushing)
        // A deadline does stop a drain: twenty seconds with text still held means the link is not
        // carrying records, and continuing to try costs airtime for a send the user is being told
        // may be down. The text stays counted, so the next press sends it. The wait survives.
        h.sync(pending = true)
        assertTrue(h.timedOut())
        assertFalse(h.flushing, "a deadline stops a send that is going nowhere")
        assertTrue(h.awaiting, "but the answer it asked for may still be coming")
    }

    @Test
    fun `a second press while waiting is allowed, and restarts the deadline`() {
        // The deadline effect is keyed on `seq`, so a press has to change it: a Boolean that is
        // already true cannot tell a re-launched effect that anything happened, and a press 15 s
        // into a 20 s wait would inherit 5 s of clock it did not earn.
        val h = PublicChannelHold()
        h.set(true)
        assertTrue(h.sync(pending = true))
        val first = h.seq
        assertTrue(h.sync(pending = true), "the buffer refilled while waiting: send it too")
        assertTrue(h.awaiting)
        assertTrue(h.seq > first, "every press is a new deadline")
    }

    @Test
    fun `the deadline reports without ending the wait, so a late answer is drawn`() {
        // The press the user already made must not be spent on nothing: on a congested channel the
        // answer is slow, not absent. Clearing the wait at the deadline meant the answer that came
        // ten seconds late was *held*, and the user had to press again to see it.
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        assertTrue(h.timedOut())
        assertTrue(h.reported, "the bar says nothing has come yet")
        assertTrue(h.awaiting, "and the answer is still owed, so it is drawn when it arrives")
        assertTrue(h.acceptScreen(painted = true), "the late answer is drawn, not held")
        h.painted()
        assertFalse(h.reported, "the report is spent once the answer arrives")
        assertFalse(h.awaiting)
    }

    @Test
    fun `show with nothing to send stops a wait left over from an earlier press`() {
        // "show" pressed on an empty buffer used to do nothing at all, so the bar went on saying
        // "waiting for the answer" for a send that was no longer in flight — a control reporting a
        // wait for something it cannot receive.
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        h.timedOut()
        assertTrue(h.awaiting)
        h.settle()
        assertFalse(h.awaiting, "nothing to send and nothing to show: stop saying waiting")
        assertFalse(h.reported)
    }

    @Test
    fun `a held screen drawn while waiting does not end the wait`() {
        // `drewHeld` is the paint a drain or a deadline makes: the count resets because the newest
        // screen is now on the glass, but the wait stays because the answer has not arrived.
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        assertFalse(h.acceptScreen(painted = true))
        assertEquals(1, h.updates)
        h.drewHeld()
        assertEquals(0, h.updates, "the newest held screen is drawn, so nothing is behind")
        assertTrue(h.awaiting, "but the answer is still owed")
    }

    @Test
    fun `drained says whether a drain was actually running`() {
        val h = PublicChannelHold()
        h.set(true)
        assertFalse(h.drained(), "idle batchers are polled: that is not a send finishing")
        h.sync(pending = true)
        assertTrue(h.drained())
        assertFalse(h.drained(), "once, not once per poll")
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
