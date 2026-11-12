package io.ldmux.mobile.mesh

import io.ldmux.mobile.ble.BleTransport
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The BLE pipe owns exactly three facts: it passes frames up, it passes frames down, and it will not
 * write while a write is outstanding. Everything else about the radio route lives in Rust, so
 * nothing here asserts protocol behaviour — a test that did would be testing the engine through a
 * mock, which is the thing this design exists to avoid.
 */
class AndroidBlePipeTest {
    private class FakeTransport : BleTransport {
        var sink: BleTransport.Listener? = null
        val written = ArrayList<ByteArray>()
        var ready = true
        var closeCount = 0
        var failNextWrite = false

        override fun setListener(listener: BleTransport.Listener?) {
            sink = listener
        }

        override fun isConnected(): Boolean = ready

        override fun send(frame: ByteArray) {
            if (failNextWrite) {
                failNextWrite = false
                throw IllegalStateException("link dropped")
            }
            written += frame
        }

        override fun close() {
            closeCount += 1
            ready = false
        }

        fun arrive(frame: ByteArray) = sink?.onFrame(frame)
        fun link(up: Boolean) {
            ready = up
            sink?.onConnectionChanged(up)
        }
    }

    private fun pipe(
        transport: FakeTransport,
        frames: MutableList<ByteArray> = ArrayList(),
        links: MutableList<Boolean> = ArrayList(),
        problems: MutableList<String> = ArrayList(),
    ) = AndroidBlePipe(
        context = null,
        onFrame = { frames += it },
        onLink = { links += it },
        onProblem = { problems += it },
        transport = transport,
    )

    @Test
    fun a_notification_reaches_the_caller_whole() {
        val t = FakeTransport()
        val frames = ArrayList<ByteArray>()
        val p = pipe(t, frames)
        t.link(true)
        // One BLE notification is one companion frame, and the pipe must not split or join them: it
        // has no parser and no buffer, by design.
        val frame = byteArrayOf(0x3e, 0x02, 0x00, 0x83.toByte(), 0x01)
        t.arrive(frame)
        assertEquals(1, frames.size)
        assertContentEquals(frame, frames[0])
        p.close()
    }

    @Test
    fun a_write_is_passed_through_and_a_frame_is_never_reshaped() {
        val t = FakeTransport()
        val p = pipe(t)
        t.link(true)
        val frame = byteArrayOf(0x3c, 0x01, 0x00)
        assertTrue(p.write(frame), "a ready link refused a write")
        assertEquals(1, t.written.size)
        assertContentEquals(frame, t.written[0])
        p.close()
    }

    @Test
    fun a_write_on_a_link_that_is_not_ready_is_refused_rather_than_queued() {
        val t = FakeTransport()
        val p = pipe(t)
        // No link yet: the caller keeps the frame and tries on its next turn. A queue here would
        // hide an interleaving, which is the defect class this route was rebuilt to remove.
        assertFalse(p.write(byteArrayOf(1, 2, 3)))
        assertEquals(0, t.written.size)
        t.link(true)
        assertTrue(p.write(byteArrayOf(1, 2, 3)))
        p.close()
    }

    @Test
    fun a_dropped_link_is_a_refusal_and_not_a_problem_sentence() {
        val t = FakeTransport()
        val problems = ArrayList<String>()
        val p = pipe(t, problems = problems)
        t.link(true)
        t.failNextWrite = true
        assertFalse(p.write(byteArrayOf(9)), "a write that lost the link must be refused")
        assertFalse(p.connected, "the pipe still believes the link is up")
        assertTrue(
            problems.isEmpty(),
            "a dropped link is ordinary and must not be reported as a fault: $problems",
        )
        p.close()
    }

    @Test
    fun closing_releases_the_transport_once() {
        val t = FakeTransport()
        val p = pipe(t)
        t.link(true)
        p.close()
        assertEquals(1, t.closeCount)
        assertFalse(p.connected)
    }
}
