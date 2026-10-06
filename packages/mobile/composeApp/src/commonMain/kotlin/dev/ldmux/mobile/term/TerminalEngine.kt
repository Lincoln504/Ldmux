package dev.ldmux.mobile.term

import dev.ldmux.mobile.ClipboardRequest

/**
 * A terminal emulator instance, backed by libghostty-vt on each platform.
 * Created at a fixed grid size; feed it bytes, read back a decoded frame.
 *
 * Threading: the native core behind this is single-threaded (no locks). Drive an
 * engine from one thread at a time. The one cross-thread operation that is
 * supported is [close]: it is idempotent, may race with other calls, and every call
 * after it is a no-op that returns its "nothing" answer (null, false, or unit).
 */
expect class TerminalEngine(cols: Int, rows: Int) {
    fun write(bytes: ByteArray, length: Int = bytes.size)
    fun resize(cols: Int, rows: Int)

    /** Scroll the viewport by [rows] (negative is up). */
    fun scrollBy(rows: Int)

    /** Scroll to the oldest scrollback, and to the live prompt. */
    fun scrollToTop()
    fun scrollToBottom()

    /** Snapshot the screen, or null if nothing is renderable yet. */
    fun render(): TerminalFrame.Frame?

    /**
     * The clipboard write a program in the shell last asked for, if any, and
     * consumed by asking.
     *
     * Called immediately after [write], because the request arrives inside the
     * same bytes. Holding it anywhere else would be a second place keeping the
     * same piece of state, and this project has already paid once for a value
     * written in more than one place before anyone read it.
     */
    fun takeClipboardRequest(): ClipboardRequest?

    /**
     * How the grid is drawn, in surface pixels. Only the selection gesture uses
     * this; it is what makes "the same word" and "the same cell" answerable.
     */
    fun setGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int)

    /** Pointer down on a viewport cell. Answers whether the selection changed. */
    fun selectionPress(col: Int, row: Int, timeMs: Long): Boolean

    /** Long press, which the engine turns into "the word under the finger". */
    fun selectionDeepPress(timeMs: Long): Boolean

    fun selectionDrag(col: Int, row: Int, timeMs: Long): Boolean

    fun selectionRelease(col: Int, row: Int, valid: Boolean, timeMs: Long): Boolean

    /**
     * Set the selection to an explicit inclusive range of viewport cells, for the
     * handles the user drags rather than for a finger.
     */
    fun selectionSet(startCol: Int, startRow: Int, endCol: Int, endRow: Int): Boolean

    /** The selection's ends, or null when nothing is selected. */
    fun selectionBounds(): IntArray?

    fun selectionClear(): Boolean

    fun selectionSelectAll(): Boolean

    /** The selected text, or null when there is nothing selected. */
    fun selectionText(): String?

    /** Release the native terminal. Idempotent; calls after it are no-ops. */
    fun close()
}