package dev.ldmux.mobile

/**
 * How often the client is willing to repaint, given how long the link takes to
 * answer.
 *
 * A terminal does not need 60 Hz, and wanting it is what makes a phone client
 * feel heavy: every repaint wakes the CPU, and on a slow link the frames arrive
 * after they are useful anyway. So the interval follows the round trip —
 * `RTT / 2`, because that is roughly how long a keystroke's echo takes to
 * return, which is the cadence the user is actually watching.
 *
 * Clamped at both ends, and the clamps are the policy:
 *  * [minIntervalMs] is the ceiling on the frame rate (~30 Hz). Terminals
 *    stream text, not video, and the input path is protected separately — a
 *    saturated main thread delaying *typing* was the original reason to coalesce
 *    at all (docs/PREDICTIVE-ECHO.md), not a reason to paint at 60 Hz.
 *  * [maxIntervalMs] is the floor (~6.7 Hz). Past this the display stops
 *    looking live and starts looking broken, and a user trying to read a slow
 *    terminal is better served by the predictive echo than by more draws.
 *
 * The estimate is deliberately asymmetric. A *lower* sample is adopted at once,
 * because a single good round trip proves the link can do it and queueing behind
 * a large snapshot only ever makes samples look worse than the link is. A higher
 * sample is absorbed slowly, so one slow pong does not visibly stutter the
 * display.
 */
class FramePacing(
    private val minIntervalMs: Long = MIN_INTERVAL_MS,
    private val maxIntervalMs: Long = MAX_INTERVAL_MS,
) {
    private var smoothedRttMs: Double = NO_SAMPLE
    private var samples = 0

    /** The current repaint interval. */
    val intervalMs: Long
        get() {
            val rtt = if (samples == 0) ASSUMED_RTT_MS else smoothedRttMs
            return (rtt / 2.0).toLong().coerceIn(minIntervalMs, maxIntervalMs)
        }

    /** True once a real round trip has been measured (rather than assumed). */
    val measured: Boolean get() = samples > 0

    /** The smoothed round trip, for display. Assumed until measured. */
    val rttMs: Long get() = (if (samples == 0) ASSUMED_RTT_MS else smoothedRttMs).toLong()

    fun onRttSample(sampleMs: Long) {
        if (sampleMs < 0) return
        samples++
        val sample = sampleMs.toDouble()
        smoothedRttMs = if (samples == 1 || sample < smoothedRttMs) {
            sample
        } else {
            smoothedRttMs + (sample - smoothedRttMs) * RISE
        }
    }

    /** A dropped connection proves nothing about the link, so forget the sample. */
    fun onDisconnected() {
        samples = 0
        smoothedRttMs = NO_SAMPLE
    }

    companion object {
        /** ~30 Hz. */
        const val MIN_INTERVAL_MS = 33L
        /** ~6.7 Hz. */
        const val MAX_INTERVAL_MS = 150L
        /** Until a measurement lands, assume a middling mobile link: 40 ms. */
        const val ASSUMED_RTT_MS = 40.0
        private const val NO_SAMPLE = -1.0
        /** Weight for a *rising* sample. Falling samples are adopted whole. */
        private const val RISE = 0.25
    }
}