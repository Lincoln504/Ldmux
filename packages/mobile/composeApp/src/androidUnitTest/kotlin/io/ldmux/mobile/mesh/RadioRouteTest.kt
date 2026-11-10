package io.ldmux.mobile.mesh

import io.iotone.meshcore.codec.MeshcoreFrameCodec
import io.iotone.meshcore.frames.AdvertPathFrame
import io.iotone.meshcore.frames.DecodeFailure
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Route learning: what the radio hands back, and how a heard path becomes a usable one.
 *
 * The firmware facts these assert, since they are not guessable from this side:
 * `pathLength = ((hashSize - 1) << 6) | hopCount` (`src/Packet.h:83`), the advert-path answer is
 * `[0x16][receivedAt u32][pathLength][bytes]` with `hopCount * hashSize` bytes
 * (`examples/companion_radio/MyMesh.cpp`, `CMD_GET_ADVERT_PATH`), and a flooded packet's path is in
 * travel order, so sending back along it reverses the hop groups.
 */
class RadioRouteTest {

    @Test
    fun `a heard path is reversed by hop, not by byte`() {
        // Three one-byte hops: pathLength 0x03 means hashSize 1, three hops.
        val heard = byteArrayOf(0x0A, 0x0B, 0x0C)
        val route = RadioRoute.reversed(0x03, heard)
        assertNotNull(route)
        assertEquals(0x03, route!!.encoded)
        assertArrayEquals(byteArrayOf(0x0C, 0x0B, 0x0A), route.bytes)
    }

    @Test
    fun `a two-byte hash keeps its bytes together`() {
        // hashSize 2, two hops: 0x42 = ((2-1) << 6) | 2.
        val heard = byteArrayOf(0xA0.toByte(), 0xA1.toByte(), 0xB0.toByte(), 0xB1.toByte())
        val route = RadioRoute.reversed(0x42, heard)
        assertNotNull(route)
        assertArrayEquals(
            "reversing the bytes inside a hop would name a different repeater",
            byteArrayOf(0xB0.toByte(), 0xB1.toByte(), 0xA0.toByte(), 0xA1.toByte()),
            route!!.bytes,
        )
    }

    @Test
    fun `a zero-hop advertiser is a valid empty route`() {
        val route = RadioRoute.reversed(0, ByteArray(0))
        assertNotNull(route)
        assertEquals(0, route!!.encoded)
        assertEquals(0, route.bytes.size)
    }

    @Test
    fun `a path that does not match its length byte is refused`() {
        assertNull("two hops promised, one byte given", RadioRoute.reversed(0x02, byteArrayOf(0x0A)))
        assertNull("hops with stray bytes", RadioRoute.reversed(0, byteArrayOf(0x0A)))
    }

    @Test
    fun `the reserved four-byte hash width is refused`() {
        // The width bits encode 1..=4 and the firmware reserves 4 for the future
        // (`Packet::isValidPathLen`, `src/Packet.cpp:13-18`), so 0xC2 is malformed, not wide.
        assertNull(RadioRoute.reversed(0xC2, ByteArray(8)))
        assertNull(RadioRoute.fromContact(0xC2, ByteArray(8)))
    }

    @Test
    fun `a path wider than the firmware allows is refused`() {
        // hashSize 2 with 33 hops = 66 bytes > MAX_PATH_SIZE.
        assertNull(RadioRoute.reversed(0x40 or 33, ByteArray(66)))
    }

    @Test
    fun `a contact path is used as it stands`() {
        val route = RadioRoute.fromContact(0x02, byteArrayOf(0x0C, 0x0B))
        assertNotNull(route)
        assertArrayEquals(byteArrayOf(0x0C, 0x0B), route!!.bytes)
        assertNull("a contact whose bytes disagree with its length", RadioRoute.fromContact(0x03, byteArrayOf(0x0C)))
        assertNull(RadioRoute.fromContact(0, byteArrayOf(0x0C)))
    }

    @Test
    fun `the advert-path request is the command, a reserved zero, and the whole key`() {
        // The firmware's guard is `len >= PUB_KEY_SIZE + 2` and it matches on the first seven bytes
        // of the key, so the frame carries all 32 — a six-byte prefix frame is "unsupported".
        val key = ByteArray(32) { (it + 1).toByte() }
        val frame = MeshcoreFrameCodec.getAdvertPath(key)
        assertEquals(34, frame.size)
        assertEquals(0x2A.toByte(), frame[0])
        assertEquals(0x00.toByte(), frame[1])
        assertArrayEquals(key, frame.copyOfRange(2, 34))
    }

