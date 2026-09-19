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
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import dev.ldmux.mobile.voice.Dictation
import dev.ldmux.mobile.voice.VoiceState
import dev.ldmux.mobile.voice.VoiceBanner
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.material3.LocalTextStyle
import androidx.compose.ui.text.PlatformTextStyle
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.em
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

/**
 * The outline of a control that is *chosen*.
 *
 * Deliberately not [Accent]. A checkmark outline is a state, not an action, and at
 * `#D0D0D0` it read as the brightest thing on the launcher — brighter than the rows
 * it was describing. This is one clear step down: still unambiguous, no longer
 * competing with the one primary action on the screen.
 */
val Chosen = Color(0xFF9A9A9A)

/**
 * The fill for a control that is *on*: the menu button that says you are in a shell, and
 * the mark that says a window is out on the desktop.
 *
 * It used to be [Accent], the brightest value in the palette, so the menu button was a
 * near-white block in an app whose whole idea is that importance lives in weight and
 * contrast rather than in a hue. That reads as a light rather than as a state, and it
 * put the brightest thing in sight in the corner of every screen.
 *
 * A raised tone says the same thing without shouting. Deliberately not *barely* a
 * difference: `Bg` is #0A0A0A and this is #2E2E2E, which is better than four times the
 * luminance of the button beside it, so it reads as raised at a glance and still leaves
 * [Accent] meaning the one primary action on a screen.
 */
val Raised = Color(0xFF2E2E2E)
val Surface = Color(0xFF1A1A1A)

/**
 * The palette, as Material sees it.
 *
 * A scheme existed before this, but it was built inline inside `AppMenu` and wrapped
 * only that menu, so it was the single place in the app where Material's defaults
 * applied and everywhere else took a platform fallback. The same scheme, named once
 * and hoisted to the root.
 */
internal val LdmuxColors = darkColorScheme(
    surface = Surface,
    onSurface = Fg,
    onSurfaceVariant = Dim,
    background = Bg,
    onBackground = Fg,
    primary = Accent,
)

/**
 * The smallest a control may be, in either direction.
 *
 * Android's minimum touch target is 48dp and nothing in this app specified one: every
 * control's height was whatever its padding and its type happened to add up to, which
 * came to 24-34dp. That is small enough to miss, and it is the other half of why the
 * same control felt different in two places — the padding differed and nothing else
 * did. Stated once, applied by every control in this file.
 */
/**
 * The text style every `Text` in the app starts from, and the reason the app looked
 * thick.
 *
 * This exists because of a measurement. A launcher row came out **71.7dp** tall for two
 * lines of text, and the arithmetic says why. Material's `LocalTextStyle` is
 * `bodyLarge`, whose line height is **24sp** -- and a `Text` that asks for `fontSize =
 * 11.sp` overrides the size and not the line height. So an 11sp subtitle was reserving
 * 24sp of vertical space, plus Android's font padding of about 4dp per line, which
 * comes to 56dp for two lines before the row's own padding adds anything.
 *
 * That is the whole of it, and no control-height setting could have fixed it. A minimum
 * height only binds when the content is *smaller* than it, and this content was larger.
 * Every density control in the app was fighting a line height nobody chose, which is why
 * turning the setting on moved six call sites and nothing you could see.
 *
 * Both halves are fixed here and nowhere else:
 *
 * - `lineHeight` in `em`, so it is proportional to the size actually asked for rather
 *   than to a style nobody selected. `1.2` is above the ~1.17 a font's own ascent and
 *   descent usually take, so nothing clips, and far below 24sp.
 * - no font padding, which is a platform extra of a few dp per line that exists to
 *   leave room for a font's ascent and descent rather than for text a person reads.
 */
internal val BodyStyle = TextStyle(
    lineHeight = 1.2.em,
    platformStyle = PlatformTextStyle(includeFontPadding = false),
)

internal val MinTouch = 48.dp

/**
 * The one row that gets less than [MinTouch], and only in portrait.
 *
 * Portrait's header sits above two rows of keys that are themselves above the
 * keyboard, so it is the only chrome in the app stacked three deep. It is also
 * navigation rather than typing, and it is wide rather than tall: each arrow is
 * about 44dp across, so the target stays comfortable while the row stops competing
 * with the keys below it. Landscape keeps [MinTouch], where there is room and the
 * same row sits at the top of a panel with nothing above it.
 */
internal val DenseTouch = 34.dp

/**
 * How tall a control is, decided once, for the whole app.
 *
 * Both settings are real. 48dp is the platform's minimum touch target and the app
 * went a long time without one, at 24-34dp, which is small enough to miss; it is
 * also a lot of a phone screen to spend on chrome. Only the person holding the
 * phone knows which they would rather have, so it is a setting rather than a
 * judgement call made in a commit.
 */
