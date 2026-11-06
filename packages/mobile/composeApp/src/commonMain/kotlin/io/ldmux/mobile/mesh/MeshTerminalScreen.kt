package io.ldmux.mobile.mesh

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
import io.ldmux.mobile.voice.VoiceState
import io.ldmux.mobile.voice.VoiceBanner
import io.ldmux.mobile.voice.Dictation
import io.ldmux.mobile.NewShellRow
import io.ldmux.mobile.shellRowTitle
import io.ldmux.mobile.SidePanel
import io.ldmux.mobile.BackToLauncher
import io.ldmux.mobile.AppLifecycle
import io.ldmux.mobile.BackgroundSession
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.ldmux.mobile.Accent
import io.ldmux.mobile.Bg
import io.ldmux.mobile.Dim
import io.ldmux.mobile.Fg
import io.ldmux.mobile.LdmuxConfig
import io.ldmux.mobile.LocalTouch
import io.ldmux.mobile.TerminalSurfaceFor
import io.ldmux.mobile.AppHeader
import io.ldmux.mobile.ChoiceRow
import io.ldmux.mobile.HoldOrientation
import io.ldmux.mobile.OrientationPolicy
import io.ldmux.mobile.OpenBar
import io.ldmux.mobile.openLabel
import io.ldmux.mobile.PortraitBar
import io.ldmux.mobile.ScrollPad
import io.ldmux.mobile.Session
import io.ldmux.mobile.Warn
import io.ldmux.mobile.tildify
import io.ldmux.mobile.MonoFont
import kotlin.random.Random
import kotlin.time.TimeSource
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import androidx.compose.ui.draw.clip
import io.ldmux.mobile.IconBtn
import io.ldmux.mobile.IconPopOut
import io.ldmux.mobile.NewShellModal
import io.ldmux.mobile.RadiusMedium
import io.ldmux.mobile.Surface
import kotlinx.coroutines.Job
import kotlinx.coroutines.withTimeoutOrNull
import io.ldmux.mobile.AccentBtn
import io.ldmux.mobile.HeldElsewhere
import io.ldmux.mobile.Notice
import io.ldmux.mobile.NoticeLine

/**
 * How long a sync waits for the host's answer before the screen stops waiting and says so.
 *
 * A radio round trip is about a second and a full screen can be several records, so this is
 * generous on purpose; a link that has run out of budget never answers, and that is the case this
 * exists for. Longer than this and a person is sitting on a screen that looks live and is not.
 */
internal const val SYNC_ANSWER_TIMEOUT_MS = 20_000L

/**
 * How long the measured grid must stop moving before it is sent to the host.
 *
 * Long enough to outlast the keyboard animation (about 200 ms on the devices measured) and short
 * enough that a rotation does not feel delayed. The cost of not waiting is a spurious `Resize`
 * record — which in manual-sync mode is a sync the user paid for — and a shell that starts at the
 * keyboard-down height and is then fitted to the real one.
 */
internal const val GRID_SETTLE_MS = 250L

/**
 * How long the first fit waits for the keyboard to appear before sending the size it has.
 *
 * The keyboard is opened by the app when a terminal appears, and it is an animation plus an IME
 * start: on the device measured it arrived about half a second after the shell did. While it is on
 * its way the surface's grid is the keyboard-DOWN one, and sending it costs a record and starts
 * the shell's program at the wrong height. This bound exists so a keyboard that never appears
 * (a hardware keyboard, a refused IME) cannot leave the host at the old size for ever.
 */
internal const val IME_WAIT_MS = 2_000L

/**
 * How long a link may be down before this route leaves the held screen for the shell list.
 *
 * Longer than the tailnet's [io.ldmux.mobile.LINK_FAILURE_GRACE_MS] on purpose: the tailnet's
 * eight seconds is a socket's tolerance, while this route's own verdicts are slower — the driver
 * calls a session dead after twenty seconds of silence and retries its first handshake at eight —
 * so eight seconds would abandon a rebuild that is working. See the effect that uses it.
 */