    @Test
    fun `the advert-path answer decodes into hops and a path`() {
        // [0x16][receivedAt = 0x01020304][pathLength 0x02][0xAA][0xBB]
        val frame = byteArrayOf(
            0x16, 0x04, 0x03, 0x02, 0x01, 0x02, 0xAA.toByte(), 0xBB.toByte(),
        )
        val decoded = MeshcoreFrameCodec.decode(frame)
        assertTrue("decoded as ${decoded::class.simpleName}", decoded is AdvertPathFrame)
        val path = decoded as AdvertPathFrame
        assertEquals(2, path.hopCount())
        assertEquals(1, path.hashSize())
        assertEquals(0x01020304, path.receivedAtSeconds())
        assertArrayEquals(byteArrayOf(0xAA.toByte(), 0xBB.toByte()), path.path())
    }

    @Test
    fun `the route hint is the marker and the radio's key, and nothing else`() {
        val key = ByteArray(32) { (it + 1).toByte() }
        val hint = RouteHint.record(key)
        assertNotNull(hint)
        assertEquals(RouteHint.LENGTH, hint!!.size)
        assertArrayEquals(RouteHint.MAGIC, hint.copyOfRange(0, 4))
        assertArrayEquals(key, hint.copyOfRange(4, 36))
        assertNull("a key that is not a public key names nothing", RouteHint.record(ByteArray(31)))
        assertNull(RouteHint.record(ByteArray(33)))
    }

    @Test
    fun `a heard route beats a stored contact route, and an empty source never takes one away`() {
        // A contact's out-path is persisted and a companion can never replace it, so it is never
        // fresh information; the advert path is a route a packet travelled minutes ago.
        assertEquals("advert", RadioRoute.preferHeard("contact", "advert"))
        // The one case the cache cannot answer.
        assertEquals("contact", RadioRoute.preferHeard("contact", null))
        assertEquals("advert", RadioRoute.preferHeard(null, "advert"))
        assertNull(RadioRoute.preferHeard(null, null))
    }

    @Test
    fun `an advert-path answer that promises more bytes than it carries is a decode failure`() {
        // pathLength says two hops, one byte arrives.
        val frame = byteArrayOf(0x16, 0x04, 0x03, 0x02, 0x01, 0x02, 0xAA.toByte())
        assertTrue(MeshcoreFrameCodec.decode(frame) is DecodeFailure)
    }

    // ── the on-demand route request, and the reply that answers it ───────────────────────────

    /**
     * **The request is a `REQ` flood addressed by the host's key hash.** A raw record cannot be
     * flooded, so this is the only way a companion can ask; if the header or the hashes are wrong
     * the mesh either drops it or some other node's hash matches and answers it.
     */
    @Test
    fun `a route request is a flood addressed to the host`() {
        val host = ByteArray(32) { (it + 1).toByte() }
        val own = ByteArray(32) { (it + 0x40).toByte() }
        val packet = RadioRoute.routeRequest(2, host, own)
        assertNotNull(packet)
        packet!!
        // header: route FLOOD (1), payload type REQ (0), version 0.
        assertEquals(0x01, packet[0].toInt() and 0xFF)
        assertEquals(1, packet[0].toInt() and 0x03) // route FLOOD
        assertEquals(0, (packet[0].toInt() shr 2) and 0x0F)
        // path byte: two-byte hashes, no hops yet.
        assertEquals(0x40, packet[1].toInt() and 0xFF)
        // payload: host hash, own hash, LDRQ, own key.
        assertEquals(host[0], packet[2])
        assertEquals(own[0], packet[3])
        assertArrayEquals("LDRQ".toByteArray(), packet.copyOfRange(4, 8))
        assertArrayEquals(own, packet.copyOfRange(8, 40))
        // The nonce follows the key, so a retried request is not a duplicate packet.
        assertEquals(44, packet.size)
    }

