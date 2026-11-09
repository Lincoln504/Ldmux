package io.ldmux.mobile.mesh

import io.ldmux.mobile.StickyKey
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
        val idle = MeshInputBatcher.IDLE_MS
        val hold = MeshInputBatcher.MAX_HOLD_MS
        // Both windows are tunables, so the test is written against them and never against a
        // literal millisecond count: the rule is "quiet for the idle window, or held for the hold
        // window". The keys are spread so the hold window cannot fire before the idle one.
        b.appendText(bytes("l"), 0)
        b.appendText(bytes("s"), hold / 4)
        b.appendText(bytes(" -l"), hold / 2)
        assertNull(b.peek(hold / 2 + idle - 1), "still typing")
        assertEquals(text("ls -l"), b.peek(hold / 2 + idle))
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

    /**
     * **An armed modifier the user forgot lets go, and the lapse is visible.** The handle only
     * toggled, so a Ctrl armed a minute earlier silently turned the next Esc into C-Esc; the
     * tailnet's handle has expired an unused latch for exactly that reason.
     */
    @Test
    fun `an armed modifier lapses after the latch timeout and no longer modifies the next key`() {
        val b = MeshInputBatcher()
        var now = 0L
        val h = MeshTerminalHandle(b, { now })
        h.toggleModifier(StickyKey.Ctrl)
        assertTrue(h.modifiers.ctrl)
        now = io.ldmux.mobile.LATCH_TIMEOUT_MS + 1
        h.expireModifiers()
        assertFalse(h.modifiers.armed, "the screen's tick redraws the key as unlatched")
        h.sendKey("Escape")
        val keys = b.pending.filterIsInstance<MeshOut.Keys>().flatMap { it.names }
        assertEquals(listOf("Escape"), keys, "no chord nobody asked for")
    }

    @Test
    fun `a paste that does not fit is refused and says so`() {
        val b = MeshInputBatcher(capacity = 4)
        val h = MeshTerminalHandle(b, { 0L })
        assertFalse(h.paste("too long"))
        assertTrue(h.overflow)
        assertTrue(h.paste("a\nb"))
        assertFalse(h.overflow)
    }

    @Test
    fun `a paste keeps its newlines and folds CRLF, and only typing becomes carriage returns`() {
        // **The pane decides what a newline means**, and only the host can read its paste mode:
        // a phone that turned every newline into a carriage return ran each pasted line as a
        // command and the rest went to whatever the first one started (reported 2026-11-07).
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        assertTrue(h.paste("one\ntwo\n"))
        assertEquals(listOf<MeshOut>(text("one\ntwo\n")), b.pending)
        h.discardHeld()
        // A paste copied where lines end CRLF must not arrive with a blank line after each.
        assertTrue(h.paste("one\r\ntwo"))
        assertEquals(listOf<MeshOut>(text("one\ntwo")), b.pending)
        h.discardHeld()
        // Typed text is not a paste: a newline from the keyboard or from dictation is Enter.
        h.sendText("ls\n")
        assertEquals(listOf<MeshOut>(text("ls\r")), b.pending)
    }

    @Test
    fun `a name that is not printable ASCII is not queued`() {
        val b = MeshInputBatcher()
        val h = MeshTerminalHandle(b, { 0L })
        h.sendKey("two words")
        h.sendKey("")
        assertTrue(b.isEmpty)
    }
    @Test
    fun `discard throws away everything waiting, keys included`() {
        // The public-channel mode's "discard": typed text and named keys, in order, gone without
        // one byte leaving the phone.
        val b = MeshInputBatcher()
        b.appendText(bytes("rm -rf /tmp/x"), 0)
        b.appendKey("Enter", 10)
        assertFalse(b.isEmpty)
        b.clear()
        assertTrue(b.isEmpty)
        assertNull(b.peek(10_000))
        assertTrue(b.nextDueMs() == null, "nothing waiting means nothing to schedule")
    }

    /// **A sync press sends the held text now, not after the typing window.**
    ///
    /// The windows coalesce keystrokes, and on the public preset each is one airtime (600 ms)
    /// because `pace` sizes them to the record. A press is a decision, not more typing: it must not
    /// wait out the window it has just made moot.
    @Test
    fun `a press sends the held text without waiting for the typing window`() {
        val b = MeshInputBatcher()
        b.appendText("abc".encodeToByteArray(), 0)
        assertNull(b.peek(10), "nothing leaves inside the typing window")
        b.flushNow()
        val out = b.peek(10)
        assertTrue(out is MeshOut.Text, "the held text is due at once: $out")
        assertEquals("abc", (out as MeshOut.Text).bytes.decodeToString())
    }

    @Test
    fun `the held summary counts the characters and keys the press will send`() {
        val handle = MeshTerminalHandle(MeshInputBatcher(), { 0L })
        handle.sendText("ls -l")
        handle.sendKey("Enter")
        handle.refreshPending()
        // **No record estimate.** The bar used to append `≈N records`; it is a number nobody can
        // act on (the price is decided by the text, and the link's own budget line says what is
        // left), so the summary is only what is waiting.
        assertEquals("5 characters and 1 key", handle.heldSummary)
        handle.discardHeld()
        assertEquals("", handle.heldSummary)
        assertFalse(handle.hasPending)
    }

    @Test
    fun `a long paste is counted in characters, however many records it takes`() {
        val handle = MeshTerminalHandle(MeshInputBatcher(), { 0L })
        handle.sendText("x".repeat(MeshTerm.MAX_INPUT * 2 + 1))
        handle.refreshPending()
        assertEquals("${MeshTerm.MAX_INPUT * 2 + 1} characters", handle.heldSummary)
    }

    @Test
    fun `the local echo shows characters, a line end, a tab and an erase, and nothing else`() {
        // What the fake draws has to be characters a terminal can show. A named key the shell
        // interprets — an arrow, Escape, C-c — renders as nothing, because painting one would
        // invent an effect the shell has not run. They are counted instead.
        val handle = MeshTerminalHandle(MeshInputBatcher(), { 0L })
        handle.sendText("ls")
        handle.sendKey("Enter")
        handle.sendText("a\tb")
        // The first backspace meets a plain character still waiting here, so it removes it and
        // costs nothing on the air — the batcher's own rule, unchanged by the mode. The second
        // meets a tab, which it will not fold, so it stays a key.
        handle.sendKey("BSpace")
        handle.sendKey("BSpace")
        handle.sendKey("Up")
        handle.sendKey("C-c")
        handle.refreshPending()
        assertEquals("ls\r\na\t\b \b", handle.echoText)
        // Four characters ("ls" and "a\t") and four keys (Enter, BSpace, Up, C-c).
        assertEquals("4 characters and 4 keys", handle.heldSummary)
    }

    @Test
    fun `an empty buffer echoes nothing, and discard empties the echo`() {
        val handle = MeshTerminalHandle(MeshInputBatcher(), { 0L })
        handle.refreshPending()
        assertEquals("", handle.echoText)
        handle.sendText("hello")
        handle.refreshPending()
        assertEquals("hello", handle.echoText)
        handle.discardHeld()
        assertEquals("", handle.echoText)
    }

}
