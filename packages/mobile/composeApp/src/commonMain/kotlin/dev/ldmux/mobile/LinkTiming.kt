package dev.ldmux.mobile

import kotlin.random.Random

/**
 * The reconnect and liveness arithmetic both sockets share, as pure functions.
 *
 * Two clients had two spellings of the same backoff (a doubling `waitMs` in one, a
 * shifted attempt count with jitter in the other) and their own copies of the
 * missed-pong counter. A policy stated twice drifts, and a policy buried in a
 * coroutine cannot be tested; both are fixed by having it here.
 */

/** Retry floor, doubled per attempt up to [RETRY_MAX_MS]. */
internal const val RETRY_BASE_MS = 400L
internal const val RETRY_MAX_MS = 10_000L

/**
 * How long a probe waits for a pong after the network changed underneath a socket
 * that still looks open. A radio that moved from Wi-Fi to LTE leaves the old socket
 * "connected" until three pings go unanswered (12-16 s); this cuts that to one short
 * wait without declaring a healthy link dead (a pong on a slow mobile link is well
 * under this).
 */
internal const val NETWORK_PROBE_MS = 2_500L

/** The pause before retry number [attempt] (0-based): 0.4 s doubling to a 10 s cap. */
internal fun reconnectDelayMs(attempt: Int): Long =
    (RETRY_BASE_MS shl attempt.coerceIn(0, 5)).coerceAtMost(RETRY_MAX_MS)

/**
 * [base] plus up to half again, so a daemon restart does not bring every page of
 * every phone back in lockstep. Zero stays zero (an immediate retry has no herd).
 */
internal fun withJitter(base: Long, random: Random = Random): Long =
    if (base <= 0) 0L else base + random.nextLong(base / 2 + 1)

/**
 * Counts consecutive heartbeats that went unanswered.
 *
 * Call [onPong] when a pong arrives and [beat] once per ping interval; [beat] answers
 * whether the link should now be declared dead. A ping nobody answers detects
 * nothing on its own: on a sleeping radio or a NAT that forgot the flow the OS says
 * nothing for a long time, so silence has to be counted.
 */
internal class MissedPongs(private val limit: Int = CLIENT_MISSED_PONGS) {
    private var seen = false
    private var outstanding = 0

    fun onPong() {
        seen = true
    }

    /** Treat the link as freshly answered (a new connection starts with a clean slate). */
    fun reset() {
        seen = true
        outstanding = 0
    }

    /** One interval has passed. True when [limit] intervals in a row had no pong. */
    fun beat(): Boolean {
        outstanding = if (seen) 0 else outstanding + 1
        seen = false
        return outstanding >= limit
    }
}
