package dev.ldmux.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.ldmux.mobile.term.TerminalEngine
import dev.ldmux.mobile.term.TerminalFrame
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * Owns one session's terminal: the libghostty engine plus the attach stream.
 * Recreating the engine on each snapshot gives a clean parser state and a full
 * repaint, which is exactly what a snapshot (attach or history page) means.
 */
class TerminalController(
    private val config: LdmuxConfig,
    private val sessionId: String,
) : TerminalHandle {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var engine = TerminalEngine(DEFAULT_COLS, DEFAULT_ROWS)

    private val _frame = MutableStateFlow<TerminalFrame.Frame?>(null)
    val frame: StateFlow<TerminalFrame.Frame?> = _frame

    private var lastRenderAt = 0L
    private var renderScheduled = false

    private val predictor = Predictor()
    private var staleJob: Job? = null

    /** Server-reported terminal modes (affects input encoding later). */
    var modes by mutableStateOf(TerminalModes())
        private set

    /**
     * Set by the platform surface: it owns the microphone, which the shared
     * sidebar can only ask for. Null until the surface is composed.
     */
    var onStartVoice: (() -> Unit)? = null

    private val attach = AttachClient(config, sessionId)

    init {
        attach.onSnapshot = { cols, rows, screen, _, m ->
            // A snapshot is the truth: any outstanding prediction is settled by
            // it (this is also the recovery path after a mispredict).
            predictor.reset()
            staleJob?.cancel()
            engine.close()
            engine = TerminalEngine(cols, rows)
            if (screen.isNotEmpty()) {
                // tmux capture-pane separates rows with a bare LF. A VT treats LF
                // as a line feed only (column preserved), so a snapshot must be
                // given CR+LF or the screen staircases. Live PTY output is fine.
                val normalized = screen.replace("\r\n", "\n").replace("\n", "\r\n")
                val bytes = normalized.encodeToByteArray()
                engine.write(bytes, bytes.size)
            }
            modes = m
            _frame.value = engine.render()
        }
        attach.onOutput = { bytes -> writeOutput(bytes) }
        attach.start(scope)
    }

    /**
     * Apply one output frame, first reconciling it against any prediction.
     *
     * Feeding output before reconciling would paint the echo twice (once from
     * the prediction, once from the server).
     */
    private fun writeOutput(bytes: ByteArray) {
        if (!config.predict) {
            engine.write(bytes, bytes.size)
            scheduleRender()
            return
        }
        when (val c = predictor.reconcile(bytes)) {
            is Predictor.Consumption.Consume -> {
                staleJob?.cancel()
                if (c.remaining.isNotEmpty()) engine.write(c.remaining, c.remaining.size)
                scheduleRender()
            }
            Predictor.Consumption.Passthrough -> {
                engine.write(bytes, bytes.size)
                scheduleRender()
            }
            Predictor.Consumption.Mispredict -> {
                // The prediction was wrong and a VT cannot un-apply it, so the
                // screen is rebuilt from the server instead.
                staleJob?.cancel()
                attach.snapshotNow()
            }
        }
    }

    /**
     * Repaint at most once per frame.
     *
     * Output arrives per write() from tmux, which under a flood (`cat` a big
     * file, `yes`) is far faster than the display: rendering each frame would
     * burn the main thread on work nobody can see, and a saturated main thread
     * delays *input*, which is the one thing that must stay responsive. The
     * first frame after an idle moment paints immediately (so an echoed
     * keystroke is not delayed); anything arriving inside the window is
     * coalesced into the next one. `dooboxes` calls the adaptive version of
     * this an MVP requirement for exactly this reason (see docs/PREDICTIVE-ECHO.md).
     */
    private fun scheduleRender() {
        val now = System.currentTimeMillis()
        val since = now - lastRenderAt
        if (since >= FRAME_MS) {
            lastRenderAt = now
            _frame.value = engine.render()
            return
        }
        if (renderScheduled) return
        renderScheduled = true
        scope.launch {
            delay(FRAME_MS - since)
            renderScheduled = false
            lastRenderAt = System.currentTimeMillis()
            _frame.value = engine.render()
        }
    }

    override fun sendKey(name: String) {
        // Submitting a line re-arms prediction: a fresh line is expected to echo
        // again, even if the previous one did not (a password prompt).
        if (name == "Enter") predictor.onLineSubmitted()
        attach.keys(listOf(name))
    }

    override fun sendText(text: String) {
        val bytes = text.encodeToByteArray()
        val canShowNow = config.predict && !modes.alternateOn && predictor.canPredict(bytes)
        if (canShowNow) {
            // Paint it before the server is even asked. If the echo disagrees,
            // writeOutput() throws this away and repaints from a snapshot.
            engine.write(bytes, bytes.size)
            predictor.predict(bytes)
            armStaleCheck()
            _frame.value = engine.render()
        }
        attach.input(text)
    }

    /**
     * A prediction that is never echoed means the pane is not echoing (a
     * password prompt, most importantly). Show the truth again and stop
     * predicting until the line is submitted.
     */
    private fun armStaleCheck() {
        staleJob?.cancel()
        staleJob = scope.launch {
            delay(STALE_MS)
            if (predictor.hasPending) {
                predictor.abandon()
                attach.snapshotNow()
            }
        }
    }
    override fun loadHistory() = attach.scrollback(MAX_HISTORY_LINES)
    override fun jumpToBottom() = attach.scrollback(0)

    override fun startVoice() {
        onStartVoice?.invoke()
    }

    /**
     * The grid the surface can paint. This is the resize request: the daemon
     * sizes the tmux pane to it, so the shell is always the right size for the
     * screen showing it rather than fixed at creation.
     */
    fun setGrid(cols: Int, rows: Int) {
        attach.setGrid(cols, rows)
    }

    fun dispose() {
        staleJob?.cancel()
        attach.stop()
        engine.close()
        scope.cancel()
    }

    private companion object {
        const val DEFAULT_COLS = 120
        const val DEFAULT_ROWS = 40
        const val MAX_HISTORY_LINES = 5000
        /** How long an unconfirmed prediction may stay on screen before we
         *  conclude the pane is not echoing. Roughly 2-3 mobile round trips. */
        const val STALE_MS = 900L
        /** ~60 Hz. The ceiling; phase 3 in docs/PREDICTIVE-ECHO.md lowers it to
         *  max(2 Hz, 1/2 RTT) when the link is slow. */
        const val FRAME_MS = 16L
    }
}