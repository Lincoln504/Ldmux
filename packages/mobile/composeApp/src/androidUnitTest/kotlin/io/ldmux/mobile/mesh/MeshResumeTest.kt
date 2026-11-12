package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals

/**
 * Which shell a rebuilt or resumed connection comes back to.
 *
 * The value it returns lived in the screen as a `remember { mutableStateOf("") }` that nothing ever
 * assigned, and three separate behaviours read it: a rebuilt connection re-attaches it, a platform
 * release comes back to it, and the terminal surface is keyed on it. Because it was always empty, a
 * connection that came back asked for no shell at all.
 *
 * The damage was worse than a lost place. Leaving the app for ten seconds released the claim and
 * cleared the mirror while the page still believed it was attaching, so the return had no id to
 * attach, nothing to ask the host for, and the screen sat on "asking host for the shell" with no
 * action but leaving it. The rule below is the whole fix, and it is a pure function so the four
 * cases can be stated without a screen or a radio.
 */
class MeshResumeTest {
    @Test
    fun opening_a_shell_remembers_it() {
        // From the launcher: nothing was remembered, and the attach answers with the shell's id.
        assertEquals("s1", resumeShellFor(attached = "s1", onList = false, previous = ""))
        // A shell created from the phone lands the same way, after the host names it.
        assertEquals("s2", resumeShellFor(attached = "s2", onList = false, previous = ""))
    }

    @Test
    fun switching_pages_remembers_the_one_being_shown() {
        assertEquals("s2", resumeShellFor(attached = "s2", onList = false, previous = "s1"))
        assertEquals("s1", resumeShellFor(attached = "s1", onList = false, previous = "s2"))
    }

    @Test
    fun going_back_to_the_launcher_gives_the_place_up() {
        assertEquals("", resumeShellFor(attached = "s1", onList = true, previous = "s1"))
        // Even with nothing attached, standing on the list means the place is nobody's.
        assertEquals("", resumeShellFor(attached = null, onList = true, previous = "s1"))
    }

    @Test
    fun a_rebuild_between_the_tap_and_the_answer_keeps_the_shell_it_was_opening() {
        // `open` sets `Attached` and then waits for the host, and a link can drop in that window.
        // The id the user asked for is the one the next connection must resume.
        assertEquals("s3", resumeShellFor(attached = null, onList = false, previous = "s3"))
    }

    @Test
    fun a_first_connection_with_nothing_chosen_remembers_nothing() {
        assertEquals("", resumeShellFor(attached = null, onList = false, previous = ""))
    }
}
