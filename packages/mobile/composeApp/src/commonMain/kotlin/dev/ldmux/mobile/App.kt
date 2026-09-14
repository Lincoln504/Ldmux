package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
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

/** The whole palette. Minimal, dark, one accent — no decoration. */
val Bg = Color(0xFF0B0E14)
val Fg = Color(0xFFE6E6E6)
val Dim = Color(0xFF8A8F98)
val Accent = Color(0xFF4C8DFF)
val Surface = Color(0xFF141822)

/** Where the daemon is and how to authenticate. Persisted by the platform. */
data class LdmuxConfig(val host: String, val token: String) {
    val baseUrl: String get() = if (host.startsWith("http")) host else "http://$host"

    val wsUrl: String get() {
        val ws = baseUrl.replace("https://", "wss://").replace("http://", "ws://")
        return if (token.isEmpty()) "$ws/ws" else "$ws/ws?token=$token"
    }

    /** The web-client URL for one session (each pager page is one shell). */
    fun urlFor(sessionId: String?): String {
        val params = buildList {
            if (token.isNotEmpty()) add("token=$token")
            if (sessionId != null) add("session=$sessionId")
        }
        return if (params.isEmpty()) "$baseUrl/" else "$baseUrl/?${params.joinToString("&")}"
    }

    companion object {
        val DEFAULT = LdmuxConfig(host = "localhost:7623", token = "")
    }
}

/** Platform persistence for [LdmuxConfig]. */
expect object AppPrefs {
    fun load(): LdmuxConfig
    fun save(config: LdmuxConfig)
}

/** The terminal surface: a WebView hosting the shared web client. */
@Composable
expect fun TerminalSurface(url: String, modifier: Modifier)

/**
 * The app: one full-height terminal, one page per session, swipe left/right to
 * switch (FR-8). A 36dp bar carries the current session and settings; everything
 * else lives in the terminal itself.
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
    // Sessions come and go; never leave the pager on a page that no longer exists.
    LaunchedEffect(pages.size) {
        if (pages.isNotEmpty() && pagerState.currentPage >= pages.size) {
            pagerState.scrollToPage(pages.size - 1)
        }
    }

    Column(Modifier.fillMaxSize().background(Bg)) {
        TopBar(
            title = pages.getOrNull(pagerState.currentPage)?.let { "${it.kind} · ${it.label}" } ?: "no sessions",
            count = pages.size,
            current = pagerState.currentPage,
            onNewShell = { client.create("shell") },
            onNewPi = { client.create("pi") },
            onSettings = { showSettings = true },
            onRefresh = { client.start(scope) },
        )
        if (pages.isEmpty()) {
            Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
                Text("No sessions. Open one from the host, or pull to refresh.", color = Dim, fontSize = 13.sp)
            }
        } else {
            HorizontalPager(
                state = pagerState,
                beyondViewportPageCount = 2,
                modifier = Modifier.fillMaxWidth().weight(1f),
            ) { page ->
                TerminalSurface(config.urlFor(pages[page].id), Modifier.fillMaxSize())
            }
        }
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

@Composable
private fun TopBar(
    title: String,
    count: Int,
    current: Int,
    onNewShell: () -> Unit,
    onNewPi: () -> Unit,
    onSettings: () -> Unit,
    onRefresh: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().height(36.dp).background(Surface).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text("ldmux", color = Fg, fontFamily = FontFamily.Monospace, fontSize = 14.sp)
        if (count > 1) Dots(count, current)
        Text(title, color = Dim, fontFamily = FontFamily.Monospace, fontSize = 12.sp, modifier = Modifier.weight(1f))
        TextButton(onClick = onNewShell) { Text("+shell", color = Accent, fontSize = 12.sp) }
        TextButton(onClick = onNewPi) { Text("+pi", color = Accent, fontSize = 12.sp) }
        TextButton(onClick = onRefresh) { Text("↻", color = Dim, fontSize = 14.sp) }
        TextButton(onClick = onSettings) { Text("settings", color = Accent, fontSize = 12.sp) }
    }
}

@Composable
private fun Dots(count: Int, current: Int) {
    Row(horizontalArrangement = Arrangement.spacedBy(4.dp), verticalAlignment = Alignment.CenterVertically) {
        repeat(count) { i ->
            Box(
                Modifier.size(6.dp).clip(CircleShape).background(if (i == current) Accent else Dim),
            )
        }
    }
}

@Composable
private fun SettingsDialog(current: LdmuxConfig, onDismiss: () -> Unit, onSave: (LdmuxConfig) -> Unit) {
    var host by remember { mutableStateOf(current.host) }
    var token by remember { mutableStateOf(current.token) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Connection", color = Fg) },
        containerColor = Surface,
        text = {
            Column {
                OutlinedTextField(
                    value = host,
                    onValueChange = { host = it },
                    label = { Text("host[:port]") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Box(Modifier.height(10.dp))
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
            TextButton(onClick = { onSave(LdmuxConfig(host.trim(), token.trim())) }) {
                Text("save", color = Accent)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}
