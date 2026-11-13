package io.ldmux.mobile.mesh

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The radio engine as the shared screen and the settings screens see it.
 *
 * **This is a seam, not an implementation.** The engine itself is Rust behind `ldmux_client_*`
 * (`crates/session-ffi`, `docs/RADIO-PLAN.md` §8.1); the Android class that implements this
 * interface owns the GATT pipe, one loop and one timer, and nothing else. It exists because the
 * terminal screen and the settings screens live in `commonMain` and the engine's Kotlin face lives
 * in `androidMain` — the same split, and the same reason, as [RadioLink].
 *
 * **Nothing here speaks a protocol.** There is no frame, no record, no handshake and no retry in
 * this file or in anything that implements it: those are the library's. What crosses this seam is
 * pages, commands, a screen mirror and the radio's own readings.
 *
 * **One loop, one timer.** Every mutating call is `suspend` and is serialised onto the engine's
 * single thread, because the library is not thread safe and a radio is one link. The loop wakes on
 * [nextDeadline], on a notification, and on a queued call; there is no ticker.
 */
interface RadioEngine {
    /** The configured link is up: the radio answered and the GATT connection is established. */
    val linkUp: StateFlow<Boolean>

    /**
     * Which of the four things is true about the link, as a fact rather than a sentence.
     *
     * **A state the user must cure is a fact the link already has**, and it is reported here so
     * nothing above has to infer it from [problem]'s wording. `RadioMissing` and `PermissionDenied`
     * are the two the person can act on; `LinkDown` is everything else that stopped the link, and
     * retrying is the whole response to it.
     */
    val linkState: StateFlow<EngineLink>

    /**
     * Why the last attempt failed, in words, or null while it is fine.
     *
     * A state says what kind of thing is wrong; this says which thing — "Bluetooth is off" and
     * "not a Bluetooth address" are both [MeshLinkState.RadioUnavailable] and are cured by
     * different actions.
     */
    val problem: StateFlow<String?>

    /** The engine is open and usable. False after [close]. */
    val isOpen: Boolean

    /**
     * The radio's own reading, updated whenever it answers.
     *
     * A flow rather than a call because the settings screens draw it and the loop is the only
     * thing allowed to touch the engine.
     */
    val radio: StateFlow<RadioStatus>

    /** What the engine believes about the path to the host. See [Route.hopsOrNull]. */
    val route: StateFlow<Route>

    /** Everything the app has to draw or say, in the order the engine produced it. */
    val events: Flow<ClientEvent>

    /** Open the GATT link and start the engine's loop. */
    suspend fun start()

    /** Begin (or resume) the bootstrap to the host. */
    suspend fun connect(nowMs: Long, wallMs: Long): Int

    /** A session with the host exists. */
    suspend fun established(): Boolean

    /**
     * Tune the radio, and optionally its path width and power.
     *
     * Zero width and [UNSET_POWER] mean "leave what the radio has", so a screen that changes only
     * the tuple does not silently rewrite an operator's path width or power.
     */
    suspend fun setPreset(
        freqKhz: Int,
        bwHz: Int,
        sf: Int,
        cr: Int,
        pathWidth: Int = 0,
        powerDbm: Int = UNSET_POWER,
    ): Int

    /** Set the region's 16-byte flood key, or an empty array for unscoped floods. */
    suspend fun setScope(key: ByteArray): Int

    /** How many records the radio says may still be sent this hour. */
    suspend fun setRecordsLeft(left: Int)

    /** Open one page at the phone's grid. */
    suspend fun open(shell: String, cols: Int, rows: Int, nowMs: Long): Int

    /** Stop drawing a page without ending the shell. */
    suspend fun closePage(shell: String): Int

    /** End the shell (or let go of it when a desk window holds it). */
    suspend fun end(shell: String, nowMs: Long): Int

    /** Ask the host to make a shell; the id arrives as [ClientEvent.Created]. */
    suspend fun createShell(cwd: String, nowMs: Long): Int

    /** Take a shell another device holds. */
    suspend fun takeover(shell: String, nowMs: Long): Int

