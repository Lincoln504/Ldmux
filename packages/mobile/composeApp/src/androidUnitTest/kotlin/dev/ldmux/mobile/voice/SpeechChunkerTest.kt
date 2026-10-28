package dev.ldmux.mobile.voice

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class SpeechChunkerTest {
    @Test
    fun `short text is one chunk`() {
        assertEquals(SpeechChunks(listOf("Done."), false), chunkForSpeech("Done."))
    }

    @Test
    fun `sentences pack into chunks up to the limit and never split mid-sentence`() {
        val text = "One. Two. Three. Four."
        assertEquals(listOf("One. Two. Three. Four."), chunkForSpeech(text).chunks)
        val tight = chunkForSpeech(text, maxChunk = 10).chunks
        assertTrue(tight.all { it.length <= 10 }, tight.toString())
        assertEquals("One. Two. Three. Four.", tight.joinToString(" "))
    }

    @Test
    fun `an overlong sentence breaks at a space and a CJK run breaks hard`() {
        val words = "word ".repeat(100).trim()
        val c = chunkForSpeech(words, maxChunk = 50).chunks
        assertTrue(c.all { it.length <= 50 })
        assertEquals(words, c.joinToString(" "))
        val cjk = "あ".repeat(130)
        assertTrue(chunkForSpeech(cjk, maxChunk = 50).chunks.all { it.length <= 50 })
    }

    @Test
    fun `japanese sentence punctuation is a boundary`() {
        assertEquals(listOf("完了しました。 次です。"), chunkForSpeech("完了しました。次です。").chunks)
        assertEquals(2, chunkForSpeech("完了しました。次です。", maxChunk = 8).chunks.size)
    }

    @Test
    fun `a runaway answer is capped and says so`() {
        val long = "This is a sentence of moderate length. ".repeat(100)
        val r = chunkForSpeech(long)
        assertTrue(r.truncated)
        assertTrue(r.chunks.sumOf { it.length } <= SPEECH_TOTAL_MAX_CHARS)
        assertFalse(chunkForSpeech("short.").truncated)
    }

    @Test
    fun `blank input is no chunks`() {
        assertEquals(SpeechChunks(emptyList(), false), chunkForSpeech("  \n \t "))
    }
}
