package io.ldmux.mobile

import io.ktor.client.HttpClient
import io.ktor.client.request.header
import io.ktor.http.HttpHeaders
import io.ktor.client.plugins.websocket.DefaultClientWebSocketSession
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.websocket.Frame
import io.ktor.websocket.close
import io.ktor.websocket.readText
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.cancel
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/** One ldmux session, as the daemon reports it in a `sessions` message. */
data class Session(
    val id: String,
    val kind: String,
    val title: String,
    val cwd: String,
    val status: String,
    /** Empty for an RPC-only pi session (nothing to attach to in the keeper). */
    val keeperName: String,
    /** Who opened it: "phone", "local", "cli", "adopt". */
    val createdBy: String = "",
    /**
     * What the pane is running — `bash`, `vim`, `pi`.
     *
     * The launcher shows this because a directory alone does not identify a
     * shell: two rows in the same folder are otherwise the same row, and "which
     * of these is my editor" is the question the list is for.
     */
    val process: String = "",
    /**
     * Whether a desktop window is holding this shell open, and where.
     *
     * The server's fact, not a window-manager probe: a client cannot see the
     * desktop, so anything it inferred would be a guess — and it was, cached for
     * a second, and wrong in both directions.
     */
    val liveWorkspace: String? = null,
    /**
     * Whether this shell has **ever** had a desk window.
     *
     * `liveWorkspace` says "has one now"; this says "ever had one", and the pair is what
     * separates a shell created headlessly from the CLI — an ordinary row — from one whose
     * window went away, which is the only case worth an amber label. Absent from an older
     * daemon, so it defaults to false and nothing is flagged.
     */
    val hadWindow: Boolean = false,
    /**
     * The device holding this shell, when one does (ADR-0010).
     *
     * One device at a time. A shell someone else holds is not refused — it is
     * offered, with who has it, and the user decides.
     */
    val ownerName: String? = null,
    /** "phone" or "desktop". */
    val ownerKind: String? = null,
    /** True while the shell is held by some *other* device. */
    val heldElsewhere: Boolean = false,
) {
    /**
     * Can this session be opened as a terminal page?
     *
     * Not merely "has a keeper name": a session that has **died** keeps its name in
     * the registry until the record is pruned, so the pager used to spend pages
     * on corpses — and since the page you were looking at could be one of them,
     * taps and keys went nowhere with no explanation. Only a running session is
     * a page; `starting` counts, because it is on its way.
     */
    val attachable: Boolean get() = keeperName.isNotEmpty() && status != "dead"
    val label: String get() = if (title.isNotEmpty()) title else id

    /**
     * Did this phone open it?
     *
     * Only a shell the phone opened gets the per-shell menu (put it on the
     * desktop / close it). A shell someone started at their desk is theirs: the
     * phone may look at it, but it is not the phone's to move or end.
     */
    val openedByPhone: Boolean get() = createdBy == "phone"
}

/**
 * One desktop workspace, as the daemon's window manager reports it.
 *
 * `windowCount` is what makes this useful in the launcher: a workspace with five
 * windows on it is a place with things in it, not just a number.
 */
data class DesktopWorkspace(
    val name: String,
    val index: Int?,
    val focused: Boolean,
    val visible: Boolean,
    val windowCount: Int,
)

/** One window on the desktop. */
data class DesktopWindow(
    val id: String,
    val appClass: String,
    val title: String,
    val workspace: String,
)

/** The outcome of a make-live or close-live request. */
data class LiveResult(val sessionId: String, val ok: Boolean, val detail: String?)

/** Everything the launcher shows before a terminal. */
data class DesktopInventory(
    val wm: String,
    val workspaces: List<DesktopWorkspace>,
    val windows: List<DesktopWindow>,
    /** Set when the window manager could not be enumerated. */
    val detail: String? = null,
) {
    /** Is there anything to show at all? */
    val isEmpty: Boolean
        get() = workspaces.isEmpty() && windows.isEmpty()
}

/**
 * A minimal daemon client for the native shell: the session *list* (for the
 * pager) and the ability to open and end sessions. The terminal itself is
 * attached over the same `/ws` endpoint by [AttachClient]; this keeps its own
 * connection so a terminal stream failing never blinds the session list.
 */
