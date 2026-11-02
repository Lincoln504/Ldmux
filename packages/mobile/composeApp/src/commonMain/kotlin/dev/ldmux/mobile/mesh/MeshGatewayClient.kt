package dev.ldmux.mobile.mesh

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.flow.getAndUpdate
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout

/**
 * Where the link to the MeshMore gateway is. One value, so the UI never infers it from
 * several booleans. Every failure is its own state and none falls back to another route.
 */
enum class MeshLinkState {
    /** No radio selected yet. Nothing to connect with. */
    NeedsRadio,

    /** Android refused the BLE permission needed to reach the radio. */
    NeedsPermission,

    /** The host radio is not a contact, or its direct path is missing. */
    NoPath,

    /** Legacy MeshMore gateway token. The radio route does not use it. */
    NeedsToken,
    Idle,
    Connecting,

    /** HELLO, PAIR and BIND done and the radio reports connected. */
    Bound,

    /** Bound, but the radio is not connected (STATUS), or HELLO_OK said so. Sends will fail. */
    RadioUnavailable,

    /** Another app holds the single gateway session (`CLOSE busy`). */
    GatewayBusy,

    /** The gateway speaks a different contract version. */
    ProtocolMismatch,

    /** Nothing is listening on the gateway port: MeshMore is not running, or its switch is off. */
    Unreachable,

    /**
     * The token was refused (rotated or revoked), the host is not a MeshCore contact, or the host
     * rejected the session handshake (wrong pairing secret, a different phone key pinned, a stale
     * or replayed timestamp). All of them are cured by pairing again.
     */
    Refused,

    /**
     * The end-to-end session beneath a bound link gave up: the host stopped answering, or the
     * native session could not run. The L2 link may be fine; the far end is not.
     */
    SessionFailed,

    /** Closed by the gateway or by us. Reconnect is the caller's decision, never automatic. */
    Closed,
}

/** A failure to reach [MeshLinkState.Bound], carrying the state it maps to. */
class MeshGatewayException(val state: MeshLinkState, message: String) : Exception(message)

/** The answer to one [MeshGatewayClient.send]. What the radio did, **never** delivery. */
data class MeshSendResult(val seq: Int, val outcome: MeshSendOutcome)

/**
 * The LDMUX side of the MeshMore gateway.
 *
 * Separate from `LdmuxClient` (the tailnet daemon transport), which it does not touch.
 *
 * Concurrency: exactly one read loop owns the socket's read side and drives the whole state
 * machine, so frames are handled in order. Writes go through [writeLock], so two senders
 * cannot interleave the bytes of two frames. Teardown runs through one `compareAndSet`, so
 * the socket is closed exactly once however many paths (peer close, malformed frame,
 * [close], cancelled scope) reach it, and every pending [send] is failed rather than leaked.
 *
 * What it will not do, on purpose:
 * - It never reconnects by itself. A route that quietly re-opens hides the reason it dropped.
 * - It never retransmits. ADR-0020: a retransmission is a new record with a new counter, and
 *   that is the session's decision, not the carrier's.
 * - It never reports delivery. `SEND_RESULT` is what the radio did.
 *
 * @param token the 16-byte pairing token. A credential: never logged, never in [toString].
 * @param peerPubKey the 32-byte public key of the node to bind (the host's radio).
 */
