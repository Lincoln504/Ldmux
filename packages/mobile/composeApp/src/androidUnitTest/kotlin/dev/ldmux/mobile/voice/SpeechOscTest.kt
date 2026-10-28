package dev.ldmux.mobile.voice

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class SpeechOscTest {
    private fun osc(json: String, terminator: String = "\u0007") = "\u001b]$SPEECH_OSC_NUMBER;$json$terminator"
    private fun feed(p: SpeechOscParser, s: String) = p.feed(s.encodeToByteArray())

    private val say = """{"v":1,"kind":"say","text":"Two files changed.","id":"m1","final":false}"""

    @Test
    fun `a complete sequence yields one event`() {
        val got = feed(SpeechOscParser(), "before" + osc(say) + "after")
        assertEquals(listOf(SpeechEvent("Two files changed.", "m1", false)), got)
    }

    @Test
    fun `either terminator works`() {
        assertEquals(1, feed(SpeechOscParser(), osc(say, "\u001b\\")).size)
        assertEquals(1, feed(SpeechOscParser(), osc(say, "\u0007")).size)
    }

    @Test
    fun `final defaults to true and a missing id is null`() {
        val got = feed(SpeechOscParser(), osc("""{"v":1,"kind":"say","text":"hi"}""")).single()
        assertEquals(SpeechEvent("hi", null, true), got)
    }

    @Test
    fun `a sequence split at every possible byte still parses once`() {
        val bytes = ("x" + osc(say) + "y").encodeToByteArray()
        for (cut in 1 until bytes.size) {
            val p = SpeechOscParser()
            val a = p.feed(bytes.copyOfRange(0, cut))
            val b = p.feed(bytes.copyOfRange(cut, bytes.size))
            assertEquals(1, a.size + b.size, "split at $cut")
        }
    }

    @Test
    fun `fed one byte at a time it parses once`() {
        val p = SpeechOscParser()
        val total = osc(say).encodeToByteArray().flatMap { p.feed(byteArrayOf(it)) }
        assertEquals(1, total.size)
    }

    @Test
    fun `an oversize payload is dropped whole, not truncated into speech`() {
        val big = """{"v":1,"kind":"say","text":"${"a".repeat(SPEECH_OSC_MAX_BYTES)}"}"""
        val p = SpeechOscParser()
        assertTrue(feed(p, osc(big)).isEmpty())
        // And the parser is back in step: the next, valid one is read.
        assertEquals(1, feed(p, osc(say)).size)
    }

    @Test
    fun `unknown versions, kinds and malformed json are ignored`() {
        val p = SpeechOscParser()
        for (bad in listOf(
            """{"v":2,"kind":"say","text":"hi"}""",
            """{"v":1,"kind":"shout","text":"hi"}""",
            """{"v":1,"kind":"say"}""",
            """{"v":1,"kind":"say","text":"   "}""",
            """{"v":1,"kind":"say","text":"hi","id":"${"i".repeat(65)}"}""",
            """{not json""",
            """[1,2]""",
            "",
        )) {
            assertTrue(feed(p, osc(bad)).isEmpty(), bad)
        }
    }

    @Test
    fun `other OSC numbers are skipped and do not confuse the parser`() {
        val p = SpeechOscParser()
        val title = "\u001b]0;a title\u0007"
        val clip = "\u001b]52;c;SGVsbG8=\u001b\\"
        val near = "\u001b]13380;$say\u0007" // not 1338
        assertTrue(feed(p, title + clip + near).isEmpty())
        assertEquals(1, feed(p, title + osc(say) + clip).size)
    }

    @Test
    fun `an escape inside the payload abandons it rather than speaking a fragment`() {
        val p = SpeechOscParser()
        val got = feed(p, "\u001b]$SPEECH_OSC_NUMBER;{\"v\":1,\"kind\":\"say\",\"te\u001b[31mxt\":\"hi\"}\u0007")
        assertTrue(got.isEmpty())
        assertEquals(1, feed(p, osc(say)).size, "the next sequence is read normally")
    }

    @Test
    fun `non-ASCII text survives`() {
        val got = feed(SpeechOscParser(), osc("""{"v":1,"kind":"say","text":"完了しました。"}""")).single()
        assertEquals("完了しました。", got.text)
    }

    @Test
    fun `reset drops a half-read sequence`() {
        val p = SpeechOscParser()
        p.feed("\u001b]$SPEECH_OSC_NUMBER;{\"v\":1,".encodeToByteArray())
        p.reset()
        assertTrue(feed(p, "\"kind\":\"say\",\"text\":\"hi\"}\u0007").isEmpty())
    }

    @Test
    fun `parseSpeechPayload is the same reader the daemon message uses`() {
        assertNull(parseSpeechPayload("""{"v":1,"kind":"say","text":""}"""))
        assertEquals("hi", parseSpeechPayload("""{"v":1,"kind":"say","text":"hi"}""")?.text)
    }
}