class LdmuxClient(private var config: LdmuxConfig) {
    /**
     * Recreated by [start], because [stop] closes it. It was a `val` that [stop]
     * closed for good, so a stopped client could never be started again — which is why
     * nothing ever stopped it, and the launcher socket pinged for as long as the process
     * lived, backgrounded or not.
     */
    private var httpClient: HttpClient? = null
    /**
     * Tear the socket down without cancelling the coroutine that is reading it (the same reason
     * as `AttachClient.dropSocket`: a close is a write that hangs, a cancel kills the retry loop).
     */
    private fun dropSocket() {
        val c = httpClient
        httpClient = null
        runCatching { c?.close() }
    }

    private val http: HttpClient get() = httpClient ?: HttpClient { ldmuxWebSockets() }.also { httpClient = it }

    /** The id this device announces itself as. Not an account, not a credential. */
    private val deviceId: String get() = config.deviceId

    private val _sessions = MutableStateFlow<List<Session>>(emptyList())
    val sessions: StateFlow<List<Session>> = _sessions

    /**
     * Whether the socket is up right now.
     *
     * The app is a remote terminal: disconnected is a visible state, not a
     * spinner. It also drives the foreground service — the daemon link is the
     * only reason this process needs to stay alive while the screen is off.
     */
    private val _connected = MutableStateFlow(false)
    val connected: StateFlow<Boolean> = _connected

    /** The link as one value (see [LinkState]); [connected] is its `Online` case. */
    private val _link = MutableStateFlow(initialLink(config))
    val link: StateFlow<LinkState> = _link

    /**
     * Which run of [loop] is current. A cancelled loop still runs its cleanup, and
     * without this it cleared the *new* loop's socket and marked it disconnected.
     */
    private var generation = 0

    /**
     * Woken when something outside the retry loop knows the network just came
     * back. The loop is usually asleep in its backoff, and waiting out the rest
     * of a ten-second sleep after the phone has already rejoined the wifi is the
     * difference between a terminal that feels live and one that feels broken.
     */
    private val wake = MutableSharedFlow<Unit>(extraBufferCapacity = 1)

    private val _desktop = MutableStateFlow<DesktopInventory?>(null)
    val desktop: StateFlow<DesktopInventory?> = _desktop

    /**
     * Bumped for every inventory that arrives.
     *
     * The value itself is not enough to know a refresh finished: two readings a
     * second apart are usually identical, and a screen watching the value would
     * never see the second one and would sit on its spinner forever.
     */
    private val _desktopTick = MutableStateFlow(0)
    val desktopTick: StateFlow<Int> = _desktopTick

    /**
     * The daemon's answer to a control message that failed — `new`, `close`,
     * `kill`, an unattached scrollback request, bad JSON, anything the `catch`
     * around the daemon's message switch turns into `{type:"error"}`.
     *
     * This used to arrive and go straight in the bin: `onMessage` below had no
     * `"error"` branch at all, so a rejected `new` (e.g. a path that is not a
     * directory) looked identical to one that was silently ignored, and a
     * refresh that failed left its "…" on screen forever with nothing to say
     * why. One place to catch it, so every caller gets an answer instead of a
     * guess.
     */
    data class ServerError(val code: String, val message: String)
    private val _serverError = MutableStateFlow<ServerError?>(null)
    val serverError: StateFlow<ServerError?> = _serverError

    /** Bumped for every error, the same way [desktopTick] is: two identical
     * errors in a row must still both be seen. */
    private val _errorTick = MutableStateFlow(0)
    val errorTick: StateFlow<Int> = _errorTick

    /**
     * The id of the shell the daemon just created, on request.
     *
     * A `sessions` list cannot say which row is the one you asked for, so the
     * client used to match it by directory — a guess that fails when the path was
     * blank (the daemon chose the directory, so there is nothing to match) and
     * when the directory already has a shell (the guess finds that one). What
     * followed was a client that had just asked for a shell, standing on a terminal
     * screen that said "No sessions".
     *
     * It is a *list*, not a slot, because one gesture can ask for more than one
     * shell: choosing two rows in the launcher and pressing open creates two, and
     * they are created in a loop. With a single slot the second id overwrote the
     * first before anything read it, so `open (2)` produced one page and the other
     * shell existed with nothing pointing at it. The tick below was added to make
     * "two in a row" visible and did not help on its own — it told the reader there
     * was something new, while the value it read had already been replaced.
     */
    private val _created = MutableStateFlow<List<String>>(emptyList())
    val created: StateFlow<List<String>> = _created

