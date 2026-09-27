package dev.ldmux.mobile

import io.ktor.client.HttpClient
import io.ktor.client.request.get
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
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.add
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/** One ldmux session, as the daemon reports it in a `sessions` message. */
data class Session(
    val id: String,
    val kind: String,
    val title: String,
    val cwd: String,
    val status: String,
    /** Empty for an RPC-only pi session (nothing to attach to in tmux). */
    val tmuxName: String,
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
     * Not merely "has a tmux name": a session that has **died** keeps its name in
     * the registry until the record is pruned, so the pager used to spend pages
     * on corpses — and since the page you were looking at could be one of them,
     * taps and keys went nowhere with no explanation. Only a running session is
     * a page; `starting` counts, because it is on its way.
     */
    val attachable: Boolean get() = tmuxName.isNotEmpty() && status != "dead"
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
    val pid: Int?,
)

/**
 * A pi agent session running on the host.
 *
 * [label] is the directory's basename and is what the launcher shows, because
 * "which project is that agent on" is the question a person is actually asking.
 *
 * [sessionId] is resolved from the transcript and is a best effort — pi does not
 * report which session a process is writing to (see the daemon's
 * `assignTranscripts`). [pid] and [cwd] are facts.
 */
data class DesktopPiSession(
    val pid: Int,
    val cwd: String,
    val sessionId: String?,
    val startedAt: String?,
    val windowId: String?,
    val workspace: String?,
    val label: String,
    /** The ldmux shell this pi runs inside, when it runs inside one. */
    val ldmuxSessionId: String? = null,
)

/** The outcome of a make-live or close-live request. */
data class LiveResult(val sessionId: String, val ok: Boolean, val detail: String?)

/**
 * A shell is held by another device (ADR-0010).
 *
 * [holder] is null when nobody holds it — the holder left while we were asking,
 * which is not a failure, just a different sentence.
 */
data class OwnedNotice(val sessionId: String, val holder: String?)

/** Everything the launcher shows before a terminal. */
data class DesktopInventory(
    val wm: String,
    val workspaces: List<DesktopWorkspace>,
    val windows: List<DesktopWindow>,
    val piSessions: List<DesktopPiSession>,
    /** Set when windows could not be listed; the pi sessions are still real. */
    val detail: String? = null,
) {
    /** Is there anything to show at all? */
    val isEmpty: Boolean
        get() = workspaces.isEmpty() && windows.isEmpty() && piSessions.isEmpty()
}

/**
 * A minimal daemon client for the native shell: the session *list* (for the
 * pager) and the ability to open and end sessions. The terminal itself is
 * attached over the same `/ws` endpoint by [AttachClient]; this keeps its own
 * connection so a terminal stream failing never blinds the session list.
 */
class LdmuxClient(private var config: LdmuxConfig) {
    private val http = HttpClient { ldmuxWebSockets() }

    /** The id this device announces itself as. Not an account, not a credential. */
    private val deviceId: String get() = config.deviceId

    /**
     * Say who we are, before anything else.
     *
     * A client that never does holds nothing, so exclusivity silently stops
     * applying to it: two devices could type into one shell and neither the
     * server nor the user would know.
     */
    private fun helloMessage(): String = buildJsonObject {
        put("type", "hello")
        putJsonObject("device") {
            put("id", config.deviceId)
            put("name", config.deviceLabel)
            put("kind", "phone")
        }
    }.toString()
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
    private val _link = MutableStateFlow(if (config.isPaired) LinkState.Connecting else LinkState.Unpaired)
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

    /**
     * A shell was refused because another device holds it (ADR-0010).
     *
     * Held as a value plus a counter rather than as a flow of events: the state
     * has to survive recomposition, and a dialog that appears only if it happens
     * to be collected at the right moment is a dialog that silently does not
     * appear.
     */
    private val _owned = MutableStateFlow<OwnedNotice?>(null)
    val owned: StateFlow<OwnedNotice?> = _owned

    private val _ownedTick = MutableStateFlow(0)
    val ownedTick: StateFlow<Int> = _ownedTick

    /** The user said take it over, so the notice has been dealt with. */
    fun clearOwned() {
        _owned.value = null
    }

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
     * Open a new session on the host (FR-10). The list refreshes from the reply.
     *
     * There is no `command` here on purpose: every shell this client can ask for is
     * the host's login shell (docs/SPEC.md §7), never a chosen program, so there is
     * nothing for a caller to pass. `kind = "pi"` is the one exception the daemon
     * still recognises, and it needs no argv either.
     */
    fun create(kind: String, cwd: String? = null, title: String? = null) {
        val msg = buildJsonObject {
            put("type", "new")
            put("kind", kind)
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
        }
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

    /** Set by the pong handler, read once per beat: the launcher's liveness. */
    private var pongSeen = false

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
        _link.value = if (config.isPaired) LinkState.Connecting else LinkState.Unpaired
        scope?.let { s -> if (config.isPaired) job = s.launch { loop() } }
    }

    fun start(scope: CoroutineScope) {
        this.scope = scope
        if (job != null || !config.isPaired) return
        job = scope.launch { loop() }
    }

