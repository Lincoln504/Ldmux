package dev.ldmux.mobile.mesh

import java.io.IOException
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.withTimeout

/**
 * An in-memory [MeshSocket] plus the "gateway" end of it. No phone, no network, no timing.
 *
 * [feed] is the gateway writing to the client. [nextFrame] is the gateway reading what the
 * client wrote. [closeCount] proves the socket is closed exactly once.
 */
class FakeMeshSocket : MeshSocket {
    private val toClient = Channel<ByteArray>(Channel.UNLIMITED)
    private val fromClient = Channel<ByteArray>(Channel.UNLIMITED)
    private val decoder = MeshFrameDecoder()
    private val queued = ArrayDeque<MeshFrame>()
    private var leftover = ByteArray(0)
    val closeCount = AtomicInteger()
    val written = ArrayList<ByteArray>()

    override suspend fun read(buffer: ByteArray): Int {
        if (leftover.isEmpty()) {
            val r = toClient.receiveCatching()
            if (r.isSuccess) leftover = r.getOrThrow()
            else throw r.exceptionOrNull() ?: return -1
        }
        val n = minOf(buffer.size, leftover.size)
        leftover.copyInto(buffer, 0, 0, n)
        leftover = leftover.copyOfRange(n, leftover.size)
        return n
    }

    override suspend fun write(bytes: ByteArray) {
        if (closeCount.get() > 0) throw IOException("socket closed")
        synchronized(written) { written += bytes }
        fromClient.send(bytes)
    }

    override fun close() {
        closeCount.incrementAndGet()
        toClient.close(IOException("closed by client"))
        fromClient.close()
    }

    /** Gateway to client. */
    fun feed(wire: ByteArray) { toClient.trySend(wire) }
    fun feed(frame: MeshFrame) = feed(frame.encode())
    fun feed(hex: String) = feed(hex(hex))

    /** Gateway sees the peer hang up cleanly (end of stream). */
    fun gatewayHangsUp() { toClient.close() }

    /** Client to gateway: the next whole frame the client wrote. */
    suspend fun nextFrame(timeoutMs: Long = 5_000): MeshFrame = withTimeout(timeoutMs) {
        while (queued.isEmpty()) {
            queued += decoder.add(fromClient.receive())
        }
        queued.removeFirst()
    }
}
