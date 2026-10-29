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
 *
 * A snapshot is applied by [planSnapshot]: rebuilding the engine gives a clean parser
 * state and a full repaint, but it also loses the scrollback and parks the viewport at
 * the bottom, so it is done only while the view is following live output. While the
 * user is reading history the screen is replayed into the engine they are reading.
 *
 * The socket is **not** opened by the constructor: [begin] does that, called when the
 * composition has actually kept this controller. A controller built by a composition
 * that is then abandoned must not leave a connected socket holding the shell.
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

    /**
     * Where a clipboard write from the program goes, if anyone is listening.
     *
     * A program in the shell can ask to put something on the phone's clipboard,
     * and until this was wired the request parsed and then vanished. The surface
     * owns the actual clipboard and sets this; nothing here knows what a clipboard
     * is, which is what keeps this file testable without a device.
     */
    var onClipboardRequest: ((ClipboardRequest) -> Unit)? = null

    /**
     * Where something the agent wants said goes, from either producer: the daemon's typed
     * `speech` message and a private OSC sequence in the byte stream (`docs/VOICE-MODE.md`
     * section 3.5). Both arrive here, so a listener sees one stream of events and never which
     * channel carried them.
     */
    private val speech = dev.ldmux.mobile.voice.SpeechIntake()

    override fun setSpeechListener(listener: ((dev.ldmux.mobile.voice.SpeechEvent) -> Unit)?) {
        speech.listener = listener
    }

    /** Server-reported terminal modes (affects input encoding later). */
    var modes by mutableStateOf(TerminalModes())
        private set

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

    /** Whether the daemon this page is attached to understands `S-` on named keys. */
    val shiftKeys: Boolean get() = attach.shiftKeys

    /**
     * Whether typed text is shown before the shell echoes it (see
     * [FramePacing.predict]). Read from the pacing, never set by the user: it is a
     * fact about the link at this moment.
     */
    private val predict: Boolean get() = pacing.predict

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
     * Input state the session owns and the chrome reads back (`modes` above is
     * the same pattern).
     */
    override var modifiers by mutableStateOf(Modifiers())
        private set

    private val clockOrigin = TimeSource.Monotonic.markNow()
    private val latch = ModifierLatch(now = { clockOrigin.elapsedNow().inWholeMilliseconds })
    private var latchJob: Job? = null

    override fun toggleModifier(which: StickyKey) {
        modifiers = latch.toggle(which)
        latchJob?.cancel()
        // An armed modifier the user never used lapses, visibly: the key is drawn
        // unlatched again instead of waiting to chord some later keystroke.
        val wait = latch.remainingMs()
        latchJob = if (wait == null) null else scope.launch {
            delay(wait + 1)
            modifiers = latch.current()
        }
    }

    override fun clearModifiers() {
        latchJob?.cancel()
        latchJob = null
        latch.clear()
        modifiers = Modifiers()
    }

    /** The modifiers armed *now*: [ModifierLatch] has already let go of expired ones. */
    private fun armedModifiers(): Modifiers {
        val live = latch.current()
        if (live != modifiers) modifiers = live
        return live
    }

    /** Spend the latch: one keystroke, one use. */
    private fun spendLatch() {
        latchJob?.cancel()
        latchJob = null
        latch.clear()
        modifiers = Modifiers()
    }

    private fun linkStats() = LinkStats(
        rttMs = pacing.rttMs,
        frameIntervalMs = pacing.intervalMs,
        measured = pacing.measured,
    )

    init {
        attach.onSnapshot = { cols, rows, screen, history, m -> applySnapshot(cols, rows, screen, history, m) }
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
            // The name the daemon gave, or empty when it gave none: the UI says
            // "open elsewhere" for empty. It used to invent the words "another
            // device" out of a null, which reads like a device that exists and was
            // the sentence the user quoted back as "the desktop took it over".
            _ownedBy.value = holder ?: ""
        }
        attach.onRtt = { sample ->
            // A round trip is proof the socket is there, whatever it said before.
            linkOffline = false
            pacing.onRttSample(sample)
            _link.value = linkStats()
        }
        attach.onEnded = {
            // The daemon says this shell is gone. Nothing is retried (the attach loop
            // stops by itself), so the page has to go: a page kept on a shell that no
            // longer exists is a frozen frame with no message and no retry, which is
            // what this callback was declared for. It was declared, invoked and wired
            // to nothing, so the removal depended entirely on the launcher's session
            // list arriving — which it does when the list socket is up, and does not
            // when the two sockets disagree. Cleared here, and App.kt prunes the page.
            _ownedBy.value = null
            _ended.value = true
        }
        attach.onSpeech = { speech.deliver(it) }
        attach.onSessions = { owners ->
            // The holder let go (`owner: null`, or it is us): the shell is free and
            // this page can attach again. Without this the notice was final until
            // the user tapped: the socket stayed healthy, so nothing reconnected,
            // and the daemon never pushed "it is free now" through the one frame
            // that said so.
            val mine = owners[sessionId]
            if (owners.containsKey(sessionId) && _ownedBy.value != null && mine == null) {
                _ownedBy.value = null
                attach.reconnectNow()
            }
        }
        attach.onOffline = {
            linkOffline = true
            // A takeover notice from before the link dropped describes a state
            // nobody has confirmed since; the offline notice is the honest one.
            _ownedBy.value = null
            pacing.onDisconnected()
            _link.value = linkStats()
        }
        attach.onAttached = {
            // The daemon accepted the attach: the link is up, even before the first
            // round trip has been measured, so input is no longer refused.
            linkOffline = false
            _rejected.value = false
            _link.value = linkStats()
        }
        attach.onRejected = {
            _rejected.value = true
        }
    }

    /**
     * Open the stream, if this page is the one on screen.
     *
     * Called by whoever *kept* the controller (see `rememberTerminalSurface`), never by
     * the constructor: construction happens during composition, and a composition can be
     * thrown away after building it — which left a connected controller holding the
     * shell, with nobody to dispose it.
     */
    fun begin() {
        if (disposed || started) return
        started = true
        if (active) attach.start(scope)
    }

    private var started = false
    private var disposed = false

    /**
     * Apply a snapshot to the engine, as [planSnapshot] decides.
     *
     * Rebuilding throws away the engine, and the selection lives in the engine
     * (ADR-0014), so on that path the selection is carried across by its *text*: a
     * snapshot normally arrives because new output did, so the same cells can hold
     * different words, and a restored selection covering text nobody chose is worse
     * than none because they will copy it. On the in-place path the engine — and with it
     * the scrollback, the viewport anchor and a selection that follows a resize —
     * survives.
     */
    private fun applySnapshot(cols: Int, rows: Int, screen: String, history: Boolean, m: TerminalModes) {
        // A sequence half-read before a rebuild must not complete against the new picture.
        speech.resetParser()
        // A snapshot is the truth: any outstanding prediction is settled by it (this is
        // also the recovery path after a mispredict).
        predictor.reset()
        staleJob?.cancel()
        // A snapshot only ever follows a *successful* attach, so this is the one place
        // that can prove the refusal no longer applies.
        _ownedBy.value = null
        historyLoading = false
        val current = _frame.value
        val inPlace = planSnapshot(current, cols, rows, m.alternateOn, history) == SnapshotApply.ReplayInPlace
        val resized = inPlace && current != null && (current.cols != cols || current.rows != rows)
        val keptSelection = if (resized) {
            // A real resize reflows the engine's own selection; re-projecting one that
            // never moved would move it.
            null
        } else {
            selectionBounds()?.let { b ->
                selectionText()?.takeIf { it.isNotEmpty() }?.let { b to it }
            }
        }
        if (inPlace) {
            if (resized) engine.resize(cols, rows)
            feed(
                resizeReplayBytes(
                    screen = screen,
                    cursorX = m.cursorX,
                    cursorY = m.cursorY,
                    cols = cols,
                    rows = rows,
                    modes = m,
                ),
            )
            // Same grid, new content: the selection survived at the same cells, and the
            // text is the only honest test of whether it still means what was chosen.
            if (!resized && keptSelection != null && selectionText() != keptSelection.second) clearSelection()
            hasSelection = engine.selectionText() != null
        } else {
            engine.close()
            engine = TerminalEngine(cols, rows)
            // State, then content, then cursor — see [snapshotBytes], which is the only
            // thing that decides that order.
            feed(
                snapshotBytes(
                    screen = screen,
                    alternateOn = m.alternateOn,
                    cursorX = m.cursorX,
                    cursorY = m.cursorY,
                    cols = cols,
                    rows = rows,
                    // Everything else the program had set, which a fresh engine does not
                    // know. See [modeSequence].
                    modes = m,
                ),
            )
            if (history) {
                // The page-in this controller asked for has been answered. Did it
                // reach *deeper* than what we already held? A page-in replaces the
                // scrollback with the keeper's newest `lines` rows, so asking for the
                // same count again would be a round trip for the same rows. When it
                // grew, the next ask doubles; when it did not, the keeper has no more
                // and asking again would spin.
                val afterTotal = engine.render()?.scrollTotal
                if (afterTotal == null || afterTotal <= historyTotalBefore) {
                    historyExhausted = true
                } else {
                    historyAskLines = (historyAskLines * 2).coerceAtMost(pacing.historyMaxLines())
                }
                historyLoading = false
                if (historyRestoreAtTop) {
                    // The request was made from the top of what was loaded, and a page-in
                    // only adds rows above it. How many now sit above the row the user
                    // was reading is not knowable here — live output that arrived between
                    // the request and this reply moved the count — so the top is the one
                    // place that is still honest.
                    historyRestoreAtTop = false
                    engine.scrollToTop()
                }
            } else {
                // An ordinary snapshot rebuilt the engine from the visible screen alone:
                // whatever scrollback it held is gone, so a page-in can be asked for again
                // from the start — and from a size decided *when the user asks*, not
                // now. This used to seed the ask here, and "here" is attach time, when
                // there is no round trip to classify the link by, so the first page was
                // always the fast-link one (5,000 rows, ~400 KB) even on a link that had
                // already been measured as slow a second later.
                historyExhausted = false
                historyAskLines = 0
                historyLoading = false
                historyRestoreAtTop = false
            }
            if (keptSelection != null) {
                val (bounds, text) = keptSelection
                // **The rows go back where they were, and the text is the test.** A
                // rebuild hands the new engine a fresh coordinate system, so a scroll
                // offset cannot re-project them; if these rows no longer hold this text,
                // the screen the selection was made on is gone, and the selection is
                // dropped rather than mis-copied.
                val onScreen = bounds[1] >= 0 && bounds[3] >= 0 && bounds[1] < rows && bounds[3] < rows
                if (!onScreen || !selectionSet(bounds[0], bounds[1], bounds[2], bounds[3]) || selectionText() != text) {
                    clearSelection()
                }
            } else {
                // A freshly built engine has no selection, so the flag must not claim
                // one — it would offer the user a copy of nothing.
                hasSelection = false
            }
        }
        modes = m
        // A fresh snapshot is the only thing that knows the foreground program again, so
        // it is also what re-arms the click path.
        mouseRefused = false
        _frame.value = engine.render()
    }

    /**
     * Feed the engine, then take whatever clipboard write arrived inside those
     * bytes.
     *
     * Every path that feeds bytes comes through here rather than calling the
     * engine directly, because the request travels *inside* the byte stream: a
     * path that fed the engine and skipped the drain would swallow a copy with
     * nothing to show for it, and it would be a path nobody thought to check.
     */
    private fun feed(bytes: ByteArray, length: Int = bytes.size) {
        // Observe, never remove: the engine ignores OSC 1338 (see `SpeechOscParser`), so the
        // same bytes still go to it. Only parsed when someone is listening.
        speech.feedBytes(bytes, length)
        engine.write(bytes, length)
        // Read the clipboard slot only when someone can take it. The read both
        // answers and *consumes*, so draining it with no handler installed would
        // discard the user's copy — a recomposition gap is enough to lose it, and
        // the loss is invisible because the program was told the write succeeded.
        if (onClipboardRequest != null) {
            engine.takeClipboardRequest()?.let { request ->
                // The program is untrusted: capped and cleaned before anyone acts on it,
                // and a failure of the platform clipboard is the clipboard's, not the
                // stream's (a throw here used to reach the receive loop and drop the
                // socket).
                admitClipboardRequest(request)?.let { admitted ->
                    try {
                        onClipboardRequest?.invoke(admitted)
                    } catch (e: Exception) {
                        appLog("clipboard write failed: ${e::class.simpleName}")
                    }
                }
            }
        }
    }

    /**
     * Paste text the way the program expects to receive a paste.
     *
     * A paste is not a keystroke, so it must not spend the sticky-modifier latch:
     * that latch belongs to what the user types next, and routing a paste through
     * [sendText] would quietly eat it. A program that turned bracketed paste on is
     * owed the markers, because without them every newline in the paste is a
     * keypress and a pasted paragraph runs its first line before the rest is even
     * visible.
     */
    override fun paste(text: String): Boolean {
        if (text.isEmpty()) return true
        if (modes.bracketedPaste) {
            // ESC stripped: the clipboard must not be able to close the paste early.
            return sendTextRaw("\u001b[200~" + sanitizeBracketedPaste(text) + "\u001b[201~")
        }
        return sendTextRaw(text)
    }

    /**
     * Apply one output frame, first reconciling it against any prediction.
     *
     * Feeding output before reconciling would paint the echo twice (once from
     * the prediction, once from the server).
     */
    private fun writeOutput(bytes: ByteArray) {
        if (!predict) {
            feed(bytes)
            scheduleRender()
            return
        }
        when (val c = predictor.reconcile(bytes)) {
            is Predictor.Consumption.Consume -> {
                staleJob?.cancel()
                if (c.remaining.isNotEmpty()) feed(c.remaining)
                scheduleRender()
            }
            Predictor.Consumption.Passthrough -> {
                feed(bytes)
                scheduleRender()
            }
            is Predictor.Consumption.Mispredict -> {
                // The prediction was wrong and a VT cannot un-apply it, so the
                // screen is rebuilt from the server instead. The frame is fed
                // first all the same: the snapshot restores the screen, but only
                // bytes can carry a control sequence, and dropping them here
                // silently eats a program's clipboard write or window title.
                staleJob?.cancel()
                feed(c.feed)
                attach.snapshotNow()
            }
        }
    }

    /**
     * Repaint at most once per frame.
     *
     * Output arrives per write() from the server, which under a flood (`cat` a big
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
        // Before [begin] the stream is not this controller's to open yet; `begin`
        // reads the flag. After [dispose] there is nothing to open.
        if (!started || disposed) return
        if (value) attach.start(scope) else attach.stop()
    }

    override fun sendKey(name: String) {
        val armed = armedModifiers()
        if (armed.armed) {
            // One Ctrl arms one key. Spending it here rather than in the chrome is
            // what makes a hardware key and a bar key behave the same. Spent even when
            // the key is then refused: the keystroke happened.
            spendLatch()
            sendModifiedKey(name, armed)
            return
        }
        sendNamedKey(name)
    }

    /** One protocol key, refused (and said so) while there is nowhere to send it. */
    private fun sendNamedKey(name: String): Boolean {
        if (!canSend()) return false
        // Submitting a line re-arms prediction: a fresh line is expected to echo
        // again, even if the previous one did not (a password prompt).
        if (name == "Enter" || name.endsWith("-Enter")) predictor.onLineSubmitted()
        return attach.keys(listOf(name))
    }

    /**
     * Send one key with the sticky modifiers applied.
     *
     * Every modified key goes as a protocol key name (`C-a`, `M-f`), characters
     * included: the daemon turns the name into bytes in one place (see
     * `Modifiers.kt`).
     */
    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        when (val k = modifiedKey(key, modifiers)) {
            is ModifiedKey.None -> {}
            is ModifiedKey.Literal -> sendTextRaw(k.bytes)
            is ModifiedKey.Named -> sendNamedKey(k.name)
        }
    }

    override fun sendText(text: String) {
        if (text.isEmpty()) return
        val armed = armedModifiers()
        if (!armed.armed) {
            sendTextRaw(text)
            return
        }
        // The first keystroke spends the latch. The decision itself is a pure
        // function beside the modifier semantics, so it can be tested without a
        // socket — which is the only reason the old rule could rot unnoticed here
        // while every test passed.
        val typed = spendLatchOnFirst(text, armed)
        if (typed.latchAfter.armed) modifiers = typed.latchAfter else spendLatch()
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
    private fun sendTextRaw(text: String): Boolean {
        // Refused, not queued and not painted: with no live socket (or another device
        // holding the shell) these bytes go nowhere, and a prediction would show the
        // user a command no shell will ever receive. The page says why (see
        // [LinkStats.offline]); a paste returns false so its caller can too.
        val gate = gate()
        if (!gate.canSend) return false
        returnToBottom()
        val bytes = text.encodeToByteArray()
        // The gate folds in the setting; the alternate screen (which does not echo the
        // way a shell does); a password prompt, which the emulator itself tells us
        // about — so we never have to guess from timing; and a shell this device does
        // not hold. Held elsewhere is not cosmetic: the daemon refuses the input, so a
        // prediction would paint a character that is then withdrawn a moment later.
        if (gate.canPredict && predictor.canPredict(bytes)) {
            // Paint it before the server is even asked. If the echo disagrees,
            // writeOutput() throws this away and repaints from a snapshot.
            feed(bytes)
            predictor.predict(bytes)
            armStaleCheck()
            _frame.value = engine.render()
        }
        // Under the daemon's per-message cap, or a big paste is refused whole.
        var sent = true
        for (piece in chunkInput(text)) sent = attach.input(piece) && sent
        return sent
    }

    /** The gate, from what this controller knows right now. */
    private fun gate(): InputGate = inputGate(
        connected = attach.isConnected,
        heldElsewhere = _ownedBy.value != null,
        predict = predict,
        alternateScreen = modes.alternateOn,
        passwordPrompt = _frame.value?.passwordInput == true,
    )

    private fun canSend(): Boolean = gate().canSend

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
    override fun scrollBy(rows: Int, pageInHistory: Boolean): Int {
        if (rows == 0) return 0
        // What the viewport *did*, not what it was asked to do. The emulator's own
        // scrollbar is the only thing that knows: its offset counts into the
        // scrollback from the top, so scrolling up decreases it, and a request the
        // engine declines — the alternate screen, or an end of the history — leaves
        // it exactly where it was. Reading it is what makes "one row per move,
        // skipped on the alternate screen" true by construction instead of by a
        // flag that can disagree with the viewport.
        val before = _frame.value?.scrollOffset
        engine.scrollBy(rows)
        renderNow()
        val after = _frame.value?.scrollOffset
        // No frame on either side means there is no viewport to have moved and
        // nothing anchored to it, so nothing shifts. That is the honest answer; the
        // alternative is asserting a frame exists on a path where it may not.
        val moved = if (before == null || after == null) 0 else (after - before).toInt()
        // The alternate screen has no scrollback: the engine declined, so a swipe
        // goes to the program (wheel events or arrows), as a desktop wheel does.
        if (moved == 0) {
            val f = _frame.value
            if (f != null) {
                when (val a = planAltScroll(rows, f, mouseRefused)) {
                    is AltScroll.Wheel -> attach.input(a.bytes)
                    is AltScroll.Keys -> sendKeyBatches(a.keys)
                    AltScroll.None -> {}
                }
            }
        }
        // History arrives when it is asked for by the act of scrolling to the end
        // of what is already here. It used to be a button, which is the machine
        // showing its working: two controls whose difference ("show me history"
        // vs "go back to now") is a distinction the user should never have had to
        // hold, because the gesture already means both.
        //
        // Not while a selection gesture is live (ADR-0014 §4.4): the reply is a
        // page-in, which rebuilds the engine, and the engine is where the drag's
        // selection lives. The drag stops at the top of what is loaded, and the
        // next drag asks — which is a released finger, so nothing is lost.
        if (rows < 0 && pageInHistory) maybeExtendHistory()
        return moved
    }

    /**
     * Page in older output when the user reaches the top of what is loaded.
     *
     * Once per page-in at most, and only when they are actually at the top: the
     * scrollback request re-sends the whole screen, so asking on every pixel of
     * drag would be a round trip per frame.
     */
    private fun maybeExtendHistory() {
        val f = _frame.value ?: return
        if (historyLoading || historyExhausted) return
        // The offset is counted from the top of what is loaded — 0 is the oldest
        // row and `total - len` is the live bottom — so 0 is the only place there
        // is older output to ask for. The old test compared the offset against
        // the total, which is always the larger number, so the answer was always
        // "not yet" and the daemon was never asked: the scrollback stopped at
        // whatever the client had happened to accumulate.
        if (f.scrollOffset > 0L) return
        // Refused (no live socket): leave the latch clear so the next drag asks again.
        // It used to be set before the send and cleared only by an arriving snapshot,
        // so one refused request during a reconnect disabled history for the rest of
        // the page's life.
        // **Never ask for less than is already loaded.** The reply *replaces* the
        // scrollback with the newest `lines` rows, so a smaller ask is a shrink: the
        // history the user can scroll back through gets shorter, and the reply then
        // also looks like "the keeper has no more" (`afterTotal <= before`), so the
        // client stops asking for good. Reachable since the page size started
        // following the link: a link that turns slow caps the ask at 4,000 while
        // 5,000 rows are loaded.
        val held = f.scrollTotal.toInt()
        val cap = pacing.historyMaxLines()
        // The size the user asked for, decided now: the link's classification is
        // current, and a first pull on a slow link is the small one.
        val base = if (historyAskLines > 0) historyAskLines else pacing.historyPageLines()
        val ask = maxOf(base, held + 1).coerceAtMost(cap)
        if (ask <= held) {
            // The link cannot deliver a deeper page than what is already loaded, so
            // there is nothing to ask for — and asking would hand back less.
            historyExhausted = true
            return
        }
        if (!attach.scrollback(ask)) return
        historyLoading = true
        historyRestoreAtTop = true
        historyAskLines = ask
        historyTotalBefore = f.scrollTotal
    }

    /** A scrollback request is in flight: one at a time. */
    private var historyLoading = false
    /** The keeper has no rows above what is loaded: stop asking. */
    private var historyExhausted = false
    /**
     * Rows to ask for, doubling per page-in up to the link's cap.
     *
     * Seeded from the link ([FramePacing.historyPageLines]) rather than from a
     * constant: a history page on a slow link is the largest message in the
     * protocol, and asking for 5,000 rows to a link that answers in 200 ms is a
     * stall the user reads as a broken terminal.
     */
    private var historyAskLines = 0
    /** The engine's scrollback total when the outstanding request was made. */
    private var historyTotalBefore = 0L
    /** The page-in was asked for from the top of what is loaded. */
    private var historyRestoreAtTop = false

    /**
     * Park the viewport on the live bottom, whatever history it was reading.
     *
     * The bottom is where a shell happens: the prompt is the newest line, and
     * output the user has not seen arrives there. This is the one control that
     * says "back to now" without making the user drag the whole way down.
     */
    override fun scrollToBottom() {
        engine.scrollToBottom()
        renderNow()
    }

    /**
     * Repaint from what the engine already holds.
     *
     * The one case that would otherwise be invisible: a snapshot that arrived before
     * this surface had been laid out, so it was rendered into nothing and never
     * re-rendered — because nothing about the pane changed afterwards, so no new
     * snapshot was ever sent. `renderNow` emits a fresh frame object (the frame has
     * no value equality), so the surface is asked to draw again. Declared on the
     * handle and left as a no-op default there, so a test double still composes.
     */
    override fun repaint() {
        renderNow()
    }

    /**
     * Text is typed at the prompt, and the prompt is the newest line: any text
     * this session sends returns the view to the live bottom, so the echo lands
     * where the user is looking rather than rows above where they were reading.
     * Keys are deliberately left alone — the arrows are navigation, and yanking
     * the view out from under one is its own bug.
     */
    private fun returnToBottom() {
        if (_frame.value?.viewportActive == false) {
            engine.scrollToBottom()
            renderNow()
        }
    }

    override var hasSelection by mutableStateOf(false)
        private set

    /** Tell the engine how the grid is drawn, so it can reason about distance. */
    override fun selectionGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int) {
        engine.setGeometry(columns, cellW, cellH, padLeft, screenH)
    }

    /**
     * Move one end of the selection to a cell, for a dragged handle.
     *
     * This is why the handles do not go through the gesture engine's drag: that drag
     * always moves the *end* endpoint, and a handle round the other end is the
     * whole point. Here the two ends are simply two coordinates.
     */
    override fun selectionSet(
        startCol: Int,
        startRow: Int,
        endCol: Int,
        endRow: Int,
    ): Boolean {
        val changed = engine.selectionSet(startCol, startRow, endCol, endRow)
        refreshSelection()
        return changed
    }

    override fun selectionBounds(): IntArray? = engine.selectionBounds()

    override fun selectionText(): String? = engine.selectionText()

    override fun clearSelection() {
        if (engine.selectionClear()) refreshSelection()
    }

    override fun selectAll() {
        engine.selectionSelectAll()
        refreshSelection()
    }

    /** The engine is the source of truth for whether anything is selected. */
    private fun refreshSelection() {
        hasSelection = engine.selectionText() != null
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
    override fun tapCell(col: Int, row: Int): Boolean {
        val f = _frame.value ?: return false
        // A tap that cannot be sent must not draw its outline: that reads as the tap
        // having worked.
        if (!canSend()) return false
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
                return true
            }
            is TapAction.Keys -> {
                sendKeyBatches(action.keys)
                return true
            }
            // Nothing was sent, so the surface has nothing to draw: no cursor
            // moved, no click went out.
            TapAction.Ignore -> return false
        }
    }

    /** Whether the attach socket is currently down, for the diagnostics line. */
    private var linkOffline = false

    private val _ownedBy = MutableStateFlow<String?>(null)

    /**
     * The shell is gone: ended, or unknown to the daemon.
     *
     * The page must not outlive it. `App.kt` watches this and drops the page, the
     * same way it drops one whose shell left the session list.
     */
    private val _ended = MutableStateFlow(false)

    /** The daemon refused the token: the pairing is stale, pair again. */
    private val _rejected = MutableStateFlow(false)

    override val rejected: StateFlow<Boolean> = _rejected

    override val ended: StateFlow<Boolean> = _ended

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
        // Any error may be the daemon's answer to the outstanding history request
        // (`scrollback_failed`, or `not_attached` if the attach went away): such a
        // request is answered with an error, not a snapshot, so the latch has to be
        // cleared here or it stays set for the life of the page and history is
        // silently disabled. Live output does not clear it either.
        if (historyLoading) {
            historyLoading = false
            historyRestoreAtTop = false
        }
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
     * sizes the session's pty to it, so the shell is always the right size for the
     * screen showing it rather than fixed at creation.
     */
    fun setGrid(cols: Int, rows: Int) {
        attach.setGrid(cols, rows)
    }

    /** Idempotent: both a forgotten and an abandoned composition may call it. */
    fun dispose() {
        if (disposed) return
        disposed = true
        staleJob?.cancel()
        latchJob?.cancel()
        attach.stop()
        engine.close()
        scope.cancel()
    }

    private companion object {
        const val DEFAULT_COLS = 120
        const val DEFAULT_ROWS = 40
        /**
         * Rows the first history page asks for. Small enough that the reply is a
         * quick round trip, large enough to cover a screenful many times over.
         */
        /** Kept for the tests and the default: the pacing owns the live value. */
        const val HISTORY_FIRST_LINES = FramePacing.HISTORY_FIRST_LINES
        /**
         * Ceiling on a history page. The daemon clamps a request to 20 000 and the
         * keeper retains 10 000, so asking past this is bytes nobody will see.
         */
        const val HISTORY_MAX_LINES = FramePacing.HISTORY_MAX_LINES
        /** How long an unconfirmed prediction may stay on screen before we
         *  conclude the pane is not echoing. Roughly 2-3 mobile round trips. */
        const val STALE_MS = 900L
        /** How long to wait for the server's verdict on a click before giving
         *  up and using cursor motion instead. Two mobile round trips. */
        const val CLICK_VERDICT_MS = 1200L
    }
}

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
 * On a rebuild the engine is fresh, so `1049h` here *establishes* a screen rather
 * than toggling one, and the saved primary screen it discards is genuinely
 * disposable: the next snapshot carries whatever is on screen then. (An engine that
 * is being replayed into never gets this; see [planSnapshot] and [resizeReplayBytes].)
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
 * `?1000` and not `?1002`/`?1003`: the server reports only that *some* mouse mode is on,
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

