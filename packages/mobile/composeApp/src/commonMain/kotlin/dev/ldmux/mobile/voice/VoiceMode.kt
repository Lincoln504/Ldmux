package dev.ldmux.mobile.voice

/** Why the microphone is not being opened. */
enum class VoiceDenial {
    /** The platform refused the microphone. */
    Permission,

    /** Another device holds the shell: only the holder may speak into a session (ADR-0010). */
    NotHolder,
}

/**
 * Where voice mode is. `Idle` is off. The loop is
 * `Listening -> Capturing -> Submitting -> Thinking -> Speaking -> Listening`, with
 * [Interrupted] a transient between a stopped utterance and the reopened microphone.
 */
sealed interface VoicePhase {
    data object Idle : VoicePhase
    data object Listening : VoicePhase
    data object Capturing : VoicePhase
    data object Submitting : VoicePhase
    data object Thinking : VoicePhase
    data object Speaking : VoicePhase
    data object Interrupted : VoicePhase
    data class Denied(val why: VoiceDenial) : VoicePhase
    data object Unavailable : VoicePhase
    data class Failed(val reason: String) : VoicePhase
}

sealed interface VoiceEvent {
    data object Enable : VoiceEvent
    data object Disable : VoiceEvent

    /** The shell on screen changed, or whether this device holds it did. */
    data class Bind(val sessionId: String?, val holder: Boolean) : VoiceEvent

    data object MicOpened : VoiceEvent

    /** Voice activity: the user started talking. */
    data object Heard : VoiceEvent

    /** A final transcript. Blank or a stop phrase is discarded, not submitted. */
    data class Transcript(val text: String) : VoiceEvent

    /** A long silence: leave the mode rather than listen forever. */
    data object LongSilence : VoiceEvent

    data object PermissionDenied : VoiceEvent
    data object NoRecogniser : VoiceEvent

    /**
     * The only recogniser on this phone is a cloud one: what is said leaves the device. Voice
     * mode listens continuously, so unlike a one-shot dictation the user cannot be expected to
     * notice and stop it. Refused unless [VoicePolicy.allowCloud] says the user opted in.
     */
    data object CloudRecogniser : VoiceEvent
    data class Fault(val reason: String) : VoiceEvent

    /** The submit reached the shell. */
    data object Submitted : VoiceEvent

    /** Speech arrived for [sessionId] (from the daemon or an OSC). */
    data class Say(val sessionId: String, val speech: SpeechEvent) : VoiceEvent

    /** The chunk that was playing finished. */
    data object Played : VoiceEvent
    data class PlaybackFailed(val reason: String) : VoiceEvent

    /** The user spoke over the answer (only acted on when barge-in is enabled). */
    data object BargeIn : VoiceEvent

    /**
     * The recogniser ended without a transcript: it timed out, or heard nothing it could
     * use. In a continuous loop that is silence, not a fault, so the machine reopens it
     * (bounded by [VoicePolicy.maxIdleRestarts], so a recogniser that returns instantly
     * every time ends in [VoicePhase.Failed] rather than spinning).
     */
    data object RecogniserIdle : VoiceEvent

    /** Time passing, for the thinking timeout and the long-silence exit. */
    data class Tick(val nowMs: Long) : VoiceEvent
}

/** What the platform layer is asked to do. The machine performs none of it itself. */
sealed interface VoiceEffect {
    data object OpenMic : VoiceEffect
    data object CloseMic : VoiceEffect

    /** Type [text] into [sessionId] and press Enter. Only ever the user's own words. */
    data class Submit(val sessionId: String, val text: String) : VoiceEffect
    data class SpeakChunk(val text: String) : VoiceEffect
    data object StopSpeaking : VoiceEffect

    /** Something the user should be told, in one line. */
    data class Notice(val text: String) : VoiceEffect
}

/** The tunables, in one place so a test can shorten them and a reader can see them. */
data class VoicePolicy(
    /** Without a speech event after a submit, reopen the microphone after this long. */
    val thinkingTimeoutMs: Long = 120_000,
    /** Listening this long with no voice at all leaves the mode, so the mic is not open forever. */
    val longSilenceMs: Long = 60_000,
    /** Whether a cloud recogniser may be used. Off: it is never silently switched to. */
    val allowCloud: Boolean = false,
    /** Consecutive empty recogniser sessions tolerated before voice mode reports a fault. */
    val maxIdleRestarts: Int = 30,
    /** Whether talking over the answer stops it. Off: the mic is muted while speaking. */
    val bargeIn: Boolean = false,
    /** Utterances that mean "stop", not a command. Compared lowercased and trimmed. */
    val stopPhrases: Set<String> = setOf("stop", "stop listening", "cancel", "never mind"),
)

