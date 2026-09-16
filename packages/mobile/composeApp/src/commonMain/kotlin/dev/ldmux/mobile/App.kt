package dev.ldmux.mobile

import androidx.compose.foundation.background
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
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.launch

/** The whole palette. Minimal, dark, one accent — no decoration. */
val Bg = Color(0xFF0B0E14)
val Fg = Color(0xFFE6E6E6)
val Dim = Color(0xFF8A8F98)
val Accent = Color(0xFF4C8DFF)
val Surface = Color(0xFF141822)

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

    // The launcher comes first: a terminal is somewhere you go, and which shell
    // you are going to is a question worth asking before you are in one.
    var showLauncher by remember { mutableStateOf(true) }
    // Which shells the pager pages through. `null` means all of them; the
    // launcher sets a set when the user chooses, and that set is what ◀ ▶ walk.
    var pageSelection by remember { mutableStateOf<Set<String>?>(null) }
    // Directories a shell has been asked for and not yet seen. A chosen pi session
    // has no ldmux shell yet, so its page cannot be named by id until the daemon
    // reports the new one; until then the pager is briefly empty rather than
    // falling back to every shell on the host, which would ignore the choice.
    var awaitingCwds by remember { mutableStateOf(setOf<String>()) }

    val attachable = sessions.filter { it.attachable }
    val pages = pageSelection?.let { chosen -> attachable.filter { it.id in chosen } } ?: attachable

    // A shell asked for in a directory joins the selection as soon as it exists.
    LaunchedEffect(sessions, awaitingCwds) {
        if (awaitingCwds.isEmpty()) return@LaunchedEffect
        val arrived = attachable.filter { it.cwd in awaitingCwds }
        if (arrived.isNotEmpty()) {
            pageSelection = (pageSelection ?: emptySet()) + arrived.map { it.id }
            awaitingCwds = awaitingCwds - arrived.map { it.cwd }.toSet()
        }
    }

    val pagerState = rememberPagerState(pageCount = { pages.size.coerceAtLeast(1) })
    LaunchedEffect(pages.size) {
        if (pages.isNotEmpty() && pagerState.currentPage >= pages.size) {
            pagerState.scrollToPage(pages.size - 1)
        }
    }

    // One handle per session, so the chrome's keys reach the visible terminal.
    // A snapshot state map, so the panel recomposes once a surface registers.
    val handles = remember { mutableStateMapOf<String, TerminalHandle>() }
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

    if (showLauncher) {
        var desktopLoading by remember { mutableStateOf(false) }
        // Cleared by the *arrival* of a reading, not by the value changing: two
        // readings a second apart are usually identical, and keying on the value
        // would leave the button saying "…" with the answer already on screen.
        val desktopTick by client.desktopTick.collectAsState()
        LaunchedEffect(desktopTick) { if (desktopTick > 0) desktopLoading = false }
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
            onOpen = { choices ->
                val ids = choices.mapNotNull { (it as? LauncherChoice.Session)?.id }.toSet()
                val fresh = choices.filterIsInstance<LauncherChoice.NewShell>()
                for (c in fresh) client.create("shell", c.cwd, c.label)
                awaitingCwds = fresh.map { it.cwd }.toSet()
                pageSelection = ids
                showLauncher = false
                mode = UiMode.Landscape
            },
            onKill = { killTarget = it },
            onSettings = { showSettings = true },
        )
        }
        killTarget?.let { ConfirmKill(it, onDismiss = { killTarget = null }) { client.kill(it.id); killTarget = null } }
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
        return
    }

    if (portrait) {
        Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
            TerminalArea(config, pages, pagerState, handles, typing = true, Modifier.weight(1f).fillMaxWidth())
            PortraitBar(currentHandle, onLandscape = { mode = UiMode.Landscape })
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
                    onLauncher = { showLauncher = true },
                ) { showSettings = true }
            }
            if (config.panelLeft) panel()
            TerminalArea(config, pages, pagerState, handles, typing = false, Modifier.weight(1f).fillMaxHeight())
            if (!config.panelLeft) panel()
        }
    }

    killTarget?.let { target ->
        ConfirmKill(target, onDismiss = { killTarget = null }) {
            client.kill(target.id)
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
) {
    Box(modifier.background(Bg)) {
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
            TextButton(onClick = onConfirm) { Text("close", color = Color(0xFFFF6B6B)) }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}

@Composable
private fun PortraitBar(handle: TerminalHandle?, onLandscape: () -> Unit) {
    Row(
        Modifier.fillMaxWidth().background(Surface).padding(horizontal = 6.dp, vertical = 5.dp),
        horizontalArrangement = Arrangement.spacedBy(7.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // The way back to landscape, drawn: two arrows going round, the same
        // control as the panel's. Nothing here is a keyboard glyph.
        IconBtn(onClick = onLandscape) { IconFlip(Accent, box = 20.dp) }
        TinyBtn("Esc") { handle?.sendKey("Escape") }
        TinyBtn("Tab") { handle?.sendKey("Tab") }
        TinyBtn("◀") { handle?.sendKey("Left") }
        TinyBtn("▲") { handle?.sendKey("Up") }
        TinyBtn("▼") { handle?.sendKey("Down") }
        TinyBtn("▶") { handle?.sendKey("Right") }
        // Reading history must not be a trap: one tap back to the live prompt.
        TinyBtn("⤓") { handle?.jumpToBottom() }
        Spacer(Modifier.weight(1f))
        TinyBtn("C-c") { handle?.sendKey("C-c") }
        // No microphone here: the keyboard is always up in portrait, and it has
        // its own mic. Two microphones is one too many.
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
) {
    val current = pagerState.currentPage
    val here = pages.getOrNull(current)
    val handle = here?.id?.let { handles[it] }
    val total = pages.size.coerceAtLeast(1)

    fun step(delta: Int) {
        val target = (current + delta).coerceIn(0, (total - 1).coerceAtLeast(0))
        if (target != current) scope.launch { pagerState.animateScrollToPage(target) }
    }

    Column(
        Modifier.width(PANEL_WIDTH).fillMaxHeight().background(Surface)
            .padding(horizontal = 8.dp, vertical = 10.dp),
        verticalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        // Where you are, and how to move. There is no session list by design, so
        // this is the only navigation — which is why it gets the widest row and
        // the biggest touch targets in the panel rather than being strung across
        // a title. The path is tildified because the tail of it is the only part
        // that tells two shells apart.
        Row(verticalAlignment = Alignment.CenterVertically) {
            NavArrow("◀", enabled = current > 0) { step(-1) }
            Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                Text(
                    tildify(here?.cwd ?: ""),
                    color = Fg,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 11.sp,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Text(
                    "${current + 1} / $total",
                    color = Dim,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 9.sp,
                )
            }
            NavArrow("▶", enabled = current < total - 1) { step(1) }
        }

        PanelDivider()

        // Controls that change the *app*, not the shell. Kept in their own group
        // with a rule above, because the flip button used to sit first in the key
        // row wearing a glyph that read as a keyboard — it looked like a key.
        //
        // One flip control, one meaning, in both orientations: portrait is where
        // the keyboard lives, so flipping to it *is* the keyboard action, and
        // there is no separate keyboard button to confuse it with.
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            IconBtn(onClick = onFlip) { IconFlip(Accent) }
            // Landscape only: portrait always has the keyboard up and the IME
            // carries its own microphone. Two microphones is one too many.
            IconBtn(onClick = { handle?.startVoice() }) { IconMic(tint = Fg) }
        }

        PanelDivider()

        @OptIn(ExperimentalLayoutApi::class)
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            // One wrapping flow per group, rather than one flow for everything or
            // hand-laid rows. Hand-laid rows clipped the last key of the widest
            // group off the panel's edge; a single flow split the arrow cluster
            // across two lines. This keeps each group together and lets only the
            // one that needs to wrap do so.
            SIDEBAR_KEYS.forEach { group ->
                FlowRow(
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                    verticalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    group.forEach { (label, key) -> KeyBtn(label) { handle?.sendKey(key) } }
                }
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                // Words, not glyphs: `⤒`/`↓` and the cursor arrows both render as
                // arrows in a monospace fallback, so the two pairs were
                // indistinguishable. These two do different things (page in
                // history vs. return to the live prompt) and must read as such.
                KeyBtn("hist") { handle?.loadHistory() }
                KeyBtn("live") { handle?.jumpToBottom() }
            }
        }

        // Actions, at the bottom where a thumb reaches them: a new shell is the
        // primary one, so it is the only button that gets the accent.
        Row(
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            AccentBtn("+ shell", Modifier.weight(1f)) { client.create("shell") }
            // A way back to the menu. The terminal is somewhere you went, so
            // leaving it has to be possible without ending anything.
            IconBtn(onClick = onLauncher) { IconLauncher(Dim) }
            IconBtn(onClick = onSettings) { IconCog(Dim) }
            val closable = pages.getOrNull(current)
            if (closable != null) {
                IconBtn(onClick = { onKill(closable) }) { IconClose(Dim) }
            }
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
private fun KeyBtn(label: String, active: Boolean = false, onClick: () -> Unit) {
    // Single-glyph keys are given less padding than word keys. They are one
    // character wide carrying two characters' worth of margin, which is enough to
    // push the symbol group past the panel's edge and wrap its last key onto a
    // line of its own.
    val pad = if (label.length == 1) 4.dp else 8.dp
    Box(
        Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(if (active) Accent else Bg)
            .clickable(onClick = onClick)
            .padding(horizontal = pad, vertical = 5.dp),
    ) {
        Text(
            label,
            color = if (active) Bg else Fg,
            fontFamily = FontFamily.Monospace,
            fontSize = 12.sp,
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
                    "on: typed text appears instantly, before the server echoes it. " +
                        "A prompt that does not echo (a password) stops prediction until the next line.",
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
                    "a bigger font means fewer columns, and the shell is resized to match — " +
                        "nothing is scaled down to fit. Pinch on the terminal to change it too.",
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
