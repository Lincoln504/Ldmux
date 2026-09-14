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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
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
    val url: String get() = if (token.isEmpty()) baseUrl else "$baseUrl/?token=$token"

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
 * Minimal app chrome: a slim top bar and the terminal. Everything else the
 * terminal itself already provides; native chrome stays out of the way.
 */
@Composable
fun App() {
    var config by remember { mutableStateOf(AppPrefs.load()) }
    var showSettings by remember { mutableStateOf(false) }

    Column(Modifier.fillMaxSize().background(Bg)) {
        TopBar(
            title = config.host,
            onSettings = { showSettings = true },
        )
        TerminalSurface(config.url, Modifier.fillMaxWidth().weight(1f))
    }

    if (showSettings) {
        SettingsDialog(
            current = config,
            onDismiss = { showSettings = false },
            onSave = {
                config = it
                AppPrefs.save(it)
                showSettings = false
            },
        )
    }
}

@Composable
private fun TopBar(title: String, onSettings: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().height(44.dp).background(Surface).padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text("ldmux", color = Fg, fontFamily = FontFamily.Monospace, fontSize = 15.sp)
        Text(title, color = Dim, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
        TextButton(onClick = onSettings) { Text("settings", color = Accent, fontSize = 12.sp) }
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