class MeshGatewayClient(
    private val sockets: MeshSocketFactory,
    private val scope: CoroutineScope,
    private val token: ByteArray,
    private val peerPubKey: ByteArray,
    private val host: String = "127.0.0.1",
    private val port: Int = MeshGateway.DEFAULT_PORT,
    private val capabilities: Int = MeshGateway.CAP_STATUS_PUSH,
    private val handshakeTimeoutMs: Long = 8_000,
) : GatewayPort {
    init {
        require(token.size == MeshGateway.PAIRING_TOKEN_BYTES) { "pairing token must be 16 bytes" }
        require(peerPubKey.size == MeshGateway.PEER_KEY_BYTES) { "peer public key must be 32 bytes" }
    }

    private val _state = MutableStateFlow(MeshLinkState.Idle)
    override val state: StateFlow<MeshLinkState> = _state.asStateFlow()

    private val _status = MutableStateFlow<MeshStatus?>(null)

    /** The last pushed `STATUS`, or null before the first. */
    override val status: StateFlow<MeshStatus?> = _status.asStateFlow()

    /**
     * Downlink records, in the order the gateway sent them, and **none may be dropped here**.
     *
     * This used to be a `SharedFlow` with a 64-record buffer and `tryEmit`, which returns false
     * and loses the record when the collector is busy. The collector is the driver, and the driver
     * holds its lock across a send that may take up to ten seconds, so a screen update's burst of
     * eight records plus a slow radio was enough to overflow it. Losing a row only costs a
     * retransmission, but losing `msg2` is fatal and silent: the phone retries `msg1` for ever and
     * the host accepts every one of them. Measured on the device: eight accepted handshakes, zero
     * bytes received, `lastError` null on both sides, and the terminal saying "the host stopped
     * answering" while the host was answering every message it got.
     *
     * The radio's own lossiness is a separate fact (a raw packet is dropped when no app is
     * connected, `MyMesh.cpp:811-816`), so absence still is not evidence - but a record the
     * gateway handed over must reach the session.
     */
    private val inbox = Channel<ByteArray>(Channel.UNLIMITED)
    override val received: Flow<ByteArray> = inbox.receiveAsFlow()

    private val uplink = MutableStateFlow(0)

    /** Largest uplink record for the current path, from `BIND_OK`. 0 until bound. */
    override val uplinkRecordMax: Int get() = uplink.value

    private val closed = MutableStateFlow(false)
    private val started = MutableStateFlow(false)
    private val writeLock = Mutex()

    /**
     * Completed when bound, failed by [teardown] on every other exit. A member (not a local of
     * [connect]) so that [close] during the handshake releases [connect] at once instead of
     * leaving it to wait out its timeout.
     */
    private val ready = CompletableDeferred<Unit>()

    // Shared mutable state lives in StateFlows so every access is an atomic read or a
    // compare-and-set, with no lock to hold across a suspension and none to leak on teardown.
    private val pending = MutableStateFlow<Map<Int, CompletableDeferred<MeshSendResult>>>(emptyMap())
    private val nextSeq = MutableStateFlow(0)
    private val socket = MutableStateFlow<MeshSocket?>(null)
    private val readLoop = MutableStateFlow<Job?>(null)
    private val terminal = MutableStateFlow<MeshGatewayException?>(null)

    /**
     * Connect and run HELLO, PAIR, BIND. Returns when bound; throws [MeshGatewayException]
     * with the state it ended in otherwise. Callable once.
     */
    suspend fun connect() {
        check(started.compareAndSet(false, true)) { "connect() may be called once; make a new client" }
        _state.value = MeshLinkState.Connecting
        val s = try {
            withTimeout(handshakeTimeoutMs) { sockets.connect(host, port) }
        } catch (e: CancellationException) {
            if (e is TimeoutCancellationException) fail(MeshLinkState.Unreachable, "gateway did not answer")
            teardown(MeshLinkState.Closed, null)
            throw e
        } catch (e: Exception) {
            fail(MeshLinkState.Unreachable, "nothing is listening on $host:$port")
        }
        if (closed.value) {
            // close() ran while we were connecting. Do not leak the socket it never saw.
            s.close()
            throw terminal.value ?: MeshGatewayException(MeshLinkState.Closed, "closed")
        }
        socket.value = s
        readLoop.value = scope.launch { runLoop(s) }
        try {
            withTimeout(handshakeTimeoutMs) {
                write(MeshMessages.hello(capabilities))
                ready.await()
            }
        } catch (e: TimeoutCancellationException) {
            fail(MeshLinkState.Unreachable, "handshake timed out")
        }
    }

    /**
     * Send one record and wait for the gateway's answer for it.
     *
     * Refuses locally (IllegalArgumentException) a record over [uplinkRecordMax] rather than
     * spend airtime on one the radio would reject. The result is what the radio did; a
     * `SentDirect` is not a delivery.
     */
    override suspend fun send(record: ByteArray): MeshSendResult {
        val s = _state.value
        if (s != MeshLinkState.Bound && s != MeshLinkState.RadioUnavailable) {
            throw MeshGatewayException(s, "not bound")
        }
        require(record.isNotEmpty()) { "a record must not be empty" }
        require(record.size <= uplinkRecordMax) {
            "record is ${record.size} bytes, the gateway advertised $uplinkRecordMax"
        }
        val answer = CompletableDeferred<MeshSendResult>()
        val seq = register(answer) ?: throw MeshGatewayException(s, "256 records already in flight")
        // teardown may have drained `pending` just before we registered. Re-check, or this
        // sender would wait forever on an answer nobody will deliver.
        if (closed.value) {
            unregister(seq)
            throw terminal.value ?: MeshGatewayException(MeshLinkState.Closed, "closed")
        }
        try {
            write(MeshMessages.record(seq, record))
        } catch (e: Exception) {
            unregister(seq)
            throw terminal.value ?: MeshGatewayException(MeshLinkState.Closed, "write failed")
        }
        return try {
            answer.await()
        } catch (e: CancellationException) {
            unregister(seq)
            throw e
        }
    }

    /** Claims a free sequence number atomically, or null when all 256 are in flight. */
    private fun register(answer: CompletableDeferred<MeshSendResult>): Int? {
        while (true) {
            val cur = pending.value
            var seq = nextSeq.value and 0xFF
            var tries = 0
            while (cur.containsKey(seq) && tries < 256) {
                seq = (seq + 1) and 0xFF
                tries++
            }
            if (tries >= 256) return null
            if (pending.compareAndSet(cur, cur + (seq to answer))) {
                nextSeq.value = (seq + 1) and 0xFF
                return seq
            }
        }
    }

    private fun take(seq: Int): CompletableDeferred<MeshSendResult>? {
        while (true) {
            val cur = pending.value
            val w = cur[seq] ?: return null
            if (pending.compareAndSet(cur, cur - seq)) return w
        }
    }

    private fun unregister(seq: Int) {
        take(seq)
    }

    /** Idempotent. Closes the socket exactly once and fails every pending [send]. */
    override fun close() {
        teardown(MeshLinkState.Closed, null)
    }

    override fun toString(): String = "MeshGatewayClient($host:$port, ${_state.value})"

    // ---------------------------------------------------------------------------------

    private suspend fun write(frame: MeshFrame) {
        val s = socket.value ?: throw MeshGatewayException(MeshLinkState.Closed, "no socket")
        writeLock.withLock { s.write(frame.encode()) }
    }

    private enum class Phase { Hello, Pair, Bind, Bound }

    private suspend fun runLoop(s: MeshSocket) {
        val decoder = MeshFrameDecoder()
        val buf = ByteArray(1024)
        var phase = Phase.Hello
        var radioConnected = true
        try {
            while (true) {
                val n = s.read(buf)
                if (n < 0) {
                    teardown(MeshLinkState.Closed, MeshGatewayException(MeshLinkState.Closed, "gateway closed the connection"))
                    return
                }
                val frames = decoder.add(buf, n)
                for (f in frames) {
                    // CLOSE is honoured in every phase: it carries the reason.
                    if (f.type == MeshFrameType.CLOSE) {
                        val reason = MeshMessages.readClose(f) ?: MeshCloseReason.Unknown
                        val (st, msg) = when (reason) {
                            MeshCloseReason.Busy -> MeshLinkState.GatewayBusy to "another app holds the gateway"
                            MeshCloseReason.Version -> MeshLinkState.ProtocolMismatch to "gateway speaks another version"
                            MeshCloseReason.PreAuthTimeout -> MeshLinkState.Closed to "gateway timed out waiting for pairing"
                            else -> MeshLinkState.Closed to "gateway closed: ${reason.name}"
                        }
                        teardown(st, MeshGatewayException(st, msg))
                        return
                    }
                    when (phase) {
                        Phase.Hello -> {
                            val ok = MeshMessages.readHelloOk(f)
                            if (ok != null) {
                                if (ok.version != MeshGateway.VERSION) {
                                    return protocolFail(MeshLinkState.ProtocolMismatch, "gateway version ${ok.version}")
                                }
                                radioConnected = ok.radioState == MeshStatus.RADIO_CONNECTED
                                phase = Phase.Pair
                                write(MeshMessages.pair(token))
                            } else if (MeshMessages.readHelloErr(f) != null) {
                                return protocolFail(MeshLinkState.ProtocolMismatch, "hello refused")
                            } else if (isKnown(f.type)) {
                                return protocolFail(MeshLinkState.Closed, "malformed hello answer")
                            }
                        }
                        Phase.Pair -> {
                            if (f.type == MeshFrameType.PAIR_OK) {
                                phase = Phase.Bind
                                write(MeshMessages.bind(peerPubKey))
                            } else if (MeshMessages.readPairErr(f) != null) {
                                return protocolFail(MeshLinkState.Refused, "pairing token refused")
                            } else if (f.type == MeshFrameType.PAIR_ERR) {
                                return protocolFail(MeshLinkState.Closed, "malformed pair answer")
                            }
                        }
                        Phase.Bind -> {
                            val ok = MeshMessages.readBindOk(f)
                            if (ok != null) {
                                uplink.value = ok.uplinkRecordMax
                                phase = Phase.Bound
                                _state.value =
                                    if (radioConnected) MeshLinkState.Bound else MeshLinkState.RadioUnavailable
                                ready.complete(Unit)
                            } else if (MeshMessages.readBindErr(f) != null) {
                                return protocolFail(MeshLinkState.Refused, "the host is not a contact on the phone's radio")
                            } else if (f.type == MeshFrameType.BIND_OK || f.type == MeshFrameType.BIND_ERR) {
                                return protocolFail(MeshLinkState.Closed, "malformed bind answer")
                            } else if (f.type == MeshFrameType.STATUS) {
                                MeshMessages.readStatus(f)?.let { radioConnected = it.radioConnected }
                            }
                        }
                        Phase.Bound -> {
                            if (!onBoundFrame(f)) {
                                return protocolFail(MeshLinkState.Closed, "malformed frame")
                            }
                        }
                    }
                }
                if (decoder.error != null) {
                    // Fatal framing: close, and do not try to resync (contract).
                    return protocolFail(MeshLinkState.Closed, "framing error: ${decoder.error}")
                }
            }
        } catch (e: CancellationException) {
            teardown(MeshLinkState.Closed, null)
            throw e
        } catch (e: Exception) {
            teardown(MeshLinkState.Closed, MeshGatewayException(MeshLinkState.Closed, "socket failed"))
        }
    }

    /** Returns false for a malformed payload on a type we understand. Unknown types: ignored. */
    private suspend fun onBoundFrame(f: MeshFrame): Boolean {
        when (f.type) {
            MeshFrameType.RECV -> {
                // Never closed: the client outlives a session, and a closed inbox would silence
                // every record after one reconnect.
                inbox.trySend(MeshMessages.readRecv(f) ?: return false)
            }
            MeshFrameType.SEND_RESULT -> {
                val (seq, outcome) = MeshMessages.readSendResult(f) ?: return false
                // An answer for a seq nobody is waiting on (cancelled, or never sent) is dropped.
                take(seq)?.complete(MeshSendResult(seq, outcome))
            }
            MeshFrameType.STATUS -> {
                val st = MeshMessages.readStatus(f) ?: return false
                _status.value = st
                val cur = _state.value
                if (cur == MeshLinkState.Bound || cur == MeshLinkState.RadioUnavailable) {
                    _state.value = if (st.radioConnected) MeshLinkState.Bound else MeshLinkState.RadioUnavailable
                }
            }
            // Anything else, known or not, is ignored: the frame set grows without a flag day.
        }
        return true
    }

    private fun isKnown(type: Int): Boolean = type in KNOWN_TYPES

    private fun protocolFail(state: MeshLinkState, msg: String) {
        teardown(state, MeshGatewayException(state, msg))
    }

    private fun fail(state: MeshLinkState, msg: String): Nothing {
        val e = MeshGatewayException(state, msg)
        teardown(state, e)
        throw e
    }

    /**
     * The one exit. Runs at most once (the `compareAndSet`), closes the socket, fails the
     * handshake and every pending send, and leaves a terminal [state].
     */
    private fun teardown(
        state: MeshLinkState,
        error: MeshGatewayException?,
    ) {
        if (!closed.compareAndSet(false, true)) return
        val e = error ?: MeshGatewayException(MeshLinkState.Closed, "closed")
        terminal.value = e
        _state.value = state
        socket.value?.close()
        ready.completeExceptionally(e)
        pending.getAndUpdate { emptyMap() }.values.forEach { it.completeExceptionally(e) }
        // Cancelling the loop from inside itself is fine: it returns right after teardown.
        readLoop.value?.cancel()
    }

    private companion object {
        val KNOWN_TYPES = setOf(
            MeshFrameType.HELLO, MeshFrameType.HELLO_OK, MeshFrameType.HELLO_ERR,
            MeshFrameType.PAIR, MeshFrameType.PAIR_OK, MeshFrameType.PAIR_ERR,
            MeshFrameType.BIND, MeshFrameType.BIND_OK, MeshFrameType.BIND_ERR,
            MeshFrameType.RECORD, MeshFrameType.RECV, MeshFrameType.SEND_RESULT,
            MeshFrameType.STATUS, MeshFrameType.CLOSE,
        )
    }
}
