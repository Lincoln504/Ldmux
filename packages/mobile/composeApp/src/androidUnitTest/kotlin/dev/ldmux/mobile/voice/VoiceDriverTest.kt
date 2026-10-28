package dev.ldmux.mobile.voice

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class VoiceDriverTest {
    private class Mic : MicPort {
        var open = 0
        var closed = 0
        var onText: ((String) -> Unit)? = null
        var onState: ((VoiceState) -> Unit)? = null
        override fun open(onText: (String) -> Unit, onState: (VoiceState) -> Unit) {
            open++; this.onText = onText; this.onState = onState
        }
        override fun close() { closed++ }
    }

    private class Speaker : SpeakerPort {
        val said = mutableListOf<String>()
        var stops = 0
        var done: (() -> Unit)? = null
        override fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit) {
            said += text; done = onDone
        }
        override fun stop() { stops++ }
    }

    private val mic = Mic()
    private val speaker = Speaker()
    private val submitted = mutableListOf<Pair<String, String>>()
    private val driver = VoiceDriver(VoiceMode(), mic, speaker, { s, t -> submitted += s to t })

    @Test
    fun `speak, transcribe, submit, hear the answer, listen again`() {
        driver.bind("s1", true)
        driver.enable()
        assertEquals(1, mic.open)
        mic.onText!!("list the files")
        assertEquals(listOf("s1" to "list the files"), submitted)
        assertEquals(VoicePhase.Thinking, driver.phase)
        driver.say("s1", SpeechEvent("Three files.", "m", true))
        assertEquals(listOf("Three files."), speaker.said)
        speaker.done!!()
        assertEquals(2, mic.open, "the microphone reopens after the answer")
        assertEquals(VoicePhase.Listening, driver.phase)
    }

    @Test
    fun `agent speech is spoken but never typed`() {
        driver.bind("s1", true); driver.enable()
        driver.say("s1", SpeechEvent("rm -rf everything", null, true))
        assertTrue(submitted.isEmpty())
    }

    @Test
    fun `the platform reporting a denied microphone becomes a state`() {
        driver.bind("s1", true); driver.enable()
        mic.onState!!(VoiceState.Denied)
        assertEquals(VoicePhase.Denied(VoiceDenial.Permission), driver.phase)
    }

    @Test
    fun `disabling closes the microphone and silences the speaker`() {
        driver.bind("s1", true); driver.enable()
        driver.say("s1", SpeechEvent("one. two. three.", null, true))
        driver.disable()
        assertTrue(speaker.stops >= 1)
        assertTrue(mic.closed >= 1)
        assertEquals(VoicePhase.Idle, driver.phase)
    }
}
