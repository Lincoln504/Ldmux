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

    /**
     * Whether the link is slow enough that a prediction is the better answer than
     * waiting for the echo.
     *
     * There is no setting for this and there should not be: whether typed text
     * should appear before the shell echoes it is a fact about the link at this
     * moment, not a preference. The decision is taken from the smoothed round trip
     * with **two** thresholds, so a link sitting on one number cannot flap:
     * prediction comes on at [PREDICT_ON_MS] and goes off at [PREDICT_OFF_MS].
     *
     * Below the floor the echo is prompt and prediction only risks a mispredict
     * flicker. Above it the user is typing into a delay, and painted text is the
     * only honest thing that can appear at the keystroke. An unmeasured link is
     * treated as fast (the assumed round trip is [ASSUMED_RTT_MS]): predicting on
     * a guess is exactly the dishonesty the gate exists to prevent.
     */
    var predict: Boolean = false
        private set

    /**
     * A coarse class of the link, for the decisions that are about how *much* to
     * send rather than when to paint.
     *
     * The repaint interval inverts with the round trip (a slow link is painted
     * less often, which costs nothing). Volume is the opposite: the frames that
     * carry a lot of *history* have to be sized by the link, because a page the
     * user has to wait seconds for is a page that feels broken. Two thresholds,
     * like [predict], so a link hovering on one does not flap.
     */
    var slow: Boolean = false
        private set

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
        // Two thresholds: on at the top, off at the bottom, nothing in between.
        if (!predict && smoothedRttMs >= PREDICT_ON_MS) {
            predict = true
        } else if (predict && smoothedRttMs <= PREDICT_OFF_MS) {
            predict = false
        }
        if (!slow && smoothedRttMs >= SLOW_ON_MS) {
            slow = true
        } else if (slow && smoothedRttMs <= SLOW_OFF_MS) {
            slow = false
        }
    }

    /**
     * Rows one history page-in should ask for now.
     *
     * Sized by the link (see [slow]): the reply carries the whole screen plus this
     * many rows of scrollback, and each further page re-sends everything older
     * again, so the cost of a pull is quadratic in the number of pages — which is
     * why the slow-link page is small rather than merely slower.
     */
    fun historyPageLines(): Int =
        if (slow) HISTORY_FIRST_SLOW_LINES else HISTORY_FIRST_LINES

    /** The most rows one page may ask for on this link. */
    fun historyMaxLines(): Int =
        if (slow) HISTORY_MAX_SLOW_LINES else HISTORY_MAX_LINES

    /** A dropped connection proves nothing about the link, so forget the sample. */
    fun onDisconnected() {
        samples = 0
        smoothedRttMs = NO_SAMPLE
        // The link is gone, so there is nothing to predict against; the next
        // measured round trip decides again from scratch.
        predict = false
        slow = false
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
        /**
         * Prediction turns on at this smoothed round trip (ms).
         *
         * 120 ms is roughly where a keystroke stops feeling immediate: typing
         * ahead into half a second feels broken, and at a tenth of a second it
         * still feels like the terminal is keeping up.
         */
        const val PREDICT_ON_MS = 120.0
        /**
         * And off again here (ms), well below [PREDICT_ON_MS] so a link hovering
         * at the boundary does not turn prediction on and off every other frame.
         */
        const val PREDICT_OFF_MS = 80.0

        /**
         * The link counts as slow (for how much history one request asks for) at
         * this smoothed round trip, and stops counting as slow here.
         *
         * 150 ms is where a full-screen history page stops being something the
         * user can pull into without noticing the wait; 90 ms is comfortably under
         * it, so the band is wide enough that a phone moving around a house does
         * not change the page size on every sample.
         */
        const val SLOW_ON_MS = 150.0
        const val SLOW_OFF_MS = 90.0

        /** Rows a first history page asks for on a fast link. */
        const val HISTORY_FIRST_LINES = 5_000
        /** ... and the most any one page asks for, however long the user pulls. */
        const val HISTORY_MAX_LINES = 20_000
        /**
         * Rows a first history page asks for on a slow link.
         *
         * The reply is the visible screen plus that many rows, replayed into the
         * client's emulator: 5,000 rows of an 80-column screen is ~400 KB before
         * compression, which is a second or more of transfer on a 150 ms link and
         * a rebuild the user is waiting on. 1,000 rows (~80 KB) is the same gesture
         * at a size a slow link can answer inside a visible pause.
         */
        const val HISTORY_FIRST_SLOW_LINES = 1_000
        /** ... and the slow-link ceiling on one page, for the same reason. */
        const val HISTORY_MAX_SLOW_LINES = 4_000
    }
}