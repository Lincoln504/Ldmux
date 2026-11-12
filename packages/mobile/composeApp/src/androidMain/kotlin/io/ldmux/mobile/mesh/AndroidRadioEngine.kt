package io.ldmux.mobile.mesh

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
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
    creds: MeshCredentials,
    airtimeMs: Float = AIRTIME_MS,
    transport: io.iotone.meshcore.transport.MeshcoreTransport? = null,
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

    private val client: RadioClient? = RadioClient.create(
        peerStatic = creds.keys.hostPublic,
        psk = creds.keys.psk,
        localPrivate = creds.keys.localPrivate,
        localPublic = creds.keys.localPublic,
        pathWidth = 1,
        sf = 7,
        floodReachHops = 4,
        airtimeMs = airtimeMs,
        maxOpenPages = MAX_PAGES,
    )

    @Volatile
    private var closed = false

    override val isOpen: Boolean get() = !closed && client != null

    /** Work for the loop: a call whose answer the caller awaits, or a bare wake. */
    private sealed interface Work {
        class Call<T>(val run: (RadioClient) -> T, val out: CompletableDeferred<T>) : Work
        data object Wake : Work
    }

    private val queue = Channel<Work>(Channel.UNLIMITED)

    private val pipe = AndroidBlePipe(
        onFrame = { frame -> onNotification(frame) },
        onLink = { up ->
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

    override suspend fun start() {
        if (client == null) {
            _problem.value = "this build cannot create a radio client"
            return
        }
        pipe.connect(address)
    }

    override suspend fun connect(nowMs: Long, wallMs: Long): Int = post { c ->
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

    override suspend fun nextDeadline(): Long? = post { c -> c.nextDeadline() }

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
        if (closed) return
        closed = true
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
            pump(c)
            val deadline = c.nextDeadline()
            val wait = deadline?.let { (it - wallNow()).coerceAtLeast(0) } ?: NEVER_MS
            if (wait == 0L) continue
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
    private suspend fun pump(c: RadioClient) {
        while (true) {
            if (pendingFrame == null && pipe.ready) pendingFrame = c.poll(wallNow())
            val frame = pendingFrame ?: break
            if (!pipe.write(frame)) break
            pendingFrame = null
        }
        _radio.value = c.radio()
        _route.value = c.route(wallNow())
        for (event in c.drainEvents()) _events.tryEmit(event)
    }

    /** The one frame the pipe would not take, kept for the next turn. */
    private var pendingFrame: ByteArray? = null

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
actual fun radioEngine(creds: MeshCredentials, address: String): RadioEngine? =
    if (nativeRadioAvailable()) AndroidRadioEngine(address, creds) else null

/** Whether the native client library loaded. Never throws. */
private fun nativeRadioAvailable(): Boolean = NativeRadioClient.available
