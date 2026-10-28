package dev.ldmux.mobile.voice

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Handler
import android.os.Looper
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener

/**
 * Android's speech output: [TextToSpeech], with audio focus.
 *
 * Focus is requested as a transient, may-duck assistant stream, so music lowers instead of
 * stopping, and is released as soon as the utterance ends or is stopped. The engine is created
 * lazily on first use and kept: it initialises asynchronously, so a [speak] that arrives before
 * it is ready is held and played when it is, not dropped. All callbacks are posted to the main
 * thread, because the caller is a state machine that is not thread-safe.
 */
actual object SpeechOutput {
    private val main = Handler(Looper.getMainLooper())
    private var tts: TextToSpeech? = null
    private var initialised = false
    private var failed: String? = null
    private var waiting: Triple<String, () -> Unit, (String) -> Unit>? = null
    private var focus: AudioFocusRequest? = null
    private var counter = 0
    private var current: String? = null
    private var onDone: (() -> Unit)? = null
    private var onError: ((String) -> Unit)? = null

    actual val ready: Boolean get() = initialised && failed == null

    actual fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit) {
        val context: Context? = Dictation.hostActivity()?.applicationContext
        if (context == null) {
            onError("no Android context yet")
            return
        }
        if (tts == null) {
            waiting = Triple(text, onDone, onError)
            tts = TextToSpeech(context) { status ->
                main.post {
                    if (status == TextToSpeech.SUCCESS) {
                        initialised = true
                        tts?.setOnUtteranceProgressListener(listener)
                        waiting?.let { (t, d, e) -> waiting = null; start(context, t, d, e) }
                    } else {
                        failed = "the speech engine did not start"
                        waiting?.let { (_, _, e) -> waiting = null; e(failed!!) }
                    }
                }
            }
            return
        }
        failed?.let { onError(it); return }
        if (!initialised) {
            waiting = Triple(text, onDone, onError)
            return
        }
        start(context, text, onDone, onError)
    }

    private fun start(context: Context, text: String, done: () -> Unit, error: (String) -> Unit) {
        onDone = done
        onError = error
        requestFocus(context)
        val id = "ldmux-${counter++}"
        current = id
        val result = tts?.speak(text, TextToSpeech.QUEUE_FLUSH, null, id)
        if (result != TextToSpeech.SUCCESS) {
            release(context)
            current = null
            error("the speech engine refused the text")
        }
    }

    actual fun stop() {
        current = null
        onDone = null
        tts?.stop()
        Dictation.hostActivity()?.applicationContext?.let { release(it) }
    }

    private val listener = object : UtteranceProgressListener() {
        override fun onStart(utteranceId: String?) {}

        override fun onDone(utteranceId: String?) = finish(utteranceId, null)

        @Deprecated("Deprecated in Java")
        override fun onError(utteranceId: String?) = finish(utteranceId, "the speech engine failed")

        override fun onError(utteranceId: String?, errorCode: Int) =
            finish(utteranceId, "the speech engine failed ($errorCode)")
    }

    private fun finish(utteranceId: String?, error: String?) {
        main.post {
            // A stale callback from an utterance that was stopped must not advance the queue.
            if (utteranceId == null || utteranceId != current) return@post
            current = null
            Dictation.hostActivity()?.applicationContext?.let { release(it) }
            if (error == null) onDone?.invoke() else onError?.invoke(error)
        }
    }

    private fun requestFocus(context: Context) {
        val audio = context.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ASSISTANT)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
                    .build(),
            )
            .build()
        focus = request
        audio.requestAudioFocus(request)
    }

    private fun release(context: Context) {
        val audio = context.getSystemService(Context.AUDIO_SERVICE) as? AudioManager ?: return
        focus?.let { audio.abandonAudioFocusRequest(it) }
        focus = null
    }
}
