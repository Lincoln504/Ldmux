package io.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.heightIn
import androidx.compose.ui.draw.alpha
import androidx.compose.foundation.selection.toggleable
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import io.ldmux.mobile.mesh.ShellMark
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * What the launcher shows: the machine's desktops, the windows on them, the pi
 * agent sessions running inside those windows, and the ldmux shells already open
 * — in that order, because that is the order of the question being asked. "Which
 * desktop, which window, which agent, which shell of mine."
 *
 * Two things can be *chosen* rather than merely shown:
 *
 *   - **Shells and pi sessions**, by tapping. What you choose becomes the set the
 *     terminal pages through with ◀ ▶, and the `open` button opens exactly that set
 *     — it is inert, and dimmed, with nothing chosen. (This line said "nothing
 *     chosen means everything, so the screen still works as a plain session list"
 *     until 2026-09-27; there is no such mode, and the dimmed button says so.)
 *   - **A desktop workspace**, by tapping its target ring. That is where a shell
 *     opened from here will be put, and it is a deliberate choice: the previous
 *     behaviour picked "the last one" for you, which meant a shell could appear on
 *     a desktop you were not looking at, or bounce your view to another one.
 */
@Composable
fun LauncherScreen(
    /** The line under the title: what this is paired with and how the link is ([linkLabel]). */
    link: LinkState,
    sessions: List<Session>,
    desktop: DesktopInventory?,
    desktopLoading: Boolean,
    onRefresh: () -> Unit,
    onOpen: (List<LauncherEntry>) -> Unit,
    onKill: (Session) -> Unit,
    onMakeLive: (Session, String) -> Unit,
    onCloseLive: (Session) -> Unit,
    /** Start a shell — the host's login shell — where the user said. */
    onNewShell: (path: String) -> Unit,
    liveResult: LiveResult?,
    onDismissLiveResult: () -> Unit,
    onSettings: () -> Unit,
    /**
     * The marks on shells, by session id: what each one last said about itself.
     *
     * Drawn beside the name, cleared by opening the shell. Empty is the normal state and reads as
     * "nothing to say", which is also what a daemon that predates the marks means.
     */
    marks: Map<String, ShellMark> = emptyMap(),
    /**
     * The link's own line, when it has something to say (a wait, a failure). Null while it is
     * healthy and while it is unpaired, which say it elsewhere or not at all.
     */
    linkText: String? = null,
    /**
     * Under the header when the saved route cannot run: what is missing, and the one action that
     * helps. Drawn only by the caller that knows the route's problem; the tailnet draws nothing.
     */
    routeNotice: (@Composable () -> Unit)? = null,
) {
    // Which shells and pi sessions are chosen, **in the order they were chosen**.
    // Keys, not objects, so a list that refreshes under the user does not silently
    // drop a selection; a list, not a set, because the order is the page order —
    // page 1 is the first row you ticked, and that number is drawn in the row
    // (see `ChoiceRow`) so the tick box says what it is doing.
    var chosen by remember { mutableStateOf(listOf<String>()) }
    var newShellOpen by remember { mutableStateOf(false) }
    // Which shell the desktop picker is choosing for. Null means closed.
    var pickerFor by remember { mutableStateOf<Session?>(null) }

    // The desktop is fetched when the screen appears with a link (App's effect on
    // `showLauncher` and the link) and when asked for, never on a timer: it is a run
    // of window-manager and /proc reads on the host. It was also requested from
    // here on appear, so every visit asked twice.

    val openShells = sessions.filter { it.attachable }
    val entries = launcherEntries(openShells)

    Box(Modifier.fillMaxSize()) {
    Column(Modifier.fillMaxSize().background(Bg).padding(LocalTouch.current.rowPad * 3)) {
        // ── header ───────────────────────────────────────────────────────────
        // The same header every route draws (AppHeader.kt): the name, then refresh and settings
        // as one pair of squares.
        AppHeader(
            title = "ldmux",
            onRefresh = onRefresh,
            refreshing = desktopLoading,
            onSettings = onSettings,
        )
        linkText?.let { line ->
            Text(line, color = Dim, fontSize = 11.sp)
            Spacer(Modifier.height(LocalTouch.current.rowPad))
        }

        // Where a new shell goes is asked for in the dialogue that creates it, and
        // where a pushed window goes is asked for when it is pushed. There used to
        // be a row of target chips up here that set a mode for both, which is a
        // choice made in one place and spent in another — and the button that spent
        // it did nothing at all until a chip had been pressed.
        // **The daemon's diagnostic paragraph is not main-menu copy.** It reads "no window manager
        // could be enumerated (i3/sway/niri/hyprland need their msg tool on PATH; other X11
        // desktops need xprop)" and was printed verbatim under the title at 10 sp, which is an
        // error wall above the list. The launcher says whether the desktop is readable; the
        // reason stays in the RPC for anyone who asks `ldmux desktop`.
        if (desktop?.detail != null) {
            Spacer(Modifier.height(LocalTouch.current.rowPad))
            Text("desktop unreadable", color = Dim, fontSize = 10.sp, maxLines = 1)
        }

        Spacer(Modifier.height(LocalTouch.current.rowPad * 2))
        routeNotice?.let { it(); Spacer(Modifier.height(LocalTouch.current.rowPad * 2)) }

        // ── one list ─────────────────────────────────────────────────────────
        // Unpaired, there is no list and nothing to open, and the app does not jump to a scanner
        // by itself (ADR-0017). The empty state is [UnpairedHome], drawn by the caller as the
        // route notice above; this branch only stops the list from existing.
        //
        // **The empty state used to be drawn here as well**, so a fresh install read "no
        // computer paired [scan a code]" and then, immediately under it, "not paired with a
        // host — run `ldmux code`, then scan it in Settings": two headlines for one state, the
        // second pointing at a cog that the first button had already made unnecessary. One
        // place owns the copy now, and it is the place that owns the two verbs.
        if (link == LinkState.Unpaired) {
            return@Column
        }
        run {
            LazyColumn(
                Modifier.weight(1f).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
            ) {
                // **First, not last.** A list is read from the top, and starting a shell is the
                // one action the list always offers; at the bottom it moved every time a shell
                // appeared, and on a full list it was below the fold.
                item(key = "+shell") { NewShellRow { newShellOpen = true } }
                items(entries, key = { it.key }) { e ->
                    val session = openShells.firstOrNull { it.id == e.sessionId }
                    ChoiceRow(
                        // The folder is always shown, because it is what tells two
                        // shells of the same kind apart. The process is the name.
                        title = shellRowTitle(e.label, e.cwd),
                        mobileShell = e.openedByPhone,
                        subtitle = rowSubtitle(e.cwd, e.state, e.shell),
                        stateWarn = e.state == NO_WINDOW_STATE,
                        inUse = e.inUseOnPhone,
                        chosen = e.key in chosen,
                        // The page this row becomes, 1-based, in the same numbering
                        // the pager's header shows.
                        ordinal = ordinalIn(entries, chosen, e.key),
                        mark = marks[e.sessionId],
                        onClick = { chosen = toggle(chosen, e.key) },
                        // Closing is the launcher's alone, and it is offered only
                        // for a shell this phone started: a shell opened at the
                        // desk is the desk's, and the phone putting it down is not
                        // the phone ending it. The daemon enforces the same rule —
                        // this is the UI agreeing with it rather than guessing.
                        onKill = session?.takeIf { e.openedByPhone }?.let { s -> { onKill(s) } },
                    ) {
                        // The per-shell menu belongs to shells this phone opened.
                        // A shell someone started at their desk is theirs to keep;
                        // the phone may look at it, but not move or end it.
                        if (e.openedByPhone && session != null) {
                            val live = e.liveWorkspace
                            // An icon instead of five words. A square with an arrow
                            // leaving its top-right is what every desktop uses for
                            // "open this somewhere else", and five words on every row
                            // is five words too many.
                            //
                            // Its *state* is not lost in the exchange: the workspace
                            // moved to the subtitle above, and the filled treatment
                            // says at a glance that the window is out there. A bare
                            // icon would have traded noise for a blind spot.
                            IconBtn(
                                onClick = { pickerFor = session },
                                label = when {
                                    live == null -> "send to desktop"
                                    else -> liveWorkspaceOf(live)?.let { "on desktop $it" } ?: "on desktop"
                                },
                                filled = live != null,
                            ) {
                                IconPopOut(if (live != null) Fg else Dim)
                            }
                        }
                    }
                }
                // Starting a shell is the list's last row: it is one more thing
                // you can open, and it sits where the list ends rather than
                // competing with `open` for the bottom of the screen.
                // Drawn as a row, on the rows' own background: it is one more entry in
                // the list, not a second call to action beside `open`.
            }
        }

        Spacer(Modifier.height(LocalTouch.current.rowPad))
        // Opening what is chosen is the screen's one action, so it has the width.
        // In the order chosen, and skipping anything that left the list while it was
        // chosen — a shell can die between two taps, and a page number that counts a
        // row nobody can see is a number that disagrees with the pager.
        val pickedNow = chosen.mapNotNull { key -> entries.firstOrNull { it.key == key } }
        // A chosen shell is not openable while the host is unreachable: the terminal it
        // would open cannot connect, and the header already says the link is down. The row
        // selection survives, so the action returns by itself when the link does.
        OpenBar(
            openButtonLabel(pickedNow),
            enabled = pickedNow.isNotEmpty() && link == LinkState.Online,
        ) { onOpen(pickedNow) }
    }

    if (newShellOpen) {
        NewShellModal(
            onDismiss = { newShellOpen = false },
            onCreate = { path ->
                newShellOpen = false
                onNewShell(path)
            },
        )
    }
    pickerFor?.let { session ->
        DesktopPickerModal(
            workspaces = desktop?.workspaces.orEmpty(),
            windows = desktop?.windows.orEmpty(),
            liveOn = session.liveWorkspace,
            onDismiss = { pickerFor = null },
            onPick = { ws ->
                pickerFor = null
                onMakeLive(session, ws)
            },
            onTakeOff = {
                pickerFor = null
                onCloseLive(session)
            },
        )
    }
    }
}