    /** Bumped for every creation, so two in a row are both seen. */
    private val _createdTick = MutableStateFlow(0)
    val createdTick: StateFlow<Int> = _createdTick

    /** Called once the ids have been taken, so the next one is a new event. */
    fun clearCreated() {
        _created.value = emptyList()
    }

    /**
     * The last thing that happened to a shell's desktop window.
     *
     * Reported rather than assumed: putting a window on the desktop is
     * best-effort — the host may have no window manager, or an environment this
     * daemon cannot place windows in — and silently doing nothing would look
     * exactly like the button being broken.
     */
    private val _liveResult = MutableStateFlow<LiveResult?>(null)
    val liveResult: StateFlow<LiveResult?> = _liveResult

    fun clearLiveResult() {
        _liveResult.value = null
    }

    /** The banner has been read (or timed out); nothing more to say. */
    fun clearServerError() {
        _serverError.value = null
    }

    /**
     * Ask the host what is on its desktop.
     *
     * On demand, not polled: answering spawns the window manager's own tools, and
     * nothing about the answer is worth waking the radio for on a timer.
     */
    fun requestDesktop() = sendControl("""{"type":"desktop"}""", queueIfOffline = true)

    /** Refresh the session list — used for manual pull-to-refresh and on resume, so
     * the launcher shows fresh CWDs without waiting for the daemon's 2s CWD drift
     * poll to fire. */
    fun refreshSessions() = sendControl("""{"type":"list"}""", queueIfOffline = true)

    /** Put this shell's terminal on the desktop's screen, on a chosen workspace. */
    fun makeLive(sessionId: String, workspace: String) = sendControl(
        buildJsonObject {
            put("type", "makelive")
            put("sessionId", sessionId)
            put("workspace", workspace)
        }.toString(),
    )

    /** Take this shell's desktop window away. */
    fun closeLive(sessionId: String) = sendControl(
        buildJsonObject {
            put("type", "closelive")
            put("sessionId", sessionId)
        }.toString(),
    )

    private var job: Job? = null
    private var scope: CoroutineScope? = null
    private var ws: DefaultClientWebSocketSession? = null

    /**
     * Open a new shell on the host (FR-10). The list refreshes from the reply.
     *
     * There is no `command` and no `kind` here on purpose: every shell this client can
     * ask for is the host's login shell, never a chosen program, so there is nothing
     * for a caller to pass.
     */
    fun create(cwd: String? = null, title: String? = null): Boolean {
        val msg = buildJsonObject {
            put("type", "new")
            if (cwd != null) put("cwd", cwd)
            if (title != null) put("title", title)
        }.toString()
        // A create with no socket used to vanish: the launcher showed "Opening a
        // shell…" for a request that was never sent, and nothing cleared it because
        // the connection was *already* down. It is answered here the way the daemon
        // would answer a refused one, so the same path clears the wait and says why.
        if (!sendControl(msg)) {
            _serverError.value = ServerError(code = "offline", message = "not connected to the host")
            _errorTick.value += 1
            return false
        }
        return true
    }

    /**
     * Close a shell the way the user means it.
     *
     * Deliberately not `kill`. Closing lets go of a shell, and whether that also
     * ends it depends on whether a desktop window is holding it open — a fact the
     * phone cannot see, so the daemon decides and answers (ADR-0010).
     */
    fun close(sessionId: String) {
        val msg = buildJsonObject {
            put("type", "close")
            put("sessionId", sessionId)
        }.toString()
        sendControl(msg)
    }

    /** End a session outright. The unconditional form, kept for the CLI's sake. */
    fun kill(sessionId: String) {
        val msg = buildJsonObject {
            put("type", "kill")
            put("sessionId", sessionId)
        }.toString()
        sendControl(msg)
    }

    /**
     * Messages asked for before the socket exists.
     *
     * The first thing the app does is ask what is on the machine, and that
     * reliably happens *before* the websocket finishes opening — so without this
     * the launcher sat empty until someone thought to pull to refresh, and
     * nothing anywhere said anything had been lost.
     *
     * Only reads are kept. A keystroke or a click held across a reconnect would
     * be delivered at a moment nobody meant it for; asking again for the state of
     * the world is harmless whenever it arrives.
     */
    private val pending = ArrayDeque<String>()

