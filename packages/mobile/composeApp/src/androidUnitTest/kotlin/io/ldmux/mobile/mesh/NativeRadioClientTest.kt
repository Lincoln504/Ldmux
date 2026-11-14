package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * Loads the **real** Rust client engine through the real JNI shim (`native/ldmux_client_jni.c`,
 * built by `native/build-session.sh host`, found through `java.library.path`) and calls it the way
 * an Android caller would. Nothing is mocked: the frames below come out of the actual engine.
 *
 * What is asserted here is the boundary, not the protocol — the protocol has its own tests in
 * `crates/session` and `crates/session-ffi`. A mistake at this seam does not look like a bug in the
 * protocol; it looks like a radio that does not work, which is why it gets its own test:
 *
 * - the handle's liveness is the library's rule, not a second one invented in Kotlin;
 * - a buffer that is too small reports the size it needed and **loses nothing**;
 * - a null handle is an error rather than a crash.
 */
class NativeRadioClientTest {
    private val now = 1_800_000_000_000L

    private fun requireLib(): Unit = assertTrue(
        NativeRadioClient.available,
        "libldmux_session.so did not load: run packages/mobile/native/build-session.sh host " +
            "(needs cargo and a JDK) and check java.library.path",
    )

    private fun client(): RadioClient? {
        requireLib()
        return RadioClient.create(
            peerStatic = ByteArray(32) { 7 },
            psk = ByteArray(32) { 5 },
            localPrivate = ByteArray(32) { 9 },
            localPublic = ByteArray(32) { 11 },
            pathWidth = 1,
            sf = 7,
            floodReachHops = 4,
            airtimeMs = 602.6f,
        )
    }

    @Test
    fun a_client_is_created_and_a_second_free_is_a_no_op() {
        requireLib()
        val slot = LongArray(1)
        val rc = NativeRadioClient.create(
            ByteArray(32) { 7 }, ByteArray(32) { 5 }, ByteArray(32) { 9 }, ByteArray(32) { 11 },
            1, 7, 4, 0, 602.6f, 0, slot,
        )
        assertEquals(ClientCode.OK, rc, "a valid configuration was refused")
        assertTrue(slot[0] != 0L, "the library did not report a handle")

        NativeRadioClient.free(slot)
        assertEquals(0L, slot[0], "free must null the caller's slot, which is what makes a reuse a null")

        // The second free goes through the same slot, so the library has nothing to free. It must not
        // crash and must not resurrect anything.
        NativeRadioClient.free(slot)
        assertEquals(0L, slot[0], "a second free changed the slot")
        assertEquals(ClientCode.ERR_NULL, NativeRadioClient.connect(slot[0], now, now))
    }

    @Test
    fun an_impossible_path_width_is_refused() {
        requireLib()
        val slot = LongArray(1)
        val rc = NativeRadioClient.create(
            ByteArray(32) { 7 }, ByteArray(32) { 5 }, ByteArray(32) { 9 }, ByteArray(32) { 11 },
            0, 7, 4, 0, 602.6f, 0, slot,
        )
        assertEquals(ClientCode.ERR_ARG, rc, "a path width of zero is not a legal hash width")
        assertEquals(0L, slot[0])
    }

    @Test
    fun a_null_handle_is_an_error_rather_than_a_crash() {
        requireLib()
        val nullHandle = 0L
        assertEquals(ClientCode.ERR_NULL, NativeRadioClient.connect(nullHandle, now, now))
        assertEquals(ClientCode.ERR_NULL, NativeRadioClient.established(nullHandle))
        assertEquals(ClientCode.ERR_NULL, NativeRadioClient.setRecordsLeft(nullHandle, 10))
        assertEquals(ClientCode.ERR_NULL, NativeRadioClient.open(nullHandle, "abcd".encodeToByteArray(), 46, 20, now))
    }

    @Test
    fun connect_asks_the_radio_who_it_is_before_anything_else() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            assertEquals(ClientCode.OK, c.connect(now, now))
            // **The radio comes up first.** `APP_START` is the companion hello: the firmware latches
            // the protocol version from `DEVICE_QUERY` and answers everything else in the shapes
            // that version selects, so nothing may precede it.
            val frame = assertNotNull(c.poll(now), "connect produced nothing to write")
            assertEquals(0x01, frame[0].toInt(), "the first command must be APP_START")
            // A payload, not a serial frame: a BLE characteristic carries `[opcode][args]`, and the
            // host adds `<` and a length when it writes to a port.
            assertTrue(frame[0] != 0x3C.toByte(), "the engine emits a payload, not a serial frame")

