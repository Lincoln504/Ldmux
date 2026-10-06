package dev.ldmux.mobile

import io.ktor.client.HttpClient
import io.ktor.client.request.header
import io.ktor.http.HttpHeaders
import io.ktor.client.plugins.websocket.DefaultClientWebSocketSession
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.websocket.Frame
import io.ktor.websocket.close
import io.ktor.websocket.readText
import kotlin.time.TimeMark
import kotlin.time.TimeSource
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** Terminal modes the server reports, which affect input encoding. */
data class TerminalModes(
    val mouse: Boolean = false,
    val appCursor: Boolean = false,
    val bracketedPaste: Boolean = false,
    /** A full-screen application is running; keys rarely echo there. */
    val alternateOn: Boolean = false,
    /** The program negotiated SGR mouse reporting, so a click can be synthesized. */
    val mouseSgr: Boolean = false,
    /**
     * Where the pane's cursor actually is, from the server's emulator.
     *
     * This is not the same as where the emulator's cursor ends up after the
     * snapshot is replayed. `capture-pane` pads the screen with blank rows, so
     * replaying it leaves the cursor at the bottom-left of the padded area — and
     * everything downstream trusts the emulator's cursor: it is what gets drawn,
     * and it is what a tap measures its distance from. Left alone, every tap
     * computes its delta from the wrong place.
     */
    val cursorX: Int = 0,
    val cursorY: Int = 0,
)

/**
 * One message from the daemon on an attached socket, parsed.
 *
 * Unknown `type`s parse to null and are ignored, as are unknown fields: the daemon
 * gains messages additively (a `session_ended`, a resync capability) and an older
 * client must keep working against it.
 */
internal sealed interface AttachMessage {
    /** The daemon is attached; [shiftKeys] says it understands the `S-` key modifier. */
    data class Caps(val shiftKeys: Boolean) : AttachMessage
    data class Pong(val id: Int?) : AttachMessage
    data class Snapshot(
        val cols: Int,
        val rows: Int,
        val screen: String,
        val history: Boolean,
        val modes: TerminalModes,
    ) : AttachMessage

    data class Error(val code: String) : AttachMessage
    data object MouseDelivered : AttachMessage
    data class Owned(val holder: String?) : AttachMessage
}

/** The control message in [text], or null when it is not one this client understands. */
internal fun parseAttachMessage(text: String): AttachMessage? {
    val root = parseJsonObject(text) ?: return null
    return when (root.text("type")) {
        "caps" -> AttachMessage.Caps(root.obj("capabilities")?.flag("shiftKeys") == true)
        "pong" -> AttachMessage.Pong(root.number("id"))
        "snapshot" -> {
            val cols = root.number("cols") ?: return null
            val rows = root.number("rows") ?: return null
            AttachMessage.Snapshot(
                cols = cols,
                rows = rows,
                screen = root.text("screen"),
                history = root.flag("history"),
                modes = TerminalModes(
                    mouse = root.flag("mouse"),
                    appCursor = root.flag("appCursor"),
                    bracketedPaste = root.flag("bracketedPaste"),
                    alternateOn = root.flag("alternateOn"),
                    mouseSgr = root.flag("mouseSgr"),
                    cursorX = root.number("cursorX") ?: 0,
                    cursorY = root.number("cursorY") ?: 0,
                ),
            )
        }
        "error" -> AttachMessage.Error(root.text("code").ifEmpty { "error" })
        "mousedelivered" -> AttachMessage.MouseDelivered
        "owned" -> AttachMessage.Owned((root["owner"] as? JsonObject)?.optionalText("name")?.ifEmpty { null })
        else -> null
    }
}

/** The payload of an Output frame (8-byte header: type 0x10, u32 BE length), or null. */
internal fun decodeOutputFrame(data: ByteArray): ByteArray? {
    if (data.size < 8) return null
    val type = data[0].toInt() and 0xff
    if (type != 0x10) return null // FrameType.Output
    val length = ((data[4].toInt() and 0xff) shl 24) or
        ((data[5].toInt() and 0xff) shl 16) or
        ((data[6].toInt() and 0xff) shl 8) or
        (data[7].toInt() and 0xff)
    // A length with the top bit set reads negative; it is a corrupt frame, not a big one.
    if (length <= 0) return null
    val end = (8L + length).coerceAtMost(data.size.toLong()).toInt()
    if (end <= 8) return null
    return data.copyOfRange(8, end)
}