    /** A key that is not 32 bytes cannot name a radio, so no request is built for it. */
    @Test
    fun `a route request needs two public keys`() {
        assertNull(RadioRoute.routeRequest(1, ByteArray(31), ByteArray(32)))
        assertNull(RadioRoute.routeRequest(1, ByteArray(32), ByteArray(0)))
        assertNull(RadioRoute.routeRequest(4, ByteArray(32), ByteArray(32)))
    }

    /**
     * **One rule for both directions.** The reply carries the path the host uses towards us; our
     * route to the host is the reverse of it, by hop group.
     */
    @Test
    fun `a route reply becomes the reverse path`() {
        // Two one-byte hops in the host's sending order: 0x22 nearest us, 0x11 nearest it.
        val record = byteArrayOf(0x4C, 0x44, 0x52, 0x52, 0x02, 0x22, 0x11)
        val route = RadioRoute.routeReply(record)
        assertNotNull(route)
        assertEquals(2, route!!.encoded and 0x3F)
        assertArrayEquals(byteArrayOf(0x11, 0x22), route.bytes)
        // Not a reply, a ragged one, and a reserved width are all refused.
        assertNull(RadioRoute.routeReply(byteArrayOf(0x13, 0x01, 0x02, 0x03, 0x04)))
        assertNull(RadioRoute.routeReply(byteArrayOf(0x4C, 0x44, 0x52, 0x52, 0xC1.toByte(), 0x00)))
        assertNull(RadioRoute.routeReply(byteArrayOf(0x4C, 0x44, 0x52, 0x52, 0x42, 0x00)))
    }

    /**
     * **The answer that reaches a phone at distance is a flood.**
     *
     * A direct reply is one packet along one chain of repeaters: measured 2026-11-09 at range, the
     * requests went out every ten seconds and the handshake never finished because the single reply
     * kept being lost. The host now answers the way the community firmware answers a flood
     * (`ReplyRoute::REPLY_ROUTE_PATH_RETURN`), and the layout below is the exact contract between
     * `build_route_reply_flood` in `crates/companion` and this reader.
     */
    /**
     * **The hop count is the low six bits, and every timer in the driver is built from it.**
     *
     * This read the width out of the hop count for two days, so a path of one to three repeaters
     * reported *zero*: the session retried at a neighbour's 3.6 s on a link whose round trip was
     * seven seconds, the repair deadline fired at its twenty-second floor on a twenty-hop route,
     * and the handshake ladder asked for a new path before its own attempt could have arrived.
     * Every one of the range failures since 2026-11-08 was measured with this number wrong.
     */
    @Test
    fun `a path byte decodes to the hops it carries at every width`() {
        // One byte a hop: the deployed configuration. The measured failure was 1→0, 2→0, 3→0,
        // 5→2, 17→8 — the width read out of the hop count.
        for (hops in listOf(0, 1, 2, 3, 4, 5, 10, 17, 28, 62, 63)) {
            assertEquals("one byte, $hops hop(s)", hops, RadioRoute.pathHops(hops, hops))
        }
        // Two bytes a hop: the width is in the top two bits, so the byte is 0x40 | hops.
        for (hops in listOf(0, 1, 2, 16, 32)) {
            assertEquals(
                "two bytes, $hops hop(s)",
                hops,
                RadioRoute.pathHops(0x40 or hops, hops * 2),
            )
        }
        // Three bytes a hop.
        for (hops in listOf(0, 1, 7, 21)) {
            assertEquals(
                "three bytes, $hops hop(s)",
                hops,
                RadioRoute.pathHops(0x80 or hops, hops * 3),
            )
        }
        // Width 4 is reserved and refused by the firmware; a byte count that is not whole hops is
        // malformed. Neither invents a route.
        assertEquals("width 4 is not a path", 0, RadioRoute.pathHops(0xC1, 1))
        assertEquals("five bytes is not whole two-byte hops", 0, RadioRoute.pathHops(0x40 or 3, 5))
        // And the driver's own view agrees with the decoder it delegates to.
        assertEquals("the chain that was actually measured", 28, RadioRoute.pathHops(28, 28))
    }

