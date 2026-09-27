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
     * Scroll the viewport by [delta] rows (negative is up), or to an end with
     * [SCROLL_TOP] / [SCROLL_BOTTOM].
     *
     * The special values are sentinels rather than an enum so the seam stays a
     * single integer; they are far outside any real row count.
     */
    external fun scroll(handle: Long, delta: Int)

    const val SCROLL_TOP: Int = Int.MIN_VALUE
    const val SCROLL_BOTTOM: Int = Int.MAX_VALUE

    /**
     * Encode the current screen as a frame (see native/ldmux_terminal.c for the
     * wire format) or null if the core had nothing to render.
     */
    external fun render(handle: Long): ByteArray?

    /**
     * Take the clipboard write the remote program last asked for, as one array:
     * the kind in byte 0 ([CLIPBOARD_SET] or [CLIPBOARD_CLEAR]) followed by the
     * UTF-8 payload. Null means the program asked for nothing.
     *
     * One call rather than a kind call and then a bytes call, because the value
     * is consumed by reading it: two reads of a consumed value is exactly how one
     * of them gets lost, which is a bug this project has already paid for.
     *
     * The payload is bytes and not a String on purpose. A JNI string built from
     * UTF-8 has to be *modified* UTF-8, which differs for anything outside the
     * BMP — an emoji in a copied path would come back as mojibake — whereas
     * Kotlin's own decoder handles plain UTF-8 correctly and lives on the side of
     * the seam that speaks Kotlin.
     */
    external fun takeClipboard(handle: Long): ByteArray?

    /**
     * How the grid is drawn, in surface pixels, for the selection gesture only.
     *
     * The engine needs this to reason about pointer distance: the slop that makes
     * two taps one word, and where a word boundary sits under the finger. It is
     * geometry rather than state, so it is set when the font size or the rotation
     * changes, not per event.
     */
    external fun setGeometry(
        handle: Long,
        columns: Int,
        cellW: Int,
        cellH: Int,
        padLeft: Int,
        screenH: Int,
    )

    /** Pointer down on a viewport cell. Returns whether the selection changed. */
    external fun selectionPress(handle: Long, col: Int, row: Int, timeMs: Long): Boolean

    /** Long press: the word under the finger. */
    external fun selectionDeepPress(handle: Long, timeMs: Long): Boolean

    external fun selectionDrag(handle: Long, col: Int, row: Int, timeMs: Long): Boolean

    /** `valid` is false when the pointer left the grid entirely. */
    external fun selectionRelease(
        handle: Long,
        col: Int,
        row: Int,
        valid: Boolean,
        timeMs: Long,
    ): Boolean

    /**
     * Set the selection to an explicit inclusive range of viewport cells.
     *
     * The handle path, as opposed to the gesture path: two ends that are dragged
     * independently, and a range that may be inverted by dragging one past the
     * other.
     */
    external fun selectionSet(
        handle: Long,
        startCol: Int,
        startRow: Int,
        endCol: Int,
        endRow: Int,
    ): Boolean

    /** {startCol, startRow, endCol, endRow}, or null when nothing is selected. */
    external fun selectionBounds(handle: Long): IntArray?

    external fun selectionClear(handle: Long): Boolean

    external fun selectionSelectAll(handle: Long): Boolean

    /** Selected text as UTF-8 bytes, or null when nothing is selected. */
    external fun selectionText(handle: Long): ByteArray?

    /** Kinds in byte 0 of a [takeClipboard] answer. */
    const val CLIPBOARD_NONE: Int = 0
    const val CLIPBOARD_SET: Int = 1
    const val CLIPBOARD_CLEAR: Int = 2
}