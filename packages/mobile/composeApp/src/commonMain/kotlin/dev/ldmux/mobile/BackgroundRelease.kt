package dev.ldmux.mobile

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
) {
    private var pending: Cancellation? = null

    fun onBackground() {
        pending?.cancel()
        pending = null
        if (graceMs <= 0L) {
            release()
        } else {
            pending = timer.schedule(graceMs) {
                pending = null
                release()
            }
        }
    }

    fun onForeground() {
        pending?.cancel()
        pending = null
    }

    /** A release is waiting out its grace. */
    val waiting: Boolean get() = pending != null
}

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
