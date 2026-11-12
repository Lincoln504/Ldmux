package io.ldmux.mobile.mesh

/**
 * The JNI declarations for `native/ldmux_client_jni.c`, and [RadioClient], their Kotlin face.
 *
 * This is the **whole** radio route: the handshake, the record layer, the transaction layer, the
 * carriage decision, the terminal protocol and the per-page screen mirror all live in Rust behind
 * `ldmux_client_*` (`crates/session-ffi`, `docs/RADIO-PLAN.md` §8.1). Nothing in this file encodes a
 * message, chooses a carriage or keeps a timer: it converts values and calls the library.
 *
 * **The handle is a one-element [LongArray], not a `Long`.** The library's contract is
 * `ldmux_client_free(LdmuxClient **)`: it nulls the caller's variable, which is what makes a second
 * free a no-op and a use after free a null rather than a dereference of freed memory. A `Long` is a
 * copy, so a caller holding one would have to invent its own liveness rule for a pointer it cannot
 * see. With the array, the variable the library nulls is the one Kotlin reads: [RadioClient.handle]
 * is 0 once freed, and every call after that returns [LDMUX_ERR_NULL].
 *
 * Every output is written into an array the caller allocated; no pointer into library memory ever
 * crosses. A struct comes back field by field into an `IntArray`/`LongArray`, so Kotlin never sees a
 * C layout and a field cannot be read at the wrong offset.
 */
object NativeRadioClient {
    /** True if the native library loaded. Never throws. */
    val available: Boolean = try {
        System.loadLibrary("ldmux_session")
        true
    } catch (t: Throwable) {
        false
    }

    // ---- life cycle -------------------------------------------------------------------------

    /** [out] receives the handle (0 on failure). */
    external fun create(
        peerStatic: ByteArray,
        psk: ByteArray,
        localPrivate: ByteArray,
        localPublic: ByteArray,
        pathWidth: Int,
        sf: Int,
        floodReachHops: Int,
        maxOpenPages: Int,
        airtimeMs: Float,
        scope: Int,
        out: LongArray,
    ): Int

    /** Frees the client and zeroes `slot[0]`. A second call is a no-op. */
    external fun free(slot: LongArray)

    external fun connect(handle: Long, nowMs: Long, wallMs: Long): Int
    external fun established(handle: Long): Int
    external fun setRecordsLeft(handle: Long, left: Int): Int

    // ---- the air ----------------------------------------------------------------------------

    /** One whole companion frame from the radio. 1 when it carried a record, 0 when not. */
    external fun onBleRx(handle: Long, frame: ByteArray, nowMs: Long): Int

    /**
     * At most one frame to write, and its length in `outLen[0]`.
     *
     * 1 when a frame was written, 0 when there is nothing to send, [LDMUX_ERR_BUFFER] when `out` was
     * too small — and then `outLen[0]` is the length it needed and the frame is kept, so nothing is
     * lost by a retry.
     */
    external fun poll(handle: Long, nowMs: Long, out: ByteArray, outLen: IntArray): Int

    /** 1 and `out[0]` = when to look again even if nothing arrives; 0 for no deadline. */
    external fun nextDeadline(handle: Long, out: LongArray): Int

    // ---- pages ------------------------------------------------------------------------------

    /** Attach to `shell` **at the phone's grid**, so the host fits the pty before it paints. */
    external fun open(handle: Long, shell: ByteArray, cols: Int, rows: Int, nowMs: Long): Int

    /** Ask the host to make a shell in `cwd`; the answer is an [EV_CREATED][NativeRadioClient.EV_CREATED] event. */
    external fun createShell(handle: Long, cwd: ByteArray, nowMs: Long): Int

    external fun takeover(handle: Long, shell: ByteArray, nowMs: Long): Int
    external fun makeLive(handle: Long, shell: ByteArray, nowMs: Long): Int

    /** Ask for a page of the shell's own scrollback, from absolute line [fromLine]. */
    external fun history(handle: Long, shell: ByteArray, fromLine: Int, rows: Int, nowMs: Long): Int

    /** End the shell (or put it down when a desk window holds it) and forget the page. */
    external fun end(handle: Long, shell: ByteArray, nowMs: Long): Int
    external fun close(handle: Long, shell: ByteArray): Int

