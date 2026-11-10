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
/**
 * **Whether a candidate route may replace the one in hand, and why not when it may not.**
 *
 * A pure decision so it can be tested at every combination, because the last version of this logic
 * had the comparison in a comment and not in the code: `held` was computed and then used only in a
 * log line, so any candidate that was not remembered as failed was adopted — including a longer one.
 *
 * Measured 2026-11-10 at range: a working **0-hop** route at 08:39:49, a 3-hop copy of the same
 * answer at 08:39:52, adopted; the phone then sent its handshake down three repeaters while the host
 * answered on the direct path, and neither end heard the other. The firmware does not reroute a
 * direct packet, so a disagreement like that never heals by itself.
 *
 * @return null to adopt, or the reason to refuse (which is what the log line says).
 */
internal fun routeRefusal(
    heldHops: Int?,
    candidateHops: Int,
    proven: Boolean,
    stale: Boolean,
    rememberedFailed: Boolean,
    /**
     * The route in hand has just been shown not to carry — a direct send went unanswered — so this
     * candidate is evidence about where the peer actually is and replaces it whatever its length.
     *
     * **Without this the link can never return to direct.** A flood exists to repair a route; if the
     * route it repairs to is refused because the dead one is marked "proven", the flood taught
     * nothing and the next send floods again for the same reason, for ever.
     */
    force: Boolean = false,
): String? = when {
    force -> null
    rememberedFailed -> "it lost a session $candidateHops hop(s)"
    // **Only traffic unseats a route that is carrying it.** Being shorter is not evidence of
    // working in both directions; a flood proves one direction only.
    proven && !stale -> "proven, ${heldHops ?: 0} hop(s) in hand"
    // **The copies of one answer are one answer, and the shortest is the one to keep.**
    heldHops != null && !stale && candidateHops > heldHops ->
        "$candidateHops hop(s) is longer than $heldHops in hand"
    else -> null
}

internal object RadioRoute {

    /** A path as the wire carries it: the encoded length byte, and the bytes it counts. */
    internal class Path(val encoded: Int, val bytes: ByteArray)

