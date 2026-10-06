package dev.ldmux.mobile

import kotlin.random.Random
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The attach socket's wire handling, backoff and error isolation — everything that can
 * be tested without opening a socket.
 */
class AttachClientTest {
    // ---- binary framing --------------------------------------------------------------

    private fun output(payload: ByteArray, type: Int = 0x10, declared: Int = payload.size): ByteArray {
        val h = ByteArray(8)
        h[0] = type.toByte()
        h[4] = (declared ushr 24).toByte()
        h[5] = (declared ushr 16).toByte()
        h[6] = (declared ushr 8).toByte()
        h[7] = declared.toByte()
        return h + payload
    }

    @Test
    fun `an output frame yields its payload`() {
        assertContentEquals("hello".encodeToByteArray(), decodeOutputFrame(output("hello".encodeToByteArray())))
    }

    @Test
    fun `a short, foreign or empty frame yields nothing`() {
        assertNull(decodeOutputFrame(ByteArray(3)))
        assertNull(decodeOutputFrame(output("x".encodeToByteArray(), type = 0x11)))
        assertNull(decodeOutputFrame(output(ByteArray(0))))
    }

    @Test
    fun `a declared length longer than the data is clamped to what arrived`() {
        assertContentEquals("abc".encodeToByteArray(), decodeOutputFrame(output("abc".encodeToByteArray(), declared = 1000)))
    }

    @Test
    fun `a corrupt length with the top bit set is not a four gigabyte frame`() {
        assertNull(decodeOutputFrame(output("abc".encodeToByteArray(), declared = -5)))
    }

    // ---- control messages ------------------------------------------------------------

    @Test
    fun `caps says whether the daemon understands shift on named keys`() {
        assertEquals(
            AttachMessage.Caps(shiftKeys = true),
            parseAttachMessage("""{"type":"caps","capabilities":{"scrollback":true,"shiftKeys":true}}"""),
        )
        // An older daemon advertises no such capability: Shift must not be sent to it.
        assertEquals(
            AttachMessage.Caps(shiftKeys = false),
            parseAttachMessage("""{"type":"caps","capabilities":{"scrollback":true}}"""),
        )
        assertEquals(AttachMessage.Caps(shiftKeys = false), parseAttachMessage("""{"type":"caps"}"""))
        // A malformed capabilities value costs the capability, not the message.
        assertEquals(
            AttachMessage.Caps(shiftKeys = false),
            parseAttachMessage("""{"type":"caps","capabilities":"yes"}"""),
        )
    }

    @Test
    fun `a snapshot is parsed with its modes`() {
        val m = parseAttachMessage(
            """{"type":"snapshot","cols":100,"rows":30,"screen":"hi","history":true,
              "mouse":true,"alternateOn":true,"cursorX":4,"cursorY":7,"bracketedPaste":true}""",
        ) as AttachMessage.Snapshot
        assertEquals(100, m.cols)
        assertEquals(30, m.rows)
        assertEquals("hi", m.screen)
        assertTrue(m.history)
        assertEquals(TerminalModes(mouse = true, alternateOn = true, bracketedPaste = true, cursorX = 4, cursorY = 7), m.modes)
    }

    @Test
    fun `null and wrongly typed fields cost the field, not the message`() {
        val m = parseAttachMessage(
            """{"type":"snapshot","cols":80,"rows":24,"screen":null,"mouse":null,"cursorX":{"a":1},"history":[]}""",
        ) as AttachMessage.Snapshot
        assertEquals("", m.screen)
        assertFalse(m.modes.mouse)
        assertEquals(0, m.modes.cursorX)
        assertFalse(m.history)
    }

    @Test
    fun `a snapshot with no usable grid is dropped`() {
        assertNull(parseAttachMessage("""{"type":"snapshot","cols":null,"rows":24}"""))
        assertNull(parseAttachMessage("""{"type":"snapshot","rows":24}"""))
    }

    @Test
    fun `unknown message types and fields are ignored, as an additive daemon needs`() {
        assertNull(parseAttachMessage("""{"type":"session_ended","reason":"exit"}"""))
        assertNull(parseAttachMessage("""{"type":42}"""))
        assertNotNull(parseAttachMessage("""{"type":"caps","futureCapability":{"inPlaceResync":true}}"""))
        assertEquals(
            AttachMessage.Error("resync"),
            parseAttachMessage("""{"type":"error","code":"resync","extra":[1,2,3]}"""),
        )
    }

    @Test
    fun `garbage is not a message`() {
        assertNull(parseAttachMessage("not json"))
        assertNull(parseAttachMessage("[1,2]"))
        assertNull(parseAttachMessage("null"))
        assertNull(parseAttachMessage(""))
    }

