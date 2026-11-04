package dev.ldmux.mobile

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import dev.ldmux.mobile.mesh.RadioScanner
import dev.ldmux.mobile.voice.Dictation

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AndroidApp.context = applicationContext
        LdmuxService.activity = this
        // Dictation finds its own Activity rather than taking one through the
        // shared seam, so this is where it is told which activity that is.
        Dictation.attachHost(this)
        CodeScanner.attachHost(this)
        RadioScanner.attachHost(this)
        ScreenOrientation.attachHost(this)
        // **The system bars are told which palette this app is, not asked to guess.**
        // `enableEdgeToEdge()` with no arguments picks light or dark bar *icons* from the phone's
        // own light/dark setting — and this app is dark-only. On a phone set to light mode the
        // clock, the battery and the gesture pill were drawn dark on a near-black app: invisible.
        // `SystemBarStyle.dark` keeps them light whatever the system theme says.
        enableEdgeToEdge(
            statusBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
            navigationBarStyle = SystemBarStyle.dark(android.graphics.Color.TRANSPARENT),
        )
        // A cold start from a pairing link: held in the inbox until the UI reads it.
        // Not on a restore — the link was already handled by the instance that saw it.
        if (savedInstanceState == null) offerPairingLink(intent)
        setContent { App() }
    }

    /** A pairing link while the app is already running (singleTask). */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        offerPairingLink(intent)
    }

    private fun offerPairingLink(intent: Intent?) {
        if (intent?.action != Intent.ACTION_VIEW) return
        val link = intent.dataString ?: return
        PairingInbox.offer(link)
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
        RadioScanner.detachHost()
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
     * The microphone permission is requested by [Dictation] (the only thing that
     * needs it), but only an Activity can receive the answer, so it has to be
     * routed back by hand.
     */
    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        RadioScanner.onRequestPermissionsResult(requestCode, grantResults)
        if (requestCode == REQ_NOTIFICATIONS) return
        Dictation.onRequestPermissionsResult(
            requestCode,
            grantResults.isNotEmpty() && grantResults[0] == android.content.pm.PackageManager.PERMISSION_GRANTED,
        )
    }

    companion object {
        private const val REQ_NOTIFICATIONS = 0x1d
    }
}