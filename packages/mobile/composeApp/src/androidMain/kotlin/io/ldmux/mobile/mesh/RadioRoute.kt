package io.ldmux.mobile.mesh

/**
 * Turning a heard route into a usable one.
 *
 * A MeshCore path is `hopCount` hashes of `hashSize` bytes, packed into one encoded byte:
 * `pathLength = ((hashSize - 1) << 6) | hopCount` (`Packet::setPathHashSizeAndCount`,
 * `src/Packet.h:83`). A flooded packet appends one hash per repeater as it travels, so a path read
 * off the air is in **travel order towards us**. Sending to that node uses the same hops in the
 * opposite order, which means reversing the groups — never the bytes inside a group, because a
 * two- or three-byte hash is one hop and reversing its bytes names nothing.
 *
 * This is the phone's half of the same rule the desktop applies to its replies
 * (`PeerPath::reversed` in `crates/daemon/src/mesh_serial.rs`), and it exists for one reason: a
 * companion radio cannot learn a route from an advert on its own (`BaseChatMesh::onAdvertRecv`
 * never touches `out_path`), and a route that only works at earshot is not a route at all.
 */
internal object RadioRoute {

    /** A path as the wire carries it: the encoded length byte, and the bytes it counts. */
    internal class Path(val encoded: Int, val bytes: ByteArray) {
        val isEmpty: Boolean get() = bytes.isEmpty()
    }

    /** The firmware's `MAX_PATH_SIZE`; a longer path cannot be carried at all. */
    const val MAX_PATH_BYTES = 64

    /**
     * The same route, to be used in the opposite direction.
     *
     * Returns `null` for anything that is not a well-formed path: a length byte that promises more
     * bytes than arrived, a path wider than the firmware's maximum, or a hop count above the six
     * bits it is packed into. A caller treats `null` as "no route", which is what a malformed cache
     * entry means — better than sending a packet that no repeater will recognise.
     */
    fun reversed(pathLength: Int, bytes: ByteArray): Path? {
        val hashSize = ((pathLength ushr 6) and 0x03) + 1
        // The two width bits encode 1..=4 and the firmware reserves 4 for the future
        // (`Packet::isValidPathLen`, `src/Packet.cpp:13-18`).
        if (hashSize == 4) return null
        val hops = pathLength and 0x3F
        if (hops == 0) return if (bytes.isEmpty()) Path(0, ByteArray(0)) else null
        val want = hops * hashSize
        if (bytes.size != want || want > MAX_PATH_BYTES) return null
        val out = ByteArray(want)
        for (hop in 0 until hops) {
            val from = (hops - 1 - hop) * hashSize
            bytes.copyInto(out, hop * hashSize, from, from + hashSize)
        }
        return Path(pathLength, out)
    }

    /**
     * A path read from a contact, which is already in sending order: the firmware stores the
     * reversed path that a path return carried (`onContactPathRecv`). `null` when the contact has
     * none, or when the bytes do not match the length byte.
     */
    fun fromContact(outPathLen: Int, outPath: ByteArray): Path? {
        val hashSize = ((outPathLen ushr 6) and 0x03) + 1
        if (hashSize == 4) return null
        val hops = outPathLen and 0x3F
        val want = hops * hashSize
        if (outPath.size != want) return null
        if (want > MAX_PATH_BYTES) return null
        return Path(outPathLen, outPath.copyOf(want))
    }
}