    /** [opId] receives the operation id when it is the caller's own array. */
    external fun send(
        handle: Long,
        shell: ByteArray,
        kind: Int,
        items: ByteArray,
        cols: Int,
        rows: Int,
        nowMs: Long,
        opId: IntArray?,
    ): Int

    external fun fetch(handle: Long, shell: ByteArray, cols: Int, rows: Int, nowMs: Long): Int
    external fun list(handle: Long, nowMs: Long): Int

    // ---- reading back -----------------------------------------------------------------------

    /** 1 when the shell is open; [out] then holds [MIRROR_FIELDS] values in [ClientMirror] order. */
    external fun mirror(handle: Long, shell: ByteArray, out: IntArray): Int

    /** 1 when the row exists; `outLen[0]` is its length, set even when `out` was too small. */
    external fun mirrorLine(handle: Long, shell: ByteArray, row: Int, out: ByteArray, outLen: IntArray): Int

    /**
     * One event, or 0 when there is none.
     *
     * [out] holds [EVENT_FIELDS] values in [ClientEvent] order, [shellOut] the shell id (its length
     * in `shellLen[0]`), and [textOut] the wire bytes of a message kind (its length in `textLen[0]`),
     * which the caller decodes with the decoder it already has.
     */
    external fun pollEvent(
        handle: Long,
        out: IntArray,
        shellOut: ByteArray,
        shellLen: IntArray,
        textOut: ByteArray,
        textLen: IntArray,
    ): Int

    /** [out] receives [STATS_FIELDS] counters. */
    external fun stats(handle: Long, out: LongArray): Int

    // ---- layout the shim writes ---------------------------------------------------------------

    /** How many values [mirror] writes. */
    const val MIRROR_FIELDS = 12

    /** How many values [pollEvent] writes. */
    const val EVENT_FIELDS = 8

    /** How many counters [stats] writes. */
    const val STATS_FIELDS = 8
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
        )
    }
}

/**
 * One radio client: the phone's whole route, held by the caller.
 *
 * A caller drives it with one loop and one timer:
 *
 * ```
 * onFrame(client.poll(now))              // write to the radio
 * client.onBleRx(notification, now)      // feed the radio back
 * client.drainEvents { … }               // draw
 * client.nextDeadline()?.let { setTimer(it) }
 * ```
 *
 * Not thread safe, deliberately: the library says so, and a radio is one link.
 */
class RadioClient private constructor(private val slot: LongArray) {
    /** The handle, or 0 once [close] has run. */
    val handle: Long get() = slot[0]

    val isOpen: Boolean get() = slot[0] != 0L

    /** Free the client. A second call is a no-op; every later call returns [ClientCode.ERR_NULL]. */
    fun close() {
        if (slot[0] != 0L) NativeRadioClient.free(slot)
    }

    fun connect(nowMs: Long, wallMs: Long): Int = NativeRadioClient.connect(handle, nowMs, wallMs)

    fun established(): Boolean = NativeRadioClient.established(handle) == 1

    /** How many records the radio says are left this hour. Never a guess: no report, no call. */
    fun setRecordsLeft(left: Int) {
        NativeRadioClient.setRecordsLeft(handle, left)
    }

    // ---- the air ----------------------------------------------------------------------------

    /** Feed one whole companion frame. Returns 1 when it carried a session record. */
    fun onBleRx(frame: ByteArray, nowMs: Long): Int =
        NativeRadioClient.onBleRx(handle, frame, nowMs)

    /**
     * The next frame to write, or null when there is nothing.
     *
     * A return of [ClientCode.ERR_BUFFER] means [out] was smaller than the frame; the library kept
     * it, and calling again with the size it reports delivers the same frame. The buffer below is a
     * companion frame's ceiling, so that cannot happen in practice — it is here so a frame bigger
     * than the ceiling is a resize rather than a lost record.
     */
    fun poll(nowMs: Long, out: ByteArray = ByteArray(FRAME_MAX)): ByteArray? {
        val len = IntArray(1)
        var rc = NativeRadioClient.poll(handle, nowMs, out, len)
        if (rc == ClientCode.ERR_BUFFER) {
            val bigger = ByteArray(len[0].coerceAtLeast(out.size * 2))
            rc = NativeRadioClient.poll(handle, nowMs, bigger, len)
            return if (rc == 1) bigger.copyOf(len[0]) else null
        }
        return if (rc == 1) out.copyOf(len[0]) else null
    }