/**
 * The voice-mode state machine. Pure: events in, [VoiceEffect]s out, no clock, no audio, no
 * socket (ADR-0013: everything decidable once lives in common code).
 *
 * Rules, each pinned by a test:
 *  - The microphone opens only for a bound shell this device holds.
 *  - It is released while the agent is thinking and while speaking (unless barge-in is on).
 *  - Speech for a shell other than the bound one is dropped and counted, not queued.
 *  - Speech text never becomes a [VoiceEffect.Submit]: only a transcript does.
 *  - A runaway answer is chunked and capped ([chunkForSpeech]); a truncation is announced.
 */
class VoiceMode(val policy: VoicePolicy = VoicePolicy()) {
    var phase: VoicePhase = VoicePhase.Idle
        private set

    /** Speech dropped because it was for a shell that was not on screen. */
    var droppedForOtherShell: Int = 0
        private set

    private var enabled = false
    private var sessionId: String? = null
    private var holder = false
    private val queue = ArrayDeque<String>()
    private var finalSeen = true
    // Times are unset (-1) until the first Tick says what the clock is. Stamping them with a
    // made-up 0 meant the first real Tick (epoch milliseconds) looked like hours had passed.
    private var thinkingSince = UNSET
    private var lastActivity = UNSET
    private var now = UNSET
    private var idleRestarts = 0

    fun handle(event: VoiceEvent): List<VoiceEffect> {
        val fx = ArrayList<VoiceEffect>()
        when (event) {
            VoiceEvent.Enable -> {
                enabled = true
                startListeningIfAllowed(fx)
            }
            VoiceEvent.Disable -> {
                enabled = false
                stopEverything(fx)
                phase = VoicePhase.Idle
            }
            is VoiceEvent.Bind -> {
                val changed = event.sessionId != sessionId || event.holder != holder
                sessionId = event.sessionId
                holder = event.holder
                if (changed && enabled) {
                    stopEverything(fx)
                    startListeningIfAllowed(fx)
                }
            }
            VoiceEvent.MicOpened -> if (phase == VoicePhase.Interrupted) phase = VoicePhase.Listening
            VoiceEvent.Heard -> when (phase) {
                VoicePhase.Listening -> {
                    phase = VoicePhase.Capturing
                    markActive()
                }
                VoicePhase.Capturing -> markActive()
                VoicePhase.Speaking -> if (policy.bargeIn) interrupt(fx)
                else -> {}
            }
            is VoiceEvent.Transcript -> if (phase == VoicePhase.Listening || phase == VoicePhase.Capturing) {
                val said = event.text.trim()
                val sid = sessionId
                when {
                    // Nothing recognised: the microphone simply stays open.
                    said.isEmpty() -> phase = VoicePhase.Listening
                    // "stop" is the user leaving the mode, not a command to type.
                    said.lowercase() in policy.stopPhrases -> {
                        fx += VoiceEffect.CloseMic
                        phase = VoicePhase.Idle
                        enabled = false
                    }
                    sid != null && holder -> {
                        fx += VoiceEffect.CloseMic
                        fx += VoiceEffect.Submit(sid, said)
                        phase = VoicePhase.Submitting
                    }
                    else -> phase = VoicePhase.Listening
                }
            }
            VoiceEvent.Submitted -> if (phase == VoicePhase.Submitting) {
                phase = VoicePhase.Thinking
                thinkingSince = now
            }
            VoiceEvent.RecogniserIdle -> if (phase == VoicePhase.Listening || phase == VoicePhase.Capturing) {
                idleRestarts++
                if (idleRestarts > policy.maxIdleRestarts) {
                    stopEverything(fx)
                    phase = VoicePhase.Failed("the recogniser keeps returning nothing")
                } else {
                    // Capturing -> Listening: whatever was half-heard produced no text.
                    phase = VoicePhase.Listening
                    fx += VoiceEffect.OpenMic
                }
            }
            VoiceEvent.LongSilence -> if (phase == VoicePhase.Listening || phase == VoicePhase.Capturing) {
                fx += VoiceEffect.CloseMic
                phase = VoicePhase.Idle
                enabled = false
                fx += VoiceEffect.Notice("voice mode ended: no speech for a while")
            }
            VoiceEvent.PermissionDenied -> {
                stopEverything(fx)
                phase = VoicePhase.Denied(VoiceDenial.Permission)
            }
            VoiceEvent.CloudRecogniser -> if (!policy.allowCloud) {
                stopEverything(fx)
                phase = VoicePhase.Unavailable
                fx += VoiceEffect.Notice("voice mode needs on-device recognition; this phone only has a cloud recogniser, so audio would leave it")
            } else {
                fx += VoiceEffect.Notice("cloud recognition: what you say leaves the phone")
            }
            VoiceEvent.NoRecogniser -> {
                stopEverything(fx)
                phase = VoicePhase.Unavailable
            }
            is VoiceEvent.Fault -> {
                stopEverything(fx)
                phase = VoicePhase.Failed(event.reason)
            }
            is VoiceEvent.Say -> onSay(event, fx)
            VoiceEvent.Played -> if (phase == VoicePhase.Speaking) advance(fx)
            is VoiceEvent.PlaybackFailed -> if (phase == VoicePhase.Speaking) {
                queue.clear()
                fx += VoiceEffect.Notice("could not speak: ${event.reason}")
                finishSpeaking(fx)
            }
            VoiceEvent.BargeIn -> if (phase == VoicePhase.Speaking && policy.bargeIn) interrupt(fx)
            is VoiceEvent.Tick -> {
                now = event.nowMs
                if (thinkingSince == UNSET) thinkingSince = now
                if (lastActivity == UNSET) lastActivity = now
                if (phase == VoicePhase.Thinking && now - thinkingSince >= policy.thinkingTimeoutMs) {
                    fx += VoiceEffect.Notice("no spoken reply; listening again")
                    openMic(fx)
                } else if (phase == VoicePhase.Listening && now - lastActivity >= policy.longSilenceMs) {
                    fx += VoiceEffect.CloseMic
                    phase = VoicePhase.Idle
                    enabled = false
                    fx += VoiceEffect.Notice("voice mode ended: no speech for a while")
                }
            }
        }
        return fx
    }