internal const val MESH_FAILURE_GRACE_MS = 30_000L

/**
 * The longest wait before a dropped link is rebuilt, for the Nth rebuild of this screen.
 *
 * **The ladder keeps climbing and it never stops.** Two seconds doubling to a 256-second cap
 * (in seconds: 2, 4, 8, 16, 32, 64, 128, 256): a link that comes back is picked up within four minutes at
 * worst, and a link that is gone costs one handshake every four minutes rather than one every
 * thirty seconds — which matters because a handshake is several records against an hourly budget
 * of dozens. The refresh square is the immediate retry for a user who is watching.
 *
 * Pure, so the ladder is a value a test can hold rather than an expression inside an effect. The
 * wait itself is drawn below the cap (full jitter): two ends that double in lockstep keep meeting.
 */
internal fun rebuildCapMs(rebuilds: Int): Long =
    (2_000L shl rebuilds.coerceIn(0, 7)).coerceAtMost(300_000L)

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
    /**
     * What the radio says its channel is, written back to the config so the public-channel rule is a
     * fact about this radio rather than the default. See the effect below for why the screen does
     * this and the settings dialog no longer has to.
     */
    onRadioClassified: (Boolean) -> Unit = {},
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
    // **Which screen this connection is showing.** Declared here rather than beside the list
    // because the rebuild below has to be able to land on it: a handshake is a new session, and
    // the screen it lands on is the list, not the shell the old session was in.
    var showList by remember(conn) { mutableStateOf(false) }
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
        // **Only a state the user must cure stops the retry.** `NoPath` and `Refused` used to be
        // in this set, and both are the radio's word about one record — a busy transmit table, a
        // path that has not resolved yet — so a transient refusal ended the retry loop for good.
        // That is the reported "the host is not connected or out of range", cured by one refresh
        // and working two seconds later: the refresh built the connection the loop had stopped
        // building. A missing radio or a missing permission is the user's to fix, and nothing
        // this screen does can.
        val terminal = conn.driver.view.value
        if (terminal is MeshTermView.LinkDown && terminal.link in setOf(
                MeshLinkState.NeedsRadio,
                MeshLinkState.NeedsPermission,
            )
        ) {
            return@LaunchedEffect
        }
        // **Full jitter, because two ends that double in lockstep keep colliding.** The wait is
        // drawn uniformly below the ladder's cap ([rebuildCapMs]): a phone and a host that both
        // back off on 2, 4, 8, 16 meet again on 2, 4, 8, 16, which is the shape every account of a
        // reconnect storm prescribes against.
        val cap = rebuildCapMs(rebuilds)
        rebuilds += 1
        delay(Random.nextLong(1, cap + 1))
        if (AppLifecycle.foreground) {
            // **A rebuilt connection lands on the shell list, not in the shell it was in.** A
            // handshake is a new session: the host may have restarted, the shell may be gone, and
            // the screen the user was looking at is a picture of a session that no longer exists.
            // Landing in it would show a stale screen under a live link. The list is the truth
            // about what the host has now, and opening a shell from it is one tap.
            showList = true
            resumeShell = ""
            attempt += 1
        } else ended = true
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
    // **A shell that was just opened shows itself, without a timer.** This used to be a 1.5 s
    // `LaunchedEffect` that pulled the held screen, and it was a workaround: the real cause was
    // that the hold judged "has anything been painted" over the source's whole life, so a new
    // shell's first screen was held as if it were the second. On a public channel a full screen
    // takes longer than 1.5 s, so the timer fired on nothing and the page stayed blank anyway.
    // `MeshTerminalSource.show` now knows which shell it is drawing and treats the first screen of
    // a new one as first, so opening a shell paints with no timer and no button.

    DisposableEffect(source) {
        onDispose { source.close() }
    }
    // **Held text does not outlive the screen.** The batcher keeps what the user typed in plain
    // heap memory until it is sent, which in manual-sync mode can be minutes. Leaving the app
    // backgrounded it there, and an Android recents thumbnail or a heap dump would show a password
    // that was never sent. The keys are encrypted at rest; the text waiting to be typed into a
    // shell should not survive the screen at all.
    DisposableEffect(source) {
        val remove = AppLifecycle.onBackground { source.scrub() }
        onDispose { remove() }
    }
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

    // **Public-channel mode is forced on a public channel** (ADR-0026): a tuple other operators'
    // radios share means every keystroke is a broadcast, so held input is the only supported way
    // to type there. On a private tuple the mode is the user's choice. `meshPublicChannel` is a
    // fact derived from the radio's own tuple; unknown reads as public, because that is the safe
    // reading.
    // **The mode is the config's, not the screen's.** It is chosen in Settings beside the channel
    // (a public channel forces it), so this effect is the only place the source learns it, and it
    // re-runs when the setting changes without rebuilding the connection.
    LaunchedEffect(conn, config.meshHoldsInput) {
        source.setPublicChannel(config.meshHoldsInput)
    }

    // **The tuple is read from the radio, so the answer is a fact and not a default.** It used to
    // be written only by the radio dialog, which meant an install that never opened that dialog
    // was classified "public" — the safe default — and then *forced* into held typing while the
    // bar said "the radio is on a public channel", which on a private tuple was simply false. The
    // link reads `SELF_INFO` as part of its own handshake (`onReady`), so the screen can hear the
    // answer without asking anyone to open a dialog. Saving it does not rebuild the connection:
    // `connectionKey` excludes this field on purpose.
    val classifiedNow by rememberUpdatedState(config.meshPublicChannel)
    LaunchedEffect(conn) {
        conn.radio.selfInfo.collect { info ->
            if (info == null) return@collect
            val preset = RadioPreset(
                frequencyMhz = info.frequencyMhz,
                bandwidthKhz = info.bandwidthKhz,
                spreadingFactor = info.spreadingFactor,
                codingRate = info.codingRate,
                txPowerDbm = info.txPowerDbm,
                // The channel is four fields and path width is not one of them, so the firmware's
                // stamp width cannot change this answer; 1 stands in for "not read here".
                pathHashSize = 1,
            )
            val public = isPublicChannel(preset)
            if (public != classifiedNow) onRadioClassified(public)
        }
    }

    // The local echo: what is held is drawn at the cursor of the host's last screen, so typing
    // looks like typing. It is rebuilt from the host's screen on every change (see `echoHeld`),
    // so backspace, a line end and a long line cannot make it drift from what it is a fake of.
    LaunchedEffect(source, handle.echoText, source.publicChannel) {
        if (source.publicChannel) source.echoHeld(handle.echoText) else source.echoHeld("")
    }

    // A sync whose answer never comes must not leave the screen frozen with no word about it: the
    // link may be out of budget or down, and the honest answer is "here is the newest screen we
    // hold, and no answer came". Twenty seconds is several radio round trips — long enough not to
    // cry wolf on a slow link, short enough that a person does not sit on a dead screen.
    var syncNotice by remember { mutableStateOf<String?>(null) }
    // **Keyed on the press count, not on "a wait is running".** It used to be keyed on
    // `awaitingSync`, which the deadline's own action sets to false — so the notice was written
    // into a state change that re-keyed the effect, whose first line nulled the notice. The message
    // the ADR promises was rendered for at most one frame. A Boolean also cannot express "the user
    // pressed sync again", so a second press 15 s into a 20 s wait inherited 5 s of clock it had not
    // earned. A count restarts the deadline on every press and nothing else.
    LaunchedEffect(source.syncSeq) {
        if (source.syncSeq == 0) return@LaunchedEffect
        delay(SYNC_ANSWER_TIMEOUT_MS)
        // Reports through the bar's own status line ("no answer yet: the link may be busy or down")
        // rather than a notice over the terminal: the row is where the mode speaks, and the answer
        // that arrives late is still drawn when it arrives.
        source.syncTimedOut()
    }
    // **Held text belongs to one shell.** A switch clears the screen, and the buffer would go to the
    // shell the user switched *to* — text typed for one shell, sent into another, without a press
    // being repeated. Discarded rather than sent, and said so for a few seconds. Keyed on the shell
    // id and not on `clear()`: `clear()` also runs on a rotation's `Resize`, where throwing away what
    // was typed would be a bug of its own.
    var heldFor by remember(conn) { mutableStateOf(shells.current) }
    LaunchedEffect(shells.current) {
        val now = shells.current ?: return@LaunchedEffect
        if (heldFor != null && heldFor != now) {
            if (handle.hasPending) {
                handle.discardHeld()
                syncNotice = "held text discarded: the shell changed"
            }
            // One shell's rows are never under another's name: the engine and its scrollback go
            // when the shell does. Here rather than on `ProfileMissing`, which a rotation also
            // produces — see the note beside `toShow`.
            source.clear()
        }
        heldFor = now
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
    // terminal for a screen with the header and the list, and so does this one. The state itself is
    // declared above, beside the rebuild that lands on it.
    // **Which shells are pages, and nothing else: the tailnet's rule.** A row is ticked, not opened,
    // and the one `open` at the bottom makes exactly the ticked rows the pages the header's arrows
    // walk, in the order they were ticked. Before there was a selection every shell the host listed
    // was a page, so a header reading `1 / 2 shells` followed a tap on one row, and the tap itself
    // attached at once with no way to choose a second.
    var chosen by remember(conn) { mutableStateOf(listOf<String>()) }
    // The launcher's own create dialogue, over the radio: it asks where, and the host starts the
    // shell. This is what the mesh route lacked — it could view the desk's shells and not start one.
    var newShellOpen by remember(conn) { mutableStateOf(false) }
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
    // **The list asks for the host's shells when it is shown over a live link.** After a rebuild
    // the screen lands here with nothing listed yet, and the list is the only thing that can say
    // what the host has now. `listShells` is idempotent while a list is in flight, so this costs
    // one record rather than one per frame.
    LaunchedEffect(showList, terminalLive, shells.entries.size) {
        if (showList && terminalLive && shells.entries.isEmpty()) conn.driver.listShells()
    }
    // Leaving the terminal is the user's decision, so it is also the one that cancels the resume: a
    // rebuild after this must land on the list, not drag them back into a shell they left.
    val openList = {
        chosen = emptyList(); showList = true; resumeShell = ""
        scope.launch {
            // Ask for the list first, then let the shell go: the host restores the pty the phone
            // displaced and a desk window can take the shell back, which is what the tailnet
            // launcher does when a page closes. The session stays, so re-opening costs no handshake.
            conn.driver.listShells()
            conn.driver.releaseShell()
        }
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
    // **A refusal the user did not ask for is not a page.** A resumed shell (or the launch attach)
    // can be refused while nothing was opened, and drawing the takeover there put it on the first
    // screen the user saw — a page state with no page. `pageOrder` is only ever written by
    // `openChosen`, so an empty one means "the user has not opened anything this session": land on
    // the home list, where the row says the shell is on the desktop and opening it is deliberate.
    LaunchedEffect(view) {
        if (view is MeshTermView.HeldBy && pageOrder.isEmpty()) openList()
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
    // **Enabled while the screen is holding a dead link too.** It used to require `terminalLive`,
    // so during a rebuild the one control that could leave a frozen screen with no other control
    // on it was switched off — and the frozen screen has no header, no keys and no bar, because
    // holding draws the terminal alone. The gesture that leaves has to leave from there.
    BackToLauncher(enabled = !showList) { openList() }

    // **The shell list is this route's launcher, and a launcher stays portrait** (ADR-0011).
    // The tailnet route holds portrait for its list and every dialog opened from it; this route
    // never did, so the same screen turned over on the mesh side and not on the other. The
    // terminal itself still follows the device — that is the whole point of the rotation work —
    // and leaving the list restores `FollowDevice` by construction ([HoldOrientation]).
    if (showList) HoldOrientation(OrientationPolicy.Portrait)

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
    //
    // **Thirty seconds, not the tailnet's eight.** Eight seconds is a WebSocket's tolerance for a
    // silent socket. This link's own verdict is slower by design: the driver calls a session dead
    // only after [MeshTermDriver]'s twenty-second silence rule, and its first handshake retry is
    // eight seconds — so a grace of eight seconds fires *inside* a retry that is still working, and
    // measured on the device 2026-11-04 a rotation whose resize answer was still in flight was
    // declared a dead link, which threw the user out of the shell and then — because the list is
    // portrait-only (ADR-0011) — rotated the phone back to portrait under them. A rotation is not
    // a failure, and a slow answer is not a dead link.
    LaunchedEffect(holding) {
        if (!holding) return@LaunchedEffect
        delay(MESH_FAILURE_GRACE_MS)
        if (holding) openList()
    }

    // **The mode's row, wherever the chrome goes.** One control, one row, drawn as part of the
    // screen's own layout: above the key pad in portrait, at the foot of the side panel in
    // landscape. It used to be a card floating over the bottom-right of the terminal, which covered
    // the shell's last line and its cursor — the two things a terminal is read for.
    // **The row exists only in batch mode.** In live mode there is nothing to hold and
    // nothing to release, so a sync button would be a control for a mode that is not running —
    // and the mode is not switched from here any more: it is a property of the link, set in
    // Settings beside the channel. What is left in the terminal is the one control the mode
    // needs, and it is drawn in every state of that mode (dimmed when it cannot act).
    val barRow: (@Composable (Modifier) -> Unit)? = if (config.meshHoldsInput) {
        { m ->
            PublicChannelBar(
                summary = handle.heldSummary,
                awaiting = source.awaitingSync,
                reported = source.answerOverdue,
                flushing = source.flushingHeld,
                updates = source.heldUpdates,
                onSync = { source.sync() },
                modifier = m,
            )
        }
    } else null

    Column(Modifier.fillMaxSize().background(Bg).safeDrawingPadding().imePadding()) {
        if (holding) {
            Column(Modifier.weight(1f).fillMaxWidth()) {
                // The one line that explains the pause, in the terminal's own type so it reads as the
                // link talking rather than as chrome.
                Text(
                    meshTermLabel(view),
                    color = Dim, fontSize = 11.sp, fontFamily = MonoFont,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
                )
                val keep = heldScreen
                LaunchedEffect(source, keep, shells.current) { if (keep != null) source.show(keep, shells.current) }
                TerminalSurfaceFor(
                    source = source,
                    key = resumeShell,
                    config = config,
                    typing = false,
                    active = false,
                    modifier = Modifier.weight(1f).fillMaxWidth(),
                    // Buttons, not a swipe: see the parameter's note.
                    scrollGestures = false,
                )
            }
        } else if ((terminalLive || view is MeshTermView.HeldBy) && !showList) {
            // **A held shell is a terminal page, not a bare view.** The tailnet route draws its
            // `in use on X / take it back` notice *over* the terminal, so the key bar, the pages and
            // the way back are all where they always are. This route invented a page with nothing
            // but the notice, which read as a fifth kind of screen between the list and a shell.
            // The terminal, and nothing above it: the tailnet's terminal has no header either. The
            // header belongs to the list screen, which is where Settings is, as on the tailnet.
            val pages = meshPagesFor(shells, pageOrder)
            val shownId = shells.current ?: pages.firstOrNull()?.id
            // **The panel *is* [SidePanel], with the mode's row in its scrolling area.** It used
            // to be a Column wrapping the panel with the row beneath it, and that wrapper broke
            // the landscape layout: with no width of its own it measured its children against the
            // whole Row, and the `fillMaxWidth()` rows inside took all of it — so the terminal's
            // Column, the Row's weighted child, was measured at zero width. Measured on the
            // device after rotating to landscape: `surface <shell> viewport 0x950`, a black
            // terminal that never asked the host to fit and so never repainted. The tailnet route
            // has no wrapper, which is why only this route went blank. The row also belongs at
            // the *top* of the scroll, beside the keys it sits above, not at the panel's foot.
            val panel: @Composable () -> Unit = {
                SidePanel(
                    pages = pages,
                    pagerState = null,
                    scope = scope,
                    handles = pages.associate { it.id to (source as io.ldmux.mobile.TerminalHandle) },
                    onVoice = {
                        Dictation.toggle(onText = { source.sendText(it) }, onState = { voice = it })
                    },
                    onLauncher = { openList() },
                    press = handle::sendKey,
                    shownId = shownId,
                    onShow = { id -> scope.launch { conn.driver.selectShell(id) } },
                    modeRow = barRow,
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
                    // **Draw what the driver committed; do not clear on a re-description.** The old
                    // version cleared the source whenever the view was `ProfileMissing`, which is
                    // produced by a *shell switch* and by a `Resize`/rotation alike. On a rotation
                    // the clear raced the host's next screen: the screen was drawn into the engine
                    // and the clear erased it, so the terminal sat black until the shell next
                    // changed. A resize keeps the old screen up until the new one arrives — the same
                    // thing the tailnet's surface does — and the switch clears in the shell effect
                    // below, where "the shell changed" is actually known.
                    val toShow = meshScreenToShow(view)
                    LaunchedEffect(source, toShow, shells.current) {
                        if (toShow != null) source.show(toShow, shells.current)
                    }
                    val drawn by source.frame.collectAsState()
                    // The host fits its pty to what the surface measures (`Resize`, profile version
                    // 3), so the font is the user's own and a line is never wider than the screen.
                    DisposableEffect(source, conn) {
                        var pending: Job? = null
                        // **The grid is sent once it has settled, not on every layout pass.**
                        // Opening a shell animates the keyboard in, and the surface reports a grid
                        // per frame, so the first reports were the keyboard-DOWN size: the host
                        // fitted its pty to a pane the user was not looking at, the shell's program
                        // drew at that height, and a second `Resize` arrived a moment later to undo
                        // it. Two records where one is correct — and in manual-sync mode a record is
                        // a sync the user paid for, on a link that allows about 58 records an hour.
                        // Waiting for the measurement to stop moving picks the keyboard-UP size,
                        // which is the size the shell actually starts at.
                        // **The first fit waits for the keyboard, then for the size to settle.**
                        // A debounce alone was not enough: the IME is requested by the app and
                        // takes a moment to appear, so there was a quiet gap in which the
                        // keyboard-DOWN size looked settled, and it was sent (measured on the
                        // device: `46x32` at t+0, then `46x19` 500 ms later when the keyboard
                        // finished opening). So while the keyboard is meant to be up and is not
                        // yet on screen, nothing is sent: the layout pass that arrives once it is
                        // up carries the real grid. `keyboardWanted` is the user's own choice, and
                        // [IME_WAIT_MS] bounds the wait so a keyboard that never appears cannot
                        // leave the host at the pty's old size for ever.
                        val opened = TimeSource.Monotonic.markNow()
                        source.onGrid = grid@{ c, r ->
                            // **Leaving is not a resize.** The list shrinks the layout, the surface
                            // reports the smaller grid, and the pending job then sent a `Resize`
                            // for a shell this screen had just let go of — which the host applied
                            // before the detach and never restored, leaving the desk shell at the
                            // phone's list height (measured 2026-11-05: 46x35 after returning home).
                            if (showList) return@grid
                            pending?.cancel()
                            pending = scope.launch {
                                // Bounded, and it always ends in a fit. Returning early instead
                                // meant the keyboard never arriving (or arriving without another
                                // layout pass) left the host at the pty's old size for ever —
                                // measured on the device as no `mesh fit` line at all after a shell
                                // opened. Waiting is a delay, not a decision.
                                val left = IME_WAIT_MS - opened.elapsedNow().inWholeMilliseconds
                                if (left > 0 && keyboardWanted && !imeUp) {
                                    withTimeoutOrNull(left) {
                                        snapshotFlow { imeUp }.first { it }
                                    }
                                }
                                delay(GRID_SETTLE_MS)
                                conn.driver.setGrid(c, r)
                            }
                        }
                        onDispose {
                            pending?.cancel()
                            source.onGrid = null
                        }
                    }
                    Box(Modifier.weight(1f).fillMaxWidth()) {
                        val held = view as? MeshTermView.HeldBy
                        if (held != null) {
                            // The page is real — key bar, pager, way back — and only the surface is
                            // replaced: somebody else is using this shell right now, and taking it
                            // is one tap. Same component as the tailnet route, so the words and the
                            // behaviour are the same words and behaviour.
                            val holder = shells.entries.firstOrNull { it.id == held.shell }?.owner ?: ""
                            HeldElsewhere(holder) { scope.launch { conn.driver.takeover(held.shell) } }
                        } else {
                        TerminalSurfaceFor(
                            source = source,
                            key = shells.current ?: "mesh",
                            config = config,
                            typing = typing,
                            active = true,
                            modifier = Modifier.fillMaxSize(),
                            keyboardRequest = keyboardRequest,
                            // **No swipe and no momentum on the radio.** The scroll pad's three
                            // buttons are the only way to move: every scroll here costs a record on
                            // a shared channel, and a fling would spend airtime on scrolling nobody
                            // asked for. The tailnet keeps the swipe, where the screen is local.
                            scrollGestures = false,
                        )
                        }
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
                            onUp = { source.scrollPage(-1) },
                            onDown = { source.scrollPage(1) },
                            onBottom = { source.scrollToBottom() },
                            modifier = Modifier.align(Alignment.TopEnd),
                        )
                        VoiceBanner(voice, Modifier.align(Alignment.TopStart))
                        // What the link adds is said only while it is true, over the terminal like the voice
                        // banner, and never as a row: the tailnet's live screen is the terminal and the
                        // bar. The hour's records and the typing that waits are on the shell list.
                        // **The app's own notice vocabulary, with its own weight.** These lines
                        // were all painted in the fault colour, including the two that are merely
                        // information (`waiting for send budget`, `full-screen program`), which
                        // teaches the reader to distrust the colour. `Notice` distinguishes them,
                        // and it is the same type and renderer the tailnet route uses.
                        val notice: Notice? = when {
                            syncNotice != null -> Notice(syncNotice!!, ok = false)
                            source.scrollNotice != null -> Notice(source.scrollNotice!!, ok = true)
                            // The refusal drops the whole keystroke and sends nothing, so a
                            // notice that says "sync to send" over an empty buffer names a button
                            // that does nothing. Say what there is to do in each state.
                            handle.overflow && source.publicChannel && handle.hasPending ->
                                Notice("too much held: sync to send", ok = false)
                            handle.overflow && source.publicChannel ->
                                Notice("input refused: the held text is at its limit", ok = false)
                            handle.overflow -> Notice("too much typed: wait for it to send", ok = false)
                            live?.stalled == true -> Notice("waiting for send budget", ok = true)
                            live?.screen?.alternate == true ->
                                Notice("full-screen program: updates are large and slow", ok = true)
                            else -> null
                        }
                        if (notice != null) {
                            Box(Modifier.align(Alignment.TopCenter).background(Bg).padding(horizontal = 6.dp, vertical = 2.dp)) {
                                NoticeLine(notice)
                            }
                        }
                    }
                }
                if (!portrait && !config.panelLeft) panel()
            }
            if (portrait) {
                // The space above the keyboard: the mode's row (batch only), then the keys.
                barRow?.invoke(Modifier.fillMaxWidth())
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
                    // **Held text is counted, waiting text is quoted.** In public-channel mode the
                    // buffer can sit there for minutes, and a literal rendering of it would keep a
                    // password on screen long after it was typed. The count says everything the
                    // list needs to say. Outside the mode the buffer drains in a moment, so the
                    // literal is still the useful one — it is what will leave next.
                    if (handle.pendingLabel.isNotEmpty()) {
                        val line = if (source.publicChannel) {
                            "held: ${handle.heldSummary}"
                        } else {
                            "waiting to send: ${handle.pendingLabel}"
                        }
                        Text(line, color = Dim, fontSize = 11.sp, fontFamily = MonoFont)
                    }
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
                        onNewShell = { newShellOpen = true },
                        onClose = { id -> scope.launch { conn.driver.closeShell(id) } },
                        onMakeLive = { id -> scope.launch { conn.driver.makeLive(id) } },
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
                    // **Taking a shell from the desk is a tap the user makes.** The host refuses a
                    // plain attach while another device holds it, and this is the one action that
                    // says "yes, here instead" — the same choice the tailnet route offers, in the
                    // same order: the state first, then the verb.
                    Spacer(Modifier.weight(1f))
                }
                // The launcher's own create dialogue, over the radio. The host starts the shell and
                // says its id; the driver attaches to that one (version 7).
                if (newShellOpen) {
                    NewShellModal(
                        onDismiss = { newShellOpen = false },
                        onCreate = { path ->
                            newShellOpen = false
                            // **The screen has to leave the list, not just the driver.** Creating a
                            // shell starts it on the host and attaches the link, but `showList` is
                            // this screen's own state: without it the new terminal existed and the
                            // list stayed in front of it, which read as "create did nothing".
                            showList = false
                            scope.launch { conn.driver.createShell(path) }
                        },
                    )
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
    /** Start a shell on the host (version 7). The same row the tailnet launcher has. */
    onNewShell: () -> Unit,
    /** End a shell this phone started, or put it down when a desk window holds it. */
    onClose: (String) -> Unit,
    /** Open one of this phone's shells in a desk window on the host. */
    onMakeLive: (String) -> Unit,
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
                    title = shellRowTitle(e.label, e.cwd),
                    subtitle = meshRowSubtitle(e),
                    chosen = e.id in chosen,
                    ordinal = picked.indexOfFirst { it.id == e.id }.takeIf { it >= 0 }?.plus(1),
                    inUse = e.id == current,
                    onClick = { onToggle(e.id) },
                    // **The same rule as the tailnet launcher**: only a shell this phone started can
                    // be ended or moved from the phone. The host enforces it too — `close` on a desk
                    // shell puts it down rather than destroying it — so the row and the host agree.
                    mobileShell = e.openedByPhone,
                    onKill = if (e.openedByPhone) ({ onClose(e.id) }) else null,
                ) {
                    if (e.openedByPhone) {
                        IconBtn(onClick = { onMakeLive(e.id) }, label = "send to desktop") {
                            IconPopOut(Dim)
                        }
                    }
                }
            }
            // The same row the tailnet launcher draws, from the same composable.
            item(key = "+shell") { NewShellRow(onClick = onNewShell) }
        }
        Spacer(Modifier.height(LocalTouch.current.rowPad))
        OpenBar(openLabel(picked.size), enabled = picked.isNotEmpty()) { onOpen(picked) }
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
    Text("$text · ${secs} s", color = Dim, fontSize = 12.sp, fontFamily = MonoFont)
}
