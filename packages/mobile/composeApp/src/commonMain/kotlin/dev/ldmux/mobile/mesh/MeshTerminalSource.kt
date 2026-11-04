package dev.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.ldmux.mobile.AltScroll
import dev.ldmux.mobile.ClipboardRequest
import dev.ldmux.mobile.TapAction
import dev.ldmux.mobile.TerminalHandle
import dev.ldmux.mobile.TerminalSource
import dev.ldmux.mobile.planAltScroll
import dev.ldmux.mobile.planTap
import dev.ldmux.mobile.term.TerminalEngine
import dev.ldmux.mobile.term.TerminalFrame
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

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

class MeshTerminalSource(
    private val input: MeshTerminalHandle,
    private val newEngine: (Int, Int) -> TerminalEngine = { c, r -> TerminalEngine(c, r) },
) : TerminalSource, TerminalHandle by input {

    private var engine: TerminalEngine? = null
    private var cols = 0
    private var rows = 0
    private var last: MeshScreen? = null

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

    /**
     * Public-channel mode: input waits on the phone and the view does not follow the host.
     *
     * The state is [PublicChannelHold]'s, because the rule is "may this screen be drawn now", and
     * that question has no engine in it. This source is the only thing that consults it, and the
     * screen is the only thing that turns it on or asks it to sync. See ADR-0026.
     */
    private val hold = PublicChannelHold()

    /** The newest screen the host sent while the view was frozen. */
    private var pendingScreen: MeshScreen? = null

    /** A held echo is currently drawn on top of the host's screen, so the next paint must clear it. */
    private var echoing: Boolean = false

    /** The mode is engaged: typing is held and the view is frozen until a sync. */
    val publicChannel: Boolean get() = hold.on

    /** A sync is out and its answer has not arrived. */
    val awaitingSync: Boolean get() = hold.awaiting

    /**
     * The buffer is being drained: the pump may send right now.
     *
     * True only from the sync press until the buffer is empty. [awaitingSync] stays true after
     * that, while the answer is on its way — and typing during that wait is held, because "no
     * data is sent live" does not pause just because a send is in flight.
     */
    val flushingHeld: Boolean get() = hold.flushing

    /** Screens the host sent while the view was frozen; the newest is what a pull shows. */
    val heldUpdates: Int get() = hold.updates

    /**
     * Turn the mode on or off. Turning it off draws whatever the host sent meanwhile, so leaving
     * the mode never leaves a stale screen claiming to be live.
     */
    fun setPublicChannel(on: Boolean) {
        if (on == hold.on) return
        hold.set(on)
        if (!on) pullHeld()
    }

    /**
     * The sync button's action. True when something is waiting and the pump must flush it; false
     * when nothing was waiting, in which case the newest held screen is drawn now.
     *
     * It cannot be a synchronous request and answer: sending queues records on the link and the
     * host's answer arrives as its next `Commit`, so the answer to "did it work" is "the next
     * screen". The screen's own deadline is what stops a wait nobody will answer.
     */
    fun sync(): Boolean {
        val pending = input.hasPending
        // Nothing typed and no screen held: the view is already the newest the host sent, so there
        // is nothing to send and nothing to show. Starting a wait here would be a wait for an
        // answer that cannot come, and the deadline would report a failure that did not happen.
        if (!pending && pendingScreen == null) return false
        if (hold.sync(pending)) return true
        pullHeld()
        return false
    }

    /** The pump drained the buffer: stop sending and wait for the answer. */
    fun syncDrained() { hold.drained() }

    /** The wait's deadline passed with no answer: draw what is held and say so. */
    fun syncTimedOut(): Boolean {
        if (!hold.timedOut()) return false
        pullHeld()
        return true
    }

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
     * A grid that changes size (the host re-described its terminal) is a new engine: the old one's
     * cells are the wrong shape, and a selection into them would mean nothing.
     */
    fun show(screen: MeshScreen) {
        if (screen.cols <= 0 || screen.lines.isEmpty()) return
        // The hold decides whether this screen may be drawn. While frozen the newest screen is
        // kept rather than drawn, so a sync shows the shell as it is now and not a replay of what
        // was typed. Nothing else about the link changes: the driver keeps polling and the
        // session keeps its acks, because a frozen view is a view and not a dead link.
        if (!hold.acceptScreen(painted = last != null)) {
            pendingScreen = screen
            return
        }
        pendingScreen = null
        applyScreen(screen)
        hold.painted()
    }

    /**
     * Draw the newest screen the host sent, without sending anything.
     *
     * The honest answer to "sync" when nothing is waiting: there is nothing to send, so the only
     * thing a button can do is show what the shell last said. False when no screen is held.
     */
    fun pullHeld(): Boolean {
        val held = pendingScreen ?: return false
        pendingScreen = null
        applyScreen(held)
        hold.painted()
        return true
    }

    private fun applyScreen(screen: MeshScreen) {
        val c = screen.cols
        val r = screen.lines.size
        if (c <= 0 || r <= 0) return
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
        // grid) or a jump backwards is not a scroll, and adds nothing.
        val previous = last
        history.note(previous, screen)
        if (engine == null || c != cols || r != rows) {
            engine?.close()
            engine = newEngine(c, r)
            cols = c
            rows = r
            history.toBottom()
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
     *  - **Autowrap is off while the echo is written.** A long line that wrapped would scroll the
     *    screen, and a row pushed into the local scrollback by a fake is history that never
     *    happened. With DECAWM off the text stops at the last column instead.
     *  - **Nothing is echoed while the view is parked in history** ([MeshHistory.offset]). The
     *    user is reading rows above the live screen; typing into the middle of them would be a
     *    fake of the wrong screen.
     *
     * An empty [text] removes the echo and draws the host's screen again.
     */
    fun echoHeld(text: String) {
        val screen = last ?: return
        val e = engine ?: return
        if (history.offset != 0) return
        writeBase(screen)
        if (text.isNotEmpty()) {
            val payload = "\u001b[?7l" + text + "\u001b[?7h"
            val bytes = payload.encodeToByteArray()
            e.write(bytes, bytes.size)
            echoing = true
        } else {
            echoing = false
        }
        _frame.value = e.render()
    }

    /** Write the host's [screen] into the engine, replacing whatever is there. */
    private fun writeBase(screen: MeshScreen) {
        val e = engine ?: return
        val bytes = meshScreenBytes(screen, cols, rows)
        e.write(bytes, bytes.size)
    }

    /** Draw the live screen, or the history window at [offset] rows above it. */
    private fun paint() {
        echoing = false
        val screen = last ?: return
        val e = engine ?: return
        if (history.offset == 0) {
            val bytes = meshScreenBytes(screen, cols, rows)
            e.write(bytes, bytes.size)
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
        engine?.close()
        engine = null
        last = null
        cols = 0
        rows = 0
        history.clear()
        hasSelection = false
        _frame.value = null
        // A screen from the shell that is gone is not an answer to anything: drop it so a sync
        // does not draw the last shell's rows under the next shell's name.
        pendingScreen = null
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
            paint()
        } else if (rows < 0) {
            // The host sends no history and this client accumulates its own, so "up" can be a tap
            // with no answer: either nothing has scrolled off yet, or the view is already at the
            // oldest row. Two different sentences, because the second one means "this is all of
            // it" and the first means "nothing yet" — and a button that says nothing at all
            // reads as broken either way.
            scrollNotice = if (history.size == 0) "no history yet on this link" else "start of history"
        } else {
            scrollNotice = null
        }
        return moved
    }

    override fun scrollToBottom() {
        scrollNotice = null
        if (history.offset == 0) return
        history.toBottom()
        paint()
    }

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
