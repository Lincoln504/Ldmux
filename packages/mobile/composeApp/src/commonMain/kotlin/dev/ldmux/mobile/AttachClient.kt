package dev.ldmux.mobile

import io.ktor.client.HttpClient
import io.ktor.client.plugins.websocket.DefaultClientWebSocketSession
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.websocket.Frame
import io.ktor.websocket.readText
import kotlin.time.TimeMark
import kotlin.time.TimeSource
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
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
     * The pane is in a tmux mode — copy-mode, or a menu.
     *
     * Reported by the server rather than tracked locally from what we asked for,
     * because the pane leaves copy-mode on its own too: `q`, or whatever the
     * program binds. What the button shows has to be the terminal's state, not
     * the client's intention.
     */
    val paneInMode: Boolean = false,
    /**
     * Where the pane's cursor actually is, from tmux.
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
 * Attaches to one ldmux session and streams its pane output (protocol v1).
 *
 * Control is JSON text frames; terminal output arrives as binary frames with an
 * 8-byte big-endian header (type=0x10 Output, then a u32 length) followed by the
 * payload bytes. See packages/protocol/src/frames.ts and remote.ts.
 */
class AttachClient(private val config: LdmuxConfig, private val sessionId: String) {
    /**
     * Recreated on every start, because [stop] closes it: the pager keeps a
     * neighbouring page composed but should not keep it *connected* (see
     * `TerminalController.setActive`).
     */
    private var http: HttpClient? = null

    private fun httpClient(): HttpClient =
        http ?: HttpClient { install(WebSockets) }.also { http = it }

    private val json = Json { ignoreUnknownKeys = true }
    private var ws: DefaultClientWebSocketSession? = null
    private var job: Job? = null
    private var scope: CoroutineScope? = null
    /** The grid the surface will paint. Sent with attach, and on every change. */
    private var wantCols = 0
    private var wantRows = 0

    var onSnapshot: ((cols: Int, rows: Int, screen: String, history: Boolean, modes: TerminalModes) -> Unit)? = null
    var onOutput: ((ByteArray) -> Unit)? = null
    var onError: ((String) -> Unit)? = null
    /**
     * A measured round trip, in ms. The display's repaint cadence follows this
     * (see FramePacing): a phone should not paint faster than the link can
     * answer, and it should slow down rather than queue up when the link is bad.
     */
    var onRtt: ((Long) -> Unit)? = null
    /** The connection went away; the current round-trip estimate is worthless. */
    var onOffline: (() -> Unit)? = null

    private var pingSeq = 0
    private var pingSentAt: TimeMark? = null

    fun start(scope: CoroutineScope) {
        if (job != null) return
        this.scope = scope
        job = scope.launch { loop() }
    }

    /** Detach and stop streaming. The connection is not kept in any form. */
    fun stop() {
        job?.cancel()
        job = null
        scope = null
        http?.let { runCatching { it.close() } }
        http = null
    }