    private fun onSay(event: VoiceEvent.Say, fx: MutableList<VoiceEffect>) {
        if (!enabled || event.sessionId != sessionId) {
            if (event.sessionId != sessionId) droppedForOtherShell++
            return
        }
        // Speech is accepted while thinking, while already speaking (a later part of the same
        // turn), and while listening (the agent answered something typed by hand). Not while
        // denied/unavailable/failed: nothing is going to be heard.
        when (phase) {
            VoicePhase.Thinking, VoicePhase.Speaking, VoicePhase.Listening,
            VoicePhase.Capturing, VoicePhase.Submitting,
            -> {}
            else -> return
        }
        val chunks = chunkForSpeech(event.speech.text)
        if (chunks.chunks.isEmpty()) return
        val wasSpeaking = phase == VoicePhase.Speaking
        chunks.chunks.forEach { queue.addLast(it) }
        finalSeen = event.speech.final
        if (chunks.truncated) fx += VoiceEffect.Notice("a long answer was shortened for speech")
        if (!wasSpeaking) {
            fx += VoiceEffect.CloseMic
            phase = VoicePhase.Speaking
            fx += VoiceEffect.SpeakChunk(queue.removeFirst())
        }
    }

    private fun advance(fx: MutableList<VoiceEffect>) {
        if (queue.isNotEmpty()) {
            fx += VoiceEffect.SpeakChunk(queue.removeFirst())
        } else {
            finishSpeaking(fx)
        }
    }

    private fun finishSpeaking(fx: MutableList<VoiceEffect>) {
        if (finalSeen) {
            openMic(fx)
        } else {
            // More of this turn is coming; wait for it with the microphone still released.
            phase = VoicePhase.Thinking
            thinkingSince = now
        }
    }

    private fun interrupt(fx: MutableList<VoiceEffect>) {
        queue.clear()
        finalSeen = true
        fx += VoiceEffect.StopSpeaking
        phase = VoicePhase.Interrupted
        fx += VoiceEffect.OpenMic
    }

    /** Voice was heard, or the microphone has just been handed back: restart both clocks. */
    private fun markActive() {
        lastActivity = now
        idleRestarts = 0
    }

    private fun openMic(fx: MutableList<VoiceEffect>) {
        if (sessionId != null && holder && enabled) {
            fx += VoiceEffect.OpenMic
            phase = VoicePhase.Listening
            markActive()
        } else {
            startListeningIfAllowed(fx)
        }
    }

    private fun startListeningIfAllowed(fx: MutableList<VoiceEffect>) {
        phase = when {
            !enabled -> VoicePhase.Idle
            sessionId == null -> VoicePhase.Idle
            !holder -> VoicePhase.Denied(VoiceDenial.NotHolder)
            else -> {
                fx += VoiceEffect.OpenMic
                markActive()
                VoicePhase.Listening
            }
        }
    }

    private fun stopEverything(fx: MutableList<VoiceEffect>) {
        if (phase == VoicePhase.Speaking || queue.isNotEmpty()) fx += VoiceEffect.StopSpeaking
        queue.clear()
        finalSeen = true
        fx += VoiceEffect.CloseMic
    }
}

private const val UNSET: Long = -1L
