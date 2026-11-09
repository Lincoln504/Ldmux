package io.ldmux.mobile.mesh

import io.ldmux.mobile.appLog
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.withTimeout

// ── The seams the driver needs, so it runs against fakes without a socket or a native library ──

/** What the driver uses of the carrier ([MeshRadioGateway] implements it). */
interface GatewayPort {
    val state: StateFlow<MeshLinkState>
    val status: StateFlow<MeshStatus?>
    val received: Flow<ByteArray>
    val uplinkRecordMax: Int

    /** Repeaters in the route to the host, or null when no route is known. */
    val routeHops: Int? get() = null

    /**
     * Why the link is down, in the link's own words, or null when it has not said.
     *
     * A state is a category and there are five ways to be unable to reach a radio - Bluetooth off,
     * no permission, no bond, the wrong firmware, another app holding it - so the category alone
     * left the screen saying "radio unavailable" to all of them. Defaulted, because a test double
     * has no radio to be wrong about.
     */
    val problem: String? get() = null

    suspend fun send(record: ByteArray): MeshSendResult

    /**
     * Re-read the route to the host now, because a shell is being opened.
     *
     * A route is learned from the air, so it is only true while the repeaters that carried it are
     * still there. Defaulted: a test double has no radio to ask, and a carrier with no route to
     * learn answers nothing.
     */
    suspend fun refreshRoute() {}

    /**
     * This radio's own public key, once the link is bound; null before that or when the link has no
     * radio to ask.
     *
     * It is what the route hint carries (see [RouteHint]): without it the host cannot look up a
     * return route, and a phone behind a repeater never gets an answer.
     */
    val radioPublicKey: ByteArray? get() = null
}

/** One sealed record and the ARQ sequence number that names its slot. */
data class SealedRecord(
    /** The ARQ slot. [SessionPort.UNKNOWN_SEQ] when the binding cannot report it. */
    val seq: Int,
    /** The sealed bytes, ready for the radio. */
    val bytes: ByteArray,
) {
    override fun equals(other: Any?): Boolean =
        other is SealedRecord && seq == other.seq && bytes.contentEquals(other.bytes)

    override fun hashCode(): Int = 31 * seq + bytes.contentHashCode()
}

/** What the driver uses of an established session ([MeshSession] implements it). */
interface SessionPort {
    fun send(shell: Int, bytes: ByteArray): QueueOutcome
    fun receive(nowMs: Long, record: ByteArray): Received
    fun poll(nowMs: Long, recordsLeft: Int, l2Slots: Int): List<ByteArray>

    /**
     * As [poll], but each record keeps the ARQ sequence number that names its slot.
     *
     * The radio's send result echoes the sequence the *caller* wrote into the send frame. That
     * number is the caller's, not the ARQ's, so [refuseSeq] needs this one to reach the right slot.
     * The default reports [UNKNOWN_SEQ], which callers must treat as "cannot refuse this one":
     * refusing the wrong slot gives an attempt back to a record that really did go out.
     */
    fun pollWithSeq(nowMs: Long, recordsLeft: Int, l2Slots: Int): List<SealedRecord> =
        poll(nowMs, recordsLeft, l2Slots).map { SealedRecord(UNKNOWN_SEQ, it) }

    /**
     * Tell the session the radio refused that record without transmitting it.
     *
     * A refusal means the transmit table was full or the path was not usable: the packet never
     * reached the air. The ARQ gives the attempt back and retries at the radio's pace, so a burst
     * the radio could not take becomes a slower burst rather than a dead session. Returns true when
     * the slot was in flight. The default does nothing.
     */
    fun refuseSeq(nowMs: Long, seq: Int): Boolean = false

    fun health(nowMs: Long): SessionHealth
    fun nextDeadlineMs(): Long?
    fun requestClose()
    fun close()

    companion object {
        /** A binding that cannot report the ARQ sequence number reports this. */
        const val UNKNOWN_SEQ = -1
    }
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

    /**
     * The gateway link is not bound, or the session beneath it gave up. [link] says the kind of
     * failure; [detail] is the link's own sentence about this one, when it has one.
     */
    data class LinkDown(val link: MeshLinkState, val detail: String? = null) : MeshTermView

    /** Link up, handshake with the host under way. A few seconds are ordinary on this link. */
    data object Handshaking : MeshTermView

    /**
     * A handshake has been asked for again and again, and nothing has come back.
     *
     * **A try that keeps failing is a verdict, not a spinner.** The ladder keeps running behind this
     * (a re-ask, then a rebuilt transport), so the link heals by itself when the host comes back —
     * but the person is owed the sentence rather than a counter that climbs for a minute and then
     * says "dropped" about a link that was never up (measured on the device, 2026-11-07: the host
     * radio answered pushes and not commands, and the phone sat on `handshaking · 59 s`).
     */
    data object NoAnswer : MeshTermView

    /** Session established and `Attach` sent, but the host has not described a terminal yet. */
    data object ProfileMissing : MeshTermView

    /** No shell was named, so the host was asked which are running and has not finished answering. */
    data object Listing : MeshTermView

    /** The host runs more than one shell and none was named: the user must choose. Nothing is guessed. */
    data class Choose(val entries: List<SessionEntry>) : MeshTermView