            // **Nothing answers here, so the window has to time out to move on** — which is exactly
            // what a radio that stopped answering does on a real link. Advancing the clock past each
            // command's deadline is what lets the queue reach the record send behind them.
            var seenSend = false
            var guard = 0
            // **Do not stop on an empty poll.** A poll with nothing to write is a window still
            // waiting for an answer, not the end of what the engine owes; the clock is what frees it.
            while (guard++ < 40 && !seenSend) {
                val next = c.poll(now + guard * 2_000L)
                if (next != null && next.isNotEmpty() && next[0] == 0x41.toByte()) seenSend = true
            }
            assertTrue(seenSend, "no record was ever handed to the radio")
        } finally {
            c.close()
        }
    }

    @Test
    fun a_buffer_that_is_too_small_reports_the_size_and_keeps_the_frame() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            assertEquals(ClientCode.OK, c.connect(now, now))

            // The first frame is `APP_START`, which is one byte: an empty buffer is the smallest
            // thing that cannot hold it.
            val tiny = ByteArray(0)
            val len = IntArray(1)
            val rc = NativeRadioClient.poll(c.handle, now, tiny, len)
            assertEquals(
                ClientCode.ERR_BUFFER, rc,
                "an empty buffer cannot hold a command, so the call must say so",
            )
            val needed = len[0]
            assertTrue(needed > 0, "the call did not report the length it needed (got $needed)")

            // **Nothing was lost by the refused call**: the same frame comes out of the next one.
            val roomy = ByteArray(needed)
            val len2 = IntArray(1)
            assertEquals(1, NativeRadioClient.poll(c.handle, now, roomy, len2))
            assertEquals(needed, len2[0], "the frame that came out is not the one that was measured")
            assertTrue(roomy.copyOf(len2[0]).isNotEmpty())
        } finally {
            c.close()
        }
    }

    @Test
    fun an_unasked_radio_reports_nothing_rather_than_a_guess() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            // **No answer, no reading.** A radio nothing has heard from reports no RSSI rather than a
            // plausible number, and a link nobody has answered has no path — which is why the next
            // attempt would flood. That is the whole reason the two structs carry a `has_` byte per
            // optional fact instead of zero, which is a real temperature.
            val status = c.radio()
            assertFalse(status.connected, "nothing has said the radio is there")
            assertNull(status.rssiDbm, "no packet has been heard")
            assertNull(status.batteryMv, "no battery has been read")
            assertNull(status.preset, "no tuple has been read")
            assertNull(status.opsLeft, "no budget has been reported")

            val route = c.route(now)
            assertFalse(route.known, "no path has been heard")
            assertTrue(route.flood, "with no path the first attempt is a flood")
            assertNull(route.hopsOrNull, "a route nobody knows has no hop count")
        } finally {
            c.close()
        }
    }

    @Test
    fun a_scope_is_sixteen_bytes_of_region_key_or_cleared() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            // The region's key, not the 16-bit code the config carries: the code is an HMAC *of* the
            // key, so a shorter buffer would name a different region.
            assertEquals(ClientCode.OK, c.setScope(ByteArray(16) { 0x5A }))
            assertEquals(ClientCode.OK, c.setScope(ByteArray(0)), "an empty key clears the scope")
            assertEquals(
                ClientCode.ERR_ARG, c.setScope(ByteArray(15)),
                "a 15-byte key is not a region and must not be truncated into one",
            )
        } finally {
            c.close()
        }
    }

    @Test
    fun stats_are_filled_from_the_engine() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            assertEquals(ClientCode.OK, c.connect(now, now))
            // **Past the discovery grace.** The first rung waits up to three seconds for the
            // radio's advert cache to name a path, so a poll at the same instant legitimately
            // produces nothing; the counter this test is about only moves once a rung is sent.
            assertNotNull(c.poll(now + 5_000), "connect produced nothing to write")
            val s = assertNotNull(c.stats(), "the engine would not report its counters")
            assertTrue(
                s.recordsSent + s.rungsUsed > 0,
                "the engine sent a frame but counted nothing: $s",
            )
        } finally {
            c.close()
        }
    }

    @Test
    fun a_page_that_is_not_open_has_no_screen_and_cannot_be_laid_out() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            // No session is needed to learn that: the page table is the client's own.
            assertNull(c.mirror("not-open"), "a closed shell has no mirror")
            assertEquals(ClientCode.ERR_NO_PAGE, c.fetch("not-open", 46, 20, now))
        } finally {
            c.close()
        }
    }
}