    /// **A shorter copy of one answer never widens a route, and only traffic unseats one that works.**
    ///
    /// The comparison used to live in a comment: `held` was computed and then used only in a log
    /// line, so a 3-hop copy adopted over a working 0-hop route at 08:39:52 on 2026-11-10 — and the
    /// phone then sent its handshake down three repeaters while the host answered direct, with no
    /// firmware rerouting to heal it.
    @Test
    fun `a route decision keeps the shortest and only traffic unseats a working route`() {
        // Nothing in hand: anything is adopted.
        assertNull(routeRefusal(heldHops = null, candidateHops = 3, proven = false, stale = false, rememberedFailed = false))
        // A longer copy of the same answer is refused, fresh or not.
        assertNotNull(routeRefusal(heldHops = 0, candidateHops = 3, proven = false, stale = false, rememberedFailed = false))
        // A shorter one is adopted.
        assertNull(routeRefusal(heldHops = 5, candidateHops = 1, proven = false, stale = false, rememberedFailed = false))
        // Once it has carried traffic, nothing a flood says unseats it while it is fresh.
        assertNotNull(routeRefusal(heldHops = 3, candidateHops = 1, proven = true, stale = false, rememberedFailed = false))
        // A stale one yields to a shorter candidate.
        assertNull(routeRefusal(heldHops = 3, candidateHops = 1, proven = true, stale = true, rememberedFailed = false))
        // A path that lost a session is refused whatever else is true.
        assertNotNull(routeRefusal(heldHops = null, candidateHops = 1, proven = false, stale = true, rememberedFailed = true))
    }

    /// **The same bytes as the host, pinned.** This wrapper is built twice — here and in Rust
    /// (`build_record_flood_in_scope`) — and a one-byte disagreement produces a record that is sent
    /// and never opened, which reads as a dead link. The Rust test asserts this identical vector.
    @Test
    fun `a record flood is the same bytes as the hosts`() {
        val host = ByteArray(32) { 0x11 }
        val own = ByteArray(32) { 0x22 }
        val packet = RadioRoute.recordFlood(
            hashSize = 1,
            hostKey = host,
            ownKey = own,
            scope = null,
            nonce = byteArrayOf(0x01, 0x02, 0x03, 0x04),
            record = byteArrayOf(0xAA.toByte(), 0xBB.toByte(), 0xCC.toByte()),
        )!!
        assertEquals(
            listOf<Byte>(
                0x01, // ROUTE_FLOOD | REQ
                0x00, // path byte: zero hops, one-byte hashes
                0x11, // dest hash = the recipient's key byte 0
                0x22, // source hash = this radio's key byte 0
                'L'.code.toByte(), 'D'.code.toByte(), 'M'.code.toByte(), 'R'.code.toByte(),
                0x01, 0x02, 0x03, 0x04, // nonce, outside the sealed record
                0xAA.toByte(), 0xBB.toByte(), 0xCC.toByte(), // the record, untouched
            ),
            packet.toList(),
        )
    }

    /// A flood addressed to this radio is a record; the same flood is not ours to open at the sender.
    @Test
    fun `a record flood reads back only for its address`() {
        val host = ByteArray(32) { 0x11 }
        val own = ByteArray(32) { 0x22 }
        val record = byteArrayOf(1, 2, 3, 4, 5)
        val packet = RadioRoute.recordFlood(1, host, own, null, byteArrayOf(9, 8, 7, 6), record)!!
        assertArrayEquals(record, RadioRoute.heardRecordFlood(packet, host))
        assertNull(RadioRoute.heardRecordFlood(packet, own))
    }

