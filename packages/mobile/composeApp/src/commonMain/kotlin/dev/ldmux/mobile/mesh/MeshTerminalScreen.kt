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
import dev.ldmux.mobile.BackgroundSession
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
import androidx.compose.runtime.snapshotFlow
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
import dev.ldmux.mobile.PortraitBar
import dev.ldmux.mobile.ScrollPad
import dev.ldmux.mobile.Session
import dev.ldmux.mobile.Warn
import dev.ldmux.mobile.tildify
import kotlin.random.Random
import kotlin.time.TimeSource
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * How long a sync waits for the host's answer before the screen stops waiting and says so.
 *
 * A radio round trip is about a second and a full screen can be several records, so this is
 * generous on purpose; a link that has run out of budget never answers, and that is the case this
 * exists for. Longer than this and a person is sitting on a screen that looks live and is not.
 */
internal const val SYNC_ANSWER_TIMEOUT_MS = 20_000L

/**
 * The mesh route's terminal. It is the **same chrome as the tailnet terminal**: the shared
 * [PortraitBar] (shell header with its arrows, menu, Esc/Tab/Home/End/PgUp/PgDn, the sticky
 * Ctrl/Alt/Shift latch, C/c, paste, Enter, the arrow pad, the keyboard toggle) drives a
 * [MeshTerminalHandle] where the tailnet drives a surface, and the terminal itself is the
 * tailnet's own surface ([TerminalSurfaceFor]) over a [MeshTerminalSource]: same painter, cursor,
 * tap-to-move, selection and IME field, fed from committed screens instead of an attach stream. What differs is the link, and the
 * differences are stated where they bite and nowhere else:
 *  - plain-text rows, no colour, scrollback, mouse or clipboard writes (the host's own
 *    `absent` list, stated once where the link is described, not above every shell list);
 *  - what you type waits a moment and leaves in one record (the list screen shows what is
 *    waiting);
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
) {
    val scope = rememberCoroutineScope()
    // Reconnection is the caller's decision by contract (the mesh connection never retries by
    // itself), so the caller needs a way to make it. A refused link or an unreachable radio
    // each left the screen stuck with no action but leaving; the only cure was to force-stop the
    // app. Rebuilding the connection is the whole retry.
    var attempt by remember(plan) { mutableStateOf(0) }
    // **A rebuilt connection comes back to the same shell.** The attach carries an id, so a resume is
    // one record instead of a fresh list and a tap: a drop costs the user a pause, not their place.
    // Keyed on the plan rather than the connection, because the connection is thrown away on every
    // rebuild, and that is exactly the value that must survive it.
    var resumeShell by remember(plan) { mutableStateOf("") }
    val conn = remember(plan, attempt) {
        val link = meshRadioLink(plan.radioAddress, plan.creds.hostRadio) ?: UnsupportedRadioLink()
        link.setBudgetMode(if (config.meshOpenAirtime) RadioBudgetMode.Open else RadioBudgetMode.Metered)
        MeshRadioConnection(link, plan.creds, plan.lib, resumeShell)
    }
    // **A connection that ended is rebuilt, not left dead.** `run` returns when the transport
    // stopped carrying records - a radio that went quiet, a socket the host stopped answering on.
    // Nothing inside the driver can revive that, which is why the refresh button was the only cure
    // (measured 2026-10-31: "securing the link" until reload, then instantly working). The rebuild
    // is that reload, on the same backoff: two seconds doubling to thirty, and back to two as soon
    // as the link is live.
    var rebuilds by remember(plan) { mutableStateOf(0) }
    // The connection ended while the app was already off screen, after the jitter below had begun.
    // The return decides then, so a backoff meant for a failure someone was watching never spends
    // the radio on a screen nobody is looking at.
    var ended by remember(conn) { mutableStateOf(false) }
    // The platform's release closed this connection. Not a failure, so it must not pay the failure
    // backoff - and the timer can fire late: Android freezes the process, so the 45-second grace
    // often expires only when the app is unfrozen on return, by which time it is foreground again.
    var released by remember(conn) { mutableStateOf(false) }
    LaunchedEffect(conn) {
        conn.run()
        // The link is down: do not hold a foreground service for it while the user reads the
        // reason or leaves the app.
        BackgroundSession.setActive(false, "")
        // **A link that died while the app was off screen is rebuilt the moment the app is back**,
        // not after a two-to-thirty-second wait meant for a failure the user watched happen. The
        // wait itself is the platform's: this coroutine is suspended here while the process may be
        // frozen, which is exactly the state a dead radio would otherwise be hidden in.
        if (released || !AppLifecycle.foreground) {
            if (!AppLifecycle.foreground) snapshotFlow { AppLifecycle.foreground }.first { it }
            attempt += 1
            return@LaunchedEffect
        }
        // A state that a retry cannot cure must not spin the rebuild loop. The plan or settings
        // change is the user's cure, and that rebuilds this screen anyway.
        val terminal = conn.driver.view.value
        if (terminal is MeshTermView.LinkDown && terminal.link in setOf(
                MeshLinkState.NeedsRadio,
                MeshLinkState.NeedsPermission,
                MeshLinkState.NoPath,
                MeshLinkState.Refused,
            )
        ) {
            return@LaunchedEffect
        }
        // **Full jitter, because two ends that double in lockstep keep colliding.** The cap is the same
        // two-to-thirty seconds as before, but the wait is drawn uniformly below it: a phone and a host
        // that both back off on 2, 4, 8, 16 meet again on 2, 4, 8, 16, which is the shape every account
        // of a reconnect storm prescribes against.
        val cap = (2_000L shl rebuilds.coerceAtMost(4)).coerceAtMost(30_000L)
        rebuilds += 1
        delay(Random.nextLong(1, cap + 1))
        if (AppLifecycle.foreground) attempt += 1 else ended = true
    }
    DisposableEffect(conn) {
        // **A long stay off screen is not a pause the link survives.** The tailnet route lets go of
        // its shells after the platform's grace and reconnects on return; this route now does the
        // same. Without it the socket is left half-open across a frozen process, the screen keeps the
        // last committed frame as if it were live, and typing goes into a session nobody is driving
        // (measured on the device, 2026-11-01: backgrounded for a long time, came back to a frozen
        // picture and no link). Closing here wakes `run`, and the effect above rebuilds on return,
        // re-attaching `resumeShell`, so the user gets their place back rather than a stale screen.
        val remove = AppLifecycle.setReleaseHandler {
            released = true
            BackgroundSession.setActive(false, "")
            conn.close()
        }
        onDispose {
            remove()
            // A rebuilt connection must not leak its reference to the shared link.
            conn.close()
        }
    }
    // The other half of that decision: a connection that ended while the app was off screen gets a
    // fresh one as soon as it is back. `released` needs no flag of its own: the release closes the
    // connection, and this is the same path.
    LaunchedEffect(AppLifecycle.foreground) {
        if (AppLifecycle.foreground && ended) {
            ended = false
            attempt += 1
        }
    }
    val view by conn.driver.view.collectAsState()
    // The mesh link is a foreground service while it is up, exactly like the tailnet
    // link. Without this Android freezes the process before the release grace and the
    // BLE link is not actually kept alive while the user looks at another app.
    LaunchedEffect(view) {
        val up = when (view) {
            MeshTermView.Idle, is MeshTermView.LinkDown, is MeshTermView.HostClosed -> false
            else -> true
        }
        BackgroundSession.setActive(up, "Mesh session open")
    }
    DisposableEffect(conn) {
        onDispose { BackgroundSession.setActive(false, "") }
    }
    val attemptNo by conn.driver.attempt.collectAsState()
    LaunchedEffect(view) { if (view is MeshTermView.Live) rebuilds = 0 }
    val shells by conn.driver.shells.collectAsState()
    // Learned from the driver, not from the tap: a shell the host chose for us (the one-shell attach)
    // resumes exactly as reliably as one the user picked.
    LaunchedEffect(shells.current) { shells.current?.let { resumeShell = it } }

    val origin = remember { TimeSource.Monotonic.markNow() }
    val clock: () -> Long = { origin.elapsedNow().inWholeMilliseconds }
    // **The input queue belongs to the connection, the picture does not.** Carrying a batcher across a
    // rebuild re-delivers text that the old session had already accepted: measured 2026-11-01, a
    // single `input text` came out as `yhgecho dsh9 > /tmpmecho dsh9 > /tmp/meshin9` in the shell -
    // the tail of the old queue prepended to the new one. Typed text is addressed to one session and
    // one sequence space, so it dies with it; only the frame below survives, because a picture has no
    // addressee.
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
    //
    // **This loop is the only sender**, so it is also where public-channel mode holds: while the
    // mode is on and no sync is waiting, the loop reads what is waiting and sends nothing. Typing
    // still accumulates in the batcher, in order, keys and all — and the bar over the terminal
    // shows it, so "held" is a visible state rather than a silent one. A sync clears the gate for
    // exactly as long as it takes to drain, and the answer arrives as the host's next screen.
    LaunchedEffect(conn) {
        while (true) {
            handle.refreshPending()
            // The gate is `flushingHeld`, not `awaitingSync`: the wait for the host's answer can
            // last seconds, and typing during it must still be held. A single flag would send
            // everything typed while the answer was on its way.
            if (source.publicChannel && !source.flushingHeld) {
                delay(120)
                continue
            }
            val step = pump.step()
            handle.refreshPending()
            // The drain is over when nothing is left, whatever the step said about windows.
            if (!handle.hasPending) source.syncDrained()
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

    // A sync whose answer never comes must not leave the screen frozen with no word about it: the
    // link may be out of budget or down, and the honest answer is "here is the newest screen we
    // hold, and no answer came". Twenty seconds is several radio round trips — long enough not to
    // cry wolf on a slow link, short enough that a person does not sit on a dead screen.
    var syncNotice by remember { mutableStateOf<String?>(null) }
    LaunchedEffect(source.awaitingSync) {
        if (!source.awaitingSync) { syncNotice = null; return@LaunchedEffect }
        delay(SYNC_ANSWER_TIMEOUT_MS)
        if (source.syncTimedOut()) syncNotice = "no answer yet: the link may be busy or down"
    }
    // A notice over a terminal is read while typing, so it must not stay once it has been read.
    // Two different lifetimes: "no answer" is a fact about the last thing you did and stays a few
    // seconds; "no history yet" is about a tap and goes as soon as the next scroll moves.
    LaunchedEffect(syncNotice) {
        if (syncNotice == null) return@LaunchedEffect
        delay(4_000)
        syncNotice = null
    }
    LaunchedEffect(source.scrollNotice) {
        if (source.scrollNotice == null) return@LaunchedEffect
        delay(2_500)
        source.clearScrollNotice()
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
    // **Which shells are pages, and nothing else: the tailnet's rule.** A row is ticked, not opened,
    // and the one `open` at the bottom makes exactly the ticked rows the pages the header's arrows
    // walk, in the order they were ticked. Before there was a selection every shell the host listed
    // was a page, so a header reading `1 / 2 shells` followed a tap on one row, and the tap itself
    // attached at once with no way to choose a second.
    var chosen by remember(conn) { mutableStateOf(listOf<String>()) }
    var pageOrder by remember(conn) { mutableStateOf(listOf<String>()) }

    val imeUp = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    // The keyboard is portrait-only and is the user's, as on the tailnet route (AppContent).
    var keyboardWanted by remember { mutableStateOf(true) }
    var keyboardRequest by remember { mutableStateOf(0) }
    val windowSize = LocalWindowInfo.current.containerSize
    val portrait = windowSize.height >= windowSize.width
    LaunchedEffect(portrait) { if (portrait) keyboardWanted = true }
    // **A rebuilt connection asks for the keyboard again.** The surface - and the hidden field that
    // carries the IME - is recreated with the connection, so a keyboard the user had open stays
    // drawn by the system while the new field has never been focused: typing went nowhere after a
    // resume (measured 2026-11-01, a command typed 12 s after a radio reconnect never reached the
    // pty). Nothing about wanting to type changed, so nothing about the request should either.
    LaunchedEffect(conn) { if (keyboardWanted) keyboardRequest++ }
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
    // Leaving the terminal is the user's decision, so it is also the one that cancels the resume: a
    // rebuild after this must land on the list, not drag them back into a shell they left.
    val openList = {
        chosen = emptyList(); showList = true; resumeShell = ""
        scope.launch { conn.driver.listShells() }
        Unit
    }
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
            view is MeshTermView.HostClosed -> {
                // A shell the host no longer has must not be resumed forever.
                resumeShell = ""
                attempt += 1
            }
            else -> attempt += 1
        }
    }
    // **The refresh square is never dimmed by a wait.** It used to dim in exactly the states that
    // are waits - securing, listing, waiting for a profile - which is when the user most wants to
    // try again: on a stuck link it sat greyed out for minutes (measured on the device,
    // 2026-10-31) and the one escape hatch from a dead link looked disabled. A retry is cheap and
    // idempotent, so the button stays live.
    // Back is the launcher button, as on the tailnet route: the same call, not a second way.
    BackToLauncher(enabled = terminalLive && !showList) { openList() }

    // safeDrawingPadding first: this is drawn instead of AppContent's own Column, so without it the
    // status line sits under the clock and the nav bar eats the last row.
    // **A drop does not take the screen away.** While the link is being rebuilt, the last painted
    // screen stays with the link's own line over it. Throwing the user back to the shell list for a
    // two-second radio hiccup is what made a working session read as a dead app (measured 2026-11-01:
    // every typing test ended on the launcher mid-sentence); the resume above brings the same shell
    // back underneath, so this is a pause drawn as a pause.
    // The last whole screen, kept across connections by the screen rather than by the source, so a
    // rebuild can re-show it into a fresh source without carrying anything addressed to a session.
    var heldScreen by remember(plan) { mutableStateOf<MeshScreen?>(null) }
    LaunchedEffect(source, view) { meshScreenToShow(view)?.let { heldScreen = it } }
    val holding = !terminalLive && !showList && resumeShell.isNotEmpty() && heldScreen != null &&
        (view is MeshTermView.LinkDown || view is MeshTermView.Handshaking || view is MeshTermView.Idle)

    // **A link that stays down returns to the shell list.** The held screen above is the right
    // answer to a two-second rebuild; it is the wrong answer to a host that has stopped
    // answering, which is a state the user must be able to leave and retry. The shell is not
    // lost by going back: re-opening it is one `Attach`, and the list carries the reason.
    LaunchedEffect(holding) {
        if (!holding) return@LaunchedEffect
        delay(dev.ldmux.mobile.LINK_FAILURE_GRACE_MS)
        if (holding) openList()
    }

    Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
        if (holding) {
            Column(Modifier.weight(1f).fillMaxWidth()) {
                // The one line that explains the pause, in the terminal's own type so it reads as the
                // link talking rather than as chrome.
                Text(
                    meshTermLabel(view),
                    color = Dim, fontSize = 11.sp, fontFamily = FontFamily.Monospace,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                )
                val keep = heldScreen
                LaunchedEffect(source, keep) { if (keep != null) source.show(keep) }
                TerminalSurfaceFor(
                    source = source,
                    key = resumeShell,
                    config = config,
                    typing = false,
                    active = false,
                    modifier = Modifier.weight(1f).fillMaxWidth(),
                )
            }
        } else if (terminalLive && !showList) {
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
                                WaitingLine("waiting for the shell's screen", attemptNo)
                            }
                        }
                        // The same three controls as the tailnet route, over the same surface.
                        // The mesh view has no history from the host, so the source keeps the
                        // rows the host's line numbers say have scrolled off.
                        ScrollPad(
                            onUp = { source.scrollBy(-dev.ldmux.mobile.SCROLL_STEP) },
                            onDown = { source.scrollBy(dev.ldmux.mobile.SCROLL_STEP) },
                            onBottom = { source.scrollToBottom() },
                            modifier = Modifier.align(Alignment.TopEnd),
                        )
                        VoiceBanner(voice, Modifier.align(Alignment.TopStart))
                        // Public-channel mode's one control: the standalone sync button. Drawn over the
                        // bottom of the terminal rather than in a key row, because it has to say what is
                        // waiting and how stale the screen is, and because while it is drawn the mode is on.
                        PublicChannelBar(
                            on = source.publicChannel,
                            summary = handle.heldSummary,
                            awaiting = source.awaitingSync,
                            flushing = source.flushingHeld,
                            updates = source.heldUpdates,
                            onToggle = {
                                if (!source.publicChannel) {
                                    source.setPublicChannel(true)
                                } else {
                                    // Leaving sends what is held: turning the mode off clears the gate in the
                                    // pump loop, so the queue drains on its own windows. A mode that could be
                                    // left while silently dropping what you typed would be worse than one
                                    // that sends on the way out, and the bar says which it is.
                                    source.setPublicChannel(false)
                                }
                            },
                            onSync = { source.sync() },
                            onDiscard = { handle.discardHeld() },
                            modifier = Modifier.align(Alignment.BottomEnd),
                        )
                        // What the link adds is said only while it is true, over the terminal like the voice
                        // banner, and never as a row: the tailnet's live screen is the terminal and the
                        // bar. The hour's records and the typing that waits are on the shell list.
                        val notice = when {
                            syncNotice != null -> syncNotice
                            source.scrollNotice != null -> source.scrollNotice
                            handle.overflow && source.publicChannel -> "too much held: sync to send"
                            handle.overflow -> "too much typed: wait for it to send"
                            live?.stalled == true -> "waiting for send budget"
                            live?.screen?.alternate == true -> "full-screen program: updates are large and slow"
                            else -> null
                        }
                        if (notice != null) {
                            Text(
                                notice,
                                color = Warn, fontSize = 11.sp,
                                modifier = Modifier.align(Alignment.TopCenter).background(Bg).padding(horizontal = 6.dp, vertical = 2.dp),
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
                    onRefresh = onRefresh,
                    refreshing = false,
                    onSettings = onOpenSettings,
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
                    // **Nothing here narrates the link.** "Not on this link: scrollback, style,
                    // hyperlinks, ..." and "send budget: 255 or more records left this hour" were
                    // two grey lines above the shell list, repeating every visit, describing a limit
                    // the user cannot act on from this screen (asked to be removed, 2026-11-01:
                    // "when did this come from? I didn't see it earlier"). What the link cannot do
                    // is true of every shell in the list, so it belongs beside the route, not above
                    // each list; what the hour has left is the radio's own measurement and the
                    // daemon reports it (`ldmux mesh status`).
                    if (handle.pendingLabel.isNotEmpty()) Text("waiting to send: ${handle.pendingLabel}", color = Dim, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
                }
                if (listed != null) {
                    // A list with nothing in it is a sentence, not an empty rectangle above `open`.
                    if (listed.isEmpty()) meshBodyFor(v)?.let { MeshBodyBlock(it) }
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
                        WaitingLine(waiting, attemptNo)
                    } else {
                        // Every other state says what is wrong. A radio that just refused the record,
                        // or a host that stopped answering, is a fresh-connection case; a missing
                        // radio or key is a settings case, not a retry.
                        meshBodyFor(v)?.let { MeshBodyBlock(it) }
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
                    // The folder is the row's second line, as on the tailnet launcher: it is what
                    // tells two shells of the same kind apart. The id is still the attach
                    // selector, it is just no longer the thing a person reads.
                    title = e.label.ifEmpty { e.id },
                    subtitle = meshRowSubtitle(e),
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
    fun page(id: String, label: String, cwd: String) = Session(
        id = id, kind = "shell", title = label, cwd = cwd.ifEmpty { label }, status = "running", keeperName = id,
    )
    val byId = shells.entries.associateBy { it.id }
    val ids = order.filter { it in byId }.distinct()
        .ifEmpty { listOfNotNull(shells.current?.takeIf { it in byId }) }
    if (ids.isEmpty()) return listOf(page("", "shell", ""))
    return ids.map { id ->
        byId.getValue(id).let { page(it.id, it.label.ifEmpty { it.id.take(8) }, it.cwd) }
    }
}

/**
 * The second line of a shell row: where the shell is, as the tailnet launcher shows it.
 *
 * A version-5 host does not send a working directory - the field is not on its wire - so the row
 * falls back to the id it would attach with rather than showing an empty line.
 */
