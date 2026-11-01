package dev.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
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
import androidx.compose.runtime.rememberUpdatedState
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
import dev.ldmux.mobile.Bg
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.LocalTouch
import dev.ldmux.mobile.TerminalSurfaceFor
import dev.ldmux.mobile.AppHeader
import dev.ldmux.mobile.ChoiceRow
import dev.ldmux.mobile.OpenBar
import dev.ldmux.mobile.openLabel
import dev.ldmux.mobile.RouteChoice
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
 *  - what you type waits a moment and leaves in one record (the list screen shows what is
 *    waiting, and how many records the hour has left);
 *  - a full-screen program makes updates large and slow (one warning line, only then).
 *
 * The shell list is the tailnet launcher's: rows are ticked, then the one `open` makes exactly the
 * ticked shells the header's pages, and moving between them is one `Attach`. A shell opened at the
 * desk appears after the list is asked for again (the header's refresh square). Nothing here
 * knows a radio parameter.
 */
@Composable
fun MeshTerminalScreen(
    plan: MeshScreenPlan,
    config: LdmuxConfig,
    onOpenSettings: () -> Unit,
    routes: List<RouteChoice> = emptyList(),
    currentRoute: RouteChoice? = null,
    onRoute: (RouteChoice) -> Unit = {},
    /** Asking MeshMore on this phone for a fresh gateway token; null where there is no such thing. */
    handoff: MeshHandoffUi? = null,
) {
    val scope = rememberCoroutineScope()
    // Reconnection is the caller's decision by contract (`MeshGatewayClient` never retries by
    // itself), so the caller needs a way to make it. A refused bind, a rotated token or an
    // unreachable gateway each left the screen stuck with no action but leaving; the only cure
    // was to force-stop the app. Rebuilding the connection is the whole retry.
    var attempt by remember(plan) { mutableStateOf(0) }
    val conn = remember(plan, attempt) { MeshTermConnection(meshSocketFactory(), scope, plan.creds, plan.lib, plan.endpoint) }
    // **A connection that ended is rebuilt, not left dead.** `run` returns when the transport
    // stopped carrying records - a radio that went quiet, a socket the host stopped answering on.
    // Nothing inside the driver can revive that, which is why the refresh button was the only cure
    // (measured 2026-10-31: "securing the link" until reload, then instantly working). The rebuild
    // is that reload, on the same backoff: two seconds doubling to thirty, and back to two as soon
    // as the link is live.
    var rebuilds by remember(plan) { mutableStateOf(0) }
    LaunchedEffect(conn) {
        conn.run()
        val wait = (2_000L shl rebuilds.coerceAtMost(4)).coerceAtMost(30_000L)
        rebuilds += 1
        delay(wait)
        attempt += 1
    }
    DisposableEffect(conn) { onDispose { conn.close() } }
    val view by conn.driver.view.collectAsState()
    LaunchedEffect(view) { if (view is MeshTermView.Live) rebuilds = 0 }
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

    // Moves what the user typed to the driver. The poll is never coarser than the batcher's own
    // windows (`IDLE_MS`, `MAX_HOLD_MS`), and a step never sends more than one record.
    LaunchedEffect(conn) {
        while (true) {
            val step = pump.step()
            handle.refreshPending()
            when (step) {
                PumpStep.Sent -> {}
                PumpStep.Waiting -> delay(150)
                PumpStep.Blocked -> delay(150)
                PumpStep.Idle -> {
                    val due = batcher.nextDueMs()
                    delay(if (due == null) 50L else (due - clock()).coerceIn(10L, 50L))
                }
            }
        }
    }

    val live = view as? MeshTermView.Live
    // **A refused token is not a decision the user should have to make.** It is unambiguous: the
    // stored token is dead, and the only cure is the fresh one MeshMore already has. So it is
    // fetched invisibly, once per refusal, and the button in the body stays as the manual fallback
    // for the cases that are not unambiguous (MeshMore off, nothing on the saved port).
    val handoffNow by rememberUpdatedState(handoff)
    LaunchedEffect(view) {
        val v = view
        val h = handoffNow ?: return@LaunchedEffect
        // `claimAuto` lives on the handoff object, which outlives this connection, so a refusal that
        // survives a re-connect cannot restart the cycle.
        if (v is MeshTermView.LinkDown && v.link == MeshLinkState.Refused && h.claimAuto()) {
            h.run(true)
        }
    }

    // Asked once per connection, on first arriving: one record up, one or more down. After that
    // it is the user's (menu → shells), never a timer, because the link has about 58 records an hour.
    var listed by remember(conn) { mutableStateOf(false) }
    LaunchedEffect(conn, live != null) {
        if (live != null && !listed) { listed = true; conn.driver.listShells() }
    }

    // The list of the host's shells is this route's launcher: the tailnet's menu → shells leaves the
    // terminal for a screen with the header and the list, and so does this one.
    var showList by remember(conn) { mutableStateOf(false) }
    // **Which shells are pages, and nothing else: the tailnet's rule.** A row is ticked, not opened,
    // and the one `open` at the bottom makes exactly the ticked rows the pages the header's arrows
    // walk, in the order they were ticked. Before there was a selection every shell the host listed
    // was a page, so a header reading `1 / 2 shells` followed a tap on one row, and the tap itself
    // attached at once with no way to choose a second.
    var chosen by remember(conn) { mutableStateOf(listOf<String>()) }
    var pageOrder by remember(conn) { mutableStateOf(listOf<String>()) }
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
    val openList = { chosen = emptyList(); showList = true; scope.launch { conn.driver.listShells() }; Unit }
    // The list's one action. The first ticked shell is page 1, as on the tailnet; the host takes one
    // `Attach` for it (`choose` on the first list, `selectShell` from a list opened over a live shell,
    // which spends nothing when it is the shell already attached).
    val openChosen: (List<SessionEntry>) -> Unit = { picked ->
        pageOrder = picked.map { it.id }
        showList = false
        val first = picked.first().id
        val choosing = view is MeshTermView.Choose
        scope.launch { if (choosing) conn.driver.choose(first) else conn.driver.selectShell(first) }
    }
    // The header's refresh square, as on the tailnet: it brings the screen up to date. Over a list that
    // is asking the host again; anywhere else it is the reconnect that used to be a button in the body.
    val onRefresh: () -> Unit = {
        when {
            terminalLive -> scope.launch { conn.driver.listShells() }
            view is MeshTermView.Choose || view is MeshTermView.NoShells -> scope.launch { conn.driver.refresh() }
            else -> attempt += 1
        }
    }
    val refreshing = view is MeshTermView.Idle || view is MeshTermView.Handshaking ||
        view is MeshTermView.ProfileMissing || view is MeshTermView.Listing
    // Back is the launcher button, as on the tailnet route: the same call, not a second way.
    BackToLauncher(enabled = terminalLive && !showList) { openList() }

    // safeDrawingPadding first: this is drawn instead of AppContent's own Column, so without it the
    // status line sits under the clock and the nav bar eats the last row.
    Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
        if (terminalLive && !showList) {
            // The terminal, and nothing above it: the tailnet's terminal has no header either. The
            // header belongs to the list screen, which is where Settings is, as on the tailnet.
            val pages = meshPagesFor(shells, pageOrder)
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
                    // is never half a screen — and never an empty one: a `Profile` blanks the driver's
                    // mirror until the host's next full screen arrives (every `Resize` sends one), and
                    // drawing that was the black terminal. Until a screen is committed the surface keeps
                    // what it had; a switch to another shell drops it, so one shell's rows are never
                    // under another's name.
                    val toShow = meshScreenToShow(view)
                    val switching = view is MeshTermView.ProfileMissing
                    LaunchedEffect(source, toShow, switching) {
                        if (toShow != null) source.show(toShow) else if (switching) source.clear()
                    }
                    val drawn by source.frame.collectAsState()
                    // The host fits its pty to what the surface measures (`Resize`, profile version
                    // 3), so the font is the user's own and a line is never wider than the screen.
                    DisposableEffect(source, conn) {
                        source.onGrid = { c, r -> scope.launch { conn.driver.setGrid(c, r) } }
                        onDispose { source.onGrid = null }
                    }
                    Box(Modifier.weight(1f).fillMaxWidth()) {
                        TerminalSurfaceFor(
                            source = source,
                            key = shells.current ?: "mesh",
                            config = config,
                            typing = typing,
                            active = true,
                            modifier = Modifier.fillMaxSize(),
                            keyboardRequest = keyboardRequest,
                        )
                        // A wait is not a black screen: with nothing drawn yet this says what is awaited, with
                        // the seconds it has taken. (A resize keeps the old screen up, so it needs no line.)
                        if (toShow == null && drawn == null) {
                            Box(Modifier.align(Alignment.TopStart).background(Bg).padding(horizontal = 6.dp, vertical = 2.dp)) {
                                WaitingLine("waiting for the shell's screen", shells.current ?: "mesh")
                            }
                        }
                        VoiceBanner(voice, Modifier.align(Alignment.TopStart))
                        // What the link adds is said only while it is true, over the terminal like the voice
                        // banner, and never as a row: the tailnet's live screen is the terminal and the
                        // bar. The hour's records and the typing that waits are on the shell list.
                        val notice = when {
                            handle.overflow -> "too much typed: wait for it to send"
                            live?.stalled == true -> "waiting for send budget"
                            live?.screen?.alternate == true -> "full-screen program: updates are large and slow"
                            else -> null
                        }
                        if (notice != null) {
                            Text(
                                notice,
                                color = Warn, fontSize = 11.sp,
                                modifier = Modifier.align(Alignment.TopEnd).background(Bg).padding(horizontal = 6.dp, vertical = 2.dp),
                            )
                        }
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
            // The launcher's shape, drawn the launcher's way: padding, the app's own header, then the
            // list with the one `open` under it. The status line is the route's own, the one thing
            // that really differs; the refresh square is the tailnet's reload (see [onRefresh]).
            Column(Modifier.weight(1f).fillMaxWidth().padding(LocalTouch.current.rowPad * 3)) {
                AppHeader(
                    title = "ldmux",
                    status = meshHeaderStatus(view, plan.endpoint),
                    onRefresh = onRefresh,
                    refreshing = refreshing,
                    onSettings = onOpenSettings,
                    routes = routes,
                    currentRoute = currentRoute,
                    onRoute = onRoute,
                )
                Spacer(Modifier.height(LocalTouch.current.rowPad * 2))
                val v = view
                // The host's own list, verbatim: only an id it listed can be opened, nothing is guessed.
                val listed = when {
                    terminalLive -> shells.entries
                    v is MeshTermView.Choose -> v.entries
                    else -> null
                }
                if (terminalLive) {
                    val absent = meshAbsentLine(live?.absent ?: emptyList())
                    if (absent.isNotEmpty()) Text(absent, color = Dim, fontSize = 11.sp)
                    Text(meshBudgetLine(live?.budgetLeft), color = Dim, fontSize = 11.sp)
                    if (handle.pendingLabel.isNotEmpty()) Text("waiting to send: ${handle.pendingLabel}", color = Dim, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
                }
                if (listed != null) {
                    // A list with nothing in it is a sentence, not an empty rectangle above `open`.
                    if (listed.isEmpty()) meshBodyFor(v, plan.endpoint)?.let { MeshBodyBlock(it, handoff, replaceStale = false) }
                    MeshShellList(
                        entries = listed,
                        current = if (terminalLive) shells.current else null,
                        chosen = chosen,
                        onToggle = { id -> chosen = if (id in chosen) chosen - id else chosen + id },
                        onOpen = openChosen,
                        modifier = Modifier.weight(1f),
                    )
                } else {
                    // Nothing to choose from: a wait says what it is waiting for, with the seconds it has
                    // taken. A dead link or a closed shell is the header's line and its refresh square.
                    val waiting = when (v) {
                        MeshTermView.Idle -> "starting"
                        MeshTermView.Handshaking -> "handshaking"
                        MeshTermView.ProfileMissing -> "the host has not described a terminal yet"
                        MeshTermView.Listing -> "asking the host which shells are running"
                        else -> null
                    }
                    if (waiting != null) {
                        WaitingLine(waiting, v::class.simpleName)
                    } else {
                        // Every other state says what is wrong, and offers the one cure there is. A token the
                        // link just refused, or a port with nothing on it, is the case a fresh handoff may
                        // replace; a token that was never stored is not a replacement at all.
                        meshBodyFor(v, plan.endpoint)?.let { MeshBodyBlock(it, handoff, replaceStale = true) }
                    }
                    Spacer(Modifier.weight(1f))
                }
            }
        }
    }
}

/**
 * The host's shells, ticked and then opened: the launcher's own rows ([ChoiceRow], numbered in the
 * order ticked) over the launcher's own `open` ([OpenBar]), so choosing is one gesture on both routes
 * and a tap on a row never leaves the list by itself.
 */
@Composable
private fun MeshShellList(
    entries: List<SessionEntry>,
    current: String?,
    chosen: List<String>,
    onToggle: (String) -> Unit,
    onOpen: (List<SessionEntry>) -> Unit,
    modifier: Modifier = Modifier,
) {
    // In the order ticked, skipping anything that left the list meanwhile (a shell can end between two taps).
    val picked = chosen.mapNotNull { id -> entries.firstOrNull { it.id == id } }
    Column(modifier) {
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
        ) {
            items(entries, key = { it.id }) { e ->
                ChoiceRow(
                    title = e.label.ifEmpty { e.id },
                    subtitle = e.id,
                    chosen = e.id in chosen,
                    ordinal = picked.indexOfFirst { it.id == e.id }.takeIf { it >= 0 }?.plus(1),
                    inUse = e.id == current,
                    onClick = { onToggle(e.id) },
                )
            }
        }
        Spacer(Modifier.height(LocalTouch.current.rowPad))
        OpenBar(openLabel(picked.size), empty = picked.isEmpty()) { onOpen(picked) }
    }
}

/**
 * What the shared surface should draw for [view]: the host's screen once at least one `Commit` has
 * been applied, and nothing before that. The driver's mirror is blank from a `Profile` until the next
 * full screen, and a blank screen is not a fact about the shell.
 */
internal fun meshScreenToShow(view: MeshTermView): MeshScreen? =
    (view as? MeshTermView.Live)?.takeIf { it.painted }?.screen

/**
 * The shared header's pages: the shells the user ticked, in that order, or — when nothing was ticked
 * because the host had exactly one shell and the attach took it — the shell attached. A shell's label
 * stands where a directory would; before the host has listed anything the one shell we are on is a
 * page of its own, so the header is never empty. Never every shell the host lists: that made one
 * ticked row read as `1 / 2 shells`.
 */
internal fun meshPagesFor(shells: MeshShells, order: List<String>): List<Session> {
    fun page(id: String, label: String) = Session(
        id = id, kind = "shell", title = label, cwd = label, status = "running", keeperName = id,
    )
    val byId = shells.entries.associateBy { it.id }
    val ids = order.filter { it in byId }.distinct()
        .ifEmpty { listOfNotNull(shells.current?.takeIf { it in byId }) }
    if (ids.isEmpty()) return listOf(page("", "shell"))
    return ids.map { id -> byId.getValue(id).let { page(it.id, it.label.ifEmpty { it.id.take(8) }) } }
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