/**
 * One row of the launcher.
 *
 * There are no sections any more. Everything the launcher offers is a shell you
 * can open, and splitting that into "desktops", "pi sessions" and "shells"
 * made the reader pass three headers to reach one list. A header promises its
 * contents are a *different kind of thing*; these were not. The rows of window
 * titles under each desktop were worse still: a window manager's inventory, not
 * anything you could open.
 */
data class LauncherEntry(
    val key: String,
    /** The session this row opens. */
    val sessionId: String,
    /** `bash`, `vim`, `pi` — what the row *is*, not only where it is. */
    val process: String,
    /** The shell's name from the daemon: the conversation's title, else its folder. */
    val title: String,
    /** The shell the session was created with (`bash`), empty when the host has no record of it. */
    val shell: String = "",
    val cwd: String,
    /**
     * A phone is using it right now. Shown as a shade, not as words: the rows used
     * to say "in use on desktop" / "on ws 9", and the only state worth marking on a
     * list of shells you can open is the one that means *someone is connected*.
     */
    val inUseOnPhone: Boolean = false,
    val openedByPhone: Boolean = false,
    val liveWorkspace: String? = null,
    /** Whether this shell has ever had a desk window; see [shellStateLabel]. */
    val hadWindow: Boolean = false,
    /**
     * Where the shell itself lives, in words, on every row. A desk window is a
     * fact the phone cannot otherwise see, and a shell with no window is the one
     * state that used to be invisible (a pile of them looked like a bug). Blank
     * means the phone icon beside the name already says it.
     */
    val state: String = "",
) {
    /**
     * What the row is called.
     *
     * **The title, not the process.** A row used to be named after the program in the pane, so
     * every agent-run shell read as `pi` and the folder underneath was the only way to tell them
     * apart — which is the thing the operator asked to stop. The daemon's title is already the
     * conversation's name with the folder as its fallback (`session_title`), so this is the same
     * word the CLI prints.
     */
    val label: String get() = title
}