    @Test
    fun `a flooded reply in the receive log carries the route`() {
        val host = ByteArray(32) { 0xA1.toByte() }
        // The inner path is the host's *belief*, and it says two hops throughout this test.
        val path = byteArrayOf(0x22, 0x11)
        val raw = floodReply(host, travelHops = 0, pathByte = 0x02, path = path)
        val route = RadioRoute.heardRouteReply(raw, host)
        assertNotNull(route)
        // **A reply that arrived directly is a direct route**, and that beats a belief that says
        // two hops: this is the ghost case, where the host's "first packet wins" guess described
        // the other direction and sent every handshake down a chain that does not reach here.
        assertEquals("arrival proves the route", 0, route!!.encoded and 0x3F)
        assertArrayEquals(byteArrayOf(), route.bytes)

        // **The travel chain is the route.** Three repeaters forwarded it, so the packet holds
        // their hashes in travel order and the reverse is what this radio must send along.
        val travelled = floodReply(host, travelHops = 3, pathByte = 0x02, path = path)
        val proof = RadioRoute.heardRouteReply(travelled, host)
        assertNotNull(proof)
        assertEquals("three repeaters", 3, proof!!.encoded and 0x3F)
        assertArrayEquals(
            "the chain in sending order",
            byteArrayOf(0xC2.toByte(), 0xC1.toByte(), 0xC0.toByte()),
            proof.bytes,
        )

        // Another node's reply is not our host's route, and a direct packet is not a flood.
        val other = ByteArray(32) { 0xB2.toByte() }
        assertNull(RadioRoute.heardRouteReply(raw, other))
        val direct = floodReply(host, travelHops = 0, pathByte = 0x02, path = path)
        direct[0] = 0x3E
        assertNull(RadioRoute.heardRouteReply(direct, host))
        // **A lost nonce is not a lost route.** The tail is four bytes of it, the travel chain is
        // intact, and the reply still teaches the route — because the route now comes from the
        // chain and not from the record inside.
        assertNotNull(RadioRoute.heardRouteReply(raw.copyOfRange(0, raw.size - 4), host))
        // **A cut into the travel chain is not a route.** The path byte promises three hashes and
        // the packet no longer holds them, so the frame is malformed and refused rather than
        // half-read into a shorter route that would send down the wrong repeaters.
        assertNull(RadioRoute.heardRouteReply(travelled.copyOfRange(0, travelled.size - 20), host))
        val foreign = floodReply(host, travelHops = 0, pathByte = 0x02, path = path)
        foreign[6] = 'X'.code.toByte()
        assertNull(RadioRoute.heardRouteReply(foreign, host))
    }

    /**
     * **The phone answers the host's request, and the two readers agree.** The host cannot learn
     * where the phone is from a session record (a direct packet's path is consumed in transit), so
     * after a daemon restart it asks; this radio answers with its own path and the host reverses it
     * exactly as this one reverses a reply.
     */
    @Test
    fun `a request from the host is answered with this radio's path`() {
        val host = ByteArray(32) { 0xA1.toByte() }
        val phone = ByteArray(32) { 0xB2.toByte() }
        val path = byteArrayOf(0x11, 0x22)
        // The host asks *us*: the destination hash is this radio's, the sender hash is the host's.
        val request = RadioRoute.routeRequest(1, phone, host)
        assertNotNull(request)
        val heard = RadioRoute.heardRouteRequest(request!!, phone, null)
        assertNotNull("a request addressed to us is ours", heard)
        val reply = RadioRoute.replyFlood(phone, heard!!, RadioRoute.Path(0x02, path))
        assertNotNull(reply)
        // Read it back the way the *host* would, with the phone's key as the sender. At the origin
        // the reply has crossed nothing, so the truthful answer is "direct" — the path carried
        // inside is not evidence about the direction the host will send in.
        val route = RadioRoute.heardRouteReply(reply!!, phone)
        assertNotNull(route)
        assertEquals("nothing forwarded it yet", 0, route!!.encoded and 0x3F)
        assertArrayEquals(byteArrayOf(), route.bytes)
        // The same request read by another radio is not that radio's.
        assertNull(RadioRoute.heardRouteRequest(request, ByteArray(32) { 0x01 }, null))
        // And a request we send to the host is not ours to answer.
        assertNull(RadioRoute.heardRouteRequest(RadioRoute.routeRequest(1, host, phone)!!, phone, null))
    }

    /**
     * **The answer goes back under the question's scope.** A mesh that denies unscoped floods drops
     * an unscoped answer before it crosses the region, so a scoped request must be answered scoped,
     * with the code recomputed over the answer's own bytes.
     */
    @Test
    fun `a scoped request is answered under the same scope`() {
        val host = ByteArray(32) { 0xA1.toByte() }
        val phone = ByteArray(32) { 0xB2.toByte() }
        val scoped = RadioRoute.routeRequest(1, phone, host, RadioRoute.scopeKey("us-co"))
        assertNotNull(scoped)
        val heard = RadioRoute.heardRouteRequest(scoped!!, phone, "us-co")
        assertNotNull(heard)
        assertEquals("us-co", heard!!.scope)
        val reply = RadioRoute.replyFlood(phone, heard, RadioRoute.Path(0x02, byteArrayOf(0x11, 0x22)))
        assertNotNull(reply)
        assertEquals("ROUTE_TYPE_TRANSPORT_FLOOD", 0x00, reply!![0].toInt() and 0x03)
        // The four code bytes precede the travel path byte.
        val code = (reply[1].toInt() and 0xFF) or ((reply[2].toInt() and 0xFF) shl 8)
        val payload = reply.copyOfRange(6, reply.size)
        assertEquals(RadioRoute.transportCode(RadioRoute.scopeKey("us-co"), 0, payload), code)
        // And it still reads back as a route: unscoped at the origin, so a direct one.
        assertEquals(
            0,
            RadioRoute.heardRouteReply(reply, phone)!!.encoded and 0x3F,
        )
    }

