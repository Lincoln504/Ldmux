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
        assertEquals(
            listOf("text:a", "key:Enter", "text:b", "key:Tab", "text:c"),
            ops("", "a\nb\tc"),
        )
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

    @Test
    fun the_shared_prefix_never_runs_past_either_string() {
        assertEquals(0, commonPrefixOf("", "abc"))
        assertEquals(0, commonPrefixOf("abc", ""))
        assertEquals(3, commonPrefixOf("abc", "abc"))
        assertTrue(inputOpsFor("abc", "").all { it is InputOp.Keys })
    }
}