/**
 * The launcher's rows: the shells there are to open, and nothing else.
 *
 * Desk `pi` processes that are not in an ldmux shell used to be rows too, noted
 * "not in ldmux", offering to start a *new* phone shell in their directory. That
 * row could not be what it looked like — the user's pi on the computer — and
 * choosing it produced a phone shell, which is the bug reported on 2026-09-20 and
 * against "if there's nothing to open it shouldn't show up" (2026-09-18). A desk
 * pi that *can* be opened is inside a desk window, and so already is one of these
 * rows (the daemon names its process and reports its live directory).
 *
 * The order is the daemon's, and it is also the order pages are opened in.
 */
internal fun launcherEntries(openShells: List<Session>): List<LauncherEntry> =
    openShells.map { shell ->
        LauncherEntry(
            key = shell.id,
            sessionId = shell.id,
            process = shell.process,
            title = shell.title,
            shell = shell.shell,
            cwd = shell.cwd,
            inUseOnPhone = shell.ownerKind == "phone",
            openedByPhone = shell.openedByPhone,
            liveWorkspace = shell.liveWorkspace,
            hadWindow = shell.hadWindow,
            state = shellStateLabel(shell.liveWorkspace, shell.openedByPhone, shell.hadWindow),
        )
    }

/**
 * One statement of where a shell lives, and almost always nothing.
 *
 *   - a desk window: blank. A shell on the desk is the ordinary state, and the
 *     row itself is the statement; naming it on every row was noise (the user
 *     asked for the text to go, 2026-10-05).
 *   - a phone shell: blank, with the phone icon beside the name saying it.
 *   - neither: `no window` — the one state that is news, because a shell nothing
 *     is showing means something went wrong.
 */