internal data class Touch(
    /**
     * Minimum height of a control.
     *
     * This only binds when a control's own content is smaller than it, and for a long
     * time nothing was: every `Text` in the app reserved Material's 24sp line height
     * whatever size it asked for, so every control was already taller than any floor
     * and this number was decoration. With the line height fixed, the floor finally
     * does the work it was written to do.
     */
    val min: Dp,
    /** The one row that may be shorter than [min]. */
    val dense: Dp,
    /**
     * Vertical padding inside a control.
     *
     * The second half of the same story. Padding generous enough to set the height by
     * itself makes [min] irrelevant, so it has to move with the density too, or the
     * thin setting is a setting that changes only the controls that did not need
     * changing.
     */
    val pad: Dp,
    /** Vertical padding inside a list row, which is where a row's thickness comes from. */
    val rowPad: Dp,
    /**
     * The drawn size of a glyph inside a control.
     *
     * A floor cannot shrink content that is already bigger than it, so the one way to
     * get a control below its floor is for the mark inside it to be smaller. This is
     * that lever, and it is why the portrait menu and flip buttons -- the two that
     * carry a 18dp mark rather than the 20-22dp defaults -- are the ones that move.
     */
    val icon: Dp,
    /** The launcher's checkbox. Its tick is drawn from it, so the two move together. */
    val check: Dp,
)

/** The two densities, named once so neither is a pair of literals in two places. */
private val ThinTouch = Touch(min = 34.dp, dense = 28.dp, pad = 5.dp, rowPad = 4.dp, icon = 18.dp, check = 16.dp)
private val TallTouch = Touch(min = MinTouch, dense = DenseTouch, pad = 8.dp, rowPad = 8.dp, icon = 22.dp, check = 18.dp)

internal fun touchFor(compact: Boolean): Touch = if (compact) ThinTouch else TallTouch

/**
 * Corner radii, in dp, named once.
 *
 * Small on purpose. A 2–3 dp radius reads as a crisp edge with the corner taken off,
 * which is what a dense terminal UI wants; the 8 and 12 dp corners it replaces read as
 * soft buttons, and at these sizes the curve is a bigger visual event than the border
 * it belongs to. Anything larger than these is a modal, and says so by name.
 */
internal val RadiusSmall = RoundedCornerShape(2.dp)
internal val RadiusMedium = RoundedCornerShape(3.dp)
internal val RadiusLarge = RoundedCornerShape(4.dp)

/**
 * The app's touch scale, installed once at the root and read by every control.
 *
 * A composition local rather than a parameter because the alternative is threading
 * a `Dp` through every composable between the setting and the button, and a
 * parameter on thirty call sites is how a setting ends up reaching most of the
 * controls and not all of them. `static` is deliberate: nothing reads this *while*
 * composing in a way that would need tracking, and the value changes about once a
 * year, so a full recomposition of the subtree on change is exactly right.
 */
internal val LocalTouch = staticCompositionLocalOf { ThinTouch }
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
     * Thin chrome, and this is the default.
     *
     * Every control in the app has one height, and that height is a real decision with
     * two defensible answers, so it is the user's rather than mine. Thin by default: a
     * terminal is a thing you read, and a row of chrome is a row of output you cannot
     * see. Set it false and every control gets the platform's 48dp minimum target,
     * which is what a thumb wants.
     *
     * One switch for the whole app rather than one per control, because the failure
     * that made it a decision was exactly that: the same control came out a different
     * height in two places, and neither of them was chosen.
     */
    val compact: Boolean = true,
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

/**
 * How long a panel waits for the window to finish turning over before it appears.
 *
 * The rotation is Android's and it has already begun by the time the app can ask
 * about it — see the note in `App` — so this is a beat out of the turn itself
 * rather than a wait on a signal that does not exist.
 */
private const val PANEL_FLIP_MS = 200L

/**
 * The app, inside one theme.
 *
 * It had no root theme at all. `MaterialTheme` appeared exactly once in this whole
 * package, wrapped around the overflow menu's `DropdownMenu` — so that menu was the
 * only place in the app where a Material default applied, and every other control
 * took `LocalIndication` (and every other Material composition local) from a platform
 * fallback that nothing here chose and nothing here could see. That is the real reason
 * one control felt responsive and the next did not: it was not two treatments, it was
 * one control with Material's ripple and the next with none of Material's anything.
 *
 * It is the same scheme the menu already used, so the menu is unchanged. What changes
 * is that everything is now inside it.
 */
@Composable
fun App() {
    // The config lives above the theme and above the touch scale, because it decides
    // both. A setting that reached only some of the controls would be worse than no
    // setting: the whole reason this one exists is that the app disagreed with
    // itself about the same control in two places.
    var config by remember { mutableStateOf(AppPrefs.load()) }
    MaterialTheme(colorScheme = LdmuxColors) {
        CompositionLocalProvider(
            LocalTextStyle provides BodyStyle,
            LocalTouch provides touchFor(config.compact),
        ) {
            AppContent(config, onConfig = { config = it })
        }
    }
}

