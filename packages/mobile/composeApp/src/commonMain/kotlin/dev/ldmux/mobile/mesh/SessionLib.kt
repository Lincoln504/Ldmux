package dev.ldmux.mobile.mesh

/**
 * The C ABI of `native/ldmux_session.h` as a Kotlin interface: the platform leaf. Every method
 * is one ABI call and nothing else; there is no logic to get wrong here, which is the point
 * (ADR-0013). Handles are opaque `Long`s: never interpret one. Outputs are written into arrays
 * the caller owns. Return values are the header's status codes.
 *
 * The constants repeat the header's numbers; `MeshSessionAbiTest` loads the real library and
 * fails if either side drifts.
 */
interface SessionLib {
    fun keypair(priv: ByteArray, pub: ByteArray): Int

    /** `out[0]` receives the initiator handle. */
    fun initiatorStart(
        priv: ByteArray,
        pub: ByteArray,
        remotePub: ByteArray,
        psk: ByteArray,
        timestampMs: Long,
        out: LongArray,
        msg1: ByteArray,
    ): Int

    /** Consumes [hs] whatever the outcome. `out[0]` receives the session handle. */
    fun initiatorFinish(hs: Long, msg2: ByteArray, recordLimit: Int, nowMs: Long, out: LongArray): Int

    fun initiatorFree(hs: Long)

    /** `out[0]` = session handle, `out[1]` = timestamp to remember. */
    fun responderAccept(
        priv: ByteArray,
        pub: ByteArray,
        psk: ByteArray,
        pinned: ByteArray?,
        lastTimestampMs: Long,
        nowMs: Long,
        msg1: ByteArray,
        index: Int,
        recordLimit: Int,
        reply: ByteArray,
        remoteStatic: ByteArray,
        out: LongArray,
    ): Int

    fun send(h: Long, shell: Int, data: ByteArray): Int
    fun receive(h: Long, nowMs: Long, record: ByteArray): Int

    /** `meta[0]` = shell, `meta[1]` = length. Returns 1, 0 or an error. */
    fun takeDelivered(h: Long, out: ByteArray, meta: IntArray): Int
    fun poll(h: Long, nowMs: Long, recordsLeft: Int, l2Slots: Int): Int

    /** `meta[0]` = length. Returns 1, 0 or an error. */
    fun takeRecord(h: Long, out: ByteArray, meta: IntArray): Int

    /**
     * As [takeRecord], but `meta[1]` receives the ARQ sequence number that names the record's slot.
     *
     * The number is what [refuseSeq] needs: the radio's send result echoes the sequence *the
     * caller* wrote into the send frame, and only this number maps that answer back to the slot it
     * belongs to. The default keeps fakes and older bindings compiling: it reports `-1`, "unknown",
     * and callers must not refuse a record whose number they do not have.
     */
    fun takeRecordSeq(h: Long, out: ByteArray, meta: IntArray): Int {
        val rc = takeRecord(h, out, meta)
        if (rc == 1) meta[1] = -1
        return rc
    }

    /**
     * Tell the session the radio refused that record without transmitting it. The attempt is given
     * back and the retry follows the radio's transmit-table pace instead of a full retry interval,
     * so a burst the radio could not take becomes a slower burst rather than a dead session.
     *
     * Returns 1 when the slot was in flight, 0 when it was already settled. The default does
     * nothing and reports 0, so a fake or an older binding simply leaves the retry ladder alone.
     */
    fun refuseSeq(h: Long, nowMs: Long, seq: Int): Int = 0
    fun close(h: Long): Int
    fun status(h: Long, nowMs: Long): Int
    fun pending(h: Long): Int
    fun nextDeadline(h: Long, out: LongArray): Int
    fun peerKey(h: Long, out: ByteArray): Int
    fun dropped(h: Long): Int
    fun free(h: Long)

    companion object {
        const val OK = 0
        const val ERR_NULL = -1
        const val ERR_PANIC = -2
        const val ERR_ARG = -3
        const val ERR_BUFFER = -4
        const val ERR_CRYPTO = -10
        const val ERR_MALFORMED = -11
        const val ERR_BAD_FILTER = -12
        const val ERR_STALE = -13
        const val ERR_REPLAY = -14
        const val ERR_PIN_MISMATCH = -15
        const val ERR_WRONG_INDEX = -20
        const val ERR_DUPLICATE = -21
        const val ERR_TOO_OLD = -22
        const val ERR_BAD_TAG = -23
        const val ERR_TOO_LONG = -24
        const val ERR_COUNTER_EXHAUSTED = -25
        const val ERR_DECODE = -26
        const val ERR_RECORD_LIMIT = -27
        const val ERR_QUEUE_FULL = -30
        const val ERR_NOT_OPEN = -31

        const val RX_QUIET = 0
        const val RX_DATA = 1
        const val RX_CLOSED = 2

        const val STATUS_FAILED = 1
        const val STATUS_PEER_CLOSED = 2
        const val STATUS_NEEDS_REKEY = 4

        const val KEY_LEN = 32
        const val MSG1_LEN = 121
        const val MSG2_LEN = 50
        const val RECORD_MAX = 172
    }
}

/**
 * The platform's library, or null where it cannot be loaded. Null is a real answer, not an
 * error: a route that needs the session reports [MeshRouteProblem.SessionNotBuilt] and the app
 * runs on, rather than crashing at startup on a device whose ABI has no build.
 */
expect fun loadSessionLib(): SessionLib?
