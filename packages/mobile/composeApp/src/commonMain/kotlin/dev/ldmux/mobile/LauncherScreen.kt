package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
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
 *     terminal pages through with ◀ ▶ — nothing chosen means everything, so the
 *     screen still works as a plain session list.
 *   - **A desktop workspace**, by tapping its target ring. That is where a shell
 *     opened from here will be put, and it is a deliberate choice: the previous
 *     behaviour picked "the last one" for you, which meant a shell could appear on
 *     a desktop you were not looking at, or bounce your view to another one.
 */
@Composable
fun LauncherScreen(
    host: String,
    sessions: List<Session>,
    desktop: DesktopInventory?,
    desktopLoading: Boolean,
    onRefresh: () -> Unit,
    onOpen: (List<LauncherEntry>) -> Unit,
    onKill: (Session) -> Unit,
    onMakeLive: (Session, String) -> Unit,
    onCloseLive: (Session) -> Unit,
    liveResult: LiveResult?,
    onDismissLiveResult: () -> Unit,
    onSettings: () -> Unit,
) {
    // Which shells and pi sessions are chosen. Keys, not objects, so a list that
    // refreshes under the user does not silently drop a selection.
    var chosen by remember { mutableStateOf(setOf<String>()) }
    var targetWorkspace by remember { mutableStateOf<String?>(null) }

    // The desktop is fetched when the screen appears and when asked for, never on
    // a timer: it is a run of window-manager and /proc reads on the host.
    LaunchedEffect(Unit) { onRefresh() }

    val openShells = sessions.filter { it.attachable }
    val entries = launcherEntries(desktop?.piSessions.orEmpty(), openShells)

    Column(Modifier.fillMaxSize().background(Bg).padding(12.dp)) {
        // ── header ───────────────────────────────────────────────────────────
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f)) {
                Text("ldmux", color = Fg, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
                Text(
                    host.ifEmpty { "no host" },
                    color = Dim,
                    fontSize = 11.sp,
                    fontFamily = FontFamily.Monospace,
                )
            }
            SmallAction(if (desktopLoading) "…" else "refresh", onRefresh)
            Spacer(Modifier.width(6.dp))
            IconBtn(onClick = onSettings) { IconCog(Dim) }
        }

        // Where a new shell goes, when a new shell is opened. A control, not a
        // section: one row of chips, not a list of desktops each carrying every
        // window on it. With nothing chosen the shell lands where the desktop
        // already is.
        val workspaces = desktop?.workspaces.orEmpty()
        if (workspaces.isNotEmpty()) {
            Spacer(Modifier.height(10.dp))
            Row(
                Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text("new shell on", color = Dim, fontSize = 10.sp)
                for (ws in workspaces) {
                    WorkspaceChip(
                        ws = ws,
                        targeted = targetWorkspace == ws.name,
                        onTarget = {
                            targetWorkspace = if (targetWorkspace == ws.name) null else ws.name
                        },
                    )
                }
            }
        } else if (desktop?.detail != null) {
            Spacer(Modifier.height(6.dp))
            Text(desktop.detail, color = Dim, fontSize = 10.sp, maxLines = 2)
        }

        Spacer(Modifier.height(10.dp))

        // ── one list ─────────────────────────────────────────────────────────
        if (entries.isEmpty()) {
            Box(Modifier.weight(1f).fillMaxWidth(), contentAlignment = Alignment.Center) {
                Text("Nothing open yet. Tap + shell.", color = Dim, fontSize = 12.sp)
            }
        } else {
            LazyColumn(
                Modifier.weight(1f).fillMaxWidth(),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                items(entries, key = { it.key }) { e ->
                    val session = e.sessionId?.let { id -> openShells.firstOrNull { it.id == id } }
                    ChoiceRow(
                        // The folder is always shown, because it is what tells two
                        // shells of the same kind apart. The process is the name.
                        title = e.label,
                        subtitle = listOfNotNull(tildify(e.cwd), e.note).joinToString(" · "),
                        chosen = e.key in chosen,
                        onClick = { chosen = toggle(chosen, e.key) },
                        onKill = session?.let { s -> { onKill(s) } },
                    ) {
                        // The per-shell menu belongs to shells this phone opened.
                        // A shell someone started at their desk is theirs to keep;
                        // the phone may look at it, but not move or end it.
                        if (e.openedByPhone && session != null) {
                            val live = e.liveWorkspace
                            val target = targetWorkspace
                            // One button, two meanings, decided by where the window
                            // already is: put it on the chosen desktop, or take it
                            // off the one it is on. A second button for "undo"
                            // would be a control to hunt for rather than a state you
                            // can see.
                            IconBtn(
                                onClick = {
                                    if (live != null) onCloseLive(session)
                                    else if (target != null) onMakeLive(session, target)
                                },
                                caption = when {
                                    live != null -> "on $live"
                                    target == null -> "pick a desktop"
                                    else -> null
                                },
                            ) {
                                IconPush(
                                    when {
                                        live != null -> Accent
                                        target == null -> Dim.copy(alpha = 0.5f)
                                        else -> Accent
                                    },
                                )
                            }
                        }
                    }
                }
            }
        }

        Spacer(Modifier.height(8.dp))
        AccentBtn(
            if (chosen.isEmpty()) "open" else "open (${chosen.size})",
            Modifier.fillMaxWidth(),
        ) {
            val picked = entries.filter { it.key in chosen }
            if (picked.isNotEmpty()) onOpen(picked)
        }
    }
}

