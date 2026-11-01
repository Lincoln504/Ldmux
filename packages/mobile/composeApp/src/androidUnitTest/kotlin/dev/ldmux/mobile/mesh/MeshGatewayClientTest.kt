package dev.ldmux.mobile.mesh

import java.io.IOException
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.take
import kotlinx.coroutines.job
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

class MeshGatewayClientTest {
    private val token = ByteArray(16) { (it + 1).toByte() }
    private val key = ByteArray(32) { (0xA0 + it).toByte() }

    private val helloOk = "4d470102000300010200"
    private val pairOk = "4d470111000000"
    private fun bindOk(max: Int) = MeshFrame(MeshFrameType.BIND_OK, byteArrayOf(2, max.toByte()))
    private fun sendResult(seq: Int, outcome: Int) =
        MeshFrame(MeshFrameType.SEND_RESULT, byteArrayOf(seq.toByte(), outcome.toByte()))
    private fun status(radio: Int, rssi: Int = -70, batt: Int = 90, tenths: Int = 0) = MeshFrame(
        MeshFrameType.STATUS,
        byteArrayOf(radio.toByte(), rssi.toByte(), batt.toByte(), (tenths and 0xFF).toByte(), (tenths shr 8).toByte()),
    )

    /** The test's own scope (for `async`) is delegated; [scope] is the client's. */
    private class Rig(
        val sock: FakeMeshSocket,
        val scope: CoroutineScope,
        val client: MeshGatewayClient,
        testScope: CoroutineScope,
    ) : CoroutineScope by testScope

    private fun rig(
        factory: (FakeMeshSocket) -> MeshSocketFactory = { s -> MeshSocketFactory { _, _ -> s } },
        timeoutMs: Long = 5_000,
        body: suspend Rig.() -> Unit,
    ) = runBlocking {
        val sock = FakeMeshSocket()
        val scope = CoroutineScope(Dispatchers.Default + Job())
        val client = MeshGatewayClient(factory(sock), scope, token.copyOf(), key.copyOf(), handshakeTimeoutMs = timeoutMs)
        val testScope = CoroutineScope(Dispatchers.Default + Job())
        try {
            withTimeout(15_000) { Rig(sock, scope, client, testScope).body() }
        } finally {
            testScope.cancel()
            client.close()
            scope.coroutineContext.job.children.forEach { it.join() }
            scope.cancel()
        }
    }

    /** Runs the full handshake against the fake gateway and returns once bound. */
    private suspend fun Rig.bind(max: Int = 160) {
        val c = async { client.connect() }
        assertEquals(MeshFrameType.HELLO, sock.nextFrame().type)
        sock.feed(helloOk)
        assertEquals(MeshFrameType.PAIR, sock.nextFrame().type)
        sock.feed(pairOk)
        assertEquals(MeshFrameType.BIND, sock.nextFrame().type)
        sock.feed(bindOk(max))
        c.await()
    }

    @Test
    fun `handshake sends HELLO then the token then the peer key, and ends bound`() = rig {
        val c = async { client.connect() }
        val hello = sock.nextFrame()
        assertContentEquals(hex("4d4701010002000102"), hello.encode(), "HELLO asks for status push")
        sock.feed(helloOk)
        val pair = sock.nextFrame()
        assertContentEquals(token, pair.payload)
        sock.feed(pairOk)
        val bind = sock.nextFrame()
        assertContentEquals(key, bind.payload)
        sock.feed(bindOk(160))
        c.await()
        assertEquals(MeshLinkState.Bound, client.state.value)
        assertEquals(160, client.uplinkRecordMax)
    }

    @Test
    fun `the handshake works when the gateway's bytes arrive one at a time`() = rig {
        val c = async { client.connect() }
        sock.nextFrame()
        for (b in hex(helloOk)) sock.feed(byteArrayOf(b))
        sock.nextFrame()
        for (b in hex(pairOk)) sock.feed(byteArrayOf(b))
        sock.nextFrame()
        for (b in bindOk(160).encode()) sock.feed(byteArrayOf(b))
        c.await()
        assertEquals(MeshLinkState.Bound, client.state.value)
    }