/**
 * Attaches to one ldmux session and streams its pane output (protocol v1).
 *
 * Control is JSON text frames; terminal output arrives as binary frames with an
 * 8-byte big-endian header (type=0x10 Output, then a u32 length) followed by the
 * payload bytes. See `crates/protocol` and `docs/SPEC.md` §6.
 */
class AttachClient(private val config: LdmuxConfig, private val sessionId: String) {
    /**
     * Recreated on every start, because [stop] closes it: the pager keeps a
     * neighbouring page composed but should not keep it *connected* (see
     * `TerminalController.setActive`).
     */
    private var http: HttpClient? = null

    private fun httpClient(): HttpClient =
        http ?: HttpClient { ldmuxWebSockets() }.also { http = it }

    private var ws: DefaultClientWebSocketSession? = null
    private var job: Job? = null
    private var scope: CoroutineScope? = null
    /** The grid the surface will paint. Sent with attach, and on every change. */
    private var wantCols = 0
    private var wantRows = 0

    /**
     * Whether a socket is open right now. Input is refused while it is not (see
     * [InputGate]): a keystroke sent into nothing used to vanish without a trace.
     */
    val isConnected: Boolean get() = ws != null

    /**
     * The daemon understands the `S-` key modifier (its `caps` says so). An older daemon
     * refuses a whole `key` message it does not know, so Shift on a named key is only
     * sent when this is true.
     */
    var shiftKeys = false
        private set

    var onSnapshot: ((cols: Int, rows: Int, screen: String, history: Boolean, modes: TerminalModes) -> Unit)? = null
    var onOutput: ((ByteArray) -> Unit)? = null
    var onError: ((String) -> Unit)? = null

    /**
     * The server put a click into a mouse-aware program.
     *
     * The tap is finished when this arrives, so the refusal fallback must stand
     * down: on a program that took the click, cursor motion afterwards is a
     * second, wrong move. Measured: a tap ended `(2,4)` instead of `(7,2)`.
     */
    var onMouseDelivered: (() -> Unit)? = null
    /**
     * A measured round trip, in ms. The display's repaint cadence follows this
     * (see FramePacing): a phone should not paint faster than the link can
     * answer, and it should slow down rather than queue up when the link is bad.
     */
    var onRtt: ((Long) -> Unit)? = null
    /** The connection went away; the current round-trip estimate is worthless. */
    var onOffline: (() -> Unit)? = null
    /**
     * Another device holds this shell (ADR-0010).
     *
     * The attach was *refused*, not failed — nothing is wrong with the link. The
     * UI answers this by naming the holder and offering to take over.
     */
    var onOwned: ((holder: String?) -> Unit)? = null
    /**
     * The shell is gone (ended, or unknown to the daemon). The loop stops: there
     * is nothing to re-attach to, and retrying it forever is a page that polls a
     * corpse.
     */
    var onEnded: (() -> Unit)? = null

    /**
     * The daemon answered and refused the token: the pairing is stale (rotated).
     *
     * The equivalent of [LinkState.Rejected] on the launcher socket. Without it a
     * rotated token retried forever with nothing on screen but a link that never
     * came up. The loop waits (a network change, or [retryNow]) instead of hammering.
     */
    var onRejected: (() -> Unit)? = null

    /** The daemon accepted the attach (`caps`): the link is up and the shell is ours. */
    var onAttached: (() -> Unit)? = null

    /**
     * Run one callback so that a failure in it is *its* failure.
     *
     * A handler throwing used to land in the receive loop's catch-all, which reads
     * as a dead link: one bad frame or one clipboard failure dropped the socket,
     * reconnected, and rebuilt the terminal. Each callback is guarded separately, so
     * a failing snapshot handler does not also lose the pong that follows it.
     */
    private inline fun guarded(what: String, block: () -> Unit) {
        try {
            block()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            appLog("page $sessionId: $what handler failed: ${e::class.simpleName}")
        }
    }

    /**
     * Take the shell from whoever has it.
     *
     * Only ever called after the user was told who has it: attachment is never a
     * silent steal, and on the other side this makes a live window say "open on
     * another device" rather than freeze.
     */
    fun takeOver() {
        wantTakeover = true
        reconnectNow()
        kick.trySend(Unit)
    }

    /** Try again now, from a refused-token wait or any other. */
    fun retryNow() {
        kick.trySend(Unit)
    }

    /** Wakes a loop parked after a refused token. */
    private val kick = Channel<Unit>(Channel.CONFLATED)

    /**
     * Set once the user has consented, consumed by the next attach.
     *
     * Consumed rather than kept: if the shell is taken back by the other device a
     * moment later, silently taking it again would be a tug of war nobody asked
     * for. The second time, the user is asked again.
     */
    private var wantTakeover = false


