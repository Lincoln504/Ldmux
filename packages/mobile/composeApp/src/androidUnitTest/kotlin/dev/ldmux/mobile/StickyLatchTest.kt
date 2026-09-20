package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The reported bug: arming Ctrl and then *typing* `q` sent a plain `q` and left Ctrl
 * armed for whatever key came next. Both halves had one cause — the latch was read
 * and spent only by the key bars, and a typed character never goes through them.
 *
 * These pin the rule at the level where it now lives, and they pin the numbers,
 * because a Ctrl-Q that is not 0x11 is the same class of bug as a Ctrl-C that is not
 * 0x03. `spendLatchOnFirst` is pure, so this needs no socket and no device — which is
 * the whole point: the old rule lived inside the chrome and could not be reached by a
 * test at all.
 */
class StickyLatchTest {
    /** The bytes that actually reach the pty for the keystroke's first character. */
    private fun firstBytes(text: String, armed: Modifiers): String {
        val t = spendLatchOnFirst(text, armed)
        if (t.head.isEmpty()) return ""
        return when (val k = modifiedKey(t.head, t.headModifiers)) {
            is ModifiedKey.Literal -> k.bytes
            is ModifiedKey.Named -> k.name
            is ModifiedKey.None -> ""
        }
    }

    @Test
    fun `arming ctrl then typing q sends 0x11, not a q`() {
        // The name of this test is the assertion: 0x11 is Ctrl-Q, and it is emphatically
        // not the character `q`. There was a second line here asserting `"q" != ...`,
        // which the `assertEquals` above already entails and which therefore could never
        // be the thing that failed. A redundant assertion is not extra safety; it is a
        // number in the count that means nothing.
        assertEquals("\u0011", firstBytes("q", Modifiers(ctrl = true)))
    }

    @Test
    fun `arming alt then typing q sends the escape prefix`() {
        assertEquals("\u001Bq", firstBytes("q", Modifiers(alt = true)))
    }

    @Test
    fun `both armed gives the alt-ctrl byte, not a stack of two chords`() {
        assertEquals("\u001B\u0011", firstBytes("q", Modifiers(ctrl = true, alt = true)))
    }

    @Test
    fun `the latch is spent by the first keystroke`() {
        val t = spendLatchOnFirst("q", Modifiers(ctrl = true))
        assertFalse(t.latchAfter.armed, "controlling one key must not leave Ctrl armed")
    }

    @Test
    fun `the rest of a typed burst is plain text`() {
        val t = spendLatchOnFirst("quit", Modifiers(ctrl = true))
        assertEquals("q", t.head)
        assertEquals("uit", t.rest)
        assertEquals(Modifiers(ctrl = true), t.headModifiers)
        assertFalse(t.latchAfter.armed)
    }

    @Test
    fun `a second keystroke is never modified`() {
        // Simulates typing `q` then `b`: the second call is made with what the first
        // one left behind, which is what the controller does.
        val first = spendLatchOnFirst("q", Modifiers(ctrl = true))
        val second = spendLatchOnFirst("b", first.latchAfter)
        assertEquals("", second.head)
        assertEquals("b", second.rest)
    }

    @Test
    fun `pressing ctrl twice is off again, not two ctrls`() {
        // `toggled` is a toggle, not an accumulate: a second tap disarms. That is the
        // property that makes a stacked `C-C-q` unrepresentable in the first place,
        // and it is why this latch needs no counter and no queue — there is no depth
        // for anything to stack in. (This expectation was wrong when first written:
        // it asserted that two taps stayed armed.)
        var latch = Modifiers().toggled(StickyKey.Ctrl)
        assertTrue(latch.ctrl)
        latch = latch.toggled(StickyKey.Ctrl)
        assertFalse(latch.ctrl)
        assertEquals(Modifiers(), latch)
    }

    @Test
    fun `a non-ascii first character has no chord and still spends the latch`() {
        val t = spendLatchOnFirst("\uD83D\uDE00x", Modifiers(ctrl = true))
        assertEquals("", t.head)
        assertEquals("\uD83D\uDE00x", t.rest)
        assertFalse(t.latchAfter.armed)
    }

    @Test
    fun `with nothing armed the whole text is ordinary`() {
        val t = spendLatchOnFirst("quit", Modifiers())
        assertEquals("", t.head)
        assertEquals("quit", t.rest)
        assertFalse(t.latchAfter.armed)
    }

    @Test
    fun `an empty keystroke spends nothing`() {
        val t = spendLatchOnFirst("", Modifiers(ctrl = true))
        assertEquals("", t.head)
        assertEquals("", t.rest)
        // Nothing was typed, so the arming was not for nothing: it is still there.
        assertEquals(Modifiers(ctrl = true), t.latchAfter)
    }

    @Test
    fun `typed text is characters, so only the first letter is chorded`() {
        // This replaces an assumption that was simply wrong: `spendLatchOnFirst` is
        // about *typed text*, not key names, so "Escape" typed is the letter E and
        // then "scape". Named keys never come through here — they go through sendKey
        // and `modifiedKey`, which is what `ModifiersTest` already pins.
        val t = spendLatchOnFirst("Escape", Modifiers(ctrl = true))
        assertEquals("E", t.head)
        assertEquals("scape", t.rest)
        assertFalse(t.latchAfter.armed)
    }
}
