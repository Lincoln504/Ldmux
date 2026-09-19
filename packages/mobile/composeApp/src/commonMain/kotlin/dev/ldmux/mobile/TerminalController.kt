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
    /** Set once the whole scrollback has been asked for, so it is asked once. */

    /** Server-reported terminal modes (affects input encoding later). */
    var modes by mutableStateOf(TerminalModes())
        private set

    /**
     * Set by the platform surface: it owns the microphone, which the shared
     * sidebar can only ask for. Null until the surface is composed.
     */
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

    /**
     * The sticky modifiers this session has armed.
     *
     * Owned by the session, not by the chrome, because a latch has to be spent by
     * whatever sends the *next* command, and exactly one object sees every path
     * that sends one. Kept in the composition it was reachable only from the two
     * key bars: a typed character goes straight to [sendText] and never touched it,
     * so arming Ctrl and then typing `q` sent a plain `q` and left Ctrl armed for
     * the next bar key — which is both halves of the reported bug at once.
     *
     * Same shape as [inCopyMode]: input state the session owns and the chrome reads
     * back. `modes` above is the same pattern.
     */
    override var modifiers by mutableStateOf(Modifiers())
        private set

    override fun toggleModifier(which: StickyKey) {
        modifiers = modifiers.toggled(which)
    }

    override fun clearModifiers() {
        modifiers = Modifiers()
    }

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
            // State, then content, then cursor — see [snapshotBytes], which is the
            // only thing that decides that order.
            val bytes = snapshotBytes(
                screen = screen,
                alternateOn = m.alternateOn,
                cursorX = m.cursorX,
                cursorY = m.cursorY,
                cols = cols,
                rows = rows,
                // Everything else the program had set, which a fresh engine does
                // not know. See [modeSequence].
                modes = m,
            )
            engine.write(bytes, bytes.size)
            // A history page rebuilds the engine, so the view has to be put back
            // where it was: the same offset from the live bottom is the same
            // content, because what arrived is above it.
            val restore = restoreOffset
            restoreOffset = null
            historyLoading = false
            historyRequested = true
            if (restore != null && restore > 0) engine.scrollBy(restore.toInt())
            modes = m
            // A fresh snapshot is the only thing that knows the foreground
            // program again, so it is also what re-arms the click path.
            mouseRefused = false
            _frame.value = engine.render()
        }
        attach.onOutput = { bytes -> writeOutput(bytes) }
        attach.onError = { code -> onServerError(code) }
        // The click landed, so the tap is finished. The timer in [tapCell] is
        // only ever for "no answer at all"; a program that took the click must
        // not then be walked to the cell as well.
        attach.onMouseDelivered = {
            pendingTapJob?.cancel()
            pendingTap = null
        }
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
        // The window is the link's, not a constant: half a round trip is the
        // one-way delay, which is the fastest new content can have arrived —
        // repainting sooner than that cannot show anything that is not already on
        // screen. On a fast link this is the 50 ms floor (~20 Hz), on a slow one it
        // stretches toward 200 ms so the client stops rendering frames nobody will
        // read. It is a budget for the main thread, which input shares, and not a
        // throttle on the wire: output is sent when the pane changes.
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
        val armed = modifiers
        if (armed.armed) {
            // One Ctrl arms one key. Spending it here rather than in the chrome is
            // what makes a hardware key and a bar key behave the same.
            modifiers = Modifiers()
            sendModifiedKey(name, armed)
            return
        }
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
            is ModifiedKey.Literal -> sendTextRaw(k.bytes)
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
        if (text.isEmpty()) return
        val armed = modifiers
        if (!armed.armed) {
            sendTextRaw(text)
            return
        }
        // The first keystroke spends the latch. The decision itself is a pure
        // function beside the modifier semantics, so it can be tested without a
        // socket — which is the only reason the old rule could rot unnoticed here
        // while every test passed.
        val typed = spendLatchOnFirst(text, armed)
        modifiers = typed.latchAfter
        if (typed.head.isEmpty()) {
            sendTextRaw(typed.rest)
        } else {
            sendModifiedKey(typed.head, typed.headModifiers)
            if (typed.rest.isNotEmpty()) sendTextRaw(typed.rest)
        }
    }

    /**
     * Send text with the modifiers already applied.
     *
     * Deliberately separate from [sendText]: the public one *spends the latch*, and
     * [sendModifiedKey] resolves a chord by calling back into this, so a single
     * slash through [sendText] would apply the modifier twice.
     */
    private fun sendTextRaw(text: String) {
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
    override fun scrollBy(rows: Int) {
        if (rows == 0) return
        engine.scrollBy(rows)
        renderNow()
        // History arrives when it is asked for by the act of scrolling to the end
        // of what is already here. It used to be a button, which is the machine
        // showing its working: two controls whose difference ("show me history"
        // vs "go back to now") is a distinction the user should never have had to
        // hold, because the gesture already means both.
        if (rows < 0) maybeExtendHistory()
    }

    /**
     * Page in older output when the user reaches the top of what is loaded.
     *
     * Once per attach at most, and only when they are actually at the top: the
     * scrollback request re-sends the whole screen, so asking on every pixel of
     * drag would be a round trip per frame.
     */
    private fun maybeExtendHistory() {
        val f = _frame.value ?: return
        // scrollOffset < scrollTotal means there is still loaded history above.
        if (f.scrollOffset < f.scrollTotal) return
        if (historyRequested || historyLoading) return
        historyLoading = true
        // Remember where the view was. Older lines arrive *above* what is already
        // displayed, and the offset is measured from the live bottom, so the same
        // content stays at the same offset — restoring the number restores the
        // view rather than dumping the user at the top.
        restoreOffset = f.scrollOffset
        attach.scrollback(MAX_HISTORY_LINES)
    }

    private var historyLoading = false
    private var historyRequested = false
    private var restoreOffset: Long? = null

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
/**
 * A whole snapshot, as the bytes that reproduce the pane.
 *
 * **A snapshot is a piece of terminal *state*, not a picture**, and this is the
 * one place that knows it. Everything a program has set which changes how the
 * *next* bytes are read has to be re-established before those bytes arrive, and
 * in the right order:
 *
 *  1. **the screen** — a full-screen program (`micro`, `vim`, `less`, `htop`)
 *     runs on the alternate screen. Replaying its text without switching screens
 *     writes it into the *primary* buffer, which still holds the shell's prompt
 *     and history: the program's output lands on top of the scrollback it was
 *     supposed to hide, the prompt shows through it, and the stray text scrolls
 *     the shell away. That is not a rendering artefact; it is the wrong screen.
 *  2. **the content** — the capture, minus the defects [snapshotReplay] records.
 *  3. **the cursor** — because the capture pads to the full height, so replaying
 *     it leaves the cursor at the bottom of the padding, not where the program
 *     put it.
 *
 * The engine is rebuilt for every snapshot, so `1049h` here *establishes* a
 * screen rather than toggling one, and the saved primary screen it discards is
 * genuinely disposable: the next snapshot carries whatever is on screen then.
 */
internal fun snapshotBytes(
    screen: String,
    alternateOn: Boolean,
    cursorX: Int,
    cursorY: Int,
    cols: Int,
    rows: Int,
    modes: TerminalModes = TerminalModes(),
): ByteArray {
    val state = StringBuilder()
    // `?1049` is the save/switch form, which is what a full-screen program uses.
    // Sent on every snapshot, `l` included: on a fresh engine leaving the primary
    // screen is a no-op, and being unconditional is what makes the two screens
    // impossible to get out of step.
    state.append(if (alternateOn) "\u001b[?1049h" else "\u001b[?1049l")
    state.append(modeSequence(modes))
    if (screen.isNotEmpty()) state.append(snapshotReplay(screen))
    state.append(cursorSequence(cursorX, cursorY, cols, rows))
    return state.toString().encodeToByteArray()
}

/**
 * A captured screen, as bytes safe to write into an emulator.
 *
 * Two things have to be right, and both are invisible until they are wrong.
 *
 * **The terminator.** `capture-pane` terminates *every* row, including the last,
 * so a 20-row pane comes back with 20 newlines. Replaying all of them moves the
 * cursor one row past the bottom, the emulator scrolls, and every line shifts up
 * by one — while the cursor is then placed at the pane's real row, so it ends up
 * sitting one row *below* the text it belongs to. Measured: 20 rows, 20 newlines,
 * cursor reported at row 2. N rows want N-1 separators.
 *
 * **The line feed.** Rows are separated by a bare LF, and a VT treats LF as a
 * line feed only — the column is preserved — so the screen staircases diagonally.
 * It has to be CR+LF. Live pty output needs neither fix; this is only for
 * replaying a capture.
 */
internal fun snapshotReplay(screen: String): String {
    val rows = if (screen.endsWith("\n")) screen.dropLast(1) else screen
    return rows.replace("\r\n", "\n").replace("\n", "\r\n")
}

/**
 * The modes the program had turned on, re-established on a fresh engine.
 *
 * A snapshot rebuilds the emulator, so everything the program set before it is
 * gone — and the emulator's flags are what the whole client reasons with. They
 * are not decoration: `mouseTracking` and `mouseSgr` are what decide whether a tap
 * is a click or a walk, and what a walk is measured from.
 *
 * Measured failure. `micro` has mouse reporting on. After a snapshot the client's
 * frame still said `mouseTracking=false mouseSgr=false`, so a tap was read as "a
 * full-screen program that did not ask for the mouse", synthesised Up/Down/Left/
 * Right from a cursor that was itself stale, and landed on a cell nobody tapped.
 * Tapping `charlie` moved the cursor to an empty line. The pane was right, the
 * screen was right, the cell the client computed was right (`col=41 row=2`) — the
 * mode was missing, and the mode is what chose the branch.
 *
 * `?1000` and not `?1002`/`?1003`: tmux reports only that *some* mouse mode is on,
 * and this client synthesises presses alone, so the press-reporting mode is the
 * one it needs. A program that wanted drag reporting loses nothing it does not
 * get back the next time it enables it.
 *
 * The `l` forms are sent too, and unconditionally, for the same reason `?1049l`
 * is: on a fresh engine they are no-ops, and being unconditional is what makes
 * this impossible to get out of step with what is on screen.
 */
internal fun modeSequence(modes: TerminalModes): String = buildString {
    append(if (modes.mouse) "\u001b[?1000h" else "\u001b[?1000l")
    append(if (modes.mouseSgr) "\u001b[?1006h" else "\u001b[?1006l")
    append(if (modes.appCursor) "\u001b[?1h" else "\u001b[?1l")
    append(if (modes.bracketedPaste) "\u001b[?2004h" else "\u001b[?2004l")
}

internal fun cursorSequence(col: Int, row: Int, cols: Int, rows: Int): String {
    val c = col.coerceIn(0, maxOf(cols - 1, 0)) + 1
    val r = row.coerceIn(0, maxOf(rows - 1, 0)) + 1
    return "\u001b[$r;${c}H"
}