package dev.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue

internal fun hex(s: String): ByteArray = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

/**
 * Byte-exact agreement with the Dart reference codec.
 *
 * Every constant below was PRODUCED BY the Dart package
 * (`meshcore_gateway`, `GatewayMessages.*(...).encode()`, Dart SDK 3.13.3, 2026-10-28), not
 * typed from the document. If a test here fails, the Kotlin side drifted from the contract.
 * Inputs: token = 1..16, peer key = 0xA0..0xBF, record = [0,3,6,...,27].
 */
class MeshGatewayFrameTest {
    private val token = ByteArray(16) { (it + 1).toByte() }
    private val key = ByteArray(32) { (0xA0 + it).toByte() }
    private val rec = ByteArray(10) { (it * 3).toByte() }

    private fun assertWire(dartHex: String, frame: MeshFrame) =
        assertContentEquals(hex(dartHex), frame.encode(), "frame ${frame}")

    @Test
    fun `builders emit exactly the bytes the Dart codec emits`() {
        assertWire("4d4701010002000102", MeshMessages.hello(capabilities = 2))
        assertWire("4d4701100010000102030405060708090a0b0c0d0e0f10", MeshMessages.pair(token))
        assertWire(
            "4d4701200020" + "00a0a1a2a3a4a5a6a7a8a9aaabacadaeafb0b1b2b3b4b5b6b7b8b9babbbcbdbebf",
            MeshMessages.bind(key),
        )
        assertWire("4d470130000b0007000306090c0f1215181b", MeshMessages.record(7, rec))
    }

    @Test
    fun `gateway-to-client frames from Dart decode to the same values`() {
        fun one(h: String): MeshFrame = MeshFrameDecoder().add(hex(h)).single()

        val helloOk = MeshMessages.readHelloOk(one("4d470102000300010200"))!!
        assertEquals(MeshHelloOk(1, 2, 0), helloOk)
        assertEquals(1, MeshMessages.readHelloErr(one("4d47010300010001")))
        assertEquals(1, MeshMessages.readPairErr(one("4d47011200010001")))
        assertEquals(MeshFrameType.PAIR_OK, one("4d470111000000").type)
        assertEquals(MeshBindOk(2, 160), MeshMessages.readBindOk(one("4d47012100020002a0")))
        assertEquals(1, MeshMessages.readBindErr(one("4d47012200010001")))
        assertContentEquals(rec, MeshMessages.readRecv(one("4d470131000a00000306090c0f1215181b")))
        assertEquals(7 to MeshSendOutcome.SentDirect, MeshMessages.readSendResult(one("4d4701320002000701")))
        assertEquals(MeshCloseReason.Busy, MeshMessages.readClose(one("4d47016000010003")))
    }

    @Test
    fun `status carries the gateway's own operations-left byte, never derived here`() {
        // Vector produced by the Dart reference: opsLeftThisHour 42, then the clamp at 255.
        val a = MeshMessages.readStatus(MeshFrameDecoder().add(hex("4d47014000060000a65734122a")).single())!!
        assertEquals(42, a.opsLeftThisHour)
        assertEquals(0x1234, a.txAirtimeTenthsThisHour)
        val b = MeshMessages.readStatus(MeshFrameDecoder().add(hex("4d47014000060000a6573412ff")).single())!!
        assertEquals(255, b.opsLeftThisHour)
    }

    @Test
    fun `an older five-byte status has no operations-left value, not zero`() {
        val s = MeshMessages.readStatus(MeshFrameDecoder().add(hex("4d47014000050002a6573412")).single())!!
        assertNull(s.opsLeftThisHour)
    }

    @Test
    fun `status decodes signed rssi, 16-bit little-endian airtime, and the two sentinels`() {
        val s = MeshMessages.readStatus(MeshFrameDecoder().add(hex("4d47014000050002a6573412")).single())!!
        assertEquals(2, s.radioState)
        assertEquals(false, s.radioConnected)
        assertEquals(-90, s.rssiDbm)
        assertEquals(87, s.batteryPercent)
        assertEquals(0x1234, s.txAirtimeTenthsThisHour)

        val u = MeshMessages.readStatus(MeshFrameDecoder().add(hex("4d4701400005000080ff0000")).single())!!
        assertNull(u.rssiDbm, "0x80 means no reading, never -128 dBm")
        assertNull(u.batteryPercent, "0xFF means not reported, never 255 %")
        assertTrue(u.radioConnected)
    }

