package dev.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.AccentBtn
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.Bg
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.Warn
import kotlinx.coroutines.launch

/**
 * The reduced terminal that the mesh route shows (ADR-0025): plain-text rows from the host, a
 * single line of input, and the limits of the link stated where the user can see them. It is
 * **not** the tailnet terminal and does not pretend to be: there is no scrollback, colour,
 * mouse, selection, resize or clipboard here, and the host's own `absent` list is printed.
 *
 * Input is one line at a time (keystrokes at about a record a minute would be unusable), sent
 * on `send`, capped at [MeshTerm.MAX_INPUT] bytes. Nothing here knows a radio parameter.
 */
@Composable
fun MeshTerminalScreen(plan: MeshScreenPlan, onLeave: () -> Unit) {
    val scope = rememberCoroutineScope()
    val conn = remember(plan) { MeshTermConnection(meshSocketFactory(), scope, plan.creds, plan.lib, plan.endpoint) }
    LaunchedEffect(conn) { conn.run() }
    DisposableEffect(conn) { onDispose { conn.close() } }
    val view by conn.driver.view.collectAsState()

    // safeDrawingPadding first: the mesh screen is drawn instead of AppContent's own Box,
    // so without it the status line sits under the clock and the nav bar eats the last row.
    Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().padding(8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            // The label must wrap to its own row rather than squeeze the button: a Row with
            // SpaceBetween gave "leave" one character of width and it drew vertically.
            Text(
                meshTermLabel(view),
                color = if (view is MeshTermView.Live) Fg else Warn,
                fontSize = 13.sp,
                maxLines = 2,
                modifier = Modifier.weight(1f, fill = false).padding(end = 8.dp),
            )
            Text("leave", color = Accent, fontSize = 13.sp, maxLines = 1, modifier = Modifier.padding(4.dp).clickable(onClick = onLeave))
        }
        Text(MESH_RATE_NOTICE, color = Dim, fontSize = 11.sp)
        val live = view as? MeshTermView.Live
        if (live != null) {
            val absent = meshAbsentLine(live.absent)
            if (absent.isNotEmpty()) Text(absent, color = Dim, fontSize = 11.sp)
            Text(meshBudgetLine(live.budgetLeft), color = Dim, fontSize = 11.sp)
            if (live.screen.alternate) Text("A full-screen program is running: updates are large and slow here.", color = Warn, fontSize = 11.sp)
            Column(Modifier.weight(1f).fillMaxWidth().verticalScroll(rememberScrollState())) {
                for (line in live.screen.lines) {
                    Text(line.ifEmpty { " " }, color = Fg, fontSize = 12.sp, fontFamily = FontFamily.Monospace, maxLines = 1)
                }
            }
            var text by remember { mutableStateOf("") }
            var note by remember { mutableStateOf<String?>(null) }
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                BasicTextField(
                    value = text,
                    onValueChange = { text = it },
                    textStyle = TextStyle(color = Fg, fontSize = 13.sp, fontFamily = FontFamily.Monospace),
                    cursorBrush = SolidColor(Accent),
                    modifier = Modifier.weight(1f).padding(vertical = 8.dp),
                    singleLine = true,
                )
                AccentBtn("send") {
                    val bytes = (text + "\r").encodeToByteArray()
                    scope.launch {
                        note = when (conn.driver.input(bytes)) {
                            InputOutcome.Accepted -> { text = ""; null }
                            InputOutcome.QueueFull -> "the send queue is full, wait for it to drain"
                            InputOutcome.TooLong -> "at most ${MeshTerm.MAX_INPUT} bytes per send"
                            InputOutcome.NotLive, InputOutcome.Empty -> "not sent"
                        }
                    }
                }
                AccentBtn("^C") { scope.launch { conn.driver.input(byteArrayOf(3)) } }
            }
            note?.let { Text(it, color = Warn, fontSize = 11.sp) }
        } else {
            // Not live: say which of the not-working cases it is, and offer nothing to type into.
            Text(meshTermLabel(view), color = Dim, fontSize = 12.sp)
            when (val v = view) {
                // The host's own list, verbatim: choosing sends that id and nothing is guessed.
                is MeshTermView.Choose -> Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    for (e in v.entries) {
                        Text(
                            meshShellLine(e),
                            color = Fg,
                            fontSize = 13.sp,
                            fontFamily = FontFamily.Monospace,
                            modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)
                                .clickable { scope.launch { conn.driver.choose(e.id) } },
                        )
                    }
                }
                MeshTermView.NoShells -> AccentBtn("ask again") { scope.launch { conn.driver.refresh() } }
                else -> {}
            }
        }
    }
}