    private suspend fun loop() {
        while (currentCoroutineContext().isActive) {
            try {
                httpClient().webSocket(config.wsUrl) {
                    ws = this
                    send(Frame.Text(
                        buildJsonObject {
                            put("type", "attach")
                            put("sessionId", sessionId)
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
                            delay(PING_INTERVAL_MS)
                        }
                    }
                    try {
                        for (frame in incoming) {
                            when (frame) {
                                is Frame.Text -> onControl(frame.readText())
                                is Frame.Binary -> onBinary(frame.data)
                                else -> Unit
                            }
                        }
                    } finally {
                        pinger.cancel()
                        ws = null
                    }
                }
            } catch (e: CancellationException) {
                // Deactivation, not a broken link: the caller asked for this.
                throw e
            } catch (_: Throwable) {
                /* offline / daemon restart: retry below */
            }
            pingSentAt = null
            onOffline?.invoke()
            delay(1500)
        }
    }

    private fun onControl(text: String) {
        val root = runCatching { json.parseToJsonElement(text).jsonObject }.getOrNull() ?: return
        when (root["type"]?.jsonPrimitive?.content) {
            "caps" -> Unit
            "pong" -> {
                val sent = pingSentAt ?: return
                pingSentAt = null
                onRtt?.invoke(sent.elapsedNow().inWholeMilliseconds)
            }
            "snapshot" -> {
                val cols = root["cols"]?.jsonPrimitive?.content?.toIntOrNull() ?: return
                val rows = root["rows"]?.jsonPrimitive?.content?.toIntOrNull() ?: return
                val screen = root["screen"]?.jsonPrimitive?.content ?: ""
                val history = root["history"]?.jsonPrimitive?.content?.toBoolean() ?: false
                val modes = TerminalModes(
                    mouse = bool(root, "mouse"),
                    appCursor = bool(root, "appCursor"),
                    bracketedPaste = bool(root, "bracketedPaste"),
                    alternateOn = bool(root, "alternateOn"),
                    mouseSgr = bool(root, "mouseSgr"),
                    paneInMode = bool(root, "paneInMode"),
                    cursorX = root["cursorX"]?.jsonPrimitive?.content?.toIntOrNull() ?: 0,
                    cursorY = root["cursorY"]?.jsonPrimitive?.content?.toIntOrNull() ?: 0,
                )
                onSnapshot?.invoke(cols, rows, screen, history, modes)
            }
            "error" -> onError?.invoke(root["code"]?.jsonPrimitive?.content ?: "error")
        }
    }

    private fun onBinary(data: ByteArray) {
        if (data.size < 8) return
        val type = data[0].toInt() and 0xff
        val length = ((data[4].toInt() and 0xff) shl 24) or
            ((data[5].toInt() and 0xff) shl 16) or
            ((data[6].toInt() and 0xff) shl 8) or
            (data[7].toInt() and 0xff)
        if (type != 0x10) return // FrameType.Output
        val end = (8 + length).coerceAtMost(data.size)
        if (end <= 8) return
        onOutput?.invoke(data.copyOfRange(8, end))
    }

    private fun bool(root: kotlinx.serialization.json.JsonObject, key: String): Boolean =
        root[key]?.jsonPrimitive?.content?.toBoolean() ?: false

    private fun sendControl(msg: String) {
        scope?.launch { runCatching { ws?.send(Frame.Text(msg)) } }
    }

    fun input(text: String) {
        sendControl(buildJsonObject { put("type", "input"); put("data", text) }.toString())
    }

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

    fun keys(keys: List<String>) {
        val array = keys.joinToString(",") { quote(it) }
        sendControl("""{"type":"key","keys":[$array]}""")
    }

    /**
     * Enter or leave tmux copy-mode.
     *
     * The scrollback this app keeps is for the *normal* screen. A full-screen
     * program draws on the alternate screen, which has no history to keep — there
     * is nothing to scroll back through — so for vim, htop and less the only
     * scrollable copy is tmux's own view of the pane, and this is how you reach
     * it. Once in copy-mode the arrow keys scroll, because they are tmux's keys
     * now, not the program's.
     */
    fun copyMode(on: Boolean) {
        sendControl(buildJsonObject { put("type", "copymode"); put("on", on) }.toString())
    }

    /**
     * A tmux prefix-key action.
     *
     * The prefix itself cannot be sent: a keystroke goes *into the pane*, where
     * the running program reads it, and the prefix is only a prefix to a tmux
     * client. The daemon is the client, so the action is named.
     */
    fun tmuxAction(action: String, name: String? = null) {
        sendControl(
            buildJsonObject {
                put("type", "tmux")
                put("action", action)
                if (name != null) put("name", name)
            }.toString(),
        )
    }

    /**
     * Deliver a tap to a mouse-aware program. Only valid when the pane reported
     * SGR mouse reporting; the server synthesizes the click so the byte sequence
     * never has to live in the client.
     */
    fun mouseClick(col: Int, row: Int) {
        sendControl(
            buildJsonObject {
                put("type", "mouseclick")
                put("col", col)
                put("row", row)
            }.toString(),
        )
    }

    fun scrollback(lines: Int) {
        sendControl(
            buildJsonObject {
                put("type", "scrollback")
                put("sessionId", sessionId)
                put("lines", lines)
            }.toString(),
        )
    }

    /**
     * Repaint the visible screen from the server. `lines = 0` is the
     * return-to-bottom form (no history); it is also the recovery step after a
     * failed input prediction, because a snapshot is the only way to un-apply
     * bytes already fed to the emulator.
     */
    fun snapshotNow() = scrollback(0)

    private fun quote(s: String): String = kotlinx.serialization.json.JsonPrimitive(s).toString()

    private companion object {
        /** Long enough that the handshake is not what gets measured. */
        const val FIRST_PING_MS = 600L
        /**
         * Every few seconds is plenty: the estimate only has to follow a real
         * change in link quality, and a phone on a metered connection should not
         * spend its budget on keep-alives. Each ping is a handful of bytes.
         */
        const val PING_INTERVAL_MS = 4_000L
    }
}