    @Test
    fun `builders refuse bad input rather than send it`() {
        assertFailsWith<IllegalArgumentException> { MeshMessages.pair(ByteArray(15)) }
        assertFailsWith<IllegalArgumentException> { MeshMessages.pair(ByteArray(17)) }
        assertFailsWith<IllegalArgumentException> { MeshMessages.bind(ByteArray(31)) }
        assertFailsWith<IllegalArgumentException> { MeshMessages.record(256, rec) }
        assertFailsWith<IllegalArgumentException> { MeshMessages.record(-1, rec) }
        assertFailsWith<IllegalArgumentException> { MeshMessages.record(0, ByteArray(0)) }
        assertFailsWith<IllegalArgumentException> { MeshMessages.record(0, ByteArray(173)) }
        MeshMessages.record(0, ByteArray(172)) // the ceiling itself is allowed
        assertFailsWith<IllegalArgumentException> { MeshFrame(0x30, ByteArray(513)) }
    }

    @Test
    fun `readers are total, a wrong type or a short payload is null not a throw`() {
        assertNull(MeshMessages.readHelloOk(MeshFrame(MeshFrameType.HELLO_OK, ByteArray(2))))
        assertNull(MeshMessages.readHelloOk(MeshFrame(MeshFrameType.PAIR_OK, ByteArray(3))))
        assertNull(MeshMessages.readBindOk(MeshFrame(MeshFrameType.BIND_OK, ByteArray(1))))
        assertNull(MeshMessages.readSendResult(MeshFrame(MeshFrameType.SEND_RESULT, ByteArray(1))))
        assertNull(MeshMessages.readStatus(MeshFrame(MeshFrameType.STATUS, ByteArray(4))))
        assertNull(MeshMessages.readClose(MeshFrame(MeshFrameType.CLOSE)))
        assertNull(MeshMessages.readRecv(MeshFrame(MeshFrameType.STATUS)))
    }

    @Test
    fun `decoder reassembles frames split at every byte boundary`() {
        val wire = hex("4d470102000300010200") + hex("4d470111000000") + hex("4d4701320002000701")
        val d = MeshFrameDecoder()
        val got = ArrayList<MeshFrame>()
        for (b in wire) got += d.add(byteArrayOf(b))
        assertEquals(listOf(MeshFrameType.HELLO_OK, MeshFrameType.PAIR_OK, MeshFrameType.SEND_RESULT), got.map { it.type })
        assertNull(d.error)
    }

    @Test
    fun `decoder yields several frames from one chunk and keeps a trailing partial`() {
        val d = MeshFrameDecoder()
        val a = hex("4d470111000000")
        val b = hex("4d4701320002000701")
        val first = d.add(a + b + b.copyOfRange(0, 3))
        assertEquals(2, first.size)
        val second = d.add(b.copyOfRange(3, b.size))
        assertEquals(1, second.size)
        assertEquals(MeshFrameType.SEND_RESULT, second[0].type)
    }

    @Test
    fun `each fatal framing error is detected and is sticky with no resync`() {
        fun err(h: String): MeshFrameError? { val d = MeshFrameDecoder(); d.add(hex(h)); return d.error }
        assertEquals(MeshFrameError.BadMagic, err("0000010111000000"))
        assertEquals(MeshFrameError.BadVersion, err("4d470211000000"))
        assertEquals(MeshFrameError.BadFlags, err("4d470111010000"))
        assertEquals(MeshFrameError.LengthTooLarge, err("4d4701110001" + "02")) // length 0x0201 = 513

        val d = MeshFrameDecoder()
        // A good frame, then bad magic, then a good frame: the first is returned, the
        // second is NOT (the stream has lost framing and is never trusted again).
        val out = d.add(hex("4d470111000000") + hex("ffff010111000000") + hex("4d470111000000"))
        assertEquals(1, out.size)
        assertEquals(MeshFrameError.BadMagic, d.error)
        assertTrue(d.add(hex("4d470111000000")).isEmpty(), "sticky: nothing after a fatal error")
    }

    @Test
    fun `an unknown frame type decodes fine so the caller can ignore it`() {
        val f = MeshFrameDecoder().add(hex("4d47017f000100aa")).single()
        assertEquals(0x7F, f.type)
    }

    @Test
    fun `a maximal frame round-trips and the length field is little endian`() {
        val big = MeshFrame(0x30, ByteArray(512) { it.toByte() })
        val wire = big.encode()
        assertEquals(519, wire.size)
        assertEquals(0x00, wire[5].toInt()); assertEquals(0x02, wire[6].toInt())
        assertContentEquals(big.payload, MeshFrameDecoder().add(wire).single().payload)
    }
}
