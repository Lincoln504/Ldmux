package dev.ldmux.mobile.voice

import kotlin.math.PI
import kotlin.math.sin
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertIs
import kotlin.test.assertTrue

/**
 * The continuous loop: silence handling, the recogniser ending empty, barge-in through a real
 * [SpeechDetector], the cloud refusal, and the ownership gate. Machine tests drive [VoiceMode]
 * directly (no audio, no clock); driver tests use fake ports.
 */
class VoiceLoopTest {
    // ---- the machine -------------------------------------------------------------------

    private fun mode(policy: VoicePolicy = VoicePolicy(longSilenceMs = 10_000, thinkingTimeoutMs = 5_000, maxIdleRestarts = 3)): VoiceMode {
        val m = VoiceMode(policy)
        m.handle(VoiceEvent.Bind("s1", true))
        return m
    }

    @Test
    fun `a long silence while listening leaves the mode and closes the microphone`() {
        val m = mode()
        m.handle(VoiceEvent.Enable)
        m.handle(VoiceEvent.Tick(5_000)) // the first tick says what the clock is
        assertTrue(m.handle(VoiceEvent.Tick(14_999)).isEmpty())
        val fx = m.handle(VoiceEvent.Tick(15_000))
        assertEquals(VoicePhase.Idle, m.phase)
        assertTrue(VoiceEffect.CloseMic in fx)
        assertTrue(fx.any { it is VoiceEffect.Notice })
    }

    @Test
    fun `the first tick is an anchor, not hours of silence`() {
        // Epoch milliseconds: if the timers had been stamped with a made-up 0, this would exit at once.
        val m = mode()
        m.handle(VoiceEvent.Enable)
        m.handle(VoiceEvent.Tick(1_700_000_000_000))
        assertEquals(VoicePhase.Listening, m.phase)

        // The same for the thinking timeout: submit before any tick, then a huge first tick.
        val t = mode()
        t.handle(VoiceEvent.Enable)
        t.handle(VoiceEvent.Transcript("ls"))
        t.handle(VoiceEvent.Submitted)
        t.handle(VoiceEvent.Tick(1_700_000_000_000))
        assertEquals(VoicePhase.Thinking, t.phase)
        t.handle(VoiceEvent.Tick(1_700_000_005_000))
        assertEquals(VoicePhase.Listening, t.phase, "and it does time out five seconds later")
    }

    @Test
    fun `hearing the user restarts the silence clock`() {
        val m = mode()
        m.handle(VoiceEvent.Enable)
        m.handle(VoiceEvent.Tick(0))
        m.handle(VoiceEvent.Tick(9_000))
        m.handle(VoiceEvent.Heard)
        assertEquals(VoicePhase.Capturing, m.phase)
        m.handle(VoiceEvent.Transcript("")) // heard something, recognised nothing
        assertEquals(VoicePhase.Listening, m.phase)
        m.handle(VoiceEvent.Tick(18_000))
        assertEquals(VoicePhase.Listening, m.phase, "only 9 s since the voice")
        m.handle(VoiceEvent.Tick(19_000))
        assertEquals(VoicePhase.Idle, m.phase)
    }

    @Test
    fun `no silence timeout while the agent is thinking or speaking`() {
        val m = mode(VoicePolicy(longSilenceMs = 1_000, thinkingTimeoutMs = 600_000))
        m.handle(VoiceEvent.Enable)
        m.handle(VoiceEvent.Tick(0))
        m.handle(VoiceEvent.Transcript("go"))
        m.handle(VoiceEvent.Submitted)
        m.handle(VoiceEvent.Tick(100_000))
        assertEquals(VoicePhase.Thinking, m.phase)
    }

    @Test
    fun `an empty recogniser session reopens the microphone, and is bounded`() {
        val m = mode() // maxIdleRestarts = 3
        m.handle(VoiceEvent.Enable)
        repeat(3) {
            assertEquals(listOf<VoiceEffect>(VoiceEffect.OpenMic), m.handle(VoiceEvent.RecogniserIdle))
            assertEquals(VoicePhase.Listening, m.phase)
        }
        val fx = m.handle(VoiceEvent.RecogniserIdle)
        assertIs<VoicePhase.Failed>(m.phase)
        assertTrue(VoiceEffect.CloseMic in fx)
        assertFalse(VoiceEffect.OpenMic in fx, "a recogniser that returns instantly does not spin")
    }

    @Test
    fun `hearing the user resets the restart budget`() {
        val m = mode()
        m.handle(VoiceEvent.Enable)
        repeat(3) { m.handle(VoiceEvent.RecogniserIdle) }
        m.handle(VoiceEvent.Heard)
        m.handle(VoiceEvent.Transcript(""))
        repeat(3) { m.handle(VoiceEvent.RecogniserIdle) }
        assertEquals(VoicePhase.Listening, m.phase)
    }

