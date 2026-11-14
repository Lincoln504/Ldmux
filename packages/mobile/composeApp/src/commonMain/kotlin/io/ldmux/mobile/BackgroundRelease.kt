package io.ldmux.mobile

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** A scheduled action that can still be called off. */
fun interface Cancellation {
    fun cancel()
}

/** Something that runs an action after a delay. A seam only so tests need no clock. */
fun interface ReleaseTimer {
    fun schedule(delayMs: Long, action: () -> Unit): Cancellation
}

/**
 * "The app left the screen: let go of the shells, after the platform's grace."
 *
 * This is the explicit call the platform lifecycle makes. It used to be a
 * `LaunchedEffect` with a `delay` in `App`, which depends on the Compose frame clock
 * and on a coroutine surviving: iOS suspends the process within seconds of
 * `didEnterBackground`, so an effect that "will run in 0 ms" may never run at all, and
 * the shells stay held (ADR-0012, ADR-0013). With a grace of 0 [onBackground] calls
 * [release] **synchronously**, on the platform's own callback, with no coroutine
 * between; with a grace it schedules one and [onForeground] cancels it.
 *
 * Not a Compose type, so the rule is testable on the JVM.
 */
class BackgroundRelease(
    private val graceMs: Long,
    private val timer: ReleaseTimer,
    private val release: () -> Unit,
    /**
     * Whether an operation is owed: a command or a fetch is in flight and its answer has not
     * arrived.
     *
     * **Dropping a page mid-operation throws away the answer the operator is waiting for**, and at
     * range it also throws away the bootstrap that produced it. The release waits for it, up to
     * [MAX_DEFER_MS], and then lets go anyway: an operation that never answers must not keep a
     * phone awake for ever.
     */
    private val owed: () -> Boolean = { false },
) {
    private var pending: Cancellation? = null
    /// How much of the deferral budget this backgrounding has already spent.
    private var deferredMs: Long = 0

    fun onBackground() {
        pending?.cancel()
        pending = null
        deferredMs = 0
        if (graceMs <= 0L) {
            release()
        } else {
            schedule(graceMs)
        }
    }

    private fun schedule(delayMs: Long) {
        pending = timer.schedule(delayMs) {
            pending = null
            val left = MAX_DEFER_MS - deferredMs
            if (owed() && left > 0) {
                // One more window, never more than the budget: the answer is worth waiting for, and
                // a phone is not.
                val wait = minOf(left, DEFER_STEP_MS)
                deferredMs += wait
                schedule(wait)
            } else {
                release()
            }
        }
    }

    fun onForeground() {
        pending?.cancel()
        pending = null
        deferredMs = 0
    }

    /** A release is waiting out its grace. */
    val waiting: Boolean get() = pending != null
}

/** How long the release may be deferred for an owed operation, in total. */
const val MAX_DEFER_MS: Long = 60_000

/** One deferral window. Short enough that the release is never far away. */
const val DEFER_STEP_MS: Long = 5_000

/** The production timer: a coroutine on the main dispatcher, owned by no composition. */
internal object CoroutineReleaseTimer : ReleaseTimer {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    override fun schedule(delayMs: Long, action: () -> Unit): Cancellation {
        val job = scope.launch {
            delay(delayMs)
            action()
        }
        return Cancellation { job.cancel() }
    }
}