    /** Reconnect without the backoff pause, once — a resync, not an outage. */
    private var skipBackoffOnce = false

    /** Set when the daemon says the shell no longer exists; ends the loop. */
    private var ended = false

    /**
     * End the current connection so the loop reconnects at once.
     *
     * Closing rather than waiting: the loop's own retry has a deliberate pause
     * before it reconnects, and a user who just tapped "take it over" should not
     * watch that pause.
     */
    private fun reconnectNow() {
        val current = ws ?: return
        scope?.launch { runCatching { current.close() } }
    }

    private var pingSeq = 0
    private var pingSentAt: TimeMark? = null
    private val missed = MissedPongs()
    /** Pongs seen on this client, ever: a network-change probe compares two readings. */
    private var pongCount = 0
    /** Retry attempts since the last successful connection, for the backoff. */
    private var attempt = 0

    fun start(scope: CoroutineScope) {
        // `isActive`, not `!= null`: a loop that ended on its own (a shell that is
        // gone, no pairing) leaves a finished job behind, and that must not make
        // the page unable to start again.
        if (job?.isActive == true) return
        // Taking the shell over is what opening it on the phone means, and it is
        // every time the page comes to the front, not once per page. The page gives
        // the shell back when it swipes away ([stop] closes the socket and the daemon
        // releases the claim after its grace), so coming back is the same act as
        // arriving: the swipe the user just made *is* the consent.
        //
        // A *reconnect* inside one activation does not re-arm: the loop's retry does
        // not call this, so a link that blinks cannot take a shell.
        wantTakeover = true
        ended = false
        kick.tryReceive()
        this.scope = scope
        job = scope.launch { loop() }
    }

    /** Detach and stop streaming. The connection is not kept in any form. */
    fun stop() {
        job?.cancel()
        job = null
        // Consent is for the attach it was given for. A takeover tapped while the
        // link was down must not fire against whoever holds the shell minutes later.
        wantTakeover = false
        attempt = 0
        scope = null
        ws = null
        http?.let { runCatching { it.close() } }
        http = null
    }

