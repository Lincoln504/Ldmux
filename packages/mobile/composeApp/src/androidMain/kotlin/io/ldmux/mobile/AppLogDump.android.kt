package io.ldmux.mobile

/**
 * The last failure's diagnostics, in the app's own private directory.
 *
 * Written once per drop and overwritten on the next, so it can never grow. `filesDir` is
 * unreadable by other apps, is excluded from backup (see the manifest rules), and is readable
 * from a debugging bench with `adb shell run-as io.ldmux.mobile cat files/mesh-last-failure.log`
 * — which is the whole point: the failure happens at range with the phone in a pocket, and the
 * process that saw it may be gone by the time anyone can ask.
 */
actual fun appLogDump(lines: List<String>) {
    val context = runCatching { AndroidApp.context }.getOrNull() ?: return
    runCatching {
        context.openFileOutput("mesh-last-failure.log", android.content.Context.MODE_PRIVATE).use { out ->
            // **The rolling log first**, because it is the one record with wall-clock stamps and it
            // survives a run that never reached a verdict. The in-memory buffer follows: it holds
            // the lines emitted before the file sink existed and the ones the sink could not write.
            val tail = AppLogFile.tail(600)
            if (tail.isNotEmpty()) {
                out.write(tail.toByteArray())
                out.write("\n".toByteArray())
            }
            out.write("--- this process's buffer ---\n".toByteArray())
            out.write(lines.joinToString("\n").toByteArray())
            out.write("\n".toByteArray())
        }
    }
}
