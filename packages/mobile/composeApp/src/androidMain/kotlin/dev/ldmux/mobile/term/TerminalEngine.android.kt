package dev.ldmux.mobile.term

import dev.ldmux.mobile.ClipboardRequest

/** Android actual: a thin wrapper over the JNI shim. */
/**
 * Android actual: a thin wrapper over the JNI shim.
 *
 * Thread-safety: the native core is single-threaded by contract, and every call
 * here goes through a [GuardedHandle], so [close] may be called from any thread,
 * more than once, and concurrently with other calls; whatever arrives after it is
 * a no-op that returns the "nothing" answer.
 */
actual class TerminalEngine actual constructor(cols: Int, rows: Int) {
    private val native = GuardedHandle(
        create = { if (NativeTerminal.available) NativeTerminal.create(cols, rows) else 0L },
        destroy = { NativeTerminal.destroy(it) },
    )

    actual fun write(bytes: ByteArray, length: Int) {
        // Never trust the caller's length past the array: the JNI side clamps too.
        val n = minOf(length, bytes.size)
        if (n > 0) native.use(Unit) { NativeTerminal.write(it, bytes, n) }
    }

    actual fun resize(cols: Int, rows: Int) {
        native.use(Unit) { NativeTerminal.resize(it, cols, rows) }
    }

    actual fun scrollBy(rows: Int) {
        if (rows != 0) native.use(Unit) { NativeTerminal.scroll(it, rows) }
    }

    actual fun scrollToTop() {
        native.use(Unit) { NativeTerminal.scroll(it, NativeTerminal.SCROLL_TOP) }
    }

    actual fun scrollToBottom() {
        native.use(Unit) { NativeTerminal.scroll(it, NativeTerminal.SCROLL_BOTTOM) }
    }

    actual fun render(): TerminalFrame.Frame? {
        val bytes = native.use<ByteArray?>(null) { NativeTerminal.render(it) } ?: return null
        return TerminalFrame.decode(bytes)
    }

    actual fun takeClipboardRequest(): ClipboardRequest? {
        val answer = native.use<ByteArray?>(null) { NativeTerminal.takeClipboard(it) } ?: return null
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
        native.use(Unit) { NativeTerminal.setGeometry(it, columns, cellW, cellH, padLeft, screenH) }
    }

    actual fun selectionSet(
        startCol: Int,
        startRow: Int,
        endCol: Int,
        endRow: Int,
    ): Boolean =
        native.use(false) {
            NativeTerminal.selectionSet(it, startCol, startRow, endCol, endRow)
        }

    actual fun selectionBounds(): IntArray? =
        native.use<IntArray?>(null) { NativeTerminal.selectionBounds(it) }

    actual fun selectionClear(): Boolean = native.use(false) { NativeTerminal.selectionClear(it) }

    actual fun selectionSelectAll(): Boolean =
        native.use(false) { NativeTerminal.selectionSelectAll(it) }

    actual fun selectionText(): String? {
        // Plain UTF-8 again, for the same reason as the clipboard: a JNI string
        // would have to be modified UTF-8 and would mangle anything off the BMP.
        return native.use<ByteArray?>(null) { NativeTerminal.selectionText(it) }?.decodeToString()
    }

    /** Free the native terminal. Idempotent and safe from any thread. */
    actual fun close() {
        native.close()
    }
}
