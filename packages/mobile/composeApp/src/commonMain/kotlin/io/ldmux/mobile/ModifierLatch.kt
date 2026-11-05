package io.ldmux.mobile

/** How long an armed Ctrl or Alt waits for its key before it lets go. */
const val LATCH_TIMEOUT_MS = 6_000L

/**
 * The sticky Ctrl/Alt latch, with a clock.
 *
 * A latch armed and then forgotten used to sit there until some unrelated key was
 * pressed minutes later, and that key went out as a chord nobody meant. A modifier
 * the user has not used within a few seconds is one they changed their mind about.
 *
 * The clock is injected (milliseconds, any monotonic origin) so expiry is a pure
 * function of time and tests do not sleep.
 */
class ModifierLatch(
    private val timeoutMs: Long = LATCH_TIMEOUT_MS,
    private val now: () -> Long,
) {
    private var armed = Modifiers()
    private var armedAt = 0L

    /** Arm or disarm [which]. Arming (re)starts the clock for everything armed. */
    fun toggle(which: StickyKey): Modifiers {
        val live = current()
        armed = live.toggled(which)
        armedAt = now()
        return armed
    }

    /** What is armed right now: nothing, once the timeout has passed. */
    fun current(): Modifiers {
        if (armed.armed && now() - armedAt >= timeoutMs) armed = Modifiers()
        return armed
    }

    /** Forget everything armed (a key spent it, or the user moved away). */
    fun clear() {
        armed = Modifiers()
    }

    /** Milliseconds until the armed modifiers lapse, or null when none are armed. */
    fun remainingMs(): Long? =
        if (!current().armed) null else (timeoutMs - (now() - armedAt)).coerceAtLeast(0)
}
