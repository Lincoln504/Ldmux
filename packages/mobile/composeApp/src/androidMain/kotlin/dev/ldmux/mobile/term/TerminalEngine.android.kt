package dev.ldmux.mobile.term

/** Android actual: a thin wrapper over the JNI shim. */
actual class TerminalEngine actual constructor(cols: Int, rows: Int) {
    private var handle: Long = if (NativeTerminal.available) NativeTerminal.create(cols, rows) else 0L

    actual fun write(bytes: ByteArray, length: Int) {
        if (handle != 0L && length > 0) NativeTerminal.write(handle, bytes, length)
    }

    actual fun resize(cols: Int, rows: Int) {
        if (handle != 0L) NativeTerminal.resize(handle, cols, rows)
    }

    actual fun render(): TerminalFrame.Frame? {
        if (handle == 0L) return null
        val bytes = NativeTerminal.render(handle) ?: return null
        return TerminalFrame.decode(bytes)
    }

    actual fun close() {
        if (handle != 0L) {
            NativeTerminal.destroy(handle)
            handle = 0L
        }
    }
}