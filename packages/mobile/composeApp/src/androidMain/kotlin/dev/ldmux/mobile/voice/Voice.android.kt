package dev.ldmux.mobile.voice

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import java.lang.ref.WeakReference

/**
 * Android's dictation: [SpeechRecognizer], headless.
 *
 * On-device recognition is preferred, because it keeps shell commands off the network
 * and removes the round trip: the dedicated on-device recogniser where the platform
 * has one (Android 12+), and `EXTRA_PREFER_OFFLINE` on every request otherwise. When
 * only a cloud recogniser exists that is what we get — it still needs no keyboard —
 * and the user is **told** ([VoiceState.ListeningCloud]) rather than left to assume.
 * The transcript goes to the sink the caller supplied.
 *
 * The activity this needs is found here rather than passed in, which is the point
 * of the seam: `SpeechRecognizer` wants a Context and a permission request wants an
 * Activity, and neither is part of what dictation *is*. iOS supplies
 * `SFSpeechRecognizer` and has nothing to pass. Held weakly, because this object
 * outlives any activity that is recreated and a recogniser holding one is a leak.
 */
actual object Dictation {
    private const val REQUEST_CODE = 0x7D

    private var host: WeakReference<Activity>? = null
    private var recognizer: SpeechRecognizer? = null
    private var onText: ((String) -> Unit)? = null
    private var onState: ((VoiceState) -> Unit)? = null

    /** The recogniser in use is not the on-device one, so audio may leave the phone. */
    private var cloud = false

    actual val listening: Boolean get() = recognizer != null

    /** The host activity, for the sibling [SpeechOutput] that needs a Context too. */
    internal fun hostActivity(): Activity? = host?.get()

    /** Called once, by the activity, so this side can reach a Context of its own. */
    fun attachHost(activity: Activity) {
        host = WeakReference(activity)
    }

    /**
     * Start (or stop, if already listening) one dictation. Must be called from the
     * main thread: `SpeechRecognizer` requires it.
     */
    actual fun toggle(onText: (String) -> Unit, onState: (VoiceState) -> Unit) {
        this.onText = onText
        this.onState = onState
        if (recognizer != null) {
            stop()
            return
        }
        val act = host?.get()
        if (act == null) {
            onState(VoiceState.Unavailable)
            return
        }
        if (act.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            onState(VoiceState.RequestingPermission)
            act.requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), REQUEST_CODE)
            return
        }
        begin(act)
    }

    /** Forward the runtime permission result from the host activity. */
    fun onRequestPermissionsResult(requestCode: Int, granted: Boolean) {
        if (requestCode != REQUEST_CODE) return
        val act = host?.get()
        if (!granted || act == null) {
            onState?.invoke(VoiceState.Denied)
            return
        }
        begin(act)
    }

    actual fun stop() {
        recognizer?.let { sr ->
            runCatching { sr.stopListening() }
            runCatching { sr.cancel() }
            runCatching { sr.destroy() }
        }
        recognizer = null
        onState?.invoke(VoiceState.Idle)
    }

    private fun begin(ctx: Context) {
        if (!SpeechRecognizer.isRecognitionAvailable(ctx)) {
            onState?.invoke(VoiceState.Unavailable)
            return
        }
        val sr = create(ctx)
        if (sr == null) {
            onState?.invoke(VoiceState.Unavailable)
            return
        }
        recognizer = sr
        sr.setRecognitionListener(listener)
        onState?.invoke(if (cloud) VoiceState.ListeningCloud else VoiceState.Listening)
        sr.startListening(
            Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)
                putExtra(RecognizerIntent.EXTRA_CALLING_PACKAGE, ctx.packageName)
                // A request to keep it on the device; honoured by recognisers that
                // support it, and a no-op for the rest (hence the notice above).
                putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
            },
        )
    }

    private fun create(ctx: Context): SpeechRecognizer? = runCatching {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && SpeechRecognizer.isOnDeviceRecognitionAvailable(ctx)) {
            cloud = false
            SpeechRecognizer.createOnDeviceSpeechRecognizer(ctx)
        } else {
            cloud = true
            SpeechRecognizer.createSpeechRecognizer(ctx)
        }
    }.getOrNull()

    private fun finish() {
        val sr = recognizer
        recognizer = null
        sr?.let {
            runCatching { it.destroy() }
        }
    }

    /**
     * Google's recognizer ends a spoken command with a full stop, which is
     * punctuation the shell does not want. Trim it (and only it) so "list files"
     * becomes `ls`-able text rather than `ls .`.
     */
    private fun normalize(text: String): String = text.trim().removeSuffix(".").trim()

    private val listener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {}
        override fun onBeginningOfSpeech() {}
        override fun onRmsChanged(rmsdB: Float) {}
        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}
        override fun onEvent(eventType: Int, params: Bundle?) {}

        override fun onPartialResults(partialResults: Bundle?) {
            val text = partialResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()
            if (!text.isNullOrBlank()) onState?.invoke(VoiceState.Partial(text, cloud))
        }

        override fun onResults(results: Bundle?) {
            val text = results?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()
            if (!text.isNullOrBlank()) onText?.invoke(normalize(text))
            onState?.invoke(VoiceState.Idle)
            finish()
        }

        override fun onError(error: Int) {
            onState?.invoke(VoiceState.Failed(errorName(error)))
            finish()
        }
    }

    private fun errorName(error: Int): String = when (error) {
        SpeechRecognizer.ERROR_AUDIO -> "audio"
        SpeechRecognizer.ERROR_CLIENT -> "client"
        SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS -> "permission"
        SpeechRecognizer.ERROR_NETWORK -> "network"
        SpeechRecognizer.ERROR_NETWORK_TIMEOUT -> "network timeout"
        SpeechRecognizer.ERROR_NO_MATCH -> "no speech"
        SpeechRecognizer.ERROR_RECOGNIZER_BUSY -> "busy"
        SpeechRecognizer.ERROR_SERVER -> "server"
        SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> "timed out"
        else -> "error $error"
    }
}