/** The app itself. A separate function only so the theme can wrap all of it. */
@Composable
private fun AppContent(config: LdmuxConfig, onConfig: (LdmuxConfig) -> Unit) {
    var showSettings by remember { mutableStateOf(false) }
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
    // Whether the portrait bar's menu is open. It lives here, at App()'s top
    // level, rather than beside the bar that draws it, because the IME watcher
    // below has to be able to tell a keypad the *user* dismissed from one we
    // dismissed ourselves by opening a focusable menu. It is the same value
    // either way, so it has one owner and not two.
    var portraitMenu by remember { mutableStateOf(false) }
    // How many shells have been asked for and not yet answered. A chosen pi session
    // has no ldmux shell yet, so there is no id to page to until the daemon says
    // which one it made — and while that is outstanding the pager is legitimately
    // empty, which is a *different* thing from "there is nothing to open" and must
    // not be shown as the same sentence.
    //
    // Counted, not derived from the session list: the answer is the daemon's
    // `created` message, and a list of sessions cannot say which row is new.
    var pendingOpens by remember { mutableStateOf(0) }

    // The shell the user just asked for, until the pager has actually gone to it.
    //
    // A request rather than an assignment, because the page list is derived from the
    // session list: between the daemon answering `created` and the session list
    // carrying the new shell there is a moment when the id exists and has no index.
    // Assigning then would scroll to a page that is not there yet.
    var gotoSessionId by remember { mutableStateOf<String?>(null) }

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
        // `plus` on a nullable set, not `(pageSelection ?: emptySet()) + ids`: `null`
        // means "every shell is a page", and collapsing it to a set here would turn
        // "all of them" into "only this one", which is the bug in the line below
        // that used to clear the selection outright.
        pageSelection = pageSelection?.plus(ids.toSet())
        pendingOpens = (pendingOpens - ids.size).coerceAtLeast(0)
        // ...and go to it. Creating a shell and staying on the screen that created it
        // is a shell the user cannot reach; this used to work only because the
        // selection was cleared, which also silently closed every other shell.
        gotoSessionId = ids.last()
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
    // **The shape of the screen is the window's, not ours.**
    //
    // The device rotates, the platform reports the new size, and this reads it. No
    // button locks anything: the OS auto-rotate setting, a foldable's inner screen,
    // split screen and a floating window all arrive here already answered, and there
    // is no second opinion to disagree with. `containerSize` rather than a
    // configuration or a sensor reading because the size is the thing that is
    // actually true about the pixels we are about to draw into.
    val windowSize = LocalWindowInfo.current.containerSize
    val portrait = windowSize.height >= windowSize.width
    val wantPortrait = showLauncher || portrait

    // **The keyboard is portrait-only, and it is not the orientation.**
    //
    // These were one variable, so putting the keyboard away *was* rotating, and the
    // user could only have both or neither: dismissing the keypad threw them into
    // landscape. Now the shape follows the device and this follows the user. Closing
    // the keyboard leaves the screen exactly where it is; the way back is to rotate
    // through portrait, which is what re-arms it.
    var keyboardWanted by remember { mutableStateOf(true) }
    LaunchedEffect(portrait) { if (portrait) keyboardWanted = true }
    val typing = portrait && keyboardWanted

    // A popup belongs to the screen it was opened on. The portrait bar leaves the
    // composition when the mode flips, and a `remember`ed flag inside it survives
    // that: the menu was found already open on the next flip back to portrait.
    LaunchedEffect(portrait) { portraitMenu = false }

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

    // The sticky modifiers used to live here, in the composition, and that was the
    // bug: a latch reachable only from the two key bars meant a *typed* character
    // never spent it, so arming Ctrl and then typing `q` sent a plain `q` and left
    // Ctrl armed for whatever key came next. It belongs to the session now
    // (TerminalHandle.modifiers), so every path that sends a command — bar key,
    // hardware key, typed character, dictation — arrives at the one place that can
    // spend it.
    //
    // The earlier move, from the panel to here, fixed the same class of fault one
    // level up: the portrait bar had no latch of its own and its Ctrl key was wired
    // straight to `C-c`, so Ctrl-then-`d` there sent C-c and then a d. Both times
    // the fault was a latch with more than one possible reader. There is one now.

    // One handle per session, so the chrome's keys reach the visible terminal.
    // A snapshot state map, so the panel recomposes once a surface registers.
    val handles = remember { mutableStateMapOf<String, TerminalHandle>() }

    // The one place a key is sent, for both modes, so the latch can neither leak
    // nor be forgotten: every key in either bar goes through here.
    val pagerState = rememberPagerState(pageCount = { pages.size.coerceAtLeast(1) })

    // Carries out the "go to the shell just created" request from above. It lives
    // here because this is where the pager exists; the request is made there because
    // that is where the daemon's answer arrives.
    LaunchedEffect(gotoSessionId, pages) {
        val want = gotoSessionId ?: return@LaunchedEffect
        val at = pages.indexOfFirst { it.id == want }
        if (at < 0) return@LaunchedEffect
        pagerState.scrollToPage(at)
        gotoSessionId = null
    }

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

    // **The page is the user's, and the pager loses it on a re-measure.**
    //
    // A `PagerState` re-derives its position when the viewport changes size rather
    // than merely re-laying-out, and the viewport changes size on every flip, because
    // `mode` changes the moment the flip button is pressed and the window follows.
    // Measured on the device, with the pager's identity printed alongside it: at a
    // viewport of 2294 px it still reported page 1, and the instant it was measured at
    // 1080 px it reported page 0 — the same state object, the same composition, the
    // terminal area composed exactly once for the whole run. So this is not a pager
    // being rebuilt and not a new state, which is what five earlier attempts assumed.
    //
    // The page is therefore captured while the viewport is *stable* — when a report
    // can only be the user, from the arrows or a swipe — and put back on the change
    // itself. On the event, not on a timer: a timer is a bet on when re-measuring has
    // finished, and every previous attempt lost that bet.
    LaunchedEffect(pagerState) {
        var keep = pagerState.currentPage
        var width = pagerState.layoutInfo.viewportSize.width
        snapshotFlow { pagerState.layoutInfo.viewportSize.width to pagerState.currentPage }
            .collect { (w, p) ->
                if (w == width) {
                    keep = p
                } else {
                    width = w
                    if (p != keep) pagerState.scrollToPage(keep)
                }
            }
    }

    // A latch that survived a glance at another shell would be a latch that
    // surprises you, so anything that ends the gesture clears it — and it is cleared
    // on the session, because that is where it lives.
    LaunchedEffect(pagerState.currentPage) {
        pages.getOrNull(pagerState.currentPage)?.id?.let { handles[it] }?.clearModifiers()
    }

    // Every key in either bar goes through here. There is nothing to consult: the
    // latch belongs to the session and the session spends it inside sendKey, so a
    // typed character and a bar key are one mechanism rather than two.
    val press: (String) -> Unit = { key ->
        pages.getOrNull(pagerState.currentPage)?.id?.let { handles[it] }?.sendKey(key)
    }
    LaunchedEffect(pages.size) {
        if (pages.isNotEmpty() && pagerState.currentPage >= pages.size) {
            pagerState.scrollToPage(pages.size - 1)
        }
    }



    var killTarget by remember { mutableStateOf<Session?>(null) }
    val currentHandle = pages.getOrNull(pagerState.currentPage)?.id?.let { handles[it] }

    // **One microphone, and it belongs to the app.** Dictation is bound to the shell
    // that is on screen when it starts, because a transcript is for the shell you
    // were looking at when you spoke. Switching shells *ends* it rather than
    // redirecting the words into whatever slid under your thumb.
    //
    // This state is here rather than in a terminal surface for a measured reason:
    // the pager keeps a neighbour composed for an instant swipe, so a per-surface
    // owner stopped the wrong page's dictation when any page was disposed, and two
    // composed pages could each draw the banner.
    var voice by remember { mutableStateOf<VoiceState>(VoiceState.Idle) }

    // Switching shells ends dictation. Read through the remembered pager rather than
    // through the page list, which a captured value would freeze at its first value.
    LaunchedEffect(pagerState) {
        var last = pagerState.currentPage
        snapshotFlow { pagerState.currentPage }.collect { page ->
            if (page != last) {
                last = page
                Dictation.stop()
            }
        }
    }

    // A microphone that outlives the screen is a microphone nobody asked for.
    LaunchedEffect(AppLifecycle.foreground) {
        if (!AppLifecycle.foreground) Dictation.stop()
    }

    // The banner is feedback rather than a notification. It is drawn over the
    // terminal, so a line that stays is a row of the terminal that stays covered —
    // and a failure that has been read, or never will be, is not worth that. Refused
    // permission is the exception: it is the one voice state a user can act on, and
    // hiding the only explanation of why the microphone does nothing would leave the
    // button looking broken.
    LaunchedEffect(voice) {
        if (voice is VoiceState.Failed || voice == VoiceState.Unavailable) {
            delay(4000)
            voice = VoiceState.Idle
        }
    }

    // Portrait *means* typing, so the keyboard belongs to it: if the user
    // dismisses the keyboard we return to landscape rather than sitting in
    // portrait with none. Debounced, because the inset also passes through zero
    // The IME watcher that used to live here is gone, and its absence is the point.
    //
    // It existed to notice the *user* dismissing the keypad and to answer that by
    // flipping the screen — which meant every other way the keyboard could go away
    // (a focusable popup of ours, the menu, the settings dialog) had to be
    // distinguished from it, and the distinguishing was done with a 600 ms debounce
    // on the IME inset. That is now unnecessary: closing the keyboard closes the
    // keyboard. There is no rotation to trigger and so nothing to tell apart.

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
                // ...and *go to it*. Creating a shell and leaving the user on the
                // screen that created it is a shell they cannot reach. The going is
                // the `gotoSessionId` request above, made when the daemon answers.
                //
                // The selection is deliberately **not** cleared here. It used to be,
                // and that silently closed every shell already open: the page set is
                // what ◀ ▶ walk, so opening a second shell left one page and the
                // arrows with nowhere to go. Creating a shell is not a reason to let
                // go of the others.
                pendingOpens = 1
                showLauncher = false
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
                        .clip(RadiusMedium)
                        .background(Surface)
                        .border(1.dp, Line, RadiusMedium)
                        .clickable { client.clearServerError() }
                        .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad),
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
                    onConfig(it)
                    AppPrefs.save(it)
                    client.reconfigure(it)
                    showSettings = false
                },
            )
        }
    } else {
        // **One terminal, in one place.** The two modes differ in what is arranged
        // around it, not in the terminal itself, and that is load-bearing rather
        // than tidy: a plain `if/else` builds a *new* `HorizontalPager` in each
        // branch, and a new pager resets the shared `PagerState` to its first page.
        // Measured, with the viewport width already final, on the frame the portrait
        // branch was first composed: `page=1 viewportW=1080`, then `page=0
        // viewportW=1080` 135 ms later. Every earlier attempt at this bug tried to
        // put the page back afterwards. None asked why it had moved, and it had moved
        // because the pager was thrown away.
        // (portraitMenu is declared in App()'s top-level state rather than here,
        // because the IME watcher has to be able to tell a keypad the user
        // dismissed from one we dismissed ourselves by opening a focusable menu.)
        // Chrome waits for the window to finish turning over, and there is none
        // before it has: a panel belongs to the shape it was drawn for.
        val chrome = panelArrived
        Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
            Row(Modifier.weight(1f).fillMaxWidth()) {
                val panel: @Composable () -> Unit = {
                    SidePanel(
                        pages,
                        pagerState,
                        scope,
                        client,
                        handles,
                        onKill = { killTarget = it },
                        onVoice = {
                            // The sink is the handle that is on screen *now*, taken
                            // once: a transcript cannot be retargeted afterwards,
                            // because by then the user has been speaking. No handle
                            // means no shell, which is not a failure to hide.
                            val target = currentHandle
                            if (target == null) {
                                voice = VoiceState.Unavailable
                            } else {
                                Dictation.toggle(onText = { target.sendText(it) }, onState = { voice = it })
                            }
                        },
                        onLauncher = {
                            // The launcher is where you choose what to open, so
                            // going back to it lets go of everything: a page is a
                            // *hold* on a shell, and holding shells you are not
                            // looking at is how the phone keeps a session someone
                            // else might want.
                            pageSelection = emptySet()
                            showLauncher = true
                        },
                        // Named rather than trailing: `press` is a parameter now, and
                        // a trailing lambda would bind to it.
                        onSettings = { showSettings = true },
                        press = press,
                    )
                }
                // The side panel is a landscape thing. Portrait's chrome is the bar
                // *below* the terminal, and `panelLeft` decides which side the panel
                // is on in the one mode that has one.
                if (!portrait && chrome && config.panelLeft) panel()
                // The banner follows the app rather than the page, so it is drawn
                // over whichever shell is on screen and there is exactly one of it.
                Box(Modifier.weight(1f).fillMaxHeight()) {
                    TerminalArea(
                        config,
                        pages,
                        pagerState,
                        handles,
                        typing = typing,
                        Modifier.fillMaxSize(),
                        pendingOpen = pendingOpens > 0,
                    )
                    VoiceBanner(voice, Modifier.align(Alignment.TopStart))
                }
                if (!portrait && chrome && !config.panelLeft) panel()
            }
            if (portrait && chrome) {
                PortraitBar(
                    handle = currentHandle,
                    pages = pages,
                    pagerState = pagerState,
                    scope = scope,
                    onHideKeyboard = { keyboardWanted = false },
                    menuExpanded = portraitMenu,
                    onMenu = { portraitMenu = true },
                    onMenuDismiss = { portraitMenu = false },
                    onLauncher = {
                        // The launcher is where you choose what to open, so going
                        // back to it lets go of everything: a page is a *hold* on a
                        // shell, and holding shells you are not looking at is how
                        // the phone keeps a session someone else might want.
                        pageSelection = emptySet()
                        showLauncher = true
                    },
                    onSettings = { showSettings = true },
                    modifiers = currentHandle?.modifiers ?: Modifiers(),
                    onSticky = { which -> currentHandle?.toggleModifier(which) },
                    press = press,
                )
            }
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
                onConfig(it)
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
            MiniTextButton(onClick = onConfirm) { Text("close", color = Color(0xFFE08A8A)) }
        },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
    )
}

