package io.ldmux.mobile.mesh

import kotlin.test.Test
import kotlin.test.assertEquals
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
    fun connect_puts_at_least_one_frame_on_the_air() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            assertEquals(ClientCode.OK, c.connect(now, now))
            // The first thing a bootstrap owes is its initiation, so one poll must produce a frame.
            val frame = assertNotNull(c.poll(now), "connect produced nothing to write")
            assertTrue(frame.isNotEmpty(), "an empty frame is not a frame")
            // A companion frame is three header bytes and a payload; the engine does not hand out
            // anything else.
            assertTrue(frame.size > 3, "a ${frame.size}-byte frame cannot be a companion frame")
        } finally {
            c.close()
        }
    }

    @Test
    fun a_buffer_that_is_too_small_reports_the_size_and_keeps_the_frame() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            assertEquals(ClientCode.OK, c.connect(now, now))

            val tiny = ByteArray(4)
            val len = IntArray(1)
            val rc = NativeRadioClient.poll(c.handle, now, tiny, len)
            assertEquals(
                ClientCode.ERR_BUFFER, rc,
                "a 4-byte buffer cannot hold a companion frame, so the call must say so",
            )
            val needed = len[0]
            assertTrue(needed > 4, "the call did not report the length it needed (got $needed)")

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
    fun stats_are_filled_from_the_engine() {
        val c = assertNotNull(client(), "the client could not be created")
        try {
            assertEquals(ClientCode.OK, c.connect(now, now))
            assertNotNull(c.poll(now), "connect produced nothing to write")
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