    private suspend fun loop() {
        while (currentCoroutineContext().isActive) {
            var connectedThisRound = false
            var rejected = false
            // A page exists only for a paired app; an unpaired config ends it.
            val endpoint = config.endpoint ?: return
            try {
                httpClient().webSocket(endpoint.wsUrl, request = { header(HttpHeaders.Authorization, endpoint.authorization) }) {
                    ws = this
                    connectedThisRound = true
                    missed.reset()
                    appLog("page $sessionId: connected, attaching${if (wantTakeover) " (take over)" else ""}")
                    // Say who we are first, exactly as the session-list client
                    // does. A connection with no identity holds nothing, so the
                    // daemon would have no claim to refuse a second device with.
                    send(Frame.Text(helloMessage(config)))
                    send(Frame.Text(
                        buildJsonObject {
                            put("type", "attach")
                            put("sessionId", sessionId)
                            if (wantTakeover) {
                                put("takeover", true)
                                // Consumed here, before the answer is known: a
                                // refusal must not leave the next attempt armed to
                                // take over whatever it finds.
                                wantTakeover = false
                            }
                            if (wantCols > 0 && wantRows > 0) {
                                put("cols", wantCols)
                                put("rows", wantRows)
                            }
                        }.toString(),
                    ))
                    // Measure the link from inside the same connection the output
                    // travels on, so the number the pacing uses is the delay the
                    // user actually experiences. The first ping waits for the
                    // handshake to settle.
                    val pinger = launch {
                        delay(FIRST_PING_MS)
                        while (isActive) {
                            val id = ++pingSeq
                            pingSentAt = TimeSource.Monotonic.markNow()
                            runCatching { send(Frame.Text("""{"type":"ping","id":$id}""")) }
                            // A ping nobody answers does not detect anything: the
                            // socket is only known to be dead when the OS says so,
                            // and on a sleeping radio or a NAT that has forgotten
                            // the flow, nothing says so for a long time — the
                            // terminal just stops updating. Three unanswered pings
                            // is a link that is not there, whatever it claims.
                            if (missed.beat()) {
                                runCatching { ws?.close() }
                                break
                            }
                            delay(PING_INTERVAL_MS)
                        }
                    }
                    // The network just changed under a socket that still looks open
                    // (Wi-Fi to LTE leaves the old one "connected" for 12-16 s). Ask
                    // now and give the answer a short, fixed time instead of waiting
                    // out three heartbeats.
                    val netWatch = launch {
                        NetworkWake.wakes.collect {
                            val before = pongCount
                            runCatching { send(Frame.Text("""{"type":"ping","id":$PROBE_PING_ID}""")) }
                            delay(NETWORK_PROBE_MS)
                            if (pongCount == before) {
                                appLog("page $sessionId: no answer after a network change; reconnecting")
                                runCatching { ws?.close() }
                            }
                        }
                    }
                    try {
                        for (frame in incoming) {
                            when (frame) {
                                is Frame.Text -> guarded("control") { handleControl(frame.readText()) }
                                is Frame.Binary -> guarded("output") { handleBinary(frame.data) }
                                else -> Unit
                            }
                        }
                    } finally {
                        pinger.cancel()
                        netWatch.cancel()
                        ws = null
                    }
                }
            } catch (e: CancellationException) {
                // Deactivation, not a broken link: the caller asked for this.
                throw e
            } catch (e: Throwable) {
                // Offline / daemon restart: retry below. Said, because a page that
                // silently retries is indistinguishable from a page that is stuck.
                appLog("page $sessionId: link lost: ${e::class.simpleName}")
                // A handshake that never completed says nothing about *why*. Ask the
                // daemon the way the launcher does: a refused token is not a link
                // problem and retrying it on a timer cannot fix it.
                if (!connectedThisRound) {
                    rejected = linkStateFromProbe(probeLinkStatus(httpClient(), endpoint)) == LinkState.Rejected
                }
            }
            appLog("page $sessionId: disconnected")
            ws = null
            pingSentAt = null
            if (ended) {
                guarded("ended") { onEnded?.invoke() }
                return
            }
            guarded("offline") { onOffline?.invoke() }
            if (rejected) {
                appLog("page $sessionId: token refused; waiting")
                guarded("rejected") { onRejected?.invoke() }
                // Waiting will not fix a refused token. A network change, the user
                // retrying, or (rarely) the daemon being given its old token back.
                withTimeoutOrNull(REJECTED_RECHECK_MS) {
                    merge(NetworkWake.wakes, kick.receiveAsFlow()).first()
                }
                attempt = 0
                continue
            }
            // No pause when a takeover is armed: that path is a user waiting on
            // the result of a tap, not a link that needs a moment to recover.
            //
            // Backoff with jitter, capped, because this runs once per open page.
            // A fixed 1.5 s retry meant a phone with no route asked again 40 times
            // a minute per shell forever — the radio cost of a client that is not
            // there — and a daemon restart had every page of every phone coming
            // back in lockstep. The list client uses the same policy
            // ([reconnectDelayMs]); two policies for one app is drift.
            if (skipBackoffOnce) {
                skipBackoffOnce = false
            } else {
                // The one attempt that follows an armed takeover is immediate — the
                // user tapped and is watching — and every attempt after that backs
                // off, because `wantTakeover` is only consumed by a connection that
                // *succeeds* and a page that cannot reach the daemon at all would
                // otherwise retry with no delay, forever.
                val immediate = wantTakeover && attempt == 0
                val base = if (immediate) 0L else reconnectDelayMs(attempt)
                attempt++
                if (base > 0 && NetworkWake.sleep(withJitter(base))) attempt = 0
            }
        }
    }

