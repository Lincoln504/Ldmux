package io.ldmux.mobile.mesh

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * The radio engine, on Android: the Rust client, the BLE pipe, and one loop.
 *
 * ```
 * notification ──► onNotification ──┐
 *                                   ├──► loop: apply what arrived, write what the engine owes,
 * a posted call (open, send, …) ────┘         drain what it says, sleep until its own deadline
 * ```
 *
 * **Why a loop rather than the caller's thread.** `ldmux_client_*` is not thread safe, and every
 * call takes the same single radio command window. So the client is touched from exactly one
 * thread — [LOOP], a dispatcher confined to one worker — and every caller posts to it and awaits
 * the library's own answer. A UI coroutine can therefore call [send] without any way to interleave
 * with a frame the loop is writing, which is the shape `RC-2` broke when two commands shared a
 * window.
 *
 * **Why one timer rather than a ticker.** The old bridge woke every 50 ms because the byte-stream
 * session needed a heartbeat. The transaction layer has deadlines of its own and reports the
 * earliest of them through [nextDeadline], so the loop sleeps until that instant, until a
 * notification arrives, or until a call is posted — whichever comes first. An idle radio costs
 * nothing, which is what "an idle screen is free" always claimed and never was.
 *
 * **What this class owns, and what it does not.** It owns the pipe (GATT and the radio lease), the
 * client handle, the loop, and the published readings. It owns **no protocol**: no frame, no record,
 * no retry, no route arithmetic, and no timing constant of its own — every deadline below is one the
 * library reported.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AndroidRadioEngine(
    private val address: String,
    creds: MeshCredentials?,
    airtimeMs: Float = AIRTIME_MS,
    transport: io.ldmux.mobile.ble.BleTransport? = null,
) : RadioEngine {

    private val _linkUp = MutableStateFlow(false)
    override val linkUp: StateFlow<Boolean> = _linkUp.asStateFlow()

    private val _problem = MutableStateFlow<String?>(null)
    override val problem: StateFlow<String?> = _problem.asStateFlow()

    private val _linkState = MutableStateFlow(EngineLink.LinkDown)
    override val linkState: StateFlow<EngineLink> = _linkState.asStateFlow()

    private val _radio = MutableStateFlow(RadioStatus.UNKNOWN)
    override val radio: StateFlow<RadioStatus> = _radio.asStateFlow()

    private val _route = MutableStateFlow(Route.UNKNOWN)
    override val route: StateFlow<Route> = _route.asStateFlow()

    /**
     * What the app has to draw or say.
     *
     * A shared flow with a buffer, because the loop must never block on a slow collector: an event
     * that cannot be delivered is a screen that is one paint behind, and the next event carries the
     * whole truth anyway (a `Snapshot` is the screen, not a difference from it).
     */
    private val _events = MutableSharedFlow<ClientEvent>(extraBufferCapacity = 64)
    override val events: Flow<ClientEvent> = _events.asSharedFlow()

    /**
     * The keys this engine runs with.
     *
     * **A radio with no host is still a radio.** The settings page reads and tunes the phone's own
     * radio before any pairing, so an engine built without credentials gets a throwaway local
     * keypair and zeroed peer fields: enough for `SELF_INFO`, `DEVICE_INFO` and a preset write, and
     * unable to start a session because there is no peer to start one with ([connect] refuses).
     * Generating a fresh pair invents no identity that could be mistaken for a paired one — the
     * engine has no host key to hand a session to.
     */
    private val hostPaired: Boolean = creds != null

    private val localKeys: Pair<ByteArray, ByteArray>? = creds?.let {
        it.keys.localPrivate to it.keys.localPublic
    } ?: runCatching { NativeRadioClient.keypair() }.getOrNull()

    private val client: RadioClient? = localKeys?.let { (localPrivate, localPublic) ->
        RadioClient.create(
            peerStatic = creds?.keys?.hostPublic ?: ByteArray(SessionLib.KEY_LEN),
            psk = creds?.keys?.psk ?: ByteArray(SessionLib.KEY_LEN),
            localPrivate = localPrivate,
            localPublic = localPublic,
            pathWidth = 1,
            sf = 7,
            floodReachHops = 4,
            airtimeMs = airtimeMs,
            maxOpenPages = MAX_PAGES,
        )
    }

    @Volatile
    private var closed = false

    /**
     * How many screens hold this engine.
     *
     * **One radio is one GATT client and one command window**, so the terminal and the settings
     * pages must share one engine rather than build one each: a second owner sending commands
     * outside the loop below is `RC-2` with a different name (`docs/RADIO-PLAN.md` §12.7d). The
     * factory hands out the same instance, and [close] is a *release* — the link, the lease and
     * the loop go away with the last holder.
     */
    private val holders = Holders()

    /** Called once, when the **last** holder released this engine. */
    var onFinalClose: ((AndroidRadioEngine) -> Unit)? = null

    /** One more screen is using this engine. Balanced by [close]. */
    fun retain() {
        holders.retain()
    }

    override val isOpen: Boolean get() = !closed && client != null

    /** Work for the loop: a call whose answer the caller awaits, or a bare wake. */
    private sealed interface Work {
        class Call<T>(val run: (RadioClient) -> T, val out: CompletableDeferred<T>) : Work
        data object Wake : Work
    }

    private val queue = Channel<Work>(Channel.UNLIMITED)

    private val pipe = AndroidBlePipe(
        // **The context the pipe needs to open a GATT connection.** `MainActivity` sets
        // `AndroidApp.context` to the application context before the first frame, and the engine is
        // built from a screen, so it is always set by then. Without it the pipe refuses to build and
        // the throw lands inside composition: opening Settings closed the whole app (measured
        // 2026-11-12, `IllegalArgumentException: a pipe with no transport needs a Context`), and the
        // terminal would have gone the same way the first time it opened a radio page.
        context = io.ldmux.mobile.AndroidApp.maybeContext,
        onFrame = { frame -> onNotification(frame) },
        onLink = { up ->
            // **The link's own transitions, in the operator's log.** Whether the pipe ever says
            // "up" is the first question when the radio is connected and the app is silent, and it
            // is invisible from every screen: measured 2026-11-12, a live GATT link with MTU 247
            // and not one frame written.
            io.ldmux.mobile.appLog("radio link ${if (up) "up" else "down"}")
            _linkUp.value = up
            if (up) _problem.value = null
            queue.trySend(Work.Wake)
        },
        onProblem = { text -> _problem.value = text },
        onState = { state -> _linkState.value = state },
        transport = transport,
    )

    private val scope = CoroutineScope(SupervisorJob() + LOOP)

    init {
        if (client != null) scope.launch { runLoop() }
    }

    /**
     * Open the GATT link, once.
     *
     * Idempotent because two screens share this engine and both call it on entry: a second
     * `connectToDevice` on a live link is a second GATT client, which is what the sharing exists to
     * prevent.
     */
    override suspend fun start() {
        if (client == null) {
            _problem.value = "this build cannot create a radio client"
            return
        }
        if (started) return
        started = true
        io.ldmux.mobile.appLog("radio start: $address")
        if (!pipe.connect(address)) io.ldmux.mobile.appLog("radio start refused: $address")
    }

    @Volatile
    private var started = false

    override suspend fun connect(nowMs: Long, wallMs: Long): Int = post { c ->
        if (!hostPaired) {
            // **A radio with no host has no session to start.** Said as the refusal it is, rather
            // than as a handshake that waits for a peer that was never named.
            _problem.value = "no host is paired · pair with the desktop first"
            return@post ClientCode.ERR_NOT_OPEN
        }
        val rc = c.connect(nowMs, wallMs)
        if (rc == ClientCode.OK && recordsLeft >= 0) c.setRecordsLeft(recordsLeft)
        rc
    }

    override suspend fun established(): Boolean = post { c -> c.established() }

    override suspend fun setPreset(
        freqKhz: Int,
        bwHz: Int,
        sf: Int,
        cr: Int,
        pathWidth: Int,
        powerDbm: Int,
    ): Int = post { c -> c.setPreset(freqKhz, bwHz, sf, cr, pathWidth, powerDbm) }

    override suspend fun setScope(key: ByteArray): Int = post { c -> c.setScope(key) }

    /**
     * The allowance the caller meters against.
     *
     * Held here as well as sent through, because a value that arrives before the client is open must
     * not be lost: it is applied on the next [connect] as well. It is the caller's number — a band's
     * duty-cycle rule — and is never derived here.
     */
    @Volatile
    private var recordsLeft: Int = -1

    override suspend fun setRecordsLeft(left: Int) {
        recordsLeft = left
        post { c -> c.setRecordsLeft(left) }
    }

    override suspend fun open(shell: String, cols: Int, rows: Int, nowMs: Long): Int =
        post { c -> c.open(shell, cols, rows, nowMs) }

    override suspend fun closePage(shell: String): Int = post { c -> c.closePage(shell) }

    override suspend fun end(shell: String, nowMs: Long): Int = post { c -> c.end(shell, nowMs) }

    override suspend fun createShell(cwd: String, nowMs: Long): Int =
        post { c -> c.createShell(cwd, nowMs) }

    override suspend fun takeover(shell: String, nowMs: Long): Int =
        post { c -> c.takeover(shell, nowMs) }

    override suspend fun makeLive(shell: String, nowMs: Long): Int =
        post { c -> c.makeLive(shell, nowMs) }

    override suspend fun history(shell: String, from: Int, rows: Int, nowMs: Long): Int =
        post { c -> c.history(shell, from, rows, nowMs) }

    override suspend fun list(nowMs: Long): Int = post { c -> c.list(nowMs) }

    override suspend fun send(
        shell: String,
        text: ByteArray,
        kind: Int,
        cols: Int,
        rows: Int,
        nowMs: Long,
    ): Int = post { c -> c.send(shell, text, kind, cols, rows, nowMs) }

    override suspend fun fetch(shell: String, cols: Int, rows: Int, nowMs: Long): Int =
        post { c -> c.fetch(shell, cols, rows, nowMs) }

    override suspend fun mirror(shell: String): ClientMirror? = post { c -> c.mirror(shell) }

    override suspend fun mirrorLine(shell: String, row: Int): ByteArray? =
        post { c -> c.mirrorLine(shell, row) }

    override suspend fun stats(): ClientStats? = post { c -> c.stats() }

    override suspend fun nextDeadline(): Long? = post { c -> c.nextDeadline(wallNow()) }

    /**
     * A notification from the radio.
     *
     * Called by the pipe on its own thread, so it does not touch the client: it posts the bytes to
     * the loop, which is the only place the client is called. That is the whole reason the loop
     * exists.
     */
    override fun onNotification(frame: ByteArray) {
        if (closed) return
        val copy = frame.copyOf()
        queue.trySend(Work.Call({ c -> c.onBleRx(copy, wallNow()) }, CompletableDeferred()))
    }

    override fun close() {
        // **A release, not a destroy.** See [holders]: the other screen may still be drawing.
        if (!holders.release()) return
        synchronized(this) {
            if (closed) return
            closed = true
        }
        onFinalClose?.invoke(this)
        // **Closing the pipe gives the lease back.** A phone that abandoned the radio used to leave
        // the other app believing it still held it (`RC-12`, `S17`).
        pipe.close()
        queue.close()
        scope.launch { client?.close() }
    }

    // ---- the loop -----------------------------------------------------------------------------

    /**
     * Apply everything that arrived, write what the engine owes, publish what it reports, sleep.
     *
     * The order is the library's contract: a notification is a record only once `onBleRx` has taken
     * it, so nothing is drained before every queued frame has been applied.
     */
    private suspend fun runLoop() {
        val c = client ?: return
        while (!closed) {
            while (true) {
                val work = queue.tryReceive().getOrNull() ?: break
                apply(c, work)
            }
            if (closed) return
            val progressed = pump(c)
            // **A frame the link refused is a deadline of its own.** It was taken out of the engine,
            // so no engine deadline covers it: with the wait derived from the engine's clock the loop
            // blocked on `queue.receive()` until the next status poll, path ask or bootstrap rung —
            // minutes, holding a record the caller had already been told was sent. Measured on the
            // phone 2026-11-13: `parts=2` and `backlog=0` with not one part frame ever written.
            val wait = if (pendingFrame != null) {
                keptFrameRetryMs
            } else {
                val deadline = c.nextDeadline(wallNow())
                deadline?.let { (it - wallNow()).coerceAtLeast(0) } ?: NEVER_MS
            }
            if (wait == 0L) {
                // **A deadline that has passed is not a reason to spin.** With the link down the
                // engine's bootstrap rung deadline never advances (nothing is transmitted, so
                // nothing is answered), `nextDeadline` keeps returning a time already past, and a
                // bare `continue` re-ran this loop with no suspend at all: one core pinned for as
                // long as the link was down mid-bootstrap. Measured on the phone 2026-11-12,
                // `adb shell echo` took 20 s with the app running and 0.57 s with it stopped.
                //
                // When the last turn wrote nothing there is nothing to do until a frame arrives or
                // a tick passes, so the loop waits a tick and re-reads the engine's deadline.
                if (!progressed) delay(loopFloorMs)
                continue
            }
            // A closed queue ends the loop; a timeout is the engine's own deadline arriving.
            val next = runCatching { withTimeoutOrNull(wait) { queue.receive() } }.getOrNull()
            if (next != null) apply(c, next)
        }
    }

    /**
     * Write every frame the engine owes, then publish what it says.
     *
     * **A frame taken from the engine is written or kept, never dropped.** `poll` hands one frame
     * over exactly once, so a write that is refused — the link went down between the check and the
     * write — would lose a companion command, and `APP_START` is sent once per connection. A kept
     * frame is offered again on the next turn, which is what makes the retry the loop's rather than
     * the library's.
     */
    private suspend fun pump(c: RadioClient): Boolean {
        var progressed = false
        while (true) {
            if (pendingFrame == null && pipe.ready) {
                pendingFrame = c.poll(wallNow())
                // **The boundary.** Every write is logged by the transport, so a frame that is taken
                // here and never written is otherwise invisible: three successive theories about the
                // field failure could not be falsified because nothing named this moment.
                pendingFrame?.let { taken ->
                    takenFrames += 1
                    if (takenFrames <= 20 || takenFrames % 50 == 0L) {
                        io.ldmux.mobile.appLog(
                            "frame taken ${taken.size}B op=0x" +
                                taken.take(2).joinToString("") { b ->
                                    (b.toInt() and 0xFF).toString(16).padStart(2, '0')
                                } + " (${takenFrames} so far)",
                        )
                    }
                }
                if (pendingFrame != null && !loggedFirstFrame) {
                    loggedFirstFrame = true
                    io.ldmux.mobile.appLog("radio first frame: ${pendingFrame?.size} bytes")
                }
            }
            val frame = pendingFrame ?: break
            if (!pipe.write(frame)) {
                // **Not once, for ever.** A one-shot line hid every refusal after the first, and a
                // refused frame is exactly what a stalled link looks like from inside: the frame is
                // kept and retried, so silence here is silence about a record that never went out.
                writeRefusals += 1
                if (writeRefusals == 1L || writeRefusals % 100L == 0L) {
                    io.ldmux.mobile.appLog(
                        "radio write refused: link not ready (${writeRefusals}x, " +
                            "frame ${frame.size} bytes held)",
                    )
                }
                break
            }
            pendingFrame = null
            progressed = true
        }
        _radio.value = c.radio()
        _route.value = c.route(wallNow())
        for (event in c.drainEvents()) {
            // **Each event, by name.** The screen's state is a translation of these, so a screen
            // that will not advance is either an event that never came or one the translator does
            // not recognise; the log says which.
            io.ldmux.mobile.appLog("event: ${event::class.simpleName}")
            _events.tryEmit(event)
            progressed = true
        }
        // **The client's own counters, once every few seconds.** "A record arrived and the session
        // layer refused it" and "a record never arrived" are identical on screen and have nothing in
        // common as faults; the log is the only place that separates them.
        val since = wallNow()
        if (since - loggedStatsAt >= statsLogMs) {
            loggedStatsAt = since
            c.stats()?.let { s ->
                io.ldmux.mobile.appLog(
                    "client: sessions=${s.sessionsEstablished} received=${s.recordsReceived} " +
                        "sent=${s.recordsSent} rungs=${s.rungsUsed} " +
                        "commands=${s.commandsSent} lists=${s.listsSent} " +
                        "admitted=${s.admitted} echo=${s.selfEcho} transit=${s.inTransit} " +
                        "hs=${s.handshakeRefused} unknown=${s.unknownDownlink} " +
                        "kind=${s.unknownKind} bad=${s.badRecord} sid=${s.sessionIndex} " +
                        "snapshots=${s.snapshotsApplied} parts=${s.partsSent} acks=${s.acksSent} " +
                        "queued=${s.radioRecords} radioAnswered=${s.radioAnswered} " +
                        "radioExpired=${s.radioExpired} backlog=${s.radioBacklog} " +
                        "readyFrames=${s.radioReady} malformed=${s.radioMalformed} " +
                        "window=${s.radioOutstanding} pumps=${s.radioPumps} " +
                        "fin=${s.answersFinished} rep=${s.answersRepeated} ign=${s.answersIgnored} " +
                        "refusal=${s.lastRecordRefusal}",
                )
            }
        }
        return progressed
    }

    /** The one frame the pipe would not take, kept for the next turn. */
    private var pendingFrame: ByteArray? = null

    /** Each of these is logged once, so a stall cannot flood the operator's log. */
    private var loggedFirstFrame = false

    /** When the client's counters were last written to the log, and how often they are. */
    private var loggedStatsAt = 0L
    private val statsLogMs = 1_000L

    /** How long the loop waits before retrying a frame the link refused. */
    private val keptFrameRetryMs = 50L

    /** How long the loop waits when a deadline has passed and nothing could be written. */
    private val loopFloorMs = 5L
    /** Frames taken from the engine, which is the moment a record enters the caller's hands. */
    private var takenFrames = 0L

    /** Frames the link refused to take, kept and retried; logged first and every hundredth. */
    private var writeRefusals = 0L

    private suspend fun apply(c: RadioClient, work: Work) {
        when (work) {
            is Work.Wake -> Unit
            is Work.Call<*> -> {
                @Suppress("UNCHECKED_CAST")
                val call = work as Work.Call<Any?>
                try {
                    call.out.complete(call.run(c))
                } catch (t: Throwable) {
                    call.out.completeExceptionally(t)
                }
            }
        }
    }

    /** Post one call to the loop and await the library's own answer. */
    private suspend fun <T> post(run: (RadioClient) -> T): T {
        if (client == null) throw IllegalStateException("the radio engine is not open")
        if (closed) throw IllegalStateException("the radio engine is closed")
        val out = CompletableDeferred<T>()
        queue.send(Work.Call(run, out))
        return out.await()
    }

    /** The engine reads its own clock: the loop never sees the caller's. */
    private fun wallNow(): Long = System.currentTimeMillis()

    companion object {
        /** One dispatcher confined to one worker, because the client is not thread safe. */
        private val LOOP: CoroutineDispatcher = Dispatchers.Default.limitedParallelism(1)

        /** How many pages the engine holds at once (`docs/RADIO-PLAN.md` §7). */
        const val MAX_PAGES = 5

        /**
         * One record's airtime at the public preset, in milliseconds.
         *
         * Every deadline inside the library is derived from this, so it is a fact about the air
         * rather than a choice: at 62.5 kHz, SF7, CR5 a full record occupies the channel for
         * 602.6 ms (`docs/AIRTIME.md`). A caller that knows the radio's tuple passes its own.
         */
        const val AIRTIME_MS = 602.6f

        /** "No deadline": the loop waits for a notification or a posted call. */
        private const val NEVER_MS = Long.MAX_VALUE
    }
}

