package io.ldmux.mobile

import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Releasing the shells when the app leaves the screen is an explicit call the platform
 * lifecycle makes, not a Compose effect. These run with no composition, no frame clock
 * and no coroutine — which is exactly the environment of a suspended iOS process.
 */
class BackgroundReleaseTest {
    private class FakeTimer : ReleaseTimer {
        class Pending(val ms: Long, val action: () -> Unit, var cancelled: Boolean = false)

        val scheduled = mutableListOf<Pending>()

        override fun schedule(delayMs: Long, action: () -> Unit): Cancellation {
            val p = Pending(delayMs, action)
            scheduled += p
            return Cancellation { p.cancelled = true }
        }

        /**
         * Fire everything due, once.
         *
         * **A fired entry is spent.** Leaving it in the list made a second `fire()` re-run every
         * earlier action, which is invisible in a test that fires once and wrong in one that fires
         * a deferral window at a time.
         */
        fun fire() {
            val due = scheduled.filter { !it.cancelled }
            scheduled.clear()
            due.forEach { it.action() }
        }
    }

    @AfterTest
    fun restore() {
        AppLifecycle.set(true)
    }

    @Test
    fun `a zero grace releases synchronously inside the background call, with no timer`() {
        val timer = FakeTimer()
        var released = 0
        BackgroundRelease(0, timer, { released++ }).also { it.onBackground() }
        assertEquals(1, released)
        assertTrue(timer.scheduled.isEmpty())
    }

    @Test
    fun `a grace schedules the release and coming back cancels it`() {
        val timer = FakeTimer()
        var released = 0
        val r = BackgroundRelease(300_000, timer, { released++ })
        r.onBackground()
        assertEquals(0, released)
        assertTrue(r.waiting)
        assertEquals(300_000L, timer.scheduled.single().ms)
        r.onForeground()
        assertFalse(r.waiting)
        timer.fire()
        assertEquals(0, released)
    }

    @Test
    fun `an uncancelled grace releases once`() {
        val timer = FakeTimer()
        var released = 0
        val r = BackgroundRelease(1000, timer, { released++ })
        r.onBackground()
        timer.fire()
        assertEquals(1, released)
        assertFalse(r.waiting)
    }

    /**
     * **An owed operation holds the release, and only for a minute.**
     *
     * Dropping a page mid-command throws away the answer the operator is waiting for, and at range
     * the whole bootstrap that produced it. The deferral is bounded because an operation that never
     * answers must not keep a phone awake.
     */
    @Test
    fun `an owed operation defers the release, up to the budget`() {
        val timer = FakeTimer()
        var released = 0
        var owed = true
        val r = BackgroundRelease(1_000, timer, { released++ }) { owed }
        r.onBackground()
        // Each fire is one deferral window; the budget is a minute of them.
        repeat(3) {
            timer.fire()
            assertEquals(0, released, "the release fired while an operation was owed")
        }
        // Spend the rest of the budget.
        repeat(20) { timer.fire() }
        assertEquals(1, released, "the deferral never ended")
        assertFalse(r.waiting)
    }

    @Test
    fun `an operation that finishes inside the deferral releases at once`() {
        val timer = FakeTimer()
        var released = 0
        var owed = true
        val r = BackgroundRelease(1_000, timer, { released++ }) { owed }
        r.onBackground()
        timer.fire()
        assertEquals(0, released)
        owed = false
        timer.fire()
        assertEquals(1, released)
    }

    @Test
    fun `backgrounding twice does not leave a stale timer behind`() {
        val timer = FakeTimer()
        var released = 0
        val r = BackgroundRelease(1000, timer, { released++ })
        r.onBackground()
        r.onBackground()
        assertEquals(2, timer.scheduled.size)
        assertTrue(timer.scheduled[0].cancelled)
        timer.fire()
        assertEquals(1, released)
    }

    @Test
    fun `the platform's own lifecycle call is what drives it`() {
        val timer = FakeTimer()
        var released = 0
        var stopped = 0
        val removeRelease = AppLifecycle.setReleaseHandler(graceMs = 0, timer = timer) { released++ }
        val removeVoice = AppLifecycle.onBackground { stopped++ }
        AppLifecycle.set(false)
        assertEquals(1, released, "synchronous inside set(false)")
        assertEquals(1, stopped)
        assertFalse(AppLifecycle.foreground)
        AppLifecycle.set(true)
        // Repeating the same state is not a transition.
        AppLifecycle.set(true)
        AppLifecycle.set(false)
        assertEquals(2, released)
        removeRelease()
        removeVoice()
        AppLifecycle.set(true)
        AppLifecycle.set(false)
        assertEquals(2, released, "a removed handler is not called")
    }

    @Test
    fun `a handler that throws does not break the platform callback`() {
        val remove = AppLifecycle.setReleaseHandler(graceMs = 0, timer = FakeTimer()) { error("boom") }
        AppLifecycle.set(false)
        remove()
    }

    @Test
    fun `with a grace the lifecycle schedules and a return cancels`() {
        val timer = FakeTimer()
        var released = 0
        val remove = AppLifecycle.setReleaseHandler(graceMs = 45_000, timer = timer) { released++ }
        AppLifecycle.set(false)
        assertEquals(0, released)
        AppLifecycle.set(true)
        timer.fire()
        assertEquals(0, released)
        remove()
    }
}