internal fun shellStateLabel(
    liveWorkspace: String?,
    openedByPhone: Boolean,
    hadWindow: Boolean = false,
): String = when {
    liveWorkspace != null -> ""
    openedByPhone -> ""
    // A shell that never had a window is ordinary: `ldmux new` makes one, and so does anything
    // that opens a shell to attach to later. Only a shell whose window *went away* is the
    // anomaly worth an amber row, and the daemon has to say which it is (`hadWindow`).
    !hadWindow -> ""
    else -> NO_WINDOW_STATE
}

/**
 * The one state that is an anomaly rather than a resting place. A shell with no
 * window should not exist while the daemon is healthy: every desktop shell has a
 * window, and every phone shell has the phone. It is drawn in amber so the eye
 * stops on it, which is the point of showing it at all.
 */
internal const val NO_WINDOW_STATE = "no window"

/**
 * The row's second line: **where it is**, then where the shell itself lives.
 *
 * The folder is always drawn. It used to be dropped when the title was the folder's own name, to
 * avoid repeating a word — and that left every unnamed row with no place on it at all (measured
 * 2026-11-07: `ldmux`, `meshmore-sns`, `tmp` and `RCNT` each showed one word and nothing else),
 * while five shells in the home directory showed the same three characters. A row's name is a
 * promise about *what* it is; only the second line can say *where*.
 *
 * **Two rows may read the same, and that is allowed.** They were pulled apart with the pane's
 * program and then a piece of the session id, which put hex on a row nobody asked to identify —
 * and the operator's answer was that identical rows are fine.
 */
internal fun rowSubtitle(cwd: String, state: String, shell: String = ""): String {
    val folder = tildify(cwd)
    return listOf(folder, shell, state).filter { it.isNotBlank() }.joinToString("  \u00b7  ")
}

