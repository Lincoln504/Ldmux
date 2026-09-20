package dev.ldmux.mobile

import android.util.Log

/**
 * `println` does not reach logcat on this app, and this is what does. See the
 * expect declaration for why it is a seam at all.
 */
actual fun appLog(message: String) {
    Log.d("ldmux", message)
}
