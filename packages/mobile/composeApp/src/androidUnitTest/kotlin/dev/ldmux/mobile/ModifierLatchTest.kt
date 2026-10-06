package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** The sticky latch lapses: an armed Ctrl nobody used must not chord a later keystroke. */
class ModifierLatchTest {
    private var now = 0L
    private fun latch(timeout: Long = 1000) = ModifierLatch(timeout) { now }

    @Test
    fun `an armed modifier is live until the timeout and gone after it`() {
        now = 0
        val l = latch()
        l.toggle(StickyKey.Ctrl)
        now = 999
        assertTrue(l.current().ctrl)
        now = 1000
        assertFalse(l.current().armed)
    }

    @Test
    fun `arming another key restarts the clock for both`() {
        val l = latch()
        l.toggle(StickyKey.Ctrl)
        now = 800
        l.toggle(StickyKey.Alt)
        now = 1500
        assertEquals(Modifiers(ctrl = true, alt = true), l.current())
        now = 1800
        assertFalse(l.current().armed)
    }

    @Test
    fun `toggling after expiry arms only the new key`() {
        val l = latch()
        l.toggle(StickyKey.Ctrl)
        now = 5000
        assertEquals(Modifiers(alt = true), l.toggle(StickyKey.Alt))
    }

    @Test
    fun `toggling the same key off disarms it`() {
        val l = latch()
        l.toggle(StickyKey.Ctrl)
        assertEquals(Modifiers(), l.toggle(StickyKey.Ctrl))
        assertNull(l.remainingMs())
    }

    @Test
    fun `clear forgets everything and remaining time counts down`() {
        val l = latch()
        l.toggle(StickyKey.Alt)
        now = 400
        assertEquals(600L, l.remainingMs())
        l.clear()
        assertFalse(l.current().armed)
        assertNull(l.remainingMs())
    }
}
