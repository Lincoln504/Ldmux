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
    internal class Path(val encoded: Int, val bytes: ByteArray)

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

    /** The four bytes that mark a flooded **route request**, and the reply that answers it. */
    private val REQUEST_MAGIC = byteArrayOf(0x4C, 0x44, 0x52, 0x51) // "LDRQ"
    private val REPLY_MAGIC = byteArrayOf(0x4C, 0x44, 0x52, 0x52) // "LDRR"

    /** Route type FLOOD. */
    private const val ROUTE_FLOOD = 1

    /** Route type TRANSPORT_FLOOD: a flood carrying two 16-bit region codes before its path. */
    private const val ROUTE_TRANSPORT_FLOOD = 0

    /** Payload type ADVERT. */
    private const val PAYLOAD_ADVERT = 4

    /**
     * The flooded packet that asks the host for a route: `[header][path byte][dest][src][LDRQ][key]`.
     *
     * **Why a flood, and why a `REQ`.** A raw record cannot be flooded — the firmware delivers
     * `RAW_CUSTOM` only on a direct route, and a repeater does not forward one — so the only way a
     * companion can flood application bytes is to build another payload type. `REQ` is the one MeshCore
     * itself uses for an on-demand route request, and a repeater forwards it whatever its payload
     * says. The destination is the host's first key byte, which is the firmware's own hash
     * (`PATH_HASH_SIZE` is 1), so no other node answers it.
     *
     * **Why it is needed at all**: a repeater drops an advert after `flood_max_advert` hops, eight
     * by default, so two radios further apart than that can never learn a route from an advert.
     * This asks instead, and the answer crosses the same distance the request just did.
     *
     * @return the packet, or null when either key is not a public key.
     */
    fun routeRequest(hashSize: Int, hostKey: ByteArray, ownKey: ByteArray): ByteArray? {
        if (hashSize !in 1..3) return null
        if (hostKey.size != 32 || ownKey.size != 32) return null
        val out = ByteArray(2 + 2 + REQUEST_MAGIC.size + 32)
        out[0] = ROUTE_FLOOD.toByte() // route FLOOD, payload REQ, version 0
        out[1] = (((hashSize - 1) shl 6) or 0).toByte() // no hops yet: we are asking
        out[2] = hostKey[0]
        out[3] = ownKey[0]
        REQUEST_MAGIC.copyInto(out, 4)
        ownKey.copyInto(out, 4 + REQUEST_MAGIC.size)
        return out
    }

    /**
     * The route a host's reply names, or null when the record is not one.
     *
     * One rule serves both directions: the reply carries the path the **sender** uses, and the
     * reader reverses it. Getting that backwards produces a route that is the mirror of the truth.
     */
    fun routeReply(record: ByteArray): Path? {
        if (record.size < 5) return null
        for (i in REPLY_MAGIC.indices) if (record[i] != REPLY_MAGIC[i]) return null
        val encoded = record[4].toInt() and 0xFF
        return reversed(encoded, record.copyOfRange(5, record.size))
    }

    /**
     * The route an advert heard in a receive-log (`0x88`) packet carries, when it is the host's.
     *
     * `[header][transport codes?][path byte][path][payload]`, where an advert's payload starts with
     * the node's 32-byte public key. **This is the event that matters**: the radio logs every packet
     * it hears, so a route is learned the moment the host advertises instead of on the next poll of
     * `GET_ADVERT_PATH` — which, with a route already in hand, used to be five minutes apart.
     */
    fun heardAdvert(raw: ByteArray, hostKey: ByteArray): Path? {
        if (hostKey.size != 32 || raw.size < 4) return null
        val header = raw[0].toInt() and 0xFF
        val route = header and 0x03
        val payloadType = (header shr 2) and 0x0F
        // A **scoped** advert is a transport-coded flood (route 0) and carries four bytes of codes
        // before its path byte; a plain flood (route 1) does not. Missing that would read the first
        // two codes as a path byte, so a region-configured host would look like a malformed route.
        if (route != ROUTE_FLOOD && route != ROUTE_TRANSPORT_FLOOD) return null
        if (payloadType != PAYLOAD_ADVERT) return null
        val pathAt = 1 + if (route == ROUTE_TRANSPORT_FLOOD) 4 else 0
        if (raw.size < pathAt + 1) return null
        val encoded = raw[pathAt].toInt() and 0xFF
        val hashSize = (encoded ushr 6) + 1
        if (hashSize == 4) return null
        val hops = encoded and 0x3F
        val want = hops * hashSize
        val payloadAt = pathAt + 1 + want
        if (payloadAt + 32 > raw.size) return null
        for (i in 0 until 32) if (raw[payloadAt + i] != hostKey[i]) return null
        return reversed(encoded, raw.copyOfRange(pathAt + 1, payloadAt))
    }

    /**
     * Which of the two route sources to send through: **the one heard on the air, when there is one.**
     *
     * The two describe the same peer and are refreshed by different events, so they can disagree:
     *
     *  - the **advert path** is read from the radio's own "most recently heard advert" cache, and it
     *    is a route a packet actually travelled, minutes ago. It tracks a phone that has moved;
     *  - the **contact path** is a snapshot the firmware persists in its flash. A companion radio
     *    never asks for a path return, and that is the only thing that replaces it, so on this
     *    transport a contact path can only be absent or stale — it is never fresh information.
     *
     * So an advert path wins whenever it exists, and the contact path is the fallback for the case
     * the cache cannot answer (a node heard only across a path return). Timestamps would not help:
     * a contact *read* is new every polling tick even though its content is old, so "newest read"
     * lets a persisted snapshot outrank a measured route every other tick — which on a phone that
     * has moved is a dead route sent down half the time, seen as a link that dies for no reason and
     * recovers after a refresh.
     *
     * A null source means only "this source has nothing to say". It never takes the other one away.
     */
    fun <T : Any> preferHeard(contact: T?, advert: T?): T? = advert ?: contact
}
