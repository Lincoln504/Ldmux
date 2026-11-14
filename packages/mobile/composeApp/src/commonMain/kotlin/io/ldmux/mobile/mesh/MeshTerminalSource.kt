package io.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.ldmux.mobile.AltScroll
import io.ldmux.mobile.BackgroundSession
import io.ldmux.mobile.ClipboardRequest
import io.ldmux.mobile.TapAction
import io.ldmux.mobile.TerminalHandle
import io.ldmux.mobile.TerminalSource
import io.ldmux.mobile.planAltScroll
import io.ldmux.mobile.planTap
import io.ldmux.mobile.Modifiers
import io.ldmux.mobile.SCROLL_OVERLAP
import io.ldmux.mobile.term.TerminalEngine
import io.ldmux.mobile.term.TerminalFrame
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import io.ldmux.mobile.appLog

/**
 * What the shared terminal surface draws on the mesh route.
 *
 * The surface is the tailnet route's own (`TerminalSurfaceFor`): same painter, same gestures,
 * same selection handles, same cursor, same IME field. This is the other end of it. Where the
 * tailnet source is fed by an attach stream, this one is fed by [show], once per screen the host
 * committed, through [meshScreenBytes]; and where the tailnet source sends keystrokes straight
 * out, this one hands them to [input], the batcher that spends the link's few records well.
 *
 * Everything about *input* is [input]'s (`TerminalHandle by input`), so a key, a paste and a
 * sticky modifier behave exactly as they did before this source existed. What is added here is
 * only what the surface asks of a terminal: a frame, a selection, a tap.
 *
 * Not carried, because the link does not carry it: mouse tracking (a tap is always cursor keys).
 * Scrollback *is* offered, but it is the client's own: the host numbers the row at the top of
 * each screen, so [MeshHistory] keeps the rows that left it, and [scrollBy] reads them. Resize
 * *is* carried (profile version 3): the grid the surface measures goes to [onGrid], and the
 * driver asks the host to fit its pty to it, so the host's grid is the phone's and a line is
 * never clipped.
 */
/** How many rows of local history a mesh view keeps. A screenful is what a scroll aims at; this
 * is a few dozen screens, far past what anyone reads back through on a radio. */
private const val MESH_HISTORY_MAX = 2_000

/**
 * How long a command's answer may take before the bar says it is late.
 *
 * Before the route is added to it ([answerDeadlineMs]), and a floor rather than a measurement: a
 * direct answer on a neighbour arrives in a second, and one across twenty hops takes a minute. The
 * line reports lateness; it never cancels the wait, because the answer is still coming.
 */
internal const val ANSWER_WAIT_MS = 20_000L

/** How long "done" stays on the bar after the answer arrived, before it reads the draft again. */
internal const val SETTLED_SHOW_MS = 2_500L

