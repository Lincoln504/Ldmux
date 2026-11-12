package io.ldmux.mobile.mesh

import io.ldmux.mobile.StickyKey
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The draft: what is typed but not sent, and the command it becomes.
 *
 * **The old queue's tests went with the old queue.** `MeshInputBatcher` coalesced keystrokes into
 * one record pieces because one keystroke was one record; the v15 engine sends the whole draft as
 * one message however long it is, so the windows, the pacing and the pump have no subject left
 * (`docs/RADIO-PLAN.md` §6.1, §7). What is worth testing is what remains: the order of the items,
 * the limits the wire imposes, and the fact that a refused keystroke is refused whole.
 */
class MeshDraftTest {
    private var now = 1_000L
    private fun handle(draft: MeshDraft = MeshDraft()) =
        draft to MeshTerminalHandle(draft, clock = { now })

    @Test
    fun `typed text and a key stay in the order they were typed`() {
        val (draft, h) = handle()
        h.sendText("ls -la")
        h.sendKey("Enter")
        val items = draft.peek()
        assertEquals(2, items.size)
        assertContentEquals("ls -la".encodeToByteArray(), (items[0] as DraftItem.Text).bytes)
        assertEquals(listOf("Enter"), (items[1] as DraftItem.Keys).names)
    }

    @Test
    fun `consecutive keys share one item, so a chord is one thing on the wire`() {
        val (draft, h) = handle()
        h.sendKey("Tab")
        h.sendKey("Enter")
        assertEquals(1, draft.size, "two keys are one Keys item: ${draft.peek()}")
        assertEquals(listOf("Tab", "Enter"), (draft.peek()[0] as DraftItem.Keys).names)
    }

    @Test
    fun `a key after text does not join the text`() {
        val (draft, h) = handle()
        h.sendText("cd ")
        h.sendKey("Bslash")
        h.sendText("tmp")
        val kinds = draft.peek().map { it::class.simpleName }
        assertEquals(listOf("Text", "Keys", "Text"), kinds)
    }

    @Test
    fun `an armed Ctrl is spent by the next key and not by the one after it`() {
        val (draft, h) = handle()
        h.toggleModifier(StickyKey.Ctrl)
        h.sendKey("c")
        h.sendKey("c")
        val keys = draft.peek().filterIsInstance<DraftItem.Keys>().flatMap { it.names }
        assertEquals(listOf("C-c", "c"), keys)
        assertFalse(h.modifiers.armed)
    }

    @Test
    fun `a paste keeps its newlines, and CRLF is folded so no blank line appears`() {
        val (draft, h) = handle()
        assertTrue(h.paste("one\r\ntwo\nthree"))
        val paste = draft.peek().single() as DraftItem.Paste
        assertEquals("one\ntwo\nthree", paste.bytes.decodeToString())
        assertEquals(DraftItem.Paste::class, paste::class)
    }

    @Test
    fun `a pasted command is a paste and not a run of Enter keys`() {
        val (draft, h) = handle()
        h.paste("echo hi\necho bye")
        val items = draft.peek()
        assertEquals(1, items.size, "the newlines are inside the paste: ${items}")
        assertTrue(items[0] is DraftItem.Paste)
    }

    @Test
    fun `a run longer than one item is split at a character boundary and rejoins whole`() {
        val (draft, h) = handle()
        val text = "é".repeat(MeshDraft.MAX_ITEM_BYTES) // two bytes each, so a naive cut splits one
        h.sendText(text)
        val items = draft.peek().filterIsInstance<DraftItem.Text>()
        assertTrue(items.size > 1, "a run past the item limit becomes several items")
        for (item in items) {
            assertTrue(item.bytes.size <= MeshDraft.MAX_ITEM_BYTES)
            // A cut between a lead byte and its continuation would decode with a replacement char.
            assertFalse(item.bytes.decodeToString().contains('\uFFFD'), "an item split mid-character")
        }
        assertEquals(text, items.joinToString("") { it.bytes.decodeToString() })
    }

    @Test
    fun `a draft at the item limit refuses the next thing whole`() {
        val (draft, h) = handle()
        // Alternating text and keys is the only way to reach the item limit: consecutive keys share
        // one item, so sixteen keys are one item and sixteen text runs are sixteen.
        repeat(MeshDraft.MAX_ITEMS / 2) {
            h.sendText("x")
            h.sendKey("Up")
        }
        assertEquals(MeshDraft.MAX_ITEMS, draft.size)
        val before = draft.peek().size
        assertFalse(h.paste("y"), "the draft is at the wire's item limit")
        assertTrue(h.overflow)
        assertEquals(before, draft.peek().size, "and a refused item changes nothing")
    }

    @Test
    fun `scrub takes everything and says nothing is held`() {
        val (draft, h) = handle()
        h.sendText("password")
        assertTrue(h.hasPending)
        h.scrub()
        assertFalse(h.hasPending)
        assertTrue(draft.isEmpty)
        assertEquals("", h.echoText)
    }

    @Test
    fun `the label shows a line end and a tab as glyphs, and counts text and keys apart`() {
        val (_, h) = handle()
        h.sendText("make")
        h.sendKey("Enter")
        assertEquals("make⏎", h.pendingLabel)
        assertEquals("4 characters and 1 key", h.heldSummary)

        val (_, t) = handle()
        t.sendKey("Tab")
        assertEquals("⇥", t.pendingLabel)
    }

    @Test
    fun `the echo is the characters, so an arrow key contributes nothing but is still counted`() {
        val (_, h) = handle()
        h.sendText("ab")
        h.sendKey("Enter")
        assertEquals("ab\r\n", h.echoText)
        h.sendKey("Up")
        assertEquals("ab\r\n", h.echoText, "an arrow key is a request, not a character")
        assertEquals("2 characters and 2 keys", h.heldSummary)
    }

    // ── the item stream the ABI carries ─────────────────────────────────────────────────────

    /**
     * **The stream is the wire's own item format**, so the library decodes what the app built with
     * the same function the `Send` message uses. `mesh_term`'s
     * `the_abi_item_stream_is_the_wire_s_item_stream` pins the other end of this.
     */
    @Test
    fun `the item stream is text, keys and paste in composition order`() {
        val stream = ItemStream.encode(
            listOf(
                DraftItem.Text("ls".encodeToByteArray()),
                DraftItem.Keys(listOf("Enter")),
                DraftItem.Paste("a\nb".encodeToByteArray()),
            ),
        )
        // [tag 1][len u16 LE][bytes] [tag 3][count][len][name] [tag 2][len u16 LE][bytes]
        val expected = buildList {
            addAll(listOf(0x01, 0x02, 0x00)); addAll("ls".encodeToByteArray().toList())
            addAll(listOf(0x03, 0x01, 0x05)); addAll("Enter".encodeToByteArray().toList())
            addAll(listOf(0x02, 0x03, 0x00)); addAll("a\nb".encodeToByteArray().toList())
        }.map { it.toByte() }.toByteArray()
        assertContentEquals(expected, stream)
    }

    @Test
    fun `a long text run is one item with its length stated in two bytes`() {
        val bytes = ByteArray(300) { 'a'.code.toByte() }
        val stream = ItemStream.encode(listOf(DraftItem.Text(bytes)))
        assertEquals(0x01, stream[0].toInt())
        assertEquals(300, (stream[1].toInt() and 0xFF) or ((stream[2].toInt() and 0xFF) shl 8))
        assertEquals(3 + 300, stream.size)
    }
}
