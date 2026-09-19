package dev.ldmux.mobile.term

import dev.ldmux.mobile.ClipboardRequest

/** Android actual: a thin wrapper over the JNI shim. */
actual class TerminalEngine actual constructor(cols: Int, rows: Int) {
    private var handle: Long = if (NativeTerminal.available) NativeTerminal.create(cols, rows) else 0L

    actual fun write(bytes: ByteArray, length: Int) {
        if (handle != 0L && length > 0) NativeTerminal.write(handle, bytes, length)
    }

    actual fun resize(cols: Int, rows: Int) {
        if (handle != 0L) NativeTerminal.resize(handle, cols, rows)
    }

    actual fun scrollBy(rows: Int) {
        if (handle != 0L && rows != 0) NativeTerminal.scroll(handle, rows)
    }

    actual fun scrollToTop() {
        if (handle != 0L) NativeTerminal.scroll(handle, NativeTerminal.SCROLL_TOP)
    }

    actual fun scrollToBottom() {
        if (handle != 0L) NativeTerminal.scroll(handle, NativeTerminal.SCROLL_BOTTOM)
    }

    actual fun render(): TerminalFrame.Frame? {
        if (handle == 0L) return null
        val bytes = NativeTerminal.render(handle) ?: return null
        return TerminalFrame.decode(bytes)
    }

    actual fun takeClipboardRequest(): ClipboardRequest? {
        if (handle == 0L) return null
        val answer = NativeTerminal.takeClipboard(handle) ?: return null
        if (answer.isEmpty()) return null
        return when (answer[0].toInt()) {
            NativeTerminal.CLIPBOARD_CLEAR -> ClipboardRequest.Clear
            NativeTerminal.CLIPBOARD_SET -> ClipboardRequest.Set(
                // Plain UTF-8 from the first byte after the kind. Decoding it here
                // rather than in C is what keeps an emoji in a copied path intact.
                answer.decodeToString(1, answer.size),
            )
            else -> null
        }
    }

    actual fun close() {
        if (handle != 0L) {
            NativeTerminal.destroy(handle)
            handle = 0L
        }
    }
}