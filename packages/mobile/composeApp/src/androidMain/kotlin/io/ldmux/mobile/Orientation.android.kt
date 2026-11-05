package io.ldmux.mobile

import android.app.Activity
import android.content.pm.ActivityInfo
import java.lang.ref.WeakReference

actual object ScreenOrientation {
    private var host: WeakReference<Activity>? = null

    /** Set by MainActivity, like the scanner's and dictation's hosts. */
    fun attachHost(activity: Activity) {
        host = WeakReference(activity)
    }

    actual fun apply(policy: OrientationPolicy) {
        // `UNSPECIFIED` is the manifest's own (absent) setting: the device, and the
        // user's auto-rotate switch, decide. `configChanges` includes orientation, so
        // neither change recreates the activity.
        host?.get()?.requestedOrientation = when (policy) {
            OrientationPolicy.Portrait -> ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
            OrientationPolicy.FollowDevice -> ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        }
    }
}
