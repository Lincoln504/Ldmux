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
    fun routeRequest(
        hashSize: Int,
        hostKey: ByteArray,
        ownKey: ByteArray,
        scope: ByteArray? = null,
    ): ByteArray? {
        if (hashSize !in 1..3) return null
        if (hostKey.size != 32 || ownKey.size != 32) return null
        if (scope != null && scope.size != 16) return null
        // The payload, which is what a scoped flood's code is computed over: the region code covers
        // the type and the payload only, so it must be hashed before the header and path join them.
        val payload = ByteArray(2 + REQUEST_MAGIC.size + 32)
        payload[0] = hostKey[0]
        payload[1] = ownKey[0]
        REQUEST_MAGIC.copyInto(payload, 2)
        ownKey.copyInto(payload, 2 + REQUEST_MAGIC.size)
        val codes = if (scope == null) 0 else 4
        val out = ByteArray(1 + codes + 1 + payload.size)
        // Route FLOOD (plain, or transport-coded when scoped) with payload REQ and version 0.
        out[0] = if (scope == null) ROUTE_FLOOD.toByte() else ROUTE_TRANSPORT_FLOOD.toByte()
        if (scope != null) {
            val code = transportCode(scope, 0, payload)
            out[1] = (code and 0xFF).toByte()
            out[2] = ((code ushr 8) and 0xFF).toByte()
            // The second code slot is reserved; the firmware writes zero there too.
            out[3] = 0
            out[4] = 0
        }
        // No hops yet: we are asking.
        out[1 + codes] = (((hashSize - 1) shl 6) or 0).toByte()
        payload.copyInto(out, 2 + codes)
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
        if (hostKey.size != 32) return null
        val packet = parse(raw) ?: return null
        if (!packet.isFlood || packet.payloadType != PAYLOAD_ADVERT) return null
        if (packet.payload.size < 32) return null
        for (i in 0 until 32) if (packet.payload[i] != hostKey[i]) return null
        return reversed(packet.pathEncoded, packet.path)
    }

    /**
     * A region's **transport key**: the first 16 bytes of `SHA256("#" + name)`.
     *
     * The firmware hashes the stored name with a leading `#` added even when the operator's name has
     * none (`RegionMap::getTransportKeysFor`, `TransportKeyStore::getAutoKeyFor`), so the key is the
     * same for `us-co` and `#us-co`, it is case-sensitive, and a client can derive it without the
     * radio having the region configured at all. A private region (a name beginning `$`) has a
     * stored key instead, which can only come from the radio.
     */
    fun scopeKey(name: String): ByteArray {
        val bare = name.trim().removePrefix("#")
        val digest = java.security.MessageDigest.getInstance("SHA-256").digest(("#" + bare).toByteArray())
        return digest.copyOfRange(0, 16)
    }

    /**
     * A packet's scope proof: `HMAC-SHA-256(key, payloadType || payload)` cut to two little-endian
     * bytes, with 0x0000 and 0xFFFF remapped.
     *
     * Only the payload and its type are hashed, so the same two bytes verify at every hop: the path
     * grows in transit, the payload does not. A repeater recomputes exactly this to decide whether
     * to forward, which is why it is computed here over the bytes that will go on the air.
     */
    fun transportCode(key: ByteArray, payloadType: Int, payload: ByteArray): Int {
        val mac = javax.crypto.Mac.getInstance("HmacSHA256")
        mac.init(javax.crypto.spec.SecretKeySpec(key, "HmacSHA256"))
        mac.update(byteArrayOf(payloadType.toByte()))
        mac.update(payload)
        val bytes = mac.doFinal()
        val code = (bytes[0].toInt() and 0xFF) or ((bytes[1].toInt() and 0xFF) shl 8)
        return when (code) {
            0 -> 1
            0xFFFF -> 0xFFFE
            else -> code
        }
    }

    /**
     * Which known region a flood was scoped to, if any.
     *
     * A scoped flood carries only the 16-bit code, never the name, so the only way to attribute it
     * is to recompute what each candidate *would* have produced for this payload. A match is
     * evidence that the repeaters which forwarded it carry that region — which is exactly what
     * makes it safe to adopt.
     */
    fun heardScope(raw: ByteArray, candidates: List<String>): String? {
        val packet = parse(raw) ?: return null
        if (!packet.isFlood) return null
        val code = packet.transportCode0 ?: return null
        for (name in candidates) {
            if (transportCode(scopeKey(name), packet.payloadType, packet.payload) == code) return name
        }
        return null
    }

    /** The parts of an over-the-air packet the route logic needs. */
    private class Ota(
        val isFlood: Boolean,
        val payloadType: Int,
        val transportCode0: Int?,
        val pathEncoded: Int,
        val path: ByteArray,
        val payload: ByteArray,
    )

    /**
     * `[header][transport codes 4?][path byte][path][payload]`, the firmware's own layout
     * (`Packet::readFrom`). Four code bytes sit between the header and the path byte for a
     * transport-coded flood or direct packet, and forgetting them turns the first code into a path
     * length.
     */
    private fun parse(raw: ByteArray): Ota? {
        if (raw.size < 2) return null
        val header = raw[0].toInt() and 0xFF
        val route = header and 0x03
        val payloadType = (header shr 2) and 0x0F
        var at = 1
        var code0: Int? = null
        if (route == ROUTE_TRANSPORT_FLOOD || route == 3) {
            if (raw.size < 5) return null
            code0 = (raw[1].toInt() and 0xFF) or ((raw[2].toInt() and 0xFF) shl 8)
            at = 5
        }
        if (raw.size <= at) return null
        val encoded = raw[at].toInt() and 0xFF
        val hashSize = (encoded ushr 6) + 1
        if (hashSize == 4) return null
        val hops = encoded and 0x3F
        val want = hops * hashSize
        val pathAt = at + 1
        if (pathAt + want > raw.size) return null
        return Ota(
            isFlood = route == ROUTE_FLOOD || route == ROUTE_TRANSPORT_FLOOD,
            payloadType = payloadType,
            transportCode0 = code0,
            pathEncoded = encoded,
            path = raw.copyOfRange(pathAt, pathAt + want),
            payload = raw.copyOfRange(pathAt + want, raw.size),
        )
    }

    /** The public region names worth testing an overheard scoped flood against. */
    val KNOWN_SCOPE_TAGS: List<String> = listOf(
        // National and supranational.
        "us", "usa", "ca", "uk", "eu", "ch", "nl", "de", "au", "nz",
        // Whole-state and province tags: the level most US and EU guides recommend for a client.
        "us-co", "us-ca", "us-tx", "us-wa", "us-or", "us-id", "us-mt", "us-az", "us-nv", "us-ut",
        "us-ny", "us-fl", "us-ga", "us-nc", "us-pa", "us-oh", "us-mi", "us-il", "us-wi", "us-mn",
        "us-mo", "us-ks", "us-ne", "us-ok", "us-nm", "us-va", "us-md", "us-ma", "us-ct", "us-nj",
        "us-ri", "us-nh", "us-vt", "us-me", "us-sc", "us-tn", "us-ky", "us-in", "us-ia", "us-ar",
        "us-la", "us-ms", "us-al", "us-ak", "us-hi", "us-dc",
        // Names some communities publish instead of the ISO form, and the largest regional tags.
        "colorado", "pnw", "bos", "northeast", "east", "west", "nl-ut", "ch-de", "ch-fr", "ch-it",
    )

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