/**
 * The platform's engine, or null when this build has no radio.
 *
 * Takes the address and the paired credentials because an engine cannot exist without them: the
 * client is created from the session keys, and the pipe needs somewhere to connect.
 */
actual fun radioEngine(creds: MeshCredentials?, address: String): RadioEngine? =
    if (nativeRadioAvailable()) RadioEngines.acquire(creds, address) else null

/**
 * The engines this process has open, one per radio address.
 *
 * **Two screens, one GATT client.** The terminal and the settings pages both need the radio, and
 * the radio accepts one central: a second engine would open a second connection, and a second owner
 * sending commands outside the engine's loop is `RC-2`'s shape. So a request for an address that is
 * already open returns the same engine with one more holder, and closing releases rather than
 * destroys. Keyed by address because that is what a GATT connection is to.
 */
private object RadioEngines {
    private val entries = mutableMapOf<String, AndroidRadioEngine>()

    @Synchronized
    fun acquire(creds: MeshCredentials?, address: String): RadioEngine {
        val known = entries[address]
        if (known != null) {
            known.retain()
            return known
        }
        val engine = AndroidRadioEngine(address, creds)
        engine.onFinalClose = { closed -> forget(address, closed) }
        entries[address] = engine
        return engine
    }

    /**
     * The engine that really closed has gone; a later request builds a new one.
     *
     * Guarded by identity: a *new* engine for the same address must not be removed by the old one's
     * callback, which can arrive after a screen has already rebuilt.
     */
    @Synchronized
    private fun forget(address: String, closed: AndroidRadioEngine) {
        if (entries[address] === closed) entries.remove(address)
    }
}

/** Whether the native client library loaded. Never throws. */
private fun nativeRadioAvailable(): Boolean = NativeRadioClient.available

/**
 * How many screens hold one engine, and whether the last one has let go.
 *
 * Pulled out of [AndroidRadioEngine] so the rule can be tested without a radio: the engine needs a
 * real `RadioClient` and a Bluetooth adapter, and a refcount that is only exercised on a device is
 * a refcount nobody has checked. **The terminal and the settings pages share one engine** — one
 * GATT client, one command window — so this is what stops Settings from closing a link the shell is
 * still drawing on, and what makes the last release the one that really closes.
 */
internal class Holders(initial: Int = 1) {
    private var count = initial

    @Synchronized
    fun retain() {
        count++
    }

    /** True when this release was the last, so the caller may really close. */
    @Synchronized
    fun release(): Boolean {
        if (count > 0) count--
        return count == 0
    }
}
