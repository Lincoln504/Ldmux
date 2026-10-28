package dev.ldmux.mobile.mesh

/**
 * The JNI declarations for `native/ldmux_session_jni.c`. Names must match exactly. This is the
 * whole Android half: each method is an ABI call, and [SessionLib] is its Kotlin face.
 */
object NativeMeshSession : SessionLib {
    /** True if the native library loaded. Never throws. */
    val available: Boolean = try {
        System.loadLibrary("ldmux_session")
        true
    } catch (t: Throwable) {
        false
    }

    external override fun keypair(priv: ByteArray, pub: ByteArray): Int
    external override fun initiatorStart(
        priv: ByteArray, pub: ByteArray, remotePub: ByteArray, psk: ByteArray,
        timestampMs: Long, out: LongArray, msg1: ByteArray,
    ): Int
    external override fun initiatorFinish(hs: Long, msg2: ByteArray, recordLimit: Int, nowMs: Long, out: LongArray): Int
    external override fun initiatorFree(hs: Long)
    external override fun responderAccept(
        priv: ByteArray, pub: ByteArray, psk: ByteArray, pinned: ByteArray?,
        lastTimestampMs: Long, nowMs: Long, msg1: ByteArray, index: Int, recordLimit: Int,
        reply: ByteArray, remoteStatic: ByteArray, out: LongArray,
    ): Int
    external override fun send(h: Long, shell: Int, data: ByteArray): Int
    external override fun receive(h: Long, nowMs: Long, record: ByteArray): Int
    external override fun takeDelivered(h: Long, out: ByteArray, meta: IntArray): Int
    external override fun poll(h: Long, nowMs: Long, recordsLeft: Int, l2Slots: Int): Int
    external override fun takeRecord(h: Long, out: ByteArray, meta: IntArray): Int
    external override fun close(h: Long): Int
    external override fun status(h: Long, nowMs: Long): Int
    external override fun pending(h: Long): Int
    external override fun nextDeadline(h: Long, out: LongArray): Int
    external override fun peerKey(h: Long, out: ByteArray): Int
    external override fun dropped(h: Long): Int
    external override fun free(h: Long)
}

actual fun loadSessionLib(): SessionLib? = if (NativeMeshSession.available) NativeMeshSession else null