internal fun meshRowSubtitle(e: SessionEntry): String =
    if (e.cwd.isEmpty()) e.id else tildify(e.cwd)

/**
 * A wait, in words.
 *
 * It used to count seconds and, past a minute, add "nothing back yet". That number was never the
 * age of the attempt: it was the age of the *screen*, so it climbed through retries that were
 * working exactly as intended and reached four figures on a link that was being re-asked every six
 * seconds (measured on the device, 2026-10-31: "107 s", "290 s" while the driver retried). A
 * number that measures the wrong thing is worse than no number - the state is what the user needs,
 * and the retry itself is the proof that something is happening.
 */
@Composable
private fun WaitingLine(text: String, key: Any?) {
    // The seconds are the age of **this attempt**, so the key changes whenever the driver starts a
    // new one. The first version keyed on the view's class name, which never changed while the
    // driver retried underneath it: it counted to 290 on a link that was being re-asked every six
    // seconds, which reads as a hang. Reset per attempt, it says what it looks like it says.
    var secs by remember(key) { mutableStateOf(0L) }
    LaunchedEffect(key) {
        while (true) {
            delay(1000)
            secs += 1
        }
    }
    Text("$text · ${secs} s", color = Dim, fontSize = 12.sp, fontFamily = FontFamily.Monospace)
}
