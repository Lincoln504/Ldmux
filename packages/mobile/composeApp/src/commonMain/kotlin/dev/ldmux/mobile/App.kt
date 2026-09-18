package dev.ldmux.mobile

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.imePadding
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.launch

/** The whole palette. Minimal, dark, one accent — no decoration. */
// Greyscale, deliberately. There is no accent *colour*: the only thing a hue
// could mean here is "this is the important one", and on a screen that is
// otherwise a terminal's own output, that is a claim worth making in weight and
// contrast instead. The one exception is the close confirmation, where the red is
// a warning rather than decoration.
val Bg = Color(0xFF0A0A0A)
val Fg = Color(0xFFEDEDED)
val Dim = Color(0xFF8C8C8C)
/** Nothing is tinted; "accent" is simply the lightest thing on screen. */
val Accent = Color(0xFFD0D0D0)
val Surface = Color(0xFF1A1A1A)
/** Rules and unselected outlines. */
val Line = Color(0xFF2A2A2A)

/** The landscape panel's width. The terminal gets everything else. */
private val PANEL_WIDTH = 190.dp

/** Where the daemon is and how to authenticate. Persisted by the platform. */
data class LdmuxConfig(
    val host: String,
    val token: String,
    val side: String = "left",
    /**
     * Show typed text before the server echoes it (docs/PREDICTIVE-ECHO.md).
     * Off by default: it trades a little honesty for a lot of latency, and a
     * prompt that does not echo is exactly where the trade is most visible.
     */
    val predict: Boolean = false,
    /**
     * Terminal font size. It is a client setting because the client owns the
     * geometry: a bigger font means fewer cells fit, and the client tells the
     * daemon the grid it can paint, so the pane reflows to match instead of
     * being scaled down to fit (ADR-0008).
     */
    val fontSp: Float = DEFAULT_FONT_SP,
    /**
     * This install's identity, announced with `hello` (ADR-0010).
     *
     * Stable for the life of the install, generated once and kept. It is **not**
     * an account and not a credential: it exists so two devices can be told apart
     * in a sentence about a shell they both want ("open on Pixel 8").
     */
    val deviceId: String = "",
    /** What that sentence calls this device. */
    val deviceLabel: String = "",
) {
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
        const val DEFAULT_FONT_SP = 12f
        const val MIN_FONT_SP = 7f
        const val MAX_FONT_SP = 28f

        val DEFAULT = LdmuxConfig(host = "localhost:7623", token = "", side = "left", predict = false)
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
/**
 * How the app is laid out. Chosen with an explicit button — the sensor never
 * rotates us, because a terminal that reflows under a table is worse than one
 * that stays put until you say so.
 */
enum class UiMode { Landscape, Portrait }

/**
 * How long a panel waits for the window to finish turning over before it appears.
 *
 * The rotation is Android's and it has already begun by the time the app can ask
 * about it — see the note in `App` — so this is a beat out of the turn itself
 * rather than a wait on a signal that does not exist.
 */
private const val PANEL_FLIP_MS = 200L

@Composable
fun App() {
    var config by remember { mutableStateOf(AppPrefs.load()) }
    var showSettings by remember { mutableStateOf(false) }
    var mode by remember { mutableStateOf(UiMode.Landscape) }
    val scope = rememberCoroutineScope()
    val client = remember { LdmuxClient(config) }

    LaunchedEffect(Unit) { client.start(scope) }
    val sessions by client.sessions.collectAsState()
    val desktop by client.desktop.collectAsState()
    val connected by client.connected.collectAsState()
    // Read here, not inside the launcher branch: a refusal releases a pending
    // shell request, and that request is made from a screen the branch does not own.
    val errorTick by client.errorTick.collectAsState()

    // A phone that has just rejoined the wifi should not wait out the retry
    // backoff: the network callback cuts it short.
    DisposableEffect(Unit) {
        BackgroundSession.watchNetwork { client.reconnectNow() }
        onDispose { BackgroundSession.stopWatchingNetwork() }
    }

    // The launcher comes first: a terminal is somewhere you go, and which shell
    // you are going to is a question worth asking before you are in one.
    var showLauncher by remember { mutableStateOf(true) }
    // Which shells the pager pages through. `null` means all of them; the
    // launcher sets a set when the user chooses, and that set is what ◀ ▶ walk.
    var pageSelection by remember { mutableStateOf<Set<String>?>(null) }
    // How many shells have been asked for and not yet answered. A chosen pi session
    // has no ldmux shell yet, so there is no id to page to until the daemon says
    // which one it made — and while that is outstanding the pager is legitimately
    // empty, which is a *different* thing from "there is nothing to open" and must
    // not be shown as the same sentence.
    //
    // Counted, not derived from the session list: the answer is the daemon's
    // `created` message, and a list of sessions cannot say which row is new.
    var pendingOpens by remember { mutableStateOf(0) }

    val attachable = sessions.filter { it.attachable }
    val pages = pageSelection?.let { chosen -> attachable.filter { it.id in chosen } } ?: attachable

    // A shell asked for in a directory joins the selection as soon as it exists.
    // The shell the daemon made joins the pages. This is the whole reason it
    // answers `created`: the client is told, and does not guess.
    val created by client.created.collectAsState()
    val createdTick by client.createdTick.collectAsState()
    LaunchedEffect(createdTick) {
        val ids = created
        if (ids.isEmpty()) return@LaunchedEffect
        // Every id the gesture asked for, not only the last one: the daemon answers
        // a create one at a time and one gesture can ask for several.
        pageSelection = (pageSelection ?: emptySet()) + ids.toSet()
        pendingOpens = (pendingOpens - ids.size).coerceAtLeast(0)
        client.clearCreated()
    }

    // A refusal and a request cannot both be outstanding. The daemon answers a
    // `new` with either `created` or `error`, so an error here is this request
    // coming back empty — and without this the screen would sit on "Opening
    // shell…" for a shell that is never coming.
    LaunchedEffect(errorTick) {
        if (errorTick > 0) pendingOpens = (pendingOpens - 1).coerceAtLeast(0)
    }

    // And a request cannot outlive the connection that carried it. Without this a
    // drop mid-create leaves the count standing forever.
    LaunchedEffect(connected) { if (!connected) pendingOpens = 0 }

    // Stay alive in the background while a terminal is OPEN — deliberately not
    // while *connected*.
    //
    // Keying it on connectivity is a deadlock, and it measured as one on the
    // device: backgrounding the app drops the socket, which cleared `connected`,
    // which stopped the service, which is the only thing that would have kept the
    // process alive to reconnect. The service has to be up *before* the link is
    // lost for the link to survive.
    //
    // Leaving the terminal (back to the launcher) stops it, because nothing is
    // being watched then — and the shells themselves are unharmed either way:
    // they live in tmux on the host, not in this process.
    // The screen's shape, decided in one place and nowhere else: the menu is
    // always portrait (a list is read the way you hold a phone), a terminal is
    // landscape, and portrait-with-a-terminal means *typing* — the compact bar
    // above the keyboard, never this panel.
    val wantPortrait = showLauncher || mode == UiMode.Portrait
    LaunchedEffect(wantPortrait) { lockOrientation(wantPortrait) }

    val terminalOpen = !showLauncher && pages.isNotEmpty()
    LaunchedEffect(terminalOpen, pages.size) {
        BackgroundSession.setActive(
            terminalOpen,
            if (pages.size == 1) "1 shell open" else "${pages.size} shells open",
        )
    }
    // **Leaving the screen lets go of everything, after a grace window.**
    //
    // The alternative — holding the sockets, the wake lock and the claims for as
    // long as the app is installed — costs a radio wakeup every four seconds per
    // open shell and a CPU that never suspends, to keep a *view* of a shell alive.
    // The view is the cheap part: the shell lives in tmux on the host, and a
    // client that reconnects gets a full snapshot of the terminal state by
    // construction. So there is nothing to preserve by staying awake.
    //
    // The grace window is what makes it not annoying: an app switch, a glance at
    // a notification, or answering a message all come back inside it and find
    // everything exactly as it was. Past it the user has gone somewhere, and the
    // honest thing is to be a list of shells when they return.
    //
    // Deliberately the same mechanism as the launcher button: dropping the pages
    // *is* the detach, so there is one way for a shell to be let go of.
    LaunchedEffect(AppLifecycle.foreground) {
        if (AppLifecycle.foreground) return@LaunchedEffect
        delay(BackgroundSession.releaseGraceMs)
        pageSelection = emptySet()
        showLauncher = true
    }

    // Back means the same thing as the launcher button: it leaves the terminal
    // and lets go. It is not a second way to do it — it is the same call.
    BackToLauncher(enabled = terminalOpen) {
        pageSelection = emptySet()
        showLauncher = true
    }

    // Leaving the app entirely disposes this composition, and then the service
    // must go too: a notification saying a shell is open, with nothing behind it,
    // is worse than no notification, and the process would stay alive for no
    // reason.
    DisposableEffect(Unit) {
        onDispose { BackgroundSession.setActive(false, "") }
    }

    // The sticky modifiers live here, not in the panel, because **both** bars send
    // keys and a latch belongs to the session rather than to the chrome that armed
    // it. Kept in the panel it existed twice — and the portrait bar, having no
    // latch of its own, had its Ctrl key wired straight to `C-c`, which is not a
    // modifier at all: pressing Ctrl and then `d` there sent C-c, then a d.
    var modifiers by remember { mutableStateOf(Modifiers()) }

    // One handle per session, so the chrome's keys reach the visible terminal.
    // A snapshot state map, so the panel recomposes once a surface registers.
    val handles = remember { mutableStateMapOf<String, TerminalHandle>() }

    // The one place a key is sent, for both modes, so the latch can neither leak
    // nor be forgotten: every key in either bar goes through here.
    val pagerState = rememberPagerState(pageCount = { pages.size.coerceAtLeast(1) })

    // A mode change is accompanied by a rotation, and the rotation is Android's:
    // the app asks for an orientation and the window turns over on its own. What is
    // *not* available is the end of that turn. The configuration is delivered at
    // the start, so reading the display's own orientation back already answers
    // "portrait" while the user is still watching the flip — which is why a panel
    // gated on that signal appeared mid-turn anyway, and why a signal is the wrong
    // instrument here at all. A beat out of the turn is the honest one: the panel
    // belongs to the new shape, so it waits the turn out and then appears, upright,
    // in place, instead of being drawn into the old window and carried round with
    // it.
    var panelArrived by remember { mutableStateOf(true) }
    var lastWanted by remember { mutableStateOf(wantPortrait) }
    if (lastWanted != wantPortrait) {
        // Written during composition deliberately: an effect would run *after* the
        // frame this is for. Converges — the next pass finds them equal and writes
        // nothing.
        lastWanted = wantPortrait
        panelArrived = false
    }
    LaunchedEffect(wantPortrait) {
        if (!panelArrived) {
            delay(PANEL_FLIP_MS)
            panelArrived = true
        }
    }

    // A latch that survived a glance at another shell would be a latch that
    // surprises you, so anything that ends the gesture clears it.
    LaunchedEffect(pagerState.currentPage) { modifiers = modifiers.cleared() }

    // The one place a key is sent, for both modes, so the latch cannot leak or be
    // forgotten: every key in either bar goes through here, modified or not.
    val press: (String) -> Unit = { key ->
        val handle = pages.getOrNull(pagerState.currentPage)?.id?.let { handles[it] }
        val armed = modifiers
        if (armed.armed) {
            handle?.sendModifiedKey(key, armed)
            modifiers = modifiers.cleared()
        } else {
            handle?.sendKey(key)
        }
    }
    LaunchedEffect(pages.size) {
        if (pages.isNotEmpty() && pagerState.currentPage >= pages.size) {
            pagerState.scrollToPage(pages.size - 1)
        }
    }



    var killTarget by remember { mutableStateOf<Session?>(null) }
    val portrait = mode == UiMode.Portrait
    val currentHandle = pages.getOrNull(pagerState.currentPage)?.id?.let { handles[it] }

    // Portrait *means* typing, so the keyboard belongs to it: if the user
    // dismisses the keyboard we return to landscape rather than sitting in
    // portrait with none. Debounced, because the inset also passes through zero
    // while the activity rotates.
    val density = LocalDensity.current
    val imeInsets = WindowInsets.ime
    var sawIme by remember { mutableStateOf(false) }
    LaunchedEffect(density) {
        snapshotFlow { imeInsets.getBottom(density) }
            .debounce(600)
            .collect { bottom ->
                if (bottom > 0) {
                    sawIme = true
                } else if (sawIme && mode == UiMode.Portrait) {
                    sawIme = false
                    mode = UiMode.Landscape
                }
            }
    }

    // One screen or the other, with a slide between them. The rotation itself is
    // the system's — Android gives an activity one orientation and a view cannot
    // opt out of that — so this does not *replace* the rotation; it keeps the
    // content moving through it, where a cut reads as a glitch.
    AnimatedContent(
        targetState = showLauncher,
        transitionSpec = {
            if (targetState) {
                (slideInHorizontally { -it / 3 } + fadeIn()) togetherWith
                    (slideOutHorizontally { it / 3 } + fadeOut())
            } else {
                (slideInHorizontally { it / 3 } + fadeIn()) togetherWith
                    (slideOutHorizontally { -it / 3 } + fadeOut())
            }
        },
        label = "menu or terminal",
    ) { showingLauncher ->
    if (showingLauncher) {
        var desktopLoading by remember { mutableStateOf(false) }
        // Cleared by the *arrival* of a reading, not by the value changing: two
        // readings a second apart are usually identical, and keying on the value
        // would leave the button saying "…" with the answer already on screen.
        val desktopTick by client.desktopTick.collectAsState()
        LaunchedEffect(desktopTick) { if (desktopTick > 0) desktopLoading = false }
        val liveResult by client.liveResult.collectAsState()
        // A refresh that fails still has to let go of "…" — otherwise a single
        // rejected `desktop` request leaves that button stuck forever, which read
        // as "the three-dot button next to settings that does nothing."
        val serverError by client.serverError.collectAsState()
        LaunchedEffect(errorTick) { if (errorTick > 0) desktopLoading = false }

        // Ask for the machine's desktop when the launcher appears, and again if
        // the link was not up yet when it did. Keyed on `connected` as well as the
        // flag because the question is only answerable while connected, and on the
        // first frame after launch the socket is not open.
        LaunchedEffect(showLauncher, connected) {
            if (showLauncher && connected) {
                desktopLoading = true
                client.requestDesktop()
            }
        }
        Box(Modifier.fillMaxSize().background(Bg).safeDrawingPadding()) {
        LauncherScreen(
            host = config.host,
            sessions = sessions,
            desktop = desktop,
            desktopLoading = desktopLoading,
            onRefresh = {
                desktopLoading = true
                client.requestDesktop()
            },
            onOpen = { picked ->
                // Two kinds of row, one gesture: an existing shell becomes a
                // page, and a row that is only a directory gets a shell opened
                // in it. Opening is asynchronous, so the selection is remembered
                // by directory and resolved when the list comes back.
                val existing = picked.mapNotNull { it.sessionId }.toSet()
                val fresh = picked.filter { it.sessionId == null }
                // The page set is set *before* the requests go out. A create can be
                // answered before this loop finishes, and an answer that arrived
                // first would then be overwritten by the line that sets the page
                // set — the shell would exist with nothing pointing at it.
                pageSelection = existing
                pendingOpens = fresh.size
                for (e in fresh) client.create("shell", e.cwd, e.label)
                showLauncher = false
                mode = UiMode.Landscape
            },
            onKill = { killTarget = it },
            onMakeLive = { session, workspace -> client.makeLive(session.id, workspace) },
            onCloseLive = { session -> client.closeLive(session.id) },
            onNewShell = { path ->
                // Always the host's login shell — there is no command to choose,
                // here or anywhere else a shell is opened. A blank path is not the
                // empty string: it is "wherever the daemon is", which is the
                // host's working directory — the same default every other caller
                // gets.
                client.create("shell", path.ifBlank { null })
                // ...and then *go to it*. Creating a shell and leaving the user on
                // the screen that created it is a shell they cannot reach, which
                // is what "opening a new shell is not working" was.
                pendingOpens = 1
                pageSelection = emptySet()
                showLauncher = false
                mode = UiMode.Landscape
            },
            liveResult = liveResult,
            onDismissLiveResult = { client.clearLiveResult() },
            onSettings = { showSettings = true },
        )
        }
        // What the daemon said, whenever it said no — a bad path in "+ shell",
        // a session that vanished before a close landed, anything the daemon's
        // own catch-all turns into an `error` reply. This used to have nowhere
        // to go at all: the client dropped it on the floor, so a rejected
        // request and an ignored one looked identical.
        serverError?.let { err ->
            LaunchedEffect(errorTick) { delay(4000); if (errorTick > 0) client.clearServerError() }
            Box(Modifier.fillMaxSize().safeDrawingPadding().padding(16.dp), contentAlignment = Alignment.TopCenter) {
                Row(
                    Modifier
                        .clip(RoundedCornerShape(8.dp))
                        .background(Surface)
                        .border(1.dp, Line, RoundedCornerShape(8.dp))
                        .clickable { client.clearServerError() }
                        .padding(horizontal = 12.dp, vertical = 8.dp),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(err.message, color = Fg, fontSize = 12.sp)
                }
            }
        }
        killTarget?.let { ConfirmKill(it, onDismiss = { killTarget = null }) { client.close(it.id); killTarget = null } }
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
    } else if (portrait && panelArrived) {
        Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
            TerminalArea(config, pages, pagerState, handles, typing = true, Modifier.weight(1f).fillMaxWidth(), pendingOpen = pendingOpens > 0)
            var portraitMenu by remember { mutableStateOf(false) }
            PortraitBar(
                handle = currentHandle,
                pages = pages,
                pagerState = pagerState,
                scope = scope,
                onLandscape = { mode = UiMode.Landscape },
                menuExpanded = portraitMenu,
                onMenu = { portraitMenu = true },
                onMenuDismiss = { portraitMenu = false },
                onLauncher = {
                    // The launcher is where you choose what to open, so going
                        // back to it lets go of everything: a page is a *hold* on
                        // a shell, and holding shells you are not looking at is how
                        // the phone keeps a session someone else might want.
                    pageSelection = emptySet()
                    showLauncher = true
                },
                onSettings = { showSettings = true },
                modifiers = modifiers,
                onModifiers = { modifiers = it },
                press = press,
            )
        }
    } else {
        Row(Modifier.fillMaxSize().background(Bg).safeDrawingPadding()) {
            val panel: @Composable () -> Unit = {
                SidePanel(
                    pages,
                    pagerState,
                    scope,
                    client,
                    handles,
                    onFlip = { mode = UiMode.Portrait },
                    onKill = { killTarget = it },
                    onLauncher = {
                        // The launcher is where you choose what to open, so going
                        // back to it lets go of everything: a page is a *hold* on
                        // a shell, and holding shells you are not looking at is how
                        // the phone keeps a session someone else might want.
                    pageSelection = emptySet()
                    showLauncher = true
                },
                    // Named rather than trailing: `press` is a parameter now, and a
                    // trailing lambda would bind to it.
                    onSettings = { showSettings = true },
                    modifiers = modifiers,
                    onModifiers = { modifiers = it },
                    press = press,
                )
            }
            if (config.panelLeft && panelArrived) panel()
            TerminalArea(config, pages, pagerState, handles, typing = false, Modifier.weight(1f).fillMaxHeight(), pendingOpen = pendingOpens > 0)
            if (!config.panelLeft && panelArrived) panel()
        }
    }
    }

    killTarget?.let { target ->
        ConfirmKill(target, onDismiss = { killTarget = null }) {
            // `close`, not `kill`. Closing lets go of a shell; whether that ends
            // it depends on whether a desktop window is holding it open, which
            // only the daemon can see (ADR-0010).
            client.close(target.id)
            killTarget = null
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

/**
 * The terminal itself, laid out the same in both modes. Handles are registered
 * here so the chrome (whichever chrome is showing) can drive the visible one.
 */
@Composable
private fun TerminalArea(
    config: LdmuxConfig,
    pages: List<Session>,
    pagerState: PagerState,
    handles: MutableMap<String, TerminalHandle>,
    typing: Boolean,
    modifier: Modifier,
    // A shell just asked for has not arrived yet (see `pendingOpens` in App()) —
    // that is not "no sessions", it is "one is on its way", and saying the wrong
    // one of those is exactly what read as a bug: the launcher had three things
    // on it, one was tapped, and the terminal screen it landed on flatly denied
    // anything was open.
    pendingOpen: Boolean = false,
) {
    Box(modifier.background(Bg)) {
        if (pages.isEmpty()) {
            Text(
                if (pendingOpen) "Opening a shell…" else "No sessions yet. Tap + shell to start one.",
                color = Dim,
                fontSize = 13.sp,
                modifier = Modifier.align(Alignment.Center).padding(16.dp),
            )
        } else {
            HorizontalPager(state = pagerState, beyondViewportPageCount = 2, modifier = Modifier.fillMaxSize()) { page ->
              // One Box per page, and it is not cosmetic: a pager page stacks its
              // children only if the content is itself a Box. As two siblings the
              // overlay was laid out *after* a fillMaxSize terminal, i.e. below the
              // bottom of the screen — so a refused attach showed a blank terminal
              // and no explanation, which is exactly what it was built to prevent.
              Box(Modifier.fillMaxSize()) {
              // A page index and a list that can change size are two independent
              // things, and the daemon broadcasts the session list on every change:
              // a shell dying can shrink `pages` between the pager deciding to
              // compose a page and this line being read, which is an index out of
              // bounds and a crash on a screen the user did nothing wrong on. A
              // shell going away is a page that stops being drawn.
                val s = pages.getOrNull(page) ?: return@HorizontalPager
                // Composed if it is within a swipe, connected only if it is the
                // one on screen (see TerminalHandle.rememberTerminalSurface).
                val handle = rememberTerminalSurface(
                    config,
                    s.id,
                    typing,
                    active = page == pagerState.currentPage,
                    Modifier.fillMaxSize(),
                )
                LaunchedEffect(handle, s.id) { handles[s.id] = handle }
                // Who has this shell, when it is not us (ADR-0010). Drawn over
                // the terminal rather than replacing the page: the page is still
                // real, it is just somebody else's to type in right now — and a
                // blank terminal with no reason on it is the failure this screen
                // exists to prevent.
                val madeVisitable by handle.ownedBy.collectAsState()
                // Local, so each page has its own answer, and a swipe does not
                // carry one page's decision to another.
                var askTakeOver by remember(s.id) { mutableStateOf(false) }
                if (madeVisitable != null) {
                    HeldElsewhere(
                        holder = madeVisitable!!,
                        confirmed = askTakeOver,
                        onAsk = { askTakeOver = true },
                        onTakeOver = {
                            askTakeOver = false
                            handle.takeOver()
                        },
                    )
                }
              }
            }
        }
    }
}

/**
 * The thin bar that rides directly above the keyboard in portrait. Deliberately
 * sparse — the way back to landscape is the important control, and the rest is
 * only the handful of keys a soft keyboard cannot produce.
 */
/**
 * Ending a shell throws away whatever is running in it, so it is always
 * confirmed — and named, so the wrong one cannot be closed by reflex.
 */
@Composable
fun ConfirmKill(target: Session, onDismiss: () -> Unit, onConfirm: () -> Unit) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Close this shell?", color = Fg) },
        containerColor = Surface,
        text = { Text(target.label, color = Dim, fontSize = 12.sp) },
        confirmButton = {
            TextButton(onClick = onConfirm) { Text("close", color = Color(0xFFE08A8A)) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}

@Composable
private fun PortraitBar(
    handle: TerminalHandle?,
    pages: List<Session>,
    pagerState: PagerState,
    scope: CoroutineScope,
    onLandscape: () -> Unit,
    onMenu: () -> Unit,
    menuExpanded: Boolean,
    onMenuDismiss: () -> Unit,
    onLauncher: () -> Unit,
    onSettings: () -> Unit,
    modifiers: Modifiers,
    onModifiers: (Modifiers) -> Unit,
    press: (String) -> Unit,
) {
    Column(
        Modifier.fillMaxWidth().background(Surface).padding(horizontal = 6.dp, vertical = 5.dp),
        verticalArrangement = Arrangement.spacedBy(5.dp),
    ) {
        // Where you are, and how to move, at the top of the bar: the same row the
        // side panel shows, because moving between shells must not mean leaving
        // them. Navigation at one end, keys in the middle, actions at the other.
        ShellHeader(pages, pagerState, scope, onLauncher, onSettings, showMenu = false)

        PanelDivider()

        // Two rows, each sharing the width. One scrolling row hid half its keys
        // off the edge, which is the one thing a keyboard-adjacent strip must not
        // do: a key you cannot see is a key that does not exist.
        //
        // Nothing here is a keyboard glyph, and nothing here is a shell button: a
        // new shell belongs to the menu, where you are choosing one, not to the
        // bar you type on.
        // The menu closes the top row and Enter closes the bottom one — swapped
        // at the user's word. The menu is how you leave the keyboard, so it belongs
        // at the far corner of the row above the typing keys rather than inside it,
        // and Enter belongs with the keys that feed a line rather than alone at the
        // top. The panel keeps the same arrangement.
        PortraitKeyRow(
            listOf(
                "Esc" to { press("Escape") },
                "Tab" to { press("Tab") },
                "\u2190" to { press("Left") },
                "\u2191" to { press("Up") },
                "\u2193" to { press("Down") },
                "\u2192" to { press("Right") },
            ),
            flip = onLandscape,
            menu = onMenu,
            menuExpanded = menuExpanded,
            onMenuDismiss = onMenuDismiss,
            onLauncher = onLauncher,
            onSettings = onSettings,
        )
        PortraitKeyRow(
            listOf(
                // The same latch as the panel's, from the same state: arm Ctrl,
                // then press any key, and Ctrl applies to that key. It used to be
                // wired straight to `C-c` here, which sent an interrupt instead of
                // arming anything — so Ctrl-then-`d` sent C-c and then a d.
                "Ctrl" to { onModifiers(modifiers.toggled(StickyKey.Ctrl)) },
                "Alt" to { onModifiers(modifiers.toggled(StickyKey.Alt)) },
                controlLabel("C-c") to { press("C-c") },
                controlLabel("C-d") to { press("C-d") },
                "|" to { press("|") },
                "-" to { press("-") },
                "/" to { press("/") },
                "\u23ce" to { press("Enter") },
            ),
            active = listOf(modifiers.ctrl, modifiers.alt, false, false, false, false, false, false),
        )
        // No microphone: the keyboard is always up in portrait and it has its own.
        // Two microphones is one too many.
    }
}

/** One row of the portrait bar: the flip control, then keys sharing the width. */
@Composable
private fun PortraitKeyRow(
    keys: List<Pair<String, () -> Unit>>,
    flip: (() -> Unit)? = null,
    menu: (() -> Unit)? = null,
    menuExpanded: Boolean = false,
    onMenuDismiss: () -> Unit = {},
    onLauncher: () -> Unit = {},
    onSettings: () -> Unit = {},
    active: List<Boolean> = emptyList(),
) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (flip != null) {
            // The way back to landscape, first: it is the control that leaves this
            // mode rather than one that types into it.
            IconBtn(onClick = flip, modifier = Modifier.weight(1f)) { IconFlip(Accent, box = 18.dp) }
        }
        keys.forEachIndexed { i, (label, action) ->
            PortraitKey(label, Modifier.weight(1f), active.getOrElse(i) { false }, action)
        }
        if (menu != null) {
            Box(Modifier.weight(1f)) {
                IconBtn(onClick = menu, modifier = Modifier.fillMaxWidth(), filled = true) {
                    IconLauncher(Bg, box = 18.dp)
                }
                AppMenu(menuExpanded, onMenuDismiss, onLauncher, onSettings)
            }
        }
    }
}

@Composable
private fun PortraitKey(
    label: String,
    modifier: Modifier,
    active: Boolean,
    onClick: () -> Unit,
) {
    Box(
        modifier
            .clip(RoundedCornerShape(7.dp))
            .background(if (active) Accent else Bg)
            .clickable(onClick = onClick)
            .padding(vertical = 9.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
    }
}

@Composable
private fun TinyBtn(label: String, onClick: () -> Unit) {
    Box(
        Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(Bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 8.dp, vertical = 4.dp),
    ) {
        Text(label, color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
    }
}

/**
 * Where you are, and how to move — one row, in both modes.
 *
 * Landscape puts it at the top of the side panel and portrait puts it at the top
 * of the bar above the keyboard. It is the same control doing the same thing:
 * leave this screen, then back and forward between the shells you are holding,
 * then the title.
 *
 * Portrait had no such row at all, so the only way to change shells was the
 * launcher — and the launcher lets go of everything, because it is where you
 * choose what to open. So changing shells in portrait was detach-everything and
 * attach-one, which is not a way to move between two shells you are holding. The
 * two modes cannot disagree about this.
 *
 * The title is the folder, not the path — see [folderName] — and the count is a
 * subtitle rather than a second row, because "2 / 3" is not a heading.
 *
 * [showMenu] is false in portrait, where the bar's own row already ends in the
 * same menu button. The menu behind it is the same [AppMenu] either way.
 */
@Composable
private fun ShellHeader(
    pages: List<Session>,
    pagerState: PagerState,
    scope: CoroutineScope,
    onLauncher: () -> Unit,
    onSettings: () -> Unit,
    showMenu: Boolean = true,
) {
    val current = pagerState.currentPage
    val total = pages.size.coerceAtLeast(1)
    val here = pages.getOrNull(current)
    var menuOpen by remember { mutableStateOf(false) }

    fun step(delta: Int) {
        val target = (current + delta).coerceIn(0, (total - 1).coerceAtLeast(0))
        if (target != current) scope.launch { pagerState.animateScrollToPage(target) }
    }

    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (showMenu) {
            Box {
                IconBtn(onClick = { menuOpen = true }, filled = true) {
                    IconLauncher(Bg)
                }
                AppMenu(
                    expanded = menuOpen,
                    onDismiss = { menuOpen = false },
                    onLauncher = onLauncher,
                    onSettings = onSettings,
                )
            }
        }
        NavArrow("\u25c0", enabled = current > 0) { step(-1) }
        Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                folderName(here?.cwd ?: ""),
                color = Fg,
                fontFamily = FontFamily.Monospace,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                if (total > 1) "${current + 1} / $total shells" else "1 shell",
                color = Dim,
                fontFamily = FontFamily.Monospace,
                fontSize = 9.sp,
                maxLines = 1,
            )
        }
        NavArrow("\u25b6", enabled = current < total - 1) { step(1) }
    }
}

/**
 * The side panel (landscape only). No session list: just back/forward between
 * the open shells, the title of the current one, the keys, and the few actions.
 */
@Composable
private fun SidePanel(
    pages: List<Session>,
    pagerState: PagerState,
    scope: CoroutineScope,
    client: LdmuxClient,
    handles: Map<String, TerminalHandle>,
    onFlip: () -> Unit,
    onKill: (Session) -> Unit,
    onLauncher: () -> Unit,
    onSettings: () -> Unit,
    modifiers: Modifiers,
    onModifiers: (Modifiers) -> Unit,
    press: (String) -> Unit,
) {
    val current = pagerState.currentPage
    val here = pages.getOrNull(current)
    val handle = here?.id?.let { handles[it] }
    val total = pages.size.coerceAtLeast(1)

    Column(
        Modifier.width(PANEL_WIDTH).fillMaxHeight().background(Surface)
            .padding(horizontal = 8.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        ShellHeader(pages, pagerState, scope, onLauncher, onSettings)

        PanelDivider()

        @OptIn(ExperimentalLayoutApi::class)
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            // One row per group, each row sharing the panel's width between its
            // keys. Keys used to be chips sized to their own text, which made a
            // ragged column where "C-d" and "|" had the same weight and nothing
            // lined up with anything. A row that fills the width is easier to hit
            // and, more to the point, says how many keys are in it.
            KeyRow(
                listOf(
                    "Ctrl" to { onModifiers(modifiers.toggled(StickyKey.Ctrl)) },
                    "Alt" to { onModifiers(modifiers.toggled(StickyKey.Alt)) },
                    "copy" to { handle?.setCopyMode(handle?.inCopyMode != true) },
                ),
                active = listOf(modifiers.ctrl, modifiers.alt, handle?.inCopyMode == true),
            )
            PanelDivider()
            KeyRow(
                listOf(
                    "Esc" to { press("Escape") },
                    "Tab" to { press("Tab") },
                    "⏎" to { press("Enter") },
                ),
            )
            PanelDivider()
            KeyRow(
                listOf(
                    "←" to { press("Left") },
                    "↑" to { press("Up") },
                    "↓" to { press("Down") },
                    "→" to { press("Right") },
                ),
            )
            PanelDivider()
            // The six things a soft keyboard cannot produce and a shell wants
            // most: two control keys, and the four characters that are not on the
            // number row where they can be reached.
            KeyRow(
                listOf(
                    controlLabel("C-c") to { press("C-c") },
                    controlLabel("C-d") to { press("C-d") },
                    "|" to { press("|") },
                    "-" to { press("-") },
                    "/" to { press("/") },
                ),
            )
        }

        // Everything that acts on the *app* lives in this one bar, and nothing
        // that acts on the shell does. That is the whole rule: the middle of the
        // panel scrolls and holds keys, the ends are fixed and hold navigation
        // and actions. It used to be neither — a flip button above the scroll, a
        // settings cog below it, and no way to tell which was which.
        Row(
            Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // One flip control, one meaning, in both orientations: portrait is
            // where the keyboard lives, so flipping to it *is* the keyboard
            // action, and a separate keyboard button only confuses it.
            IconBtn(onClick = onFlip) { IconFlip(Accent) }
            // Landscape only: portrait always has the keyboard up and the IME
            // carries its own microphone. Two microphones is one too many.
            IconBtn(onClick = { handle?.startVoice() }) { IconMic(tint = Fg) }
        }

        // Diagnostics, not a control: the repaint cadence follows the round trip,
        // so this is the reason a slow link feels the way it does.
        val link by (handle?.link ?: TerminalHandle.EMPTY_LINK).collectAsState()
        if (link.measured) {
            Text(
                "link ${link.rttMs} ms · ${link.fps} fps",
                color = Dim.copy(alpha = 0.7f),
                fontFamily = FontFamily.Monospace,
                fontSize = 9.sp,
            )
        }
    }
}

/** A button that carries the accent, for the single primary action. */
@Composable
fun AccentBtn(label: String, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .clip(RoundedCornerShape(8.dp))
            .background(Accent)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = Bg, fontSize = 13.sp)
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
private fun KeyBtn(
    label: String,
    modifier: Modifier = Modifier,
    active: Boolean = false,
    onClick: () -> Unit,
) {
    // Padding is only breathing room now. It was sized for a key that measured
    // itself — where a single-glyph key had to be told to carry less margin than
    // a word key — and it no longer is: the row divides the width and the key
    // takes its share. Keeping a word key's margin here is what clipped "C-c" to
    // "C-", because the margin came out of the label's own space.
    val pad = 3.dp
    Box(
        modifier
            .clip(RoundedCornerShape(6.dp))
            .background(if (active) Accent else Bg)
            .clickable(onClick = onClick)
            .padding(horizontal = pad, vertical = 9.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            color = if (active) Bg else Fg,
            fontFamily = FontFamily.Monospace,
            // Three-character labels get a point less: they are the widest thing
            // in a five-key row, and a point of type is cheaper than a key that
            // clips or a row that wraps.
            fontSize = if (label.length > 1) 11.sp else 12.sp,
            maxLines = 1,
            softWrap = false,
        )
    }
}

@Composable
private fun SettingsDialog(current: LdmuxConfig, onDismiss: () -> Unit, onSave: (LdmuxConfig) -> Unit) {
    var host by remember { mutableStateOf(current.host) }
    var token by remember { mutableStateOf(current.token) }
    var side by remember { mutableStateOf(current.side) }
    var predict by remember { mutableStateOf(current.predict) }
    var fontSp by remember { mutableStateOf(current.fontSp) }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Settings", color = Fg) },
        containerColor = Surface,
        // Material's dialogs mix their background from `primary` by default, which
        // is how a pure grey card picked up a violet cast on a black screen.
        tonalElevation = 0.dp,
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
                Box(Modifier.height(6.dp))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("predictive echo", color = Dim, fontSize = 12.sp)
                    TextButton(onClick = { predict = false }) {
                        Text("off", color = if (!predict) Accent else Dim)
                    }
                    TextButton(onClick = { predict = true }) {
                        Text("on", color = if (predict) Accent else Dim)
                    }
                }
                Text(
                    // Said once, in the shortest form that is still true. A setting
                    // is a thing you already decided to change; it does not need the
                    // reasoning that made you open this.
                    "text appears before the server echoes it",
                    color = Dim,
                    fontSize = 10.sp,
                )
                Box(Modifier.height(10.dp))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("text size", color = Dim, fontSize = 12.sp)
                    TextButton(onClick = { fontSp = (fontSp - 1f).coerceAtLeast(LdmuxConfig.MIN_FONT_SP) }) {
                        Text("−", color = Accent, fontSize = 16.sp)
                    }
                    Text("${fontSp.toInt()}", color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
                    TextButton(onClick = { fontSp = (fontSp + 1f).coerceAtMost(LdmuxConfig.MAX_FONT_SP) }) {
                        Text("+", color = Accent, fontSize = 16.sp)
                    }
                }
                Text(
                    "bigger text means fewer columns; the shell is resized to match",
                    color = Dim,
                    fontSize = 10.sp,
                )
            }
        },
        confirmButton = {
            TextButton(onClick = {
                onSave(
                    LdmuxConfig(
                        host.trim(),
                        token.trim(),
                        side,
                        predict,
                        fontSp.coerceIn(LdmuxConfig.MIN_FONT_SP, LdmuxConfig.MAX_FONT_SP),
                    ),
                )
            }) {
                Text("save", color = Accent)
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}

/**
 * "Another device is using this shell."
 *
 * Shown over the terminal when the daemon refuses an attach, because one device
 * owns a shell at a time (ADR-0010). It is deliberately two steps: the first says
 * who has it, and only the second takes it. Taking it makes the *other* device
 * show the same kind of screen, so doing that on a single tap would let a pocket
 * tap evict someone mid-command.
 */
@Composable
private fun BoxScope.HeldElsewhere(
    holder: String,
    confirmed: Boolean,
    onAsk: () -> Unit,
    onTakeOver: () -> Unit,
) {
    Box(
        Modifier.fillMaxSize().background(Bg.copy(alpha = 0.96f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text("open on $holder", color = Fg, fontSize = 15.sp)
            Spacer(Modifier.height(6.dp))
            Text(
                if (confirmed) {
                    // True since ownership moves the shell rather than copying it:
                    // taking it back closes the other device's view of it.
                    "this closes $holder's view of it"
                } else {
                    "one device at a time"
                },
                color = Dim,
                fontSize = 12.sp,
                textAlign = TextAlign.Center,
            )
            Spacer(Modifier.height(14.dp))
            if (confirmed) {
                AccentBtn("take it over", Modifier.width(220.dp)) { onTakeOver() }
            } else {
                AccentBtn("take it over…", Modifier.width(220.dp)) { onAsk() }
            }
        }
    }
}

/**
 * One row of keys sharing the width between them.
 *
 * `weight(1f)` each rather than sized to their own text: a row of equally wide
 * targets is easier to hit without looking, and it reads as a set — which is what
 * `C-c` and `C-d` are and what a ragged column of chips was not.
 */
@Composable
private fun KeyRow(keys: List<Pair<String, () -> Unit>>, active: List<Boolean> = emptyList()) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        keys.forEachIndexed { i, (label, action) ->
            KeyBtn(label, Modifier.weight(1f), active.getOrElse(i) { false }) { action() }
        }
    }
}

