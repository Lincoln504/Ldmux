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
        assertFalse(h.acceptScreen(painted = true), "kept again: the newest one is what show paints")
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
        assertTrue(h.drained(), "the buffer emptied")
        assertFalse(h.flushing)
        assertTrue(h.awaiting, "the answer is still owed")
        assertTrue(h.acceptScreen(painted = true), "and now it is drawn")
        h.drawn(2_000)
        h.painted()
        // **A paint no longer ends the wait.** The first screen after a send is usually the echo,
        // and the answer follows in more; ending the wait on the echo held the answer behind a
        // `show` press. The wait ends when the shell goes quiet.
        assertTrue(h.awaiting, "one screen is not the end of an answer")
        assertFalse(h.settleIfQuiet(2_500), "not quiet yet")
        assertTrue(h.settleIfQuiet(2_000 + PublicChannelHold.QUIET_MS), "quiet now")
        assertFalse(h.awaiting)
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
        // The answer arrives late: it is drawn, not held, so the user does not press again. The
        // wait itself ends when the shell goes quiet rather than on the first screen.
        assertTrue(h.acceptScreen(painted = true))
        h.drawn(9_000)
        h.painted()
        assertFalse(h.reported, "the report is spent once the answer arrives")
        assertTrue(h.awaiting, "the wait outlives a single screen")
        assertTrue(h.settleIfQuiet(9_000 + PublicChannelHold.QUIET_MS), "quiet ends it")
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
        h.drawn(1_000)
        h.painted()
        assertTrue(h.flushing, "a paint is not the drain ending")
        assertTrue(h.awaiting, "and a paint is not the answer arriving either")
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
        h.drawn(5_000)
        h.painted()
        assertFalse(h.reported, "the report is spent once the answer arrives")
        assertTrue(h.awaiting, "the wait ends on quiet, not on one screen")
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
        // Drawing the newest kept screen is what `show` does: the view is up to date, and the wait
        // stays because the answer has not arrived.
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        assertFalse(h.acceptScreen(painted = true))
        assertTrue(h.awaiting, "the answer is still owed after the kept screen is drawn")
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
    fun `turning the mode off clears the wait`() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true)
        h.set(false)
        assertFalse(h.on)
        assertFalse(h.awaiting)
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

    /// **A second press must not stop a send that is already running.**
    ///
    /// `sync(pending)` used to assign `flushing = pending`, so a press that landed while the
    /// batcher happened to be empty — mid-drain, after the pump had taken the last piece — turned
    /// the drain off. The pump then stopped with records still unsent: the user's text had left the
    /// batcher and never left the phone, the bar said "waiting for the answer", and `show` had
    /// nothing to draw because no answer was coming.
    @Test
    fun `pressing again while a drain runs does not end the drain`() {
        val h = PublicChannelHold()
        h.set(true)
        assertTrue(h.sync(pending = true, nowMs = 1_000), "the first press sends")
        assertTrue(h.flushing, "and starts a drain")
        assertFalse(h.sync(pending = false, nowMs = 1_500), "a second press with nothing new to add")
        assertTrue(h.flushing, "must leave the drain running")
        assertTrue(h.drained(), "and the drain ends only when the pump says it is empty")
        assertFalse(h.flushing)
    }

    @Test
    fun theHostsIdleMarkerEndsTheWaitAtOnce() {
        val h = PublicChannelHold()
        h.set(true)
        h.sync(pending = true, nowMs = 0)
        assertTrue(h.awaiting, "a press starts the wait")
        // The host says the pane has stopped changing. No four-second quiet window: the answer is
        // over, and the bar must stop saying it is waiting.
        assertTrue(h.answered(), "the marker ends a wait")
        assertFalse(h.awaiting, "the wait is over")
        assertFalse(h.reported, "and it is not reported as an overdue answer")
        // A marker with no wait running is not an event.
        assertFalse(h.answered(), "nothing to end")
    }

    /// **A show press unfreezes the view without claiming an answer is coming.**
    ///
    /// `awaiting` is the draw gate and a press with an empty buffer still sets it: the next screen
    /// is newer than the one the user asked to see. What must not be claimed is a wait for an
    /// answer to a send that never happened — the bar said "waiting for answer" over a screen that
    /// had already been drawn, for the whole quiet window.
    @Test
    fun `a press with nothing typed is not a wait for an answer`() {
        val h = PublicChannelHold()
        h.set(true)
        assertTrue(h.sync(pending = true, nowMs = 1_000))
        assertTrue(h.waitingOnSend, "something was sent, so an answer is owed")
        assertTrue(h.answered())
        assertFalse(h.waitingOnSend, "the answer ended it")

        assertFalse(h.sync(pending = false, nowMs = 2_000), "nothing to send")
        assertTrue(h.awaiting, "the view still follows the next screen")
        assertFalse(h.waitingOnSend, "but no answer is owed")
    }
}