    @Test
    fun `an idle recogniser while thinking or speaking is ignored`() {
        val m = mode()
        m.handle(VoiceEvent.Enable)
        m.handle(VoiceEvent.Transcript("go"))
        m.handle(VoiceEvent.Submitted)
        assertTrue(m.handle(VoiceEvent.RecogniserIdle).isEmpty(), "the mic is released while thinking")
        assertEquals(VoicePhase.Thinking, m.phase)
    }

    @Test
    fun `a cloud-only recogniser is refused by default, and never silently used`() {
        val m = mode()
        m.handle(VoiceEvent.Enable)
        val fx = m.handle(VoiceEvent.CloudRecogniser)
        assertEquals(VoicePhase.Unavailable, m.phase)
        assertTrue(VoiceEffect.CloseMic in fx)
        assertTrue(fx.filterIsInstance<VoiceEffect.Notice>().single().text.contains("leave"))

        val ok = VoiceMode(VoicePolicy(allowCloud = true))
        ok.handle(VoiceEvent.Bind("s1", true))
        ok.handle(VoiceEvent.Enable)
        val fx2 = ok.handle(VoiceEvent.CloudRecogniser)
        assertEquals(VoicePhase.Listening, ok.phase)
        assertTrue(fx2.filterIsInstance<VoiceEffect.Notice>().single().text.contains("leaves the phone"), "and it says so")
    }