    /** Bounded, because a client that cannot reach the daemon must not grow. */
    private val maxPending = 16

    /** Returns whether the message went to a socket (queued reads count as sent). */
    private fun sendControl(msg: String, queueIfOffline: Boolean = false): Boolean {
        val socket = ws
        if (socket == null) {
            if (queueIfOffline && pending.size < maxPending) {
                pending.addLast(msg)
                return true
            }
            return false
        }
        scope?.launch {
            runCatching { socket.send(Frame.Text(msg)) }
        }
        return true
    }

    /** The launcher's liveness: heartbeats that went unanswered. */
    private val missed = MissedPongs()

    /** Pongs seen, ever: a network-change probe compares two readings of it. */
    private var pongCount = 0

    /**
     * Point the client at a (possibly) different daemon, or at none.
     *
     * Everything learned from the old one goes: its shells, its desktop and the
     * reads queued for it. Keeping them showed the new host's name above the old
     * host's shells for as long as the new one was unreachable.
     */
    fun reconfigure(config: LdmuxConfig) {
        this.config = config
        job?.cancel()
        job = null
        generation++
        ws = null
        pending.clear()
        _sessions.value = emptyList()
        _desktop.value = null
        _connected.value = false
        _link.value = initialLink(config)
        scope?.let { s -> if (config.endpoint != null) job = s.launch { loop() } }
    }

    fun start(scope: CoroutineScope) {
        this.scope = scope
        if (job?.isActive == true || config.endpoint == null) return
        if (_link.value != LinkState.Online) _link.value = LinkState.Connecting
        job = scope.launch { loop() }
    }

    /**
     * Close the socket and stop retrying. Safe to call and to follow with [start]:
     * the HTTP client is rebuilt on demand.
     */
    fun stop() {
        job?.cancel()
        job = null
        generation++
        scope = null
        ws = null
        _connected.value = false
        // A closed socket is not an online link. Leaving the old state in place meant a
        // stop/start cycle — the app going away and coming back, or Disconnect then return —
        // showed `Online` until the next attempt finished, which also suppressed the
        // `unreachable` line and the rule that returns the user to the shell list.
        _link.value = initialLink(config)
        httpClient?.let { runCatching { it.close() } }
        httpClient = null
    }

    /**
     * Retry right now instead of waiting out the backoff. Called when the OS says
     * a usable network has appeared.
     */
    fun reconnectNow() {
        wake.tryEmit(Unit)
    }