    @Test
    fun `an error with no code still reports as an error`() {
        assertEquals(AttachMessage.Error("error"), parseAttachMessage("""{"type":"error","code":null}"""))
    }

    @Test
    fun `owned names the holder when there is one`() {
        assertEquals(AttachMessage.Owned("desk"), parseAttachMessage("""{"type":"owned","owner":{"name":"desk"}}"""))
        assertEquals(AttachMessage.Owned(null), parseAttachMessage("""{"type":"owned","owner":null}"""))
        assertEquals(AttachMessage.Owned(null), parseAttachMessage("""{"type":"owned","owner":{"name":""}}"""))
    }

    @Test
    fun `a pong carries its id when it has one`() {
        assertEquals(AttachMessage.Pong(7), parseAttachMessage("""{"type":"pong","id":7}"""))
        assertEquals(AttachMessage.Pong(null), parseAttachMessage("""{"type":"pong"}"""))
    }

    // ---- error isolation -------------------------------------------------------------

    private val snapshot = """{"type":"snapshot","cols":80,"rows":24,"screen":"x"}"""

    @Test
    fun `a throwing snapshot handler does not escape, and the next message still arrives`() {
        val client = AttachClient(testConfig(), "s1")
        client.onSnapshot = { _, _, _, _, _ -> error("bad frame") }
        val got = mutableListOf<ByteArray>()
        client.onOutput = { got += it }
        client.handleControl(snapshot) // must not throw: a throw is a dead link to the receive loop
        client.handleBinary(output("after".encodeToByteArray()))
        assertEquals(1, got.size)
        assertContentEquals("after".encodeToByteArray(), got[0])
    }

    @Test
    fun `a throwing output handler is isolated`() {
        val client = AttachClient(testConfig(), "s1")
        client.onOutput = { error("clipboard exploded") }
        client.handleBinary(output("x".encodeToByteArray()))
        var snapshots = 0
        client.onSnapshot = { _, _, _, _, _ -> snapshots++ }
        client.handleControl(snapshot)
        assertEquals(1, snapshots)
    }

    @Test
    fun `every callback is guarded separately`() {
        val client = AttachClient(testConfig(), "s1")
        client.onOwned = { error("owned") }
        client.onError = { error("error") }
        client.onMouseDelivered = { error("mouse") }
        client.onAttached = { error("attached") }
        client.handleControl("""{"type":"owned","owner":{"name":"desk"}}""")
        client.handleControl("""{"type":"error","code":"no_mouse"}""")
        client.handleControl("""{"type":"mousedelivered"}""")
        client.handleControl("""{"type":"caps"}""")
    }

    @Test
    fun `hostile input never throws`() {
        val client = AttachClient(testConfig(), "s1")
        for (text in listOf("", "{", "null", "[]", """{"type":null}""", """{"type":"snapshot","cols":"x"}""", "\u0000")) {
            client.handleControl(text)
        }
        for (n in 0..16) client.handleBinary(ByteArray(n) { 0xff.toByte() })
    }

    @Test
    fun `nothing can be sent before a socket exists, and the caller is told`() {
        val client = AttachClient(testConfig(), "s1")
        assertFalse(client.isConnected)
        assertFalse(client.input("ls\n"))
        assertFalse(client.keys(listOf("Enter")))
        assertFalse(client.snapshotNow())
        assertFalse(client.mouseClick(1, 1))
    }

    // ---- backoff and liveness --------------------------------------------------------

    @Test
    fun `backoff doubles from 400 ms to a 10 s cap`() {
        assertEquals(
            listOf(400L, 800L, 1600L, 3200L, 6400L, 10_000L, 10_000L, 10_000L),
            (0..7).map { reconnectDelayMs(it) },
        )
        assertEquals(400L, reconnectDelayMs(-3))
    }

    @Test
    fun `jitter stays within half again and leaves an immediate retry immediate`() {
        val r = Random(1)
        repeat(200) {
            val j = withJitter(1000, r)
            assertTrue(j in 1000..1500, "$j")
        }
        assertEquals(0L, withJitter(0, r))
    }

    @Test
    fun `three silent heartbeats in a row declare the link dead`() {
        val m = MissedPongs(3)
        assertFalse(m.beat())
        assertFalse(m.beat())
        assertTrue(m.beat())
    }

    @Test
    fun `a pong between heartbeats starts the count over`() {
        val m = MissedPongs(3)
        assertFalse(m.beat())
        assertFalse(m.beat())
        m.onPong()
        assertFalse(m.beat())
        assertFalse(m.beat())
        assertFalse(m.beat())
        assertTrue(m.beat())
        // A fresh connection starts clean, whatever the last one died of.
        val n = MissedPongs(2)
        n.beat()
        n.reset()
        assertFalse(n.beat())
    }
}