    /**
     * The host's `build_route_reply_flood`:
     * `[flood][travel path][dest][src]["LDRR"][host key]["LDRR"][path byte][path][nonce]`.
     *
     * The inner record is a whole direct reply, magic included — the same bytes the direct reply
     * carried — so one parser serves both and the two cannot drift.
     */
    private fun floodReply(
        host: ByteArray,
        travelHops: Int,
        pathByte: Int,
        path: ByteArray,
    ): ByteArray {
        val travel = ByteArray(travelHops) { (0xC0 + it).toByte() }
        val out = ArrayList<Byte>()
        out.add(0x01)
        out.add((travelHops and 0x3F).toByte())
        travel.forEach { out.add(it) }
        out.add(0x42) // destination hash: the phone
        out.add(host[0]) // sender hash: the host
        "LDRR".forEach { out.add(it.code.toByte()) }
        host.forEach { out.add(it) }
        // The inner record is a direct reply in full, so it carries its own magic.
        "LDRR".forEach { out.add(it.code.toByte()) }
        out.add(pathByte.toByte())
        path.forEach { out.add(it) }
        listOf(7, 8, 9, 10).forEach { out.add(it.toByte()) }
        return out.toByteArray()
    }

    /**
     * **The advert as it arrived, not as the cache remembers it.** A `0x88` log line carries the
     * whole packet with its path, so the route is learned on the event instead of on the next
     * `GET_ADVERT_PATH` — which, with a route in hand, was five minutes apart.
     */
    @Test
    fun `an advert heard on the air names the host route`() {
        val host = ByteArray(32) { (it + 7).toByte() }
        // header FLOOD|ADVERT, path byte three one-byte hops, path, payload starts with the key.
        val raw = byteArrayOf(0x11, 0x03, 0x0A, 0x0B, 0x0C) + host + ByteArray(4)
        val route = RadioRoute.heardAdvert(raw, host)
        assertNotNull(route)
        assertArrayEquals(byteArrayOf(0x0C, 0x0B, 0x0A), route!!.bytes)
        // A **scoped** advert is a transport-coded flood: four bytes of region codes before the
        // path byte, so the path starts at 6 and not at 2.
        val scoped = byteArrayOf(0x10, 0, 0, 0, 0, 0x02, 0x0B, 0x0C) + host + ByteArray(4)
        val scopedRoute = RadioRoute.heardAdvert(scoped, host)
        assertNotNull(scopedRoute)
        assertArrayEquals(byteArrayOf(0x0C, 0x0B), scopedRoute!!.bytes)
        // Another node's advert, a direct packet, and a truncated payload are not the host's.
        assertNull(RadioRoute.heardAdvert(raw, ByteArray(32) { (it + 8).toByte() }))
        assertNull(RadioRoute.heardAdvert(byteArrayOf(0x12, 0x00) + host, host))
        assertNull(RadioRoute.heardAdvert(raw.copyOfRange(0, 20), host))
    }

    // ── flood scope: a region's key, and the per-packet code that proves it ──────────────────

    /**
     * **Both languages derive the same region key.** The firmware's derivation is
     * `SHA256("#" + name)` cut to 16 bytes, and the pinned value is the one
     * `crates/companion/src/codec.rs` asserts for the same name: a difference here would make every
     * scoped packet this phone sends unrecognisable to every repeater, with no error anywhere.
     */
    @Test
    fun `a scope key is the firmware derivation`() {
        val key = RadioRoute.scopeKey("us-co")
        assertEquals(
            "62caa5aebca0852fa47c9642c5c630ac",
            key.joinToString("") { "%02x".format(it) },
        )
        // The `#` is optional in what an operator types, and hashing is case-sensitive.
        assertArrayEquals(key, RadioRoute.scopeKey("#us-co"))
        assertFalse(RadioRoute.scopeKey("us-co").contentEquals(RadioRoute.scopeKey("US-CO")))
    }

