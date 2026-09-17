package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The sticky modifiers produce raw bytes for the pty, so these check the
 * *numbers*, not that a string came out. Ctrl-C that is not 0x03 is a bug no
 * amount of "it sent something" will catch.
 */
class ModifiersTest {
    private fun literal(key: String, mods: Modifiers): String {
        val r = modifiedKey(key, mods)
        assertTrue(r is ModifiedKey.Literal, "expected literal for $key, got $r")
        return (r as ModifiedKey.Literal).bytes
    }

    private fun named(key: String, mods: Modifiers): String {
        val r = modifiedKey(key, mods)
        assertTrue(r is ModifiedKey.Named, "expected name for $key, got $r")
        return (r as ModifiedKey.Named).name
    }

    @Test
    fun `with nothing armed a character is itself`() {
        assertEquals("a", literal("a", Modifiers()))
        assertEquals("|", literal("|", Modifiers()))
    }

    @Test
    fun `with nothing armed a named key stays a name`() {
        assertEquals("Enter", named("Enter", Modifiers()))
        assertEquals("Left", named("Left", Modifiers()))
    }

    @Test
    fun `ctrl letters are the C0 bytes a keyboard would have sent`() {
        assertEquals("\u0001", literal("a", Modifiers(ctrl = true)))
        assertEquals("\u0003", literal("c", Modifiers(ctrl = true)))
        assertEquals("\u0004", literal("d", Modifiers(ctrl = true)))
        assertEquals("\u001a", literal("z", Modifiers(ctrl = true)))
        // Uppercase is the same chord, not a different one.
        assertEquals("\u0003", literal("C", Modifiers(ctrl = true)))
    }

    @Test
    fun `the punctuation control codes are the classic ones, not guesses`() {
        // Ctrl-[ is ESC and Ctrl-? is DEL; neither is derivable from the letter
        // range, so they are the entries most worth pinning down.
        assertEquals("\u001b", literal("[", Modifiers(ctrl = true)))
        assertEquals("\u007f", literal("?", Modifiers(ctrl = true)))
        assertEquals("\u0000", literal("@", Modifiers(ctrl = true)))
        assertEquals("\u0000", literal(" ", Modifiers(ctrl = true)))
        assertEquals("\u001c", literal("\\", Modifiers(ctrl = true)))
        assertEquals("\u001d", literal("]", Modifiers(ctrl = true)))
        assertEquals("\u001e", literal("^", Modifiers(ctrl = true)))
        assertEquals("\u001f", literal("_", Modifiers(ctrl = true)))
    }

    @Test
    fun `alt prepends ESC, the way a terminal does`() {
        assertEquals("\u001bb", literal("b", Modifiers(alt = true)))
        assertEquals("\u001b|", literal("|", Modifiers(alt = true)))
    }

    @Test
    fun `ctrl and alt together are ESC then the control byte`() {
        // Alt-Ctrl-c is ESC 0x03: ESC first, because Alt is the outer escape.
        assertEquals("\u001b\u0003", literal("c", Modifiers(ctrl = true, alt = true)))
    }

    @Test
    fun `a control byte that does not exist drops the ctrl rather than inventing one`() {
        // Ctrl-| has no C0 assignment. Typing `|` is a plausible intent;
        // inventing a byte is not.
        assertEquals("|", literal("|", Modifiers(ctrl = true)))
        assertEquals("\u001b|", literal("|", Modifiers(ctrl = true, alt = true)))
    }

    @Test
    fun `named keys carry the modifier in the name`() {
        assertEquals("C-Left", named("Left", Modifiers(ctrl = true)))
        assertEquals("M-Up", named("Up", Modifiers(alt = true)))
        assertEquals("M-C-Right", named("Right", Modifiers(ctrl = true, alt = true)))
        assertEquals("C-Enter", named("Enter", Modifiers(ctrl = true)))
        assertEquals("C-Escape", named("Escape", Modifiers(ctrl = true)))
    }

    @Test
    fun `a key that is already modified is rebuilt, not stacked`() {
        // The sidebar ships `C-c` as a key in its own right. With Alt armed that
        // should be M-C-c — `C-C-c` is not a key and tmux would reject it.
        assertEquals("M-C-c", named("C-c", Modifiers(ctrl = true, alt = true)))
        assertEquals("C-c", named("C-c", Modifiers(ctrl = true)))
        assertEquals("M-c", named("C-c", Modifiers(alt = true)))
    }

    @Test
    fun `arming is a toggle and clearing disarms both`() {
        var m = Modifiers().toggled(StickyKey.Ctrl)
        assertTrue(m.ctrl && !m.alt)
        m = m.toggled(StickyKey.Alt)
        assertTrue(m.ctrl && m.alt)
        m = m.toggled(StickyKey.Ctrl)
        assertTrue(!m.ctrl && m.alt)
        assertTrue(!m.cleared().armed)
    }

    @Test
    fun `an empty key sends nothing`() {
        assertEquals(ModifiedKey.None, modifiedKey("", Modifiers(ctrl = true)))
    }
}
