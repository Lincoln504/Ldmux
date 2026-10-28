package dev.ldmux.mobile.mesh

import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * A [MeshSocket] over `java.net.Socket`.
 *
 * Blocking reads run on the IO dispatcher. A coroutine cancel cannot interrupt a blocking
 * read, so [close] is what unblocks it: closing the socket makes the pending `read` throw,
 * which the client's loop treats as the end of the connection. [close] is idempotent.
 */
class JavaMeshSocket private constructor(private val socket: Socket) : MeshSocket {
    private val closed = AtomicBoolean(false)
    private val input = socket.getInputStream()
    private val output = socket.getOutputStream()

    override suspend fun read(buffer: ByteArray): Int = withContext(Dispatchers.IO) { input.read(buffer) }

    override suspend fun write(bytes: ByteArray) = withContext(Dispatchers.IO) {
        output.write(bytes)
        output.flush()
    }

    override fun close() {
        if (closed.compareAndSet(false, true)) {
            try { socket.close() } catch (_: Exception) { }
        }
    }

    companion object : MeshSocketFactory {
        private const val CONNECT_TIMEOUT_MS = 3_000

        override suspend fun connect(host: String, port: Int): MeshSocket = withContext(Dispatchers.IO) {
            val s = Socket()
            try {
                s.tcpNoDelay = true
                s.connect(InetSocketAddress(host, port), CONNECT_TIMEOUT_MS)
            } catch (e: Exception) {
                try { s.close() } catch (_: Exception) { }
                throw e
            }
            JavaMeshSocket(s)
        }
    }
}