    /**
     * Another device holds the shell, and the host refused the attach rather than stealing it.
     *
     * This is the mesh route's `HeldBy`: the desk window has it, and the person here can take it
     * back (`Takeover`) or leave it alone. Without this the phone either stole the shell silently
     * or sat on a screen nobody was feeding.
     */
    data class HeldBy(val shell: String) : MeshTermView

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
 * The phone's attach driver for the mesh terminal profile (ADR-0025): the radio link, the
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
    /**
     * A handshake not answered in this long is reported as a dead link.
     *
     * A handshake is three small records, so a minute is already generous - the old five made a
     * lost `msg1` look like a dead host for most of a coffee break. The retry below is what makes
     * a short timeout safe: the wait is retried every [handshakeRetryMs], not once.
     */
    private val handshakeTimeoutMs: Long = 60_000,
    /**
     * The first interval before an unanswered handshake is asked again, with fresh keys each time.
     *
     * **It must exceed the round trip, or the retry destroys the answer it is waiting for.** A
     * handshake is three records each way through the ARQ on a half-duplex radio; several seconds is
     * ordinary. At six seconds the driver abandoned a handshake just as `msg2` was arriving, and the
     * late record was handed to the *new* handshake and failed to decode - measured on the device,
     * 2026-11-01: `handshakes 5 accepted` on the host while the phone still said `handshaking`, and
     * the counter resetting every six seconds. Doubling from here (CoAP's ladder, RFC 6298's
     * backoff) gives 8 s, 16 s, 32 s.
     */
    private val handshakeRetryMs: Long = 8_000,
    /**
     * How long an unanswered handshake is drawn as `handshaking` before the app says no answer came.
     *
     * Two re-asks (8 s and 16 s) plus a full round trip on a narrow channel fit inside this; the
     * ladder itself keeps going, so this changes the sentence and not the retrying.
     */
    private val handshakeVerdictMs: Long = 30_000,
    /** How many re-asks before the transport itself is judged dead and the connection rebuilt. */
    private val handshakeRetries: Int = 3,
    /** How long the gateway may take to say what the radio did with one record. */
    private val sendTimeoutMs: Long = SEND_RESULT_TIMEOUT_MS,
) {
    /**
     * Called when the host says the pane has stopped changing (`Commit` with the idle flag).
     *
     * The source wires this to its hold, so a sync's wait ends at the moment the answer ends
     * instead of on a local four-second quiet timer.
     */
    var onIdle: (() -> Unit)? = null

    private val lock = Mutex()
    private val _view = MutableStateFlow<MeshTermView>(MeshTermView.Idle)
    val view: StateFlow<MeshTermView> = _view.asStateFlow()

    /**
     * Which attempt at a connection this is.
     *
     * A wait screen wants to count the seconds of *this* try. Counting the screen's own age was
     * wrong the moment the driver started retrying underneath it: the number climbed through
     * attempts that were working (measured: 290 s on a link being re-asked every six seconds),
     * which reads as a hang rather than as progress.
     */
    private val _attempt = MutableStateFlow(0)
    val attempt: StateFlow<Int> = _attempt.asStateFlow()

    private var handshake: HandshakePort? = null
    private var handshakeSince = 0L
    /**
     * When this link's *first* handshake started, and 0 when none is running.
     *
     * **[handshakeSince] restarts on every re-ask**, which is right for the retry ladder and wrong
     * for a verdict: a host that never answers is re-asked at 8 s and 16 s, so a clock reset with
     * each attempt would never reach thirty seconds. This one survives the attempts and is cleared
     * when a session is established or the link is given up.
     */
    private var handshakeBeganAt = 0L

    /**
     * Set when this connection cannot be revived from the inside.
     *
     * **A transport that has stopped carrying records cannot be fixed by re-handshaking over it.**
     * The retry sends `msg1` into the same socket the host stopped answering on, waits out the
     * handshake timeout, and tries again for ever - which is what "the host stopped answering"
     * looked like while the host was up and answering everything it was given (measured
     * 2026-10-31: an accepted handshake count that kept climbing with zero bytes delivered). The
     * only cure is a new socket, and that is the caller's to build: `run` returns here so the
     * screen can rebuild the connection, which is what pressing refresh did by hand.
     */
    private var transportDead = false

    /** How many times this connection has re-asked an unanswered handshake. */
    private var handshakeTries = 0

    /**
     * When the host last said anything. Silence is not failure on a shell that has nothing to
     * report, so this is only consulted while something is unacked: a record the host never
     * acknowledges is the only evidence this route has that the far end is gone, and without
     * this a dead radio looked like a frozen screen.
     */
    private var lastInboundAt = 0L

    /**
     * When this link last *carried* anything, in either direction.
     *
     * The silence rule used to measure from [lastInboundAt] alone, and on an idle shell that is
     * arbitrarily old: a link that has been quiet for a minute has no inbound record to point at,
     * so the moment anything was queued — opening a shell from the list, a keystroke after a pause,
     * coming back to a shell — the very next tick saw `pending > 0` and an inbound age of minutes
     * and declared the session dead **before the record had been sent**. The screen went straight
     * to "the link dropped", the connection was torn down, and pressing refresh (which rebuilds it)
     * was the only cure. That is the recurring report, and this is the mechanism.
     */
    private var lastCarriedAt = 0L
    private var session: SessionPort? = null
    /** When a dead session is tried again, and how long to wait after the next failure. */
    private var recoverAt = 0L
    private var recoverDelayMs = FIRST_RECOVERY_MS

    /** The record size this session was sealed at, and when the last resize re-handshake ran. */
    private var sessionLimit = 0
    private var lastResizeAt = 0L

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
    /** The link's own sentence about [linkProblem], when it had one to give. */
    private var linkDetail: String? = null

    /** The shell this attach is on, by the host's own id, when it is known (see [MeshShells]). */
    private var attachedId: String? = null

    /**
     * The id the host answered `Create` with, before the attach that follows lands.
     *
     * Held rather than inferred: the attach is queued on the answer, so the screen can show "the
     * host is starting a shell" for exactly the shell it asked for instead of guessing from a list
     * another client may have changed in between.
     */
    private var createdId: String? = null

    /** The selector of the attach in flight, so a `HELD` refusal can name the shell it was for. */
    private var attempted: String? = null

    /**
     * Whether the attach on the wire named a shell, or asked the host to pick.
     *
     * The two read a refusal differently: `HELD` names a shell only for a named attach, and
     * `AMBIGUOUS` is the *answer* to an unnamed one ("you have several, name one") rather than an
     * ending. Kept as a flag because `pick` moves on — the launcher's own list request sets it to
     * `Listing` one tick later — and the refusal then looked like a closed page.
     */
    private var namedAttach = false

    /**
     * Set while the list is showing a shell this phone deliberately let go.
     *
     * The list's one-shell convenience — "exactly one shell and we did not know which we took: it
     * is that one" — would otherwise attach again the instant the list arrived, so going home
     * released the shell and the next list re-took it: the desk window never got it back, the pty
     * was fitted to the phone's list height, and the phone was never actually on its home screen.
     * It is set when a release happens and when a launch attach is refused as held, and cleared
     * only by the user opening something.
     */
    private var listHoldsNothing = false
    /** A list was asked for while attached: its answer fills [shells] and attaches nothing. */
    private var listingForHeader = false

    /** Pages of a list the host pushed (it was not asked for), accumulated until `last`. */
    private var pushedPages: ArrayList<SessionEntry>? = null
    private val _shells = MutableStateFlow(MeshShells(emptyList(), null))

    /**
     * The host's shells, for the header and the picker. Empty until the host has answered a
     * [listShells]; [MeshShells.current] is null while the attached shell's id is not known (an
     * empty-selector attach to the one shell does not say which it took, so it is learned from the
     * list: exactly one entry is the one).
     */
    val shells: StateFlow<MeshShells> = _shells.asStateFlow()

    /**
     * The marks the host sent with the attach: which shells finished, which want a look.
     *
     * The radio route replays rather than pushes (`MeshTerm.Marks`, version 9): the table is what the
     * phone missed while it was away, and it is drawn beside the launcher's rows. Opening a shell
     * removes its mark, because opening it is reading it.
     */
    private val _marks = MutableStateFlow<Map<String, ShellMark>>(emptyMap())
    val marks: StateFlow<Map<String, ShellMark>> = _marks.asStateFlow()

    /**
     * The pages of a mark table that has not finished arriving, or null between runs.
     *
     * A table of a dozen shells does not fit one message, so the host pages it and only the page that
     * says `last` is committed to [marks]. This is the other half of that rule; a page that opens a
     * run (`first`) replaces it, so a lost tail cannot merge next week's table into this one.
     */
    private var markPage: Map<String, ShellMark>? = null

    /**
     * Drop a half-arrived page run.
     *
     * Called wherever the shell or the connection changes: a run whose `first` page was lost would
     * otherwise be merged into by the next run, and the launcher would show a table that never
     * existed. The `first` flag makes that unlikely rather than impossible; this makes it
     * impossible.
     */
    private fun forgetMarkPage() {
        markPage = null
    }

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
        // A new connection gets a full set of handshake re-asks. `connect` itself is also the
        // retry path inside one connection, which is why it does not reset this.
        handshakeTries = 0
        connect()
    }

    /**
     * Start a session, or say why not. The lock is already held: [start] and the retry in [tick]
     * both end here, and neither may take it again.
     */
    private suspend fun connect() {
        _attempt.value += 1
        val g = gateway.state.value
        if (g != MeshLinkState.Bound) {
            failed(g); publish(); return
        }
        // **Every attempt asks where the host is, not just the first.** A retry is the moment the
        // route matters most: the previous attempt's records went somewhere and nothing came back,
        // and the route the phone is holding was learned wherever it was before. The refresh is a
        // serial read plus, at most, one flooded request — no more than the record the retry is
        // about to spend on a path that may no longer exist.
        runCatching { gateway.refreshRoute() }
        val h = try { begin() } catch (e: MeshSessionException) {
            failed(e.state); publish(); return
        }
        // **Tell the host which radio this is, before the first session record.** A direct packet's
        // path is consumed by the repeaters that carry it, so the host cannot learn a return route
        // from anything we send; it needs our radio's public key to look the route up in its own
        // radio's advert-path cache. Thirty-six bytes, unsealed, before the handshake — the first
        // answer we need is the handshake's, and that answer has to know where to go.
        //
        // **A refused hint is retried before the handshake goes.** The link is bound and idle, so a
        // refusal here is a transient one; sending the handshake anyway costs a whole rung of the
        // ladder (8 s, doubling) for a session the host could not answer. Bounded, so a radio that
        // will not take it still gets the handshake.
        // **Once a minute, not once a retry.** The host records this radio's key durably (it
        // survives a daemon restart), so a retry of the handshake ladder is asking the same host
        // that already knows where to answer: re-sending the hint spent a record per rung of a
        // ladder that can climb for a minute.
        val hintDue = hintSentAt == 0L || clock() - hintSentAt >= HINT_REFRESH_MS
        val hint = if (hintDue) gateway.radioPublicKey?.let(RouteHint::record) else null
        if (hint != null) {
            // **A loop, not `repeat`.** `return@repeat` continues the iteration loop, so the hint
            // was sent three times on every connect - three records of airtime for a message that
            // had already arrived. Measured on the device 2026-11-08: three `route hint` lines in
            // the host's log for one connect.
            for (attempt in 0 until HINT_TRIES) {
                if (gateway.send(hint).outcome == MeshSendOutcome.SentDirect) {
                    hintSentAt = clock()
                    break
                }
                delay(HINT_RETRY_MS)
            }
        }
        handshake = h
        handshakeSince = clock()
        if (handshakeBeganAt == 0L) handshakeBeganAt = handshakeSince
        _view.value = MeshTermView.Handshaking
        // Message 1 is handshake traffic, not an ARQ record: it has no slot to give back, so it
        // travels with an unknown sequence.
        transmit(SealedRecord(SessionPort.UNKNOWN_SEQ, h.message1))
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
        // The verdict was about *this* link; the next one starts its own clock.
        handshakeBeganAt = 0L
        session?.close()
        session = null
        forgetMarkPage()
        terminalView = MeshTermView.LinkDown(state)
        // Every one of these is the transport's word, not the user's: a refused link, a handshake
        // that got no answer, or a session the host stopped acknowledging. Re-handshaking over the
        // same socket cannot fix any of them, so this connection is over and the caller builds the
        // next one.
        transportDead = true
    }

    /** The gateway link changed. Anything but bound is a visible [MeshTermView.LinkDown]. */
    suspend fun onGatewayState(state: MeshLinkState, detail: String? = null) = lock.withLock {
        when (state) {
            MeshLinkState.Bound -> {
                linkProblem = null
                linkDetail = null
            }
            // Not decisions: the link is still coming up or is idle.
            MeshLinkState.Idle, MeshLinkState.Connecting -> {}
            else -> {
                linkProblem = state
                // The link's own sentence, if it has one. A state that arrives without one keeps
                // whatever the link last said, which is fresher than nothing.
                linkDetail = detail ?: gateway.problem
                // **A gateway that closed is the end of this connection.** `run`'s loops only end
                // on [transportDead], and a peer close used to leave them ticking over a socket
                // that no longer existed: the screen sat on "closed" for as long as the user
                // watched, with the caller's rebuild never reached because `run` never returned
                // (measured on the device, 2026-11-01: no socket open, no reconnect for minutes).
                // Every other state here is the transport's word that the link is gone; a retry is
                // the caller's, and it is exactly what returning enables.
                transportDead = true
            }
        }
        publish()
    }

    /**
     * Re-handshake when the route has grown, because a record's size is a property of the path.
     *
     * **The sealed record size is fixed at the handshake**, and a raw send carries the path inside
     * the same frame, so walking out of direct range onto a repeater chain shrinks what the link can
     * carry while the session keeps sealing at the old size. The packet builder then refuses every
     * record (`TooLong`), each one is reported as a refusal, and the session retries the same
     * oversized record until the silence bound ends it — a rebuild that would have happened at once
     * had the size been re-read. A session that grew is left alone: a smaller record still fits.
     */
    private fun resizeIfRouteGrew(health: SessionHealth) {
        val now = gateway.uplinkRecordMax
        if (!needsResize(sessionLimit, now)) return
        if (health.pending > 0) return
        if (clock() - lastResizeAt < RESIZE_FLOOR_MS) return
        lastResizeAt = clock()
        sessionLimit = now
        rekey()
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
                failed(e.state); publish(); return@withLock
            }
            session = s
            sessionLimit = gateway.uplinkRecordMax
            handshakeTries = 0
            // A session is up: the next handshake (after a drop) starts its own clock.
            handshakeBeganAt = 0L
            // A session that worked earns the fast retry back for the next failure.
            recoverDelayMs = FIRST_RECOVERY_MS
            recoverAt = 0
            _view.value = MeshTermView.ProfileMissing
            // Nothing named: attach with an empty selector first. The host takes the one shell if
            // there is exactly one (the common case, and no extra record on a link that allows
            // ≈58 an hour), answers Closed(NOT_FOUND) for none and Closed(AMBIGUOUS) for
            // several, and only then does the driver ask which (below). It never guesses an id.
            pick = if (selector.isEmpty()) Pick.Probing else Pick.Attached
            namedAttach = selector.isNotEmpty()
            // **Remembered before the answer arrives.** A refusal that names the shell (`HELD`)
            // has to name it to the screen, and the screen needs it to offer `take it back`; the
            // empty-selector case learns its id from `Profile`, this one already has it.
            if (selector.isNotEmpty()) attempted = selector
            queueAttach(selector)
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
                lastCarriedAt = clock()
                resyncMadeProgress()
                recoverDelayMs = FIRST_RECOVERY_MS
                for (c in r.chunks) if (c.shell == SHELL) feedStream(c.bytes)
            }
        }
        pump()
        publish()
    }

    /**
     * How long this link has carried nothing while something is still owed, or null when it is not
     * owed anything.
     *
     * **Only meaningful with something outstanding.** An idle shell is silent by design: the diff
     * sends nothing, so "nothing heard for a minute" is the normal, correct state of a shell
     * nobody is typing into, and warning about it would train the user to ignore the line. With a
     * record outstanding, the same silence is the preamble to [silenceFailureMs] - and in live
     * mode there is no bar between "the link stopped carrying" and "the link dropped", so the
     * screen can say it early instead of showing a screen that looks live.
     */
    suspend fun quietForMs(): Long? = lock.withLock {
        val s = session ?: return@withLock null
        val pending = runCatching { s.health(clock()).pending }.getOrDefault(0)
        if (pending <= 0) return@withLock null
        val last = maxOf(lastInboundAt, lastCarriedAt)
        if (last == 0L) return@withLock null
        (clock() - last).coerceAtLeast(0)
    }

    /** Time passed: retransmit what the session wants, and notice a dead end. */
    suspend fun tick() = lock.withLock {
        val s = session
        if (s != null) {
            val h = s.health(clock())
            // A session that has spent its nonce budget starts a new one. Nothing is wrong with
            // the link, so this is not [failed]: the screen stays up and only the keys change.
            if (h.needsRekey && terminalView == null) rekey()
            if (terminalView == null) resizeIfRouteGrew(h)
            if (h.failed && terminalView == null) failed(MeshLinkState.SessionFailed)
            // **A record the host never answers is a dead link, and the screen must say so.** With
            // nothing outstanding an idle shell is silent by design, so silence alone proves
            // nothing; with something outstanding, twenty seconds of nothing back is longer than
            // the ARQ's own retry ladder (2 s, then doubling, five attempts), so a record that
            // reaches this point has been given up on rather than merely delayed. The terminal
            // would otherwise sit frozen on the last screen with the typing going nowhere.
            // Measured from the last record either direction carried, not from the last one that
            // arrived: silence is only evidence of a dead peer when something has been sent into
            // it and not answered.
            if (terminalView == null && h.pending > 0 &&
                clock() - maxOf(lastInboundAt, lastCarriedAt) > silenceFailureMs(gateway.routeHops)
            ) {
                failed(MeshLinkState.SessionFailed)
            }
            if (h.peerClosed && terminalView == null) terminalView = MeshTermView.HostClosed(null)
            // **A live shell whose screen never arrived re-asks here.** The mirror is empty, so the
            // page can only draw "waiting for shell screen" — for ever, because nothing retried a
            // resync that was lost while the session was busy. The 4 s floor in `requestResync`
            // keeps this from becoming a loop.
            if (terminalView == null && mirror.profile != null && !painted) {
                // Each unanswered ask waits longer than the last: the answer to a full screen takes
                // seconds over a radio, and asking inside that window is what made the screen slower
                // the longer it was waited for.
                if (resyncing && clock() - resyncAskedAt >= resyncRetryMs) {
                    resyncRetryMs = (resyncRetryMs * 2).coerceAtMost(60_000L)
                }
                requestResync()
            }
            // **A fit the session refused to queue is asked again here.** Otherwise a resize
            // dropped on a full queue left the host's pty at the old grid for the life of the
            // page, and only leaving the shell and re-entering it could cure it. A fit already
            // recorded returns immediately, so a tick with nothing to do costs nothing.
            if (terminalView == null) fitHost()
            pump()
        } else if (handshake != null) {
            val waited = clock() - handshakeSince
            if (waited > handshakeTimeoutMs) {
                handshake?.abandon(); handshake = null
                failed(MeshLinkState.SessionFailed)
            } else if (handshakeTries >= handshakeRetries) {
                // Three unanswered handshakes, each given twice as long as the last, is not a lost
                // record. The transport is the suspect now, and only a new connection can test that.
                handshake?.abandon(); handshake = null
                failed(MeshLinkState.SessionFailed)
            } else if (waited >= handshakeRetryMs shl handshakeTries) {
                handshakeTries++
                // **A handshake that was not answered is asked again, not waited out.** `msg1` is
                // one record; on a radio that drops one in ten it can simply go missing, and until
                // this nothing retried it - the driver sat on "securing the link" for the whole
                // five-minute timeout first (measured on the device: 77 seconds on the counter with
                // nothing on the air). A fresh handshake is a *different* packet (a new ephemeral
                // key), which matters because the mesh drops byte-identical repeats.
                handshake?.abandon()
                handshake = null
                terminalView = null
                connect()
            }
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
     * the link carries ≈1 record a minute).
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
     * the screen, as on the tailnet. Called again on rotation or a font change.
     *
     * **The bookkeeping is "what did we ask for", not "what did the host last say"** — the same
     * rule as the tailnet's [setGrid], and the difference matters on a link whose answers take a
     * second or more. The host's `Profile` is an answer to a request already in flight, so treating
     * it as "the host is already at the right size" throws away the request the user's rotation
     * just made. Measured on the device 2026-11-04: portrait → landscape → portrait inside one
     * round trip left the pty at the landscape width while the phone painted the portrait one,
     * because the second `setGrid` found the *old* profile equal to the new grid and sent nothing.
     * Asking against [requestedFit] cannot do that, and still cannot loop: the same grid for the
     * same shell is asked for once, however often the host's answer lands.
     */
    suspend fun setGrid(cols: Int, rows: Int) = lock.withLock {
        if (cols !in 1..255 || rows !in 1..255) return@withLock
        wantedGrid = cols to rows
        appLog("mesh grid ${cols}x$rows (host ${mirror.profile?.cols}x${mirror.profile?.rows}, v$hostVersion, asked $requestedFit)")
        if (session != null && terminalView == null) { fitHost(); pump(); publish() }
    }

    private var wantedGrid: Pair<Int, Int>? = null
    private var requestedFit: Triple<String, Int, Int>? = null

    /** When this radio's route hint last went out, so a retry does not repeat it. */
    private var hintSentAt: Long = 0L
    private var hostVersion = 0

    private fun fitHost() {
        val (cols, rows) = wantedGrid ?: return
        val profile = mirror.profile ?: return
        if (hostVersion < MeshTerm.RESIZE_VERSION) return
        val key = Triple(attachedId ?: mirror.shell, cols, rows)
        // This exact grid for this exact shell is already on its way.
        if (requestedFit == key) return
        // Nothing has been asked for on this connection and the host already fits: the first
        // `Profile` after an attach usually reports the size the pty kept from the previous
        // client. Asking anyway would spend two records on a link that allows ≈58 an hour.
        // Only the *first* time, though — after a request, the reported size is an answer to it
        // and must not be read as "already right".
        if (requestedFit == null && profile.cols == cols && profile.rows == rows) return
        requestedFit = key
        appLog("mesh fit: asking ${cols}x$rows (host ${profile.cols}x${profile.rows})")
        // The resize recreates the phone's engine at the new grid, so whatever is on it is gone:
        // the host resizes the pty and, without this, sends nothing at all, which measured on the
        // device as a blank terminal until the shell was left and re-entered. Asking for the whole
        // screen in the same breath costs one record and makes the resize visible immediately.
        //
        // **A request the session refused to queue is not a request.** The outcome used to be
        // dropped, so a fit that hit a full queue was silently lost and the screen kept the old
        // grid for the life of the page — the same failure the tailnet answers with
        // `retryGridAfterRefusal`. Clearing the record lets the next tick ask again.
        val queued = queue(TermMessage.Resize(cols, rows))
        queue(TermMessage.Resync)
        if (queued != QueueOutcome.Queued) {
            requestedFit = null
            appLog("mesh fit: resize not queued ($queued); will ask again")
        }
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
     * Open `id`: the one way a shell becomes the page on screen.
     *
     * **There used to be two, chosen by the screen guessing at the driver's state** (`choose` from
     * the first list, `selectShell` once a shell was open). The guess was wrong exactly where it
     * mattered: a launcher opened from a live shell releases that shell first (`openList`), so the
     * screen was no longer `Choose`, the tap took the `selectShell` path, which refuses when no
     * shell is attached — and `open` with three shells ticked did nothing at all, with no word to
     * the user. One entry point asks the driver what it has instead of the screen guessing.
     *
     * Only an id from the host's own list is accepted, so a stale picker cannot attach a guess.
     */
    suspend fun open(id: String): Boolean = lock.withLock {
        if (session == null || terminalView != null) return@withLock false
        if (id.isEmpty() || id == attachedId) return@withLock false
        val known = choices.any { it.id == id } || _shells.value.entries.any { it.id == id }
        if (!known) return@withLock false
        pick = Pick.Attached
        listHoldsNothing = false
        // A different shell's rows are never drawn under this one's name, and the surface is
        // dropped here rather than waiting for the first screen: an attach on a radio takes
        // seconds, and the old shell's rows on screen for those seconds are a lie.
        if (id != attachedId) {
            mirror = RowMirror()
            held.clear(); lastEpoch = null; painted = false; resyncing = false; unsupported = 0
        }
        attachedId = id
        attempted = id
        // Opening a shell reads its mark: the row must stop saying "done" about the shell that is
        // now in front of the operator.
        if (_marks.value.containsKey(id)) _marks.value = _marks.value - id
        publishShells()
        queueAttach(id)
        pump(); publish()
        true
    }

    /**
     * Choose one of the shells the host listed. Kept as the name the first list's tap uses; it is
     * [open] now, because the two paths disagreed about what `open` means.
     */
    suspend fun choose(id: String): Boolean = open(id)

    /**
     * Take a shell another device holds: the answer to [MeshTermView.HeldBy] (version 8).
     *
     * The host displaces the desk window, which is told and stops; the shell follows whoever asked.
     * Sent only from the screen's own `take it back`, so stealing a shell is always something the
     * user chose.
     */
    suspend fun takeover(id: String): Boolean = lock.withLock {
        if (session == null) return@withLock false
        if (id.isEmpty()) return@withLock false
        terminalView = null
        attempted = id
        listHoldsNothing = false
        attachedId = id
        mirror = RowMirror(); held.clear(); lastEpoch = null; painted = false; resyncing = false
        queue(TermMessage.Takeover(id)); pump(); publish()
        true
    }

    /**
     * Ask the host which shells are running **while attached**, to fill [shells]. Attaches nothing:
     * the answer is information for the header, and moving is [selectShell]. Costs one record up and
     * one or more down, so callers use it once on arriving and when the user asks, not on a timer.
     */
    /**
     * Show the launcher: let go of whatever was attached, then ask the host what it has.
     *
     * **One entry point, because the two old ones each refused in a state the user was in.**
     * `listShells` refuses unless a shell is attached; `refresh` refuses unless nothing is; and the
     * screen came to the list from both directions — from a live shell (`openList`) and from a blank
     * homepage after Android's back button. Whichever refused, the list stayed empty and the screen
     * stayed blank until the user pressed refresh, which happened to take the other path. Coming to
     * the launcher is one act, so it is one method.
     */
    suspend fun launcher(): Boolean = lock.withLock {
        if (session == null || terminalView != null) return@withLock false
        // Letting go is what gives a desk window its pty back, so it happens before the list is
        // asked for; `Detach` is queued, not awaited, and the host answers with the list either
        // way (`listHoldsNothing` keeps a one-shell answer from attaching straight back).
        //
        // **Attached is a state, not an id.** An empty-selector attach takes the host's one shell
        // without learning its id, and gating the `Detach` on the id meant that shell was never
        // released: the desk window stayed locked out and the pty stayed at the phone's width for
        // as long as the phone was on the list.
        if (attachedId != null || pick == Pick.Attached) queue(TermMessage.Detach)
        attachedId = null
        listHoldsNothing = true
        pick = Pick.Listing
        pages = ArrayList()
        listingForHeader = true
        queue(TermMessage.ListRequest)
        pump(); publish()
        true
    }

    suspend fun listShells() = lock.withLock {
        if (session == null || terminalView != null || pick != Pick.Attached) return@withLock
        // **A list already in flight is not asked for again.** The screen asks whenever it is
        // showing the list and has no entries, which is once per frame until the answer lands;
        // without this guard that is a record per frame on a link that allows ≈58 an hour.
        if (listingForHeader) return@withLock
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
    suspend fun selectShell(id: String): Boolean = open(id)

    /**
     * Ask the host to start a shell, and attach to the one it makes (version 7).
     *
     * **This is the verb the radio route was missing.** Without it a phone could only type into the
     * shells the desk had started, which made the LoRa list a viewer where the tailnet launcher is a
     * client. The host records the shell as the phone's, so it stays the phone's after a restart.
     * The answer is [TermMessage.Created], and the attach happens there rather than here: the id is
     * the host's to choose.
     */
    suspend fun createShell(cwd: String): Boolean = lock.withLock {
        if (session == null || terminalView != null) return@withLock false
        // A host that predates version 7 cannot read the message; asking would spend a record on
        // silence. The list still works, so the row that offers this is what says why.
        if (hostVersion in 1 until MeshTerm.SHELL_CONTROL_VERSION) return@withLock false
        createdId = null
        queue(TermMessage.Create(MeshTerm.VERSION, cwd)); pump(); publish()
        true
    }

    /**
     * Ask the host to end a shell, or put it down when a desk window holds it (version 7).
     *
     * The host's `close`, not a destroy: ending a desk shell from a phone is not this app's
     * decision (ADR-0010), and the host answers with the list that is now true.
     */
    suspend fun closeShell(id: String): Boolean = lock.withLock {
        if (session == null || terminalView != null) return@withLock false
        if (id.isEmpty()) return@withLock false
        // **The row goes now, because the user's act was the event.** Waiting for the host's
        // refreshed list left the ✕ looking inert: the shell did end on the host and the row
        // stayed, so the only way to see the result was to refresh. The row is dropped here and a
        // list is asked for in the same breath, so what replaces it is the host's answer rather
        // than this guess.
        val left = _shells.value.entries.filterNot { it.id == id }
        if (attachedId == id) attachedId = null
        _shells.value = MeshShells(left, attachedId)
        // **The list the screen draws is `choices`, not `_shells`.** When the view is `Choose` the
        // rows come from this list, so updating only the shell state left the row on screen and the
        // ✕ looked inert — twice fixed in the wrong place for exactly this reason.
        choices = left
        if (left.isEmpty()) pick = Pick.None
        queue(TermMessage.Close(id))
        queue(TermMessage.ListRequest)
        listingForHeader = true; pages = ArrayList()
        pump(); publish()
        true
    }

    /** Ask the host to open a shell in a desk window: the launcher's "open on desktop" (version 7). */
    suspend fun makeLive(id: String): Boolean = lock.withLock {
        if (session == null || terminalView != null) return@withLock false
        if (id.isEmpty()) return@withLock false
        queue(TermMessage.MakeLive(id)); pump(); publish()
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

    /** Whether the attached shell has never delivered a screen: the page has nothing to draw. */
    val screenUnpainted: Boolean
        get() = session != null && mirror.profile != null && !painted

    /**
     * Whether a named attach is on the wire and its answer has not arrived.
     *
     * **The screen must not ask for the list in this state.** A resume after a rebuild is a named
     * attach, and until the host's `Profile` arrives nothing is painted, so the screen looks "not
     * live" — which is exactly the condition its own list retry keys on. That retry's `launcher()`
     * queues a `Detach` in the same tick, the host drops the attach, and the `Profile` that then
     * arrives is still accepted: the page paints, looks live, and every keystroke afterwards is
     * dropped by a host that no longer holds the shell, silently (`Input` for an unattached shell
     * is not answered). A drop has to cost a pause, not the user's typing.
     */
    val attaching: Boolean
        get() = (pick == Pick.Attached || pick == Pick.Probing) && mirror.profile == null &&
            terminalView == null && session != null

    /**
     * Let the shell go while keeping the session.
     *
     * The tailnet launcher lets a shell go when its page closes, and the host then gives the shell
     * back to whoever held it. The mesh route kept the attach while its list was on screen, so a
     * desk window stayed locked out and its pty stayed at the phone's width after the user had
     * stopped looking at it. A `Detach` releases the shell and the host restores the grid the phone
     * displaced; the session itself stays, which is what keeps the next open free of a handshake.
     */
    suspend fun releaseShell(): Boolean = lock.withLock {
        if (session == null || terminalView != null) return@withLock false
        if (attachedId == null && pick != Pick.Attached) return@withLock false
        queue(TermMessage.Detach); pump()
        // The list is about to arrive; it must not take the shell straight back.
        listHoldsNothing = true
        attachedId = null
        // **The picture stays and the mirror goes.** The page keeps the last frame it painted (a
        // claim released because the app left the screen is not a reason to blank a shell the user
        // is coming back to), but the rows are no longer the host's idea of this phone's screen, so
        // the re-attach has to start from a full one.
        mirror = RowMirror(); held.clear(); lastEpoch = null; painted = false; resyncing = false
        publishShells(); publish()
        true
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
        val watchers = listOf(
            launch { gateway.received.collect { onRecord(it) } },
            launch { gateway.state.collect { onGatewayState(it) } },
            launch { gateway.status.collect { tick() } },
        )
        try {
            start()
            while (!transportDead) {
                val wake = nextWakeMs()
                val wait = if (wake == null) IDLE_TICK_MS else (wake - clock()).coerceIn(50L, IDLE_TICK_MS)
                delay(wait)
                tick()
            }
        } finally {
            // `coroutineScope` joins its children, so the collectors have to be stopped or this
            // never returns - and returning is the point: the transport is dead and the caller
            // builds the next connection. Detach first: this phone is letting the shell go, and
            // the host should be free to give it back to a desk window.
            detach()
            watchers.forEach { it.cancel() }
        }
    }

    // ── internals (lock held) ──────────────────────────────────────────────────────────

    /**
     * Put [m] in the session's send queue. Returns what the session said, so a caller that must
     * know whether its request left (the fit, which retries) can tell a dropped record from a
     * sent one. Callers that do not care simply ignore the result.
     */
    private fun queue(m: TermMessage): QueueOutcome = queueAll(listOf(m))

    /**
     * Queue several messages **as one record** when they fit, because a record is the unit of air.
     *
     * A record carries 143 bytes of payload and costs about 0.6 s on the public preset; `Attach` is
     * 11 bytes and its `Resize` 6, so sending them as two records spent 0.6 s of air on 17 bytes of
     * meaning. The host's decoder is a stream decoder (it has accepted several messages per record
     * since `STREAM_VERSION` 4), so the pair travels as one.
     *
     * Falls back to one record per message when a group does not fit, and returns the *first*
     * failure so a caller that cares about a refusal still sees it.
     */
    private fun queueAll(messages: List<TermMessage>): QueueOutcome {
        val s = session ?: return QueueOutcome.NotOpen
        val parts = messages.map { it.encode() }
        val total = parts.sumOf { it.size }
        if (parts.size > 1 && total <= MeshTerm.RECORD_PAYLOAD) {
            val joined = ByteArray(total)
            var at = 0
            for (p in parts) {
                p.copyInto(joined, at)
                at += p.size
            }
            return s.send(SHELL, joined)
        }
        var first: QueueOutcome = QueueOutcome.Queued
        for (p in parts) {
            val outcome = s.send(SHELL, p)
            if (outcome != QueueOutcome.Queued && first == QueueOutcome.Queued) first = outcome
        }
        return first
    }

    /**
     * How long a resync may stay unanswered before the request is made again, and it **doubles**.
     *
     * **A fixed four-second retry was a livelock over a radio.** One full screen is six or seven
     * records and takes as many seconds at 62.5 kHz, so the phone asked again *before* the answer it
     * was waiting for could arrive; every ask made the host render another whole screen, the host's
     * queue filled (measured: 9 sent, 14 pending), nothing ever completed, and the silence bound then
     * declared a link that was busy to be dead — which is the "waiting for shell screen, 65 s … then
     * link dropped" loop. A retry now waits a full window *after the last evidence of progress* and
     * backs off 4, 8, 16, 32, then 60 seconds.
     */
    private var resyncRetryMs = RESYNC_FIRST_RETRY_MS
    private var resyncAskedAt = Long.MIN_VALUE

    /**
     * Ask at once, because the host has named a gap.
     *
     * **Not gated on the session being idle.** A lost commit is exactly the case where the session
     * still owes records — that is what loss looks like — so the speculative guard would suppress the
     * one ask that is certainly needed. Rate-limited so a burst of frames cannot turn it into a
     * storm.
     */
    // Null, not zero: a clock that starts at zero (as a test's does) would otherwise treat its first
    // ask as one that happened a moment ago and suppress it.
    private var lastGapAskAt: Long? = null

    private fun askResyncNow() {
        val now = clock()
        if (lastGapAskAt?.let { now - it < GAP_ASK_FLOOR_MS } == true) return
        lastGapAskAt = now
        resyncing = true
        resyncAskedAt = now
        queue(TermMessage.Resync)
    }

    private fun requestResync() {
        val now = clock()
        // **A busy link is not a lost request.** With records still owed, the answer is already on
        // its way; asking again only adds work to the queue it has to get through.
        if (sessionPending() > 0) return
        if (resyncing && now - resyncAskedAt < resyncRetryMs) return
        resyncing = true
        resyncAskedAt = now
        queue(TermMessage.Resync)
    }

    /**
     * The host painted something, so the request is working: wait a full first window again.
     *
     * Called for every frame that arrives, because "progress" and "the answer" are different facts —
     * a screen can arrive in pieces and each piece is evidence the link is doing its job.
     */
    private fun resyncMadeProgress() {
        resyncAskedAt = clock()
        resyncRetryMs = RESYNC_FIRST_RETRY_MS
    }

    private fun sessionPending(): Int = runCatching { session?.health(clock())?.pending ?: 0 }.getOrDefault(0)

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
                appLog("mesh profile ${m.cols}x${m.rows} v${m.version} shell ${m.shell}")
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
            is TermMessage.Rows, is TermMessage.Commit ->
                if (mirror.profile != null) {
                    handleScreen(m)
                } else {
                    // **Rows for a screen we do not know the size of.** The host sends a `Profile`
                    // with the first frame after an attach, so a commit here means the host thinks
                    // this phone is attached while the phone has no grid — the frame was missed, or
                    // the mirror was reset by a re-show the host never heard about. The rows are
                    // discarded (they name rows of an unknown grid) and the *retry is shortened*
                    // rather than fired here: the tick asks, and asking is silent while a switch's
                    // own attach is still in flight. Without this the phone sat on a blank screen
                    // for up to the backoff's sixty seconds with a cursor placed from a grid it did
                    // not have.
                    if (m is TermMessage.Commit) {
                        resyncRetryMs = RESYNC_FIRST_RETRY_MS
                        resyncAskedAt = 0L
                    }
                }
            is TermMessage.Closed -> when {
                // **"Several shells" is the answer to an unnamed attach, not an ending.** It means
                // "name one", so the screen goes to the list it should have been on. It is keyed on
                // whether the attach named a shell rather than on `Pick.Probing`, because the pick
                // moves on: the launcher's own list request sets it to `Listing` one tick later, and
                // the refusal was then read as a closed page — "the host runs several shells" over a
                // live host, with the refresh square as the only way forward.
                m.reason == MeshTerm.Closed.AMBIGUOUS && !namedAttach -> {
                    listHoldsNothing = true
                    backToList()
                }
                pick == Pick.Probing && m.reason == MeshTerm.Closed.NOT_FOUND -> pick = Pick.None
                // **A shell that ended is not a link that ended.** The session is alive and this host
                // still has other shells, so the answer is the list the user picked from, exactly as
                // the tailnet route returns to its launcher when a shell exits. Ending the session
                // here would spend a whole handshake (minutes on this link) to reach a screen the
                // host can supply in one record. Everything the ended shell left is dropped first.
                // **Somebody else is holding it.** The tailnet route asks in exactly this
                // situation; the radio route used to take the shell, which resized the desk
                // window's pty and stopped its typing with no way back.
                // **Held is a page state, not a home-screen state.** A launch attach uses an empty
                // selector and the host picks the shell, so a refusal there says nothing this
                // screen can act on; the list already shows which shells are held ("on the
                // desktop"), and that is where the user chooses. Only an open the user asked for
                // by name becomes `HeldBy`, and only a terminal page draws it.
                m.reason == MeshTerm.Closed.HELD -> {
                    val named = (attempted ?: "").ifEmpty { null }
                    if (named == null) {
                        listHoldsNothing = true
                        backToList()
                    } else {
                        terminalView = MeshTermView.HeldBy(named)
                        pick = Pick.Attached
                    }
                }
                m.reason == MeshTerm.Closed.EXITED -> backToList()
                else -> terminalView = MeshTermView.HostClosed(m.reason)
            }
            // **The table is paged, and the phone assembles it.** A page that does not say `last`
            // is appended to what has arrived; the page that says `last` replaces the table, which
            // is also how the host clears a mark it no longer holds. Replacing per page would show
            // only the last page of a table that needs several.
            is TermMessage.Marks -> {
                val page = m.entries.mapNotNull { e -> ShellMark.of(e.kind)?.let { e.id to it } }.toMap()
                markPage = if (m.first || markPage == null) page else markPage!! + page
                if (m.last) {
                    _marks.value = markPage ?: emptyMap()
                    markPage = null
                }
            }
            is TermMessage.Sessions -> onSessions(m)
            // **The host has started a shell for us.** Attach to exactly that id: the list is not
            // consulted, so another client's shell cannot be picked up by accident between the
            // request and the answer.
            is TermMessage.Created -> {
                createdId = m.id
                // No `pick` condition: the answer to a create is this link's to act on whatever the
                // list was showing. Backing out of a failed list leaves `pick` at `None`, and that
                // silently swallowed the new shell's id — the create worked and nothing opened.
                if (terminalView == null) {
                    mirror = RowMirror()
                    held.clear(); lastEpoch = null; painted = false; resyncing = false; unsupported = 0
                    attachedId = m.id
                    publishShells()
                    queueAttach(m.id)
                    // **And ask for the list, because the row set on the phone is now wrong.**
                    // The host answers `Created` with the id and nothing else, so the shell this
                    // phone just started was absent from its own list until a manual refresh: the
                    // row, its `close` action and its "opened on this phone" mark were all missing,
                    // which reads as "create did nothing" and, later, as a ✕ that cannot be found
                    // to press. One record, once, after the shell exists.
                    listingForHeader = true
                    pages = ArrayList()
                    queue(TermMessage.ListRequest)
                }
            }
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
        forgetMarkPage()
        pick = Pick.Listing
        pages = ArrayList()
        choices = emptyList()
        publishShells()
        queue(TermMessage.ListRequest)
    }

    /**
     * A list answered, or a list **pushed**.
     *
     * The host sends a fresh `Sessions` when a shell it just ended or created changes the set, and
     * this used to throw every such page away unless the phone had asked for one: pressing the
     * close button on a shell this phone opened did nothing at all until the user refreshed, at
     * which point the row vanished. The event had arrived and the state ignored it. A pushed list
     * is as authoritative as an asked-for one, so it is applied the same way.
     */
    private fun onSessions(m: TermMessage.Sessions) {
        if (pick != Pick.Listing && !listingForHeader) {
            // Pushed: accumulate its pages and replace the row set, without touching what the
            // screen is showing (this runs while a shell is open, or on the list).
            val buf = pushedPages ?: ArrayList<SessionEntry>().also { pushedPages = it }
            buf.addAll(m.entries)
            if (!m.last) return
            pushedPages = null
            val all = buf.distinctBy { it.id }.take(MeshTerm.MAX_SESSIONS)
            // The shell this link was on may be the one that just ended.
            if (attachedId != null && all.none { it.id == attachedId }) attachedId = null
            _shells.value = MeshShells(all, attachedId)
            // Same rule as `closeShell`: `Choose` draws its own list.
            if (pick == Pick.Choosing) {
                choices = all
                if (all.isEmpty()) pick = Pick.None
            }
            publish()
            return
        }
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
            // Showing the one shell as a row rather than opening it: see [listHoldsNothing].
            1 if listHoldsNothing -> { pick = Pick.Choosing; choices = all }
            1 -> {
                pick = Pick.Attached
                attachedId = all[0].id
                attempted = all[0].id
                queueAttach(all[0].id)
            }
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
        // **A commit with no rows is legitimate**: a cursor or a flag changed and nothing on the
        // screen did, so there is nothing to carry. A commit *out of order* is the case that is not
        // legitimate, and it is asked for at once rather than on the tick's speculative timer.
        if (!full && !inOrder) {
            // A diff against a base we do not hold: discard it and ask for the whole screen.
            askResyncNow()
            return
        }
        for (r in rows) mirror.apply(r)
        mirror.apply(m)
        lastEpoch = m.epoch
        // **Painted means there is something to paint.** A commit over an empty mirror left
        // `painted` true, so the page drew an empty grid with a cursor in it and never asked again.
        painted = mirror.lines.isNotEmpty()
        if (m.flags and MeshTerm.CommitFlags.IDLE != 0) onIdle?.invoke()
        if (full) resyncing = false
        appLog("mesh commit epoch ${m.epoch} full=$full rows=${mirror.lines.size} cols=${mirror.profile?.cols}")
    }

    /**
     * Queue an `Attach` **with the phone's grid**, and remember that the fit is already asked for.
     *
     * **The host's pty is whatever a desk window left it at** — measured 158x82 against a phone's
     * 46x17 — and a full frame of that is about eighty records, which at the public tuple is a
     * minute and a half of blank screen *before* the phone's `Resize` is even read. `fitHost()`
     * cannot send it earlier because it needs the host's `Profile` to compare against, so the grid
     * goes with the attach: one extra record against eighty saved, and the host's `ATTACH_GRACE`
     * then does what it was added for (the resize arrives inside it, so the first frame is drawn at
     * the phone's size rather than the desk's).
     */
    private fun queueAttach(selector: String) {
        val grid = wantedGrid
        if (grid == null) {
            queue(TermMessage.Attach(MeshTerm.VERSION, selector))
            return
        }
        // **The grid travels with the attach, in the same record.** Two records for 17 bytes of
        // meaning cost a whole extra 0.6 s, and the host's grace then waits for a resize that is
        // already in its hands. A host that predates `Resize` ignores the message and answers
        // `Profile` with the flag clear; the version is not known on the first attach, so it is sent
        // and judged by the answer rather than by a guess.
        val (cols, rows) = grid
        val outcome = queueAll(
            listOf(TermMessage.Attach(MeshTerm.VERSION, selector), TermMessage.Resize(cols, rows)),
        )
        // **Remembered only when it queued.** `fitHost` starts with "already asked" and would never
        // ask again, so a refusal recorded here left the pane at the desk's grid for the life of the
        // connection - the very case `fitHost` clears the field for.
        requestedFit = if (outcome == QueueOutcome.Queued) Triple(selector, cols, rows) else null
    }

    private suspend fun pump() {
        // See `left`: the session is only muted by a gateway that says so.
        val s = session ?: return
        // A gateway that has not spoken yet has not said "none": treating silence as zero mutes
        // the session, including the acks that keep it alive.
        val left = gateway.status.value?.opsLeftThisHour ?: UNKNOWN_BUDGET
        val records = try {
            s.pollWithSeq(clock(), left, L2_WINDOW)
        } catch (e: MeshSessionException) {
            // **A dead session ends the connection, not just the view.** This used to set the
            // view and return, which left `run`'s loop ticking over a session that could never
            // send again: the screen showed the failure, the caller's rebuild was never reached
            // because `run` never returned, and the only cure was the refresh button. Measured on
            // the device 2026-11-04: "the host did not answer" until refresh, then working in two
            // seconds. `failed` is the one place that says "this connection is over".
            failed(e.state); return
        }
        for (r in records) {
            if (!transmit(r)) break
            lastCarriedAt = clock()
        }
    }

    /** Hand one record to the gateway. False when the link is gone and the rest should wait. */
    private suspend fun transmit(record: SealedRecord): Boolean {
        val result = try {
            // **Bounded, because the lock is held across it.** Every entry point of this driver (a
            // received record, a keystroke, the tick that notices a dead link) takes the same mutex,
            // so a gateway that never answered froze the screen with nothing able to say so: the
            // silence rule lives in `tick`, which could not run. A record with no answer in time is
            // treated like a busy radio — the session's own retransmission covers it — and the rest
            // wait for the next pump.
            withTimeout(sendTimeoutMs) { gateway.send(record.bytes) }
        } catch (e: RadioLinkException) {
            // Same rule as `pump`: the radio refusing to carry a record is the transport's word,
            // and a transport that has stopped carrying records cannot be revived from inside it.
            failed(e.state); return false
        } catch (e: TimeoutCancellationException) {
            return false
        }
        lastSendOutcome = result.outcome
        when (result.outcome) {
            // The host is not a contact, or the record is over what the radio takes: retrying
            // changes nothing, so it is a visible failure rather than a silent loop.
            // **A refusal is the transport's word, so it ends the connection.** It used to set
            // the view and keep looping, which is the same dead end as `pump` above: the screen
            // said "the radio refused the record" and nothing ever tried again. A refusal is
            // often transient (a busy transmit table, a path that has not resolved yet), and the
            // caller's rebuild is exactly the retry that cures it.
            MeshSendOutcome.ErrNotFound -> failed(
                if (gateway.state.value == MeshLinkState.NoPath) MeshLinkState.NoPath
                else MeshLinkState.Refused,
            )
            // **A record the radio would not take for its size is not "move closer".** `NoPath`
            // tells the user the path is unusable; this is a record that did not fit, which the
            // carrier's own limit should make impossible — so it is reported as a refusal, which
            // retries, rather than as a link that needs a shorter route.
            MeshSendOutcome.ErrTooLong -> failed(MeshLinkState.Refused)
            // Busy, a local timeout or an unknown answer: the record was not acknowledged by the
            // host, so the session's own bounded retransmission covers it.
            else -> {}
        }
        // **A refusal is not a transmission.** Every outcome below says the packet never reached
        // the air: the transmit table was full, the path was unusable, the record was over what the
        // radio takes, the budget was spent, or the radio never answered. Counting one as an
        // attempt is how five refusals used to end a session and force a handshake that costs more
        // records than the burst did. Giving it back turns a burst the radio could not take into a
        // slower burst. An unknown sequence number is left alone: refusing the wrong slot would
        // give a free attempt to a record that really did go out.
        when (result.outcome) {
            MeshSendOutcome.ErrBusy,
            MeshSendOutcome.ErrNotFound,
            MeshSendOutcome.ErrTooLong,
            MeshSendOutcome.ErrBudget,
            MeshSendOutcome.LocalTimeout,
            -> if (record.seq != SessionPort.UNKNOWN_SEQ) {
                runCatching { session?.refuseSeq(clock(), record.seq) }
            }
            else -> {}
        }
        return terminalView == null
    }

    private fun publish() {
        val tv = terminalView
        if (tv != null) { _view.value = tv; return }
        val lp = linkProblem
        if (lp != null) { _view.value = MeshTermView.LinkDown(lp, linkDetail); return }
        _view.value = when {
            session == null && handshake == null -> MeshTermView.Idle
            handshake != null ->
                if (clock() - handshakeBeganAt > handshakeVerdictMs) MeshTermView.NoAnswer
                else MeshTermView.Handshaking
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
                    // Unknown is not zero. The gateway route always reports a
                    // budget; a direct radio does not, and treating null as
                    // "stalled" painted "waiting for send budget" over a link
                    // that was sending normally.
                    stalled = pending > 0 && left != null && left == 0,
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

        /**
         * What to assume before the gateway has reported a budget.
         *
         * Unlimited is the safe assumption: the cost of being wrong is a little extra airtime on
         * a link whose policy nobody has stated, and the cost of assuming zero is a session that
         * cannot even acknowledge what it receives, which reads as "host stopped answering".
         */
        const val UNKNOWN_BUDGET = 255
        const val IDLE_TICK_MS = 30_000L

        /**
         * How often a refused route hint is retried before the handshake goes anyway.
         *
         * The hint is what teaches the host where to send its answer, so a handshake that leaves
         * without it spends a rung of the ladder (8 s, doubling) on a session the host cannot
         * complete. Three tries at a quarter second covers a transient refusal without holding the
         * handshake behind a radio that will not take it at all.
         */
        const val HINT_TRIES = 3
        const val HINT_RETRY_MS = 250L

        /**
         * How long a hint is trusted before another one is worth a record.
         *
         * The host keeps this radio's key durably, so a retry of the handshake ladder is asking a
         * host that already knows where to answer. A minute is longer than any rung of the ladder
         * and short enough that a host which lost its state is re-taught quickly.
         */
        const val HINT_REFRESH_MS = 60_000L
    }
}

/**
 * Whether the session must be re-sealed because the route now carries less than it was built for.
 *
 * Pure and separate so the rule is a value a test holds: a link that answers a *larger* record than
 * the session uses is left alone (the smaller record still fits), and a link that answers nothing at
 * all is not a reason to handshake again.
 */
/**
 * The least gap between two resize re-handshakes: a route that flaps between a direct link and one
 * repeater must not cost a handshake on every tick.
 */
private const val RESIZE_FLOOR_MS = 30_000L

/** The first wait before a resync is asked again; each ask doubles it, to a minute. */
private const val RESYNC_FIRST_RETRY_MS = 4_000L

/** The least gap between two "the host named a gap" asks, so a burst cannot become a storm. */
private const val GAP_ASK_FLOOR_MS = 1_500L

internal fun needsResize(sessionLimit: Int, linkLimit: Int): Boolean =
    linkLimit > 0 && sessionLimit > 0 && linkLimit < sessionLimit

/**
 * How long an unanswered record may wait before this route calls the link dead, for a route of
 * `hops` repeaters.
 *
 * **The bound is a function of the distance, and it is a backstop rather than a stopwatch.** The ARQ
 * gives up on its own after its attempts; this is the user-visible verdict for the case the ARQ
 * cannot see — a radio off the air, where nothing ever comes back at all.
 *
 * It began at twenty seconds, which is shorter than a *legitimate* burst: one full screen is six or
 * seven records at 0.6 s each plus their acknowledgements, so a shell that was painting perfectly
 * well was declared dead mid-paint, rebuilt (a handshake is a dozen records) and started again —
 * measured 2026-11-08 as "waiting for shell screen, 65 s … then link dropped". Sixty seconds covers
 * a full repaint at this tuple with room to spare, each hop buys 1.5 s for the round trip (this
 * mesh's floods were measured at 18-29 hops), and the ceiling keeps a genuinely dead link from
 * hanging for more than two minutes.
 */
internal fun silenceFailureMs(hops: Int?): Long =
    (60_000L + (hops ?: 0).coerceIn(0, 40) * 1_500L).coerceAtMost(120_000L)

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
