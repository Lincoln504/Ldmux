package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
    onOpen: (List<LauncherChoice>) -> Unit,
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
    val piSessions = desktop?.piSessions.orEmpty()
    val choices = launcherChoices(piSessions, openShells)
    val chosenCount = choices.count { it.key in chosen }

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

        Spacer(Modifier.height(12.dp))

        LazyColumn(Modifier.weight(1f).fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            // ── desktops ─────────────────────────────────────────────────────
            val workspaces = desktop?.workspaces.orEmpty()
            if (workspaces.isNotEmpty()) {
                item { Section("desktops", "${desktop?.wm ?: ""} · tap the ring to aim a new shell") }
                items(workspaces, key = { "ws:${it.name}" }) { ws ->
                    WorkspaceRow(
                        ws = ws,
                        // The windows on it, which is what makes a workspace a place.
                        windows = desktop?.windows.orEmpty().filter { it.workspace == ws.name },
                        targeted = targetWorkspace == ws.name,
                        onTarget = { targetWorkspace = if (targetWorkspace == ws.name) null else ws.name },
                    )
                }
            } else if (desktop != null && desktop.detail != null) {
                // Not an error state: the pi and shell lists below are still real.
                item { Section("desktops", desktop.detail) }
            }

            // ── pi agent sessions ─────────────────────────────────────────────
            if (piSessions.isNotEmpty()) {
                item { Section("pi sessions", "running on the host right now") }
                items(piSessions, key = { "pi:${it.pid}" }) { pi ->
                    val existing = openShells.firstOrNull { it.cwd == pi.cwd }
                    if (existing != null) {
                        ChoiceRow(
                            title = existing.label,
                            subtitle = "${tildify(pi.cwd)} · already open",
                            chosen = existing.id in chosen,
                            onClick = { chosen = toggle(chosen, existing.id) },
                            onKill = { onKill(existing) },
                        )
                    } else {
                        val key = LauncherChoice.NewShell(pi.cwd, pi.label).key
                        ChoiceRow(
                            title = pi.label,
                            subtitle = buildString {
                                append(tildify(pi.cwd))
                                pi.workspace?.let { append(" · ws ").append(it) }
                            },
                            chosen = key in chosen,
                            onClick = { chosen = toggle(chosen, key) },
                        )
                    }
                }
            }

            // ── open shells ───────────────────────────────────────────────────
            item { Section("shells", if (openShells.isEmpty()) "none open" else "already open on the host") }
            items(openShells, key = { "s:${it.id}" }) { s ->
                ChoiceRow(
                    title = s.label,
                    subtitle = buildString {
                        append(tildify(s.cwd))
                        s.workspaceOf(desktop)?.let { append(" · on desktop ").append(it) }
                    },
                    chosen = s.id in chosen,
                    onClick = { chosen = toggle(chosen, s.id) },
                    onKill = { onKill(s) },
                ) {
                    // The per-shell menu belongs to shells this phone opened. A
                    // shell someone started at their desk is theirs to keep; the
                    // phone may look at it, but not move or end it.
                    if (s.openedByPhone) {
                        val target = targetWorkspace
                        IconBtn(
                            onClick = { if (target != null) onMakeLive(s, target) },
                            caption = if (target == null) "pick a desktop" else null,
                        ) {
                            IconPush(if (target == null) Dim.copy(alpha = 0.5f) else Accent)
                        }
                    }
                }
            }
        }

        Spacer(Modifier.height(10.dp))

        // ── open ─────────────────────────────────────────────────────────────
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            AccentBtn(
                // Nothing chosen means everything, which is what a session list
                // should do; choosing narrows it to the set you page through.
                label = if (chosenCount == 0) "open all (${choices.size})" else "open ($chosenCount)",
                modifier = Modifier.weight(1f),
                onClick = {
                    val picked = if (chosenCount == 0) choices else choices.filter { it.key in chosen }
                    chosen = setOf()
                    onOpen(picked)
                },
            )
        }
        if (targetWorkspace != null) {
            Spacer(Modifier.height(6.dp))
            Text("aimed at desktop $targetWorkspace", color = Dim, fontSize = 11.sp)
        }
        // What came of the last desktop request. Shown rather than assumed: the
        // host may have no window manager, and doing nothing silently looks
        // exactly like a broken button.
        liveResult?.let { result ->
            Spacer(Modifier.height(6.dp))
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Text(
                    if (result.ok) "on the desktop" else "could not: ${result.detail ?: "failed"}",
                    color = if (result.ok) Accent else Color(0xFFFF6B6B),
                    fontSize = 11.sp,
                    modifier = Modifier.weight(1f),
                )
                SmallAction("ok", onDismissLiveResult)
            }
        }
    }
}

/**
 * What a launcher row stands for: a shell that is already open, or a directory a
 * new shell should be opened in.
 */