class MeshTerminalSource(
    private val input: MeshTerminalHandle,
    private val newEngine: (Int, Int) -> TerminalEngine = { c, r -> TerminalEngine(c, r) },
    /**
     * The same monotonic clock the handle stamps input with, for the draw-through window.
     *
     * Required, not defaulted: a fixed clock would leave the window open for ever, which turns
     * batch mode into live mode by accident.
     */
    private val now: () -> Long,
    /**
     * Repeaters on the route to the host, asked afresh each time a wait is sized.
     *
     * **A wait for something that crosses the mesh is a function of how far it crosses.** The
     * history page is one record each way, so on a twenty-hop route the answer takes a minute and a
     * flat ten-second wait declared it lost — the up key then asked again while the first page was
     * still travelling. Defaulted because a source with no link (a test) has no route to ask about.
     */
    private val hops: () -> Int? = { null },
) : TerminalSource, TerminalHandle by input {

    private var engine: TerminalEngine? = null
    private var cols = 0
    private var rows = 0
    private var last: MeshScreen? = null

    /**
     * The shell the drawn screen belongs to.
     *
     * **This, not a UI effect, is what makes a newly opened shell paint.** The hold draws a screen
     * only when nothing has been painted yet (`acceptScreen(painted = last != null)`), which was
     * judged over the source's whole life: a previous shell's last screen left `last` set, the new
     * shell's first screen was therefore "not the first", and the hold kept it. The screen effect
     * that cleared the source was a separate `LaunchedEffect` keyed on the attached id, so whether
     * it ran before the new shell's first commit depended on effect ordering — and on a public
     * channel, where the first full screen takes seconds, the bar sat at "view 2 behind" over a
     * blank page until a refresh.
     *
     * A screen from a different shell than the last one drawn is the first screen of that shell,
     * by definition, and is treated as one.
     */
    private var drawnShell: String? = null

    /**
     * Rows that have left the live screen, oldest first.
     *
     * The link carries one screen and no history (`MESH-GATEWAY`), so this is the client's own:
     * the host numbers the row at the top of each screen (`firstLine`), and the rows between two
     * of those numbers are exactly what scrolled off. Nothing is invented - an unknown line
     * number accumulates nothing - and the buffer is bounded, because a build log is not a
     * document anyone scrolls to the top of.
     */
    private val history = MeshHistory(MESH_HISTORY_MAX)

    private val _frame = MutableStateFlow<TerminalFrame.Frame?>(null)
    override val frame: StateFlow<TerminalFrame.Frame?> = _frame

    /** A draft echo is currently drawn on top of the host's screen. */
    private var echoing: Boolean = false

    /**
     * The draft as it should be echoed, kept so a repaint can put it back.
     *
     * Empty when nothing is typed. Without this the echo lasted only until the next paint: the
     * answer's repaint erased the text while the bar went on counting it — text the screen no longer
     * showed and the user could not see to correct. Now every paint writes "the host's screen, plus
     * what is still typed", which is the same invariant [echoHeld] states, just also true after a
     * repaint.
     */
    private var echoText: String = ""

    /**
     * The screen [MeshHistory] was last fed as "the new one", so a screen that arrives twice is
     * noted once.
     */
    private var notedScreen: MeshScreen? = null

    /** What this page's command is doing, for the bar. */
    private var state: BatchState by mutableStateOf(BatchState.Idle)

    /** When the last command went out, so lateness is measured from the press. */
    private var sentAt: Long = 0

    /** When "done" stops being shown. */
    private var settledUntil: Long = 0

    /** What this page's command is doing, for the bar. */
    internal val batchState: BatchState get() = state

    /**
     * Whether a screen has been drawn at all.
     *
     * Before the first paint the page is blank, and the screen says so rather than drawing an empty
     * engine: the surface has nothing to show until the host's first screen arrives.
     */
    val hasScreen: Boolean get() = last != null

    /**
     * Send the draft: **one command, one message**, and the answer is one screen.
     *
     * Returns false when there was nothing typed or the engine refused the send, in which case the
     * draft is untouched — a refused command is still the operator's text, and dropping it would
     * make a failure look like a successful send.
     */
    suspend fun send(): Boolean {
        val items = input.draft.peek()
        if (items.isEmpty()) return false
        val send = onSend ?: return false
        state = BatchState.Sending
        refreshRefused = false
        refreshed()
        if (!send(items)) {
            state = BatchState.Idle
            BackgroundSession.setOwed(false)
            return false
        }
        // **Taken only after the engine accepted it.** A send that throws or refuses leaves the
        // draft where it was, so the press can be repeated without retyping.
        input.draft.clear()
        input.refreshPending()
        sentAt = now()
        state = BatchState.Waiting
        // **The release waits for this.** Dropping the page mid-command throws away the answer and,
        // at range, the bootstrap that produced it.
        BackgroundSession.setOwed(true)
        return true
    }

    /**
     * Ask for the screen again. **Never touches the draft**, so looking at a shell can never run
     * what is typed in it.
     *
     * **The press says what it did, or it looks broken.** This used to invoke a callback and change
     * nothing else, so on a screen that was already correct the only visible effect of pressing
     * Refresh was none — reported as "the refresh button doesn't work like it used to". It now
     * enters [BatchState.Waiting] like a send does, which reuses the one clock that already knows
     * how long an answer may take: no answer turns it into [BatchState.NoAnswer] with its own
     * sentence, and a `Settled` clears it. A refusal (no session, no allowance) clears the state
     * and leaves [refreshRefused] set, so the bar can say why instead of pretending to work.
     */
    fun refresh() {
        val ask = onRefresh ?: return
        refreshed()
        // **A refusal is a fact, and the bar has a line for it.** `ask()` reports whether the
        // engine took the request; discarding that answer left `refreshRefused` permanently false
        // and its line in the bar unreachable, so a press with no session looked like a press that
        // worked. What settles a wait that *was* accepted is the answer arriving, which
        // `answered()` handles for a screen and the driver handles for a list.
        if (!ask()) {
            state = BatchState.Idle
            refreshRefused = true
            BackgroundSession.setOwed(false)
            return
        }
        state = BatchState.Waiting
        sentAt = now()
        refreshRefused = false
        BackgroundSession.setOwed(true)
    }

    /**
     * Whether the last refresh was refused before it reached the air.
     *
     * A refusal is a fact the operator can act on (pair the host, wait for the hour's allowance),
     * and it is not the same as "asked and no answer came", which is what [BatchState.NoAnswer]
     * means.
     */
    var refreshRefused: Boolean by mutableStateOf(false)
        private set

    /**
     * The host answered: its screen arrived.
     *
     * Called from the answer event, not from a timer. A `Settled` is the host's own word that the
     * pane stopped changing, which is the only honest end for "waiting for the answer".
     */
    fun answered() {
        if (state == BatchState.Waiting || state == BatchState.NoAnswer) {
            state = BatchState.Settled
            settledUntil = now() + SETTLED_SHOW_MS
        }
        // The answer arrived, so nothing is owed any more.
        BackgroundSession.setOwed(false)
    }

    /**
     * Advance the bar's clock: a wait that has gone on too long is late, and "done" is not for ever.
     *
     * Called on the screen's tick. Both transitions are statements about time, not about the air, so
     * neither needs an event to trigger it.
     */
    fun batchTick() {
        val t = now()
        when (state) {
            BatchState.Waiting -> {
                if (t - sentAt > answerDeadlineMs(ANSWER_WAIT_MS, hops())) {
                    state = BatchState.NoAnswer
                    // A wait that has run out is not an operation any more: the release must not
                    // be held open by an answer that is not coming.
                    BackgroundSession.setOwed(false)
                }
            }
            BatchState.Settled -> {
                if (t > settledUntil) state = BatchState.Idle
            }
            BatchState.Idle -> {}
            else -> {}
        }
    }

    /**
     * Drop everything typed and every rendering of it (the screen going away).
     *
     * What is typed is the one plaintext this app keeps, and it can sit for minutes on a slow link.
     * Persistence was never the risk (nothing is saved); surviving in the background was.
     */
    fun scrub() = input.scrub()

    /** The bar's status line is drawn from this; reading the draft recomposes only the bar. */
    private fun refreshed() = input.refreshPending()

    /**
     * Why a scroll did nothing, while it is worth saying.
     *
     * The mesh pad's arrows are the same three controls as the tailnet's, and here they can do
     * nothing at all: the host sends no history and the client accumulates its own, so a session
     * that has not scrolled yet has no history to scroll into. A button that returns 0 with no
     * word about it reads as broken — measured on the device 2026-11-03. Cleared by the next
     * scroll that moved, or by the screen after it has shown it.
     */
    var scrollNotice: String? by mutableStateOf(null)
        private set

    /** The host's `Key` names are the tailnet's, but the mesh bar sends no `S-` forms. */
    override val shiftKeys: Boolean get() = false

    /**
     * Never called: nothing on this link can set the clipboard (the rows are plain text with every
     * control character removed, so an OSC 52 cannot reach the engine). Kept because the surface
     * assigns it.
     */
    override var onClipboardRequest: ((ClipboardRequest) -> Unit)? = null

    override fun setActive(value: Boolean) {}

    /** Where the surface's measured grid goes: the driver's request to the host to fit its pty. */
    var onGrid: ((Int, Int) -> Unit)? = null

    /**
     * The grid the surface can paint at the user's own font size, measured by its painter. The
     * tailnet sends the same number to the daemon as a resize; here it is the `Resize` message.
     */
    override fun setGrid(cols: Int, rows: Int) {
        onGrid?.invoke(cols, rows)
    }

    /**
     * Put [screen] on the engine. Called for every change of the committed screen, on the main
     * thread (the engine is single-threaded). The same screen twice is skipped, because a repaint
     * costs a frame decode and nothing changed.
     *
     * **There is no freeze any more.** The old model had to decide whether a screen "may be drawn
     * now", because the host streamed and the operator had asked to be left alone. In v15 the host
     * sends one screen per command, so every screen that arrives is the answer to a press that was
     * made: it is drawn. `docs/RADIO-PLAN.md` §7.
     *
     * A grid that changes size (the host re-described its terminal) is a new engine: the old one's
     * cells are the wrong shape, and a selection into them would mean nothing.
     */
    fun show(screen: MeshScreen, shell: String? = null) {
        if (screen.cols <= 0 || screen.lines.isEmpty()) return
        // One shell's rows are never drawn under another's name, and a new shell starts blank.
        if (shell != null && shell != drawnShell) {
            forgetScreen()
            drawnShell = shell
        }
        applyScreen(screen)
    }

    /**
     * Forget the screen: the engine and its frame go, so the surface draws nothing until the next
     * [show]. Used when the page changed, so one shell's rows are never drawn under another's name.
     */
    private fun applyScreen(screen: MeshScreen) {
        val c = screen.cols
        val r = screen.lines.size
        if (c <= 0 || r <= 0) return
        io.ldmux.mobile.appLog("mesh show ${c}x$r engine ${cols}x$rows offset ${history.offset} same ${screen == last}")
        if (screen == last && engine != null) {
            // A screen identical to the one drawn may still need drawing: if a held echo is on
            // top of it, this is the moment the echo goes and the truth comes back. Without this
            // a sync answered by an unchanged screen left the fake text on screen for good.
            if (echoing) {
                echoing = false
                paint()
            }
            return
        }
        // Keep the rows that left before the new screen replaces the old one. A reflow (a new
        // grid) or a jump backwards is not a scroll, and adds nothing. The previous screen is the
        // last one history saw, which after a freeze is the last *held* screen, not the drawn one.
        val previous = last
        if (screen != notedScreen) history.note(notedScreen ?: previous, screen)
        notedScreen = screen
        if (engine == null || c != cols || r != rows) {
            val reflow = engine != null && c != cols
            engine?.close()
            engine = newEngine(c, r)
            cols = c
            rows = r
            // **A width change is a reflow; a height change is not a reason to leave history.** The
            // view used to go to the live bottom whenever either changed, so a shell whose last row
            // appeared and disappeared (a prompt, a status line) yanked the operator out of the
            // history they had scrolled to — measured on the device 2026-11-09 as `offset 33` then
            // `offset 0` two screens later. The offset counts rows above the live bottom, and
            // `window()` clamps it, so a shorter grid keeps the position it can.
            if (reflow) history.toBottom()
            hasSelection = false
        }
        last = screen
        // A view parked in history is somewhere the user put it. New output must not drag it
        // back to live; the pad's bottom button is how it returns.
        if (history.offset == 0) paint()
    }

    /**
     * Write what is being held into the engine, so the user sees their typing at the cursor.
     *
     * This is a **fake**, and it is built so that it cannot drift: every call rebuilds the host's
     * last screen first and then writes the held text on top, so the echo is always exactly "the
     * shell as it was, plus what you have typed since". Painting each keystroke onto the previous
     * echo would accumulate an image that disagrees with the screen it is a fake of, and no amount
     * of care about backspace and wrapping keeps that true.
     *
     * Two deliberate limits:
     *
     *  - **Autowrap is off while the echo is written, and the draft is cut to the cells that are
     *    left on this row.** A wrapped line would scroll the screen, and a row pushed into the local
     *    scrollback by a fake is history that never happened; a draft written past the last column
     *    overwrote that column on top of itself. [draftEcho] windows it instead.
     *  - **Nothing is echoed while the view is parked in history** ([MeshHistory.offset]). The
     *    user is reading rows above the live screen; typing into the middle of them would be a
     *    fake of the wrong screen.
     *
     * An empty [text] removes the echo and draws the host's screen again.
     */
    fun echoHeld(text: String) {
        // Remembered, not just drawn: [paint] puts it back on the host's next screen, so a repaint
        // while text is still held does not erase the text the bar is still counting. Remembered
        // **as it is**, because turning it into cells needs the row and the width it will be drawn
        // at, and those are known at paint time and not before ([draftEcho]). Nothing here is a
        // safety filter: that lives in [draftEcho] too, which is the one thing that writes the
        // draft into an engine that executes escape sequences.
        echoText = text
        paint()
    }

    /** Draw the live screen, or the history window at [offset] rows above it. */
    private fun paint() {
        echoing = false
        val screen = last ?: return
        val e = engine ?: return
        if (history.offset == 0) {
            val bytes = meshScreenBytes(screen, cols, rows)
            e.write(bytes, bytes.size)
            // **The held text goes back on top.** A paint is "the host's screen as it is now", and
            // the held text has not been sent yet, so the honest picture is both. Not on a
            // full-screen program, though: the same rule VS Code's local echo follows for `vim`,
            // `less` and `tmux` — a program that repaints the screen decides where text lands, so
            // a fake written at the cursor lands in somebody else's drawing. It stays counted in
            // the bar instead. Nothing is echoed in history either, where the rows above the live
            // screen are not where typing goes.
            if (echoText.isNotEmpty() && !screen.alternate) {
                // Autowrap off around the whole echo, and faint (`ESC[2m`) around the text:
                // provisional text looks provisional. That is the one thing a fake must never be —
                // mistakable for the shell's own output, which is what the same characters will
                // look like when the send comes back.
                //
                // **[draftEcho] decides the cells, and it can decide them as empty.** A draft with
                // nothing visible in it (a lone arrow key) draws nothing at all, and writing the
                // escape pair around an empty string would leave the engine in the faint pen for
                // nothing.
                val drawn = draftEcho(echoText, screen.cursorX, cols)
                if (drawn.isNotEmpty()) {
                    val payload = "\u001b[?7l\u001b[2m" + drawn + "\u001b[22m\u001b[?7h"
                    val held = payload.encodeToByteArray()
                    e.write(held, held.size)
                    echoing = true
                }
            }
        } else {
            val window = screen.copy(
                lines = history.window(screen, rows),
                cursorX = 0,
                cursorY = rows - 1,
                dead = false,
            )
            val bytes = meshScreenBytes(window, cols, rows, showCursor = false)
            e.write(bytes, bytes.size)
        }
        _frame.value = e.render()
    }

    override fun repaint() {
        _frame.value = engine?.render()
    }


    /**
     * Forget the screen: the engine and its frame go, so the surface draws nothing until the next
     * [show]. Used when the host is attached to another shell, so that one shell's rows are never
     * drawn under another's name.
     */
    fun clear() {
        forgetScreen()
        drawnShell = null
    }

    /** Drop the engine, the drawn screen and everything that belonged to it. */
    private fun forgetScreen() {
        engine?.close()
        engine = null
        last = null
        cols = 0
        rows = 0
        history.clear()
        hasSelection = false
        _frame.value = null
        // The echo belongs to a screen that no longer exists, and so does the row history was last
        // fed: both would be a fake of the previous shell drawn under the next one's name.
        echoText = ""
        echoing = false
        notedScreen = null
        // A command's wait belongs to the page it was made on: leaving ends it, so a returning
        // page never resumes a count from a visit that is over.
        state = BatchState.Idle
        scrollNotice = null
    }

    /** Free the engine. Idempotent. */
    fun close() {
        engine?.close()
        engine = null
    }

    // ── selection: the engine's, exactly as the tailnet route's ─────────────────────────────

    override var hasSelection by mutableStateOf(false)
        private set

    override fun selectionGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int) {
        engine?.setGeometry(columns, cellW, cellH, padLeft, screenH)
    }

    override fun selectionSet(startCol: Int, startRow: Int, endCol: Int, endRow: Int): Boolean {
        val changed = engine?.selectionSet(startCol, startRow, endCol, endRow) ?: false
        refreshSelection()
        return changed
    }

    override fun selectionBounds(): IntArray? = engine?.selectionBounds()

    override fun selectionText(): String? = engine?.selectionText()

    override fun clearSelection() {
        if (engine?.selectionClear() == true) refreshSelection()
    }

    override fun selectAll() {
        engine?.selectionSelectAll()
        refreshSelection()
    }

    private fun refreshSelection() {
        hasSelection = engine?.selectionText() != null
        _frame.value = engine?.render()
    }

    /**
     * Scroll the client's own history, or - on the alternate screen, which has none - send the
     * program the keys a wheel would. Negative is up, as everywhere else.
     */
    /**
     * Typing goes to the prompt, and the prompt is the live bottom.
     *
     * The same rule the tailnet route follows, stated here because the two handles are
     * separate: reading history and then typing sent the keys to a prompt off screen, and
     * nothing moved, which is indistinguishable from a link that stopped listening.
     */
    private fun followInput() {
        if (history.offset != 0) scrollToBottom()
    }

    override fun sendKey(name: String) {
        followInput()
        input.sendKey(name)
    }

    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        followInput()
        input.sendModifiedKey(key, modifiers)
    }

    override fun sendText(text: String) {
        if (text.isEmpty()) return
        followInput()
        input.sendText(text)
    }

    override fun paste(text: String): Boolean {
        if (text.isEmpty()) return true
        followInput()
        return input.paste(text)
    }

    override fun scrollPage(direction: Int) {
        if (rows <= 0) return
        val step = io.ldmux.mobile.pageStep(rows)
        scrollBy(if (direction < 0) -step else step)
    }

    override fun scrollBy(rows: Int, pageInHistory: Boolean): Int {
        val screen = last ?: return 0
        if (screen.alternate) {
            val f = _frame.value ?: return 0
            return when (val action = planAltScroll(rows, f, mouseRefused = true)) {
                is AltScroll.Keys -> {
                    for (name in action.keys) input.sendPlainKey(name)
                    0
                }
                is AltScroll.Wheel -> 0
                AltScroll.None -> 0
            }
        }
        val moved = history.scroll(rows)
        if (moved != 0) {
            scrollNotice = null
            awaitingHistory = false
            paint()
        } else if (rows < 0) {
            askForHistory(rows)
        } else {
            scrollNotice = null
        }
        return moved
    }

    /**
     * The up key ran out of locally accumulated history, so **ask the host for the page above**.
     *
     * **The keeper has the rows; the link only carries the live screen.** A phone that joined a
     * shell late, or was frozen while the shell printed, cannot have them from its own
     * accumulation — and answering "no history yet on this link" was true of this client and
     * useless to the operator, whose up key means "fetch a page". The request names the absolute
     * line immediately above the oldest row held here, so a page joins what is held instead of
     * overlapping it.
     *
     * A host that cannot number its lines (`firstLine == 0`) has no line to ask for, and a keeper
     * with no history above has nothing to give: the first keeps the old sentence, the second gets
     * "start of history" from the empty page.
     */
    private fun askForHistory(rows: Int) {
        val screen = last
        val top = screen?.let { if (it.firstLine > 0) it.firstLine.toInt() - history.size else 0 } ?: 0
        val want = (screen?.lines?.size ?: 17).coerceIn(1, MeshTerm.MAX_HISTORY_ROWS)
        val ask = onWantHistory
        if (top <= 0 || ask == null) {
            scrollNotice = if (history.size == 0) "no history yet on this link" else "start of history"
            paint()
            return
        }
        // **`rows` counts what the press wants, `want` is a page.** The host serves at most one
        // page-sized answer per press, so a press never costs two records.
        // **A lost request expires; it does not block the key for ever.** The radio's own
        // retransmission is bounded (six airtimes per hop), so a page that has not arrived by
        // [HISTORY_WAIT_MS] is treated as lost and the next press asks again.
        if (awaitingHistory && now() - historyAsked < answerDeadlineMs(HISTORY_WAIT_MS, hops())) return
        awaitingHistory = true
        historyAsked = now()
        scrollNotice = "fetching history"
        // **Fetch a page, move by the press.** A page is one request either way, so a one-row key
        // asks for the same page a page key does and then lands one row above where it was: the
        // rest of the page is already in hand for the next press, and no press costs two records.
        pendingAdvance = (-rows).coerceIn(1, want)
        ask((top - want).coerceAtLeast(0), want)
        paint()
    }

    /** Wired by the screen to the driver: ask the host for [rows] lines from absolute line [from]. */
    var onWantHistory: ((Int, Int) -> Unit)? = null

    /**
     * Wired by the screen: send this draft as one command. True when the engine took it.
     *
     * The source does not know how a command reaches the air; it knows that what is typed is one, and
     * that a refusal must leave the draft alone. `docs/RADIO-PLAN.md` §7.
     */
    var onSend: (suspend (List<DraftItem>) -> Boolean)? = null

    /** Wired by the screen: ask the host for this page's screen again. Never spends the draft. */
    /**
     * Ask the host for the screen. Returns false when the request could not be made at all.
     *
     * **A Boolean, not Unit**: a refresh that never left this process and one that was answered by
     * silence are different facts, and the bar says which.
     */
    var onRefresh: (() -> Boolean)? = null

    /** A history request is in flight, so an up key pressed again does not queue another. */
    private var awaitingHistory = false

    /** When it went out, so a lost request does not leave the up key dead for ever. */
    private var historyAsked = 0L

    /** How many rows the press that asked for the page wanted to move. */
    private var pendingAdvance = 1

    /**
     * A page of the shell's own scrollback arrived: prepend it and draw it.
     *
     * An empty page is the host's answer that nothing is above — `last` alone cannot say that, since
     * a full page and an empty one are both "last" of their answer.
     */
    private companion object {
        /**
         * How long a history request may be in flight before the up key may ask again, before the
         * route's own answer deadline is added to it.
         */
        const val HISTORY_WAIT_MS = 10_000L
    }

    /**
     * **The answer to an up key: rows from the shell's own scrollback, placed where they belong.**
     *
     * The page states the absolute line of its first row, so it is placed by what it says rather
     * than by what was asked for. Three cases, and all three are real:
     * - it ends exactly at this client's oldest row (the normal case): prepend it;
     * - it overlaps rows already held (the host served from a line this client has): trim the tail,
     *   so nothing is shown twice;
     * - it ends above this client's oldest row (the pane dropped those rows since the request): the
     *   gap is named in the notice rather than spliced over, because inventing rows is worse than
     *   showing a break.
     *
     * `older` says whether the pane holds more history above, so the operator learns "start of
     * history" without spending a record to find out.
     */
    fun prependHistory(page: TermMessage.HistoryPage) {
        awaitingHistory = false
        // The press that asked owns its movement, once: a later page must not inherit it.
        val advance = pendingAdvance
        pendingAdvance = 1
        val screen = last ?: return
        if (page.lines.isEmpty()) {
            // An empty answer is a fact: the pane had nothing to serve, and `older` says whether
            // that is the top of the history or a request the host could only clamp.
            scrollNotice = if (page.older) "history is above this" else "start of history"
            paint()
            return
        }
        // **Where the page belongs, in the host's own numbering.** `top` is the absolute line of
        // this client's oldest held row, so rows at or above it are new and rows below it are ones
        // already on screen. Placing by number is what makes an overlap harmless and a gap visible:
        // the alternative, assuming every page joins, shows rows twice or splices a hole shut.
        val top = if (screen.firstLine > 0) screen.firstLine.toInt() - history.size else 0
        val pageEnd = page.from + page.lines.size
        val take = (minOf(pageEnd, top) - page.from).coerceAtLeast(0)
        val missing = (top - pageEnd).coerceAtLeast(0)
        if (take == 0) {
            // Every row of the page is one this client already holds, or below it.
            scrollNotice = if (page.older) null else "start of history"
            paint()
            return
        }
        val keep = page.lines.subList(0, take)
        scrollNotice = when {
            missing > 0 -> "history has a gap here"
            page.older -> null
            else -> "start of history"
        }
        // `offset` is read **before** the prepend: `MeshHistory.prepend` parks itself on the new
        // top, so asking afterwards would measure the wrong position.
        val held = history.size
        val was = history.offset
        val atTop = was >= held
        history.prepend(keep)
        history.park(if (atTop) held + advance else was + keep.size)
        paint()
    }

    override fun scrollToBottom() {
        scrollNotice = null
        if (history.offset == 0) return
        history.toBottom()
        paint()
    }

    /**
     * Rows this view is parked above the live bottom; 0 means it is following the shell.
     *
     * Read-only, and the observable the key bars are judged by: a bar key goes through [sendKey],
     * which follows the input back to live, while the page's own batcher would send it into a
     * prompt the user cannot see. That is RELEASE-AUDIT 9.14, and it was fixed on the tailnet and
     * reopened here, so it is a fact worth being able to assert.
     */
    internal val scrolledRows: Int get() = history.offset

    /** The screen has shown [scrollNotice]; stop drawing it. */
    fun clearScrollNotice() { scrollNotice = null }

    /**
     * A tap moves the shell's cursor with the arrow keys, by the same plan as the tailnet route
     * ([planTap]) — the mouse path is never taken, since nothing here reports mouse tracking. The
     * keys go through [input], so a long walk is coalesced into a few records and a refusal (the
     * queue is full) is reported as a tap that did nothing, with no outline drawn.
     */
    override fun tapCell(col: Int, row: Int): Boolean {
        val f = _frame.value ?: return false
        return tapOn(f, col, row)
    }

    /** [tapCell] against a given frame, so the rule is testable without a native engine. */
    internal fun tapOn(f: TerminalFrame.Frame, col: Int, row: Int): Boolean {
        return when (val action = planTap(col, row, f, mouseRefused = true)) {
            is TapAction.Keys -> {
                var all = true
                for (name in action.keys) all = tapKey(name) && all
                all
            }
            else -> false
        }
    }

    /** A tap's arrow key never spends a Ctrl the user armed for their next keystroke. */
    private fun tapKey(name: String): Boolean = input.sendPlainKey(name)
}
