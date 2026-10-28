package dev.ldmux.mobile.mesh

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

// ── The seams the driver needs, so it runs against fakes without a socket or a native library ──

/** What the driver uses of the MeshMore gateway client ([MeshGatewayClient] implements it). */
interface GatewayPort {
    val state: StateFlow<MeshLinkState>
    val status: StateFlow<MeshStatus?>
    val received: SharedFlow<ByteArray>
    val uplinkRecordMax: Int
    suspend fun send(record: ByteArray): MeshSendResult
    fun close()
}

/** What the driver uses of an established session ([MeshSession] implements it). */
interface SessionPort {
    fun send(shell: Int, bytes: ByteArray): QueueOutcome
    fun receive(nowMs: Long, record: ByteArray): Received
    fun poll(nowMs: Long, recordsLeft: Int, l2Slots: Int): List<ByteArray>
    fun health(nowMs: Long): SessionHealth
    fun nextDeadlineMs(): Long?
    fun requestClose()
    fun close()
}

/** A started handshake ([PendingHandshake] implements it). Exactly one of finish or abandon. */
interface HandshakePort {
    val message1: ByteArray
    fun finish(message2: ByteArray, recordLimit: Int, nowMs: Long): SessionPort
    fun abandon()
}

/** The features a link may not carry, named for the UI (`Profile.unsupported` bits). */
enum class MeshTermFeature(val bit: Int) {
    Scrollback(MeshTerm.Unsupported.SCROLLBACK),
    Style(MeshTerm.Unsupported.STYLE),
    Hyperlinks(MeshTerm.Unsupported.HYPERLINKS),
    Mouse(MeshTerm.Unsupported.MOUSE),
    Images(MeshTerm.Unsupported.IMAGES),
    Resize(MeshTerm.Unsupported.RESIZE),
    Clipboard(MeshTerm.Unsupported.CLIPBOARD),
}

/** The features the host said are absent, in a fixed order. The UI must not offer these. */
fun absentFeatures(unsupported: Int): List<MeshTermFeature> =
    MeshTermFeature.entries.filter { unsupported and it.bit != 0 }

/** A painted screen. Plain text, one string per row; never half a screen (the driver paints on Commit). */
data class MeshScreen(
    val lines: List<String>,
    val cursorX: Int,
    val cursorY: Int,
    /** A full-screen program is running: rewrites are wholesale, so updates are large and slow. */
    val alternate: Boolean,
    /** The shell has exited. */
    val dead: Boolean,
    val epoch: Int,
)

/**
 * What the mesh terminal is doing, as one value. The three "it is not working" cases are
 * different values on purpose: the cure for each is different.
 */
sealed interface MeshTermView {
    /** Nothing started yet. */
    data object Idle : MeshTermView

    /** The gateway link is not bound, or the session beneath it gave up. The link's own state says why. */
    data class LinkDown(val link: MeshLinkState) : MeshTermView

    /** Link up, handshake with the host under way. Minutes are normal on this link. */
    data object Handshaking : MeshTermView

    /** Session established and `Attach` sent, but the host has not described a terminal yet. */
    data object ProfileMissing : MeshTermView

    /** The host described a terminal. [absent] is what this link cannot carry: show it, do not offer it. */
    data class Live(
        val screen: MeshScreen,
        val absent: List<MeshTermFeature>,
        /** Records the gateway says may still be sent this hour; null when it has not said. */
        val budgetLeft: Int?,
        /** Something is waiting to be sent and the budget is zero or unreported. */
        val stalled: Boolean,
        /** The mirror is being rebuilt after a gap and still shows the previous screen. */
        val resyncing: Boolean,
        /** True once at least one `Commit` has been applied. Before that the screen is blank. */
        val painted: Boolean,
    ) : MeshTermView

    /** The host ended the attach ([reason] is a `MeshTerm.Closed` code) or the peer closed the session (null). */
    data class HostClosed(val reason: Int?) : MeshTermView
}

enum class InputOutcome { Accepted, NotLive, QueueFull, TooLong, Empty }

