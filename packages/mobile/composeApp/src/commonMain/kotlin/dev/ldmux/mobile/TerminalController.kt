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
import kotlin.time.TimeMark
import kotlin.time.TimeSource

/**
 * Owns one session's terminal: the libghostty engine plus the attach stream.
 * Recreating the engine on each snapshot gives a clean parser state and a full
 * repaint, which is exactly what a snapshot (attach or history page) means.
 */
class TerminalController(
    private val config: LdmuxConfig,
    private val sessionId: String,
    active: Boolean = true,
) : TerminalHandle {

    /**
     * Whether the stream should be running.
     *
     * The pager keeps a neighbouring page composed so a swipe is instant, but
     * composed is not the same as *connected*: attached-and-hidden would mean
     * streaming output for a terminal nobody is looking at, and — because the
     * viewing client owns the pane geometry — resizing the panes of sessions the
     * user is not even on. So an inactive page keeps its last painted frame and
     * drops its connection until it is the one on screen.
     */
    private var active = active

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var engine = TerminalEngine(DEFAULT_COLS, DEFAULT_ROWS)

    private val _frame = MutableStateFlow<TerminalFrame.Frame?>(null)
    val frame: StateFlow<TerminalFrame.Frame?> = _frame

    private var lastRenderAt: TimeMark? = null
    private var renderScheduled = false

    /**
     * The repaint cadence, which follows the measured round trip. A phone should
     * not paint faster than the link can answer, and it should slow down rather
     * than queue up when the link is bad (docs/PREDICTIVE-ECHO.md, phase 3).
     */
    private val pacing = FramePacing()

    private val predictor = Predictor()
    private var staleJob: Job? = null
    private var scrollTopOnNextSnapshot = false

    /** Server-reported terminal modes (affects input encoding later). */
    var modes by mutableStateOf(TerminalModes())
        private set

    /**
     * Set by the platform surface: it owns the microphone, which the shared
     * sidebar can only ask for. Null until the surface is composed.
     */
    var onStartVoice: (() -> Unit)? = null

    /**
     * A tap waiting on the server's verdict about whether this pane takes
     * clicks. Kept so a refused click can still move the cursor, which is what
     * the user meant by tapping in the first place.
     */
    private var pendingTap: Pair<Int, Int>? = null
    private var pendingTapJob: Job? = null

    /**
     * The server has told us this pane will not accept a mouse click. Cleared on
     * every snapshot, because the foreground program may have changed.
     */
    private var mouseRefused = false

    private val attach = AttachClient(config, sessionId)

    private val _link = MutableStateFlow(LinkStats(0, FramePacing.MIN_INTERVAL_MS, false))

    /** What the link is doing, for the chrome to show. */
    override val link: StateFlow<LinkStats> = _link

    private fun linkStats() = LinkStats(
        rttMs = pacing.rttMs,
        frameIntervalMs = pacing.intervalMs,
        measured = pacing.measured,
    )

    init {
        attach.onSnapshot = { cols, rows, screen, _, m ->
            // A snapshot is the truth: any outstanding prediction is settled by
            // it (this is also the recovery path after a mispredict).
            predictor.reset()
            staleJob?.cancel()
            // A snapshot only ever follows a *successful* attach, so this is the
            // one place that can prove the refusal no longer applies.
            _ownedBy.value = null
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
            // Then put the cursor where the pane actually has it. Replaying the
            // screen leaves it at the end of the padded area (capture-pane fills
            // the full height with blank rows), which is not the cursor's
            // position — and the emulator's cursor is what gets drawn and what
            // every tap measures its distance from.
            val place = cursorSequence(m.cursorX, m.cursorY, cols, rows).encodeToByteArray()
            engine.write(place, place.size)
            if (scrollTopOnNextSnapshot) {
                scrollTopOnNextSnapshot = false
                engine.scrollToTop()
            }
            modes = m
            // A fresh snapshot is the only thing that knows the foreground
            // program again, so it is also what re-arms the click path.
            mouseRefused = false
            _frame.value = engine.render()
        }
        attach.onOutput = { bytes -> writeOutput(bytes) }
        attach.onError = { code -> onServerError(code) }
        attach.onOwned = { holder ->
            // The attach was refused because another device holds this shell,
            // not because anything is broken (ADR-0010). Surfaced rather than
            // retried: retrying on its own is exactly the silent steal the rule
            // exists to prevent.
            _ownedBy.value = holder ?: "another device"
        }
        attach.onRtt = { sample ->
            pacing.onRttSample(sample)
            _link.value = linkStats()
        }
        attach.onOffline = {
            pacing.onDisconnected()
            _link.value = linkStats()
        }
        if (active) attach.start(scope)
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
        // The window is the link's, not a constant: half a round trip is roughly
        // how long a keystroke's echo takes to come back, which is the cadence
        // the user is watching. On a fast link this is the 33 ms floor (~30 Hz),
        // on a slow one it stretches toward 150 ms so the client stops queuing
        // frames nobody will see.
        val frameMs = pacing.intervalMs
        // Monotonic, not wall clock: this is a duration, and a common-code file
        // shared with iOS must not reach for `System.currentTimeMillis`.
        val since = lastRenderAt?.elapsedNow()?.inWholeMilliseconds ?: Long.MAX_VALUE
        if (since >= frameMs) {
            lastRenderAt = TimeSource.Monotonic.markNow()
            _frame.value = engine.render()
            return
        }
        if (renderScheduled) return
        renderScheduled = true
        scope.launch {
            delay(frameMs - since)
            renderScheduled = false
            lastRenderAt = TimeSource.Monotonic.markNow()
            _frame.value = engine.render()
        }
    }

    /** Start or stop the stream as this page enters or leaves the foreground. */
    fun setActive(value: Boolean) {
        if (value == active) return
        active = value
        if (value) attach.start(scope) else attach.stop()
    }

    override fun sendKey(name: String) {
        // Submitting a line re-arms prediction: a fresh line is expected to echo
        // again, even if the previous one did not (a password prompt).
        if (name == "Enter") predictor.onLineSubmitted()
        attach.keys(listOf(name))
    }

    /**
     * Send one key with the sticky modifiers applied.
     *
     * A modified *character* becomes literal bytes (Ctrl-C is 0x03) — which is
     * what a keyboard would have sent, and is what makes readline and vim inside
     * the shell see the chord rather than a letter. A modified *named* key stays a
     * tmux key name, because its sequence is the terminal's business, not ours.
     */
    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        when (val k = modifiedKey(key, modifiers)) {
            is ModifiedKey.None -> {}
            is ModifiedKey.Literal -> sendText(k.bytes)
            is ModifiedKey.Named -> {
                if (k.name == "Enter") predictor.onLineSubmitted()
                attach.keys(listOf(k.name))
            }
        }
    }

    override fun setCopyMode(on: Boolean) {
        attach.copyMode(on)
    }

    /**
     * Whether the pane is in a tmux mode.
     *
     * Both the server's reading and the engine's are consulted: the server knows
     * for certain, but it only speaks on a state push, while the engine sees the
     * screen change immediately. Taking either means the button reflects the
     * terminal rather than the last thing we asked for.
     */
    override val inCopyMode: Boolean
        get() = modes.paneInMode

    override fun sendText(text: String) {
        val bytes = text.encodeToByteArray()
        // Three gates, any one of which is enough to refuse: the setting; a
        // full-screen app (alternate screen, which does not echo the way a
        // shell does); and a password prompt, which the emulator itself tells us
        // about — so we never have to guess from timing.
        val canShowNow = config.predict &&
            !modes.alternateOn &&
            _frame.value?.passwordInput != true &&
            predictor.canPredict(bytes)
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
    override fun loadHistory() {
        // The emulator holds its own scrollback, so history is paged in once and
        // then scrolled locally — scrolling must not cost a round trip per
        // swipe. The view is parked on the oldest row when it arrives.
        scrollTopOnNextSnapshot = true
        attach.scrollback(MAX_HISTORY_LINES)
    }

    override fun jumpToBottom() {
        engine.scrollToBottom()
        renderNow()
    }

    override fun scrollBy(rows: Int) {
        if (rows == 0) return
        engine.scrollBy(rows)
        renderNow()
    }

    /** Scrolling is a local view change: paint it immediately, never coalesced. */
    private fun renderNow() {
        lastRenderAt = TimeSource.Monotonic.markNow()
        _frame.value = engine.render()
    }

    /**
     * Move the remote cursor to a tapped cell.
     *
     * The decision itself lives in [planTap], which is pure and tested; this
     * only carries it out. A click is a request to the server (only it can see
     * the pane), so it may come back refused — see [onServerError].
     */
    override fun tapCell(col: Int, row: Int) {
        val f = _frame.value ?: return
        when (val action = planTap(col, row, f, mouseRefused)) {
            is TapAction.Click -> {
                pendingTap = action.col to action.row
                pendingTapJob?.cancel()
                pendingTapJob = scope.launch {
                    delay(CLICK_VERDICT_MS)
                    // No answer in time: do not let the tap vanish.
                    val stuck = pendingTap ?: return@launch
                    pendingTap = null
                    _frame.value?.let { moveCursorTo(stuck.first, stuck.second, it) }
                }
                attach.mouseClick(action.col, action.row)
            }
            is TapAction.Keys -> sendKeyBatches(action.keys)
            TapAction.Ignore -> Unit
        }
    }

    private val _ownedBy = MutableStateFlow<String?>(null)

    /**
     * The shell is held by another device, named when known.
     *
     * Cleared by a snapshot, which arrives only when the attach succeeded — so
     * this cannot get stuck showing a refusal after a successful takeover.
     */
    override val ownedBy: StateFlow<String?> = _ownedBy

    /** The user confirmed: take the shell from whoever has it. */
    override fun takeOver() {
        attach.takeOver()
    }

    /**
     * The server refused a click. That is the expected answer when the pane's
     * mouse mode is stuck on from a program that has since exited — so this is
     * not an error to report, it is the signal to use cursor motion instead.
     */
    private fun onServerError(code: String) {
        if (code != "no_mouse" && code != "shell_foreground") return
        mouseRefused = true
        pendingTapJob?.cancel()
        val tap = pendingTap ?: return
        pendingTap = null
        _frame.value?.let { moveCursorTo(tap.first, tap.second, it) }
    }

    private fun moveCursorTo(col: Int, row: Int, f: TerminalFrame.Frame) {
        when (val action = planTap(col, row, f, mouseRefused = true)) {
            is TapAction.Keys -> sendKeyBatches(action.keys)
            else -> Unit
        }
    }

    private fun sendKeyBatches(keys: List<String>) {
        for (batch in batchKeys(keys)) attach.keys(batch)
    }

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
        /** How long to wait for the server's verdict on a click before giving
         *  up and using cursor motion instead. Two mobile round trips. */
        const val CLICK_VERDICT_MS = 1200L
    }
}

/**
 * A VT sequence that puts the cursor at a cell, 1-based on the wire and clamped
 * to the grid.
 *
 * Written after a snapshot is replayed, because the replayed screen ends
 * wherever its last non-blank row ended. See [TerminalModes.cursorX].
 */
internal fun cursorSequence(col: Int, row: Int, cols: Int, rows: Int): String {
    val c = col.coerceIn(0, maxOf(cols - 1, 0)) + 1
    val r = row.coerceIn(0, maxOf(rows - 1, 0)) + 1
    return "\u001b[$r;${c}H"
}