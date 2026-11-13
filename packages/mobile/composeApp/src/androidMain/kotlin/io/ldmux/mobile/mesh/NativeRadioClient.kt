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
    /**
     * Fill [priv] and [pub] with a fresh device keypair, 32 bytes each.
     *
     * The one call the app makes outside a client handle: a pairing starts before there is a
     * session to put it in. It lives on this object so the app links one library and the suite has
     * one implementation.
     */
    external fun keypair(priv: ByteArray, pub: ByteArray): Int

    /** The pair, or null when the library refused. Convenience over [keypair] for the one caller. */
    fun keypair(): Pair<ByteArray, ByteArray>? {
        val priv = ByteArray(KEY_LEN)
        val pub = ByteArray(KEY_LEN)
        return if (keypair(priv, pub) == OK) priv to pub else null
    }

    /** A keypair coordinate, in bytes. */
    const val KEY_LEN = 32

    /** The ABI status for a call that worked. */
    const val OK = 0

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
    external fun nextDeadline(handle: Long, nowMs: Long, out: LongArray): Int

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

    // ---- the phone's own radio ---------------------------------------------------------------

    /**
     * [out] receives [RADIO_FIELDS] values in [RadioStatus] order, and [text] the node's name
     * followed by its firmware string, each at the offset and length the values name.
     */
    external fun radio(handle: Long, out: IntArray, text: ByteArray): Int

    /**
     * [out] receives [ROUTE_FIELDS] values in [Route] order, and [pathOut] the path bytes whose
     * length lands in `pathLen[0]`.
     */
    external fun route(handle: Long, nowMs: Long, out: IntArray, pathOut: ByteArray, pathLen: IntArray): Int

    /** The region's 16-byte flood key, or an empty array to send unscoped. */
    external fun setScope(handle: Long, key: ByteArray): Int

    /** `powerDbm` meaning "leave the radio's transmit power alone". */
    const val UNSET_POWER: Int = -128

    external fun setPreset(
        handle: Long,
        freqKhz: Int,
        bwHz: Int,
        sf: Int,
        cr: Int,
        pathWidth: Int,
        powerDbm: Int,
    ): Int

    // ---- layout the shim writes ---------------------------------------------------------------

    /** How many values [mirror] writes. */
    const val MIRROR_FIELDS = 12

    /** How many values [pollEvent] writes. */
    const val EVENT_FIELDS = 8

    /** How many counters [stats] writes. */
    const val STATS_FIELDS = 40

    /** How many values [radio] writes. */
    const val RADIO_FIELDS = 23

    /** Bytes of a node's name in the `radio` text buffer. */
    const val RADIO_NAME_MAX = 32

    /** Bytes of a firmware string in the `radio` text buffer. */
    const val RADIO_FIRMWARE_MAX = 24

    /** The whole `radio` text buffer: the name, then the firmware string. */
    const val RADIO_TEXT = RADIO_NAME_MAX + RADIO_FIRMWARE_MAX

    /** How many values [route] writes. */
    const val ROUTE_FIELDS = 8
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

    /**
     * Feed one companion **payload** from the radio.
     *
     * A characteristic carries the payload and no serial framing, so the bytes go in as they
     * arrived. The engine decides what they are: a receive-log push carrying a record, the answer to
     * a command it asked, or a push about somebody else's traffic. Returns 1 when it carried a
     * record.
     */
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
    fun nextDeadline(nowMs: Long): Long? {
        val out = LongArray(1)
        return if (NativeRadioClient.nextDeadline(handle, nowMs, out) == 1) out[0] else null
    }

    // ---- the phone's own radio ----------------------------------------------------------------

    /**
     * What the radio last said about itself.
     *
     * Read after every answer has been fed back: the engine updates it from `SELF_INFO`,
     * `DEVICE_INFO`, the radio's stats and the battery answer, and from the readings on every
     * receive-log push.
     */
    fun radio(): RadioStatus {
        val out = IntArray(NativeRadioClient.RADIO_FIELDS)
        val text = ByteArray(NativeRadioClient.RADIO_TEXT)
        val rc = NativeRadioClient.radio(handle, out, text)
        return if (rc == ClientCode.OK) RadioStatus.of(out, text) else RadioStatus.UNKNOWN
    }

    /**
     * What the engine believes about the path to the host.
     *
     * [hops][Route.hops] is the number the scrollback wait is sized against: a page that has not
     * arrived is not a page that is lost, and a flat deadline on a long route loses one still
     * travelling.
     */
    fun route(nowMs: Long): Route {
        val out = IntArray(NativeRadioClient.ROUTE_FIELDS)
        val path = ByteArray(64)
        val len = IntArray(1)
        val rc = NativeRadioClient.route(handle, nowMs, out, path, len)
        return if (rc == ClientCode.OK) Route.of(out, path, len[0]) else Route.UNKNOWN
    }

    /** The region's 16-byte flood key, or an empty array for unscoped floods. */
    fun setScope(key: ByteArray): Int = NativeRadioClient.setScope(handle, key)

    /**
     * Tune the radio.
     *
     * [pathWidth] 0 and [powerDbm] [NativeRadioClient.UNSET_POWER] mean "leave what the radio has", so a screen that
     * only changes the tuple does not silently rewrite an operator's path width or power. The
     * writes queue on the radio's one command window; the firmware's answer arrives through the
     * normal frame loop, never as a return value here.
     */
    fun setPreset(
        freqKhz: Int,
        bwHz: Int,
        sf: Int,
        cr: Int,
        pathWidth: Int = 0,
        powerDbm: Int = NativeRadioClient.UNSET_POWER,
    ): Int = NativeRadioClient.setPreset(handle, freqKhz, bwHz, sf, cr, pathWidth, powerDbm)

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