/**
 * The workspace a shell's live window is on, or null when there is not a usable one.
 *
 * `!= null` is not the test. The daemon records an **empty string** when the window opened
 * but i3 did not place it — a slow window manager, or i3 gone — because the window
 * genuinely exists and has to be tracked (it is writable, and a claim has to follow it).
 * That empty string then matched `liveWorkspace != null`, rendered a dangling
 * `on desktop ` with no noun in it, and **shadowed the branches below it**, so a row that
 * should have said "in use on desktop" said nothing useful at all.
 *
 * Blank means the placement is unknown, and unknown is not a fact to print. One function
 * for both call sites, because the two drifted the moment there were two.
 */
internal fun liveWorkspaceOf(workspace: String?): String? = workspace?.takeIf { it.isNotBlank() }

/** What the launcher's left button says, given what is chosen. */
internal fun openButtonLabel(picked: List<LauncherEntry>): String = openLabel(picked.size)

/** The same label for any list of chosen rows: the mesh route's shells are not [LauncherEntry]s. */
internal fun openLabel(count: Int): String = if (count == 0) "open" else "open ($count)"

/**
 * The list's one action, drawn like the rows above it (and like `+ shell`): the list's own
 * background and shape, rather than a filled accent bar. Inert and dimmed with nothing chosen.
 * Both routes draw this one, so choosing and then opening is the same gesture on either.
 */
@Composable
internal fun OpenBar(label: String, enabled: Boolean, onOpen: () -> Unit) {
    // `enabled` goes into `clickable`, not into the lambda. The bar used to stay a live
    // button whenever nothing was chosen and swallow the tap inside its own body: the
    // label dimmed, so it looked disabled, while a screen reader still announced "Button"
    // and a double-tap did nothing with no explanation. Every other control in the app
    // passes `enabled` through (`AccentBtn`, `MiniTextButton`); this one did not.
    Box(
        Modifier
            .fillMaxWidth()
            .clip(RadiusMedium)
            .background(Surface)
            .clickable(role = Role.Button, enabled = enabled) { onOpen() }
            .padding(vertical = LocalTouch.current.rowPad * 2),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = if (enabled) Fg else Dim, fontSize = 14.sp)
    }
}

/**
 * What a row is called.
 *
 * A shell the host has no name for must not be introduced by its id: the id is an attach selector,
 * twelve hex characters nobody recognises, and the operator asked for the folder instead. The label
 * the host sends is the name; failing that, the folder the shell is in; failing that, the plain word
 * `shell`. One rule, so the tailnet launcher and the radio route cannot disagree about it.
 */
internal fun shellRowTitle(label: String, cwd: String): String {
    if (label.isNotBlank()) return label
    val folder = cwd.trimEnd('/').substringAfterLast('/')
    if (folder.isNotBlank() && folder != "~") return folder
    if (cwd.trim() == "~" || cwd.trim() == "/") return cwd.trim()
    return "shell"
}

/**
 * The list's first row: starting another shell.
 *
 * Drawn as a row on the rows' own background, because it is one more entry in the list and not a
 * second call to action beside `open`. Extracted so the tailnet launcher and the radio route draw
 * the same thing by construction rather than by two copies kept in step by hand.
 */
@Composable
internal fun NewShellRow(modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .fillMaxWidth()
            .clip(RadiusMedium)
            .background(Surface)
            .clickable { onClick() }
            .padding(vertical = LocalTouch.current.rowPad * 2),
        contentAlignment = Alignment.Center,
    ) {
        // **One row, centred: the label then its mark.** A `Box` centres each child, so a second
        // child drawn with the label lands on top of it — measured on the device as "new⌷shell".
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("new shell", color = Fg, fontSize = 14.sp)
            // The same grey phone the shells it starts will carry: a shell created here is the
            // phone's, and the row that makes them says so before the fact rather than after it.
            Spacer(Modifier.width(5.dp))
            IconPhone(
                tint = Dim,
                box = 12.dp,
                modifier = Modifier.semantics { contentDescription = "starts a shell on this phone" },
            )
        }
    }
}

