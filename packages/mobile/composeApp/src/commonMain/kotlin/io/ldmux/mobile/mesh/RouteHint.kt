package io.ldmux.mobile.mesh

/**
 * The four bytes and a key that tell the host which radio this is.
 *
 * **Why this exists at all.** The host has to answer a session handshake, and to answer it, it has to
 * know where to send. A direct MeshCore packet's path is *consumed* in transit — every repeater
 * removes its own hash (`Mesh::removeSelfFromPath`) — so an arriving record carries no route back,
 * and the host cannot learn one from anything this phone sends over the session. What it can do is
 * look the route up in its own radio's advert-path cache, and that lookup needs this radio's public
 * key.
 *
 * So the first thing sent after the link binds is a record that is not a session message:
 * `"LDRH"` followed by the 32-byte Ed25519 public key. Nothing in it is secret — the key identifies a
 * radio that advertises itself anyway — and nothing in it is trusted: a forged hint can only change
 * where records are *sent*, and everything inside them is sealed by the session above.
 */
internal object RouteHint {
    /** The marker the host looks for, and the reason a record of exactly 36 bytes is not a message. */
    val MAGIC: ByteArray = byteArrayOf(0x4C, 0x44, 0x52, 0x48) // "LDRH"

    /** How long a hint record is: the marker, one public key, and four bytes of nonce. */
    const val LENGTH = 40

    /**
     * The record naming [publicKey], or null when the key is not a public key.
     *
     * **The nonce is not decoration.** MeshCore dedupes on `SHA256(payload_type ‖ payload)` in a
     * 160-entry ring with no timeout, at every repeater and every receiver, so a byte-identical
     * retransmission of a direct record is silently dropped — and the hint is retried up to three
     * times in one connect. Four random bytes make each attempt a different packet.
     */
    fun record(publicKey: ByteArray): ByteArray? {
        if (publicKey.size != 32) return null
        val out = ByteArray(LENGTH)
        MAGIC.copyInto(out)
        publicKey.copyInto(out, MAGIC.size)
        kotlin.random.Random.nextBytes(out, MAGIC.size + 32, LENGTH)
        return out
    }
}