    /** Open the shell in a desk window on the host. */
    suspend fun makeLive(shell: String, nowMs: Long): Int

    /** Ask for a scrollback page: [rows] rows starting at line [from]. */
    suspend fun history(shell: String, from: Int, rows: Int, nowMs: Long): Int

    /** Ask which shells are running. */
    suspend fun list(nowMs: Long): Int

    /**
     * Send one command: the whole draft as a single message.
     *
     * [kind] is [ClientCode.ITEM_STREAM] for a composed draft — which is what a command with more
     * than one item uses, so `ls -la` and its `Enter` are one message — or one of
     * [ClientCode.ITEM_TEXT], [ClientCode.ITEM_PASTE], [ClientCode.ITEM_KEYS] for a single item. The
     * answer is at most one [ClientEvent.Settled] carrying what the host did with it.
     */
    suspend fun send(
        shell: String,
        text: ByteArray,
        kind: Int,
        cols: Int,
        rows: Int,
        nowMs: Long,
    ): Int

    /**
     * Ask for the screen again.
     *
     * **Never touches the draft.** Looking at a shell and running what is typed in it are
     * different intentions, and the refresh control must not be able to do the second.
     */
    suspend fun fetch(shell: String, cols: Int, rows: Int, nowMs: Long): Int

    /** The screen the engine holds for [shell], or null when that page is not open. */
    suspend fun mirror(shell: String): ClientMirror?

    /** One row of [shell]'s screen as UTF-8, or null past the last row. */
    suspend fun mirrorLine(shell: String, row: Int): ByteArray?

    /** The engine's counters, or null when it is closed. */
    suspend fun stats(): ClientStats?

    /**
     * When the caller should look again even if nothing arrives.
     *
     * Null when there is nothing pending. A timer built from this is the only timer the route has.
     */
    suspend fun nextDeadline(): Long?

    /** Feed one companion notification in. Called by the pipe, on the loop. */
    fun onNotification(frame: ByteArray)

    /** Close the link, the lease, and the loop. */
    fun close()

    companion object {
        /** `powerDbm` meaning "leave the radio's transmit power alone", as `ldmux_client.h` has it. */
        const val UNSET_POWER: Int = -128
    }
}

/**
 * What is true about the link, in the smallest set a screen needs to act on.
 *
 * Deliberately not a protocol vocabulary: there is no "handshaking" or "refused" here, because
 * those are things the session layer says in [RadioEngine.events] and [RadioEngine.problem]. This
 * is only "can the phone reach its own radio".
 */
enum class EngineLink {
    /** The radio is not reachable and the phone can fix that: nothing chosen, no adapter, Bluetooth off. */
    RadioMissing,

    /** Android refused the Bluetooth permission the link needs. */
    PermissionDenied,

    /** The link is not up, and retrying is the response. */
    LinkDown,

    /** The GATT link is established. */
    Connected,
}

/**
 * The platform's engine, or null when this build has no radio.
 *
 * Android returns its engine; a build without a radio platform returns null and the screen says so
 * rather than pretending.
 *
 * **`creds` is null for a phone that has not paired yet**, and that is a real state rather than an
 * error: the radio's own operating point can be read with no host at all (`SELF_INFO` is answered
 * before any session exists), which is how the settings page works on a fresh install. An engine
 * without a peer has no session to start, so [RadioEngine.connect] refuses — it can read and tune a
 * radio, and it cannot talk to a host that has not been named.
 */
expect fun radioEngine(creds: MeshCredentials?, address: String): RadioEngine?

/**
 * The engine on a build that has no native library in it.
 *
 * **Not a stub of a missing feature — the feature is reported missing.** `ldmux_client_*` is a
 * shared object that Gradle packages; a build without it (a desktop JVM test, an ABI that was not
 * shipped) cannot talk to a radio at all, and `radioEngine` says so by returning null. The screen
 * already draws that as `LinkDown(NeedsRadio)`, so what it needs is an engine that *reports* the
 * absence rather than one that throws.
 */