    @Test
    fun `a refused token ends Refused and closes the socket once`() = rig {
        val c = async { runCatching { client.connect() } }
        sock.nextFrame(); sock.feed(helloOk)
        sock.nextFrame()
        sock.feed("4d47011200010001") // PAIR_ERR bad_token
        sock.feed("4d47016000010001") // then the gateway's CLOSE(protocol)
        val e = c.await().exceptionOrNull() as MeshGatewayException
        assertEquals(MeshLinkState.Refused, e.state)
        assertEquals(MeshLinkState.Refused, client.state.value)
        assertEquals(1, sock.closeCount.get())
    }

    @Test
    fun `binding a host that is not a contact ends Refused`() = rig {
        val c = async { runCatching { client.connect() } }
        sock.nextFrame(); sock.feed(helloOk)
        sock.nextFrame(); sock.feed(pairOk)
        sock.nextFrame()
        sock.feed("4d47012200010001") // BIND_ERR not_a_contact
        val e = c.await().exceptionOrNull() as MeshGatewayException
        assertEquals(MeshLinkState.Refused, e.state)
        assertTrue("contact" in e.message!!)
    }

    @Test
    fun `a hello refusal is a protocol mismatch`() = rig {
        val c = async { runCatching { client.connect() } }
        sock.nextFrame()
        sock.feed("4d47010300010001")
        assertEquals(MeshLinkState.ProtocolMismatch, (c.await().exceptionOrNull() as MeshGatewayException).state)
    }

    @Test
    fun `a gateway on another version is a protocol mismatch`() = rig {
        val c = async { runCatching { client.connect() } }
        sock.nextFrame()
        // HELLO_OK claiming version 2 (frame header still v1, payload version 2).
        sock.feed(MeshFrame(MeshFrameType.HELLO_OK, byteArrayOf(2, 2, 0)))
        assertEquals(MeshLinkState.ProtocolMismatch, (c.await().exceptionOrNull() as MeshGatewayException).state)
    }

    @Test
    fun `CLOSE busy before anything else is GatewayBusy, not a generic failure`() = rig {
        val c = async { runCatching { client.connect() } }
        sock.nextFrame()
        sock.feed("4d47016000010003")
        val e = c.await().exceptionOrNull() as MeshGatewayException
        assertEquals(MeshLinkState.GatewayBusy, e.state)
        assertEquals(1, sock.closeCount.get())
    }

    @Test
    fun `nothing listening is Unreachable`() = rig(factory = { MeshSocketFactory { _, _ -> throw IOException("refused") } }) {
        val e = assertFailsWith<MeshGatewayException> { client.connect() }
        assertEquals(MeshLinkState.Unreachable, e.state)
        assertEquals(MeshLinkState.Unreachable, client.state.value)
    }

    @Test
    fun `a gateway that never answers HELLO is Unreachable after the timeout`() = rig(timeoutMs = 200) {
        val e = assertFailsWith<MeshGatewayException> { client.connect() }
        assertEquals(MeshLinkState.Unreachable, e.state)
        assertEquals(1, sock.closeCount.get(), "the half-open socket is closed")
    }

    @Test
    fun `a radio that is not connected at bind is RadioUnavailable and a STATUS can recover it`() = rig {
        val c = async { client.connect() }
        sock.nextFrame()
        sock.feed(MeshFrame(MeshFrameType.HELLO_OK, byteArrayOf(1, 2, 1))) // radio disconnected
        sock.nextFrame(); sock.feed(pairOk)
        sock.nextFrame(); sock.feed(bindOk(160))
        c.await()
        assertEquals(MeshLinkState.RadioUnavailable, client.state.value)
        sock.feed(status(0))
        withTimeout(3000) { client.state.first { it == MeshLinkState.Bound } }
    }

    @Test
    fun `STATUS drives Bound and RadioUnavailable and keeps the latest, with sentinels as null`() = rig {
        bind()
        sock.feed(status(2, rssi = -91, batt = 80, tenths = 300))
        withTimeout(3000) { client.state.first { it == MeshLinkState.RadioUnavailable } }
        assertEquals(-91, client.status.value!!.rssiDbm)
        assertEquals(300, client.status.value!!.txAirtimeTenthsThisHour)

        sock.feed(MeshFrame(MeshFrameType.STATUS, byteArrayOf(0, 0x80.toByte(), 0xFF.toByte(), 0, 0)))
        withTimeout(3000) { client.state.first { it == MeshLinkState.Bound } }
        assertNull(client.status.value!!.rssiDbm)
        assertNull(client.status.value!!.batteryPercent)
    }