/** One workspace, as a chip to aim a new shell at. */
@Composable
internal fun ChoiceRow(
    title: String,
    subtitle: String,
    chosen: Boolean,
    onClick: () -> Unit,
    onKill: (() -> Unit)? = null,
    /** The row's state is an anomaly (amber), not a resting place (grey). */
    stateWarn: Boolean = false,
    /**
     * A shell this phone opened, marked beside its name.
     *
     * Which shells came from here matters on this screen and nowhere else: the
     * launcher is where you decide what to do with them, and only a shell this
     * phone started can be moved to the desktop or closed from it.
     */
    mobileShell: Boolean = false,
    /** A phone is connected to it: a shade of its own, between plain and chosen. */
    inUse: Boolean = false,
    /** The page this row becomes when opened (1-based), when it is chosen. */
    ordinal: Int? = null,
    /**
     * What the shell last said about itself: an agent finished a turn, or it wants a look.
     *
     * A small square beside the name rather than a notification: the list is the one screen that
     * shows every shell at once, so it is where "which of these are done" belongs. It is cleared by
     * opening the shell, which is the acknowledgement.
     */
    mark: ShellMark? = null,
    trailing: @Composable () -> Unit = {},
) {
    // How much of the row's own width the overlaid controls take: a control is its
    // glyph plus its own horizontal padding (`IconBtn`: 9.dp a side), and rows with
    // controls carry two of them with the row's 6.dp between. Zero for a row with
    // none, so a desk shell's address keeps the whole width.
    val controlWidth = LocalTouch.current.icon + 18.dp
    val trailingWidth = if (mobileShell) controlWidth * 2 + 6.dp else 0.dp
    // **The row is one height, whatever it carries.** The per-shell controls are
    // the tallest thing in any row — a control has a minimum touch height of
    // [MinTouch] (36 dp compact) and the text has none — so laying them out beside
    // the name made the two rows that carry them *thicker bars* than every other
    // row (measured on the phone, 2026-10-05: 64 dp against 53 dp). They are drawn
    // over the row's right end instead, matched to its height, so they cannot set
    // it. Their width is reserved out of the text, which is why the address ends a
    // little earlier on those rows.
    //
    // Two boxes and not a Box inside the Row: `matchParentSize` is what makes the
    // controls take the row's height *after* the row has been measured without
    // them, which is the whole trick.
    Box(Modifier.fillMaxWidth()) {
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RadiusMedium)
            // Being chosen is the checkbox's to say, and only the checkbox's: a tint
            // and an outline on top of a tick was three signals for one fact. The
            // row's own shade is reserved for "a phone is using this".
            .background(if (inUse) Accent.copy(alpha = 0.09f) else Surface)
            // **The row is a checkbox, and says so.** It was a plain clickable: a screen reader
            // announced "clickable" and nothing about whether this shell was chosen, while the
            // tick's state lived only in a drawn tick and a numeral. `toggleable` puts the role,
            // the state and the action in the semantics tree; the visual stays the tick box that
            // becomes a page number.
            .toggleable(value = chosen, role = Role.Checkbox, onValueChange = { onClick() })
            // The folder moved up beside the name, but the row keeps the height it
            // had with two lines: the list's rhythm must not change under the
            // finger. The 8.dp is the removed line's height, split over both sides.
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.rowPad + 8.dp)
            .padding(end = trailingWidth),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // A tick box that becomes a number, because these *are* the multi-select:
        // several become the pages you swipe through, and the order matters — page 1
        // is the first one ticked. A tick said "one of several" and left the reader
        // to guess which page this would be; the number says it, and it is the same
        // number the pager's header shows (`1 / 3 shells`). With one row ticked it
        // is `1`, so the control still answers "is this chosen" at a glance.
        Box(
            Modifier
                .size(LocalTouch.current.check)
                .clip(RadiusSmall)
                .background(if (chosen) Accent else Color.Transparent)
                .border(1.5.dp, if (chosen) Chosen else Dim, RadiusSmall),
            contentAlignment = Alignment.Center,
        ) {
            if (chosen) {
                if (ordinal != null) {
                    Text(
                        "$ordinal",
                        color = Bg,
                        fontFamily = MonoFont,
                        fontSize = 10.sp,
                        maxLines = 1,
                    )
                } else {
                    IconCheck(Bg, box = LocalTouch.current.check - 6.dp)
                }
            }
        }
        Spacer(Modifier.width(10.dp))
        // One line: what it is, then where it is, then (only if it is news) its
        // state. The row is a name; the folder is its address, small and grey
        // beside it. Two lines said the same thing twice and made every shell two
        // rows tall.
        Row(Modifier.weight(1f), verticalAlignment = Alignment.CenterVertically) {
            Text(
                title,
                color = Fg,
                fontSize = 14.sp,
                maxLines = 1,
                // A long hostname or a long shell name just stopped mid-character with no sign
                // that there was more. An ellipsis is the sign.
                overflow = TextOverflow.Ellipsis,
                fontWeight = FontWeight.Medium,
            )
            if (mobileShell) {
                // One space after the name, in the grey the address uses: a fact
                // about this row rather than a control, so it is not a button and
                // must not look like one. Described as well as drawn — a picture is
                // not a description.
                Spacer(Modifier.width(5.dp))
                IconPhone(
                    tint = Dim,
                    box = 12.dp,
                    modifier = Modifier.semantics { contentDescription = "opened on this phone" },
                )
            }
            if (subtitle.isNotBlank()) {
                Spacer(Modifier.width(8.dp))
                Text(
                    subtitle,
                    color = if (stateWarn) Warn else Dim,
                    fontSize = 11.sp,
                    maxLines = 1,
                    fontFamily = MonoFont,
                    modifier = Modifier.weight(1f, fill = false),
                )
            }
            if (mark != null) {
                Spacer(Modifier.width(7.dp))
                ShellMarkSquare(mark)
            }
        }
    }
    // The controls, over the row's own right end. `matchParentSize` keeps them out
    // of its measurement, and the height that is left is theirs at full touch size.
    Row(
        Modifier.matchParentSize().padding(end = 10.dp),
        horizontalArrangement = Arrangement.End,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        trailing()
        if (onKill != null) {
            Spacer(Modifier.width(6.dp))
            IconBtn(onClick = onKill, label = "close shell") { IconClose(Dim) }
        }
    }
    }
}