    /** One text frame. Internal so the framing is tested without a socket. */
    internal fun handleControl(text: String) {
        when (val msg = parseAttachMessage(text) ?: return) {
            // The daemon has answered, so the link works and the next failure is
            // a new one: the backoff starts over rather than staying where the
            // last outage left it.
            is AttachMessage.Caps -> {
                attempt = 0
                shiftKeys = msg.shiftKeys
                appLog("page $sessionId: attached")
                guarded("attached") { onAttached?.invoke() }
            }
            is AttachMessage.Pong -> {
                missed.onPong()
                pongCount++
                // The probe's own pong is not a round-trip sample for the heartbeat.
                if (msg.id == PROBE_PING_ID) return
                val sent = pingSentAt ?: return
                pingSentAt = null
                val rtt = sent.elapsedNow().inWholeMilliseconds
                guarded("rtt") { onRtt?.invoke(rtt) }
            }
            is AttachMessage.Snapshot ->
                guarded("snapshot") { onSnapshot?.invoke(msg.cols, msg.rows, msg.screen, msg.history, msg.modes) }
            is AttachMessage.Error -> {
                val code = msg.code
                appLog("page $sessionId: error $code")
                when (code) {
                    // The daemon dropped output it could not deliver (the stream
                    // reconnected, or this socket fell too far behind) and stopped
                    // sending until the client attaches again. Ignoring it left a
                    // terminal that was wrong and then silent, with pings still
                    // answered so nothing ever noticed. A fresh attach is the
                    // recovery: it carries a snapshot of the screen as it is now.
                    "resync" -> {
                        skipBackoffOnce = true
                        reconnectNow()
                    }
                    "unknown_session", "session_dead" -> {
                        ended = true
                        reconnectNow()
                    }
                    // The attach itself failed on the daemon; the socket is healthy
                    // and would sit there with no stream. Retry, with the backoff.
                    "attach_failed" -> reconnectNow()
                }
                guarded("error") { onError?.invoke(code) }
            }
            AttachMessage.MouseDelivered -> guarded("mouse") { onMouseDelivered?.invoke() }
            is AttachMessage.Owned -> {
                // Refused, not broken: the shell is held by another device. Told
                // as an ordinary outcome so the UI can offer to take it over
                // rather than showing an error about the link.
                appLog("page $sessionId: held by ${msg.holder}")
                guarded("owned") { onOwned?.invoke(msg.holder) }
            }
        }
    }

    /** One binary frame. Internal for the same reason as [handleControl]. */
    internal fun handleBinary(data: ByteArray) {
        val payload = decodeOutputFrame(data) ?: return
        guarded("output") { onOutput?.invoke(payload) }
    }

    /** Whether the message went to a socket. A refused message is the caller's to report. */
    private fun sendControl(msg: String): Boolean {
        val socket = ws ?: return false
        val s = scope ?: return false
        s.launch { runCatching { socket.send(Frame.Text(msg)) } }
        return true
    }

    fun input(text: String): Boolean =
        sendControl(buildJsonObject { put("type", "input"); put("data", text) }.toString())

    /**
     * Tell the daemon the grid this client will paint. Sent with attach (so the
     * first snapshot is already the right size) and again on every viewport
     * change; the daemon answers a change with a fresh snapshot.
     */
    fun setGrid(cols: Int, rows: Int) {
        if (cols <= 0 || rows <= 0) return
        if (cols == wantCols && rows == wantRows) return
        wantCols = cols
        wantRows = rows
        // Before the socket exists the size rides along with attach; the daemon
        // would have no stream to resize yet anyway.
        if (ws == null) return
        sendControl(
            buildJsonObject {
                put("type", "resize")
                put("cols", cols)
                put("rows", rows)
            }.toString(),
        )
    }

    fun keys(keys: List<String>): Boolean {
        val array = keys.joinToString(",") { quote(it) }
        return sendControl("""{"type":"key","keys":[$array]}""")
    }

    /**
     * Deliver a tap to a mouse-aware program. Only valid when the pane reported
     * SGR mouse reporting; the server synthesizes the click so the byte sequence
     * never has to live in the client.
     */
    fun mouseClick(col: Int, row: Int): Boolean =
        sendControl(
            buildJsonObject {
                put("type", "mouseclick")
                put("col", col)
                put("row", row)
            }.toString(),
        )

    fun scrollback(lines: Int): Boolean =
        sendControl(
            buildJsonObject {
                put("type", "scrollback")
                put("sessionId", sessionId)
                put("lines", lines)
            }.toString(),
        )

    /**
     * Repaint the visible screen from the server. `lines = 0` is the
     * return-to-bottom form (no history); it is also the recovery step after a
     * failed input prediction, because a snapshot is the only way to un-apply
     * bytes already fed to the emulator.
     */
    fun snapshotNow() = scrollback(0)

    private fun quote(s: String): String = JsonPrimitive(s).toString()

    internal companion object {
        /** Long enough that the handshake is not what gets measured. */
        const val FIRST_PING_MS = 600L
        /**
         * Every few seconds is plenty: the estimate only has to follow a real
         * change in link quality, and a phone on a metered connection should not
         * spend its budget on keep-alives. Each ping is a handful of bytes.
         *
         * Shared with the launcher socket, which has the same duty and no reason
         * to hold a second opinion — see [CLIENT_PING_INTERVAL_MS].
         */
        const val PING_INTERVAL_MS = CLIENT_PING_INTERVAL_MS

        /** The id of a network-change probe, which is not a round-trip sample. */
        const val PROBE_PING_ID = -1

        /** How long a page parked on a refused token waits before looking again. */
        const val REJECTED_RECHECK_MS = 300_000L
    }
}