/**
 * One row of the launcher.
 *
 * There are no sections any more. Everything the launcher offers is a place you
 * can have a shell — one already running, or a directory something is already
 * working in — and splitting that into "desktops", "pi sessions" and "shells"
 * made the reader pass three headers to reach one list. A header promises its
 * contents are a *different kind of thing*; these were not. The rows of window
 * titles under each desktop were worse still: a window manager's inventory, not
 * anything you could open.
 */
data class LauncherEntry(
    val key: String,
    /** The open session; null when this row would start a new shell. */
    val sessionId: String?,
    /** `bash`, `vim`, `pi` — what the row *is*, not only where it is. */
    val process: String,
    val cwd: String,
    /** Where it already is, when that is worth saying. */
    val note: String? = null,
    val openedByPhone: Boolean = false,
    val liveWorkspace: String? = null,
) {
    val label: String get() = process.ifEmpty { "shell" }
}

/**
 * The launcher's rows, as one flat list.
 *
 * Order is pi-derived rows first, then open shells, because this is also the
 * order pages are opened in and therefore the order they are paged through — an
 * order that must be decided somewhere, and deciding it here keeps the screen and
 * the pager from disagreeing.
 *
 * A pi session in a directory that already has a shell is *that shell*, not a
 * second row: offering both would show one piece of work twice, and choosing the
 * wrong one would open a duplicate shell.
 */
internal fun launcherEntries(
    piSessions: List<DesktopPiSession>,
    openShells: List<Session>,
): List<LauncherEntry> {
    val shellCwds = openShells.map { it.cwd }.toSet()
    return buildList {
        val seen = mutableSetOf<String>()
        for (pi in piSessions) {
            if (pi.cwd in shellCwds) continue
            if (!seen.add(pi.cwd)) continue
            add(
                LauncherEntry(
                    key = "new:${pi.cwd}",
                    sessionId = null,
                    // The process *is* pi — that is what makes the directory worth
                    // offering, and it is the one thing the row can honestly say.
                    process = "pi",
                    cwd = pi.cwd,
                    note = pi.workspace?.let { "ws $it" },
                ),
            )
        }
        for (shell in openShells) {
            add(
                LauncherEntry(
                    key = shell.id,
                    sessionId = shell.id,
                    process = shell.process,
                    cwd = shell.cwd,
                    note = when {
                        shell.liveWorkspace != null -> "on desktop ${shell.liveWorkspace}"
                        shell.heldElsewhere -> shell.ownerName?.let { "open on $it" }
                        else -> null
                    },
                    openedByPhone = shell.openedByPhone,
                    liveWorkspace = shell.liveWorkspace,
                ),
            )
        }
    }
}

/** One workspace, as a chip to aim a new shell at. */
@Composable
private fun WorkspaceChip(ws: DesktopWorkspace, targeted: Boolean, onTarget: () -> Unit) {
    Row(
        Modifier
            .clip(RoundedCornerShape(14.dp))
            .background(if (targeted) Accent.copy(alpha = 0.22f) else Surface)
            .border(
                width = if (targeted) 1.5.dp else 1.dp,
                color = if (targeted) Accent else Color(0xFF232838),
                shape = RoundedCornerShape(14.dp),
            )
            .clickable(onClick = onTarget)
            .padding(horizontal = 10.dp, vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            ws.name,
            color = if (targeted) Fg else Fg.copy(alpha = 0.8f),
            fontSize = 12.sp,
            fontFamily = FontFamily.Monospace,
        )
        if (ws.focused) {
            Spacer(Modifier.width(4.dp))
            Text("\u25cf", color = Accent, fontSize = 8.sp)
        }
    }
}

@Composable
private fun ChoiceRow(
    title: String,
    subtitle: String,
    chosen: Boolean,
    onClick: () -> Unit,
    onKill: (() -> Unit)? = null,
    trailing: @Composable () -> Unit = {},
) {
    Row(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(if (chosen) Accent.copy(alpha = 0.18f) else Surface)
            .border(
                width = if (chosen) 1.5.dp else 0.dp,
                color = if (chosen) Accent else Color.Transparent,
                shape = RoundedCornerShape(8.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        // A tick box, because these *are* the multi-select: several become the set
        // you page through.
        Box(
            Modifier
                .size(18.dp)
                .clip(RoundedCornerShape(4.dp))
                .background(if (chosen) Accent else Color.Transparent)
                .border(1.5.dp, if (chosen) Accent else Dim, RoundedCornerShape(4.dp)),
            contentAlignment = Alignment.Center,
        ) {
            if (chosen) Text("✓", color = Bg, fontSize = 12.sp, fontWeight = FontWeight.Bold)
        }
        Spacer(Modifier.width(10.dp))
        Column(Modifier.weight(1f)) {
            Text(title, color = Fg, fontSize = 14.sp, maxLines = 1, fontWeight = FontWeight.Medium)
            Text(subtitle, color = Dim, fontSize = 11.sp, maxLines = 1, fontFamily = FontFamily.Monospace)
        }
        trailing()
        if (onKill != null) {
            Spacer(Modifier.width(6.dp))
            IconBtn(onClick = onKill) { IconClose(Dim) }
        }
    }
}

@Composable
private fun SmallAction(label: String, onClick: () -> Unit) {
    Box(
        Modifier
            .clip(RoundedCornerShape(6.dp))
            .background(Surface)
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 7.dp),
    ) {
        Text(label, color = Dim, fontSize = 11.sp)
    }
}

/** Add or remove a key from a selection, without mutating the set. */
private fun toggle(current: Set<String>, key: String): Set<String> =
    if (key in current) current - key else current + key
