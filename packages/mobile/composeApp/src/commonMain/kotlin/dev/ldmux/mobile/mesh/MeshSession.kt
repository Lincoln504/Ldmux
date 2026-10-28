package dev.ldmux.mobile.mesh

/**
 * The phone's end of the L3 session (ADR-0020, ADR-0023): Noise handshake, the 6+2+16 record
 * and the ARQ. **None of it is implemented here.** One implementation of the suite exists, in
 * Rust (`crates/session`), and this class drives it through [SessionLib], which mirrors the C ABI
 * in `native/ldmux_session.h` one-for-one. What this file owns is the part that is the same on
 * every platform: ownership of the handle, the order of calls, and mapping the ABI's status
 * codes onto the one error vocabulary the mesh package already has, [MeshLinkState].
 *
 * Knows nothing about a radio, a preset, a path or an airtime figure. The only numbers it takes
 * are the record size the gateway advertised (`BIND_OK`) and the count of records the gateway
 * says may still be sent (`STATUS.opsLeftThisHour`), and it never sends more than either.
 *
 * Threading: one coroutine owns an instance, like [MeshGatewayClient]'s read loop. Calls on one
 * instance must not overlap.
 */
class MeshSession internal constructor(
    private val lib: SessionLib,
    handle: Long,
) {
    private var handle: Long = handle

    /** Whether this session can still be used. False after [close]. */
    val isOpen: Boolean get() = handle != 0L

    private fun live(): Long {
        check(handle != 0L) { "session is closed" }
        return handle
    }

    /** The host's static key as the handshake proved it. 32 bytes. */
    fun peerKey(): ByteArray {
        val out = ByteArray(SessionLib.KEY_LEN)
        val rc = lib.peerKey(live(), out)
        if (rc != SessionLib.OK) throw sessionFailure(rc, "peer key")
        return out
    }

    /**
     * Queue bytes for [shell]. All or nothing: [QueueOutcome.Full] leaves the queue as it was, and
     * the caller slows down rather than the session growing.
     */
    fun send(shell: Int, bytes: ByteArray): QueueOutcome {
        require(shell in 0..255) { "shell must fit one byte" }
        return when (val rc = lib.send(live(), shell, bytes)) {
            SessionLib.OK -> QueueOutcome.Queued
            SessionLib.ERR_QUEUE_FULL -> QueueOutcome.Full
            SessionLib.ERR_NOT_OPEN -> QueueOutcome.NotOpen
            else -> throw sessionFailure(rc, "send")
        }
    }

    /**
     * Feed one record from the gateway's `RECV`. A record that fails any check (tag, replay,
     * index, decode) changes no state and comes back as [Received.Rejected]: it is evidence of a
     * noisy link or an attacker, not of a broken session, so it does not end it.
     */
    fun receive(nowMs: Long, record: ByteArray): Received {
        val rc = lib.receive(live(), nowMs, record)
        return when {
            rc == SessionLib.RX_QUIET -> Received.Quiet
            rc == SessionLib.RX_CLOSED -> Received.PeerClosed
            rc == SessionLib.RX_DATA -> Received.Data(drainDelivered())
            rc == SessionLib.ERR_NULL || rc == SessionLib.ERR_PANIC -> throw sessionFailure(rc, "receive")
            else -> Received.Rejected(rc)
        }
    }

    private fun drainDelivered(): List<Delivered> {
        val out = ArrayList<Delivered>()
        val buf = ByteArray(SessionLib.RECORD_MAX)
        val meta = IntArray(2)
        while (true) {
            val rc = lib.takeDelivered(live(), buf, meta)
            if (rc == 0) break
            if (rc != 1) throw sessionFailure(rc, "take delivered")
            out += Delivered(meta[0], buf.copyOf(meta[1]))
        }
        return out
    }

    /**
     * Ask what to transmit now. **Never more than [recordsLeft]** (the gateway's
     * `recordsLeftThisHour`) nor [l2Slots] (the L2 credit window minus records still awaiting
     * `SEND_RESULT`). An unknown budget is the caller's to pass as 0: this class does not guess.
     * Each returned array is one sealed record, ready for `MeshGatewayClient.send`.
     */
    fun poll(nowMs: Long, recordsLeft: Int, l2Slots: Int): List<ByteArray> {
        val n = lib.poll(live(), nowMs, recordsLeft.coerceIn(0, 255), l2Slots.coerceIn(0, 255))
        if (n < 0) throw sessionFailure(n, "poll")
        val out = ArrayList<ByteArray>(n)
        val buf = ByteArray(SessionLib.RECORD_MAX)
        val meta = IntArray(1)
        while (true) {
            val rc = lib.takeRecord(live(), buf, meta)
            if (rc == 0) break
            if (rc != 1) throw sessionFailure(rc, "take record")
            out += buf.copyOf(meta[0])
        }
        return out
    }

    /** End the session once everything queued is acknowledged. */
    fun requestClose() {
        val rc = lib.close(live())
        if (rc != SessionLib.OK) throw sessionFailure(rc, "close")
    }

    /** What the ARQ says about itself. Cheap; call as often as the UI needs. */
    fun health(nowMs: Long): SessionHealth {
        val bits = lib.status(live(), nowMs)
        if (bits < 0) throw sessionFailure(bits, "status")
        return SessionHealth(
            failed = bits and SessionLib.STATUS_FAILED != 0,
            peerClosed = bits and SessionLib.STATUS_PEER_CLOSED != 0,
            needsRekey = bits and SessionLib.STATUS_NEEDS_REKEY != 0,
            pending = lib.pending(live()).coerceAtLeast(0),
            dropped = lib.dropped(live()).coerceAtLeast(0),
        )
    }

    /** When the ARQ next wants a [poll], in the caller's clock, or null if it does not. */
    fun nextDeadlineMs(): Long? {
        val out = LongArray(1)
        return when (val rc = lib.nextDeadline(live(), out)) {
            1 -> out[0]
            0 -> null
            else -> throw sessionFailure(rc, "deadline")
        }
    }

    /** Free the native session. Safe to call twice. */
    fun close() {
        val h = handle
        handle = 0L
        if (h != 0L) lib.free(h)
    }

    companion object {
        /**
         * Begin a handshake. [localPrivate] and [localPublic] are this phone's static keypair,
         * [hostPublic] is the daemon's, [psk] is the 32-byte secret from the pairing QR. [wallMs]
         * is wall-clock Unix milliseconds: the daemon rejects a timestamp outside its window.
         */
        fun begin(
            lib: SessionLib,
            localPrivate: ByteArray,
            localPublic: ByteArray,
            hostPublic: ByteArray,
            psk: ByteArray,
            wallMs: Long,
        ): PendingHandshake {
            require(localPrivate.size == SessionLib.KEY_LEN && localPublic.size == SessionLib.KEY_LEN)
            require(hostPublic.size == SessionLib.KEY_LEN) { "host public key must be 32 bytes" }
            require(psk.size == SessionLib.KEY_LEN) { "psk must be 32 bytes" }
            val out = LongArray(1)
            val msg1 = ByteArray(SessionLib.MSG1_LEN)
            val rc = lib.initiatorStart(localPrivate, localPublic, hostPublic, psk, wallMs, out, msg1)
            if (rc != SessionLib.OK) throw sessionFailure(rc, "handshake start")
            return PendingHandshake(lib, out[0], msg1)
        }

        /** Generate a static keypair: `first` is private, `second` is public. Store it securely. */
        fun generateKeypair(lib: SessionLib): Pair<ByteArray, ByteArray> {
            val priv = ByteArray(SessionLib.KEY_LEN)
            val pub = ByteArray(SessionLib.KEY_LEN)
            val rc = lib.keypair(priv, pub)
            if (rc != SessionLib.OK) throw sessionFailure(rc, "keypair")
            return priv to pub
        }
    }
}