    /** When to look again even if no notification arrives, or null when there is no deadline. */
    fun nextDeadline(): Long? {
        val out = LongArray(1)
        return if (NativeRadioClient.nextDeadline(handle, out) == 1) out[0] else null
    }

    // ---- pages ------------------------------------------------------------------------------

    fun open(shell: String, cols: Int, rows: Int, nowMs: Long): Int =
        NativeRadioClient.open(handle, shell.encodeToByteArray(), cols, rows, nowMs)

    /** Ask the host to make a shell; the id arrives as [ClientEvent.Created]. */
    fun createShell(cwd: String, nowMs: Long): Int =
        NativeRadioClient.createShell(handle, cwd.encodeToByteArray(), nowMs)

    /** Take the shell from whoever holds it. */
    fun takeover(shell: String, nowMs: Long): Int =
        NativeRadioClient.takeover(handle, shell.encodeToByteArray(), nowMs)

    /** Open the shell in a desk window on the host: send to desktop. */
    fun makeLive(shell: String, nowMs: Long): Int =
        NativeRadioClient.makeLive(handle, shell.encodeToByteArray(), nowMs)

    /** Ask for the shell's scrollback, starting at absolute line [from]. */
    fun history(shell: String, from: Int, rows: Int, nowMs: Long): Int =
        NativeRadioClient.history(handle, shell.encodeToByteArray(), from, rows, nowMs)

    /** End the shell, or put it down when a desk window holds it, and forget the page. */
    fun end(shell: String, nowMs: Long): Int =
        NativeRadioClient.end(handle, shell.encodeToByteArray(), nowMs)

    fun closePage(shell: String): Int =
        NativeRadioClient.close(handle, shell.encodeToByteArray())

    /**
     * Send one command to [shell], as one message however much text it is.
     *
     * [kind] is [ClientCode.ITEM_TEXT] for text typed at a prompt or [ClientCode.ITEM_PASTE] for
     * anything that must arrive without running — a multi-line paste, a key's escape sequence. The
     * host decides the pane's own bracketed-paste mode; the phone never guesses it.
     */
    fun send(
        shell: String,
        items: ByteArray,
        kind: Int = ClientCode.ITEM_TEXT,
        cols: Int,
        rows: Int,
        nowMs: Long,
    ): Int {
        val op = IntArray(1)
        return NativeRadioClient.send(
            handle, shell.encodeToByteArray(), kind, items, cols, rows, nowMs, op,
        )
    }

    /** Ask for the screen again. **Spends no draft**: looking can never run what is typed. */
    fun fetch(shell: String, cols: Int, rows: Int, nowMs: Long): Int =
        NativeRadioClient.fetch(handle, shell.encodeToByteArray(), cols, rows, nowMs)

    fun list(nowMs: Long): Int = NativeRadioClient.list(handle, nowMs)

    // ---- reading back -----------------------------------------------------------------------

    /** The page's screen, or null when that shell is not open. */
    fun mirror(shell: String): ClientMirror? {
        val out = IntArray(NativeRadioClient.MIRROR_FIELDS)
        return if (NativeRadioClient.mirror(handle, shell.encodeToByteArray(), out) == 1) {
            ClientMirror.of(out)
        } else {
            null
        }
    }

    /** One row of a page's screen, or null when that row does not exist. */
    fun mirrorLine(shell: String, row: Int): ByteArray? {
        val out = ByteArray(ROW_MAX)
        val len = IntArray(1)
        var rc = NativeRadioClient.mirrorLine(handle, shell.encodeToByteArray(), row, out, len)
        if (rc == ClientCode.ERR_BUFFER) {
            val bigger = ByteArray(len[0].coerceAtLeast(out.size * 2))
            rc = NativeRadioClient.mirrorLine(handle, shell.encodeToByteArray(), row, bigger, len)
            return if (rc == 1) bigger.copyOf(len[0]) else null
        }
        return if (rc == 1) out.copyOf(len[0]) else null
    }

