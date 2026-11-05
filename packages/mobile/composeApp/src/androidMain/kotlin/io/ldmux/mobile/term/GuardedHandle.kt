package io.ldmux.mobile.term

/**
 * An owned native handle that can be closed exactly once and never used after.
 *
 * The C core has no locks and `ldmux_terminal_free` releases the memory every other
 * call dereferences (see the threading contract in `native/ldmux_terminal.h`). The
 * engine is driven from the render/IO side and closed from wherever the screen is
 * torn down, which need not be the same thread, and a plain `if (handle != 0)` check
 * followed by the call is a race against a concurrent close: the call can land on
 * freed memory. So every use and the close take one monitor: a close waits for the
 * call in flight, and a call that arrives after it sees the handle gone and returns
 * its fallback instead of reaching native code.
 *
 * The monitor is uncontended in the normal single-thread case, which is the
 * contract; it exists so that violating that contract on teardown is a no-op rather
 * than a use-after-free.
 */
internal class GuardedHandle(create: () -> Long, private val destroy: (Long) -> Unit) {
    @PublishedApi
    internal val lock = Any()

    @PublishedApi
    internal var handle: Long = create()

    /** True until [close], and false from the start if creation failed (handle 0). */
    val isOpen: Boolean get() = synchronized(lock) { handle != 0L }

    /** Run [block] with the live handle, or return [whenClosed] if there is none. */
    inline fun <T> use(whenClosed: T, block: (Long) -> T): T =
        synchronized(lock) {
            val h = handle
            if (h == 0L) whenClosed else block(h)
        }

    /** Release the native state. Idempotent; later [use] calls return their fallback. */
    fun close() {
        synchronized(lock) {
            val h = handle
            if (h != 0L) {
                handle = 0L
                destroy(h)
            }
        }
    }
}
