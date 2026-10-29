package dev.ldmux.mobile.voice

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.ldmux.mobile.TerminalHandle

/** [Dictation] as the driver's microphone: one recognition session per open. */
object DictationMic : MicPort {
    override fun open(onText: (String) -> Unit, onState: (VoiceState) -> Unit) {
        // toggle() switches off a session that is running, which is the opposite of "open".
        if (Dictation.listening) Dictation.stop()
        Dictation.toggle(onText, onState)
    }

    override fun close() {
        Dictation.stop()
    }
}

/** [SpeechOutput] as the driver's speaker. */
object SpeechOutputSpeaker : SpeakerPort {
    override fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit) = SpeechOutput.speak(text, onDone, onError)
    override fun stop() = SpeechOutput.stop()
}

/**
 * The app's one voice mode: the [VoiceMode] machine, its [VoiceDriver], and the shell it is
 * bound to. It adds no policy. It exists so the UI has a single object that is bound to "the
 * shell on screen" and so that the two inputs that say which shell that is (the handle and
 * whether this device holds it) reach the machine in one place.
 *
 * **What it enforces, each pinned by a test:**
 *  - Speech for a shell that is not the bound one is ignored ([say], and the machine drops it).
 *  - Text is typed into the bound shell only while [holder] is true. A shell held elsewhere
 *    makes the machine refuse to open the microphone at all (ADR-0010).
 *  - What is submitted is only ever the user's transcript. Speech text is spoken, never typed.
 *
 * Main thread only, like the driver it wraps.
 */
class VoiceController(
    mic: MicPort = DictationMic,
    speaker: SpeakerPort = SpeechOutputSpeaker,
    capture: CapturePort? = null,
    policy: VoicePolicy = VoicePolicy(),
    private val typeInto: (TerminalHandle, String) -> Unit = { handle, text ->
        handle.sendText(text)
        handle.sendKey("Enter")
    },
) {
    /** Where voice mode is, for the UI to draw. */
    var phase: VoicePhase by mutableStateOf(VoicePhase.Idle)
        private set

    /** The last one-line thing the user should be told, or null. */
    var notice: String? by mutableStateOf(null)

    /** Voice mode is switched on (it may still be [VoicePhase.Denied] or [VoicePhase.Unavailable]). */
    var enabled: Boolean by mutableStateOf(false)
        private set

    private var sessionId: String? = null
    private var handle: TerminalHandle? = null

    private val driver = VoiceDriver(
        mode = VoiceMode(policy),
        mic = mic,
        speaker = speaker,
        submit = { sid, text ->
            // The machine only emits Submit for the bound shell it holds; this checks the handle
            // is still that shell's, so a transcript can never land in a shell that slid in later.
            val h = handle
            if (h != null && sid == sessionId) typeInto(h, text)
        },
        notice = { notice = it },
        capture = capture,
        onPhase = {
            phase = it
            // Idle means the machine is off: the user said stop, a long silence ended it, or there is
            // no shell to listen for. The toggle follows it rather than staying lit over a dead mic.
            if (it == VoicePhase.Idle) enabled = false
        },
    )

    fun toggle() = if (enabled) disable() else enable()

    fun enable() {
        enabled = true
        notice = null
        driver.enable()
    }

    fun disable() {
        enabled = false
        driver.disable()
    }

    /** The shell on screen changed, or whether this device holds it did. */
    fun bind(sessionId: String?, handle: TerminalHandle?, holder: Boolean) {
        this.sessionId = sessionId
        this.handle = handle
        driver.bind(sessionId, holder && handle != null)
    }

    /** An agent's speech from a shell. Ignored unless it is for the bound one. */
    fun say(sessionId: String, speech: SpeechEvent) = driver.say(sessionId, speech)

    fun tick(nowMs: Long) = driver.tick(nowMs)

    /** The app went to the background: nothing may keep listening or speaking. */
    fun shutdown() {
        enabled = false
        driver.shutdown()
    }
}
