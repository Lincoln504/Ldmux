package dev.ldmux.mobile.term

import dev.ldmux.mobile.ClipboardRequest

/**
 * A terminal emulator instance, backed by libghostty-vt on each platform.
 * Created at a fixed grid size; feed it bytes, read back a decoded frame.
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

    fun close()
}