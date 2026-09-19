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

    actual fun setGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int) {
        if (handle != 0L) NativeTerminal.setGeometry(handle, columns, cellW, cellH, padLeft, screenH)
    }

    actual fun selectionPress(col: Int, row: Int, timeMs: Long): Boolean =
        handle != 0L && NativeTerminal.selectionPress(handle, col, row, timeMs)

    actual fun selectionDeepPress(timeMs: Long): Boolean =
        handle != 0L && NativeTerminal.selectionDeepPress(handle, timeMs)

    actual fun selectionDrag(col: Int, row: Int, timeMs: Long): Boolean =
        handle != 0L && NativeTerminal.selectionDrag(handle, col, row, timeMs)

    actual fun selectionRelease(col: Int, row: Int, valid: Boolean, timeMs: Long): Boolean =
        handle != 0L && NativeTerminal.selectionRelease(handle, col, row, valid, timeMs)

    actual fun selectionSet(
        startCol: Int,
        startRow: Int,
        endCol: Int,
        endRow: Int,
    ): Boolean =
        handle != 0L &&
            NativeTerminal.selectionSet(handle, startCol, startRow, endCol, endRow)

    actual fun selectionBounds(): IntArray? =
        if (handle == 0L) null else NativeTerminal.selectionBounds(handle)

    actual fun selectionClear(): Boolean = handle != 0L && NativeTerminal.selectionClear(handle)

    actual fun selectionSelectAll(): Boolean =
        handle != 0L && NativeTerminal.selectionSelectAll(handle)

    actual fun selectionText(): String? {
        if (handle == 0L) return null
        // Plain UTF-8 again, for the same reason as the clipboard: a JNI string
        // would have to be modified UTF-8 and would mangle anything off the BMP.
        return NativeTerminal.selectionText(handle)?.decodeToString()
    }

    actual fun close() {
        if (handle != 0L) {
            NativeTerminal.destroy(handle)
            handle = 0L
        }
    }
}