package dev.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.material3.Text
import dev.ldmux.mobile.voice.VoiceState
import dev.ldmux.mobile.voice.VoiceBanner
import dev.ldmux.mobile.voice.Dictation
import dev.ldmux.mobile.SidePanel
import dev.ldmux.mobile.BackToLauncher
import dev.ldmux.mobile.AppLifecycle
import androidx.compose.ui.Alignment
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.AccentBtn
import dev.ldmux.mobile.Bg
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.LocalTouch
import dev.ldmux.mobile.TerminalSurfaceFor
import dev.ldmux.mobile.AppHeader
import dev.ldmux.mobile.PortraitBar
import dev.ldmux.mobile.Session
import dev.ldmux.mobile.Warn
import kotlin.time.TimeSource
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * The mesh route's terminal. It is the **same chrome as the tailnet terminal**: the shared
 * [PortraitBar] (shell header with its arrows, menu, Esc/Tab/Home/End/PgUp/PgDn, the sticky
 * Ctrl/Alt/Shift latch, C/c, paste, Enter, the arrow pad, the keyboard toggle) drives a
 * [MeshTerminalHandle] where the tailnet drives a surface, and the terminal itself is the
 * tailnet's own surface ([TerminalSurfaceFor]) over a [MeshTerminalSource]: same painter, cursor,
 * tap-to-move, selection and IME field, fed from committed screens instead of an attach stream. What differs is the link, and the
 * differences are stated where they bite and nowhere else:
 *  - plain-text rows, no colour, scrollback, mouse or clipboard writes (the host's own
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
fun MeshTerminalScreen(plan: MeshScreenPlan, config: LdmuxConfig, onLeave: () -> Unit, onOpenSettings: () -> Unit) {
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
    val source = remember(conn) { MeshTerminalSource(handle) }
    DisposableEffect(source) { onDispose { source.close() } }
    val pump = remember(conn) { MeshInputPump(batcher, { out ->
        when (out) {
            is MeshOut.Text -> conn.driver.input(out.bytes)
            is MeshOut.Keys -> conn.driver.keys(out.names)
        }
    }, clock) }

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

    // The list of the host's shells is this route's launcher: the tailnet's menu → shells leaves the
    // terminal for a screen with the header and the list, and so does this one.
    var showList by remember(conn) { mutableStateOf(false) }
    var menuOpen by remember(conn) { mutableStateOf(false) }
    val imeUp = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    // The keyboard is portrait-only and is the user's, as on the tailnet route (AppContent).
    var keyboardWanted by remember { mutableStateOf(true) }
    var keyboardRequest by remember { mutableStateOf(0) }
    val windowSize = LocalWindowInfo.current.containerSize
    val portrait = windowSize.height >= windowSize.width
    LaunchedEffect(portrait) { if (portrait) { keyboardWanted = true }; menuOpen = false }
    val typing = portrait && keyboardWanted

    // Dictation is the tailnet's, with its own state and banner: it types into the shell, which on
    // this route means the batcher. A microphone that outlives the screen is stopped at once.
    var voice by remember { mutableStateOf<VoiceState>(VoiceState.Idle) }
    DisposableEffect(conn) {
        val remove = AppLifecycle.onBackground { Dictation.stop() }
        onDispose { remove(); Dictation.stop() }
    }
    LaunchedEffect(voice) {
        if (voice is VoiceState.Failed || voice == VoiceState.Unavailable || voice == VoiceState.NoSpeech) {
            delay(4000)
            voice = VoiceState.Idle
        }
    }

    val terminalLive = live != null || (view is MeshTermView.ProfileMissing && shells.entries.isNotEmpty())
    val openList = { showList = true; scope.launch { conn.driver.listShells() }; Unit }
    // Back is the launcher button, as on the tailnet route: the same call, not a second way.
    BackToLauncher(enabled = terminalLive && !showList) { openList() }

    // safeDrawingPadding first: this is drawn instead of AppContent's own Column, so without it the
    // status line sits under the clock and the nav bar eats the last row.
    Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
        if (terminalLive && !showList) {
            // The terminal, and nothing above it: the tailnet's terminal has no header either. The
            // header belongs to the list screen, which is where Settings is, as on the tailnet.
            val pages = meshPages(shells)
            val shownId = shells.current ?: pages.firstOrNull()?.id
            val panel: @Composable () -> Unit = {
                SidePanel(
                    pages = pages,
                    pagerState = null,
                    scope = scope,
                    handles = pages.associate { it.id to (source as dev.ldmux.mobile.TerminalHandle) },
                    onVoice = {
                        Dictation.toggle(onText = { source.sendText(it) }, onState = { voice = it })
                    },
                    // No speech channel crosses this link, so there is no voice mode to run.
                    onVoiceMode = null,
                    onLauncher = { openList() },
                    press = handle::sendKey,
                    shownId = shownId,
                    onShow = { id -> scope.launch { conn.driver.selectShell(id) } },
                )
            }
            Row(Modifier.weight(1f).fillMaxWidth()) {
                if (!portrait && config.panelLeft) panel()
                Column(Modifier.weight(1f).fillMaxHeight()) {
                    // The terminal is the tailnet route's own surface (TerminalSurfaceFor): its painter,
                    // its cursor, tap-to-move, selection and the IME field that takes typing at the
                    // prompt. Painting on `Commit` only is the driver's rule, so the screen shown here
                    // is never half a screen.
                    LaunchedEffect(source, live?.screen) { live?.screen?.let(source::show) }
                    BoxWithConstraints(Modifier.weight(1f).fillMaxWidth()) {
                        val fit = meshFitFontSp(maxWidth.value, live?.screen?.cols ?: 0, LocalDensity.current.fontScale, config.fontSp)
                        TerminalSurfaceFor(
                            source = source,
                            key = shells.current ?: "mesh",
                            config = config.copy(fontSp = fit),
                            typing = typing,
                            active = true,
                            modifier = Modifier.fillMaxSize(),
                            keyboardRequest = keyboardRequest,
                        )
                        VoiceBanner(voice, Modifier.align(Alignment.TopStart))
                        if (live?.screen?.alternate == true) {
                            // A warning only while it is true, over the terminal like the voice banner.
                            Text(
                                "full-screen program: updates are large and slow",
                                color = Warn, fontSize = 11.sp,
                                modifier = Modifier.align(Alignment.TopEnd).background(Bg).padding(horizontal = 6.dp, vertical = 2.dp),
                            )
                        }
                    }
                    // The one line the link's limits live on: what is waiting to be sent, and the hour's
                    // records. Next to the input, where the user is when it matters.
                    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 2.dp), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text(
                            if (handle.overflow) "too much typed: wait for it to send" else handle.pendingLabel.ifEmpty { meshTermLabel(view, plan.endpoint) },
                            color = if (handle.overflow) Warn else if (handle.pendingLabel.isEmpty()) Dim else Fg,
                            fontSize = 11.sp, fontFamily = FontFamily.Monospace, maxLines = 1,
                            modifier = Modifier.weight(1f, fill = false),
                        )
                        Text(meshBudgetLine(live?.budgetLeft).removePrefix("send budget: "), color = Dim, fontSize = 11.sp, maxLines = 1)
                    }
                }
                if (!portrait && !config.panelLeft) panel()
            }
            if (portrait) {
                PortraitBar(
                    handle = handle,
                    pages = pages,
                    pagerState = null,
                    scope = scope,
                    keyboardUp = imeUp,
                    // Decided by what is on screen, not by the flag, exactly as the tailnet bar does it: the
                    // back gesture puts the keypad away without telling us.
                    onToggleKeyboard = {
                        if (imeUp) {
                            keyboardWanted = false
                        } else {
                            keyboardWanted = true
                            keyboardRequest++
                        }
                    },
                    onMenu = { menuOpen = true },
                    menuExpanded = menuOpen,
                    onMenuDismiss = { menuOpen = false },
                    onLauncher = { openList() },
                    modifiers = handle.modifiers,
                    onSticky = handle::toggleModifier,
                    press = handle::sendKey,
                    shownId = shownId,
                    onShow = { id -> scope.launch { conn.driver.selectShell(id) } },
                )
            }
        } else {
            // The launcher's shape: the app's own header (the one the tailnet launcher draws, with
            // Settings), then the body. The status line is the route's own, the one thing that really
            // differs; a refresh square is absent because a reconnect is what it would mean, and the
            // body offers that where the state explains it.
            Box(Modifier.fillMaxWidth().padding(start = LocalTouch.current.pad * 3, end = LocalTouch.current.pad * 3, top = LocalTouch.current.pad * 3)) {
                AppHeader(
                    title = "ldmux",
                    status = meshTermLabel(view, plan.endpoint),
                    onRefresh = null,
                    onSettings = onOpenSettings,
                    onLeave = onLeave,
                )
            }
            Column(Modifier.padding(horizontal = 8.dp, vertical = 6.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                if (terminalLive) {
                    // The host's own shells; choosing one attaches it and returns to the terminal.
                    val absent = meshAbsentLine(live?.absent ?: emptyList())
                    if (absent.isNotEmpty()) Text(absent, color = Dim, fontSize = 11.sp)
                    if (shells.entries.isEmpty()) Text("no shells listed yet", color = Dim, fontSize = 12.sp)
                    for (e in shells.entries) {
                        Text(
                            meshShellLine(e),
                            color = if (e.id == shells.current) Accent else Fg,
                            fontSize = 13.sp,
                            fontFamily = FontFamily.Monospace,
                            modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp)
                                .clickable { showList = false; scope.launch { conn.driver.selectShell(e.id) } },
                        )
                    }
                    Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        AccentBtn("terminal") { showList = false }
                        AccentBtn("ask again") { scope.launch { conn.driver.listShells() } }
                    }
                } else {
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
                        else -> {
                            // A wait is not nothing: it says what it is waiting for, with the seconds it
                            // has taken, and offers reconnect the moment the user wants to stop waiting.
                            val waiting = when (view) {
                                MeshTermView.Idle -> "starting"
                                MeshTermView.Handshaking -> "handshaking"
                                MeshTermView.ProfileMissing -> "the host has not described a terminal yet"
                                MeshTermView.Listing -> "asking the host which shells are running"
                                else -> null
                            }
                            if (waiting != null) {
                                WaitingLine(waiting, view::class.simpleName)
                                AccentBtn("reconnect") { attempt += 1 }
                            }
                        }
                    }
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

/** A wait with the seconds it has taken: proof the link is working, not the app hanging. */
@Composable
private fun WaitingLine(text: String, key: String?) {
    var secs by remember(key) { mutableStateOf(0L) }
    LaunchedEffect(key) {
        while (true) {
            delay(1000)
            secs += 1
        }
    }
    // Past a minute with no answer, say so. The handshake's own bound is five minutes, and the
    // difference between "still trying" and "nothing is out there" is the one thing the phone can
    // tell the user before that bound expires.
    val suffix = if (secs >= 60) " · nothing back yet" else ""
    Text("$text · ${secs} s$suffix", color = Dim, fontSize = 12.sp, fontFamily = FontFamily.Monospace)
}
