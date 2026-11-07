package io.ldmux.mobile.mesh

import io.iotone.meshcore.codec.MeshcoreFrameCodec
import io.iotone.meshcore.frames.AdvertPathFrame
import io.iotone.meshcore.frames.DecodeFailure
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Route learning: what the radio hands back, and how a heard path becomes a usable one.
 *
 * The firmware facts these assert, since they are not guessable from this side:
 * `pathLength = ((hashSize - 1) << 6) | hopCount` (`src/Packet.h:83`), the advert-path answer is
 * `[0x16][receivedAt u32][pathLength][bytes]` with `hopCount * hashSize` bytes
 * (`examples/companion_radio/MyMesh.cpp`, `CMD_GET_ADVERT_PATH`), and a flooded packet's path is in
 * travel order, so sending back along it reverses the hop groups.
 */
class RadioRouteTest {

    @Test
    fun `a heard path is reversed by hop, not by byte`() {
        // Three one-byte hops: pathLength 0x03 means hashSize 1, three hops.
        val heard = byteArrayOf(0x0A, 0x0B, 0x0C)
        val route = RadioRoute.reversed(0x03, heard)
        assertNotNull(route)
        assertEquals(0x03, route!!.encoded)
        assertArrayEquals(byteArrayOf(0x0C, 0x0B, 0x0A), route.bytes)
    }

    @Test
    fun `a two-byte hash keeps its bytes together`() {
        // hashSize 2, two hops: 0x42 = ((2-1) << 6) | 2.
        val heard = byteArrayOf(0xA0.toByte(), 0xA1.toByte(), 0xB0.toByte(), 0xB1.toByte())
        val route = RadioRoute.reversed(0x42, heard)
        assertNotNull(route)
        assertArrayEquals(
            "reversing the bytes inside a hop would name a different repeater",
            byteArrayOf(0xB0.toByte(), 0xB1.toByte(), 0xA0.toByte(), 0xA1.toByte()),
            route!!.bytes,
        )
    }

    @Test
    fun `a zero-hop advertiser is a valid empty route`() {
        val route = RadioRoute.reversed(0, ByteArray(0))
        assertNotNull(route)
        assertEquals(0, route!!.encoded)
        assertTrue(route.isEmpty)
    }

    @Test
    fun `a path that does not match its length byte is refused`() {
        assertNull("two hops promised, one byte given", RadioRoute.reversed(0x02, byteArrayOf(0x0A)))
        assertNull("hops with stray bytes", RadioRoute.reversed(0, byteArrayOf(0x0A)))
    }

    @Test
    fun `a path wider than the firmware allows is refused`() {
        // hashSize 2 with 33 hops = 66 bytes > MAX_PATH_SIZE.
        assertNull(RadioRoute.reversed(0x40 or 33, ByteArray(66)))
    }

    @Test
    fun `a contact path is used as it stands`() {
        val route = RadioRoute.fromContact(0x02, byteArrayOf(0x0C, 0x0B))
        assertNotNull(route)
        assertArrayEquals(byteArrayOf(0x0C, 0x0B), route!!.bytes)
        assertNull("a contact whose bytes disagree with its length", RadioRoute.fromContact(0x03, byteArrayOf(0x0C)))
        assertNull(RadioRoute.fromContact(0, byteArrayOf(0x0C)))
    }

    @Test
    fun `the lookup prefix is the first six bytes of the key`() {
        val key = ByteArray(32) { it.toByte() }
        val prefix = RadioRoute.prefix(key)
        assertNotNull(prefix)
        assertArrayEquals(byteArrayOf(0, 1, 2, 3, 4, 5), prefix)
        assertNull("a key shorter than the prefix names nothing", RadioRoute.prefix(byteArrayOf(1, 2)))
    }

    @Test
    fun `the advert-path request is the command, a reserved zero, and the prefix`() {
        val frame = MeshcoreFrameCodec.getAdvertPath(byteArrayOf(1, 2, 3, 4, 5, 6))
        assertArrayEquals(byteArrayOf(0x2A, 0x00, 1, 2, 3, 4, 5, 6), frame)
    }

    @Test
    fun `the advert-path answer decodes into hops and a path`() {
        // [0x16][receivedAt = 0x01020304][pathLength 0x02][0xAA][0xBB]
        val frame = byteArrayOf(
            0x16, 0x04, 0x03, 0x02, 0x01, 0x02, 0xAA.toByte(), 0xBB.toByte(),
        )
        val decoded = MeshcoreFrameCodec.decode(frame)
        assertTrue("decoded as ${decoded::class.simpleName}", decoded is AdvertPathFrame)
        val path = decoded as AdvertPathFrame
        assertEquals(2, path.hopCount())
        assertEquals(1, path.hashSize())
        assertEquals(0x01020304, path.receivedAtSeconds())
        assertArrayEquals(byteArrayOf(0xAA.toByte(), 0xBB.toByte()), path.path())
    }

    @Test
    fun `an advert-path answer that promises more bytes than it carries is a decode failure`() {
        // pathLength says two hops, one byte arrives.
        val frame = byteArrayOf(0x16, 0x04, 0x03, 0x02, 0x01, 0x02, 0xAA.toByte())
        assertTrue(MeshcoreFrameCodec.decode(frame) is DecodeFailure)
    }
}
