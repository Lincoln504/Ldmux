package dev.ldmux.mobile.mesh

import java.net.InetAddress
import java.net.ServerSocket
import kotlin.concurrent.thread
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.job
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout

/**
 * The same client over a REAL loopback TCP socket, against a tiny in-test gateway that
 * speaks the contract. Proves `JavaMeshSocket` (the production socket) and that closing
 * unblocks a read parked in the kernel. Torn down in `finally`.
 */
class JavaMeshSocketTest {
    private val token = ByteArray(16) { (it + 1).toByte() }
    private val key = ByteArray(32) { (0xA0 + it).toByte() }

    private fun frameIn(input: java.io.InputStream): MeshFrame {
        val head = input.readNBytes(7)
        val len = (head[5].toInt() and 0xFF) or ((head[6].toInt() and 0xFF) shl 8)
        return MeshFrameDecoder().add(head + input.readNBytes(len)).single()
    }

    @Test
    fun `full handshake, a record, a result, a downlink and a status over a real socket`() = runBlocking {
        val server = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))
        val seen = ArrayList<Int>()
        val seenKey = CompletableHolder<ByteArray>()
        val t = thread(isDaemon = true) {
            server.accept().use { c ->
                val i = c.getInputStream(); val o = c.getOutputStream()
                seen += frameIn(i).type
                o.write(MeshFrame(MeshFrameType.HELLO_OK, byteArrayOf(1, 2, 0)).encode()); o.flush()
                val pair = frameIn(i); seen += pair.type
                assertContentEquals(token, pair.payload)
                o.write(MeshFrame(MeshFrameType.PAIR_OK).encode()); o.flush()
                val bind = frameIn(i); seen += bind.type; seenKey.set(bind.payload)
                o.write(MeshFrame(MeshFrameType.BIND_OK, byteArrayOf(2, 160.toByte())).encode()); o.flush()
                val rec = frameIn(i); seen += rec.type
                o.write(MeshFrame(MeshFrameType.SEND_RESULT, byteArrayOf(rec.payload[0], 1)).encode())
                o.write(MeshFrame(MeshFrameType.RECV, byteArrayOf(9, 8, 7)).encode())
                o.write(MeshFrame(MeshFrameType.STATUS, byteArrayOf(0, -60, 55, 1, 0)).encode())
                o.flush()
                i.read() // wait for the client to hang up
            }
        }
        val scope = CoroutineScope(Dispatchers.Default + Job())
        val client = MeshGatewayClient(JavaMeshSocket, scope, token, key, port = server.localPort)
        try {
            withTimeout(10_000) {
                client.connect()
                assertEquals(MeshLinkState.Bound, client.state.value)
                val down = scope.async { client.received.first() }
                val r = client.send(ByteArray(40) { 1 })
                assertEquals(MeshSendOutcome.SentDirect, r.outcome)
                assertContentEquals(byteArrayOf(9, 8, 7), down.await())
                client.status.first { it != null }
                assertEquals(-60, client.status.value!!.rssiDbm)
            }
            assertEquals(
                listOf(MeshFrameType.HELLO, MeshFrameType.PAIR, MeshFrameType.BIND, MeshFrameType.RECORD),
                seen.toList(),
            )
            assertContentEquals(key, seenKey.get())
        } finally {
            client.close()
            scope.coroutineContext.job.children.forEach { it.join() }
            scope.cancel()
            server.close()
            t.join(3000)
        }
    }

    @Test
    fun `nothing listening on the port is Unreachable`() = runBlocking {
        val port = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1")).use { it.localPort } // closed now
        val scope = CoroutineScope(Dispatchers.Default + Job())
        val client = MeshGatewayClient(JavaMeshSocket, scope, token, key, port = port)
        try {
            val e = assertFailsWith<MeshGatewayException> { withTimeout(10_000) { client.connect() } }
            assertEquals(MeshLinkState.Unreachable, e.state)
        } finally {
            client.close(); scope.cancel()
        }
    }

    @Test
    fun `close() unblocks a read that is parked on a silent gateway`() = runBlocking {
        val server = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1"))
        val t = thread(isDaemon = true) {
            server.accept().use { c -> frameIn(c.getInputStream()); c.getInputStream().read() }
        }
        val scope = CoroutineScope(Dispatchers.Default + Job())
        val client = MeshGatewayClient(JavaMeshSocket, scope, token, key, port = server.localPort, handshakeTimeoutMs = 30_000)
        try {
            val c = async { runCatching { client.connect() } }
            delay(300) // HELLO is out, the gateway never answers
            client.close()
            withTimeout(5_000) { assertTrue(c.await().isFailure) }
            withTimeout(5_000) { scope.coroutineContext.job.children.forEach { it.join() } }
            assertEquals(MeshLinkState.Closed, client.state.value)
        } finally {
            scope.cancel(); server.close(); t.join(3000)
        }
    }

    private class CompletableHolder<T> {
        @Volatile private var v: T? = null
        fun set(x: T) { v = x }
        fun get(): T = v!!
    }
}
