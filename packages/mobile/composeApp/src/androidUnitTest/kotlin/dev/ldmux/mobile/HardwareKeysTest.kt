package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * Hardware keyboard mapping: AltGr, modifiers on named keys, and who owns text.
 */
class HardwareKeysTest {
    private fun key(
        named: String? = null,
        shift: Boolean = false,
        ctrl: Boolean = false,
        alt: Boolean = false,
        rightAlt: Boolean = false,
        meta: Boolean = false,
        withAlt: Char? = null,
        without: Char? = withAlt,
        softKeyboard: Boolean = false,
    ) = mapHardwareKey(
        HardwareKey(
            named = named,
            shift = shift,
            ctrl = ctrl,
            alt = alt || rightAlt,
            rightAlt = rightAlt,
            meta = meta,
            charWithAlt = withAlt?.code ?: 0,
            charWithoutAlt = without?.code ?: 0,
            softKeyboardUp = softKeyboard,
        ),
    )

    // ---- AltGr ---------------------------------------------------------------------

    @Test
    fun `altgr q on a european layout types at, it is not meta-q`() {
        // The right Alt is down and the layout says this key produces '@' with it.
        assertEquals(HardwareAction.Type("@"), key(rightAlt = true, withAlt = '@', without = 'q'))
    }

    @Test
    fun `altgr composes the other layout symbols`() {
        for (c in "{}[]\\|~€") {
            assertEquals(HardwareAction.Type(c.toString()), key(rightAlt = true, withAlt = c, without = 'x'), "AltGr $c")
        }
    }

    @Test
    fun `alt f whose character does not change is a chord`() {
        assertEquals(HardwareAction.Press("M-f"), key(alt = true, withAlt = 'f', without = 'f'))
        // Even on the right Alt: same character with or without it means no composition.
        assertEquals(HardwareAction.Press("M-f"), key(rightAlt = true, withAlt = 'f', without = 'f'))
    }

    @Test
    fun `a left alt stays a chord even where a generic key map gives it a symbol`() {
        assertEquals(HardwareAction.Press("M-q"), key(alt = true, withAlt = '`', without = 'q'))
    }

    @Test
    fun `altgr while the soft keyboard owns text is left to it`() {
        assertEquals(HardwareAction.Pass, key(rightAlt = true, withAlt = '@', without = 'q', softKeyboard = true))
    }

    // ---- named keys carry their modifiers ------------------------------------------

    @Test
    fun `ctrl left is C-Left, not a bare Left`() {
        assertEquals(HardwareAction.Press("C-Left"), key(named = "Left", ctrl = true))
        assertEquals(HardwareAction.Press("M-Up"), key(named = "Up", alt = true))
        assertEquals(HardwareAction.Press("M-C-Right"), key(named = "Right", ctrl = true, alt = true))
        assertEquals(HardwareAction.Press("C-Home"), key(named = "Home", ctrl = true))
        assertEquals(HardwareAction.Press("C-F5"), key(named = "F5", ctrl = true))
    }

    @Test
    fun `an unmodified named key is just its name`() {
        assertEquals(HardwareAction.Press("Left"), key(named = "Left"))
        assertEquals(HardwareAction.Press("Enter"), key(named = "Enter"))
        // Shift on a named key is not forwarded: the daemon has no shift modifier to name.
        assertEquals(HardwareAction.Press("Left"), key(named = "Left", shift = true))
    }

    @Test
    fun `shift tab is back-tab whatever else is held`() {
        assertEquals(HardwareAction.Press("BTab"), key(named = "Tab", shift = true))
        assertEquals(HardwareAction.Press("BTab"), key(named = "Tab", shift = true, ctrl = true))
    }

    @Test
    fun `ctrl on enter and escape is not named`() {
        assertEquals(HardwareAction.Press("Enter"), key(named = "Enter", ctrl = true))
        assertEquals(HardwareAction.Press("Escape"), key(named = "Escape", ctrl = true))
        assertEquals(HardwareAction.Press("M-Escape"), key(named = "Escape", alt = true))
    }

    // ---- characters ----------------------------------------------------------------

    @Test
    fun `a plain character is typed when there is no soft keyboard`() {
        assertEquals(HardwareAction.Type("a"), key(withAlt = 'a'))
        assertEquals(HardwareAction.Type("A"), key(shift = true, withAlt = 'A'))
    }

    @Test
    fun `a plain character is the text field's while the soft keyboard is up`() {
        assertEquals(HardwareAction.Pass, key(withAlt = 'a', softKeyboard = true))
    }

    @Test
    fun `ctrl chords are ours even with the soft keyboard up`() {
        assertEquals(HardwareAction.Press("C-c"), key(ctrl = true, withAlt = 'c', softKeyboard = true))
        assertEquals(HardwareAction.Press("C-Space"), key(ctrl = true, withAlt = ' '))
        assertEquals(HardwareAction.Press("C-["), key(ctrl = true, withAlt = '['))
        assertEquals(HardwareAction.Press("M-C-c"), key(ctrl = true, alt = true, withAlt = 'c'))
    }

    @Test
    fun `modifier-only and dead-key events are not input`() {
        assertEquals(HardwareAction.Pass, key(ctrl = true))
        assertEquals(HardwareAction.Pass, key(shift = true))
        assertEquals(HardwareAction.Pass, key(meta = true, withAlt = 'a'))
        // A dead key sets the combining-accent bit, which reads as a huge/negative code point.
        val dead = mapHardwareKey(HardwareKey(null, false, false, false, false, false, 0x80000000.toInt() or '`'.code, 0, false))
        assertEquals(HardwareAction.Pass, dead)
    }

    @Test
    fun `control codes and unmapped keys are not typed`() {
        assertEquals(HardwareAction.Pass, key())
        val ctl = mapHardwareKey(HardwareKey(null, false, false, false, false, false, 0x07, 0x07, false))
        assertEquals(HardwareAction.Pass, ctl)
    }
}
