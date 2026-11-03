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
        val c = screen.cols
        val r = screen.lines.size
        if (c <= 0 || r <= 0) return
        if (screen == last && engine != null) return
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

    /** Draw the live screen, or the history window at [offset] rows above it. */
    private fun paint() {
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
        if (moved != 0) paint()
        return moved
    }

    override fun scrollToBottom() {
        if (history.offset == 0) return
        history.toBottom()
        paint()
    }

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
