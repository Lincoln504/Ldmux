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
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.withTimeout

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
    /** The host's column count from its `Profile`; the rows are [lines]. 0 until a profile arrived. */
    val cols: Int = 0,
    /**
     * The host's absolute line number for the first of [lines] (0 when it has none). Two screens
     * whose `firstLine` differ by N say that N rows left the top into history, which is the fact a
     * local scrollback needs and a mirror of screen positions cannot invent.
     */
    val firstLine: Int = 0,
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

    /** No shell was named, so the host was asked which are running and has not finished answering. */
    data object Listing : MeshTermView

    /** The host runs more than one shell and none was named: the user must choose. Nothing is guessed. */
    data class Choose(val entries: List<SessionEntry>) : MeshTermView

    /** The host answered and runs no shell. [MeshTermDriver.refresh] asks again. */
    data object NoShells : MeshTermView

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

/** The host's shells as the header shows them. [current] is the one attached, when known. */
data class MeshShells(val entries: List<SessionEntry>, val current: String?)

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
    /** How long the gateway may take to say what the radio did with one record. */
    private val sendTimeoutMs: Long = SEND_RESULT_TIMEOUT_MS,
) {
    private val lock = Mutex()
    private val _view = MutableStateFlow<MeshTermView>(MeshTermView.Idle)
    val view: StateFlow<MeshTermView> = _view.asStateFlow()

    private var handshake: HandshakePort? = null
    private var handshakeSince = 0L

    /**
     * When the host last said anything. Silence is not failure on a shell that has nothing to
     * report, so this is only consulted while something is unacked: a record the host never
     * acknowledges is the only evidence this route has that the far end is gone, and without
     * this a dead radio looked like a frozen screen.
     */
    private var lastInboundAt = 0L
    private var session: SessionPort? = null
    /** When a dead session is tried again, and how long to wait after the next failure. */
    private var recoverAt = 0L
    private var recoverDelayMs = FIRST_RECOVERY_MS
    private val decoder = TermDecoder()
    private var mirror = RowMirror()
    private var held = ArrayList<TermMessage.Rows>()
    private var lastEpoch: Int? = null
    private var painted = false
    private var resyncing = false
    private var unsupported = 0
    private var terminalView: MeshTermView? = null

    /** What is known about which shell to attach (only used when no selector was given). */
    private enum class Pick { NotNeeded, Probing, Listing, Choosing, None, Attached }
    private var pick = Pick.NotNeeded
    private var pages = ArrayList<SessionEntry>()
    private var choices: List<SessionEntry> = emptyList()
    /** A recoverable gateway problem (radio out). Cleared when the link is bound again. */
    private var linkProblem: MeshLinkState? = null

    /** The shell this attach is on, by the host's own id, when it is known (see [MeshShells]). */
    private var attachedId: String? = null
    /** A list was asked for while attached: its answer fills [shells] and attaches nothing. */
    private var listingForHeader = false
    private val _shells = MutableStateFlow(MeshShells(emptyList(), null))

    /**
     * The host's shells, for the header and the picker. Empty until the host has answered a
     * [listShells]; [MeshShells.current] is null while the attached shell's id is not known (an
     * empty-selector attach to the one shell does not say which it took, so it is learned from the
     * list: exactly one entry is the one).
     */
    val shells: StateFlow<MeshShells> = _shells.asStateFlow()

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
        connect()
    }

    /**
     * Start a session, or say why not. The lock is already held: [start] and the retry in [tick]
     * both end here, and neither may take it again.
     */
    private suspend fun connect() {
        val g = gateway.state.value
        if (g != MeshLinkState.Bound && g != MeshLinkState.RadioUnavailable) {
            failed(g); publish(); return
        }
        val h = try { begin() } catch (e: MeshSessionException) {
            failed(e.state); publish(); return
        }
        handshake = h
        handshakeSince = clock()
        _view.value = MeshTermView.Handshaking
        transmit(h.message1)
        publish()
    }

    /**
     * Replace a session that has spent its nonce budget, without taking the screen away.
     *
     * Nothing failed: the link is up, the host is answering, and the only thing that must
     * change is the keys. Reusing [failed] here would say the host stopped answering at 2^16
     * records or 12 hours, which is the one moment the user has done nothing wrong, and it
     * would back off as if the host were gone. The handshake is immediate, and the new session
     * brings a full screen, so the pane repaints at once.
     */
    private fun rekey() {
        handshake?.abandon()
        handshake = null
        session?.close()
        session = null
        recoverAt = clock()
        recoverDelayMs = FIRST_RECOVERY_MS
    }

    /**
     * The session or the handshake ended badly. A dead session is retried on a schedule; a refused
     * pairing is not, because waiting cannot cure a token or a key and the screen already offers
     * the cure that can.
     */
    private fun failed(state: MeshLinkState) {
        // Drop what failed. A retry that kept the dead session would never run, and the native
        // session holds keys and a queue that only a fresh handshake should replace.
        handshake?.abandon()
        handshake = null
        session?.close()
        session = null
        terminalView = MeshTermView.LinkDown(state)
        if (state == MeshLinkState.SessionFailed) {
            recoverAt = clock() + recoverDelayMs
            recoverDelayMs = (recoverDelayMs * 2).coerceAtMost(MAX_RECOVERY_MS)
        }
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
            // A session that worked earns the fast retry back for the next failure.
            recoverDelayMs = FIRST_RECOVERY_MS
            recoverAt = 0
            _view.value = MeshTermView.ProfileMissing
            // Nothing named: attach with an empty selector first. The host takes the one shell if
            // there is exactly one (the common case, and no extra record on a link that allows
            // about 58 an hour), answers Closed(NOT_FOUND) for none and Closed(AMBIGUOUS) for
            // several, and only then does the driver ask which (below). It never guesses an id.
            pick = if (selector.isEmpty()) Pick.Probing else Pick.Attached
            queue(TermMessage.Attach(MeshTerm.VERSION, selector))
            pump()
            return@withLock
        }
        val s = session ?: return@withLock
        when (val r = s.receive(clock(), record)) {
            Received.Quiet -> {}
            is Received.Rejected -> rejectedRecords++
            Received.PeerClosed -> { terminalView = MeshTermView.HostClosed(null) }
            is Received.Data -> {
                lastInboundAt = clock()
                recoverDelayMs = FIRST_RECOVERY_MS
                for (c in r.chunks) if (c.shell == SHELL) feedStream(c.bytes)
            }
        }
        pump()
        publish()
    }

    /** Time passed: retransmit what the session wants, and notice a dead end. */
    suspend fun tick() = lock.withLock {
        val s = session
        if (s != null) {
            val h = s.health(clock())
            // A session that has spent its nonce budget starts a new one. Nothing is wrong with
            // the link, so this is not [failed]: the screen stays up and only the keys change.
            if (h.needsRekey && terminalView == null) rekey()
            if (h.failed && terminalView == null) failed(MeshLinkState.SessionFailed)
            // **A record the host never answers is a dead link, and the screen must say so.** With
            // nothing outstanding an idle shell is silent by design, so silence alone proves
            // nothing; with something outstanding, a minute of nothing back means the radio, the
            // far end, or both are gone, and the terminal would otherwise sit frozen on the last
            // screen with the user's typing going nowhere.
            if (terminalView == null && h.pending > 0 && clock() - lastInboundAt > SILENCE_FAILURE_MS) {
                failed(MeshLinkState.SessionFailed)
            }
            if (h.peerClosed && terminalView == null) terminalView = MeshTermView.HostClosed(null)
            pump()
        } else if (handshake != null && clock() - handshakeSince > handshakeTimeoutMs) {
            handshake?.abandon(); handshake = null
            failed(MeshLinkState.SessionFailed)
        }
        // The retry itself. Nothing else can bring a dead session back, and the user asking
        // again should not be the mechanism.
        if (recoverAt > 0 && clock() >= recoverAt && session == null && handshake == null) {
            recoverAt = 0
            terminalView = null
            connect()
        }
        publish()
    }

    /** When the driver next wants [tick], in [clock]'s time, or null. */
    suspend fun nextWakeMs(): Long? = lock.withLock {
        val deadline = session?.nextDeadlineMs()
        val retry = recoverAt.takeIf { it > 0 }
        when {
            deadline == null -> retry
            retry == null -> deadline
            else -> minOf(deadline, retry)
        }
    }

    // ── what the user does ─────────────────────────────────────────────────────────────

    /**
     * Send keystrokes to the shell. Only while the terminal is live; at most
     * [MeshTerm.MAX_INPUT] bytes per call (a paste is the caller's to split and to rate-limit:
     * the link carries about one record a minute).
     */
    suspend fun input(bytes: ByteArray): InputOutcome = lock.withLock {
        if (bytes.isEmpty()) return@withLock InputOutcome.Empty
        if (bytes.size > MeshTerm.MAX_INPUT) return@withLock InputOutcome.TooLong
        sendToShell(TermMessage.Input(bytes))
    }

    /**
     * Send named keys (`Up`, `S-Tab`, `C-c`). The host encodes them for the pane's own cursor-key
     * and keyboard mode, which this side cannot see, so an arrow is the right bytes in `vim` and in
     * a plain prompt. Same rules as [input]: only while live, and one record per call.
     */
    suspend fun keys(names: List<String>): InputOutcome = lock.withLock {
        if (names.isEmpty()) return@withLock InputOutcome.Empty
        val message = TermMessage.Key(names)
        val wire = try { message.encode() } catch (_: TermEncodeException) { return@withLock InputOutcome.TooLong }
        if (wire.size > 3 + MeshTerm.MAX_INPUT) return@withLock InputOutcome.TooLong
        sendToShell(message)
    }

    /**
     * The phone's grid in cells, from the surface's own geometry. The host is asked to fit its pty
     * to it once per attached shell and grid (version 3 and above), so a line is never wider than
     * the screen, as on the tailnet. Called again on rotation or a font change; the same grid for
     * the same shell is not asked twice, so a host that clamps it cannot start a loop.
     */
    suspend fun setGrid(cols: Int, rows: Int) = lock.withLock {
        if (cols !in 1..255 || rows !in 1..255) return@withLock
        wantedGrid = cols to rows
        if (session != null && terminalView == null) { fitHost(); pump(); publish() }
    }

    private var wantedGrid: Pair<Int, Int>? = null
    private var requestedFit: Triple<String, Int, Int>? = null
    private var hostVersion = 0

    private fun fitHost() {
        val (cols, rows) = wantedGrid ?: return
        val profile = mirror.profile ?: return
        if (hostVersion < MeshTerm.RESIZE_VERSION) return
        if (profile.cols == cols && profile.rows == rows) return
        val key = Triple(attachedId ?: mirror.shell, cols, rows)
        if (requestedFit == key) return
        requestedFit = key
        queue(TermMessage.Resize(cols, rows))
        // The resize recreates the phone's engine at the new grid, so whatever is on it is gone:
        // the host resizes the pty and, without this, sends nothing at all, which measured on the
        // device as a blank terminal until the shell was left and re-entered. Asking for the whole
        // screen in the same breath costs one record and makes the resize visible immediately.
        queue(TermMessage.Resync)
    }

    private suspend fun sendToShell(message: TermMessage): InputOutcome {
        val s = session
        if (s == null || _view.value !is MeshTermView.Live || terminalView != null) return InputOutcome.NotLive
        val out = when (s.send(SHELL, message.encode())) {
            QueueOutcome.Queued -> InputOutcome.Accepted
            QueueOutcome.Full -> InputOutcome.QueueFull
            QueueOutcome.NotOpen -> InputOutcome.NotLive
        }
        if (out == InputOutcome.Accepted) pump()
        publish()
        return out
    }

    /**
     * Choose one of the shells the host listed. Only an id from the host's own list is accepted:
     * anything else returns false and sends nothing, so a stale picker cannot attach a guess.
     */
    suspend fun choose(id: String): Boolean = lock.withLock {
        if (session == null || terminalView != null || pick != Pick.Choosing) return@withLock false
        if (choices.none { it.id == id }) return@withLock false
        pick = Pick.Attached
        attachedId = id
        publishShells()
        queue(TermMessage.Attach(MeshTerm.VERSION, id))
        pump(); publish()
        true
    }

    /**
     * Ask the host which shells are running **while attached**, to fill [shells]. Attaches nothing:
     * the answer is information for the header, and moving is [selectShell]. Costs one record up and
     * one or more down, so callers use it once on arriving and when the user asks, not on a timer.
     */
    suspend fun listShells() = lock.withLock {
        if (session == null || terminalView != null || pick != Pick.Attached) return@withLock
        listingForHeader = true; pages = ArrayList()
        queue(TermMessage.ListRequest); pump(); publish()
    }

    /**
     * Move to another of the host's shells without leaving the session: one `Attach`, and the host
     * answers with a fresh `Profile` and a full screen. Only an id from the host's own list is
     * accepted, and the shell already attached is a no-op, so a stale header cannot attach a guess
     * or spend a record to stay where it is. The old screen is dropped at once (the view says the
     * host has not described the terminal yet) rather than painting one shell's rows under another's name.
     */
    suspend fun selectShell(id: String): Boolean = lock.withLock {
        if (session == null || terminalView != null || pick != Pick.Attached) return@withLock false
        if (_shells.value.entries.none { it.id == id } || id == attachedId) return@withLock false
        mirror = RowMirror()
        held.clear(); lastEpoch = null; painted = false; resyncing = false; unsupported = 0
        attachedId = id
        publishShells()
        queue(TermMessage.Attach(MeshTerm.VERSION, id))
        pump(); publish()
        true
    }

    /** Ask the host which shells are running again (after [MeshTermView.NoShells], or from the picker). */
    suspend fun refresh() = lock.withLock {
        if (session == null || terminalView != null) return@withLock
        if (pick == Pick.None || pick == Pick.Choosing) {
            pick = Pick.Listing; pages = ArrayList()
            queue(TermMessage.ListRequest); pump(); publish()
        }
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
            failed(MeshLinkState.SessionFailed)
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
                if (pick == Pick.Probing) pick = Pick.Attached
                hostVersion = m.version
                unsupported = m.unsupported
                // The host names the shell it attached (version 2), so the header's current marker
                // is known at once, including after an empty-selector attach that chose for us.
                if (m.shell.isNotEmpty() && m.shell != attachedId) { attachedId = m.shell; publishShells() }
                mirror.apply(m)
                held.clear(); lastEpoch = null; painted = false
                // The host answered a resync with a fresh profile: the screen that follows is full.
                // If its grid is not the phone's, ask it to fit (the answer is another profile).
                fitHost()
            }
            // Before a profile there is no screen to paint into: these are the previous shell's
            // last updates arriving after a switch, and must not be answered with a Resync.
            is TermMessage.Rows, is TermMessage.Commit -> if (mirror.profile != null) handleScreen(m)
            is TermMessage.Closed -> when {
                // The answer to an empty-selector attach: not an ending, a question for the user.
                pick == Pick.Probing && m.reason == MeshTerm.Closed.AMBIGUOUS -> {
                    pick = Pick.Listing; pages = ArrayList(); queue(TermMessage.ListRequest)
                }
                pick == Pick.Probing && m.reason == MeshTerm.Closed.NOT_FOUND -> pick = Pick.None
                // **A shell that ended is not a link that ended.** The session is alive and this host
                // still has other shells, so the answer is the list the user picked from, exactly as
                // the tailnet route returns to its launcher when a shell exits. Ending the session
                // here would spend a whole handshake (minutes on this link) to reach a screen the
                // host can supply in one record. Everything the ended shell left is dropped first.
                m.reason == MeshTerm.Closed.EXITED -> backToList()
                else -> terminalView = MeshTermView.HostClosed(m.reason)
            }
            is TermMessage.Sessions -> onSessions(m)
            // Phone→host messages from a host are not ours to act on.
            else -> {}
        }
    }

    private fun handleScreen(m: TermMessage) {
        when (m) {
            is TermMessage.Rows -> {
                if (held.isNotEmpty() && held[0].epoch != m.epoch) held.clear()
                held += m
            }
            is TermMessage.Commit -> commit(m)
            else -> {}
        }
    }

    /**
     * A page of the shell list. Ignored unless a list was asked for, so a stray one cannot attach
     * anything. A list asked for to choose a shell may attach (one entry) or ask the user; a list
     * asked for by the header only fills [shells].
     */
    /** Forget the ended attach and ask the host what it has now. The session is untouched. */
    private fun backToList() {
        mirror = RowMirror()
        held.clear(); lastEpoch = null; painted = false; resyncing = false; unsupported = 0
        hostVersion = 0
        attachedId = null
        terminalView = null
        pick = Pick.Listing
        pages = ArrayList()
        choices = emptyList()
        publishShells()
        queue(TermMessage.ListRequest)
    }

    private fun onSessions(m: TermMessage.Sessions) {
        if (pick != Pick.Listing && !listingForHeader) return
        pages.addAll(m.entries)
        if (!m.last) return
        val all = pages.distinctBy { it.id }.take(MeshTerm.MAX_SESSIONS)
        pages = ArrayList()
        if (listingForHeader && pick != Pick.Listing) {
            listingForHeader = false
            // Exactly one shell and we did not know which we took: it is that one.
            if (attachedId == null && all.size == 1) attachedId = all[0].id
            _shells.value = MeshShells(all, attachedId)
            return
        }
        when (all.size) {
            0 -> pick = Pick.None
            1 -> { pick = Pick.Attached; attachedId = all[0].id; queue(TermMessage.Attach(MeshTerm.VERSION, all[0].id)) }
            else -> { pick = Pick.Choosing; choices = all }
        }
        _shells.value = MeshShells(all, attachedId)
    }

    private fun publishShells() { _shells.value = MeshShells(_shells.value.entries, attachedId) }

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
            // **Bounded, because the lock is held across it.** Every entry point of this driver (a
            // received record, a keystroke, the tick that notices a dead link) takes the same mutex,
            // so a gateway that never answered froze the screen with nothing able to say so: the
            // silence rule lives in `tick`, which could not run. A record with no answer in time is
            // treated like a busy radio — the session's own retransmission covers it — and the rest
            // wait for the next pump.
            withTimeout(sendTimeoutMs) { gateway.send(record) }
        } catch (e: MeshGatewayException) {
            terminalView = MeshTermView.LinkDown(e.state); return false
        } catch (e: TimeoutCancellationException) {
            return false
        }
        lastSendOutcome = result.outcome
        when (result.outcome) {
            // The host is not a contact, or the record is over what the radio takes: retrying
            // changes nothing, so it is a visible failure rather than a silent loop.
            MeshSendOutcome.ErrNotFound -> terminalView = MeshTermView.LinkDown(MeshLinkState.Refused)
            MeshSendOutcome.ErrTooLong -> failed(MeshLinkState.SessionFailed)
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
            mirror.profile == null -> when (pick) {
                Pick.Listing -> MeshTermView.Listing
                Pick.Choosing -> MeshTermView.Choose(choices)
                Pick.None -> MeshTermView.NoShells
                else -> MeshTermView.ProfileMissing
            }
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
                        cols = mirror.profile?.cols ?: 0,
                        firstLine = mirror.firstLine,
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

/**
 * How long an unanswered record may wait before this route calls the link dead. The ARQ gives up
 * on its own after its attempts; this is the shorter, user-visible bound for the case the ARQ
 * cannot see: a radio that is off the air, where nothing ever comes back at all.
 */
private const val SILENCE_FAILURE_MS = 20_000L

/**
 * A failed session is retried, not mourned.
 *
 * The host restarts, the radio reattaches, the gateway reconnects: each of those leaves the phone
 * holding a session the daemon no longer has, and nothing the user can see explains why the
 * terminal is stuck. Every failed session therefore starts again, doubling the pause so a host
 * that is truly off is not hammered on airtime, and capped so a host that comes back is picked up
 * within half a minute rather than the next time the user reopens the screen.
 */
private const val FIRST_RECOVERY_MS = 2_000L
private const val MAX_RECOVERY_MS = 30_000L

/** Longer than the gateway's own bound on the radio's answer, short enough that `tick` runs again. */
private const val SEND_RESULT_TIMEOUT_MS = 10_000L
