package dev.ldmux.mobile.term

import java.util.concurrent.CountDownLatch
import java.util.concurrent.atomic.AtomicInteger
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The close contract of the native engine, tested through the guard it is built on
 * (the JNI library itself does not load on the JVM): close once, never use after,
 * and a close that races a call waits for it instead of freeing under it.
 */
class GuardedHandleTest {

    @Test
    fun `close destroys exactly once, however many times it is called`() {
        val destroyed = AtomicInteger()
        val seen = ArrayList<Long>()
        val h = GuardedHandle({ 42L }, { destroyed.incrementAndGet(); seen.add(it) })
        assertTrue(h.isOpen)
        h.close()
        h.close()
        h.close()
        assertEquals(1, destroyed.get())
        assertEquals(listOf(42L), seen)
        assertFalse(h.isOpen)
    }

    @Test
    fun `calls after close are no-ops that return their fallback`() {
        val h = GuardedHandle({ 7L }, { })
        assertEquals(7L, h.use(-1L) { it })
        h.close()
        var ran = false
        assertEquals(-1L, h.use(-1L) { ran = true; it })
        assertNull(h.use<String?>(null) { "x" })
        assertFalse(ran, "the block must not run against a freed handle")
    }

    @Test
    fun `a handle that failed to create is closed from the start`() {
        val destroyed = AtomicInteger()
        val h = GuardedHandle({ 0L }, { destroyed.incrementAndGet() })
        assertFalse(h.isOpen)
        assertEquals("none", h.use("none") { "some" })
        h.close()
        assertEquals(0, destroyed.get(), "there is nothing to free")
    }

    @Test
    fun `a close racing a call waits for the call and never overlaps it`() {
        val inCall = CountDownLatch(1)
        val release = CountDownLatch(1)
        val order = java.util.Collections.synchronizedList(ArrayList<String>())
        val h = GuardedHandle({ 1L }, { order.add("destroy") })
        val user = Thread {
            h.use(Unit) {
                order.add("call-start")
                inCall.countDown()
                release.await()
                order.add("call-end")
            }
        }
        user.start()
        inCall.await()
        val closer = Thread { h.close() }
        closer.start()
        Thread.sleep(50) // the closer is blocked on the monitor
        assertFalse(order.contains("destroy"), "destroyed while a call was in flight")
        release.countDown()
        user.join()
        closer.join()
        assertEquals(listOf("call-start", "call-end", "destroy"), order)
    }

    @Test
    fun `hammering use against close never touches a destroyed handle`() {
        repeat(200) {
            val live = java.util.concurrent.atomic.AtomicBoolean(true)
            val bad = AtomicInteger()
            val h = GuardedHandle({ 9L }, { live.set(false) })
            val workers = List(4) {
                Thread {
                    repeat(2000) { h.use(Unit) { if (!live.get()) bad.incrementAndGet() } }
                }
            }
            workers.forEach { it.start() }
            h.close()
            workers.forEach { it.join() }
            assertEquals(0, bad.get())
        }
    }

    @Test
    fun `the engine is inert and close-safe without the native library`() {
        // On the JVM the shim does not load, so this is the closed state from the
        // first call: every operation answers "nothing", and close is repeatable.
        val e = TerminalEngine(80, 24)
        e.write(ByteArray(4), 99)
        e.resize(10, 10)
        assertNull(e.render())
        assertFalse(e.selectionSet(0, 0, 1, 0))
        assertNull(e.selectionText())
        e.close()
        e.close()
        e.write(ByteArray(1), 1)
        assertNull(e.render())
    }
}
