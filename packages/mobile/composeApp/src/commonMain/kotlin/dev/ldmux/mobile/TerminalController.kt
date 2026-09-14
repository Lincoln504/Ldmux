package dev.ldmux.mobile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.ldmux.mobile.term.TerminalEngine
import dev.ldmux.mobile.term.TerminalFrame
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
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
        attach.onOutput = { bytes ->
            engine.write(bytes, bytes.size)
            scheduleRender()
        }
        attach.start(scope)
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

    override fun sendKey(name: String) = attach.keys(listOf(name))
    override fun sendText(text: String) = attach.input(text)
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
        attach.stop()
        engine.close()
        scope.cancel()
    }

    private companion object {
        const val DEFAULT_COLS = 120
        const val DEFAULT_ROWS = 40
        const val MAX_HISTORY_LINES = 5000
        /** ~60 Hz. The ceiling; phase 3 in docs/PREDICTIVE-ECHO.md lowers it to
         *  max(2 Hz, 1/2 RTT) when the link is slow. */
        const val FRAME_MS = 16L
    }
}