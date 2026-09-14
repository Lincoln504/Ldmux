package dev.ldmux.mobile

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import dev.ldmux.mobile.voice.VoiceDictation

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        AndroidApp.context = applicationContext
        enableEdgeToEdge()
        setContent { App() }
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
        VoiceDictation.onRequestPermissionsResult(
            requestCode,
            grantResults.isNotEmpty() && grantResults[0] == android.content.pm.PackageManager.PERMISSION_GRANTED,
        )
    }
}