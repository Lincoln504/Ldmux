package dev.ldmux.mobile.mesh

/**
 * The byte pipe under [MeshGatewayClient]: one TCP connection to the MeshMore gateway.
 *
 * An interface, not a class, so the client's state machine is tested with an in-memory pair
 * and no phone. The real one is `JavaMeshSocket` (Android).
 *
 * Contract: [read] suspends until bytes arrive and returns the count, or -1 at end of
 * stream, and throws if the socket fails. [close] is idempotent, callable from any thread,
 * and must make a pending [read] return or throw rather than hang.
 */
interface MeshSocket {
    suspend fun read(buffer: ByteArray): Int
    suspend fun write(bytes: ByteArray)
    fun close()
}

/** Opens a [MeshSocket]. Throws when nothing is listening. */
fun interface MeshSocketFactory {
    suspend fun connect(host: String, port: Int): MeshSocket
}
