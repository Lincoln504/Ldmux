package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.StickyKey
import kotlinx.coroutines.runBlocking
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/** Keystroke coalescing: one record per piece, never one per key, and never silent. */
class MeshInputTest {
    private fun bytes(s: String) = s.encodeToByteArray()

    @Test
    fun `typing waits for quiet, then leaves as one piece`() {
        val b = MeshInputBatcher()
        b.append(bytes("l"), 0); b.append(bytes("s"), 100); b.append(bytes(" -l"), 200)
        assertNull(b.peek(300), "still typing")
        assertNull(b.peek(200 + MeshInputBatcher.IDLE_MS - 1))
        assertContentEquals(bytes("ls -l"), b.peek(200 + MeshInputBatcher.IDLE_MS))
    }

    @Test
    fun `a line end leaves at once, and so does an interrupt`() {
        val b = MeshInputBatcher()
        b.append(bytes("ls"), 0); b.append(byteArrayOf(0x0D), 10)
        assertContentEquals(bytes("ls\r"), b.peek(10))
        b.accepted(3)
        assertTrue(b.isEmpty)
        b.append(byteArrayOf(0x03), 20)
        assertTrue(b.due(20))
    }

    @Test
    fun `steady typing still goes out after the maximum hold`() {
        val b = MeshInputBatcher()
        var t = 0L
        while (t < MeshInputBatcher.MAX_HOLD_MS) { b.append(bytes("x"), t); t += 300 }
        assertTrue(b.due(t), "the hold is bounded though it never went quiet for 700 ms")
    }

    @Test
    fun `a full record leaves at once and is never over the cap, nor splits a character`() {
        val b = MeshInputBatcher()
        b.append(ByteArray(95) { 'a'.code.toByte() } + bytes("é") + bytes("zz"), 0)
        val first = b.peek(0)!!
        assertTrue(first.size <= MeshTerm.MAX_INPUT)
        assertEquals(95, first.size, "the two-byte character is not cut in half")
        b.accepted(first.size)
        assertContentEquals(bytes("ézz"), b.pending)
    }

    @Test
    fun `a backspace over a waiting character costs nothing on the air`() {
        val b = MeshInputBatcher()
        b.append(bytes("lss"), 0); b.append(byteArrayOf(0x7F), 5)
        assertContentEquals(bytes("ls"), b.pending)
        val e = MeshInputBatcher()
        e.append(byteArrayOf(0x7F), 0)
        assertContentEquals(byteArrayOf(0x7F), e.pending, "nothing waiting to cancel: it is sent")
    }

    @Test
    fun `more than can wait is refused whole`() {
        val b = MeshInputBatcher(capacity = 8)
        assertTrue(b.append(bytes("12345678"), 0))
        assertFalse(b.append(bytes("9"), 1))
        assertContentEquals(bytes("12345678"), b.pending)
    }

    @Test
    fun `a full queue keeps the bytes and a live one removes only what was taken`() = runBlocking<Unit> {
        val b = MeshInputBatcher()
        var now = 0L
        var outcome = InputOutcome.QueueFull
        val sent = ArrayList<ByteArray>()
        val pump = MeshInputPump(b, { sent += it; outcome }, { now })
        b.append(bytes("ls\r"), 0)
        assertEquals(PumpStep.Waiting, pump.step())
        assertContentEquals(bytes("ls\r"), b.pending)
        outcome = InputOutcome.NotLive
        assertEquals(PumpStep.Blocked, pump.step())
        assertContentEquals(bytes("ls\r"), b.pending)
        outcome = InputOutcome.Accepted
        assertEquals(PumpStep.Sent, pump.step())
        assertTrue(b.isEmpty)
        assertEquals(PumpStep.Idle, pump.step())
        assertEquals(3, sent.size, "three tries, one piece each")
    }

    @Test
    fun `a bar key, a typed character and the latch all become bytes in the same buffer`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.sendText("ab")
        h.sendKey("Escape")
        h.toggleModifier(StickyKey.Ctrl)
        assertTrue(h.modifiers.ctrl)
        h.sendKey("c")
        assertFalse(h.modifiers.armed, "one key spends the latch")
        h.sendKey("Up")
        assertContentEquals("ab\u001b\u0003\u001b[A".encodeToByteArray(), b.pending)
        assertEquals("ab␛^C␛[A", h.pendingLabel)
    }

    @Test
    fun `a latched Ctrl is spent by the first typed character only`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.toggleModifier(StickyKey.Ctrl)
        h.sendText("dx")
        assertContentEquals(byteArrayOf(0x04, 'x'.code.toByte()), b.pending)
        assertFalse(h.modifiers.armed)
    }

    @Test
    fun `a paste that does not fit is refused and says so, a newline is a carriage return`() {
        val b = MeshInputBatcher(capacity = 4)
        val h = MeshTerminalHandle(b, { 0L })
        assertFalse(h.paste("too long"))
        assertTrue(h.overflow)
        assertTrue(h.paste("a\nb"))
        assertContentEquals(bytes("a\rb"), b.pending)
        assertFalse(h.overflow)
    }

    @Test
    fun `a key the table refuses sends nothing`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.sendKey("NotAKey")
        assertTrue(b.isEmpty)
    }
}
