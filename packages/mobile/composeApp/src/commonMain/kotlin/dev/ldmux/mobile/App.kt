package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.PagerState
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** The whole palette. Minimal, dark, one accent — no decoration. */
val Bg = Color(0xFF0B0E14)
val Fg = Color(0xFFE6E6E6)
val Dim = Color(0xFF8A8F98)
val Accent = Color(0xFF4C8DFF)
val Surface = Color(0xFF141822)

/** Where the daemon is and how to authenticate. Persisted by the platform. */
data class LdmuxConfig(val host: String, val token: String, val side: String = "left") {
    val baseUrl: String get() = if (host.startsWith("http")) host else "http://$host"

    val wsUrl: String get() {
        val ws = baseUrl.replace("https://", "wss://").replace("http://", "ws://")
        return if (token.isEmpty()) "$ws/ws" else "$ws/ws?token=$token"
    }

    val panelLeft: Boolean get() = side != "right"

    /**
     * The web-client URL for one session. `chrome=0` tells the client the native
     * shell owns the chrome, so it renders only the terminal.
     */
    fun urlFor(sessionId: String?): String {
        val params = buildList {
            if (token.isNotEmpty()) add("token=$token")
            if (sessionId != null) add("session=$sessionId")
            add("chrome=0")
        }
        return "$baseUrl/?${params.joinToString("&")}"
    }

    companion object {
        val DEFAULT = LdmuxConfig(host = "localhost:7623", token = "", side = "left")
    }
}

/** Platform persistence for [LdmuxConfig]. */
expect object AppPrefs {
    fun load(): LdmuxConfig
    fun save(config: LdmuxConfig)
}

/**
 * Landscape app: a terminal filling the screen with a slim **side** panel — no
 * top bar. The panel carries everything: prev/next between open shells, the
 * extra keys, new shell, and settings. Side (left/right) is a setting.
 */
@Composable
fun App() {
    var config by remember { mutableStateOf(AppPrefs.load()) }
    var showSettings by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val client = remember { LdmuxClient(config) }

    LaunchedEffect(Unit) { client.start(scope) }
    val sessions by client.sessions.collectAsState()
    val pages = sessions.filter { it.attachable }

    val pagerState = rememberPagerState(pageCount = { pages.size.coerceAtLeast(1) })
    LaunchedEffect(pages.size) {
        if (pages.isNotEmpty() && pagerState.currentPage >= pages.size) {
            pagerState.scrollToPage(pages.size - 1)
        }
    }

    // One handle per session, so the sidebar's keys reach the visible terminal.
    // A snapshot state map, so the panel recomposes once a surface registers.
    val handles = remember { mutableStateMapOf<String, TerminalHandle>() }

    Row(Modifier.fillMaxSize().background(Bg).safeDrawingPadding()) {
        if (config.panelLeft) SidePanel(pages, pagerState, scope, client, handles) { showSettings = true }
        Box(Modifier.weight(1f).fillMaxHeight().background(Bg)) {
            if (pages.isEmpty()) {
                Text(
                    "No sessions. Open a terminal with a tmux session, or tap +sh.",
                    color = Dim,
                    fontSize = 13.sp,
                    modifier = Modifier.align(Alignment.Center).padding(16.dp),
                )
            } else {
                HorizontalPager(state = pagerState, beyondViewportPageCount = 2, modifier = Modifier.fillMaxSize()) { page ->
                    val s = pages[page]
                    val handle = rememberTerminalSurface(config, s.id, Modifier.fillMaxSize())
                    LaunchedEffect(handle, s.id) { handles[s.id] = handle }
                }
            }
        }
        if (!config.panelLeft) SidePanel(pages, pagerState, scope, client, handles) { showSettings = true }
    }

    if (showSettings) {
        SettingsDialog(
            current = config,
            onDismiss = { showSettings = false },
            onSave = {
                config = it
                AppPrefs.save(it)
                client.reconfigure(it)
                showSettings = false
            },
        )
    }
}

/**
 * The side panel. No session list: just back/forward between the open shells,
 * the title of the current one, the keys, and the few actions.
 */
@Composable
private fun SidePanel(
    pages: List<Session>,
    pagerState: PagerState,
    scope: CoroutineScope,
    client: LdmuxClient,
    handles: Map<String, TerminalHandle>,
    onSettings: () -> Unit,
) {
    val current = pagerState.currentPage
    val currentId = pages.getOrNull(current)?.id
    val handle = currentId?.let { handles[it] }
    val title = pages.getOrNull(current)?.label ?: "—"

    fun step(delta: Int) {
        val target = (current + delta).coerceIn(0, (pages.size - 1).coerceAtLeast(0))
        if (target != current) scope.launch { pagerState.animateScrollToPage(target) }
    }

    Column(
        Modifier.width(190.dp).fillMaxHeight().background(Surface).padding(10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // Back and forth between the open shells.
        Row(verticalAlignment = Alignment.CenterVertically) {
            NavArrow("◀", enabled = current > 0) { step(-1) }
            Text(
                title,
                color = Fg,
                fontFamily = FontFamily.Monospace,
                fontSize = 12.sp,
                maxLines = 1,
                modifier = Modifier.weight(1f).padding(horizontal = 6.dp),
            )
            NavArrow("▶", enabled = current < pages.size - 1) { step(1) }
        }

        Column(Modifier.weight(1f).verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            SIDEBAR_KEYS.forEach { row ->
                Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    row.forEach { (label, key) -> KeyBtn(label) { handle?.sendKey(key) } }
                }
            }
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                KeyBtn("⤒") { handle?.loadHistory() }
                KeyBtn("↓") { handle?.jumpToBottom() }
            }
        }

        TextButton(onClick = { client.create("shell") }) { Text("+sh", color = Accent, fontSize = 13.sp) }
        TextButton(onClick = onSettings) { Text("settings", color = Accent, fontSize = 13.sp) }
    }
}

@Composable
private fun NavArrow(glyph: String, enabled: Boolean, onClick: () -> Unit) {
    Text(
        glyph,
        color = if (enabled) Accent else Dim.copy(alpha = 0.4f),
        fontSize = 18.sp,
        modifier = Modifier
            .clip(RoundedCornerShape(6.dp))
            .clickable(enabled = enabled, onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 2.dp),
    )
}

@Composable
private fun KeyBtn(label: String, onClick: () -> Unit) {
    Box(
        Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(Bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Text(label, color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
    }
}

@Composable
private fun SettingsDialog(current: LdmuxConfig, onDismiss: () -> Unit, onSave: (LdmuxConfig) -> Unit) {
    var host by remember { mutableStateOf(current.host) }
    var token by remember { mutableStateOf(current.token) }
    var side by remember { mutableStateOf(current.side) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Connection", color = Fg) },
        containerColor = Surface,
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("panel", color = Dim, fontSize = 12.sp)
                    TextButton(onClick = { side = "left" }) { Text("left", color = if (side == "left") Accent else Dim) }
                    TextButton(onClick = { side = "right" }) { Text("right", color = if (side == "right") Accent else Dim) }
                }
                Box(Modifier.height(4.dp))
                OutlinedTextField(
                    value = host,
                    onValueChange = { host = it },
                    label = { Text("host[:port]") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Box(Modifier.height(6.dp))
                OutlinedTextField(
                    value = token,
                    onValueChange = { token = it },
                    label = { Text("token") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onSave(LdmuxConfig(host.trim(), token.trim(), side)) }) {
                Text("save", color = Accent)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}
