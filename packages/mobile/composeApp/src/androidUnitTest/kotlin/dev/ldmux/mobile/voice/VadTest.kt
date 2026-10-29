package dev.ldmux.mobile.voice

import kotlin.math.PI
import kotlin.math.sin
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** Synthetic 20 ms frames at 16 kHz. */
private const val N = 320

private fun sine(hz: Double, amp: Double): ShortArray = ShortArray(N) { i -> (amp * sin(2 * PI * hz * i / 16_000.0)).toInt().toShort() }
private fun silence(): ShortArray = ShortArray(N)

/** Deterministic broadband noise at [amp] peak (an LCG: no platform random, same every run). */
private fun noise(amp: Int, seed: Int): ShortArray {
    var x = seed
    return ShortArray(N) {
        x = x * 1103515245 + 12345
        (((x ushr 16) % (2 * amp + 1)) - amp).toShort()
    }
}

/** The worst hiss: alternating sign every sample, so a crossing rate of ~1. */
private fun hiss(amp: Int): ShortArray = ShortArray(N) { i -> (if (i % 2 == 0) amp else -amp).toShort() }

private fun SpeechDetector.run(vararg frames: ShortArray): List<VadEvent> = frames.mapNotNull { accept(it) }

private fun frames(n: Int, f: () -> ShortArray): Array<ShortArray> = Array(n) { f() }

class VadTest {
    @Test
    fun `silence is never speech`() {
        val d = SpeechDetector()
        assertTrue(d.run(*frames(500) { silence() }).isEmpty())
        assertFalse(d.inSpeech)
    }

    @Test
    fun `quiet room noise below the absolute floor is not speech`() {
        val d = SpeechDetector()
        assertTrue(d.run(*Array(500) { noise(300, it + 1) }).isEmpty())
    }

    @Test
    fun `a voice-like tone is voiced, and speech starts after the onset frames, once`() {
        val d = SpeechDetector()
        val voice = sine(200.0, 5000.0)
        assertTrue(d.classify(voice).voiced)
        val events = d.run(*frames(40) { voice })
        assertEquals(listOf<VadEvent>(VadEvent.SpeechStart), events)
        assertTrue(d.inSpeech)
    }

    @Test
    fun `speech does not start before the onset frame count`() {
        val d = SpeechDetector(VadConfig(onsetFrames = 5))
        val voice = sine(200.0, 5000.0)
        repeat(4) { assertNull(d.accept(voice)) }
        assertEquals(VadEvent.SpeechStart, d.accept(voice))
    }

    @Test
    fun `loud broadband hiss is rejected by the crossing rate even though it is loud`() {
        val d = SpeechDetector()
        val h = hiss(9000)
        val f = d.classify(h)
        assertTrue(f.rms > 8000, "it is loud")
        assertTrue(f.zcr > 0.9)
        assertFalse(f.voiced)
        assertTrue(d.run(*frames(200) { h }).isEmpty())
    }

    @Test
    fun `a click of one or two frames is not speech`() {
        val d = SpeechDetector()
        val events = d.run(silence(), sine(200.0, 9000.0), sine(200.0, 9000.0), *frames(100) { silence() })
        assertTrue(events.isEmpty())
    }

    @Test
    fun `a short gap inside speech is bridged and the end comes only after the hangover`() {
        val d = SpeechDetector(VadConfig(onsetFrames = 3, hangoverFrames = 25))
        val voice = sine(200.0, 5000.0)
        assertEquals(listOf<VadEvent>(VadEvent.SpeechStart), d.run(*frames(10) { voice }))
        // 20 silent frames (400 ms) is a breath: still in speech, then voice resumes.
        assertTrue(d.run(*frames(20) { silence() }).isEmpty())
        assertTrue(d.inSpeech)
        assertTrue(d.run(*frames(10) { voice }).isEmpty(), "no second start: it never ended")
        // Now a real end, on exactly the 25th silent frame.
        val sil = frames(25) { silence() }
        for (i in 0 until 24) assertNull(d.accept(sil[i]))
        assertEquals(VadEvent.SpeechEnd, d.accept(sil[24]))
        assertFalse(d.inSpeech)
    }

    @Test
    fun `a steady hum is absorbed into the noise floor while louder speech over it still counts`() {
        val d = SpeechDetector()
        val hum = sine(120.0, 2000.0) // rms ~1414, above the absolute floor of 600
        val events = d.run(*frames(700) { hum })
        assertEquals(VadEvent.SpeechStart, events.first(), "at first it looks like speech")
        assertFalse(d.classify(hum).voiced, "after a few seconds the hum is the room")
        assertTrue(d.noiseFloor > 500)
        // Once absorbed the floor sits at the hum's level, so a voice must be ~3x louder than the hum.
        assertTrue(d.classify(sine(200.0, 12000.0)).voiced, "a voice well over the hum is still heard")
        assertFalse(d.classify(sine(200.0, 3000.0)).voiced, "one barely over it is not")
    }

    @Test
    fun `an empty frame is silence`() {
        val d = SpeechDetector()
        assertFalse(d.classify(ShortArray(0)).voiced)
        assertNull(d.accept(ShortArray(0)))
    }

    @Test
    fun `the barge-in config is stricter so a normal voice at modest level does not interrupt`() {
        val modest = sine(200.0, 2200.0) // rms ~1556: speech by default, below barge-in's floor
        assertTrue(SpeechDetector().classify(modest).voiced)
        assertFalse(SpeechDetector(VadConfig.forBargeIn()).classify(modest).voiced)
        // And it needs a longer onset (10 frames, 200 ms), not 5.
        val d = SpeechDetector(VadConfig.forBargeIn())
        val loud = sine(200.0, 9000.0)
        repeat(9) { assertNull(d.accept(loud)) }
        assertEquals(VadEvent.SpeechStart, d.accept(loud))
    }

    @Test
    fun `reset forgets everything in progress`() {
        val d = SpeechDetector()
        d.run(*frames(10) { sine(200.0, 5000.0) })
        assertTrue(d.inSpeech)
        d.reset()
        assertFalse(d.inSpeech)
    }
}
