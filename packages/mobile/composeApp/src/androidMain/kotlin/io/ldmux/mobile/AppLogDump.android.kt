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
            out.write(lines.joinToString("\n").toByteArray())
            out.write("\n".toByteArray())
        }
    }
}
