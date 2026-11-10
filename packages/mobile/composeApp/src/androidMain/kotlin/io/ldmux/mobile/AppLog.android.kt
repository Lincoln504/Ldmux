package io.ldmux.mobile

import android.util.Log

/**
 * `println` does not reach logcat on this app, and this is what does. See the
 * expect declaration for why it is a seam at all.
 *
 * The line is also recorded in [AppLogBuffer], which is what the settings screen's
 * diagnostics row shows. One call, one place to look: the buffer must not become a
 * second thing callers have to remember to write to, or half the app's story lives in
 * one and half in the other.
 */
actual fun appLog(message: String) {
    AppLogBuffer.record(message)
    // **On disk as well as in the buffer**, because a field failure can end without ever reaching a
    // verdict — see [AppLogFile]: the device sat on "securing the link", `failed` never ran, and the
    // one file that would have held the evidence was nine hours stale.
    AppLogFile.append(message)
    Log.d("ldmux", message)
}
