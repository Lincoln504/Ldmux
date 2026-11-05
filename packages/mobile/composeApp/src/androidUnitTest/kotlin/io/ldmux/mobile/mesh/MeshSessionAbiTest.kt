package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * Loads the **real** Rust session through the real JNI shim (built by `native/build-session.sh
 * host`, found through java.library.path) and runs both ends in this process: the phone through
 * [MeshSession], the daemon's side through [SessionLib.responderAccept]. Nothing is mocked; if
 * the library cannot be loaded the first test fails and says why.
 */
class MeshSessionAbiTest {
    private val now = 1_800_000_000_000L

    private fun lib(): SessionLib =
        assertNotNull(
            loadSessionLib(),
            "libldmux_session.so did not load: run packages/mobile/native/build-session.sh host " +
                "(needs cargo and a JDK) and check java.library.path",
        )

    private class Pair2(val phone: MeshSession, val hostHandle: Long, val hostLib: SessionLib) {
        val host = MeshSession(hostLib, hostHandle)
    }

    private fun handshake(lib: SessionLib, limit: Int = 172, psk: ByteArray = ByteArray(32) { 5 }): Pair2 {
        val (hp, hq) = MeshSession.generateKeypair(lib)
        val (pp, pq) = MeshSession.generateKeypair(lib)
        val pending = MeshSession.begin(lib, pp, pq, hq, psk, now)
        assertEquals(SessionLib.MSG1_LEN, pending.message1.size)
        val reply = ByteArray(SessionLib.MSG2_LEN)
        val remote = ByteArray(32)
        val out = LongArray(2)
        val rc = lib.responderAccept(hp, hq, psk, null, 0, now, pending.message1, 3, limit, reply, remote, out)
        assertEquals(SessionLib.OK, rc)
        assertContentEquals(pq, remote, "the host learns the phone key to pin")
        assertEquals(now, out[1])
        val phone = pending.finish(reply, limit, now)
        assertContentEquals(hq, phone.peerKey(), "the phone learns the host key it asked for")
        return Pair2(phone, out[0], lib)
    }

    private fun pump(from: MeshSession, to: MeshSession, t: Long): List<Delivered> {
        val got = ArrayList<Delivered>()
        for (rec in from.poll(t, 20, 8)) {
            assertTrue(rec.size <= SessionLib.RECORD_MAX)
            val r = to.receive(t, rec)
            if (r is Received.Data) got += r.chunks
        }
        return got
    }

    @Test
    fun the_native_library_loads_and_its_constants_match_the_header() {
        val l = lib()
        // The Kotlin constants repeat ldmux_session.h; a wrong one shows up as the wrong error.
        val (priv, pub) = MeshSession.generateKeypair(l)
        assertEquals(32, priv.size)
        assertEquals(32, pub.size)
        val e = assertFailsWith<MeshSessionException> {
            MeshSession.begin(l, priv, pub, pub, ByteArray(32), now)
                .finish(ByteArray(SessionLib.MSG2_LEN), 172, now)
        }
        assertEquals(SessionLib.ERR_MALFORMED, e.code, "message 2 must start with kind 0x12")
        assertEquals(MeshLinkState.SessionFailed, e.state)
    }

    @Test
    fun a_handshake_then_one_record_each_way_carries_the_plaintext() {
        val p = handshake(lib())
        assertEquals(QueueOutcome.Queued, p.phone.send(1, "ls -la".encodeToByteArray()))
        val toHost = pump(p.phone, p.host, now + 1)
        assertEquals(listOf(Delivered(1, "ls -la".encodeToByteArray())), toHost)

        assertEquals(QueueOutcome.Queued, p.host.send(1, "total 0".encodeToByteArray()))
        val toPhone = pump(p.host, p.phone, now + 2)
        assertEquals(listOf(Delivered(1, "total 0".encodeToByteArray())), toPhone)
        p.phone.close(); p.host.close()
    }

    @Test
    fun poll_never_returns_more_than_the_gateway_budget() {
        val p = handshake(lib())
        repeat(6) { p.phone.send(0, ByteArray(144) { 1 }) }
        assertEquals(0, p.phone.poll(now, recordsLeft = 0, l2Slots = 8).size, "no budget, nothing sent")
        assertEquals(2, p.phone.poll(now, recordsLeft = 2, l2Slots = 8).size)
        // Out-of-range budgets are clamped, not wrapped into a larger one.
        assertEquals(0, p.phone.poll(now, recordsLeft = -5, l2Slots = 8).size)
        p.phone.close(); p.host.close()
    }