    @Test
    fun `a record goes out with a sequence number and the result comes back for that seq`() = rig {
        bind()
        val a = async { client.send(ByteArray(100) { 7 }) }
        val f = sock.nextFrame()
        assertEquals(MeshFrameType.RECORD, f.type)
        assertEquals(0, f.payload[0].toInt(), "first seq is 0")
        assertEquals(101, f.payload.size)
        sock.feed(sendResult(0, 1))
        assertEquals(MeshSendResult(0, MeshSendOutcome.SentDirect), a.await())
    }

    @Test
    fun `two records in flight are answered out of order and each gets its own result`() = rig {
        bind()
        val a = async { client.send(ByteArray(10) { 1 }) }
        val fa = sock.nextFrame()
        val b = async { client.send(ByteArray(10) { 2 }) }
        val fb = sock.nextFrame()
        val sa = fa.payload[0].toInt(); val sb = fb.payload[0].toInt()
        assertTrue(sa != sb)
        sock.feed(sendResult(sb, 4)) // errBusy for the SECOND first
        sock.feed(sendResult(sa, 2)) // sentFlood for the first
        assertEquals(MeshSendOutcome.ErrBusy, b.await().outcome)
        assertEquals(MeshSendOutcome.SentFlood, a.await().outcome)
    }

    @Test
    fun `a record longer than the advertised capacity is refused locally and never written`() = rig {
        bind(max = 150)
        val before = sock.written.size
        assertFailsWith<IllegalArgumentException> { client.send(ByteArray(151)) }
        assertFailsWith<IllegalArgumentException> { client.send(ByteArray(0)) }
        assertEquals(before, sock.written.size)
        val ok = async { client.send(ByteArray(150)) } // exactly the advertised size is fine
        sock.nextFrame()
        sock.feed(sendResult(0, 1))
        assertEquals(MeshSendOutcome.SentDirect, ok.await().outcome)
    }

    @Test
    fun `send before bound is refused with the state it is in`() = rig {
        val e = assertFailsWith<MeshGatewayException> { client.send(ByteArray(4)) }
        assertEquals(MeshLinkState.Idle, e.state)
    }

    @Test
    fun `an answer for a sequence nobody awaits is dropped and does not break later sends`() = rig {
        bind()
        sock.feed(sendResult(99, 1))
        val a = async { client.send(ByteArray(4)) }
        sock.nextFrame()
        sock.feed(sendResult(0, 1))
        assertEquals(0, a.await().seq)
    }

    @Test
    fun `downlink records reach the received flow, whole`() = rig {
        bind()
        val got = async { client.received.first() }
        delay(50)
        sock.feed(MeshFrame(MeshFrameType.RECV, ByteArray(148) { it.toByte() }))
        assertContentEquals(ByteArray(148) { it.toByte() }, got.await())
    }

    @Test
    fun `a burst of records survives a collector that is inside a send`() = rig {
        bind()
        // The collector here is slow on purpose: the driver holds its lock across a send, and a
        // screen update arrives as a burst. With a fixed buffer and `tryEmit` the tail of the
        // burst was dropped without a trace, which is how a handshake answer could vanish.
        val got = async {
            val seen = ArrayList<Int>()
            client.received.take(100).collect {
                seen += it[0].toInt()
                delay(3)
            }
            seen
        }
        delay(50)
        for (i in 0 until 100) sock.feed(MeshFrame(MeshFrameType.RECV, byteArrayOf(i.toByte())))
        assertEquals((0 until 100).toList(), withTimeout(10_000) { got.await() })
    }

    @Test
    fun `an unknown frame type is ignored and the connection stays up`() = rig {
        bind()
        sock.feed(MeshFrame(0x7F, byteArrayOf(1, 2, 3)))
        sock.feed(hex(helloOk)) // a known type, out of phase: also ignored
        val a = async { client.send(ByteArray(4)) }
        sock.nextFrame()
        sock.feed(sendResult(0, 1))
        assertEquals(MeshSendOutcome.SentDirect, a.await().outcome)
        assertEquals(MeshLinkState.Bound, client.state.value)
        assertEquals(0, sock.closeCount.get())
    }