/**
 * Whether this snapshot is a resize the live engine can take in place.
 *
 * Three things have to be true, and each one is load-bearing:
 *
 * - **there is a frame** — the engine has been fed at least once, so there is
 *   something to preserve. The first snapshot of a session has nothing and is
 *   a rebuild like any other;
 * - **the grid changed** — a snapshot whose size matches the engine is not a
 *   resize, it is a resync, and a resync is asking for a clean parser state,
 *   which only a fresh engine delivers;
 * - **the screen did not change** — `?1049` cannot be sent here, because on an
 *   engine that already holds its screens it *switches* them rather than
 *   establishing one (see [snapshotBytes]). If the program moved between the
 *   primary and the alternate screen since the last frame, the replay below
 *   would land on the wrong one, so that case falls back to the rebuild.
 */
internal fun replayResize(
    current: TerminalFrame.Frame?,
    cols: Int,
    rows: Int,
    alternateOn: Boolean,
): Boolean =
    current != null &&
        (current.cols != cols || current.rows != rows) &&
        current.alternateOn == alternateOn

/**
 * A captured screen, as bytes that replace the visible screen of an engine that
 * keeps everything else.
 *
 * The difference from [snapshotBytes] is the whole point: a fresh engine needs
 * `?1049` to *establish* which screen it is on, and an engine that is being
 * resized already knows — sending it here would switch screens and orphan the
 * scrollback this path exists to keep. So the modes are re-asserted in their
 * idempotent forms (every `h`/`l` here is a no-op on an engine that already
 * holds them), then the state that would make a replay land in the wrong cells
 * is reset (scroll region, origin mode, insert mode), and only the visible
 * screen is erased: the scrollback above it is the point of this path.
 *
 * The terminator and the line feed are [snapshotReplay]'s problem, for the same
 * reasons it documents.
 */
internal fun resizeReplayBytes(
    screen: String,
    cursorX: Int,
    cursorY: Int,
    cols: Int,
    rows: Int,
    modes: TerminalModes = TerminalModes(),
): ByteArray {
    val state = StringBuilder()
    state.append(modeSequence(modes))
    state.append("\u001b[r\u001b[?6l\u001b[4l\u001b[H\u001b[2J")
    if (screen.isNotEmpty()) state.append(snapshotReplay(screen))
    state.append(cursorSequence(cursorX, cursorY, cols, rows))
    return state.toString().encodeToByteArray()
}