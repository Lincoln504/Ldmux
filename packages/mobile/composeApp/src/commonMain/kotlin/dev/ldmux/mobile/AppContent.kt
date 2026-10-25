package dev.ldmux.mobile

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
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
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.foundation.interaction.collectIsDraggedAsState
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import dev.ldmux.mobile.voice.Dictation
import dev.ldmux.mobile.voice.VoiceState
import dev.ldmux.mobile.voice.VoiceBanner
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalWindowInfo
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.launch

/**
 * How long a panel waits for the window to finish turning over before it appears.
 *
 * The rotation is Android's and it has already begun by the time the app can ask
 * about it — see the note in `App` — so this is a beat out of the turn itself
 * rather than a wait on a signal that does not exist.
 */
private const val PANEL_FLIP_MS = 200L

/** The app itself. A separate function only so the theme can wrap all of it. */
@Composable
internal fun AppContent(config: LdmuxConfig, store: ConfigStore) {
    var showSettings by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val client = remember { LdmuxClient(config) }

    // Started with the composition and stopped with it. Nothing ever stopped it, so the
    // launcher socket pinged the daemon for as long as the process lived.
    DisposableEffect(client) {
        client.start(scope)
        onDispose { client.stop() }
    }
    val sessions by client.sessions.collectAsState()
    val desktop by client.desktop.collectAsState()
    val connected by client.connected.collectAsState()
    val link by client.link.collectAsState()
    // Read here, not inside the launcher branch: a refusal releases a pending
    // shell request, and that request is made from a screen the branch does not own.
    val errorTick by client.errorTick.collectAsState()

    // A phone that has just rejoined the wifi should not wait out the retry
    // backoff: the network callback cuts it short.
    DisposableEffect(Unit) {
        // A network change: cut a backoff short, and have every open socket check that it
        // is still alive (the one on the old network will not say so by itself).
        BackgroundSession.watchNetwork {
            client.reconnectNow()
            NetworkWake.fire()
        }
        onDispose { BackgroundSession.stopWatchingNetwork() }
    }

    // The launcher comes first: a terminal is somewhere you go, and which shell
    // you are going to is a question worth asking before you are in one.
    var showLauncher by remember { mutableStateOf(true) }
    // Which shells the pager pages through: the ones the user chose, and nothing
    // else. It used to start as `null`, meaning "every shell", so the first
    // `+ shell` after launch opened a pager over *all* of them, attached page 0 —
    // some other shell, possibly one on the desk — claimed it and resized it to the
    // phone, and only then scrolled to the new one. A page is a hold on a shell,
    // so nothing is a page that was not asked for.
    //
    // **A list, not a set: the order is the user's.** The pages used to be
    // `attachable.filter { it.id in selection }`, which means the daemon's session
    // order — and that order is *last activity first* (`sessions.sort_by` in
    // `crates/daemon/src/daemon.rs`), so every shell that printed anything moved to
    // the front of the list. With several shells open, the page at index N became a
    // different shell whenever any of them produced output: the pager's content
    // changed under the finger, the watcher below scrolled to put the shell back
    // where it belonged, and the next broadcast did it again. Measured on the phone
    // 2026-10-25: typing in one shell made the screen slide between shells and
    // return, which is what "it acts like it is swiping but comes back" was.
    //
    // A shell's page number is now fixed when it is opened and stays fixed while
    // the terminal is open (it only shifts when a page is closed). Going back to the
    // launcher picks again, and picks in the launcher's order.
    var pageOrder by remember { mutableStateOf<List<String>>(emptyList()) }

    // **The connection follows the pairing, and only the pairing.** A display
    // setting changes nothing here. A new pairing (or none) is a different machine:
    // the pages were the old one's shells, so they close and the launcher shows the
    // new one — or says `unpaired`.
    var connectedKey by remember { mutableStateOf(config.connectionKey) }
    LaunchedEffect(config.connectionKey) {
        if (config.connectionKey == connectedKey) return@LaunchedEffect
        connectedKey = config.connectionKey
        client.reconfigure(config)
        pageOrder = emptyList()
        showLauncher = true
    }
    // Whether the portrait bar's menu is open. It lives here, at App()'s top
    // level, rather than beside the bar that draws it, because the IME watcher
    // below has to be able to tell a keypad the *user* dismissed from one we
    // dismissed ourselves by opening a focusable menu. It is the same value
    // either way, so it has one owner and not two.
    var portraitMenu by remember { mutableStateOf(false) }
    // How many shells have been asked for and not yet answered. A new shell has no
    // id to page to until the daemon says which one it made — and while that is
    // outstanding the pager is legitimately
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
    // In the user's order. An id the list does not carry right now keeps its place
    // *in the order* — it is not dropped and re-appended — so it comes back at its
    // own index; while it is missing, the pages after it sit one earlier.
    val pages = pagesInOrder(pageOrder, attachable)

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
        pageOrder = addToOrder(pageOrder, ids)
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
    // they live in the keeper on the host, not in this process.
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
    // Whether the soft keyboard is actually on screen, for the portrait toggle.
    val imeUp = WindowInsets.ime.getBottom(LocalDensity.current) > 0
    var keyboardRequest by remember { mutableStateOf(0) }

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
    // The view is the cheap part: the shell lives in the keeper on the host, and a
    // client that reconnects gets a full snapshot of the terminal state by
    // construction. So there is nothing to preserve by staying awake.
    //
    // The grace window is what makes it not annoying: an app switch, a glance at
    // a notification, or answering a message all come back inside it and find
    // everything exactly as it was. Past it the user has gone somewhere, and the
    // honest thing is to be a list of shells when they return.
    //
    // **This is registered with the platform lifecycle, not run as an effect.** An
    // effect needs a live composition and a frame clock, and a suspended iOS process
    // has neither: the release would be a coroutine that never ran. `AppLifecycle.set`
    // — the platform's own callback — runs the handler (synchronously when the platform's
    // grace is 0), so the release is an explicit call (see `BackgroundRelease`).
    //
    // Deliberately the same mechanism as the launcher button: dropping the pages
    // *is* the detach, so there is one way for a shell to be let go of. The launcher
    // socket stops too (it is restarted on return, below).
    DisposableEffect(client) {
        val removeRelease = AppLifecycle.setReleaseHandler {
            pageOrder = emptyList()
            showLauncher = true
            client.stop()
        }
        // A microphone that outlives the screen is a microphone nobody asked for, and
        // it is stopped at once, not after the grace.
        val removeVoice = AppLifecycle.onBackground { Dictation.stop() }
        onDispose {
            removeRelease()
            removeVoice()
        }
    }
    LaunchedEffect(AppLifecycle.foreground) {
        if (AppLifecycle.foreground) {
            // Back on screen after the launcher socket was let go: bring it up (a no-op
            // when it is running).
            client.start(scope)
            // **Fresh session data on the way back in.** The daemon pushes a
            // broadcast whenever a directory changes, so this is belt and braces —
            // but the case it covers is the one the push cannot: a phone that was
            // asleep, whose socket came back with the app and whose list is
            // whatever it was before.
            if (connected) client.refreshSessions()
        }
    }

    // **A shell that ends takes its page with it, and the last one takes the pager.**
    // Closing a shell at the desk (or `exit`, or a kill anywhere) arrives as the
    // daemon's pushed session list; the page is gone from `pages` at once, since
    // only attachable shells are pages. When none of the chosen shells is left, the
    // terminal is showing nothing, so the app goes back to the list of what there is.
    //
    // Not while the list could be stale or incomplete: offline, a lost link is not
    // an ended shell; and a shell just asked for is chosen before the list carries
    // it (`gotoSessionId` is set until the pager reaches it).
    val chosenAlive = attachable.any { it.id in pageOrder }
    LaunchedEffect(chosenAlive, connected, pendingOpens, gotoSessionId, showLauncher) {
        // Not conditioned on the selection being non-empty: the effect below prunes
        // ended ids from it, and may do so first.
        if (!showLauncher && connected && pendingOpens == 0 && gotoSessionId == null && !chosenAlive) {
            pageOrder = emptyList()
            showLauncher = true
        }
    }
    // Ended shells leave the selection too, so a shell id is never a page-to-be
    // after its shell has gone. Only a shell the list still carries and says is not
    // attachable is removed: an id absent from the list is left alone, because a
    // list can be momentarily empty or stale and a shell that was just asked for is
    // chosen before the list carries it. An absent id cannot keep the app on a blank
    // terminal either — `chosenAlive` only looks at attachable shells.
    LaunchedEffect(sessions) {
        val gone = pageOrder.filter { id -> sessions.any { it.id == id && !it.attachable } }
        if (gone.isNotEmpty()) pageOrder = pageOrder - gone.toSet()
    }

    // The notification's Disconnect is the launcher button pressed from outside
    // the app. It used to stop only the service, leaving the sockets and claims up
    // behind a notification that had said it disconnected.
    LaunchedEffect(Unit) {
        DisconnectRequests.requests.collect {
            DisconnectRequests.handled()
            pageOrder = emptyList()
            showLauncher = true
        }
    }

    // Back means the same thing as the launcher button: it leaves the terminal
    // and lets go. It is not a second way to do it — it is the same call.
    BackToLauncher(enabled = terminalOpen) {
        pageOrder = emptyList()
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

    // The shell that is meant to be on screen, by id — not the pager's page
    // index, which a re-measure resets underneath the user. Declared here,
    // before anything that answers to it. See the watcher below for why.
    val shownSessionId = remember { mutableStateOf<String?>(null) }

    // Carries out the "go to the shell just created" request from above. It lives
    // here because this is where the pager exists; the request is made there because
    // that is where the daemon's answer arrives.
    LaunchedEffect(gotoSessionId, pages, sessions, connected) {
        val want = gotoSessionId ?: return@LaunchedEffect
        when (gotoState(want, sessions, pages.map { it.id }, connected)) {
            GotoState.Arrive -> {
                val at = pages.indexOfFirst { it.id == want }
                // Named as the intent first: the scroll below is programmatic, and the
                // watcher below distinguishes intent from a re-measure by this id.
                shownSessionId.value = want
                pagerState.scrollToPage(at)
                gotoSessionId = null
            }
            // It can never arrive (it died, or the link dropped): an outstanding request
            // blocks the "none of my shells is left" rule below, so it must not linger.
            GotoState.Abandon -> gotoSessionId = null
            GotoState.Wait -> Unit
        }
    }
    // And a shell that is merely never listed cannot hold the request forever either.
    // Keyed on the request alone, so the session list's own broadcasts do not keep
    // restarting the clock.
    LaunchedEffect(gotoSessionId) {
        if (gotoSessionId != null) {
            delay(GOTO_TIMEOUT_MS)
            gotoSessionId = null
        }
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

    // A pager that is showing nothing has no shell on screen, and the id the
    // watcher below consults is what the *next* open navigates from. Left set, a
    // later visit to the launcher and a fresh pick reopened on a page nobody chose.
    LaunchedEffect(pageOrder.isEmpty()) {
        if (pageOrder.isEmpty()) shownSessionId.value = null
    }

    // **The page is the user's, and the pager loses it on a re-measure.**
    //
    // A `PagerState` re-derives its position when the viewport changes size rather
    // than merely re-laying-out, and the viewport changes size on every rotation
    // (and when the landscape panel arrives, which is a second resize after the
    // first). Measured on the device, with the pager's identity printed alongside
    // it: at a viewport of 2294 px it still reported page 1, and the instant it was
    // measured at 1080 px it reported page 0 — the same state object, the same
    // composition, the terminal area composed exactly once for the whole run.
    //
    // The fix that preceded this one latched the page while the viewport width was
    // stable and put it back on the change. It lost a race a rotation runs every
    // time: the width change and the pager's reset do not arrive in the same
    // emission. The first emission carries the new width with the old page — which
    // updates `width` and leaves `keep` alone — and the *second* carries the same
    // new width with the reset page 0, which by then looks like a stable viewport
    // and a user's choice, so `keep` was overwritten with 0 and the wrong shell
    // was "restored".
    //
    // So the shell on screen is tracked by **id**, which survives a re-measure
    // because it is not a position in anything. The two things that can change the
    // page without a re-measure — a swipe and the arrows — say so themselves: a
    // swipe is a drag interaction on the pager, and both arrows and "go to the new
    // shell" set the id before they move. What is left, a settled page that is not
    // the shell we mean, is a reset, and is put back. The index is looked up fresh
    // every time, because shells open and close and a page number captured earlier
    // is exactly the kind of stale guess this exists to stop.
    //
    // Deliberately not `isScrollInProgress` as the *choice*: that is also true of an
    // animated arrow press and of a re-measure's own adjustment. It is used below as
    // "still moving, do not decide yet", which is a different question.
    val pagesForPager by rememberUpdatedState(pages)
    val pagerDragged by pagerState.interactionSource.collectIsDraggedAsState()
    LaunchedEffect(pagerState) {
        // A drag is a **latch, not a decision.** `currentPage` follows the finger, so
        // a swipe dragged past the halfway point and then released short of the snap
        // threshold would leave the page it nearly reached adopted — and adopting a
        // page is an *attach*, and a first attach takes the shell over (ADR-0010), so
        // a cancelled swipe would take a shell nobody landed on. The latch is set
        // while the finger is down and the choice is read once the pager has stopped.
        var sawDrag = false
        snapshotFlow {
            Triple(
                pagerDragged to pagerState.isScrollInProgress,
                pagerState.currentPage,
                pagesForPager.getOrNull(pagerState.currentPage)?.id,
            )
        }.collect { (motion, page, id) ->
            val (fingerDown, moving) = motion
            if (fingerDown) sawDrag = true
            // Mid-flight — an arrow's animation, or a swipe still settling: what the
            // pager reports now is not where it is going to stop.
            if (moving || fingerDown) return@collect
            if (id == null) return@collect
            if (sawDrag) {
                // The finger is up and the pager has stopped: this is where the user
                // put it, and a snap back to where it started is not a choice.
                sawDrag = false
                shownSessionId.value = id
                return@collect
            }
            val shown = shownSessionId.value
            if (shown == id) return@collect
            val target = pagesForPager.indexOfFirst { it.id == shown }
            if (target >= 0 && target != page) {
                pagerState.scrollToPage(target)
            } else {
                // The shell that was on screen is gone (its page closed), or the
                // pager is already on the right index: adopt what is here.
                shownSessionId.value = id
            }
        }
    }

    // A latch that survived a glance at another shell would be a latch that
    // surprises you, so anything that ends the gesture clears it — and it is cleared
    // on the session, because that is where it lives. Keyed on the shell, not on
    // the pager's page: a re-measure that puts the pager on page 0 for a frame
    // is not a glance at another shell.
    LaunchedEffect(shownSessionId.value) {
        shownSessionId.value?.let { id -> handles[id]?.clearModifiers() }
    }

    // The shell everything in the chrome addresses — keys, the microphone, the
    // copy mode — is the one on screen, by id. The pager's own page index is
    // reset underneath the user on a re-measure (see `shownSessionId` above), and
    // a key aimed at whatever page it momentarily reports would type into a
    // shell nobody is looking at.
    val shownIndex = pages.indexOfFirst { it.id == shownSessionId.value }
    val shown = pages.getOrNull(shownIndex)

    // Every key in either bar goes through here. There is nothing to consult: the
    // latch belongs to the session and the session spends it inside sendKey, so a
    // typed character and a bar key are one mechanism rather than two.
    val press: (String) -> Unit = { key ->
        shown?.id?.let { handles[it] }?.sendKey(key)
    }
    LaunchedEffect(pages.size) {
        if (pages.isNotEmpty() && pagerState.currentPage >= pages.size) {
            pagerState.scrollToPage(pages.size - 1)
        }
    }



    var killTarget by remember { mutableStateOf<Session?>(null) }
    val currentHandle = shown?.id?.let { handles[it] }

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

    // Switching shells ends dictation. Keyed on the shell's *id*, not the pager's
    // page: the pager's page is reset by a re-measure (a rotation, the landscape
    // panel arriving), and a rotation is not the user moving to another shell — it
    // used to kill the transcription and look like a dropped connection. The
    // session's own id is what "switching shells" means, and it is what the rest of
    // this file watches (see `shownSessionId`).
    LaunchedEffect(shownSessionId.value) {
        Dictation.stop()
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
        // No link, no answer coming: the refresh icon must not stay dimmed for a
        // request that is queued behind a refused token or an unreachable host.
        LaunchedEffect(link) { if (link != LinkState.Online) desktopLoading = false }

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
        // The launcher and everything opened from it stay portrait; the shell view
        // follows the device (ADR-0011).
        HoldOrientation(OrientationPolicy.Portrait)
        Box(Modifier.fillMaxSize().background(Bg).safeDrawingPadding()) {
        LauncherScreen(
            status = linkLabel(link, config),
            link = link,
            sessions = sessions,
            desktop = desktop,
            desktopLoading = desktopLoading,
            onRefresh = {
                // Refresh is also "try the link again": it is the one thing that
                // retries a refused token, and it cuts an unreachable backoff short.
                if (link != LinkState.Online) client.reconnectNow()
                if (link == LinkState.Online) {
                    desktopLoading = true
                    client.requestDesktop()
                    // Also refresh the session list so the launcher shows fresh
                    // CWDs — the daemon's 2s poll will catch them automatically
                    // once this ships, but a manual refresh is the immediate
                    // escape hatch and the only way before the daemon fix lands.
                    client.refreshSessions()
                }
            },
            onOpen = { picked ->
                // The chosen shells become the pages, and nothing else is one, in the
                // order they were ticked — page 1 is the first row ticked, which is
                // the number the row's badge showed.
                pageOrder = picked.map { it.sessionId }
                // **...and the pager starts on page 1.** The pager and the shell it
                // last showed outlive a visit to the launcher (both are declared
                // above the launcher/terminal switch), so without this the pager
                // opened on whatever index it was left parked on: tick three shells
                // and land on page 2, with the header disagreeing with the badges.
                // Same mechanism as `+ shell`, so there is one way to arrive at a
                // page rather than two.
                picked.firstOrNull()?.let { gotoSessionId = it.sessionId }
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
                //
                // ...and *go to it*, but only if the request was actually sent. Creating
                // a shell and leaving the user on the screen that created it is a shell
                // they cannot reach; navigating to an empty terminal for a request that
                // never left the phone (offline) is the same stall with a worse message.
                // When it was refused, `create` says so itself (the error banner on this
                // screen) and the user stays here. The going is the `gotoSessionId`
                // request above, made when the daemon answers.
                //
                // The selection is deliberately **not** cleared here. It used to be,
                // and that silently closed every shell already open: the page set is
                // what ◀ ▶ walk, so opening a second shell left one page and the
                // arrows with nowhere to go. Creating a shell is not a reason to let
                // go of the others.
                if (client.create("shell", path.ifBlank { null })) {
                    pendingOpens = 1
                    showLauncher = false
                }
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
        // The close confirmation and Settings are drawn once, below both branches.
        // They were drawn here as well, so on the launcher each opened twice, one
        // dialog stacked on the other.
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
                            pageOrder = emptyList()
                            showLauncher = true
                        },
                        press = press,
                        shownId = shownSessionId.value,
                        onShow = { id -> shownSessionId.value = id },
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
                        shownId = shownSessionId.value,
                        Modifier.fillMaxSize(),
                        keyboardRequest = keyboardRequest,
                        pendingOpen = pendingOpens > 0,
                        // The attach's own verdict about the shell it was attached
                        // to, which is not the same signal as the session list: the
                        // list arrives when the list socket is up, and a page left
                        // behind is a frozen frame with nothing to read.
                        onShellGone = { id -> pageOrder = pageOrder - id },
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
                    keyboardUp = imeUp,
                    onToggleKeyboard = {
                        // Decided by what is on screen, not by the flag: the system
                        // back gesture puts the keypad away without telling us, and
                        // a toggle that followed the flag would then need two taps.
                        if (imeUp) {
                            keyboardWanted = false
                        } else {
                            keyboardWanted = true
                            // The page on screen takes focus and shows it — needed
                            // after a system dismiss, which took focus as well.
                            keyboardRequest++
                        }
                    },
                    menuExpanded = portraitMenu,
                    onMenu = { portraitMenu = true },
                    onMenuDismiss = { portraitMenu = false },
                    onLauncher = {
                        // The launcher is where you choose what to open, so going
                        // back to it lets go of everything: a page is a *hold* on a
                        // shell, and holding shells you are not looking at is how
                        // the phone keeps a session someone else might want.
                        pageOrder = emptyList()
                        showLauncher = true
                    },
                    modifiers = currentHandle?.modifiers ?: Modifiers(),
                    onSticky = { which -> currentHandle?.toggleModifier(which) },
                    press = press,
                    shownId = shownSessionId.value,
                    onShow = { id -> shownSessionId.value = id },
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

    // A pairing link from outside the app (ADR-0017): asked about, never applied
    // silently, because it replaces where every keystroke goes.
    val inbound by PairingInbox.link.collectAsState()
    inbound?.let { raw ->
        when (val decision = inboundPairing(raw, config)) {
            InboundPairing.Invalid -> NotACode(onDismiss = { PairingInbox.clear() })
            is InboundPairing.Same -> LaunchedEffect(raw) {
                // Already paired with exactly this: nothing to ask. The name is kept
                // current, since that is the one part that may differ.
                store.pair(decision.pairing)
                PairingInbox.clear()
            }
            is InboundPairing.Ask -> ConfirmPairing(
                decision.pairing,
                onDismiss = { PairingInbox.clear() },
                onConfirm = {
                    store.pair(decision.pairing)
                    PairingInbox.clear()
                    showSettings = false
                },
            )
        }
    }

    if (showSettings) {
        SettingsDialog(
            current = config,
            onDismiss = { showSettings = false },
            onSave = {
                store.update(it)
                showSettings = false
            },
            onPaired = { p ->
                store.pair(p)
                showSettings = false
            },
            onUnpair = { store.unpair() },
        )
    }
}