/**
 * The overflow menu: the few things that act on the *app*.
 *
 * One composable for both modes, because it is the same three actions in both and
 * a second copy would be a second place for them to drift apart. Wrapped in a
 * dark scheme because Material components arrive wearing Material's default
 * *light* theme, and a white rectangle in a dark app is what that looks like.
 */
@Composable
fun AppMenu(
    expanded: Boolean,
    onDismiss: () -> Unit,
    onLauncher: () -> Unit,
    onSettings: () -> Unit,
) {
    MaterialTheme(
        colorScheme = darkColorScheme(
            surface = Surface,
            onSurface = Fg,
            onSurfaceVariant = Dim,
            primary = Accent,
        ),
    ) {
        DropdownMenu(
            expanded = expanded,
            onDismissRequest = onDismiss,
            // Two Material defaults have to go, and both for the same reason: this
            // app has no hue. The container is `surfaceContainer` (a purple-tinted
            // grey), and Material then paints a *tonal* elevation over it, which in
            // a dark scheme is an overlay mixed from `primary` — the menu came out
            // faintly violet against a screen that is black and grey.
            containerColor = Surface,
            tonalElevation = 0.dp,
        ) {
            DropdownMenuItem(
                text = { Text("shells", color = Fg) },
                onClick = { onDismiss(); onLauncher() },
            )
            DropdownMenuItem(
                text = { Text("settings", color = Fg) },
                onClick = { onDismiss(); onSettings() },
            )
        }
    }
}