    @Test
    fun a_full_payload_fills_the_172_byte_record_exactly() {
        val p = handshake(lib())
        p.phone.send(0, ByteArray(143) { 7 })
        val recs = p.phone.poll(now, 5, 5)
        assertEquals(1, recs.size)
        // 6 clear + 2 inner + 5 ARQ (seq, ack, mark) + 143 + 16 tag = 172
        assertEquals(172, recs[0].size, "one record at the ceiling")
        p.phone.close(); p.host.close()
    }

    @Test
    fun a_tampered_record_is_rejected_without_ending_the_session() {
        val p = handshake(lib())
        p.phone.send(0, "x".encodeToByteArray())
        val rec = p.phone.poll(now, 5, 5).single()
        val bad = rec.copyOf().also { it[it.size - 1] = (it[it.size - 1].toInt() xor 1).toByte() }
        val r = p.host.receive(now, bad)
        assertEquals(Received.Rejected(SessionLib.ERR_BAD_TAG), r)
        assertEquals(1, p.host.health(now).dropped)
        // The untouched original still opens: a forged record moved no state.
        assertIs<Received.Data>(p.host.receive(now, rec))
        p.phone.close(); p.host.close()
    }

    @Test
    fun a_replayed_record_is_rejected() {
        val p = handshake(lib())
        p.phone.send(0, "once".encodeToByteArray())
        val rec = p.phone.poll(now, 5, 5).single()
        assertIs<Received.Data>(p.host.receive(now, rec))
        assertEquals(Received.Rejected(SessionLib.ERR_DUPLICATE), p.host.receive(now, rec))
        p.phone.close(); p.host.close()
    }

    @Test
    fun the_wrong_pairing_secret_is_refused_as_a_re_pair_not_a_dead_link() {
        val l = lib()
        val (hp, hq) = MeshSession.generateKeypair(l)
        val (pp, pq) = MeshSession.generateKeypair(l)
        val pending = MeshSession.begin(l, pp, pq, hq, ByteArray(32) { 1 }, now)
        val rc = l.responderAccept(
            hp, hq, ByteArray(32) { 2 }, null, 0, now, pending.message1, 1, 172,
            ByteArray(50), ByteArray(32), LongArray(2),
        )
        assertEquals(SessionLib.ERR_BAD_FILTER, rc)
        assertEquals(MeshLinkState.Refused, sessionStateFor(rc))
        assertEquals(MeshLinkState.Refused, sessionStateFor(SessionLib.ERR_PIN_MISMATCH))
        assertEquals(MeshLinkState.Refused, sessionStateFor(SessionLib.ERR_STALE))
        assertEquals(MeshLinkState.Refused, sessionStateFor(SessionLib.ERR_REPLAY))
        assertEquals(MeshLinkState.SessionFailed, sessionStateFor(SessionLib.ERR_PANIC))
        pending.abandon()
    }

    @Test
    fun a_closed_session_refuses_use_instead_of_touching_freed_memory() {
        val p = handshake(lib())
        p.phone.close()
        assertTrue(!p.phone.isOpen)
        p.phone.close() // twice is fine
        assertFailsWith<IllegalStateException> { p.phone.send(0, byteArrayOf(1)) }
        assertFailsWith<IllegalStateException> { p.phone.poll(now, 1, 1) }
        p.host.close()
    }

    @Test
    fun a_finished_handshake_cannot_be_finished_again() {
        val l = lib()
        val (_, hq) = MeshSession.generateKeypair(l)
        val (pp, pq) = MeshSession.generateKeypair(l)
        val pending = MeshSession.begin(l, pp, pq, hq, ByteArray(32), now)
        assertFailsWith<MeshSessionException> { pending.finish(ByteArray(50), 172, now) }
        assertFailsWith<IllegalStateException> { pending.finish(ByteArray(50), 172, now) }
        pending.abandon() // and abandoning after is harmless
    }

    @Test
    fun a_full_queue_slows_the_caller_down_rather_than_growing() {
        val p = handshake(lib())
        var queued = 0
        while (p.phone.send(0, ByteArray(143)) == QueueOutcome.Queued) {
            queued++
            assertTrue(queued < 1000, "queue must be bounded")
        }
        assertEquals(64, queued, "ARQ queue_max")
        p.phone.close(); p.host.close()
    }

    @Test
    fun close_is_a_record_the_peer_sees() {
        val p = handshake(lib())
        p.phone.requestClose()
        val recs = p.phone.poll(now, 5, 5)
        assertEquals(1, recs.size)
        assertEquals(Received.PeerClosed, p.host.receive(now, recs[0]))
        assertTrue(p.host.health(now).peerClosed)
        p.phone.close(); p.host.close()
    }
}