/**
 * The phone's attach driver for the mesh terminal profile (ADR-0025): the gateway link, the
 * end-to-end session and the row codec, joined.
 *
 * It owns **policy**, and nothing platform or radio:
 *  - never sends input for a shell it is not attached to (a [MeshTermView.Live] view is the only
 *    state that accepts [input]);
 *  - never paints half a screen: `Rows` are held and applied with their `Commit`, and a `Commit`
 *    that does not follow the one before it (and is not a full screen) is discarded and answered
 *    with `Resync`, so a missed update cannot corrupt the screen;
 *  - never transmits past what the gateway says remains this hour (the session's `poll` takes it);
 *  - never retries by itself. A transmit that the radio refused is the ARQ's to retry, with its
 *    own bounds, and a failed session is a visible [MeshTermView.LinkDown].
 *
 * All entry points are suspend and serialised by one mutex, because the session must not be
 * entered from two places at once.
 *
 * @param begin starts a handshake (generates message 1). Wall time and the keys are the caller's.
 * @param clock monotonic milliseconds, the same clock the session's deadlines use.
 */
class MeshTermDriver(
    private val gateway: GatewayPort,
    private val begin: () -> HandshakePort,
    private val selector: String = "",
    private val clock: () -> Long,
    /** A handshake not answered in this long is reported as a dead link. Minutes are normal here. */
    private val handshakeTimeoutMs: Long = 5 * 60_000,
) {
    private val lock = Mutex()
    private val _view = MutableStateFlow<MeshTermView>(MeshTermView.Idle)
    val view: StateFlow<MeshTermView> = _view.asStateFlow()

    private var handshake: HandshakePort? = null
    private var handshakeSince = 0L
    private var session: SessionPort? = null
    private val decoder = TermDecoder()
    private var mirror = RowMirror()
    private var held = ArrayList<TermMessage.Rows>()
    private var lastEpoch: Int? = null
    private var painted = false
    private var resyncing = false
    private var unsupported = 0
    private var terminalView: MeshTermView? = null
    /** A recoverable gateway problem (radio out). Cleared when the link is bound again. */
    private var linkProblem: MeshLinkState? = null

    /** Records that failed a session check (tag, replay, decode). Evidence of noise or an attacker, not a failure. */
    var rejectedRecords = 0
        private set

    /** What the radio answered to the last record it was handed. */
    var lastSendOutcome: MeshSendOutcome? = null
        private set

    // ── lifecycle ───────────────────────────────────────────────────────────────────────

    /** Begin: needs a bound gateway. Sends handshake message 1 as the first record. */
    suspend fun start() = lock.withLock {
        check(handshake == null && session == null) { "already started" }
        val g = gateway.state.value
        if (g != MeshLinkState.Bound && g != MeshLinkState.RadioUnavailable) {
            terminalView = MeshTermView.LinkDown(g); publish(); return@withLock
        }
        val h = try { begin() } catch (e: MeshSessionException) {
            terminalView = MeshTermView.LinkDown(e.state); publish(); return@withLock
        }
        handshake = h
        handshakeSince = clock()
        _view.value = MeshTermView.Handshaking
        transmit(h.message1)
        publish()
    }

    /** The gateway link changed. Anything but bound is a visible [MeshTermView.LinkDown]. */
    suspend fun onGatewayState(state: MeshLinkState) = lock.withLock {
        when (state) {
            MeshLinkState.Bound -> linkProblem = null
            // Not decisions: the link is still coming up or is idle.
            MeshLinkState.Idle, MeshLinkState.Connecting -> {}
            else -> linkProblem = state
        }
        publish()
    }

    /** One record from the gateway's `RECV`. */
    suspend fun onRecord(record: ByteArray) = lock.withLock {
        if (terminalView is MeshTermView.HostClosed) return@withLock
        val h = handshake
        if (h != null) {
            handshake = null
            val s = try {
                h.finish(record, gateway.uplinkRecordMax, clock())
            } catch (e: MeshSessionException) {
                terminalView = MeshTermView.LinkDown(e.state); publish(); return@withLock
            }
            session = s
            _view.value = MeshTermView.ProfileMissing
            queue(TermMessage.Attach(MeshTerm.VERSION, selector))
            pump()
            return@withLock
        }
        val s = session ?: return@withLock
        when (val r = s.receive(clock(), record)) {
            Received.Quiet -> {}
            is Received.Rejected -> rejectedRecords++
            Received.PeerClosed -> { terminalView = MeshTermView.HostClosed(null) }
            is Received.Data -> for (c in r.chunks) if (c.shell == SHELL) feedStream(c.bytes)
        }
        pump()
        publish()
    }

    /** Time passed: retransmit what the session wants, and notice a dead end. */
    suspend fun tick() = lock.withLock {
        val s = session
        if (s != null) {
            val h = s.health(clock())
            if (h.failed && terminalView == null) terminalView = MeshTermView.LinkDown(MeshLinkState.SessionFailed)
            if (h.peerClosed && terminalView == null) terminalView = MeshTermView.HostClosed(null)
            pump()
        } else if (handshake != null && clock() - handshakeSince > handshakeTimeoutMs) {
            handshake?.abandon(); handshake = null
            terminalView = MeshTermView.LinkDown(MeshLinkState.SessionFailed)
        }
        publish()
    }

    /** When the driver next wants [tick], in [clock]'s time, or null. */
    suspend fun nextWakeMs(): Long? = lock.withLock { session?.nextDeadlineMs() }

    // ── what the user does ─────────────────────────────────────────────────────────────

    /**
     * Send keystrokes to the shell. Only while the terminal is live; at most
     * [MeshTerm.MAX_INPUT] bytes per call (a paste is the caller's to split and to rate-limit:
     * the link carries about one record a minute).
     */
    suspend fun input(bytes: ByteArray): InputOutcome = lock.withLock {
        val s = session
        if (s == null || _view.value !is MeshTermView.Live || terminalView != null) return@withLock InputOutcome.NotLive
        if (bytes.isEmpty()) return@withLock InputOutcome.Empty
        if (bytes.size > MeshTerm.MAX_INPUT) return@withLock InputOutcome.TooLong
        val out = when (s.send(SHELL, TermMessage.Input(bytes).encode())) {
            QueueOutcome.Queued -> InputOutcome.Accepted
            QueueOutcome.Full -> InputOutcome.QueueFull
            QueueOutcome.NotOpen -> InputOutcome.NotLive
        }
        if (out == InputOutcome.Accepted) pump()
        publish()
        out
    }

    /** Ask the host for the whole screen again. */
    suspend fun resync() = lock.withLock {
        if (session != null && terminalView == null) { requestResync(); pump(); publish() }
    }

    /** Leave: tell the host, close the session once everything is acknowledged, free the native handle. */
    suspend fun detach() = lock.withLock {
        val s = session
        if (s != null && terminalView == null) {
            queue(TermMessage.Detach)
            try { s.requestClose(); pump() } catch (_: MeshSessionException) {}
        }
        handshake?.abandon(); handshake = null
        s?.close(); session = null
        if (terminalView == null) terminalView = MeshTermView.Idle
        publish()
    }

    /** Drive the whole thing from the gateway's flows until cancelled. */
    suspend fun run() = coroutineScope {
        launch { gateway.received.collect { onRecord(it) } }
        launch { gateway.state.collect { onGatewayState(it) } }
        launch { gateway.status.collect { tick() } }
        start()
        try {
            while (true) {
                val wake = nextWakeMs()
                val wait = if (wake == null) IDLE_TICK_MS else (wake - clock()).coerceIn(50L, IDLE_TICK_MS)
                delay(wait)
                tick()
            }
        } catch (e: CancellationException) {
            detach()
            throw e
        }
    }

    // ── internals (lock held) ──────────────────────────────────────────────────────────

    private fun queue(m: TermMessage) {
        val s = session ?: return
        s.send(SHELL, m.encode())
    }

    private fun requestResync() {
        if (resyncing) return
        resyncing = true
        queue(TermMessage.Resync)
    }

    private fun feedStream(bytes: ByteArray) {
        val items = try {
            decoder.push(bytes)
        } catch (e: TermStreamException) {
            // The stream is unusable: say so and stop rather than painting from garbage.
            terminalView = MeshTermView.LinkDown(MeshLinkState.SessionFailed)
            return
        }
        for (item in items) when (item) {
            is TermItem.Malformed -> requestResync()
            is TermItem.Message -> handle(item.message)
        }
    }

    private fun handle(m: TermMessage) {
        when (m) {
            is TermMessage.Profile -> {
                unsupported = m.unsupported
                mirror.apply(m)
                held.clear(); lastEpoch = null; painted = false
                // The host answered a resync with a fresh profile: the screen that follows is full.
            }
            is TermMessage.Rows -> {
                if (held.isNotEmpty() && held[0].epoch != m.epoch) held.clear()
                held += m
            }
            is TermMessage.Commit -> commit(m)
            is TermMessage.Closed -> terminalView = MeshTermView.HostClosed(m.reason)
            // Phone→host messages from a host are not ours to act on.
            else -> {}
        }
    }

    private fun commit(m: TermMessage.Commit) {
        val rows = held.filter { it.epoch == m.epoch }
        held.clear()
        val full = rows.any { it.full }
        val last = lastEpoch
        val inOrder = last != null && m.epoch == ((last + 1) and 0xffff)
        if (!full && !inOrder) {
            // A diff against a base we do not hold: discard it and ask for the whole screen.
            requestResync()
            return
        }
        for (r in rows) mirror.apply(r)
        mirror.apply(m)
        lastEpoch = m.epoch
        painted = true
        if (full) resyncing = false
    }

    private suspend fun pump() {
        val s = session ?: return
        val left = gateway.status.value?.opsLeftThisHour ?: 0
        val records = try {
            s.poll(clock(), left, L2_WINDOW)
        } catch (e: MeshSessionException) {
            terminalView = MeshTermView.LinkDown(e.state); return
        }
        for (r in records) {
            if (!transmit(r)) break
        }
    }

    /** Hand one record to the gateway. False when the link is gone and the rest should wait. */
    private suspend fun transmit(record: ByteArray): Boolean {
        val result = try {
            gateway.send(record)
        } catch (e: MeshGatewayException) {
            terminalView = MeshTermView.LinkDown(e.state); return false
        }
        lastSendOutcome = result.outcome
        when (result.outcome) {
            // The host is not a contact, or the record is over what the radio takes: retrying
            // changes nothing, so it is a visible failure rather than a silent loop.
            MeshSendOutcome.ErrNotFound -> terminalView = MeshTermView.LinkDown(MeshLinkState.Refused)
            MeshSendOutcome.ErrTooLong -> terminalView = MeshTermView.LinkDown(MeshLinkState.SessionFailed)
            // Busy, a local timeout or an unknown answer: the record was not acknowledged by the
            // host, so the session's own bounded retransmission covers it.
            else -> {}
        }
        return terminalView == null
    }

    private fun publish() {
        val tv = terminalView
        if (tv != null) { _view.value = tv; return }
        val lp = linkProblem
        if (lp != null) { _view.value = MeshTermView.LinkDown(lp); return }
        _view.value = when {
            session == null && handshake == null -> MeshTermView.Idle
            handshake != null -> MeshTermView.Handshaking
            mirror.profile == null -> MeshTermView.ProfileMissing
            else -> {
                val left = gateway.status.value?.opsLeftThisHour
                val pending = session?.let { runCatching { it.health(clock()).pending }.getOrDefault(0) } ?: 0
                MeshTermView.Live(
                    screen = MeshScreen(
                        lines = mirror.lines,
                        cursorX = mirror.cursorX,
                        cursorY = mirror.cursorY,
                        alternate = mirror.flags and MeshTerm.CommitFlags.ALTERNATE != 0,
                        dead = mirror.flags and MeshTerm.CommitFlags.DEAD != 0,
                        epoch = mirror.epoch,
                    ),
                    absent = absentFeatures(unsupported),
                    budgetLeft = left,
                    stalled = pending > 0 && (left == null || left == 0),
                    resyncing = resyncing,
                    painted = painted,
                )
            }
        }
    }

    private companion object {
        /** The one shell number the profile uses on the session. */
        const val SHELL = 0
        /** The gateway's L2 credit window: records it will take unanswered. */
        const val L2_WINDOW = 8
        const val IDLE_TICK_MS = 30_000L
    }
}
