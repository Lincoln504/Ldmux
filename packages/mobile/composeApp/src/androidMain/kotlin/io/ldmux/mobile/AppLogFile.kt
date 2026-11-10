package io.ldmux.mobile

import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * The rolling log on disk, so a failure that never reaches a verdict can still be read.
 *
 * **Why this exists, precisely.** The app used to keep nothing on disk (`AppLog`'s own contract:
 * a log that outlives the process is a privacy decision this app does not make, with one exception
 * — a link failure was written once). On 2026-11-10 that exception failed the only test that
 * matters: the phone went out of range, the screen said "securing the link" and sat there, the
 * session never reached [MeshTermDriver.failed], and therefore **nothing was ever written**. The
 * failure dump from 01:53 was nine hours old when it was read, and logcat had rolled over because
 * another app on this phone writes megabytes a minute. The whole field test left no evidence.
 *
 * **What it costs, said plainly.** Protocol and state lines only — the same lines `appLog` already
 * emits, never terminal content, never anything typed, never a key. App-private (`filesDir`,
 * `MODE_PRIVATE`), excluded from backup by the same manifest rules, and bounded twice: by
 * [MAX_BYTES] on disk and by [KEEP_BYTES] when it is trimmed, so it cannot grow without end and
 * cannot fill the device.
 *
 * Every line carries a wall-clock stamp, because the question a reader has is "when", and a
 * monotonic counter answers a different question than the one an operator asks.
 */
internal object AppLogFile {
    private const val NAME = "mesh-log.txt"
    private const val MAX_BYTES = 256 * 1024
    private const val KEEP_BYTES = 128 * 1024
    private val lock = Any()
    private val stamp = SimpleDateFormat("MM-dd HH:mm:ss.SSS", Locale.US)

    private val file: File?
        get() = runCatching { AndroidApp.context?.filesDir?.resolve(NAME) }.getOrNull()

    /** Append one line, stamped. Never throws: a diagnostic must not be the reason something fails. */
    fun append(message: String) {
        val f = file ?: return
        val line = "${stamp.format(Date())}  $message\n"
        synchronized(lock) {
            runCatching {
                f.appendText(line)
                if (f.length() > MAX_BYTES) trim(f)
            }
        }
    }

    /** The last [maxLines] lines, newest last. */
    fun tail(maxLines: Int): String {
        val f = file ?: return ""
        return synchronized(lock) {
            runCatching {
                val lines = f.readLines()
                lines.takeLast(maxLines).joinToString("\n")
            }.getOrDefault("")
        }
    }

    /**
     * Keep the newest [KEEP_BYTES] of the file.
     *
     * A whole-file rewrite rather than a rotation: one file to find, one file to delete, and no
     * numbering for the operator to work out. Trimming at a line boundary keeps the oldest kept
     * line readable.
     */
    private fun trim(f: File) {
        val bytes = f.readBytes()
        val from = (bytes.size - KEEP_BYTES).coerceAtLeast(0)
        var start = from
        while (start < bytes.size && bytes[start] != '\n'.code.toByte()) start++
        f.writeBytes(bytes.copyOfRange((start + 1).coerceAtMost(bytes.size), bytes.size))
    }
}
