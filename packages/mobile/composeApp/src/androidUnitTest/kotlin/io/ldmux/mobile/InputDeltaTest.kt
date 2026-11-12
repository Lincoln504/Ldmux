package io.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The hidden input field's changes, read as differences.
 *
 * These exist because the previous mechanism lost characters and nothing caught it.
 * The case that matters most is the first one: a burst is what a paste is, what a
 * swipe is, and what `adb shell input text` is, and it is the one the old sentinel
 * reset destroyed — five of ten characters never reached the app, and the only
 * reason anyone found out was a device measurement.
 */
class InputDeltaTest {
    /** Readable form, so a failure says what the terminal would have been told. */
    private fun ops(previous: String, next: String): List<String> =
        inputOpsFor(previous, next).map {
            when (it) {
                is InputOp.Keys -> it.names.joinToString(",", prefix = "key:")
                is InputOp.Text -> "text:${it.value}"
                is InputOp.Paste -> "paste:${it.value}"
            }
        }

    private fun typed(previous: String, next: String): String =
        inputOpsFor(previous, next).filterIsInstance<InputOp.Text>().joinToString("") { it.value }

    @Test
    fun a_burst_of_characters_is_delivered_whole_and_in_order() {
        // The regression: this is exactly one `input text` call of ten characters,
        // and the old code delivered `acfhj` to the terminal.
        val seen = mutableListOf<String>()
        var field = ""
        for (ch in "abcdefghij") {
            seen += typed(field, field + ch)
            field += ch
        }
        assertEquals("abcdefghij", seen.joinToString(""))
    }

    @Test
    fun one_character_is_one_run_of_text() {
        assertEquals(listOf("text:a"), ops("", "a"))
    }

    @Test
    fun no_change_is_no_operations() {
        assertEquals(emptyList(), ops("abc", "abc"))
    }

    @Test
    fun a_shorter_value_is_a_backspace() {
        assertEquals(listOf("key:BSpace"), ops("abc", "ab"))
    }

    @Test
    fun several_characters_removed_are_several_backspaces() {
        assertEquals(listOf("key:BSpace,BSpace,BSpace"), ops("abcdef", "abc"))
    }

    @Test
    fun deletion_comes_before_the_text_that_replaced_it() {
        // Order is the whole point: a backspace sent after the text it was meant to
        // precede would delete the character just typed.
        assertEquals(listOf("key:BSpace", "text:x"), ops("abc", "abx"))
    }

    @Test
    fun enter_is_a_named_key_and_not_text() {
        assertEquals(listOf("text:ls", "key:Enter"), ops("", "ls\n"))
    }

    @Test
    fun tab_is_a_named_key_and_not_text() {
        // Tab must arrive as a real TAB, both for completion and so it is never
        // predicted locally.
        assertEquals(listOf("text:ec", "key:Tab"), ops("", "ec\t"))
    }

    @Test
    fun text_is_split_around_the_keys_inside_it() {
        // A tab is still a key inside a line: it must reach the pane as Tab so completion and a
        // program's own bindings see a real TAB, and so it can never be predicted locally.
        assertEquals(listOf("text:a", "key:Tab", "text:c"), ops("", "a\tc"))
        // **But a line end with text after it is not a key.** This exact string used to read as
        // `text:a`, `key:Enter`, `text:b`, `key:Tab`, `text:c` — five sends, and over the radio five
        // records a half-duplex repeater collides. It arrived as one change, so it is one paste, and
        // a paste is the only shape that can carry a block of lines without running them.
        assertEquals(listOf("paste:a\nb\tc"), ops("", "a\nb\tc"))
    }

    @Test
    fun a_deletion_and_a_key_in_one_change_keep_their_order() {
        assertEquals(listOf("key:BSpace", "key:Enter"), ops("x", "\n"))
    }

