package dev.ldmux.mobile.voice

/** The microphone, as the driver needs it. [Dictation] is the Android-backed implementation. */
interface MicPort {
    fun open(onText: (String) -> Unit, onState: (VoiceState) -> Unit)
    fun close()
}

/** The speaker, as the driver needs it. [SpeechOutput] is the Android-backed implementation. */
interface SpeakerPort {
    fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit)
    fun stop()
}

/**
 * Connects [VoiceMode] to a microphone, a speaker and the shell. It owns no policy: every
 * decision is the machine's, and this only turns [VoiceEffect]s into calls and platform
 * callbacks back into [VoiceEvent]s. Keeping that split is what lets the machine be tested
 * without audio and this without a state machine's rules.
 *
 * Must be used from one thread (the main thread): neither it nor [VoiceMode] locks.
 */
class VoiceDriver(
    private val mode: VoiceMode,
    private val mic: MicPort,
    private val speaker: SpeakerPort,
    /** Type [text] into the session and press Enter. Called only with the user's own words. */
    private val submit: (sessionId: String, text: String) -> Unit,
    private val notice: (String) -> Unit = {},
    /**
     * Where PCM for barge-in comes from, or null for no barge-in. Opened only while the answer
     * is being spoken **and** the policy allows barge-in, and closed the moment either stops
     * being true, so the microphone is never open "just in case".
     */
    private val capture: CapturePort? = null,
    /** Told after every event, with the machine's phase, so a UI can follow it. */
    private val onPhase: (VoicePhase) -> Unit = {},
    private val bargeInVad: VadConfig = VadConfig.forBargeIn(),
) {
    val phase: VoicePhase get() = mode.phase

    private var detector: SpeechDetector? = null

    fun enable() = send(VoiceEvent.Enable)
    fun disable() = send(VoiceEvent.Disable)
    fun bind(sessionId: String?, holder: Boolean) = send(VoiceEvent.Bind(sessionId, holder))
    fun tick(nowMs: Long) = send(VoiceEvent.Tick(nowMs))

    /** An unrecoverable change from outside (the app went to the background). */
    fun shutdown() {
        send(VoiceEvent.Disable)
        capture?.stop()
        detector = null
    }

    /** An agent's speech for [sessionId], from the daemon message or an OSC. */
    fun say(sessionId: String, speech: SpeechEvent) = send(VoiceEvent.Say(sessionId, speech))

    private fun send(event: VoiceEvent) {
        for (effect in mode.handle(event)) run(effect)
        syncCapture()
        onPhase(mode.phase)
    }

    /** The capture runs iff the answer is being spoken and barge-in is on. Nothing else. */
    private fun syncCapture() {
        val port = capture ?: return
        val want = mode.policy.bargeIn && mode.phase == VoicePhase.Speaking
        if (want && detector == null) {
            val d = SpeechDetector(bargeInVad)
            detector = d
            port.start(
                onFrame = { frame -> if (d.accept(frame) == VadEvent.SpeechStart) send(VoiceEvent.BargeIn) },
                onError = {
                    // Barge-in is a convenience: if the capture cannot open, the answer still plays
                    // and the microphone stays closed. Say so once rather than failing the mode.
                    detector = null
                    notice("could not listen while speaking: $it")
                },
            )
        } else if (!want && detector != null) {
            detector = null
            port.stop()
        }
    }

    private fun run(effect: VoiceEffect) {
        when (effect) {
            VoiceEffect.OpenMic -> {
                mic.open(
                    onText = { send(VoiceEvent.Transcript(it)) },
                    onState = { state ->
                        when (state) {
                            VoiceState.Listening -> send(VoiceEvent.MicOpened)
                            VoiceState.ListeningCloud -> send(VoiceEvent.CloudRecogniser)
                            is VoiceState.Partial -> send(VoiceEvent.Heard)
                            VoiceState.NoSpeech -> send(VoiceEvent.RecogniserIdle)
                            VoiceState.Denied -> send(VoiceEvent.PermissionDenied)
                            VoiceState.Unavailable -> send(VoiceEvent.NoRecogniser)
                            is VoiceState.Failed -> send(VoiceEvent.Fault(state.reason))
                            VoiceState.Idle, VoiceState.RequestingPermission -> {}
                        }
                    },
                )
                // The recogniser has no "opened" callback that is distinct from listening on
                // every platform; the machine moves out of Interrupted on MicOpened, so say it.
                send(VoiceEvent.MicOpened)
            }
            VoiceEffect.CloseMic -> mic.close()
            is VoiceEffect.Submit -> {
                submit(effect.sessionId, effect.text)
                send(VoiceEvent.Submitted)
            }
            is VoiceEffect.SpeakChunk -> speaker.speak(
                effect.text,
                onDone = { send(VoiceEvent.Played) },
                onError = { send(VoiceEvent.PlaybackFailed(it)) },
            )
            VoiceEffect.StopSpeaking -> speaker.stop()
            is VoiceEffect.Notice -> notice(effect.text)
        }
    }
}