sealed interface LauncherChoice {
    val key: String

    data class Session(val id: String, val label: String, val cwd: String) : LauncherChoice {
        override val key: String get() = id
    }

    data class NewShell(val cwd: String, val label: String) : LauncherChoice {
        override val key: String get() = "new:$cwd"
    }
}

/**
 * Which desktop a shell's live window is on, or null when it has none.
 *
 * Read from the window that *this session's class* names, so it is the window the
 * phone put there and not some other alacritty that happens to be open.
 */
private fun Session.workspaceOf(desktop: DesktopInventory?): String? =
    desktop?.windows?.firstOrNull { it.appClass == "ldmux-live-$id" }?.workspace?.ifEmpty { null }

private fun toggle(current: Set<String>, key: String): Set<String> =
    if (key in current) current - key else current + key

/**
 * Everything the launcher can open, in the order it appears on screen.
 *
 * Two rules, both of which matter more than they look:
 *
 *   - A pi session that already has an ldmux shell in the same directory **is**
 *     that shell, not a second thing. Offering both would present the same work
 *     twice, and choosing the wrong one would open a second shell into a
 *     directory that already has one.
 *   - The order is the screen's order, so ◀ ▶ walk the list the way it is read,
 *     not the order the rows happened to be tapped in.
 *
 * Entries are de-duplicated by key, which is not decoration: two pi agents in the
 * same directory are two agents but one place to go, and their key — the
 * directory — is what a chosen row is remembered by. Left in, the second would
 * be a row that could never be toggled independently of the first.
 */
internal fun launcherChoices(
    piSessions: List<DesktopPiSession>,
    openShells: List<Session>,
): List<LauncherChoice> {
    val shellCwds = openShells.map { it.cwd }.toSet()
    return buildList {
        for (pi in piSessions) {
            if (pi.cwd !in shellCwds) add(LauncherChoice.NewShell(pi.cwd, pi.label))
        }
        for (s in openShells) add(LauncherChoice.Session(s.id, s.label, s.cwd))
    }.distinctBy { it.key }
}

@Composable
private fun Section(title: String, detail: String) {
    Row(Modifier.fillMaxWidth().padding(top = 8.dp, bottom = 2.dp), verticalAlignment = Alignment.Bottom) {
        Text(title.uppercase(), color = Dim, fontSize = 10.sp, fontWeight = FontWeight.SemiBold)
        Spacer(Modifier.width(8.dp))
        Text(detail, color = Dim.copy(alpha = 0.7f), fontSize = 10.sp, maxLines = 1)
    }
}

@Composable
private fun WorkspaceRow(
    ws: DesktopWorkspace,
    windows: List<DesktopWindow>,
    targeted: Boolean,
    onTarget: () -> Unit,
) {
    Column(
        Modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(8.dp))
            .background(Surface)
            .border(
                width = if (targeted) 1.5.dp else 0.dp,
                color = if (targeted) Accent else Color.Transparent,
                shape = RoundedCornerShape(8.dp),
            )
            .padding(horizontal = 10.dp, vertical = 8.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            // The target ring, not a checkbox: aiming a shell somewhere is not the
            // same act as choosing a shell to open, and they must not look alike.
            Box(
                Modifier
                    .size(18.dp)
                    .clip(RoundedCornerShape(9.dp))
                    .background(if (targeted) Accent else Color.Transparent)
                    .border(1.5.dp, if (targeted) Accent else Dim, RoundedCornerShape(9.dp))
                    .clickable(onClick = onTarget),
            )
            Spacer(Modifier.width(10.dp))
            Text(
                if (ws.focused) "${ws.name} ●" else ws.name,
                color = if (ws.focused) Fg else Fg.copy(alpha = 0.85f),
                fontSize = 14.sp,
                fontFamily = FontFamily.Monospace,
                fontWeight = if (ws.focused) FontWeight.SemiBold else FontWeight.Normal,
            )
            Spacer(Modifier.width(8.dp))
            Text(
                if (ws.visible && !ws.focused) "visible" else "",
                color = Dim,
                fontSize = 10.sp,
            )
            Spacer(Modifier.weight(1f))
            Text(
                if (ws.windowCount == 0) "empty" else "${ws.windowCount} window${if (ws.windowCount == 1) "" else "s"}",
                color = Dim,
                fontSize = 11.sp,
            )
        }
        for (w in windows) {
            Row(Modifier.fillMaxWidth().padding(start = 28.dp, top = 3.dp)) {
                Text(
                    w.title.ifBlank { w.appClass },
                    color = Dim,
                    fontSize = 11.sp,
                    maxLines = 1,
                    modifier = Modifier.weight(1f),
                )
                Spacer(Modifier.width(6.dp))
                Text(w.appClass, color = Dim.copy(alpha = 0.6f), fontSize = 10.sp, maxLines = 1)
            }
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