    @Test
    fun a_change_in_the_middle_is_read_from_the_shared_prefix() {
        // What a replacement looks like: the shared prefix ends where the two values
        // stop agreeing, so the tail of the old value goes and the tail of the new
        // one arrives.
        assertEquals(6, commonPrefixOf("hello world", "hello there"))
        assertEquals(listOf("key:BSpace,BSpace,BSpace,BSpace,BSpace", "text:there"), ops("hello world", "hello there"))
    }

    /**
     * **Why a synthesized burst counts as keys.** `adb shell input text` does not append to the
     * field: it rewrites it, so one injected character can arrive as "the old value is gone" and
     * then "the new value is here". The first half is a real deletion in the field, and a deletion
     * is a backspace — that is not a bug in the reading, it is the reading. A held-input summary
     * therefore says "14 characters and 120 keys" after `input text` even though nothing but text
     * was typed, and a real keyboard, which appends, does not produce it.
     */
    @Test
    fun a_rewritten_field_reads_as_deletions_and_that_is_the_truth() {
        // What the IME does for one injected character: the field goes from the old value to empty,
        // then to the new value.
        assertEquals(listOf("key:BSpace"), ops("a", ""))
        assertEquals(listOf("text:ab"), ops("", "ab"))
        // A whole burst in one change is text; nothing is deleted, so nothing is a key.
        assertTrue(inputOpsFor("", "abcdefghij").all { it is InputOp.Text })
        // And a burst with a line end in it is the one shape that is a paste, whatever produced it:
        // a paste is defined by carrying text across lines, not by where it came from.
        assertTrue(inputOpsFor("", "a\nb").all { it is InputOp.Paste })
    }

    // ── a block of text is a paste, not a run of Enter keys ─────────────────
    //
    // The IME reports a paste as one change to the field, and the delta used to walk it a character
    // at a time and turn every line end into an `Enter` key. On the radio route that ran the first
    // line of a paragraph as a command, one record per line, before the user had seen the rest; on
    // the tailnet route it broke bracketed paste for the same reason.

    @Test
    fun a_two_line_paste_is_one_paste_and_no_enter_keys() {
        val read = ops("", "echo one\necho two")
        assertEquals(listOf("paste:echo one\necho two"), read)
        assertTrue(read.none { it.startsWith("key:Enter") }, "a pasted line end is not a keypress")
        // The bound of the rule, stated once: text after a line end is what makes a change a paste.
        // One line and its terminator is still typing, because a program with bracketed paste would
        // otherwise be left with the line sitting unexecuted in its prompt.
        assertEquals(listOf("text:ls", "key:Enter"), ops("", "ls\n"))
    }

    @Test
    fun a_three_line_paste_keeps_every_newline() {
        assertEquals(
            listOf("paste:a\nb\nc"),
            ops("", "a\nb\nc"),
        )
    }

    @Test
    fun a_single_newline_is_still_the_enter_key() {
        // The IME's own Enter arrives as one character appended to the field. It is a keypress and
        // must reach the pane as one, so a program's own bindings see a real Return.
        assertEquals(listOf("key:Enter"), ops("", "\n"))
        assertEquals(listOf("key:Enter"), ops("", "\r"))
    }

    @Test
    fun a_crlf_paste_does_not_become_a_double_line_end() {
        // A clipboard on a platform that writes CRLF would otherwise paste a blank line between
        // every line: the host turns each `\n` into a carriage return for a pane without bracketed
        // paste, so `\r\n` arrives as two line ends.
        assertEquals(listOf("paste:a\nb\nc"), ops("", "a\r\nb\r\nc"))
    }

    @Test
    fun the_shared_prefix_never_runs_past_either_string() {
        assertEquals(0, commonPrefixOf("", "abc"))
        assertEquals(0, commonPrefixOf("abc", ""))
        assertEquals(3, commonPrefixOf("abc", "abc"))
        assertTrue(inputOpsFor("abc", "").all { it is InputOp.Keys })
    }
}