    private suspend fun loop() {
        val gen = ++generation
        val endpoint = config.endpoint ?: return
        fun current() = gen == generation
        // Backed off rather than fixed, and capped (see [reconnectDelayMs], the policy
        // the terminal pages share): a daemon that is down for an hour should not be
        // asked twelve hundred times, and one that comes back ten seconds later should
        // not make the user wait long to notice.
        var attempt = 0
        while (currentCoroutineContext().isActive) {
            try {
                appLog("connecting to ${endpoint.host}")
                http.webSocket(endpoint.wsUrl, request = { header(HttpHeaders.Authorization, endpoint.authorization) }) {
                    ws = this
                    attempt = 0
                    // Flush what was asked for while there was no socket, oldest
                    // first, before the fresh list — the order the user asked in.
                    for (queued in pending) runCatching { send(Frame.Text(queued)) }
                    pending.clear()
                    _connected.value = true
                    _link.value = LinkState.Online
                    appLog("connected")
                    // Who we are, before anything else (ADR-0010). Ownership is
                    // keyed on the connection plus this identity, and a device's
                    // claims are dropped when the connection ends — so a client
                    // that never says who it is holds nothing, and exclusivity
                    // silently stops applying to it.
                    send(Frame.Text(helloMessage(config)))
                    send(Frame.Text("""{"type":"list"}"""))
                    // Say something, periodically. The daemon reaps a connection
                    // that has said nothing for a minute — that is how a phone which
                    // vanished without a FIN stops holding shells it is not looking
                    // at — and a client that never speaks would be reaped while it
                    // was perfectly healthy, leaving the launcher flickering offline
                    // once a minute. A ping is the cheapest thing that can be said,
                    // and the daemon already answers it.
                    //
                    // ...and the answers are counted. A ping nobody answers detects
                    // nothing: on a sleeping radio or a NAT that has forgotten the
                    // flow the socket stays "connected" while the list goes stale and
                    // every command sent into it disappears. Three unanswered beats is
                    // a link that is not there — the same rule the terminal pages use.
                    missed.reset()
                    val beat = launch {
                        while (isActive) {
                            delay(CLIENT_PING_INTERVAL_MS)
                            if (missed.beat()) {
                                appLog("launcher link silent; reporting unreachable")
                                // Reported, then the socket is dropped. Reporting alone left the
                                // read loop parked in `for (frame in incoming)` below, so `loop()`
                                // never reached its retry: on a stable network with a silently dead
                                // path (NAT expiry, a hung daemon, a blackholed tailnet route) the
                                // launcher said "unreachable" and never tried again. Dropping is what
                                // the network-change probe already does for the same reason.
                                _link.value = LinkState.Unreachable
                                _connected.value = false
                                dropSocket()
                                break
                            }
                            runCatching {
                                // Bounded: see CLIENT_PING_SEND_TIMEOUT_MS.
                                withTimeout(CLIENT_PING_SEND_TIMEOUT_MS) {
                                    send(Frame.Text("""{"type":"ping"}"""))
                                }
                            }
                        }
                    }
                    // The network changed under a socket that still looks open: ask
                    // now and allow a short, fixed time for the answer.
                    val netWatch = launch {
                        NetworkWake.wakes.collect {
                            val before = pongCount
                            runCatching { send(Frame.Text("""{"type":"ping"}""")) }
                            delay(NETWORK_PROBE_MS)
                            if (pongCount == before) {
                                appLog("launcher link silent after a network change; reporting unreachable")
                                _link.value = LinkState.Unreachable
                                _connected.value = false
                                dropSocket()
                            }
                        }
                    }
                    try {
                        for (frame in incoming) {
                            // A message that cannot be handled is that message's
                            // problem: it must not read as a dead link.
                            if (frame is Frame.Text) {
                                try {
                                    onMessage(frame.readText())
                                } catch (e: CancellationException) {
                                    throw e
                                } catch (e: Exception) {
                                    appLog("message handler failed: ${e::class.simpleName}")
                                }
                            }
                        }
                    } finally {
                        beat.cancel()
                        netWatch.cancel()
                        if (ws === this) ws = null
                    }
                }
                // The socket closed after having been up: the machine went away or
                // restarted. Unreachable until the next attempt says otherwise.
                if (current()) _link.value = LinkState.Unreachable
            } catch (e: CancellationException) {
                // Reconfigured or stopped: the caller asked for this, and the new
                // loop owns every piece of state from here.
                throw e
            } catch (e: Throwable) {
                // Why, not just that: a refused token and a machine that is off are
                // different problems with different fixes, and the handshake's own
                // exception cannot tell them apart. The daemon can (see
                // [probeLinkStatus]).
                appLog("connect failed: ${e::class.simpleName}")
                val state = linkStateFromProbe(probeLinkStatus(http, endpoint))
                if (current()) _link.value = state
            }
            if (!current()) return
            _connected.value = false
            if (_link.value == LinkState.Rejected) {
                // Waiting will not fix a refused token. Only a refresh (or a new
                // pairing, which replaces this loop) tries again.
                appLog("token refused; waiting for a refresh or a new pairing")
                wake.first()
                attempt = 0
                continue
            }
            // Either the timeout elapses, or the network coming back cuts it
            // short. `withTimeoutOrNull` returning normally means the wake fired.
            val woken = withTimeoutOrNull(withJitter(reconnectDelayMs(attempt))) { wake.first() } != null
            attempt = if (woken) 0 else attempt + 1
        }
    }

