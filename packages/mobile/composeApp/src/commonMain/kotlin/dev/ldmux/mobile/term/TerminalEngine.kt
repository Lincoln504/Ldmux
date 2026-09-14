package dev.ldmux.mobile.term

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

    fun close()
}