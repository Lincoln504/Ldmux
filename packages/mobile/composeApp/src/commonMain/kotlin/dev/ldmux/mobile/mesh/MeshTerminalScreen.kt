package dev.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imePadding
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
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.AccentBtn
import dev.ldmux.mobile.Bg
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.InputOp
import dev.ldmux.mobile.PortraitBar
import dev.ldmux.mobile.Session
import dev.ldmux.mobile.Warn
import dev.ldmux.mobile.inputOpsFor
import kotlin.time.TimeSource
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * The mesh route's terminal. It is the **same chrome as the tailnet terminal**: the shared
 * [PortraitBar] (shell header with its arrows, menu, Esc/Tab/Home/End/PgUp/PgDn, the sticky
 * Ctrl/Alt/Shift latch, C/c, paste, Enter, the arrow pad, the keyboard toggle) drives a
 * [MeshTerminalHandle] where the tailnet drives a surface. What differs is the link, and the
 * differences are stated where they bite and nowhere else:
 *  - plain-text rows, no colour, scrollback, mouse, selection or clipboard (the host's own
 *    `absent` list, one line);
 *  - what you type waits a moment and leaves in one record (the line above the keys shows
 *    what is waiting, and how many records the hour has left);
 *  - a full-screen program makes updates large and slow (one warning line, only then).
 *
 * The header lists the host's own shells and moves between them with one `Attach`; a shell
 * opened at the desk appears after the list is asked for again (menu → shells). Nothing here
 * knows a radio parameter.
 */
