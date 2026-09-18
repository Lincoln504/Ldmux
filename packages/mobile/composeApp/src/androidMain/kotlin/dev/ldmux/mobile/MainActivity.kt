package dev.ldmux.mobile

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import dev.ldmux.mobile.voice.VoiceDictation

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AndroidApp.context = applicationContext
        LdmuxService.activity = this
        enableEdgeToEdge()
        setContent { App() }
    }

    override fun onStart() {
        super.onStart()
        AppLifecycle.set(true)
    }

    /**
     * The app left the screen.
     *
     * Deliberately `onStop` and not `onPause`: `onPause` fires for a dialog, a
     * permission prompt and the soft keyboard in some builds, and none of those
     * mean the user has gone anywhere.
     */
    override fun onStop() {
        AppLifecycle.set(false)
        super.onStop()
    }

    override fun onDestroy() {
        // Cleared rather than leaked: the service asks this for the notification
        // permission, and asking a destroyed activity is a crash, not a no-op.
        if (LdmuxService.activity === this) LdmuxService.activity = null
        super.onDestroy()
    }

    /**
     * Ask for the notification permission if it is both needed and not yet
     * granted.
     *
     * Android 13 is where it became a runtime permission. Below that it is not
     * asked for at all — a dialog for something already granted is noise, and the
     * whole point of asking is that this app shows a notification only while a
     * shell is open. Denial is not fatal: the service still runs, the
     * notification is simply not shown.
     */
    fun requestNotificationPermissionIfNeeded() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val granted = checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED
        if (granted) return
        runCatching { requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQ_NOTIFICATIONS) }
    }

    /**
     * The microphone permission is requested by [VoiceDictation] (the only thing
     * that needs it), but only an Activity can receive the answer, so it has to
     * be routed back by hand.
     */
    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == REQ_NOTIFICATIONS) return
        VoiceDictation.onRequestPermissionsResult(
            requestCode,
            grantResults.isNotEmpty() && grantResults[0] == android.content.pm.PackageManager.PERMISSION_GRANTED,
        )
    }

    companion object {
        private const val REQ_NOTIFICATIONS = 0x1d
    }
}