    @Test
    fun `bad magic closes, fails the pending send, and a frame after it is never delivered`() = rig {
        bind()
        val pending = async { runCatching { client.send(ByteArray(4)) } }
        sock.nextFrame()
        val received = async { runCatching { withTimeout(300) { client.received.first() } } }
        delay(50)
        // Garbage, then a perfectly good RECV in the same chunk: no resync, so it is dropped.
        sock.feed(hex("ffff01010000") + MeshFrame(MeshFrameType.RECV, ByteArray(3)).encode())
        val e = pending.await().exceptionOrNull() as MeshGatewayException
        assertEquals(MeshLinkState.Closed, e.state)
        assertEquals(MeshLinkState.Closed, client.state.value)
        assertTrue(received.await().isFailure, "the RECV behind the corruption must not surface")
        assertEquals(1, sock.closeCount.get())
    }

    @Test
    fun `a malformed payload on a type we understand closes the connection`() = rig {
        bind()
        val p = async { runCatching { client.send(ByteArray(4)) } }
        sock.nextFrame()
        sock.feed(MeshFrame(MeshFrameType.SEND_RESULT, byteArrayOf(0))) // one byte: truncated
        assertTrue(p.await().isFailure)
        assertEquals(MeshLinkState.Closed, client.state.value)
    }

    @Test
    fun `the gateway hanging up fails a pending send instead of leaking it`() = rig {
        bind()
        val p = async { runCatching { client.send(ByteArray(4)) } }
        sock.nextFrame()
        sock.gatewayHangsUp()
        assertEquals(MeshLinkState.Closed, (p.await().exceptionOrNull() as MeshGatewayException).state)
        assertEquals(1, sock.closeCount.get())
    }

    @Test
    fun `close is idempotent, closes the socket once, fails pending sends, and leaves no running job`() = rig {
        bind()
        val p = async { runCatching { client.send(ByteArray(4)) } }
        sock.nextFrame()
        client.close(); client.close(); client.close()
        assertTrue(p.await().isFailure)
        assertEquals(1, sock.closeCount.get())
        assertEquals(MeshLinkState.Closed, client.state.value)
        withTimeout(3000) { scope.coroutineContext.job.children.forEach { it.join() } }
        assertTrue(scope.coroutineContext.job.children.none { it.isActive }, "no leaked job after close")
        assertFailsWith<MeshGatewayException> { client.send(ByteArray(4)) }
    }

    @Test
    fun `close during connect does not leak the socket that connect then returns`() = runBlocking {
        val sock = FakeMeshSocket()
        val gate = CompletableDeferred<Unit>()
        val scope = CoroutineScope(Dispatchers.Default + Job())
        val client = MeshGatewayClient(MeshSocketFactory { _, _ -> gate.await(); sock }, scope, token, key)
        val c = async { runCatching { client.connect() } }
        delay(50)
        client.close()
        gate.complete(Unit)
        withTimeout(3000) { assertTrue(c.await().isFailure) }
        assertEquals(1, sock.closeCount.get())
        scope.cancel()
    }

    @Test
    fun `connect may be called only once`() = rig {
        bind()
        assertFailsWith<IllegalStateException> { client.connect() }
    }

    @Test
    fun `sequence numbers wrap, and a 257th record in flight is refused`() = rig {
        bind()
        val sends = (0 until 256).map { async { runCatching { client.send(ByteArray(2)) } } }
        repeat(256) { sock.nextFrame() }
        val e = assertFailsWith<MeshGatewayException> { client.send(ByteArray(2)) }
        assertTrue("256" in e.message!!)
        client.close()
        sends.forEach { assertTrue(it.await().isFailure) }
    }

    @Test
    fun `the token never appears in toString`() = rig {
        val s = client.toString()
        assertTrue(token.joinToString("") { "%02x".format(it) } !in s)
        assertTrue("0102030405" !in s)
    }

    @Test
    fun `constructor refuses a wrong-sized token or key`() {
        val scope = CoroutineScope(Job())
        val f = MeshSocketFactory { _, _ -> error("unused") }
        assertFailsWith<IllegalArgumentException> { MeshGatewayClient(f, scope, ByteArray(15), key) }
        assertFailsWith<IllegalArgumentException> { MeshGatewayClient(f, scope, token, ByteArray(31)) }
        scope.cancel()
    }
}
