package io.ldmux.mobile

/**
 * The longest the foreground service's partial wake lock may be held without being taken again.
 *
 * A timed wake lock is a backstop, not a working limit: it exists so that a bug anywhere else
 * cannot keep the CPU awake indefinitely. A lock whose lifetime is meant to be "as long as the
 * service runs" therefore has to be taken again before this lapses, which is what [WakeLockHold]
 * does.
 */
internal const val WAKE_LOCK_MAX_MS = 10 * 60 * 1000L

/**
 * The platform wake lock, as this app needs it.
 *
 * Two methods and no timeout state of its own, so the rule about when the lock is held is plain
 * Kotlin that a JVM test can drive with a fake clock (see `WakeLockHoldTest`).
 */
internal interface PartialWakeLock {
    /** Take the lock for at most [timeoutMs], or take it again if it is already held. */
    fun acquire(timeoutMs: Long)

    /** Let it go. Not an error when it is not held. */
    fun release()
}

/**
 * Holds a partial wake lock for as long as the foreground service runs.
 *
 * **A start intent is not a heartbeat.** The lock used to be taken only from `onStartCommand`,
 * which the service receives when it is created or when the notification's text changes — so a
 * phone left off screen on one link state lost the CPU ten minutes in while the notification went
 * on claiming the link was open. That is the opposite of what the service is for: keeping the BLE
 * link, the retry ladder and the claim alive while the operator is not looking. (Measured
 * 2026-11-11: no app log line at all between 00:21 and 00:55 while walking out to 28 hops.)
 *
 * So the lock is taken again at half its timeout, from a timer the service owns, and it is held
 * exactly as long as [hold] has been called and [letGo] has not. Half rather than
 * `timeout - epsilon`: the renewal must not race the expiry, and a wake-up every five minutes
 * while the CPU is already awake costs nothing.
 *
 * If the renewal somehow cannot happen the lock lapses, which is the backstop doing its job.
 */
internal class WakeLockHold(
    private val lock: PartialWakeLock,
    private val timer: ReleaseTimer,
    private val timeoutMs: Long = WAKE_LOCK_MAX_MS,
) {
    private var pending: Cancellation? = null

    /** Whether this hold wants the lock: true from [hold] until [letGo]. */
    var held: Boolean = false
        private set

    /** The service is running: take the lock, and keep it taken. Safe to call on every start. */
    fun hold() {
        if (held) return
        held = true
        lock.acquire(timeoutMs)
        renewIn()
    }

    /** The service is stopping: let go, and stop renewing. Safe to call more than once. */
    fun letGo() {
        if (!held) return
        held = false
        pending?.cancel()
        pending = null
        lock.release()
    }

    private fun renewIn() {
        pending?.cancel()
        pending = timer.schedule(timeoutMs / 2) {
            pending = null
            if (!held) return@schedule
            lock.acquire(timeoutMs)
            renewIn()
        }
    }
}
