package dev.ldmux.mobile

import io.ktor.client.HttpClient
import io.ktor.client.plugins.websocket.DefaultClientWebSocketSession
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.websocket.Frame
import io.ktor.websocket.readText
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
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put

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
)

/** The outcome of a make-live or close-live request. */
data class LiveResult(val sessionId: String, val ok: Boolean, val detail: String?)

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
    private val http = HttpClient { install(WebSockets) }
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

    /**
     * Ask the host what is on its desktop.
     *
     * On demand, not polled: answering spawns the window manager's own tools, and
     * nothing about the answer is worth waking the radio for on a timer.
     */
    fun requestDesktop() = sendControl("""{"type":"desktop"}""")

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

    /** Open a new session on the host (FR-10). The list refreshes from the reply. */
    fun create(kind: String, cwd: String? = null, title: String? = null) {
        val msg = buildJsonObject {
            put("type", "new")
            put("kind", kind)
            if (cwd != null) put("cwd", cwd)
            if (title != null) put("title", title)
        }.toString()
        sendControl(msg)
    }

    /** End a session. */
    fun kill(sessionId: String) {
        val msg = buildJsonObject {
            put("type", "kill")
            put("sessionId", sessionId)
        }.toString()
        sendControl(msg)
    }

    private fun sendControl(msg: String) {
        scope?.launch {
            runCatching { ws?.send(Frame.Text(msg)) }
        }
    }

    fun reconfigure(config: LdmuxConfig) {
        this.config = config
        job?.cancel()
        job = scope?.launch { loop() }
    }

    fun start(scope: CoroutineScope) {
        this.scope = scope
        if (job != null) return
        job = scope.launch { loop() }
    }

    fun stop() {
        job?.cancel()
        job = null
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
        // Backed off rather than fixed, and capped: a daemon that is down for an
        // hour should not be asked twelve hundred times, and one that comes back
        // ten seconds later should not make the user wait two seconds to notice.
        var waitMs = 400L
        while (currentCoroutineContext().isActive) {
            try {
                http.webSocket(config.wsUrl) {
                    ws = this
                    waitMs = 400L
                    _connected.value = true
                    send(Frame.Text("""{"type":"list"}"""))
                    try {
                        for (frame in incoming) {
                            if (frame is Frame.Text) onMessage(frame.readText())
                        }
                    } finally {
                        ws = null
                    }
                }
            } catch (_: Throwable) {
                /* offline / daemon restart: fall through to the retry delay */
            }
            _connected.value = false
            // Either the timeout elapses, or the network coming back cuts it
            // short. `withTimeoutOrNull` returning normally means the wake fired.
            val woken = withTimeoutOrNull(waitMs) { wake.first() } != null
            if (!woken) waitMs = (waitMs * 2).coerceAtMost(10_000L)
            else waitMs = 400L
        }
    }

    private fun onMessage(text: String) {
        val root = runCatching { Json.parseToJsonElement(text).jsonObject }.getOrNull() ?: return
        when (root["type"]?.jsonPrimitive?.content) {
            "desktop" -> {
                _desktop.value = parseDesktop(root["inventory"]?.jsonObject)
                _desktopTick.value += 1
            }
            "live" -> _liveResult.value = LiveResult(
                sessionId = root["sessionId"]?.jsonPrimitive?.content ?: "",
                ok = root["ok"]?.jsonPrimitive?.content == "true",
                detail = root["detail"]?.jsonPrimitive?.content,
            )
            "sessions" -> {
                val arr = root["sessions"] as? JsonArray ?: return
                _sessions.value = arr.mapNotNull { element ->
                    val o = element.jsonObject
                    val id = o["id"]?.jsonPrimitive?.content ?: return@mapNotNull null
                    Session(
                        id = id,
                        kind = o["kind"]?.jsonPrimitive?.content ?: "shell",
                        title = o["title"]?.jsonPrimitive?.content ?: "",
                        cwd = o["cwd"]?.jsonPrimitive?.content ?: "",
                        status = o["status"]?.jsonPrimitive?.content ?: "",
                        tmuxName = o["tmuxName"]?.jsonPrimitive?.content ?: "",
                        createdBy = o["createdBy"]?.jsonPrimitive?.content ?: "",
                    )
                }
            }
        }
    }
}

// Reading JSON without `jsonPrimitive`, which throws on a null or an object.
// Every accessor here answers "nothing" instead, so one unexpected field cannot
// take down the launcher.

private fun JsonObject.text(key: String): String =
    ((this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull })?.content ?: ""

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
            )
        },
        detail = o.optionalText("detail"),
    )
}
