package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.StickyKey
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** Keystroke coalescing: one record per piece, never one per key, never silent, order kept. */
class MeshInputTest {
    private fun text(s: String) = MeshOut.Text(s.encodeToByteArray())
    private fun bytes(s: String) = s.encodeToByteArray()

    @Test
    fun `typing waits for quiet, then leaves as one piece`() {
        val b = MeshInputBatcher()
        b.appendText(bytes("l"), 0); b.appendText(bytes("s"), 100); b.appendText(bytes(" -l"), 200)
        assertNull(b.peek(300), "still typing")
        assertNull(b.peek(200 + MeshInputBatcher.IDLE_MS - 1))
        assertEquals(text("ls -l"), b.peek(200 + MeshInputBatcher.IDLE_MS))
    }

    @Test
    fun `a line end leaves at once, and so does an interrupt, as text or as a named key`() {
        val b = MeshInputBatcher()
        b.appendText(bytes("ls"), 0); b.appendText(byteArrayOf(0x0D), 10)
        assertEquals(text("ls\r"), b.peek(10))
        b.accepted(b.peek(10)!!)
        assertTrue(b.isEmpty)
        b.appendText(byteArrayOf(0x03), 20)
        assertTrue(b.due(20))
        b.accepted(b.peek(20)!!)
        b.appendKey("Enter", 30)
        assertTrue(b.due(30), "Enter is a finished thought")
        b.accepted(b.peek(30)!!)
        b.appendKey("C-c", 40)
        assertTrue(b.due(40), "an interrupt that waits is not one")
        b.accepted(b.peek(40)!!)
        b.appendKey("Up", 50)
        assertFalse(b.due(50), "an arrow is not urgent")
    }

    @Test
    fun `steady typing still goes out after the maximum hold`() {
        val b = MeshInputBatcher()
        var t = 0L
        while (t < MeshInputBatcher.MAX_HOLD_MS) { b.appendText(bytes("x"), t); t += 300 }
        assertTrue(b.due(t), "the hold is bounded though it never went quiet for 700 ms")
    }

    @Test
    fun `a full record leaves at once and is never over the cap, nor splits a character`() {
        val b = MeshInputBatcher()
        b.appendText(ByteArray(95) { 'a'.code.toByte() } + bytes("é") + bytes("zz"), 0)
        val first = b.peek(0) as MeshOut.Text
        assertTrue(first.bytes.size <= MeshTerm.MAX_INPUT)
        assertEquals(95, first.bytes.size, "the two-byte character is not cut in half")
        b.accepted(first)
        assertEquals(listOf<MeshOut>(text("ézz")), b.pending)
    }

    @Test
    fun `text and keys stay in the order they were typed and leave as separate pieces`() {
        val b = MeshInputBatcher()
        b.appendText(bytes("ls"), 0); b.appendKey("Enter", 1); b.appendKey("Up", 2); b.appendText(bytes("x"), 3)
        assertEquals(
            listOf(text("ls"), MeshOut.Keys(listOf("Enter", "Up")), text("x")),
            b.pending,
        )
        val order = ArrayList<MeshOut>()
        while (true) { val p = b.peek(10_000) ?: break; order += p; b.accepted(p) }
        assertEquals(b.pending, emptyList())
        assertEquals(listOf(text("ls"), MeshOut.Keys(listOf("Enter", "Up")), text("x")), order)
    }

    @Test
    fun `keys coalesce into one piece up to the name and size caps`() {
        val b = MeshInputBatcher()
        repeat(MeshTerm.MAX_KEYS + 2) { b.appendKey("Up", it.toLong()) }
        val pieces = b.pending.map { (it as MeshOut.Keys).names.size }
        assertEquals(listOf(MeshTerm.MAX_KEYS, 2), pieces)
        val big = MeshInputBatcher()
        repeat(10) { big.appendKey("M-C-S-Left", 0) }
        for (piece in big.pending) {
            val body = (piece as MeshOut.Keys).names.joinToString(" ").length
            assertTrue(body <= MeshTerm.MAX_INPUT, "a Key body of $body bytes would not encode")
            TermMessage.Key(piece.names).encode()
        }
    }

    @Test
    fun `a backspace over a waiting character costs nothing on the air`() {
        val b = MeshInputBatcher()
        b.appendText(bytes("lss"), 0); b.appendKey("BSpace", 5)
        assertEquals(listOf<MeshOut>(text("ls")), b.pending)
        val e = MeshInputBatcher()
        e.appendKey("BSpace", 0)
        assertEquals(listOf<MeshOut>(MeshOut.Keys(listOf("BSpace"))), e.pending, "nothing waiting to cancel: it is sent")
        val k = MeshInputBatcher()
        k.appendText(bytes("a"), 0); k.appendKey("Up", 1); k.appendKey("BSpace", 2)
        assertEquals(2, k.pending.size, "a key between: the backspace is the host's, not a correction")
    }