    // ---- the driver, with fake ports ----------------------------------------------------

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
        override fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit) { said += text; done = onDone }
        override fun stop() { stops++ }
    }

    private class Capture : CapturePort {
        var starts = 0
        var stops = 0
        var onFrame: ((ShortArray) -> Unit)? = null
        var onError: ((String) -> Unit)? = null
        override fun start(onFrame: (ShortArray) -> Unit, onError: (String) -> Unit) { starts++; this.onFrame = onFrame; this.onError = onError }
        override fun stop() { stops++ }
    }

    private fun voice(amp: Double) = ShortArray(320) { i -> (amp * sin(2 * PI * 200.0 * i / 16_000.0)).toInt().toShort() }

    private class Rig(bargeIn: Boolean, withCapture: Boolean = true) {
        val mic = Mic()
        val speaker = Speaker()
        val capture = Capture()
        val submitted = mutableListOf<Pair<String, String>>()
        val notices = mutableListOf<String>()
        val driver = VoiceDriver(
            VoiceMode(VoicePolicy(bargeIn = bargeIn, thinkingTimeoutMs = 5_000, longSilenceMs = 60_000)),
            mic, speaker, { s, t -> submitted += s to t }, { notices += it },
            capture = if (withCapture) capture else null,
        )
    }

    @Test
    fun `the microphone is released while the agent thinks and reopens on its answer`() {
        val r = Rig(bargeIn = false)
        r.driver.bind("s1", true); r.driver.enable()
        assertEquals(1, r.mic.open)
        r.mic.onText!!("run the tests")
        assertEquals(VoicePhase.Thinking, r.driver.phase)
        assertEquals(1, r.mic.closed, "released at submit")
        assertEquals(1, r.mic.open, "and not reopened while thinking")
        r.driver.say("s1", SpeechEvent("All green.", "a", true))
        r.speaker.done!!()
        assertEquals(2, r.mic.open)
    }

    @Test
    fun `no spoken reply reopens the microphone after the thinking timeout`() {
        val r = Rig(bargeIn = false)
        r.driver.bind("s1", true); r.driver.enable()
        r.driver.tick(1_000)
        r.mic.onText!!("hello")
        r.driver.tick(5_999)
        assertEquals(VoicePhase.Thinking, r.driver.phase)
        r.driver.tick(6_000)
        assertEquals(VoicePhase.Listening, r.driver.phase)
        assertEquals(2, r.mic.open)
        assertTrue(r.notices.any { it.contains("listening again") })
    }

    @Test
    fun `the recogniser timing out is reopened by the driver, not treated as a fault`() {
        val r = Rig(bargeIn = false)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onState!!(VoiceState.NoSpeech)
        assertEquals(VoicePhase.Listening, r.driver.phase)
        assertEquals(2, r.mic.open)
        r.mic.onState!!(VoiceState.Idle) // a plain end after a result is not "idle recogniser"
        assertEquals(2, r.mic.open)
    }

    @Test
    fun `a cloud recogniser ends voice mode visibly instead of streaming audio off the phone`() {
        val r = Rig(bargeIn = false)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onState!!(VoiceState.ListeningCloud)
        assertEquals(VoicePhase.Unavailable, r.driver.phase)
        assertTrue(r.mic.closed >= 1)
        assertTrue(r.notices.single().contains("leave"))
    }

    @Test
    fun `another device holding the shell means the microphone is never opened`() {
        val r = Rig(bargeIn = true)
        r.driver.bind("s1", false); r.driver.enable()
        assertEquals(VoicePhase.Denied(VoiceDenial.NotHolder), r.driver.phase)
        assertEquals(0, r.mic.open)
        assertEquals(0, r.capture.starts)
        r.driver.say("s1", SpeechEvent("hi", null, true))
        assertEquals(VoicePhase.Denied(VoiceDenial.NotHolder), r.driver.phase)
        assertTrue(r.speaker.said.isEmpty() || r.capture.starts == 0)
        assertTrue(r.submitted.isEmpty())
    }

    @Test
    fun `barge-in - the user talking over the answer stops it and hands the microphone back`() {
        val r = Rig(bargeIn = true)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onText!!("explain")
        r.driver.say("s1", SpeechEvent("A long answer.", "a", true))
        assertEquals(VoicePhase.Speaking, r.driver.phase)
        assertEquals(1, r.capture.starts, "the capture opens only while speaking")
        val opens = r.mic.open
        // 9 voiced frames: not yet. The 10th is the onset.
        repeat(9) { r.capture.onFrame!!(voice(9000.0)) }
        assertEquals(0, r.speaker.stops)
        r.capture.onFrame!!(voice(9000.0))
        assertEquals(1, r.speaker.stops)
        assertEquals(opens + 1, r.mic.open)
        assertEquals(1, r.capture.stops, "capture is closed the moment speaking ends")
    }

    @Test
    fun `barge-in ignores the phone's own quiet echo`() {
        val r = Rig(bargeIn = true)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onText!!("explain")
        r.driver.say("s1", SpeechEvent("A long answer.", "a", true))
        repeat(200) { r.capture.onFrame!!(voice(1500.0)) }
        assertEquals(0, r.speaker.stops)
        assertEquals(VoicePhase.Speaking, r.driver.phase)
    }

    @Test
    fun `with barge-in off the capture is never opened`() {
        val r = Rig(bargeIn = false)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onText!!("explain")
        r.driver.say("s1", SpeechEvent("A long answer.", "a", true))
        assertEquals(VoicePhase.Speaking, r.driver.phase)
        assertEquals(0, r.capture.starts)
    }

    @Test
    fun `the capture closes when the answer finishes on its own`() {
        val r = Rig(bargeIn = true)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onText!!("explain")
        r.driver.say("s1", SpeechEvent("Short.", "a", true))
        assertEquals(1, r.capture.starts)
        r.speaker.done!!()
        assertEquals(1, r.capture.stops)
        assertEquals(VoicePhase.Listening, r.driver.phase)
    }

    @Test
    fun `a capture that cannot open does not fail the answer`() {
        val r = Rig(bargeIn = true)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onText!!("explain")
        r.driver.say("s1", SpeechEvent("Short.", "a", true))
        r.capture.onError!!("the microphone is in use")
        assertEquals(VoicePhase.Speaking, r.driver.phase)
        assertTrue(r.notices.any { it.contains("could not listen while speaking") })
    }

    @Test
    fun `shutdown closes the microphone, the speaker and the capture`() {
        val r = Rig(bargeIn = true)
        r.driver.bind("s1", true); r.driver.enable()
        r.mic.onText!!("explain")
        r.driver.say("s1", SpeechEvent("Answer.", "a", true))
        r.driver.shutdown()
        assertEquals(VoicePhase.Idle, r.driver.phase)
        assertTrue(r.speaker.stops >= 1)
        assertTrue(r.capture.stops >= 1)
        assertTrue(r.mic.closed >= 1)
    }

    @Test
    fun `every terminal state is reachable and none of them listens`() {
        val denied = Rig(false); denied.driver.bind("s1", true); denied.driver.enable()
        denied.mic.onState!!(VoiceState.Denied)
        assertEquals(VoicePhase.Denied(VoiceDenial.Permission), denied.driver.phase)

        val none = Rig(false); none.driver.bind("s1", true); none.driver.enable()
        none.mic.onState!!(VoiceState.Unavailable)
        assertEquals(VoicePhase.Unavailable, none.driver.phase)

        val failed = Rig(false); failed.driver.bind("s1", true); failed.driver.enable()
        failed.mic.onState!!(VoiceState.Failed("busy"))
        assertEquals(VoicePhase.Failed("busy"), failed.driver.phase)

        val intr = Rig(true); intr.driver.bind("s1", true); intr.driver.enable()
        intr.mic.onText!!("x"); intr.driver.say("s1", SpeechEvent("y", null, true))
        repeat(10) { intr.capture.onFrame!!(voice(9000.0)) }
        // Interrupted is transient: the driver immediately reopens the mic, which moves it to Listening.
        assertEquals(VoicePhase.Listening, intr.driver.phase)
    }
}
