package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
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

    /**
     * A control key is written `C/c`.
     *
     * `C-c` is tmux's name for the key, not a way to write it down: the dash reads as
     * a range or a minus, and the key is "control, then c". Both bars show these, so
     * the label is derived from the name in one place rather than typed beside it in
     * two — which is how the panel and the portrait bar came to disagree.
     */
    /**
     * Ctrl-D is not offered anywhere, on purpose.
     *
     * It was on both bars and it was asked for by name to be taken out. Ctrl-D sends
     * end-of-file to the shell, which closes the pane: a key that ends a session is
     * the last thing that should sit on a bar pressed without looking. The reason it
     * is easy to leave in is that it looks like Ctrl-C, which is why this asserts the
     * absence rather than trusting the list to stay right.
     *
     * **It reads the functions `App.kt` renders from, and that is the whole point.**
     * It used to read `SIDEBAR_KEYS`, a constant that had exactly two references in
     * the repository — its own definition and this test — because both bars had grown
     * their own inline literals. Adding `C/d` to either bar therefore left this green.
     * A guard on a list nothing draws reads as protection and is not.
     */
    @Test
    fun theKeyListOffersNoControlD() {
        val press: (String) -> Unit = {}
        val portrait = portraitControlKeys({}, press).map { it.first }
        val panel = panelControlKeys(press).map { it.first }

        // The two bars are checked separately, because they are two lists and the
        // failure this guards against is one of them being fixed and the other not.
        for ((where, labels) in listOf("portrait" to portrait, "panel" to panel)) {
            assertTrue(labels.contains("C/c"), "the $where bar should still offer C/c: $labels")
            assertFalse(labels.contains("C/d"), "the $where bar offers C/d again: $labels")
        }
        // And the lists are not empty, so a `contains` on an empty list cannot be
        // what makes the assertions above pass. A guard that passes because it is
        // looking at nothing is the failure mode this test was written while having.
        assertEquals(7, portrait.size)
        assertEquals(4, panel.size)
    }

    @Test
    fun controlKeysReadWithASlash() {
        assertEquals("C/c", controlLabel("C-c"))
        assertEquals("C/d", controlLabel("C-d"))
        // The wire name is untouched: this is a label, not a translation.
        assertFalse(controlLabel("C-c").contains("-"))
        // Anything that is not a control key is itself, so the helper can be applied
        // to a whole row without special-casing.
        assertEquals("Tab", controlLabel("Tab"))
        assertEquals("|", controlLabel("|"))
    }
}