    @Test
    fun `more than can wait is refused whole`() {
        val b = MeshInputBatcher(capacity = 8)
        assertTrue(b.appendText(bytes("12345678"), 0))
        assertFalse(b.appendText(bytes("9"), 1))
        assertFalse(b.appendKey("Up", 1))
        assertEquals(listOf<MeshOut>(text("12345678")), b.pending)
    }

    @Test
    fun `a full queue keeps the piece and a live one removes only what was taken`() = runBlocking<Unit> {
        val b = MeshInputBatcher()
        var now = 0L
        var outcome = InputOutcome.QueueFull
        val sent = ArrayList<MeshOut>()
        val pump = MeshInputPump(b, { sent += it; outcome }, { now })
        b.appendText(bytes("ls\r"), 0)
        assertEquals(PumpStep.Waiting, pump.step())
        assertEquals(listOf<MeshOut>(text("ls\r")), b.pending)
        outcome = InputOutcome.NotLive
        assertEquals(PumpStep.Blocked, pump.step())
        assertEquals(listOf<MeshOut>(text("ls\r")), b.pending)
        outcome = InputOutcome.Accepted
        assertEquals(PumpStep.Sent, pump.step())
        assertTrue(b.isEmpty)
        assertEquals(PumpStep.Idle, pump.step())
        assertEquals(3, sent.size, "three tries, one piece each")
    }

    @Test
    fun `a bar key is sent by name, typed text as bytes, and the latch becomes a named modifier`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.sendText("ab")
        h.sendKey("Escape")
        h.toggleModifier(StickyKey.Ctrl)
        assertTrue(h.modifiers.ctrl)
        h.sendKey("c")
        assertFalse(h.modifiers.armed, "one key spends the latch")
        h.sendKey("Up")
        val flat = b.pending
        assertEquals(text("ab"), flat[0])
        // Whatever the latch turned `c` into, it is a name or literal text, never a phone-built escape.
        val keys = flat.filterIsInstance<MeshOut.Keys>().flatMap { it.names }
        assertTrue("Escape" in keys && "Up" in keys, "names, not bytes: $keys")
        assertFalse(flat.any { it is MeshOut.Text && it.bytes.contains(0x1B) }, "the phone builds no ESC sequences")
    }

    @Test
    fun `the phone sends the same name whatever the pane's mode is, for the host to encode`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.sendKey("Up")
        assertEquals(listOf<MeshOut>(MeshOut.Keys(listOf("Up"))), b.pending)
        assertEquals("‹Up›", h.pendingLabel)
    }

    @Test
    fun `a latched Ctrl is spent by the first typed character only`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.toggleModifier(StickyKey.Ctrl)
        h.sendText("dx")
        assertFalse(h.modifiers.armed)
        val flat = b.pending
        assertTrue(flat.size >= 2, "the modified first character and the plain rest are separate pieces: $flat")
        assertEquals(text("x"), flat.last())
    }

    @Test
    fun `a paste that does not fit is refused and says so, a newline is a carriage return`() {
        val b = MeshInputBatcher(capacity = 4)
        val h = MeshTerminalHandle(b, { 0L })
        assertFalse(h.paste("too long"))
        assertTrue(h.overflow)
        assertTrue(h.paste("a\nb"))
        assertEquals(listOf<MeshOut>(text("a\rb")), b.pending)
        assertFalse(h.overflow)
    }

    @Test
    fun `a name that is not printable ASCII is not queued`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.sendKey("two words")
        h.sendKey("")
        assertTrue(b.isEmpty)
    }
}

/** The block cursor: drawn where the host's last `Commit` said, and only on that row. */
class MeshCursorRowTest {
    private fun inverted(a: androidx.compose.ui.text.AnnotatedString) =
        a.spanStyles.map { a.text.substring(it.start, it.end) }

    @Test
    fun `the character under the cursor is the one inverted`() {
        val a = meshCursorRow("$ ls", 1)
        assertEquals("$ ls", a.text)
        assertEquals(listOf(" "), inverted(a))
        assertEquals(listOf("l"), inverted(meshCursorRow("$ ls", 2)))
        assertEquals(listOf("s"), inverted(meshCursorRow("$ ls", 3)))
    }

    @Test
    fun `past the end of the text the cursor is a block on a space`() {
        val a = meshCursorRow("$", 4)
        assertEquals("$    ", a.text)
        assertEquals(listOf(" "), inverted(a))
        assertEquals(listOf(" "), inverted(meshCursorRow("", 0)))
    }

    @Test
    fun `a character outside the basic plane is kept whole and a negative column is column zero`() {
        assertEquals(listOf("\uD83D\uDE00"), inverted(meshCursorRow("\uD83D\uDE00x", 0)))
        assertEquals(listOf("a"), inverted(meshCursorRow("ab", -3)))
    }
}
