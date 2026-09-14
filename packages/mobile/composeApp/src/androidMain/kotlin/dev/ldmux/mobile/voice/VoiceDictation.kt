package dev.ldmux.mobile.voice

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.os.Bundle

/** What the voice button is doing, for the little on-screen indicator. */
sealed interface VoiceState {
    data object Idle : VoiceState
    data object RequestingPermission : VoiceState
    data object Listening : VoiceState
    data class Partial(val text: String) : VoiceState
    data object Denied : VoiceState
    data object Unavailable : VoiceState
    data class Failed(val reason: String) : VoiceState
}

/**
 * Headless dictation, so voice can be a first-class input without the on-screen
 * keyboard ever appearing.
 *
 * This deliberately does **not** go through the IME's voice-typing (the mic on
 * the keyboard): that path only exists while a soft keyboard is up and an
 * InputConnection is live, which is exactly what a terminal wants to avoid. The
 * platform's own [SpeechRecognizer] is a headless service — no keyboard, no
 * overlay — and results arrive through [RecognitionListener] callbacks we can
 * forward straight into the pane.
 *
 * On-device recognition is preferred where the platform offers it (Android 12+),
 * because it keeps shell commands off the network and removes the round trip.
 * When only cloud recognition is available that is what we get; it still needs
 * no keyboard. The caller supplies the transcript sink.
 */
object VoiceDictation {
    private const val REQUEST_CODE = 0x7D

    private var activity: Activity? = null
    private var recognizer: SpeechRecognizer? = null
    private var onText: ((String) -> Unit)? = null
    private var onState: ((VoiceState) -> Unit)? = null

    /** True while a recognition session is running (the button toggles it). */
    val listening: Boolean get() = recognizer != null

    /**
     * Start (or stop, if already listening) one dictation. Must be called from
     * the main thread: SpeechRecognizer requires it.
     */
    fun toggle(activity: Activity, onText: (String) -> Unit, onState: (VoiceState) -> Unit) {
        this.activity = activity
        this.onText = onText
        this.onState = onState
        if (recognizer != null) {
            stop()
            return
        }
        if (activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            onState(VoiceState.RequestingPermission)
            activity.requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), REQUEST_CODE)
            return
        }
        begin(activity)
    }

    /** Forward the runtime permission result from the host activity. */
    fun onRequestPermissionsResult(requestCode: Int, granted: Boolean) {
        if (requestCode != REQUEST_CODE) return
        val act = activity
        if (!granted || act == null) {
            onState?.invoke(VoiceState.Denied)
            return
        }
        begin(act)
    }

    fun stop() {
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
        onState?.invoke(VoiceState.Listening)
        sr.startListening(
            Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
                putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
                putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
                putExtra(RecognizerIntent.EXTRA_MAX_RESULTS, 1)
                putExtra(RecognizerIntent.EXTRA_CALLING_PACKAGE, ctx.packageName)
            },
        )
    }

    private fun create(ctx: Context): SpeechRecognizer? = runCatching {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && SpeechRecognizer.isOnDeviceRecognitionAvailable(ctx)) {
            SpeechRecognizer.createOnDeviceSpeechRecognizer(ctx)
        } else {
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
    private fun normalize(text: String): String =
        text.trim().removeSuffix(".").trim()

    private val listener = object : RecognitionListener {
        override fun onReadyForSpeech(params: Bundle?) {}
        override fun onBeginningOfSpeech() {}
        override fun onRmsChanged(rmsdB: Float) {}
        override fun onBufferReceived(buffer: ByteArray?) {}
        override fun onEndOfSpeech() {}
        override fun onEvent(eventType: Int, params: Bundle?) {}

        override fun onPartialResults(partialResults: Bundle?) {
            val text = partialResults?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()
            if (!text.isNullOrBlank()) onState?.invoke(VoiceState.Partial(text))
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

/** The Activity behind a (possibly wrapped) Compose context. */
fun Context.findActivity(): Activity? {
    var ctx: Context? = this
    while (ctx is ContextWrapper) {
        if (ctx is Activity) return ctx
        ctx = ctx.baseContext
    }
    return null
}