    /**
     * One text frame from the daemon. Internal so it is tested against hostile and
     * future payloads without a socket.
     *
     * Every read is tolerant (see `WireJson.kt`): the throwing accessors this used to
     * call raise on a `null` or an object where a string was expected, and a raise here
     * was the receive loop's problem — a dropped socket for one odd field. Unknown
     * message types and unknown fields are ignored, so a newer daemon (a `session_ended`
     * message, a capability field) costs an older client nothing.
     */
    internal fun onMessage(text: String) {
        val root = parseJsonObject(text) ?: return
        when (root.text("type")) {
            "pong" -> {
                missed.onPong()
                pongCount++
            }
            "error" -> {
                _serverError.value = ServerError(
                    code = root.text("code").ifEmpty { "error" },
                    message = root.text("message").ifEmpty { "the daemon refused that" },
                )
                _errorTick.value += 1
            }
            "created" -> {
                val id = root.optionalText("sessionId")
                if (!id.isNullOrEmpty()) _created.value = _created.value + id
                _createdTick.value += 1
            }
            "desktop" -> {
                _desktop.value = parseDesktop(root["inventory"] as? JsonObject)
                _desktopTick.value += 1
            }
            "live" -> _liveResult.value = LiveResult(
                sessionId = root.text("sessionId"),
                ok = root.flag("ok"),
                detail = root.optionalText("detail"),
            )
            // `owned` on this socket is ignored on purpose: this client never attaches a
            // stream, so a takeover cannot reach it. The ownership notice a page must show
            // comes from the attach path (`AttachClient.onOwned` → `HeldElsewhere`).
            "sessions" -> {
                _sessions.value = parseSessions(root["sessions"], deviceId)
                appLog("sessions parsed=${_sessions.value.size} attachable=${_sessions.value.count { it.attachable }}")
            }
        }
    }
}

/**
 * The session list, read with this file's tolerant accessors.
 *
 * This parsed with `jsonPrimitive` directly while every other parser here used
 * `text` / `number` / `flag`, and `jsonPrimitive` **throws** on a value that is
 * `JsonNull` or an object. The safe-call only guards an *absent* key, which is
 * why this looked defensive and was not.
 *
 * Where the throw landed is what made it expensive: inside `mapNotNull`, inside the
 * receive loop, inside `catch (_: Throwable)`. One unreadable field was therefore
 * not one dropped row — it was every row, a silently torn-down socket, and a
 * launcher reading "Nothing open yet" with nothing anywhere to say why. The comment
 * on the helpers below already said this. This is that comment, applied.
 *
 * Extracted from the message handler so the parse can be tested against the
 * daemon's real payload instead of only against what the daemon was believed to
 * send.
 */
internal fun parseSessions(element: JsonElement?, deviceId: String): List<Session> {
    val arr = element as? JsonArray ?: return emptyList()
    return arr.mapNotNull { entry ->
        val o = entry as? JsonObject ?: return@mapNotNull null
        val id = o.text("id")
        if (id.isEmpty()) return@mapNotNull null
        val owner = o["owner"] as? JsonObject
        val ownerId = owner?.optionalText("id")
        Session(
            id = id,
            kind = o.text("kind").ifEmpty { "shell" },
            title = o.text("title"),
            cwd = o.text("cwd"),
            status = o.text("status"),
            keeperName = o.text("keeperName"),
            createdBy = o.text("createdBy"),
            process = o.text("process"),
            liveWorkspace = o.optionalText("liveWorkspace"),
            hadWindow = o.flag("hadWindow"),
            ownerName = owner?.optionalText("name"),
            ownerKind = owner?.optionalText("kind"),
            heldElsewhere = ownerId != null && ownerId != deviceId,
        )
    }
}

/**
 * Read a desktop inventory out of the wire JSON.
 *
 * Tolerant by design: a field this client has not learned yet reads as empty,
 * and a daemon that cannot enumerate windows sends an empty list with a `detail`
 * rather than an error. The launcher must always have something to draw.
 */
internal fun parseDesktop(o: JsonObject?): DesktopInventory? {
    if (o == null) return null
    return DesktopInventory(
        wm = o.text("wm"),
        workspaces = o.objects("workspaces").map {
            DesktopWorkspace(
                name = it.text("name"),
                index = it.number("index"),
                focused = it.flag("focused"),
                visible = it.flag("visible"),
                windowCount = it.number("windowCount") ?: 0,
            )
        },
        windows = o.objects("windows").map {
            DesktopWindow(
                id = it.text("id"),
                appClass = it.text("appClass"),
                title = it.text("title"),
                workspace = it.text("workspace"),
            )
        },
        detail = o.optionalText("detail"),
    )
}
