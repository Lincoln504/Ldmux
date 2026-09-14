package dev.ldmux.mobile.term

/**
 * The Kotlin side of the libghostty-vt seam. Loads the JNI shim built by
 * scripts/build-libghostty-vt.sh (libldmux_terminal.so) and exposes the
 * terminal core: create a terminal, feed it the remote PTY's bytes, resize it,
 * and read back an encoded frame of styled cells.
 *
 * All native state lives behind an opaque handle; call [destroy] exactly once.
 */
object NativeTerminal {
    /** True if the native library loaded. Never throws. */
    val available: Boolean = try {
        System.loadLibrary("ldmux_terminal")
        true
    } catch (t: Throwable) {
        false
    }

    /** Create a terminal of [cols] x [rows]. Returns 0 on failure. */
    external fun create(cols: Int, rows: Int): Long

    external fun destroy(handle: Long)

    /** Feed terminal output bytes (what the remote pane produced). */
    external fun write(handle: Long, data: ByteArray, len: Int)

    external fun resize(handle: Long, cols: Int, rows: Int)

    /**
     * Encode the current screen as a frame (see native/ldmux_terminal.c for the
     * wire format) or null if the core had nothing to render.
     */
    external fun render(handle: Long): ByteArray?

    /** Acknowledge a rendered frame so the core can reset its dirty state. */
    external fun clean(handle: Long)
}