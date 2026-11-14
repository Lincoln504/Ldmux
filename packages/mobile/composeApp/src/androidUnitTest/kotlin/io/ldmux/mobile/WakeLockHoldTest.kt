package io.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The wake lock is held for as long as the service runs, not for as long as the last start intent
 * is recent.
 *
 * The failure these pin: a phone left off screen on one link state takes the lock once and then
 * never again, because a start intent arrives only when the notification's text changes — so the
 * timed lock expired ten minutes into a session while the notification went on claiming the link
 * was open (2026-11-11, no app log line between 00:21 and 00:55 while walking out).
 */
class WakeLockHoldTest {
    private class FakeLock : PartialWakeLock {
        val acquires = mutableListOf<Long>()
        var releases = 0

        override fun acquire(timeoutMs: Long) {
            acquires += timeoutMs
        }

        override fun release() {
            releases++
        }
    }

    /** A clock a test moves by hand: exactly the seam `BackgroundRelease` already uses. */
    private class FakeTimer : ReleaseTimer {
        class Pending(val ms: Long, val action: () -> Unit) {
            var cancelled = false
            var fired = false
        }

        val scheduled = mutableListOf<Pending>()

        override fun schedule(delayMs: Long, action: () -> Unit): Cancellation {
            val p = Pending(delayMs, action)
            scheduled += p
            return Cancellation { p.cancelled = true }
        }

        /** Run each armed renewal once, as a real timer would: a fired one does not fire again. */
        fun fireDue() {
            val due = live
            due.forEach { it.fired = true }
            due.forEach { it.action() }
        }

        /** Armed renewals: scheduled, not cancelled, not yet fired. */
        val live: List<Pending> get() = scheduled.filter { !it.cancelled && it.fired.not() }
    }

    private fun newHold(lock: FakeLock, timer: FakeTimer, timeoutMs: Long = 1_000L) =
        WakeLockHold(lock, timer, timeoutMs)

    @Test
    fun `a held lock is taken again before its timeout lapses`() {
        val lock = FakeLock()
        val timer = FakeTimer()
        val hold = newHold(lock, timer, timeoutMs = 1_000L)

        hold.hold()
        assertEquals(listOf(1_000L), lock.acquires)
        assertEquals(1, timer.live.size, "one renewal is scheduled")
        assertEquals(500L, timer.live.single().ms, "renewed at half the timeout, before it lapses")

        timer.fireDue()
        assertEquals(listOf(1_000L, 1_000L), lock.acquires, "the lock is taken again")
        assertEquals(1, timer.live.size, "and the renewal arms itself for the next window")
    }

    @Test
    fun `a renewal keeps the lock held for as long as the service runs`() {
        val lock = FakeLock()
        val timer = FakeTimer()
        val hold = newHold(lock, timer, timeoutMs = 1_000L)

        hold.hold()
        repeat(6) { timer.fireDue() }

        assertEquals(7, lock.acquires.size, "one acquire plus six renewals")
        assertEquals(0, lock.releases, "never released while it is wanted")
        assertTrue(hold.held)
    }

    @Test
    fun `a repeated start does not take the lock or schedule a second renewal`() {
        val lock = FakeLock()
        val timer = FakeTimer()
        val hold = newHold(lock, timer, timeoutMs = 1_000L)

        hold.hold()
        hold.hold()
        hold.hold()

        assertEquals(1, lock.acquires.size)
        assertEquals(1, timer.live.size, "a start intent is not a heartbeat, and not a second timer")
    }

    @Test
    fun `letting go releases once and stops renewing`() {
        val lock = FakeLock()
        val timer = FakeTimer()
        val hold = newHold(lock, timer, timeoutMs = 1_000L)

        hold.hold()
        hold.letGo()

        assertEquals(1, lock.releases)
        assertFalse(hold.held)
        assertTrue(timer.live.isEmpty(), "no renewal is left armed")

        // A renewal that was already scheduled is cancelled, so the lock cannot come back.
        timer.fireDue()
        assertEquals(1, lock.acquires.size, "nothing re-acquired after the service stopped")

        hold.letGo()
        assertEquals(1, lock.releases, "letting go twice is not two releases")
    }

    @Test
    fun `the lock can be wanted again after the service was stopped`() {
        val lock = FakeLock()
        val timer = FakeTimer()
        val hold = newHold(lock, timer, timeoutMs = 1_000L)

        hold.hold()
        hold.letGo()
        hold.hold()

        assertEquals(2, lock.acquires.size)
        assertEquals(1, lock.releases)
        assertEquals(1, timer.live.size, "one renewal, not the cancelled one plus a new one")

        timer.fireDue()
        assertEquals(3, lock.acquires.size)
    }

    @Test
    fun `the production timeout renews well before ten minutes`() {
        val lock = FakeLock()
        val timer = FakeTimer()

        newHold(lock, timer, timeoutMs = WAKE_LOCK_MAX_MS).hold()

        assertEquals(listOf(WAKE_LOCK_MAX_MS), lock.acquires)
        assertEquals(WAKE_LOCK_MAX_MS / 2, timer.live.single().ms)
        assertTrue(timer.live.single().ms < WAKE_LOCK_MAX_MS, "a renewal that cannot race the expiry")
    }
}