    fun stop() {
        job?.cancel()
        job = null
        generation++
        scope = null
        _connected.value = false
        http.close()
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
        // Backed off rather than fixed, and capped: a daemon that is down for an
        // hour should not be asked twelve hundred times, and one that comes back
        // ten seconds later should not make the user wait two seconds to notice.
        var waitMs = 400L
        while (currentCoroutineContext().isActive) {
            try {
                appLog("connecting to ${endpoint.host}")
                http.webSocket(endpoint.wsUrl, request = { header(HttpHeaders.Authorization, endpoint.authorization) }) {
                    ws = this
                    waitMs = 400L
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
                    send(Frame.Text(helloMessage()))
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
                    pongSeen = true
                    val beat = launch {
                        var missed = 0
                        while (isActive) {
                            delay(CLIENT_PING_INTERVAL_MS)
                            missed = if (pongSeen) 0 else missed + 1
                            pongSeen = false
                            if (missed >= CLIENT_MISSED_PONGS) {
                                appLog("launcher link silent for $missed beats; reconnecting")
                                runCatching { this@webSocket.close() }
                                break
                            }
                            runCatching { send(Frame.Text("""{"type":"ping"}""")) }
                        }
                    }
                    try {
                        for (frame in incoming) {
                            if (frame is Frame.Text) onMessage(frame.readText())
                        }
                    } finally {
                        beat.cancel()
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
                // exception cannot tell them apart. The daemon can (see [probe]).
                appLog("connect failed: ${e::class.simpleName}")
                val state = linkStateFromProbe(probe(endpoint))
                if (current()) _link.value = state
            }
            if (!current()) return
            _connected.value = false
            if (_link.value == LinkState.Rejected) {
                // Waiting will not fix a refused token. Only a refresh (or a new
                // pairing, which replaces this loop) tries again.
                appLog("token refused; waiting for a refresh or a new pairing")
                wake.first()
                waitMs = 400L
                continue
            }
            // Either the timeout elapses, or the network coming back cuts it
            // short. `withTimeoutOrNull` returning normally means the wake fired.
            val woken = withTimeoutOrNull(waitMs) { wake.first() } != null
            if (!woken) waitMs = (waitMs * 2).coerceAtMost(10_000L)
            else waitMs = 400L
        }
    }

    /**
     * The daemon's HTTP status for a plain `GET /ws`, or null when nothing answered.
     * Bounded, because a machine that is not there does not say so quickly.
     */
    private suspend fun probe(endpoint: Endpoint): Int? = withTimeoutOrNull(PROBE_TIMEOUT_MS) {
        runCatching {
            http.get(endpoint.probeUrl) { header(HttpHeaders.Authorization, endpoint.authorization) }.status.value
        }.getOrNull()
    }

    private fun onMessage(text: String) {
        val root = runCatching { Json.parseToJsonElement(text).jsonObject }.getOrNull() ?: return
        when (root["type"]?.jsonPrimitive?.content) {
            "pong" -> pongSeen = true
            "error" -> {
                _serverError.value = ServerError(
                    code = root["code"]?.jsonPrimitive?.content ?: "error",
                    message = root["message"]?.jsonPrimitive?.content ?: "the daemon refused that",
                )
                _errorTick.value += 1
            }
            "created" -> {
                val id = root["sessionId"]?.jsonPrimitive?.content
                if (id != null) _created.value = _created.value + id
                _createdTick.value += 1
            }
            "desktop" -> {
                _desktop.value = parseDesktop(root["inventory"]?.jsonObject)
                _desktopTick.value += 1
            }
            "live" -> _liveResult.value = LiveResult(
                sessionId = root["sessionId"]?.jsonPrimitive?.content ?: "",
                ok = root["ok"]?.jsonPrimitive?.content == "true",
                detail = root["detail"]?.jsonPrimitive?.content,
            )
            "owned" -> {
                // A shell is held by another device (ADR-0010). Ordinary, not an
                // error: it is the answer to "open this", and the app answers it
                // by telling the user who has it and offering to take over.
                _owned.value = OwnedNotice(
                    sessionId = root["sessionId"]?.jsonPrimitive?.content ?: "",
                    holder = (root["owner"] as? JsonObject)
                        ?.get("name")?.let { (it as? JsonPrimitive)?.content },
                )
                _ownedTick.value += 1
            }
            "sessions" -> {
                _sessions.value = parseSessions(root["sessions"], deviceId)
                appLog("sessions parsed=${_sessions.value.size} attachable=${_sessions.value.count { it.attachable }}")
            }
        }
    }
}

// Reading JSON without `jsonPrimitive`, which throws on a null or an object.
// Every accessor here answers "nothing" instead, so one unexpected field cannot
// take down the launcher.

private fun JsonObject.text(key: String): String =
    ((this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull })?.content ?: ""

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
            tmuxName = o.text("tmuxName"),
            createdBy = o.text("createdBy"),
            process = o.text("process"),
            liveWorkspace = o.optionalText("liveWorkspace"),
            ownerName = owner?.optionalText("name"),
            ownerKind = owner?.optionalText("kind"),
            heldElsewhere = ownerId != null && ownerId != deviceId,
        )
    }
}

private fun JsonObject.optionalText(key: String): String? =
    ((this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull })?.content

private fun JsonObject.number(key: String): Int? = text(key).toIntOrNull()

private fun JsonObject.flag(key: String): Boolean = text(key) == "true"

private fun JsonObject.objects(key: String): List<JsonObject> =
    (this[key] as? JsonArray)?.mapNotNull { it as? JsonObject } ?: emptyList()

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
                pid = it.number("pid"),
            )
        },
        piSessions = o.objects("piSessions").map {
            DesktopPiSession(
                pid = it.number("pid") ?: 0,
                cwd = it.text("cwd"),
                sessionId = it.optionalText("sessionId"),
                startedAt = it.optionalText("startedAt"),
                windowId = it.optionalText("windowId"),
                workspace = it.optionalText("workspace"),
                label = it.text("label"),
                ldmuxSessionId = it.optionalText("ldmuxSessionId"),
            )
        },
        detail = o.optionalText("detail"),
    )
}
