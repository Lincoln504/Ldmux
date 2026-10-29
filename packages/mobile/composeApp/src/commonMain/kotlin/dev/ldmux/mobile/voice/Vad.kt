package dev.ldmux.mobile.voice

import kotlin.math.abs
import kotlin.math.sqrt

/**
 * Voice-activity detection, in common code and over plain PCM so that every decision can be
 * tested with synthetic frames (ADR-0013). The platform's only job is to produce 16-bit mono
 * frames ([CapturePort]); what counts as speech is decided here.
 *
 * A frame is *voiced* when its RMS energy clears both an absolute floor and a multiple of an
 * adaptive noise floor, **and** its zero-crossing rate is speech-like. Energy alone fires on a
 * fan or a hiss; the crossing rate is the cheap second witness that separates a voice
 * (low-to-mid crossing rate) from broadband hiss (a crossing rate near the Nyquist limit).
 * This is deliberately simple. It is the gate for barge-in, where a wrong answer costs an
 * interrupted sentence, not a model: it is not a recogniser and does not try to be one.
 */
data class VadConfig(
    /** The absolute RMS (in PCM16 units) a frame must exceed whatever the room sounds like. */
    val minEnergy: Double = 600.0,
    /** A frame must also exceed the adaptive noise floor by this factor. */
    val floorRatio: Double = 3.0,
    /** Zero crossings per sample above which a frame is hiss rather than a voice. */
    val maxZcr: Double = 0.35,
    /** Consecutive voiced frames before speech is declared. A click is one or two frames. */
    val onsetFrames: Int = 5,
    /** Consecutive unvoiced frames before speech is declared over. Bridges a breath. */
    val hangoverFrames: Int = 25,
) {
    companion object {
        /**
         * Stricter than the default, for hearing the user over the phone's own speaker: a
         * louder floor and a longer onset, so the answer being spoken (which the microphone
         * also hears) does not cut itself off.
         */
        fun forBargeIn(): VadConfig = VadConfig(minEnergy = 1800.0, floorRatio = 4.0, onsetFrames = 10)
    }
}

/** What one frame looked like. */
data class VadFrame(val rms: Double, val zcr: Double, val voiced: Boolean)

sealed interface VadEvent {
    /** [VadConfig.onsetFrames] voiced frames in a row. */
    data object SpeechStart : VadEvent

    /** [VadConfig.hangoverFrames] unvoiced frames in a row after speech. */
    data object SpeechEnd : VadEvent
}

/**
 * The detector: frames in, at most one [VadEvent] out per frame.
 *
 * Not thread-safe; feed it from one thread. The noise floor adapts quickly downwards (a room
 * that goes quiet) and slowly upwards, and slowly even through voiced frames, so a steady hum
 * that was loud enough to trigger once is absorbed into the floor instead of being "speech"
 * forever.
 */
class SpeechDetector(private val config: VadConfig = VadConfig()) {
    private var floor = 0.0
    private var run = 0
    private var gap = 0

    /** True between [VadEvent.SpeechStart] and [VadEvent.SpeechEnd]. */
    var inSpeech: Boolean = false
        private set

    /** The adaptive noise floor, for diagnostics and tests. */
    val noiseFloor: Double get() = floor

    fun classify(frame: ShortArray): VadFrame {
        if (frame.isEmpty()) return VadFrame(0.0, 0.0, false)
        var sum = 0.0
        var crossings = 0
        var prev = frame[0].toInt()
        for (i in frame.indices) {
            val s = frame[i].toInt()
            sum += s.toDouble() * s
            if (i > 0 && ((s >= 0) != (prev >= 0))) crossings++
            prev = s
        }
        val rms = sqrt(sum / frame.size)
        val zcr = crossings.toDouble() / frame.size
        val threshold = maxOf(config.minEnergy, floor * config.floorRatio)
        return VadFrame(rms, zcr, rms > threshold && zcr <= config.maxZcr)
    }

    fun accept(frame: ShortArray): VadEvent? {
        val f = classify(frame)
        adapt(f)
        if (f.voiced) {
            gap = 0
            run++
            if (!inSpeech && run >= config.onsetFrames) {
                inSpeech = true
                return VadEvent.SpeechStart
            }
        } else {
            run = 0
            if (inSpeech) {
                gap++
                if (gap >= config.hangoverFrames) {
                    inSpeech = false
                    gap = 0
                    return VadEvent.SpeechEnd
                }
            }
        }
        return null
    }

    fun reset() {
        run = 0
        gap = 0
        inSpeech = false
    }

    private fun adapt(f: VadFrame) {
        val alpha = when {
            f.voiced -> 0.002
            f.rms < floor -> 0.2
            else -> 0.02
        }
        floor += (abs(f.rms) - floor) * alpha
    }
}

/**
 * Where PCM comes from. One implementation per platform, and each is a thin delegation: open
 * the microphone, read 16 kHz mono 16-bit frames, call [onFrame] **on the main thread**, stop.
 * It carries no policy and no notion of speech.
 */
interface CapturePort {
    /** Frames are about 20 ms. [onError] is called once, and nothing follows it. */
    fun start(onFrame: (ShortArray) -> Unit, onError: (String) -> Unit)

    fun stop()
}

/** A platform with no capture (a test double, or a target that has none yet). */
expect fun newCapture(): CapturePort?
