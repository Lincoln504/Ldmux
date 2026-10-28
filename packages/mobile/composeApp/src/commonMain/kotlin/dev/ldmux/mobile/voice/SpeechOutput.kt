package dev.ldmux.mobile.voice

/**
 * Speak one chunk of text. The shared concept is "say this, tell me when you have finished or
 * could not" and nothing else (ADR-0013): audio focus, the engine, the voice and the locale are
 * the platform's business, and each platform half is a thin delegation. The queue, the chunking
 * and when to speak at all live in common code ([VoiceMode], [chunkForSpeech]).
 *
 * One utterance at a time: a new [speak] replaces nothing, so the caller feeds the next chunk
 * from [onDone]. [stop] ends the current one without calling [onDone].
 */
expect object SpeechOutput {
    /** True once the platform's synthesiser is ready (it initialises asynchronously). */
    val ready: Boolean

    fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit)

    fun stop()
}