@Composable
private fun PortraitBar(
    handle: TerminalHandle?,
    pages: List<Session>,
    pagerState: PagerState,
    scope: CoroutineScope,
    onHideKeyboard: () -> Unit,
    onMenu: () -> Unit,
    menuExpanded: Boolean,
    onMenuDismiss: () -> Unit,
    onLauncher: () -> Unit,
    onSettings: () -> Unit,
    modifiers: Modifiers,
    onSticky: (StickyKey) -> Unit,
    press: (String) -> Unit,
) {
    Column(
        Modifier.fillMaxWidth().background(Surface).padding(horizontal = 6.dp, vertical = LocalTouch.current.pad),
        verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
    ) {
        // Where you are, and how to move, at the top of the bar: the same row the
        // side panel shows, because moving between shells must not mean leaving
        // them. Navigation at one end, keys in the middle, actions at the other.
        ShellHeader(pages, pagerState, scope, onLauncher, onSettings, showMenu = false, dense = true)

        PanelDivider()

        // Two rows, each sharing the width. One scrolling row hid half its keys
        // off the edge, which is the one thing a keyboard-adjacent strip must not
        // do: a key you cannot see is a key that does not exist.
        //
        // Enter moved down to close the bottom row at the user's word, so that it
        // sits with the keys that feed a line rather than alone at the top. The two
        // controls that leave this strip are not part of either row: they stack at
        // the leading edge, menu above flip, one per row.
        //
        // Nothing here is a keyboard glyph, and nothing here is a shell button: a
        // new shell belongs to the menu, where you are choosing one, not to the
        // bar you type on.
        // The two ways out of this strip, stacked at the leading edge: the menu on
        // top and the flip directly below it, both flush left. They were strung
        // along the top row, then re-placed beside each other, and the shape that
        // survived is top-to-bottom in the corner — which is where a thumb rests,
        // and the pair is a pair: leaving the keyboard and changing the keyboard's
        // shape are the same decision made twice.
        val menuHere: @Composable RowScope.() -> Unit = {
            Box(Modifier.weight(1f)) {
                IconBtn(onClick = onMenu, label = "menu", modifier = Modifier.fillMaxWidth(), filled = true) {
                    IconLauncher(Fg, box = LocalTouch.current.icon)
                }
                AppMenu(menuExpanded, onMenuDismiss, onLauncher, onSettings)
            }
        }
        // Putting the keyboard away. It is not a rotation any more: the screen stays
        // exactly where it is, and the keypad is the only thing that leaves.
        val keyboardHere: @Composable RowScope.() -> Unit = {
            IconBtn(
                onClick = onHideKeyboard,
                label = "hide keyboard",
                modifier = Modifier.weight(1f),
            ) {
                IconKeyboardHide(Accent, box = LocalTouch.current.icon)
            }
        }
        PortraitKeyRow(
            listOf(
                "Esc" to { press("Escape") },
                "Tab" to { press("Tab") },
                "\u2190" to { press("Left") },
                "\u2191" to { press("Up") },
                "\u2193" to { press("Down") },
                "\u2192" to { press("Right") },
            ),
            leading = menuHere,
        )
        PortraitKeyRow(
            listOf(
                // The same latch as the panel's, from the same state: arm Ctrl,
                // then press any key, and Ctrl applies to that key. It used to be
                // wired straight to `C-c` here, which sent an interrupt instead of
                // arming anything — so Ctrl-then-`d` sent C-c and then a d.
                "Ctrl" to { onSticky(StickyKey.Ctrl) },
                "Alt" to { onSticky(StickyKey.Alt) },
                controlLabel("C-c") to { press("C-c") },
                "|" to { press("|") },
                "-" to { press("-") },
                "/" to { press("/") },
                "\u23ce" to { press("Enter") },
            ),
            leading = keyboardHere,
            active = listOf(modifiers.ctrl, modifiers.alt, false, false, false, false, false),
        )
        // No microphone: the keyboard is always up in portrait and it has its own.
        // Two microphones is one too many.
    }
}

