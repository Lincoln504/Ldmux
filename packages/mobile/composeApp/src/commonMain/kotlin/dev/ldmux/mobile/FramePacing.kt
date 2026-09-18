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
 *  * [minIntervalMs] is the ceiling on the frame rate (~20 Hz). Terminals stream
 *    text, not video, and what the ceiling actually buys is **main-thread
 *    headroom** — a repaint walks the whole grid, and the main thread is the one
 *    the input path runs on, so every render the user cannot perceive is a render
 *    competing with their typing. The original reason to coalesce at all was a
 *    saturated main thread delaying input (docs/PREDICTIVE-ECHO.md); this is that
 *    argument carried to its conclusion rather than a new one.
 *
 *    Deliberately **not** a claim about the network. Output is event-driven on the
 *    wire — a frame is sent when the pane changes — so this number moves no bytes.
 *    What it moves is how often the CPU is asked to redraw what already arrived.
 *  * [maxIntervalMs] is the floor (~5 Hz). Past this the display stops looking
 *    live and starts looking broken, and a user reading a slow terminal is better
 *    served by the predictive echo than by more draws.
 *
 * Both clamps were twice this eager, and were lowered on purpose: the cost of a
 * repaint is paid on every frame, while the benefit of the 30th frame per second
 * is not perceptible in a grid of text.
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
        /** ~20 Hz. */
        const val MIN_INTERVAL_MS = 50L
        /** ~5 Hz. */
        const val MAX_INTERVAL_MS = 200L
        /** Until a measurement lands, assume a middling mobile link: 40 ms. */
        const val ASSUMED_RTT_MS = 40.0
        private const val NO_SAMPLE = -1.0
        /** Weight for a *rising* sample. Falling samples are adopted whole. */
        private const val RISE = 0.25
    }
}