/** Add or remove a key from a selection, without mutating the set. */
private fun toggle(current: List<String>, key: String): List<String> =
    if (key in current) current - key else current + key

/**
 * The 1-based page a row would become, or null when it is not chosen.
 *
 * Counted over the rows that are still in the list (`ordinalIn` is why `pickedNow`
 * and this agree): a shell that died after being ticked must not keep its number
 * taken, or the launcher would say "open (2)" and show `1` and `3`.
 */
internal fun ordinalIn(entries: List<LauncherEntry>, chosen: List<String>, key: String): Int? {
    if (key !in chosen) return null
    val visible = chosen.filter { k -> entries.any { it.key == k } }
    return visible.indexOf(key).takeIf { it >= 0 }?.plus(1)
}


/**
 * The mark beside a shell's name: a small square, one per shell, two meanings.
 *
 * **Deliberately a square and not a notification.** The operator's ask was a list of squares on the
 * rows rather than a stream of notifications: a terminal that buzzes is one that gets uninstalled,
 * and the launcher already answers "what is running" in one glance. `done` reads as calm (the work
 * finished), `attention` as the interface's own amber (the shell is waiting on a person).
 *
 * Colour alone would be a message a screen reader cannot hear and a colour-blind reader cannot
 * separate, so each mark carries its own description.
 */
@Composable
internal fun ShellMarkSquare(mark: ShellMark, size: Dp = 9.dp) {
    val (colour, description) = when (mark) {
        ShellMark.Done -> Accent to "finished its turn"
        ShellMark.Attention -> Warn to "needs attention"
    }
    Box(
        Modifier
            .size(size)
            .clip(RadiusSmall)
            .background(colour)
            .semantics { contentDescription = description },
    )
}
