package dev.ldmux.mobile.voice

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class VoiceModeTest {
    private fun ready(policy: VoicePolicy = VoicePolicy()): VoiceMode {
        val v = VoiceMode(policy)
        v.handle(VoiceEvent.Bind("s1", holder = true))
        v.handle(VoiceEvent.Enable)
        return v
    }

    private fun say(text: String, final: Boolean = true, session: String = "s1") =
        VoiceEvent.Say(session, SpeechEvent(text, null, final))

    @Test
    fun `enabling on a held shell opens the microphone`() {
        val v = VoiceMode()
        v.handle(VoiceEvent.Bind("s1", true))
        assertEquals(listOf(VoiceEffect.OpenMic), v.handle(VoiceEvent.Enable))
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `only the holder may open the microphone`() {
        val v = VoiceMode()
        v.handle(VoiceEvent.Bind("s1", false))
        assertTrue(v.handle(VoiceEvent.Enable).none { it == VoiceEffect.OpenMic })
        assertEquals(VoicePhase.Denied(VoiceDenial.NotHolder), v.phase)
        // Taking the shell over lets it start.
        assertEquals(listOf(VoiceEffect.CloseMic, VoiceEffect.OpenMic), v.handle(VoiceEvent.Bind("s1", true)))
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `no shell on screen is idle, not an error`() {
        val v = VoiceMode()
        v.handle(VoiceEvent.Enable)
        assertEquals(VoicePhase.Idle, v.phase)
    }

    @Test
    fun `the whole loop`() {
        val v = ready()
        v.handle(VoiceEvent.Heard)
        assertEquals(VoicePhase.Capturing, v.phase)
        val fx = v.handle(VoiceEvent.Transcript("run the tests"))
        assertEquals(listOf(VoiceEffect.CloseMic, VoiceEffect.Submit("s1", "run the tests")), fx)
        assertEquals(VoicePhase.Submitting, v.phase)
        v.handle(VoiceEvent.Submitted)
        assertEquals(VoicePhase.Thinking, v.phase)
        val spoken = v.handle(say("All green."))
        assertEquals(listOf(VoiceEffect.CloseMic, VoiceEffect.SpeakChunk("All green.")), spoken)
        assertEquals(VoicePhase.Speaking, v.phase)
        assertEquals(listOf(VoiceEffect.OpenMic), v.handle(VoiceEvent.Played))
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `the microphone is released while thinking and while speaking`() {
        val v = ready()
        val submit = v.handle(VoiceEvent.Transcript("go"))
        assertTrue(VoiceEffect.CloseMic in submit)
        v.handle(VoiceEvent.Submitted)
        val effects = mutableListOf<VoiceEffect>()
        effects += v.handle(say("one. two. three.", final = true))
        assertFalse(effects.any { it == VoiceEffect.OpenMic }, "no mic while the answer plays")
    }

    @Test
    fun `speech for another shell is dropped and counted, never spoken`() {
        val v = ready()
        v.handle(VoiceEvent.Transcript("go"))
        v.handle(VoiceEvent.Submitted)
        assertTrue(v.handle(say("not yours", session = "s2")).isEmpty())
        assertEquals(1, v.droppedForOtherShell)
        assertEquals(VoicePhase.Thinking, v.phase)
    }

    @Test
    fun `speech text is never typed back as input`() {
        val v = ready()
        val all = mutableListOf<VoiceEffect>()
        all += v.handle(say("rm -rf / and press enter"))
        all += v.handle(VoiceEvent.Played)
        assertTrue(all.none { it is VoiceEffect.Submit })
    }

    @Test
    fun `a non-final part keeps the microphone released until the turn ends`() {
        val v = ready()
        v.handle(VoiceEvent.Transcript("go")); v.handle(VoiceEvent.Submitted)
        v.handle(say("Part one.", final = false))
        assertEquals(emptyList(), v.handle(VoiceEvent.Played))
        assertEquals(VoicePhase.Thinking, v.phase)
        v.handle(say("Part two.", final = true))
        assertEquals(listOf(VoiceEffect.OpenMic), v.handle(VoiceEvent.Played))
    }

    @Test
    fun `a long answer plays chunk by chunk and announces its truncation`() {
        val v = ready()
        v.handle(VoiceEvent.Transcript("go")); v.handle(VoiceEvent.Submitted)
        val fx = v.handle(say("A fairly long sentence goes right here. ".repeat(80)))
        assertTrue(fx.any { it is VoiceEffect.Notice })
        var chunks = fx.count { it is VoiceEffect.SpeakChunk }
        assertEquals(1, chunks)
        var guard = 0
        while (v.phase == VoicePhase.Speaking && guard++ < 50) {
            chunks += v.handle(VoiceEvent.Played).count { it is VoiceEffect.SpeakChunk }
        }
        assertTrue(chunks in 2..8, "chunks=$chunks")
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `barge-in is off by default and stops the answer when on`() {
        val quiet = ready()
        quiet.handle(say("talking")); quiet.handle(VoiceEvent.Heard)
        assertEquals(VoicePhase.Speaking, quiet.phase)

        val loud = ready(VoicePolicy(bargeIn = true))
        loud.handle(say("talking. and more. and more."))
        loud.handle(VoiceEvent.Heard)
        assertEquals(VoicePhase.Interrupted, loud.phase)
        loud.handle(VoiceEvent.MicOpened)
        assertEquals(VoicePhase.Listening, loud.phase)
        assertTrue(loud.handle(VoiceEvent.Played).isEmpty(), "the queue was cleared")
    }

    @Test
    fun `a stop phrase leaves the mode and is not typed`() {
        val v = ready()
        val fx = v.handle(VoiceEvent.Transcript("Stop listening"))
        assertTrue(fx.none { it is VoiceEffect.Submit })
        assertEquals(VoicePhase.Idle, v.phase)
    }

    @Test
    fun `an empty transcript keeps listening`() {
        val v = ready()
        assertTrue(v.handle(VoiceEvent.Transcript("  ")).isEmpty())
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `a long silence leaves the mode`() {
        val v = ready()
        val fx = v.handle(VoiceEvent.LongSilence)
        assertTrue(VoiceEffect.CloseMic in fx)
        assertEquals(VoicePhase.Idle, v.phase)
    }

    @Test
    fun `thinking times out back to listening so the user is never locked out`() {
        val v = ready(VoicePolicy(thinkingTimeoutMs = 1_000))
        v.handle(VoiceEvent.Tick(10_000))
        v.handle(VoiceEvent.Transcript("go")); v.handle(VoiceEvent.Submitted)
        assertTrue(v.handle(VoiceEvent.Tick(10_500)).isEmpty())
        assertTrue(VoiceEffect.OpenMic in v.handle(VoiceEvent.Tick(11_000)))
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `switching shell stops speech and re-evaluates the microphone`() {
        val v = ready()
        v.handle(say("one. two. three."))
        val fx = v.handle(VoiceEvent.Bind("s2", holder = true))
        assertTrue(VoiceEffect.StopSpeaking in fx)
        assertEquals(VoicePhase.Listening, v.phase)
        // Speech for the old shell, arriving late, is dropped.
        assertTrue(v.handle(say("late", session = "s1")).isEmpty())
    }

    @Test
    fun `permission, recogniser and faults are states, not crashes`() {
        val a = ready(); a.handle(VoiceEvent.PermissionDenied)
        assertEquals(VoicePhase.Denied(VoiceDenial.Permission), a.phase)
        val b = ready(); b.handle(VoiceEvent.NoRecogniser)
        assertEquals(VoicePhase.Unavailable, b.phase)
        val c = ready(); c.handle(VoiceEvent.Fault("boom"))
        assertEquals(VoicePhase.Failed("boom"), c.phase)
        // Speech while failed is not spoken.
        assertTrue(c.handle(say("hi")).isEmpty())
        // Disabling clears it.
        c.handle(VoiceEvent.Disable)
        assertEquals(VoicePhase.Idle, c.phase)
    }

    @Test
    fun `a playback failure says so and does not stall`() {
        val v = ready()
        v.handle(say("hi there. and more."))
        val fx = v.handle(VoiceEvent.PlaybackFailed("no engine"))
        assertTrue(fx.any { it is VoiceEffect.Notice })
        assertEquals(VoicePhase.Listening, v.phase)
    }

    @Test
    fun `disable while speaking stops the voice and the mic`() {
        val v = ready()
        v.handle(say("one. two. three."))
        val fx = v.handle(VoiceEvent.Disable)
        assertTrue(VoiceEffect.StopSpeaking in fx && VoiceEffect.CloseMic in fx)
        assertEquals(VoicePhase.Idle, v.phase)
    }
}