/** A started handshake. Exactly one of [finish] or [abandon] must be called. */
class PendingHandshake internal constructor(
    private val lib: SessionLib,
    private var handle: Long,
    /** Message 1, 121 bytes: send it as one record. */
    val message1: ByteArray,
) {
    /**
     * Consume message 2. [recordLimit] is `BIND_OK`'s advertised record size, so the layout is
     * sized to what the radio will accept. The native handshake is consumed whatever the outcome.
     */
    fun finish(message2: ByteArray, recordLimit: Int, nowMs: Long): MeshSession {
        val h = handle
        check(h != 0L) { "handshake already finished or abandoned" }
        handle = 0L
        val out = LongArray(1)
        val rc = lib.initiatorFinish(h, message2, recordLimit, nowMs, out)
        if (rc != SessionLib.OK) throw sessionFailure(rc, "handshake finish")
        return MeshSession(lib, out[0])
    }

    fun abandon() {
        val h = handle
        handle = 0L
        if (h != 0L) lib.initiatorFree(h)
    }
}

enum class QueueOutcome { Queued, Full, NotOpen }

data class Delivered(val shell: Int, val bytes: ByteArray) {
    override fun equals(other: Any?): Boolean =
        other is Delivered && shell == other.shell && bytes.contentEquals(other.bytes)

    override fun hashCode(): Int = 31 * shell + bytes.contentHashCode()
}

sealed interface Received {
    /** A pure acknowledgement, or a record held until the gap before it is filled. */
    data object Quiet : Received

    data class Data(val chunks: List<Delivered>) : Received

    /** The peer ended the session. Maps to [MeshLinkState.Closed]. */
    data object PeerClosed : Received

    /** Failed a check and changed nothing. [code] is an ABI status, see [SessionLib]. */
    data class Rejected(val code: Int) : Received
}

data class SessionHealth(
    /** Retries were exhausted: the far end stopped answering. Maps to [MeshLinkState.SessionFailed]. */
    val failed: Boolean,
    val peerClosed: Boolean,
    /** 2^16 records or 12 hours: a new handshake is due. Acting on it is not built yet. */
    val needsRekey: Boolean,
    val pending: Int,
    val dropped: Int,
)

/**
 * Failure of the session layer, in the mesh package's one vocabulary. [state] is where the
 * link ends up; [code] is the ABI status for logs and tests. No session secret appears in
 * [message].
 */
class MeshSessionException(val state: MeshLinkState, val code: Int, message: String) :
    Exception(message)

/**
 * Map an ABI status to the link state it means. A handshake the host rejected is the same
 * situation as a refused token, and the same cure (re-pair), so it reuses
 * [MeshLinkState.Refused]; every other failure is [MeshLinkState.SessionFailed].
 */
fun sessionStateFor(code: Int): MeshLinkState = when (code) {
    SessionLib.ERR_BAD_FILTER,
    SessionLib.ERR_STALE,
    SessionLib.ERR_REPLAY,
    SessionLib.ERR_PIN_MISMATCH,
    -> MeshLinkState.Refused
    else -> MeshLinkState.SessionFailed
}

internal fun sessionFailure(code: Int, what: String): MeshSessionException =
    MeshSessionException(sessionStateFor(code), code, "mesh session: $what failed ($code)")
