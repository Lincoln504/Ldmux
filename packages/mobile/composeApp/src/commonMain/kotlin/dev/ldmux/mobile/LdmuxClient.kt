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
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
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
) {
    val attachable: Boolean get() = tmuxName.isNotEmpty()
    val label: String get() = if (title.isNotEmpty()) title else id
}

/**
 * A minimal daemon client for the native shell.
 *
 * The terminal itself is the shared web client inside a WebView, which speaks
 * the full protocol. This client only needs the session *list* (to build the
 * swipe pager) and the ability to open new sessions, so it subscribes to the
 * same `/ws` endpoint and sends `list` / `new`.
 */
class LdmuxClient(private var config: LdmuxConfig) {
    private val http = HttpClient { install(WebSockets) }
    private val _sessions = MutableStateFlow<List<Session>>(emptyList())
    val sessions: StateFlow<List<Session>> = _sessions

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
        http.close()
    }

    private suspend fun loop() {
        while (currentCoroutineContext().isActive) {
            try {
                http.webSocket(config.wsUrl) {
                    ws = this
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
            delay(2000)
        }
    }

    private fun onMessage(text: String) {
        val root = runCatching { Json.parseToJsonElement(text).jsonObject }.getOrNull() ?: return
        when (root["type"]?.jsonPrimitive?.content) {
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
                    )
                }
            }
        }
    }
}
