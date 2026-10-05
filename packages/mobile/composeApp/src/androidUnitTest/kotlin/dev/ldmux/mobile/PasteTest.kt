package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

class PasteTest {
    @Test
    fun `an escape in a bracketed paste cannot close the paste early`() {
        val hostile = "ls\u001b[201~; rm -rf ~\n"
        val safe = sanitizeBracketedPaste(hostile)
        assertFalse('\u001b' in safe)
        assertEquals("ls[201~; rm -rf ~\n", safe)
    }

    @Test
    fun `a large paste is split under the daemon limit and loses nothing`() {
        val text = "é".repeat(30_000)
        val parts = chunkInput(text)
        assertEquals(text, parts.joinToString(""))
        for (p in parts) assertTrue(p.encodeToByteArray().size < 16 * 1024, "${p.length}")
    }

    @Test
    fun `a chunk never splits a surrogate pair`() {
        val text = "\uD83D\uDE00".repeat(5000)
        val parts = chunkInput(text, 4001)
        assertEquals(text, parts.joinToString(""))
        for (p in parts) assertTrue(!p.last().isHighSurrogate() && !p.first().isLowSurrogate())
    }

    @Test
    fun `empty and small text`() {
        assertEquals(emptyList(), chunkInput(""))
        assertEquals(listOf("abc"), chunkInput("abc"))
    }

    @Test
    fun `hardware chords become protocol key names`() {
        assertEquals("C-c", hardwareChord('c', ctrl = true, alt = false))
        assertEquals("C-c", hardwareChord('C', ctrl = true, alt = false))
        assertEquals("M-b", hardwareChord('b', ctrl = false, alt = true))
        assertEquals("M-C-c", hardwareChord('c', ctrl = true, alt = true))
        assertEquals("C-[", hardwareChord('[', ctrl = true, alt = false))
        assertNull(hardwareChord('c', ctrl = false, alt = false))
    }
}