    /** The per-packet code, pinned across languages: `HMAC-SHA256(key, 0 || "payload")` cut to 2. */
    @Test
    fun `a transport code covers the payload and its type`() {
        val key = RadioRoute.scopeKey("us-co")
        assertEquals(0x9C1A, RadioRoute.transportCode(key, 0, "payload".toByteArray()))
        assertEquals(0x1E95, RadioRoute.transportCode(RadioRoute.scopeKey("us"), 0, "payload".toByteArray()))
        assertNotEquals(
            "the payload type is hashed too",
            RadioRoute.transportCode(key, 0, "payload".toByteArray()),
            RadioRoute.transportCode(key, 4, "payload".toByteArray()),
        )
        assertNotEquals(
            RadioRoute.transportCode(key, 0, "payload".toByteArray()),
            RadioRoute.transportCode(key, 0, "other".toByteArray()),
        )
    }

    /**
     * **A scoped request is a transport flood**, and the far end can name the region from the code
     * alone — the name is never on the air. A parser that forgot the four code bytes would read the
     * first code as a path length and mangle everything after it.
     */
    @Test
    fun `a scoped request proves its region`() {
        val host = ByteArray(32) { (it + 1).toByte() }
        val own = ByteArray(32) { (it + 0x40).toByte() }
        val key = RadioRoute.scopeKey("us-co")
        val packet = RadioRoute.routeRequest(1, host, own, key)
        assertNotNull(packet)
        packet!!
        assertEquals("transport flood", 0, packet[0].toInt() and 0x03)
        // Path byte after the two codes, then the payload: no hops yet, one-byte hashes.
        assertEquals(0x00, packet[5].toInt() and 0xFF)
        assertEquals(host[0], packet[6])
        assertEquals(own[0], packet[7])
        assertArrayEquals("LDRQ".toByteArray(), packet.copyOfRange(8, 12))
        // ...and the payload is hashes, marker, key and nonce, which is what the code covers.
        assertEquals(42, packet.size - 6)
        // The same payload recomputes the same code, which is how the region is identified.
        val payload = packet.copyOfRange(6, packet.size)
        val expected = RadioRoute.transportCode(key, 0, payload)
        assertEquals(expected, (packet[1].toInt() and 0xFF) or ((packet[2].toInt() and 0xFF) shl 8))
        assertEquals("us-co", RadioRoute.heardScope(packet, RadioRoute.KNOWN_SCOPE_TAGS))
        // ...and a key that names another region is not it.
        assertNull(RadioRoute.heardScope(packet, listOf("us", "colorado")))
    }

    /** An unscoped request carries no codes, so no region can be attributed to it. */
    @Test
    fun `an unscoped request has no scope to prove`() {
        val packet = RadioRoute.routeRequest(1, ByteArray(32), ByteArray(32))
        assertNotNull(packet)
        assertEquals("plain flood", 1, packet!![0].toInt() and 0x03)
        assertNull(RadioRoute.heardScope(packet, RadioRoute.KNOWN_SCOPE_TAGS))
        // A scoped request builder refuses a key that is not 16 bytes rather than hashing it.
        assertNull(RadioRoute.routeRequest(1, ByteArray(32), ByteArray(32), ByteArray(15)))
    }

    /** A scoped **advert** is a transport flood too, and its path starts after the codes. */
    @Test
    fun `a scoped advert names the host route`() {
        val host = ByteArray(32) { (it + 7).toByte() }
        // header TRANSPORT_FLOOD|ADVERT, two codes, path byte one hop, path 0x0A, then the key.
        val raw = byteArrayOf(
            0x10, 0x00, 0x00, 0x00, 0x00, 0x01, 0x0A,
        ) + host + ByteArray(4)
        val route = RadioRoute.heardAdvert(raw, host)
        assertNotNull(route)
        assertArrayEquals(byteArrayOf(0x0A), route!!.bytes)
    }
}