@Composable
fun MeshTerminalScreen(plan: MeshScreenPlan, onLeave: () -> Unit) {
    val scope = rememberCoroutineScope()
    // Reconnection is the caller's decision by contract (`MeshGatewayClient` never retries by
    // itself), so the caller needs a way to make it. A refused bind, a rotated token or an
    // unreachable gateway each left the screen stuck with no action but leaving; the only cure
    // was to force-stop the app. Rebuilding the connection is the whole retry.
    var attempt by remember(plan) { mutableStateOf(0) }
    val conn = remember(plan, attempt) { MeshTermConnection(meshSocketFactory(), scope, plan.creds, plan.lib, plan.endpoint) }
    LaunchedEffect(conn) { conn.run() }
    DisposableEffect(conn) { onDispose { conn.close() } }
    val view by conn.driver.view.collectAsState()
    val shells by conn.driver.shells.collectAsState()

    val origin = remember { TimeSource.Monotonic.markNow() }
    val clock: () -> Long = { origin.elapsedNow().inWholeMilliseconds }
    val batcher = remember(conn) { MeshInputBatcher() }
    val handle = remember(conn) { MeshTerminalHandle(batcher, clock) }
    val pump = remember(conn) { MeshInputPump(batcher, { conn.driver.input(it) }, clock) }

    // Moves what the user typed to the driver. Polling at a quarter second is far finer than
    // the batcher's own windows (700 ms idle), and a step never sends more than one record.
    LaunchedEffect(conn) {
        while (true) {
            val step = pump.step()
            handle.refreshPending()
            when (step) {
                PumpStep.Sent -> {}
                PumpStep.Waiting -> delay(600)
                PumpStep.Blocked -> delay(500)
                PumpStep.Idle -> {
                    val due = batcher.nextDueMs()
                    delay(if (due == null) 250L else (due - clock()).coerceIn(30L, 250L))
                }
            }
        }
    }

    val live = view as? MeshTermView.Live
    // Asked once per connection, on first arriving: one record up, one or more down. After that
    // it is the user's (menu → shells), never a timer, because the link has about 58 records an hour.
    var listed by remember(conn) { mutableStateOf(false) }
    LaunchedEffect(conn, live != null) {
        if (live != null && !listed) { listed = true; conn.driver.listShells() }
    }

    var showPicker by remember(conn) { mutableStateOf(false) }
    var menuOpen by remember(conn) { mutableStateOf(false) }
    val imeUp = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    val keyboard = LocalSoftwareKeyboardController.current
    val focus = remember { FocusRequester() }

    // safeDrawingPadding first: the mesh screen is drawn instead of AppContent's own Box,
    // so without it the status line sits under the clock and the nav bar eats the last row.
    Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Row(Modifier.fillMaxWidth().padding(start = 8.dp, end = 8.dp, top = 8.dp), horizontalArrangement = Arrangement.SpaceBetween) {
            // The label must wrap to its own row rather than squeeze the button: a Row with
            // SpaceBetween gave "leave" one character of width and it drew vertically.
            Text(
                meshTermLabel(view, plan.endpoint),
                color = if (live != null) Fg else Warn,
                fontSize = 13.sp,
                maxLines = 2,
                modifier = Modifier.weight(1f, fill = false).padding(end = 8.dp),
            )
            Text("leave", color = Accent, fontSize = 13.sp, maxLines = 1, modifier = Modifier.padding(4.dp).clickable(onClick = onLeave))
        }
        if (live != null || (view is MeshTermView.ProfileMissing && shells.entries.isNotEmpty())) {
            val absent = meshAbsentLine(live?.absent ?: emptyList())
            if (absent.isNotEmpty()) Text(absent, color = Dim, fontSize = 11.sp, modifier = Modifier.padding(horizontal = 8.dp))
            if (live?.screen?.alternate == true) {
                Text("A full-screen program is running: updates are large and slow here.", color = Warn, fontSize = 11.sp, modifier = Modifier.padding(horizontal = 8.dp))
            }
            Column(Modifier.weight(1f).fillMaxWidth().padding(horizontal = 8.dp).verticalScroll(rememberScrollState())) {
                for (line in live?.screen?.lines.orEmpty()) {
                    Text(line.ifEmpty { " " }, color = Fg, fontSize = 12.sp, fontFamily = FontFamily.Monospace, maxLines = 1)
                }
            }
            if (showPicker) {
                Column(Modifier.fillMaxWidth().background(dev.ldmux.mobile.Surface).padding(8.dp), verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    if (shells.entries.isEmpty()) Text("no shells listed yet", color = Dim, fontSize = 12.sp)
                    for (e in shells.entries) {
                        Text(
                            meshShellLine(e),
                            color = if (e.id == shells.current) Accent else Fg,
                            fontSize = 13.sp,
                            fontFamily = FontFamily.Monospace,
                            modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)
                                .clickable { showPicker = false; scope.launch { conn.driver.selectShell(e.id) } },
                        )
                    }
                }
            }
            // The one line the link's limits live on: what is waiting to be sent, and the hour's
            // records. Next to the input, where the user is when it matters.
            Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(
                    if (handle.overflow) "too much typed: wait for it to send" else handle.pendingLabel.ifEmpty { "type, or use the keys" },
                    color = if (handle.overflow) Warn else if (handle.pendingLabel.isEmpty()) Dim else Fg,
                    fontSize = 12.sp, fontFamily = FontFamily.Monospace, maxLines = 1,
                    modifier = Modifier.weight(1f, fill = false),
                )
                Text(meshBudgetLine(live?.budgetLeft).removePrefix("send budget: "), color = Dim, fontSize = 11.sp, maxLines = 1)
            }
            // The soft keyboard's target. It is an input method's buffer, read as a delta (the same
            // rule the tailnet surface uses), so a swipe, a paste and a backspace all arrive as what
            // they were. Backspace on an empty buffer is not a text change: it is caught as a key.
            var field by remember(conn) { mutableStateOf("") }
            BasicTextField(
                value = field,
                onValueChange = { next ->
                    for (op in inputOpsFor(field, next)) when (op) {
                        is InputOp.Keys -> op.names.forEach(handle::sendKey)
                        is InputOp.Text -> handle.sendText(op.value)
                    }
                    // A buffer that only grows is a buffer the IME keeps re-reading: start it fresh
                    // once it is long, between changes, when nothing is mid-edit.
                    field = if (next.length > 200) "" else next
                },
                textStyle = TextStyle(color = Bg, fontSize = 1.sp),
                cursorBrush = SolidColor(Bg),
                modifier = Modifier
                    .fillMaxWidth().padding(horizontal = 8.dp)
                    .focusRequester(focus)
                    .onPreviewKeyEvent { e ->
                        if (e.type == KeyEventType.KeyDown && e.key == Key.Backspace && field.isEmpty()) { handle.sendKey("BSpace"); true } else false
                    },
            )
            PortraitBar(
                handle = handle,
                pages = meshPages(shells),
                pagerState = null,
                scope = scope,
                keyboardUp = imeUp,
                onToggleKeyboard = {
                    if (imeUp) keyboard?.hide() else { focus.requestFocus(); keyboard?.show() }
                },
                onMenu = { menuOpen = true },
                menuExpanded = menuOpen,
                onMenuDismiss = { menuOpen = false },
                // The menu's one item is "shells": here it asks the host again and shows its list.
                onLauncher = { showPicker = !showPicker; scope.launch { conn.driver.listShells() } },
                modifiers = handle.modifiers,
                onSticky = handle::toggleModifier,
                press = handle::sendKey,
                shownId = shells.current ?: meshPages(shells).firstOrNull()?.id,
                onShow = { id -> scope.launch { conn.driver.selectShell(id) } },
            )
        } else {
            // Not live: say which of the not-working cases it is, and offer nothing to type into.
            Column(Modifier.padding(horizontal = 8.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
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
                    is MeshTermView.LinkDown, is MeshTermView.HostClosed -> AccentBtn("reconnect") { attempt += 1 }
                    else -> {}
                }
            }
        }
    }
}

/**
 * The host's shells as the shared header's pages. The header draws a folder name and a count, so a
 * shell's label stands where a directory would; before the host has listed anything the one shell
 * we are on is a page of its own, so the header is never empty.
 */
internal fun meshPages(shells: MeshShells): List<Session> {
    fun page(id: String, label: String) = Session(
        id = id, kind = "shell", title = label, cwd = label, status = "running", keeperName = id,
    )
    if (shells.entries.isEmpty()) return listOf(page("", "shell"))
    return shells.entries.map { page(it.id, it.label.ifEmpty { it.id.take(8) }) }
}
