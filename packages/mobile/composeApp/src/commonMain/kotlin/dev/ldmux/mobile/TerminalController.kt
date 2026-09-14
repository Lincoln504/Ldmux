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

    /** Server-reported terminal modes (affects input encoding later). */
    var modes by mutableStateOf(TerminalModes())
        private set

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
            _frame.value = engine.render()
        }
        attach.start(scope)
    }

    override fun sendKey(name: String) = attach.keys(listOf(name))
    override fun sendText(text: String) = attach.input(text)
    override fun loadHistory() = attach.scrollback(MAX_HISTORY_LINES)
    override fun jumpToBottom() = attach.scrollback(0)

    fun dispose() {
        attach.stop()
        engine.close()
        scope.cancel()
    }

    private companion object {
        const val DEFAULT_COLS = 120
        const val DEFAULT_ROWS = 40
        const val MAX_HISTORY_LINES = 5000
    }
}