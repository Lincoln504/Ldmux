package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue

class ClipboardPolicyTest {
    @Test
    fun `a clear is always admitted`() {
        assertEquals(ClipboardRequest.Clear, admitClipboardRequest(ClipboardRequest.Clear))
    }

    @Test
    fun `an ordinary copy passes unchanged`() {
        val r = ClipboardRequest.Set("git status\n\tdone\r\n")
        assertEquals(r, admitClipboardRequest(r))
    }

    @Test
    fun `a copy at the cap is admitted and one over it is refused whole, not truncated`() {
        val atCap = ClipboardRequest.Set("x".repeat(MAX_CLIPBOARD_CHARS))
        assertEquals(atCap, admitClipboardRequest(atCap))
        // Refused entirely: a clipboard holding half of what the program said is worse
        // than one left as it was.
        assertNull(admitClipboardRequest(ClipboardRequest.Set("x".repeat(MAX_CLIPBOARD_CHARS + 1))))
    }

    @Test
    fun `control characters are dropped so the clipboard is safe to paste into a terminal later`() {
        val r = admitClipboardRequest(ClipboardRequest.Set("a\u0000b\u001b[31mc\u007fd\u0007e")) as ClipboardRequest.Set
        assertTrue('\u0000' !in r.text && '\u001b' !in r.text && '\u007f' !in r.text && '\u0007' !in r.text)
        assertEquals("ab[31mcde", r.text)
    }

    @Test
    fun `non-ascii text survives`() {
        val r = ClipboardRequest.Set("café 日本語 😀")
        assertEquals(r, admitClipboardRequest(r))
    }
}
