package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The sticky modifiers produce protocol key names, so these check the exact name, not
 * that a string came out. A Ctrl-C that is not `C-c` is a bug no amount of "it
 * sent something" will catch.
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

    /**
     * A modified character goes by name, never as the byte.
     *
     * The client cannot know which encoding the program on the pty wants (the
     * legacy byte, or an extended-key form), so the daemon owns the name-to-bytes
     * table. Nothing here may regress to a [ModifiedKey.Literal] for a chord.
     */
    @Test
    fun `ctrl on a letter is the key name, not the C0 byte`() {
        assertEquals("C-a", named("a", Modifiers(ctrl = true)))
        assertEquals("C-c", named("c", Modifiers(ctrl = true)))
        assertEquals("C-z", named("z", Modifiers(ctrl = true)))
        // Uppercase is the same chord, not a different one — and `C-A` would reach
        // an extended-keys program as a different key.
        assertEquals("C-c", named("C", Modifiers(ctrl = true)))
    }

    @Test
    fun `the punctuation control chords keep their classic keys`() {
        // The daemon turns these into ESC, DEL, NUL, 0x1c-0x1f.
        assertEquals("C-[", named("[", Modifiers(ctrl = true)))
        assertEquals("C-?", named("?", Modifiers(ctrl = true)))
        assertEquals("C-@", named("@", Modifiers(ctrl = true)))
        assertEquals("C-Space", named(" ", Modifiers(ctrl = true)))
        assertEquals("C-\\", named("\\", Modifiers(ctrl = true)))
        assertEquals("C-]", named("]", Modifiers(ctrl = true)))
        assertEquals("C-^", named("^", Modifiers(ctrl = true)))
        assertEquals("C-_", named("_", Modifiers(ctrl = true)))
    }

    @Test
    fun `alt on a character is the meta name`() {
        assertEquals("M-b", named("b", Modifiers(alt = true)))
        assertEquals("M-|", named("|", Modifiers(alt = true)))
        // Alt keeps case: Alt-B is not Alt-b.
        assertEquals("M-B", named("B", Modifiers(alt = true)))
        assertEquals("M-Space", named(" ", Modifiers(alt = true)))
    }

    @Test
    fun `ctrl and alt together are one name, meta first`() {
        // `M-C-c` is the protocol name for ESC 0x03.
        assertEquals("M-C-c", named("c", Modifiers(ctrl = true, alt = true)))
    }

    @Test
    fun `a ctrl chord that does not exist drops the ctrl rather than inventing one`() {
        // Ctrl-| has no C0 assignment. Typing `|` is a plausible intent;
        // inventing a key is not.
        assertEquals("|", literal("|", Modifiers(ctrl = true)))
        assertEquals("M-|", named("|", Modifiers(ctrl = true, alt = true)))
    }

    @Test
    fun `a character outside ASCII goes out as itself`() {
        assertEquals("é", literal("é", Modifiers(ctrl = true)))
    }

    @Test
    fun `named keys carry the modifier in the name`() {
        assertEquals("C-Left", named("Left", Modifiers(ctrl = true)))
        assertEquals("M-Up", named("Up", Modifiers(alt = true)))
        assertEquals("M-C-Right", named("Right", Modifiers(ctrl = true, alt = true)))
    }

    /**
     * Ctrl means nothing on Escape and Enter — the daemon sends ESC and CR whatever the
     * modifier — so a latched Ctrl is spent by them *without* being named: `C-Escape` and
     * `C-Enter` are not keys any program waits for. Alt still applies (ESC-prefixed).
     */
    @Test
    fun `a latched ctrl is spent by escape and enter without being named`() {
        assertEquals("Enter", named("Enter", Modifiers(ctrl = true)))
        assertEquals("Escape", named("Escape", Modifiers(ctrl = true)))
        assertEquals("M-Enter", named("Enter", Modifiers(ctrl = true, alt = true)))
        assertEquals("M-Escape", named("Escape", Modifiers(alt = true)))
        // ...and Tab keeps its Ctrl, which the daemon does not ignore.
        assertEquals("C-Tab", named("Tab", Modifiers(ctrl = true)))
    }

    @Test
    fun `a key that is already modified is rebuilt, not stacked`() {
        // The sidebar ships `C-c` as a key in its own right. With Alt armed that
        // should be M-C-c — `C-C-c` is not a key and the daemon would reject it.
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
     * `C-c` is the protocol's name for the key, not a way to write it down: the dash reads as
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
