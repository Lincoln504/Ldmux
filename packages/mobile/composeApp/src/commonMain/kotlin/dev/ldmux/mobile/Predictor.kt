package dev.ldmux.mobile

/**
 * Predictive local echo (see docs/PREDICTIVE-ECHO.md).
 *
 * The idea: when the user types, show the character immediately by feeding it to
 * the local emulator, and reconcile against the echo when it comes back — so a
 * 200 ms round trip stops being visible. This is the piece that makes a remote
 * terminal feel local.
 *
 * Only *literal text* is ever predicted (our `input` message). Named keys
 * (`key`) are resolved by tmux against the pane's real modes and are never
 * predicted, so control characters, arrows, function keys and terminal queries
 * still take the honest round trip. That is the predictable/unpredictable gate
 * dooboxes recorded, enforced by the message split rather than by a table.
 *
 * The safety net is that the prediction is *self-correcting*:
 *
 * - echoed byte-for-byte  -> consume it, write only the rest (nothing is
 *   painted twice, nothing flickers);
 * - anything else         -> the prediction was wrong. A VT cannot un-apply
 *   bytes, so the caller asks for a fresh snapshot and repaints. One round trip,
 *   only on a mispredict;
 * - nothing comes back at all (a prompt that does not echo, i.e. a password) ->
 *   the caller declares the prediction stale, shows the truth again, and
 *   **stops predicting** until the next line is submitted.
 *
 * That last rule is what keeps a password from being drawn: predictions are
 * abandoned the moment echo stops, and `echoing` is only re-armed on submit.
 */
class Predictor {

    sealed interface Consumption {
        /** Nothing was predicted; write the output unchanged. */
        data object Passthrough : Consumption

        /** The echo was confirmed; write whatever is left after the match. */
        data class Consume(val remaining: ByteArray) : Consumption

        /** The prediction was wrong: repaint from a snapshot instead. */
        data object Mispredict : Consumption
    }

    private val pending = ArrayDeque<Byte>()

    /** False after echo has been seen to stop; re-armed by [onLineSubmitted]. */
    var echoing: Boolean = true
        private set

    val hasPending: Boolean get() = pending.isNotEmpty()

    /**
     * Drop any outstanding prediction without touching [echoing]. Called when a
     * snapshot rebuilds the screen: the snapshot already contains the truth, but
     * it must not re-arm prediction after a mispredict or an abandonment.
     */
    fun reset() {
        pending.clear()
    }

    /** Enter was pressed: a fresh line, and a fresh assumption that it echoes. */
    fun onLineSubmitted() {
        pending.clear()
        echoing = true
    }

    /**
     * May these bytes be shown before the server confirms them? Printable text
     * only — never a control byte, an escape, or a newline (those travel as
     * named keys, and this is the belt to that braces).
     */
    fun canPredict(bytes: ByteArray): Boolean {
        if (!echoing) return false
        if (bytes.isEmpty()) return false
        for (b in bytes) {
            val v = b.toInt() and 0xff
            if (v in 0x20..0x7e) continue // printable ASCII
            if (v >= 0x80) continue // UTF-8 continuation/lead
            return false
        }
        return true
    }

    fun predict(bytes: ByteArray) {
        for (b in bytes) pending.addLast(b)
    }

    /**
     * Reconcile an output frame against the outstanding prediction.
     *
     * The echo can arrive split across frames, so a prefix match that runs out of
     * *output* is a partial confirmation, not a failure: those bytes really were
     * our echo.
     */
    fun reconcile(output: ByteArray): Consumption {
        if (pending.isEmpty()) return Consumption.Passthrough
        val pendingBefore = pending.size
        val limit = minOf(output.size, pendingBefore)
        var matched = 0
        while (matched < limit && output[matched] == pending[matched]) matched++
        repeat(matched) { pending.removeFirst() }
        return when {
            // The whole prediction was echoed: write the remainder.
            matched == pendingBefore -> Consumption.Consume(output.copyOfRange(matched, output.size))
            // Output ran out mid-echo: consume what matched and write nothing.
            matched == output.size -> Consumption.Consume(ByteArray(0))
            // Divergence: this was not our echo.
            else -> {
                pending.clear()
                echoing = false
                Consumption.Mispredict
            }
        }
    }

    /** Nothing came back in time: the pane is not echoing. Give up and stop. */
    fun abandon() {
        pending.clear()
        echoing = false
    }
}