object UnavailableRadioEngine : RadioEngine {
    private val down = MutableStateFlow(EngineLink.RadioMissing)
    private val nothing = MutableStateFlow<RadioStatus>(RadioStatus.UNKNOWN)
    private val unknown = MutableStateFlow(Route.UNKNOWN)
    private val falseFlow = MutableStateFlow(false)

    override val linkUp: StateFlow<Boolean> = falseFlow.asStateFlow()
    override val linkState: StateFlow<EngineLink> = down.asStateFlow()
    override val problem: StateFlow<String?> =
        MutableStateFlow("this build has no radio library").asStateFlow()
    override val isOpen: Boolean get() = false
    override val radio: StateFlow<RadioStatus> = nothing.asStateFlow()
    override val route: StateFlow<Route> = unknown.asStateFlow()
    override val events: Flow<ClientEvent> = emptyFlow()

    override suspend fun start() {}
    override suspend fun connect(nowMs: Long, wallMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun established(): Boolean = false
    override suspend fun setPreset(
        freqKhz: Int,
        bwHz: Int,
        sf: Int,
        cr: Int,
        pathWidth: Int,
        powerDbm: Int,
    ): Int = ClientCode.ERR_NULL
    override suspend fun setScope(key: ByteArray): Int = ClientCode.ERR_NULL
    override suspend fun setRecordsLeft(left: Int) {}
    override suspend fun open(shell: String, cols: Int, rows: Int, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun closePage(shell: String): Int = ClientCode.ERR_NULL
    override suspend fun end(shell: String, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun createShell(cwd: String, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun takeover(shell: String, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun makeLive(shell: String, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun history(shell: String, from: Int, rows: Int, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun list(nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun send(
        shell: String,
        text: ByteArray,
        kind: Int,
        cols: Int,
        rows: Int,
        nowMs: Long,
    ): Int = ClientCode.ERR_NULL
    override suspend fun fetch(shell: String, cols: Int, rows: Int, nowMs: Long): Int = ClientCode.ERR_NULL
    override suspend fun mirror(shell: String): ClientMirror? = null
    override suspend fun mirrorLine(shell: String, row: Int): ByteArray? = null
    override suspend fun stats(): ClientStats? = null
    override suspend fun nextDeadline(): Long? = null
    override fun onNotification(frame: ByteArray) {}
    override fun close() {}
}

/**
 * A draft's items as the ABI carries them: `[kind][len u16 LE][bytes]` for text and pastes, the
 * `Keys` form for names.
 *
 * **The same bytes `mesh_term`'s `Send` carries**, decoded there by `decode_item_stream`, so an item
 * means one thing on both sides. It lives here because this file already owns the ABI's vocabulary
 * (`ClientCode`, the mirrors of `ldmux_client.h`), and a second encoder somewhere else is how a
 * command came to be several messages.
 */
object ItemStream {
    private const val TEXT = 0x01
    private const val PASTE = 0x02
    private const val KEYS = 0x03

    fun encode(items: List<DraftItem>): ByteArray {
        val out = ArrayList<Byte>(64)
        for (item in items) when (item) {
            is DraftItem.Text -> putRun(out, TEXT, item.bytes)
            is DraftItem.Paste -> putRun(out, PASTE, item.bytes)
            is DraftItem.Keys -> {
                if (item.names.isEmpty()) continue
                out.add(KEYS.toByte())
                out.add(item.names.size.toByte())
                for (name in item.names) {
                    val bytes = name.encodeToByteArray()
                    out.add(bytes.size.toByte())
                    for (b in bytes) out.add(b)
                }
            }
        }
        return ByteArray(out.size) { out[it] }
    }

    private fun putRun(out: ArrayList<Byte>, tag: Int, bytes: ByteArray) {
        if (bytes.isEmpty()) return
        out.add(tag.toByte())
        out.add((bytes.size and 0xFF).toByte())
        out.add(((bytes.size shr 8) and 0xFF).toByte())
        for (b in bytes) out.add(b)
    }
}

/** Status codes, exactly as `ldmux_client.h` defines them. */
object ClientCode {
    const val OK = 0
    const val ERR_NULL = -1
    const val ERR_PANIC = -2
    const val ERR_ARG = -3
    const val ERR_BUFFER = -4
    const val ERR_DECODE = -26
    const val ERR_TOO_LONG = -24
    const val ERR_QUEUE_FULL = -30
    const val ERR_NOT_OPEN = -31
    const val ERR_TOO_MANY_PAGES = -32
    const val ERR_ALREADY_OPEN = -33
    const val ERR_NO_PAGE = -34

    /** A shell id is at most this many bytes (`LDMUX_SHELL_MAX`). */
    const val SHELL_MAX = 40

    /** What one send item is: text typed at a prompt, or a paste that must not run as it arrives. */
    const val ITEM_TEXT = 0
    const val ITEM_PASTE = 1

    /**
     * Key names, single-space separated (`"C-c"`, `"Up S-Tab"`).
     *
     * The engine encodes them for the pane's live cursor-key and Kitty mode, which only the host can
     * read, so the phone sends names rather than escape sequences it would have to guess.
     */
    const val ITEM_KEYS = 2
    /** A whole command: an item stream, so `ls -la` and its `Enter` are one message. */
    const val ITEM_STREAM = 3

    /** What the sentence for a failed call should say. */
    fun word(code: Int): String = when (code) {
        OK -> "ok"
        ERR_NULL -> "the radio client is closed"
        ERR_PANIC -> "the radio client failed inside"
        ERR_ARG -> "the radio client refused its arguments"
        ERR_BUFFER -> "the buffer was too small"
        ERR_DECODE -> "the message could not be built"
        ERR_TOO_LONG -> "that is too long to send"
        ERR_QUEUE_FULL -> "the queue is full"
        ERR_NOT_OPEN -> "no session with the host"
        ERR_TOO_MANY_PAGES -> "close a shell first: this many pages is the limit"
        ERR_ALREADY_OPEN -> "that shell is already open"
        ERR_NO_PAGE -> "that shell is not open"
        else -> "the radio client failed ($code)"
    }
}

/**
 * One page's screen, as the library holds it.
 *
 * [painted] is false before the first screen arrives, which is a real state rather than an empty
 * screen: a page is open, the host has been asked, and nothing has come back yet.
 */
data class ClientMirror(
    val painted: Boolean,
    val cols: Int,
    val rows: Int,
    val cursorX: Int,
    val cursorY: Int,
    val alternate: Boolean,
    val dead: Boolean,
    /** False means this is a mid-state, not the end of the command. */
    val settled: Boolean,
    val hasApplied: Boolean,
    /** What the host did with the command, in `mesh_term`'s `applied` vocabulary. */
    val appliedStatus: Int,
    val snapshotId: Int,
    val firstLine: Int,
) {
    companion object {
        /** The value an open page reports before its first screen. */
        val UNPAINTED = ClientMirror(
            painted = false, cols = 0, rows = 0, cursorX = 0, cursorY = 0, alternate = false,
            dead = false, settled = false, hasApplied = false, appliedStatus = 0, snapshotId = 0,
            firstLine = 0,
        )

        internal fun of(v: IntArray): ClientMirror = ClientMirror(
            painted = v[0] != 0,
            cols = v[1],
            rows = v[2],
            cursorX = v[3],
            cursorY = v[4],
            alternate = v[5] != 0,
            dead = v[6] != 0,
            settled = v[7] != 0,
            hasApplied = v[8] != 0,
            appliedStatus = v[9],
            snapshotId = v[10],
            firstLine = v[11],
        )
    }
}

/**
 * One thing the app has to draw or say.
 *
 * The four message kinds carry the **wire bytes** of what the host sent, so they decode with
 * [TermDecoder] — the same decoder the tailnet route uses. A second encoding for one fact would be a
 * second thing to be wrong in.
 */
sealed interface ClientEvent {
    val shell: String

    /** The page is open and the host painted at this grid. */
    data class Attached(override val shell: String, val cols: Int, val rows: Int) : ClientEvent

    /**
     * A command's answer arrived.
     *
     * [settled] false means the pane was still changing when the host's budget ran out, so this is a
     * mid-state rather than the end of the command. [appliedStatus] is what the host did with it —
     * the answer to "did my command run", which the screen alone cannot say.
     */
    data class Settled(
        override val shell: String,
        val snapshotId: Int,
        val settled: Boolean,
        val hasApplied: Boolean,
        val appliedStatus: Int,
    ) : ClientEvent

    /** Another device holds that shell. */
    data class Held(override val shell: String) : ClientEvent

    /** The host let the shell go, or refused it; [reason] is `mesh_term`'s `closed` byte. */
    data class Closed(override val shell: String, val reason: Int, val message: TermMessage.Closed) : ClientEvent

    /** The host made the shell a [RadioClient.createShell] asked for; [id] is what to open. */
    data class Created(val id: String) : ClientEvent {
        /** A created shell is not a page yet, so it has no shell id of its own. */
        override val shell: String get() = id
    }

    data class Sessions(override val shell: String, val last: Boolean, val message: TermMessage.Sessions) : ClientEvent
    data class Marks(override val shell: String, val last: Boolean, val message: TermMessage.Marks) : ClientEvent
    data class History(override val shell: String, val last: Boolean, val message: TermMessage.HistoryPage) : ClientEvent
}

/** The library's own counters, for a log that has to say what happened. */
data class ClientStats(
    val recordsSent: Long,
    val recordsReceived: Long,
    val commandsSent: Long,
    val fetchesSent: Long,
    val snapshotsApplied: Long,
    val snapshotsRefused: Long,
    val sessionsEstablished: Long,
    val rungsUsed: Long,
    /** Bootstrap rungs sent while no path to the host was held. */
    val blindAsks: Long,
    /** True when the mesh answered a blind ask with a path. */
    val routeAnswered: Boolean,
    val radioRecords: Long,
    val radioAnswered: Long,
    val radioExpired: Long,
    /** Records this end admitted: the session layer took them. */
    val admitted: Long = 0,
    /** Dropped as this end's own transmission coming back. */
    val selfEcho: Long = 0,
    /** Dropped because they were still in transit to another node. */
    val inTransit: Long = 0,
    /** Answers the handshake refused. */
    val handshakeRefused: Long = 0,
    /** Downlinks that decrypted but did not decode as a message. */
    val unknownDownlink: Long = 0,
    /** Lists this end asked for. */
    val listsSent: Long = 0,
    /** Records whose kind this end does not know. */
    val unknownKind: Long = 0,
    /** Records that decrypted to nothing usable. */
    val badRecord: Long = 0,
    /** The live session's index, or 0 when there is none. */
    val sessionIndex: Long = 0,
    /** Parts handed to the radio, retransmissions included. */
    val partsSent: Long = 0,
    /** Standalone acknowledgements handed to the radio. */
    val acksSent: Long = 0,
    /** Commands waiting for the radio's one answer window. */
    val radioBacklog: Long = 0,
    /** Frames the radio has ready for the caller to write. */
    val radioReady: Long = 0,
    /** Records abandoned after the window accepted them. */
    val radioMalformed: Long = 0,
    /** Whether the radio's one answer window is held right now. */
    val radioOutstanding: Long = 0,
    /** How many times the caller asked the radio to make progress. */
    val radioPumps: Long = 0,
    /** Answers that finished a session. */
    val answersFinished: Long = 0,
    /** Answers that repeated the one this attempt finished on. */
    val answersRepeated: Long = 0,
    /** Answers ignored because they were for another initiation. */
    val answersIgnored: Long = 0,
    /** The last record refused by the record layer, as a stable code; 0 when none has been. */
    val lastRecordRefusal: Long = 0,
    /** The index the refused record carried, and the counter it carried. */
    val lastRefusalIndex: Long = 0,
    val lastRefusalCounter: Long = 0,
    /** The index this end holds. */
    val localIndex: Long = 0,
    /** Records that arrived twice, which the receive log makes routine. */
    val duplicateRecords: Long = 0,
    /** Records for another session index: the peer re-handshook and this end followed. */
    val wrongIndexRecords: Long = 0,
    /** Acknowledgements staged by the transaction layer. */
    val acksStaged: Long = 0,
    /** Transport records that arrived while this end held no session. */
    val recordsWithoutSession: Long = 0,
    /** The kind and length of the last record admitted. */
    val lastAdmittedKind: Long = 0,
    val lastAdmittedLen: Long = 0,
    /** Records the radio refused to transmit, which is not the same as records the air lost. */
    val recordsRefused: Long = 0,
) {
    companion object {
        internal fun of(v: LongArray): ClientStats = ClientStats(
            recordsSent = v[0],
            recordsReceived = v[1],
            commandsSent = v[2],
            fetchesSent = v[3],
            snapshotsApplied = v[4],
            snapshotsRefused = v[5],
            sessionsEstablished = v[6],
            rungsUsed = v[7],
            blindAsks = v[8],
            routeAnswered = v[9] != 0L,
            radioRecords = v[10],
            radioAnswered = v[11],
            radioExpired = v[12],
            admitted = v[13],
            selfEcho = v[14],
            inTransit = v[15],
            handshakeRefused = v[16],
            unknownDownlink = v[17],
            listsSent = v[18],
            unknownKind = v[19],
            badRecord = v[20],
            sessionIndex = v[21],
            partsSent = v[22],
            acksSent = v[23],
            radioBacklog = v[24],
            radioReady = v[25],
            radioMalformed = v[26],
            radioOutstanding = v[27],
            radioPumps = v[28],
            answersFinished = v[29],
            answersRepeated = v[30],
            answersIgnored = v[31],
            lastRecordRefusal = v[32],
            lastRefusalIndex = v[33],
            lastRefusalCounter = v[34],
            localIndex = v[35],
            duplicateRecords = v[36],
            wrongIndexRecords = v[37],
            acksStaged = v[38],
            recordsWithoutSession = v[39],
            lastAdmittedKind = v[40],
            lastAdmittedLen = v[41],
            recordsRefused = v[42],
        )
    }
}

/**
 * What the phone's radio last said about itself.
 *
 * Every reading is separately optional, because they arrive from different questions and a radio
 * that answered one may not answer the others. [rssiDbm] is the last packet **this end** heard,
 * which is fresher than the radio's own answer to `GET_STATS` — a busy mesh means that answer is
 * about whatever a neighbour said last.
 */
data class RadioStatus(
    /** The radio answered `APP_START`, which is the only evidence this end has that it is there. */
    val connected: Boolean,
    val rssiDbm: Int?,
    /** In quarters of a dB, which is the firmware's own scale. */
    val snrQuarterDb: Int?,
    val batteryMv: Int?,
    /** The radio's own transmit airtime since it booted. */
    val txAirSecs: Long,
    val rxAirSecs: Long,
    /** The caller's record budget, or null when nothing reported one. */
    val opsLeft: Int?,
    val preset: RadioPreset?,
    /** Bytes per hop the firmware reported, or null when it did not say. */
    val pathHashWidth: Int?,
    val firmwareVersionCode: Int,
    /**
     * The node's own name, from `SELF_INFO`, or null when the radio has not answered it.
     *
     * A name a person gave the radio is the only thing on the settings page that identifies
     * *which* box this is: an address is not something anyone recognises.
     */
    val name: String? = null,
    /** The radio's own maximum transmit power in dBm, which is the regulatory ceiling. */
    val maxTxPowerDbm: Int? = null,
    /** The firmware's own version string, from `DEVICE_INFO`, or null when it did not say. */
    val firmware: String? = null,
    /** The contact capacity the firmware reports, or null when it did not say. */
    val maxContacts: Int? = null,
) {
    companion object {
        /** Nothing has been asked, or nothing has answered. */
        val UNKNOWN = RadioStatus(
            connected = false, rssiDbm = null, snrQuarterDb = null, batteryMv = null,
            txAirSecs = 0, rxAirSecs = 0, opsLeft = null, preset = null,
            pathHashWidth = null, firmwareVersionCode = 0,
        )

        internal fun of(v: IntArray, text: ByteArray = ByteArray(0)): RadioStatus = RadioStatus(
            connected = v[0] != 0,
            rssiDbm = if (v[1] != 0) v[2] else null,
            snrQuarterDb = if (v[1] != 0) v[3] else null,
            batteryMv = if (v[4] != 0) v[5] else null,
            txAirSecs = v[6].toLong(),
            rxAirSecs = v[7].toLong(),
            opsLeft = if (v[8] == 255) null else v[8],
            preset = if (v[9] != 0) {
                RadioPreset(
                    frequencyMhz = v[10] / 1000.0,
                    bandwidthKhz = v[11] / 1000.0,
                    spreadingFactor = v[12],
                    codingRate = v[13],
                    txPowerDbm = v[14],
                    pathHashSize = if (v[15] == 0) 1 else v[15],
                )
            } else {
                null
            },
            pathHashWidth = if (v[15] == 0) null else v[15],
            firmwareVersionCode = v[16],
            name = if (v[17] != 0 && v[19] > 0) utf8(text, 0, v[19]) else null,
            maxTxPowerDbm = if (v[17] != 0) v[18] else null,
            firmware = if (v[20] != 0 && v[21] > 0) utf8(text, NAME_MAX, v[21]) else null,
            maxContacts = if (v[20] != 0 && v[22] != 0xFFFF) v[22] else null,
        )

        /** Bytes of a node's name in the `radio` text buffer. */
        private const val NAME_MAX = 32
    }
}

/**
 * What the engine believes about the path to the host.
 *
 * [hops] is why this exists: the scrollback wait is sized against the route, so a flat deadline on a
 * twenty-hop route loses a page that was still arriving.
 */
data class Route(
    /** A fresh path is held, so the next attempt goes direct. */
    val known: Boolean,
    /** An authenticated direct arrival proved it. A flood's reversed chain never does. */
    val proven: Boolean,
    val fresh: Boolean,
    /** The next first attempt would flood, which is also what "no route" looks like. */
    val flood: Boolean,
    val hops: Int,
    val pathWidth: Int,
    val path: ByteArray,
    val snrQuarterDb: Int,
) {
    /** How far away the host is, or null when no path is known. */
    val hopsOrNull: Int? get() = if (known) hops else null

    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is Route) return false
        return known == other.known && proven == other.proven && fresh == other.fresh &&
            flood == other.flood && hops == other.hops && pathWidth == other.pathWidth &&
            path.contentEquals(other.path) && snrQuarterDb == other.snrQuarterDb
    }

    override fun hashCode(): Int {
        var result = known.hashCode()
        result = 31 * result + hops
        result = 31 * result + pathWidth
        result = 31 * result + path.contentHashCode()
        return result
    }

    companion object {
        /** No path is held: every first attempt floods. */
        val UNKNOWN = Route(
            known = false, proven = false, fresh = false, flood = true, hops = 0, pathWidth = 1,
            path = ByteArray(0), snrQuarterDb = 0,
        )

        internal fun of(v: IntArray, path: ByteArray, pathLen: Int): Route = Route(
            known = v[0] != 0,
            proven = v[1] != 0,
            fresh = v[2] != 0,
            flood = v[3] != 0,
            hops = v[4],
            pathWidth = v[5],
            snrQuarterDb = v[6],
            path = path.copyOf(pathLen.coerceIn(0, path.size)),
        )
    }
}

/**
 * `len` bytes of [text] starting at [offset], decoded as UTF-8, or null when they are not valid.
 *
 * **Decoded with replacement, never thrown.** The library cuts a name at a character boundary, so
 * a well-formed one always decodes; this is the belt for a firmware that reports bytes that are not
 * UTF-8 at all, and a settings page must not crash because a radio's name is not valid text.
 */
private fun utf8(text: ByteArray, offset: Int, len: Int): String? {
    if (offset < 0 || len <= 0 || offset + len > text.size) return null
    return text.copyOfRange(offset, offset + len).decodeToString(throwOnInvalidSequence = false)
        .ifEmpty { null }
}