    /** `PAYLOAD_TYPE_REQ`: the vehicle a stock companion radio can flood. */
    const val PAYLOAD_REQ = 0x00

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
    /**
     * How many repeaters a path holds, from the firmware's encoded length byte.
     *
     * `path_len` packs `((width - 1) << 6) | hops`: the low six bits are the hop count, the high
     * two are the hash width minus one. **One decoder, because two readers of the same byte
     * disagreed**: `RadioPath.hops()` read the hop count out of the width bits and returned 0 for
     * every path of one to three repeaters, so every route-derived timer in the driver was sized
     * for a neighbour while the phone was ten hops out (`41d6bbc`, 2026-11-08 → 2026-11-10).
     *
     * Width 4 is reserved and refused by the firmware (`Packet::isValidPathLen`), so a frame that
     * claims it describes no path at all; and a byte count that does not match whole hops is
     * malformed rather than rounded down.
     */
    fun pathHops(pathLength: Int, byteCount: Int): Int {
        val width = ((pathLength ushr 6) and 0x03) + 1
        if (width == 4) return 0
        if (byteCount < 0 || byteCount % width != 0) return 0
        return (byteCount / width).coerceAtMost(0x3F)
    }

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
        val payload = ByteArray(2 + REQUEST_MAGIC.size + 32 + 4)
        payload[0] = hostKey[0]
        payload[1] = ownKey[0]
        REQUEST_MAGIC.copyInto(payload, 2)
        ownKey.copyInto(payload, 2 + REQUEST_MAGIC.size)
        // **Four bytes of nonce, or a retry is dropped.** MeshCore dedupes on
        // `SHA256(payload_type ‖ payload)` in a 160-entry ring with no timeout at every repeater and
        // every receiver, so a byte-identical second request never leaves the first repeater. The
        // host echoes it into the reply for the same reason.
        kotlin.random.Random.nextBytes(payload, 2 + REQUEST_MAGIC.size + 32, payload.size)
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
        val hashSize = (encoded ushr 6) + 1
        if (hashSize == 4) return null
        val want = (encoded and 0x3F) * hashSize
        // **The path is the first `hops * hash` bytes; the nonce follows.** Slicing to the end of
        // the record would feed four nonce bytes into the reversal and change the hop count.
        if (record.size < 5 + want) return null
        return reversed(encoded, record.copyOfRange(5, 5 + want))
    }

    /**
     * A route request heard on the air, when it is addressed to this radio.
     *
     * **The host asks this radio for a route too, and it has to, or a restarted daemon can never
     * answer.** The host learns where the phone is from a flood — its advert or its own request —
     * and a *direct* packet's path is consumed by the repeaters that carry it, so a session record
     * arriving from the phone teaches it nothing. After a daemon restart the host holds no path, and
     * the phone holds a fresh one and therefore never asks: the phone sends `msg1`, the host cannot
     * address `msg2`, and nothing moves until the phone's own handshake ladder gives up and drops
     * the route. Answering the host's request closes that in one round trip.
     *
     * Returns the requester's radio key and the nonce to echo, or null when the packet is not a
     * request addressed to us.
     */
    fun heardRouteRequest(raw: ByteArray, ownKey: ByteArray, scope: String?): Request? {
        if (ownKey.size != 32) return null
        val packet = parse(raw) ?: return null
        if (!packet.isFlood || packet.payloadType != PAYLOAD_REQ) return null
        val payload = packet.payload
        if (payload.size < 2 + REQUEST_MAGIC.size + 32 + 4) return null
        if (payload[0] != ownKey[0]) return null
        for (i in REQUEST_MAGIC.indices) if (payload[2 + i] != REQUEST_MAGIC[i]) return null
        val host = payload.copyOfRange(6, 38)
        val nonce = payload.copyOfRange(38, 42)
        // **The answer goes back the way the question came.** A dense mesh denies unscoped floods,
        // so an unscoped answer to a scoped request is dropped before it crosses the region. Only a
        // scope this end can name is usable: this radio's own, or a public region whose key
        // reproduces the request's code.
        val scopeName = when {
            packet.transportCode0 == null -> null
            scope != null && transportCode(scopeKey(scope), packet.payloadType, payload) == packet.transportCode0 -> scope
            else -> heardScope(raw, KNOWN_SCOPE_TAGS)
        }
        return Request(host, nonce, scopeName)
    }

    /**
     * A request heard on the air: who asked, the nonce to echo, and the scope to answer under.
     *
     * [scope] is null for an unscoped request and for one whose region this end cannot name — the
     * answer then goes unscoped, which is what the request will have been able to cross.
     */
    internal class Request(val hostKey: ByteArray, val nonce: ByteArray, val scope: String?)

    /**
     * The answer to a heard request: a flooded reply carrying **this radio's** path to the host.
     *
     * The layout is the host's `build_route_reply_flood` with the roles swapped, so the host reads it
     * with `parse_route_reply_flood(packet, phone_key)` and reverses the carried path into its own.
     */
    fun replyFlood(ownKey: ByteArray, request: Request, path: Path): ByteArray? {
        if (ownKey.size != 32) return null
        if (path.bytes.size != (path.encoded and 0x3F) * (((path.encoded ushr 6) and 0x03) + 1)) return null
        // The body first, because a scoped packet's transport code is an HMAC over exactly these
        // bytes (`HMAC(key, payloadType || payload)`, cut to two little-endian bytes).
        val body = ArrayList<Byte>(2 + 4 + 32 + 4 + 1 + path.bytes.size + 4)
        body.add(request.hostKey[0]) // destination hash: the host
        body.add(ownKey[0]) // sender hash: this radio
        REPLY_MAGIC.forEach { body.add(it) }
        ownKey.forEach { body.add(it) }
        REPLY_MAGIC.forEach { body.add(it) }
        body.add(path.encoded.toByte())
        path.bytes.forEach { body.add(it) }
        request.nonce.forEach { body.add(it) }
        val payload = body.toByteArray()
        val out = ArrayList<Byte>(8 + payload.size)
        val scope = request.scope
        if (scope == null) {
            out.add(0x01) // ROUTE_TYPE_FLOOD
            // The reply's own travel path, empty at origin, at the width the carried path uses.
            out.add((path.encoded and 0xC0).toByte())
        } else {
            out.add(0x00) // ROUTE_TYPE_TRANSPORT_FLOOD: four code bytes before the path
            val code = transportCode(scopeKey(scope), PAYLOAD_REQ, payload)
            out.add((code and 0xFF).toByte())
            out.add(((code ushr 8) and 0xFF).toByte())
            out.add(0)
            out.add(0) // the second slot is reserved, as the firmware leaves it
            out.add((path.encoded and 0xC0).toByte())
        }
        payload.forEach { out.add(it) }
        return out.toByteArray()
    }

    /**
     * The route a **flooded** reply heard in a receive-log (`0x88`) packet carries.
     *
     * **This is the answer that reaches a phone at distance, and it is why a route request is
     * worth sending at all.** A reply sent directly depends on one chain of repeaters forwarding it:
     * one lost hop and the phone learns nothing, waits, and asks again — measured 2026-11-09 on a
     * single-repeater link at -11 dB SNR, where the requests went out every ten seconds and the
     * handshake never finished. A flooded reply is forwarded by every repeater in range, and the
     * community firmware's own rule for answering a flood is a flooded path return
     * (`ReplyRoute::REPLY_ROUTE_PATH_RETURN`); the adverts that compete with it are dropped after
     * `flood_max_advert` (8) hops while an unscoped `REQ` flood travels 64.
     *
     * The layout is the host's `build_route_reply_flood`, and it is pinned on both sides:
     * `[0x01 flood][travel path byte][travel path][dest hash][src hash]["LDRR"][host key 32]
     * [path byte][path][nonce]`.
     *
     * **The travel path is the route; the path inside is the host's belief.** A flood accumulates
     * one hash per repeater that forwards it (`Mesh::routeRecvPacket` appends this node's hash and
     * raises the hop count), so the packet in hand carries the exact chain it crossed to reach this
     * phone, and reversing it gives the chain back that every repeater accepts (`isHashMatch` on
     * the first entry, then `removeSelfFromPath`). This is upstream's own construction
     * (`createPathReturn(... pkt->path ...)`), and it is the reason the reply is flooded at all: only
     * a flood arrives with its chain intact, because a direct packet's path is *consumed* hop by
     * hop.
     *
     * The path carried inside was chosen by the host from whichever flood copy reached it first —
     * MeshCore's own "first packet wins" — so a short copy means "a repeater near the host relayed
     * it", not "this is the way to the phone". Measured 2026-11-10: the host answered with a
     * one-hop candidate while the phone was twenty-eight hops out, and every handshake sent down it
     * died at the first repeater.
     */
    /** The four bytes that mark a session record carried by a flood. Mirrors `RECORD_FLOOD_MAGIC`. */
    const val RECORD_FLOOD_MAGIC = "LDMR"

    /** The largest packet the companion frame accepts, opcode and priority byte excluded. */
    private const val MESHCORE_PACKET_MAX = 174

    /**
     * A sealed session record as a `REQ` flood.
     *
     * **The other carrier, and the reason the link can exist at the edge.** A record is normally a
     * `RAW_CUSTOM` direct packet, and the firmware delivers that only on a direct route: with no path
     * it reaches the operator's neighbours and stops. A `REQ` flood is forward-routed by every
     * repeater, so the same sealed record goes that way when no route is known. The nonce is outside
     * the record and fresh per send, because MeshCore dedupes on `payload_type ‖ payload` and a retry
     * must still be a different packet.
     */
    fun recordFlood(
        hashSize: Int,
        hostKey: ByteArray,
        ownKey: ByteArray,
        scope: ByteArray?,
        nonce: ByteArray,
        record: ByteArray,
    ): ByteArray? {
        if (hashSize !in 1..3) return null
        if (hostKey.size != 32 || ownKey.size != 32 || nonce.size != 4) return null
        if (scope != null && scope.size != 16) return null
        val magic = RECORD_FLOOD_MAGIC.encodeToByteArray()
        val payload = ByteArray(2 + magic.size + 4 + record.size)
        payload[0] = hostKey[0]
        payload[1] = ownKey[0]
        magic.copyInto(payload, 2)
        nonce.copyInto(payload, 2 + magic.size)
        record.copyInto(payload, 2 + magic.size + 4)
        val codes = if (scope == null) 0 else 4
        val out = ByteArray(1 + codes + 1 + payload.size)
        out[0] = if (scope == null) ROUTE_FLOOD.toByte() else ROUTE_TRANSPORT_FLOOD.toByte()
        if (scope != null) {
            val code = transportCode(scope, 0, payload)
            out[1] = (code and 0xFF).toByte()
            out[2] = ((code ushr 8) and 0xFF).toByte()
            out[3] = 0
            out[4] = 0
        }
        out[1 + codes] = ((hashSize - 1) shl 6).toByte()
        payload.copyInto(out, 2 + codes)
        // **The builder refuses what the radio would refuse.** The Rust builder returns `TooLong`
        // here and a test pins it; without the mirror the send throws inside the library, is caught
        // as "not queued", and the record silently takes the direct carrier instead — the failure
        // reading as a dead link rather than as an oversized record.
        if (out.size > MESHCORE_PACKET_MAX) return null
        return out
    }

    /**
     * A session record carried by a flood, when the packet is one addressed to this radio.
     *
     * Who sent it is not carried: the record's own AEAD decides that, and a flood from a stranger
     * costs one authentication check to refuse.
     */
    /** The route a flood travelled, reversed into a sending path for the way back. */
    fun heardFloodPath(raw: ByteArray): Path? {
        val packet = parse(raw) ?: return null
        if (!packet.isFlood) return null
        return reversed(packet.pathEncoded, packet.path)
    }

    fun heardRecordFlood(raw: ByteArray, ownKey: ByteArray): ByteArray? {
        if (ownKey.size != 32) return null
        val packet = parse(raw) ?: return null
        if (!packet.isFlood || packet.payloadType != PAYLOAD_REQ) return null
        val payload = packet.payload
        val magic = RECORD_FLOOD_MAGIC.encodeToByteArray()
        if (payload.size < 2 + magic.size + 4) return null
        if (payload[0] != ownKey[0]) return null
        for (i in magic.indices) if (payload[2 + i] != magic[i]) return null
        return payload.copyOfRange(2 + magic.size + 4, payload.size)
    }

    fun heardRouteReply(raw: ByteArray, hostKey: ByteArray): Path? {
        if (hostKey.size != 32) return null
        val packet = parse(raw) ?: return null
        if (!packet.isFlood || packet.payloadType != PAYLOAD_REQ) return null
        val payload = packet.payload
        if (payload.size < 2 + REPLY_MAGIC.size + 32) return null
        for (i in REPLY_MAGIC.indices) if (payload[2 + i] != REPLY_MAGIC[i]) return null
        for (i in 0 until 32) if (payload[6 + i] != hostKey[i]) return null
        // **What the packet did beats what the host thinks.** Zero hops is a neighbour and a fine
        // route, so the proof wins even when it is empty; only an unusable encoding falls back to
        // the carried path.
        reversed(packet.pathEncoded, packet.path)?.let { return it }
        return routeReply(payload.copyOfRange(38, payload.size))
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
    /** The derived keys of [KNOWN_SCOPE_TAGS], computed once: `heardScope` tests all of them. */
    private val knownScopeKeys: List<Pair<String, ByteArray>> by lazy {
        KNOWN_SCOPE_TAGS.map { it to scopeKey(it) }
    }

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
        // The full tag list is pre-derived once: this runs per scoped flood while the phone has no
        // scope, and deriving 70 keys each time is the expensive half of the work.
        val pool = if (candidates === KNOWN_SCOPE_TAGS) {
            knownScopeKeys
        } else {
            candidates.map { it to scopeKey(it) }
        }
        for ((name, key) in pool) {
            if (transportCode(key, packet.payloadType, packet.payload) == code) return name
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