    /**
     * Every event the library has, in order.
     *
     * A message kind whose wire bytes were lost to a small buffer is asked for again rather than
     * dropped: the library keeps it, so a drain that stops early cannot lose an answer.
     */
    fun drainEvents(): List<ClientEvent> {
        val out = ArrayList<ClientEvent>()
        val fields = IntArray(NativeRadioClient.EVENT_FIELDS)
        val shellOut = ByteArray(ClientCode.SHELL_MAX)
        val shellLen = IntArray(1)
        val text = ByteArray(MESSAGE_MAX)
        val textLen = IntArray(1)
        while (true) {
            textLen[0] = 0
            val rc = NativeRadioClient.pollEvent(handle, fields, shellOut, shellLen, text, textLen)
            if (rc != 1) break
            val shell = shellOut.decodeToString(0, shellLen[0].coerceIn(0, shellOut.size))
            val body = text.copyOf(textLen[0].coerceIn(0, text.size))
            out += eventOf(fields, shell, body) ?: continue
        }
        return out
    }

    fun stats(): ClientStats? {
        val out = LongArray(NativeRadioClient.STATS_FIELDS)
        return if (NativeRadioClient.stats(handle, out) == ClientCode.OK) ClientStats.of(out) else null
    }

    companion object {
        /** A companion frame's ceiling: three header bytes and a 176-byte payload. */
        const val FRAME_MAX = 179

        /** One row of a screen; a wide terminal is far under this. */
        const val ROW_MAX = 1_024

        /** The largest terminal message the library hands back. */
        const val MESSAGE_MAX = 4_096

        /**
         * Create a client, or null when the library refused the configuration.
         *
         * The keys come from the pairing. [airtimeMs] is one record's airtime at the radio's tuple,
         * which every deadline is derived from, so it is a fact about the air rather than a choice.
         */
        fun create(
            peerStatic: ByteArray,
            psk: ByteArray,
            localPrivate: ByteArray,
            localPublic: ByteArray,
            pathWidth: Int,
            sf: Int,
            floodReachHops: Int,
            airtimeMs: Float,
            scope: Int = 0,
            maxOpenPages: Int = 0,
        ): RadioClient? {
            if (!NativeRadioClient.available) return null
            val slot = LongArray(1)
            val rc = NativeRadioClient.create(
                peerStatic, psk, localPrivate, localPublic, pathWidth, sf, floodReachHops,
                maxOpenPages, airtimeMs, scope, slot,
            )
            return if (rc == ClientCode.OK && slot[0] != 0L) RadioClient(slot) else null
        }

        /** Decode one packed event, or null for a kind this build does not know. */
        private fun eventOf(fields: IntArray, shell: String, body: ByteArray): ClientEvent? {
            val kind = fields[0]
            val value = fields[1]
            return when (kind) {
                EV_ATTACHED -> ClientEvent.Attached(shell, fields[6], fields[7])
                EV_SETTLED -> ClientEvent.Settled(
                    shell = shell,
                    snapshotId = fields[2],
                    settled = fields[5] != 0,
                    hasApplied = fields[3] != 0,
                    appliedStatus = fields[4],
                )
                EV_HELD -> ClientEvent.Held(shell)
                EV_CLOSED -> firstOf<TermMessage.Closed>(body)?.let {
                    ClientEvent.Closed(shell, value, it)
                }
                EV_SESSIONS -> firstOf<TermMessage.Sessions>(body)?.let {
                    ClientEvent.Sessions(shell, value == 1, it)
                }
                EV_MARKS -> firstOf<TermMessage.Marks>(body)?.let {
                    ClientEvent.Marks(shell, value == 1, it)
                }
                EV_HISTORY -> firstOf<TermMessage.HistoryPage>(body)?.let {
                    ClientEvent.History(shell, value == 1, it)
                }
                // Not a message: the id is the text, so nothing is decoded.
                EV_CREATED -> ClientEvent.Created(body.decodeToString())
                else -> null
            }
        }

        /** The first message of type [T] in one wire buffer, or null. */
        private inline fun <reified T : TermMessage> firstOf(body: ByteArray): T? {
            if (body.isEmpty()) return null
            return TermDecoder().push(body).firstOrNull { it is T } as T?
        }

        const val EV_ATTACHED = 1
        const val EV_SETTLED = 2
        const val EV_HELD = 3
        const val EV_CLOSED = 4
        const val EV_SESSIONS = 5
        const val EV_MARKS = 6
        const val EV_HISTORY = 7
        const val EV_CREATED = 8
    }
}
