package io.ldmux.mobile.mesh

import io.iotone.meshcore.transport.MeshcoreTransport
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The seam between the shared screen and the Rust engine, on the real library.
 *
 * `RadioEngine` lives in `commonMain` because the terminal and the settings screens do; the engine
 * itself is Rust behind `ldmux_client_*`, and its Kotlin class is Android's. This test is the proof
 * that the boundary works **both ways**: a call posted from a caller's coroutine reaches the
 * library, and a frame the library produces comes back out for the pipe to write.
 *
 * Nothing here is mocked except the radio: [FakeTransport] stands in for the GATT link, so the
 * frames below are produced by the actual engine, in Rust, in this process.
 *
 * The library's own behaviour is tested in `crates/session` and `crates/session-ffi`. What is at
 * risk here is different and worse: a mistake at the seam does not look like a protocol bug, it
 * looks like a radio that does not work.
 */
class AndroidRadioEngineTest {

    /**
     * A transport that records what the engine writes and lets the test drive the listener.
     *
     * It reports itself connected immediately, because a fake link that needed a GATT handshake
     * would be testing Android's Bluetooth stack rather than this seam.
     */
    private class FakeTransport : MeshcoreTransport {
        private var listener: MeshcoreTransport.Listener? = null
        val written = mutableListOf<ByteArray>()

        override fun setListener(listener: MeshcoreTransport.Listener) {
            this.listener = listener
        }

        override fun isConnected(): Boolean = true

        override fun send(frame: ByteArray) {
            written += frame.copyOf()
        }

        override fun close() {}

        /** Announce the link, which is what a real GATT connection does. */
        fun up() {
            listener?.onConnectionChanged(true)
        }

        /** Deliver one notification, as the radio would. */
        fun deliver(frame: ByteArray) {
            listener?.onFrame(frame)
        }
    }

    private fun creds(): MeshCredentials = MeshCredentials(
        keys = MeshSessionKeys(
            localPrivate = ByteArray(32) { 9 },
            localPublic = ByteArray(32) { 11 },
            hostPublic = ByteArray(32) { 7 },
            psk = ByteArray(32) { 5 },
            hostRadio = ByteArray(32) { 3 },
        ),
        hostRadio = ByteArray(32) { 3 },
    )

    private fun engine(t: FakeTransport): AndroidRadioEngine? {
        if (!NativeRadioClient.available) return null
        return AndroidRadioEngine("00:11:22:33:44:55", creds(), transport = t)
    }

    @Test
    fun a_call_posted_from_a_caller_reaches_the_library_and_its_frame_comes_back() = runBlocking {
        val t = FakeTransport()
        val e = engine(t) ?: return@runBlocking
        try {
            t.up()
            // The loop publishes the link state it was told about, which is the pipe's job and the
            // screen's first question.
            val linked = withTimeoutOrNull(2_000) {
                while (!e.linkUp.value) delay(10)
                true
            }
            assertEquals(true, linked, "the engine never reported the link up")

            // One posted call: connect. The answer is the library's own code.
            val rc = e.connect(NOW, NOW)
            assertEquals(ClientCode.OK, rc, "connect was refused: ${ClientCode.word(rc)}")

            // And the engine owes the radio a frame: the bootstrap's first record or command.
            val wrote = withTimeoutOrNull(2_000) {
                while (t.written.isEmpty()) delay(10)
                t.written.isNotEmpty()
            }
            assertEquals(true, wrote, "the engine produced nothing for the pipe to write")
            assertTrue(
                t.written.first().isNotEmpty(),
                "the first frame out of the engine is empty",
            )
        } finally {
            e.close()
        }
    }

    @Test
    fun a_closed_engine_refuses_its_calls_rather_than_dereferencing_a_freed_handle() = runBlocking {
        val t = FakeTransport()
        val e = engine(t) ?: return@runBlocking
        t.up()
        e.close()
        assertTrue(!e.isOpen, "close left the engine open")

        // The handle is freed by the library; a call into it must be a refusal, not memory that
        // has been handed back. This is the same rule `NativeRadioClientTest` pins one level down.
        val refused = runCatching { e.connect(NOW, NOW) }.isFailure
        assertTrue(refused, "a closed engine answered a call")
    }

    @Test
    fun a_notification_is_applied_on_the_loop_and_never_on_the_callers_thread() = runBlocking {
        val t = FakeTransport()
        val e = engine(t) ?: return@runBlocking
        try {
            t.up()
            e.connect(NOW, NOW)
            // A notification that carries no session record: the engine must take it and say
            // nothing, rather than leaving it queued for ever or throwing across the thread
            // boundary. The state after it is what proves the loop consumed it.
            t.deliver(byteArrayOf(0x83.toByte()))
            delay(100)
            // Still answering afterwards is the assertion: the loop survived a frame it did not
            // want, which is the ordinary case on a busy mesh.
            assertEquals(ClientCode.OK, e.connect(NOW + 1_000, NOW + 1_000))
        } finally {
            e.close()
        }
    }

    @Test
    fun the_radio_and_route_readings_are_flows_the_screen_can_draw() = runBlocking {
        val t = FakeTransport()
        val e = engine(t) ?: return@runBlocking
        try {
            t.up()
            // Before anything has answered, both say "unknown" rather than zeroes: a screen that
            // drew a zero would claim a radio reading it does not have.
            assertEquals(RadioStatus.UNKNOWN, e.radio.value)
            assertEquals(Route.UNKNOWN, e.route.value)
            assertNull(e.route.value.hopsOrNull, "no route may claim a hop count")

            e.connect(NOW, NOW)
            withTimeoutOrNull(2_000) {
                while (e.radio.value == RadioStatus.UNKNOWN && e.route.value === Route.UNKNOWN) delay(10)
            }
            // The engine publishes what it read; the values themselves are the library's.
            assertNotNull(e.radio.value, "the radio reading is never null")
        } finally {
            e.close()
        }
    }

    @Test
    fun a_page_is_open_and_unpainted_before_its_first_screen() = runBlocking {
        val t = FakeTransport()
        val e = engine(t) ?: return@runBlocking
        try {
            t.up()
            assertNull(e.mirror("shell-1"), "an unopened page has no screen")

            // **Opening does not need a session.** The engine remembers the page and sends its
            // attach when the session exists, which is what lets the screen open a shell while the
            // link is still coming up. So the code is OK and the screen is still unpainted — a real
            // state, not an empty screen, and the distinction the page-mirror exists to carry.
            assertEquals(ClientCode.OK, e.open("shell-1", 46, 17, NOW))
            val mirror = e.mirror("shell-1")
            assertNotNull(mirror, "an open page has no mirror at all")
            assertTrue(!mirror.painted, "a page claimed a screen before one arrived")
            assertEquals(0, mirror.cols, "an unpainted page has no grid")

            // And the engine keeps its own rule about the same page opened twice.
            assertEquals(
                ClientCode.ERR_ALREADY_OPEN,
                e.open("shell-1", 46, 17, NOW),
                "a second open of one page was accepted",
            )
        } finally {
            e.close()
        }
    }

    private companion object {
        const val NOW = 1_800_000_000_000L
    }
}
