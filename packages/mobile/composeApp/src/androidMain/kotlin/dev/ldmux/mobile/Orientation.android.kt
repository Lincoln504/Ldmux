package dev.ldmux.mobile

import android.content.pm.ActivityInfo

/**
 * The screen's orientation, as an activity request.
 *
 * Written through [LdmuxService.activity] rather than a stored reference of its
 * own, because that is already the app's one live-activity handle and it is
 * cleared on destroy — a second one would be a second thing to leak. A null
 * activity is an ordinary no-op: a lock requested after teardown has nothing to
 * apply to.
 *
 * Guarded against writing the same value twice: `requestedOrientation` is a
 * binder call, and the caller is a `LaunchedEffect` inside composition.
 */
actual fun lockOrientation(portrait: Boolean) {
    val activity = LdmuxService.activity ?: return
    val want = if (portrait) {
        ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
    } else {
        ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
    }
    if (activity.requestedOrientation != want) activity.requestedOrientation = want
}
