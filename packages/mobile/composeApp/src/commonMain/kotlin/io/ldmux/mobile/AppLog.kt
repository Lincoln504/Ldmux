package io.ldmux.mobile

import kotlin.time.TimeSource

/**
 * A line of diagnostic output, on the platform's own log.
 *
 * This exists because there was no way to ask this app a question. `println` does
 * not reach logcat, commonMain cannot see `android.util.Log`, and the only surface
 * that rendered anything was the error notice — which clears itself after four
 * seconds, so a screenshot at the wrong moment said "no message arrived" when a
 * message had arrived and gone. Diagnosing the empty launcher took far longer than
 * it should have for want of one function.
 *
 * Deliberately a seam and not a framework: no levels, no tags-by-convention, no
 * configuration. A thing worth saying goes to the platform log under one tag.
 *
 * **The same call also records the line here**, because a support question ("the
 * shell list was empty") cannot be answered from a log that needs a cable and a
 * terminal. One buffer, no second logging convention.
 *
 * **Nothing is kept on disk in normal running** — a log that outlives the process is a
 * privacy decision this app does not make. The one exception is a *link failure*: when the
 * connection drops, the buffer is written once to an app-private file and overwritten on the
 * next drop. A failure at range happens with the phone in a pocket, so the process may be gone
 * by the time anyone can read a cable-attached log; the last failure is otherwise unknowable,
 * which is exactly the question a field test asks. The buffer holds protocol and state lines,
 * never terminal content or anything typed.
 */
expect fun appLog(message: String)

/**
 * Write the diagnostics buffer where it survives this process, once, on a link failure.
 *
 * App-private (`filesDir`, `MODE_PRIVATE`), overwritten each time, and never exported: the
 * answer to "why did it drop at range" without a cable, and nothing else.
 */
expect fun appLogDump(lines: List<String>)

/**
 * The last lines this app said, newest last.
 *
 * Bounded and in memory. [CAPACITY] is small enough that the whole buffer can go on the
 * clipboard and large enough for "what happened in the last minute"; when it is full the
 * oldest line goes, so the cost of a chatty caller is a shorter history and never growth.
 *
 * Thread-safe because callers are wherever the work happened (a socket coroutine, a BLE
 * callback, the main thread) and the diagnostics row reads from the main thread.
 */
object AppLogBuffer {
    const val CAPACITY = 300

    private val lines = ArrayDeque<String>()
    private val start = TimeSource.Monotonic.markNow()

    fun record(message: String) {
        // Seconds since this process started rather than a wall clock: correlating two
        // devices is not the job here, and "it happened 3 s after I opened the shell" is the
        // question a reader of this list actually has. Fixed width so the first character of
        // every message lines up.
        val stamp = start.elapsedNow().inWholeMilliseconds / 1000.0
        synchronized(lines) {
            lines.addLast("%6.1fs  %s".format(stamp, message))
            while (lines.size > CAPACITY) lines.removeFirst()
        }
    }

    /** What is held, oldest first. A copy: the caller cannot mutate the buffer. */
    fun snapshot(): List<String> = synchronized(lines) { lines.toList() }
}

/**
 * The last [max] lines, with one line saying how many were left out.
 *
 * A silent cut is the failure mode this avoids: a list that starts mid-sentence looks like
 * the whole history, and the first thing a reader does is trust it.
 */
internal fun logTail(all: List<String>, max: Int): List<String> {
    if (max <= 0) return emptyList()
    if (all.size <= max) return all
    val omitted = all.size - max
    return listOf("\u2026 $omitted earlier " + if (omitted == 1) "line" else "lines") + all.takeLast(max)
}