/**
 * One row of the portrait bar: an optional leading control, then keys sharing the
 * width.
 *
 * The leading slot exists so the two controls that *leave* this mode can stack in
 * the corner a thumb rests on — the menu above, the flip below — instead of being
 * strung along a row beside keys that type. A key you press mid-sentence and a
 * button that ends the sentence should not be the same shape in the same place.
 */
@Composable
private fun PortraitKeyRow(
    keys: List<Pair<String, () -> Unit>>,
    leading: (@Composable RowScope.() -> Unit)? = null,
    active: List<Boolean> = emptyList(),
) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        leading?.invoke(this)
        keys.forEachIndexed { i, (label, action) ->
            KeyBtn(label, Modifier.weight(1f), active.getOrElse(i) { false }, action)
        }
    }
}

/**
 * The portrait bar's keys used to be their own composable, a near-duplicate of the
 * panel's [KeyBtn], and the copy had already drifted: both latched to the accent
 * background, but only the panel's inverted its label, so the latched Ctrl and Alt in
 * portrait were light grey on light grey — the hardest-to-read state in the app, and
 * the same state the panel renders correctly. A key is one thing in both modes, so it
 * is one composable in both modes. The drift is not fixed here; it is made impossible.
 */


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
    dense: Boolean = false,
) {
    val current = pagerState.currentPage
    val total = pages.size.coerceAtLeast(1)
    val here = pages.getOrNull(current)
    var menuOpen by remember { mutableStateOf(false) }
    // Density, not structure: this is the same row drawn by the same composable in
    // both modes, and `dense` decides only how much height it is allowed.

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
                IconBtn(onClick = { menuOpen = true }, label = "menu", filled = true) {
                    IconLauncher(Fg)
                }
                AppMenu(
                    expanded = menuOpen,
                    onDismiss = { menuOpen = false },
                    onLauncher = onLauncher,
                    onSettings = onSettings,
                )
            }
        }
        NavArrow("\u25c0", enabled = current > 0, dense = dense) { step(-1) }
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
        NavArrow("\u25b6", enabled = current < total - 1, dense = dense) { step(1) }
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
    onKill: (Session) -> Unit,
    onVoice: () -> Unit,
    onLauncher: () -> Unit,
    onSettings: () -> Unit,
    press: (String) -> Unit,
) {
    val current = pagerState.currentPage
    val here = pages.getOrNull(current)
    val handle = here?.id?.let { handles[it] }
    val total = pages.size.coerceAtLeast(1)

    Column(
        Modifier.width(PANEL_WIDTH).fillMaxHeight().background(Surface)
            .padding(horizontal = 8.dp, vertical = LocalTouch.current.rowPad * 2),
        verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
    ) {
        ShellHeader(pages, pagerState, scope, onLauncher, onSettings)

        PanelDivider()

        @OptIn(ExperimentalLayoutApi::class)
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
        ) {
            // One row per group, each row sharing the panel's width between its
            // keys. Keys used to be chips sized to their own text, which made a
            // ragged column where "C-c" and "|" had the same weight and nothing
            // lined up with anything. A row that fills the width is easier to hit
            // and, more to the point, says how many keys are in it.
            // Ctrl and Alt are not offered here. Landscape has no soft keyboard, so
            // a latch has nothing to apply to: a modifier is armed to change the key
            // that comes *next*, and every key a finger can reach in this mode is a
            // panel key that can spell out what it wants. The only keyboard this mode
            // has is a real one, plugged in, and it brings its own Ctrl and Alt —
            // hardware key events are forwarded to the shell at
            // TerminalSurface.android.kt:266. Portrait keeps both, because portrait is
            // where typing happens.
            //
            // `C/c` survives below as a one-time key, so an interrupt is still one
            // press away. The pair that leaves is the pair that needed a second press.
            //
            // `copy` keeps its own row, which also reads correctly: it is a mode, not
            // a chord. It toggles tmux copy-mode, which is the only way to scroll the
            // history of a full-screen program, because a program on the alternate
            // screen has no local scrollback to page through.
            val clipboard = LocalClipboardManager.current
            // `paste` joins `copy` because it is the same kind of thing: a control
            // for the shell's clipboard rather than a chord, so it belongs in a
            // row that fills the width like every other row. It is needed in this
            // mode in particular because landscape has no soft keyboard, which
            // means no paste gesture at all — and the clipboard is where a
            // program's own OSC 52 copy lands, so without this a program's copy
            // would have nowhere to be seen.
            KeyRow(
                listOf(
                    "copy" to { handle?.setCopyMode(handle?.inCopyMode != true) },
                    "paste" to {
                        val text = clipboard.getText()?.text
                        if (!text.isNullOrEmpty()) handle?.paste(text)
                    },
                ),
                active = listOf(handle?.inCopyMode == true, false),
            )
            PanelDivider()
            // Backspace joins the keys a soft keyboard cannot send. Landscape has no
            // keyboard at all, so a dictated word that arrives wrong cannot be taken
            // back: Esc, Tab and Enter were here, and the one key that undoes a
            // character was not. `BSpace` is tmux's name for erase-to-the-left and it
            // is what the hardware path already sends (TerminalSurface.android.kt:277),
            // so there is one definition of this key rather than two.
            //
            // Labelled with a word, because the other three keys in this row are
            // words and a lone glyph would be a picture among labels. Drawn marks are
            // for the marks that have no word, not for words that have a mark.
            //
            // Portrait does not get one: the keyboard is up there and it carries its
            // own backspace.
            KeyRow(
                listOf(
                    "Esc" to { press("Escape") },
                    "Tab" to { press("Tab") },
                    "del" to { press("BSpace") },
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
            // The five things a soft keyboard cannot produce and a shell wants
            // most: one control key, and the four characters that are not on the
            // number row where they can be reached.
            //
            // Ctrl-D is deliberately not among them. It was, and it was asked for
            // by name to be taken out. It sends end-of-file, which closes the pane,
            // and a key that ends a session has no business sitting on a bar that
            // someone presses without looking while holding the phone one-handed.
            KeyRow(
                listOf(
                    controlLabel("C-c") to { press("C-c") },
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
            // No flip control here. The screen's shape is the device's now, so there
            // is nothing to flip to, and landscape has no keyboard to put away —
            // which is why the only control left in this row is the microphone.
            // Portrait's bar carries its own, because the IME has one there anyway:
            // two microphones is one too many.
            IconBtn(onClick = onVoice, label = "voice input") { IconMic(tint = Fg) }
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
            .clip(RadiusMedium)
            .background(Accent)
            .heightIn(min = LocalTouch.current.min)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = Bg, fontSize = 13.sp)
    }
}

@Composable
private fun NavArrow(glyph: String, enabled: Boolean, dense: Boolean = false, onClick: () -> Unit) {
    Box(
        Modifier
            .clip(RadiusMedium)
            // The most-repeated action there is, and it was the smallest target in the
            // app at about 28dp. A control you press constantly is the last one that
            // should be hard to hit.
            .heightIn(min = if (dense) LocalTouch.current.dense else LocalTouch.current.min)
            .clickable(enabled = enabled, role = Role.Button, onClick = onClick)
            .padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(glyph, color = if (enabled) Accent else Dim.copy(alpha = 0.4f), fontSize = 18.sp)
    }
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
            .clip(RadiusMedium)
            .background(if (active) Accent else Bg)
            .heightIn(min = LocalTouch.current.min)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = pad, vertical = LocalTouch.current.pad),
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
    var compact by remember { mutableStateOf(current.compact) }

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
                    MiniTextButton(onClick = { side = "left" }) { Text("left", color = if (side == "left") Accent else Dim) }
                    MiniTextButton(onClick = { side = "right" }) { Text("right", color = if (side == "right") Accent else Dim) }
                }
                Box(Modifier.height(LocalTouch.current.rowPad))
                OutlinedTextField(
                    value = host,
                    onValueChange = { host = it },
                    label = { Text("host[:port]") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Box(Modifier.height(LocalTouch.current.rowPad))
                OutlinedTextField(
                    value = token,
                    onValueChange = { token = it },
                    label = { Text("token") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Box(Modifier.height(LocalTouch.current.rowPad))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("predictive echo", color = Dim, fontSize = 12.sp)
                    MiniTextButton(onClick = { predict = false }) {
                        Text("off", color = if (!predict) Accent else Dim)
                    }
                    MiniTextButton(onClick = { predict = true }) {
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
                Box(Modifier.height(LocalTouch.current.rowPad * 2))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("control size", color = Dim, fontSize = 12.sp)
                    MiniTextButton(onClick = { compact = false }) {
                        Text("tall", color = if (!compact) Accent else Dim)
                    }
                    MiniTextButton(onClick = { compact = true }) {
                        Text("thin", color = if (compact) Accent else Dim)
                    }
                }
                Text(
                    "tall is easier to hit; thin fits more lines",
                    color = Dim,
                    fontSize = 10.sp,
                )
                Box(Modifier.height(LocalTouch.current.rowPad * 2))
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text("text size", color = Dim, fontSize = 12.sp)
                    // Disabled at the ends of the range rather than silently doing
                    // nothing. A live button that produces no visible change is the
                    // worst of both: it looks broken, and it hides the reason. The
                    // terminal arrow already knew that.
                    val smaller = fontSp > LdmuxConfig.MIN_FONT_SP
                    val larger = fontSp < LdmuxConfig.MAX_FONT_SP
                    MiniTextButton(
                        onClick = { fontSp = (fontSp - 1f).coerceAtLeast(LdmuxConfig.MIN_FONT_SP) },
                        enabled = smaller,
                    ) {
                        Text("−", color = if (smaller) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
                    }
                    Text("${fontSp.toInt()}", color = Fg, fontFamily = FontFamily.Monospace, fontSize = 12.sp)
                    MiniTextButton(
                        onClick = { fontSp = (fontSp + 1f).coerceAtMost(LdmuxConfig.MAX_FONT_SP) },
                        enabled = larger,
                    ) {
                        Text("+", color = if (larger) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
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
            MiniTextButton(onClick = {
                onSave(
                    LdmuxConfig(
                        host.trim(),
                        token.trim(),
                        side,
                        predict,
                        fontSp.coerceIn(LdmuxConfig.MIN_FONT_SP, LdmuxConfig.MAX_FONT_SP),
                        compact,
                        // Carried, not rebuilt. This call used to construct a
                        // config without the identity fields, so every visit to
                        // settings cleared the device id and the next load minted a
                        // new one — silently, and against ADR-0010, which exists so
                        // two devices can be told apart in a sentence about a shell
                        // they both want.
                        current.deviceId,
                        current.deviceLabel,
                    ),
                )
            }) {
                Text("save", color = Accent)
            }
        },
        dismissButton = { MiniTextButton(onClick = onDismiss) { Text("cancel", color = Dim) } },
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
            Spacer(Modifier.height(LocalTouch.current.rowPad))
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
            Spacer(Modifier.height(LocalTouch.current.rowPad * 2))
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
 * `C-c`, `|`, `-` and `/` are, and what a ragged column of chips was not.
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
        colorScheme = LdmuxColors,
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
                // Material's menu item is 48dp and no CompositionLocal reaches it,
                // which is why the app's own menu stayed thick at every density.
                modifier = Modifier.height(LocalTouch.current.min),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 0.dp),
            )
            DropdownMenuItem(
                text = { Text("settings", color = Fg) },
                onClick = { onDismiss(); onSettings() },
                modifier = Modifier.height(LocalTouch.current.min),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 0.dp),
            )
        }
    }
}


/**
 * A Material text button at this app's own control height.
 *
 * Material's buttons carry a minimum height of their own and no `CompositionLocal`
 * can reach it, so every `TextButton` in the app was 48dp in both densities. That is
 * why the settings dialog -- the one screen a person opens to check the density
 * setting -- was the one screen the setting could not thin. `Modifier.height`
 * overrides Material's `defaultMinSize`, which is what set the 48dp.
 *
 * One wrapper rather than a modifier repeated at eleven call sites, because eleven
 * call sites is how a setting ends up reaching most of the controls and not all of
 * them, which is the failure this app already had once.
 */
@Composable
private fun MiniTextButton(
    onClick: () -> Unit,
    enabled: Boolean = true,
    content: @Composable RowScope.() -> Unit,
) {
    TextButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.height(LocalTouch.current.min),
        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 0.dp),
        content = content,
    )
}
