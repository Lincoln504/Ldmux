package io.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.pager.PagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.launch

/** The landscape panel's width. The terminal gets everything else. */
internal val PANEL_WIDTH = 190.dp

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
 * same launcher square. Both call the same `onLauncher`.
 */
@Composable
internal fun ShellHeader(
    pages: List<Session>,
    pagerState: PagerState?,
    scope: CoroutineScope,
    onLauncher: () -> Unit,
    // The shell on screen, by id — not the pager's page index, which a
    // re-measure resets underneath the user (see `shownSessionId` in App). The
    // title, the count and the arrows describe the shell the user is on.
    currentId: String?,
    // The arrows move the pager, and they name where they are going: the watcher
    // in App treats a settled page that is not the named shell as a re-measure
    // to undo, and an arrow that did not name its target would be undone.
    onShow: (String) -> Unit,
    showMenu: Boolean = true,
    dense: Boolean = false,
) {
    val current = pages.indexOfFirst { it.id == currentId }.coerceAtLeast(0)
    val total = pages.size.coerceAtLeast(1)
    val here = pages.getOrNull(current)

    // Density, not structure: this is the same row drawn by the same composable in
    // both modes, and `dense` decides only how much height it is allowed.

    fun step(delta: Int) {
        val target = (current + delta).coerceIn(0, (total - 1).coerceAtLeast(0))
        if (target != current) {
            pages.getOrNull(target)?.let { onShow(it.id) }
            if (pagerState != null) scope.launch { pagerState.animateScrollToPage(target) }
        }
    }

    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        if (showMenu) {
            // The launcher square opens the launcher, in one tap: it used to open a menu
            // whose only item was "shells". See the note in [PortraitBar].
            IconBtn(onClick = onLauncher, label = "shells", filled = true) {
                IconLauncher(Fg)
            }
        }
        NavArrow("\u25c0", enabled = current > 0, dense = dense) { step(-1) }
        Column(Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                folderName(here?.cwd ?: ""),
                color = Fg,
                fontFamily = MonoFont,
                fontSize = 12.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            Text(
                if (total > 1) "${current + 1} / $total shells" else "1 shell",
                color = Dim,
                fontFamily = MonoFont,
                // 10sp, not 9: nine-point type on a phone is under the size at which the
                // shapes of letters survive, and this line is read at a glance while a
                // finger is on the terminal.
                fontSize = 10.sp,
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
internal fun SidePanel(
    pages: List<Session>,
    // Null on the mesh route, which has pages but no pager to move (see [ShellHeader]).
    pagerState: PagerState?,
    scope: CoroutineScope,
    // Neither is read here; they stay for the tailnet call site. The mesh route has no
    // daemon client and closes no shell from the panel.
    client: LdmuxClient? = null,
    handles: Map<String, TerminalHandle>,
    onKill: (Session) -> Unit = {},
    onVoice: () -> Unit,
    /**
     * Drawn first in the panel's scrolling area: the mesh route's mode row. It belongs at the top,
     * beside the reading area it governs, rather than at the panel's foot — a control the user
     * presses while reading should be where the reading starts, and the foot of a scroll is the
     * one place a thumb does not reach without scrolling first.
     */
    modeRow: (@Composable (Modifier) -> Unit)? = null,
    onLauncher: () -> Unit,
    press: (String) -> Unit,
    shownId: String?,
    onShow: (String) -> Unit,
) {
    val here = pages.firstOrNull { it.id == shownId }
    val handle = here?.id?.let { handles[it] }
    val total = pages.size.coerceAtLeast(1)

    Column(
        Modifier.width(PANEL_WIDTH).fillMaxHeight().background(Surface)
            .padding(horizontal = 8.dp, vertical = LocalTouch.current.rowPad * 2),
        verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
    ) {
        ShellHeader(pages, pagerState, scope, onLauncher, currentId = shownId, onShow = onShow)

        PanelDivider()

        @OptIn(ExperimentalLayoutApi::class)
        Column(
            Modifier.weight(1f).verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
        ) {
            // The route's own row, above the keys: the mesh route's hold-input/sync control.
            modeRow?.invoke(Modifier.fillMaxWidth())
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
            // `paste` is the only clipboard control in landscape: copy is automatic
            // (highlight → release copies to system clipboard), so there is no copy
            // button. Paste lives here because landscape has no soft keyboard and
            // no paste gesture — it is the only way to push clipboard text into the
            // shell. The clipboard is also where a program's own OSC 52 copy lands,
            // so without this a program's copy would have nowhere to be pasted.
            val clipboard = LocalClipboardManager.current
            // Paste and the microphone share one row: both bring text into the
            // shell from outside the keyboard, and the microphone used to sit alone
            // in a bar below every key — a whole row for one button. Trailing, so
            // the key keeps the width and the icon keeps its square.
            Row(
                Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                KeyBtn("paste", Modifier.weight(1f)) {
                    val text = clipboard.getText()?.text
                    if (!text.isNullOrEmpty()) handle?.paste(text)
                }
                IconBtn(onClick = onVoice, label = "voice input") { IconMic(tint = Fg) }
            }
            PanelDivider()
            // Backspace joins the keys a soft keyboard cannot send. Landscape has no
            // keyboard at all, so a dictated word that arrives wrong cannot be taken
            // back: Esc, Tab and Enter were here, and the one key that undoes a
            // character was not. `BSpace` is the protocol's name for erase-to-the-left and it
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
            // The arrows are the same block portrait uses ([ArrowPad]), not a row:
            // up over down, left and right either side. A direction is a position,
            // and landscape had the same four-in-a-row shape portrait did.
            ArrowPad(Modifier.fillMaxWidth(), press, gap = 6.dp)
            PanelDivider()
            // `PgUp`/`PgDn` live in the arrow block above, exactly as portrait has
            // them: one pad, one definition, and no key drawn twice in one panel.
            KeyRow(
                listOf(
                    "Home" to { press("Home") },
                    "End" to { press("End") },
                ),
            )
            PanelDivider()
            // Ctrl-D is deliberately not among these, and it was asked for by name to
            // be taken out. The list lives in `Keys.kt` so the guard that keeps it out
            // is checking the list this row renders. See [panelControlKeys].
            KeyRow(panelControlKeys(press))
        }

        // The bar that held only the microphone is gone: the microphone is a way to
        // put text into the shell, so it sits with `paste` at the top of the panel
        // rather than in a row of its own at the bottom. Portrait carries no
        // microphone, because the IME has one there anyway: two is one too many.

        // Diagnostics, not a control: the repaint cadence follows the round trip,
        // so this is the reason a slow link feels the way it does. It survives the
        // link going down — the whole page has nothing else to say about it, and a
        // line that simply disappears reads as a slow app rather than a dead socket.
        // Typing in that state is dropped silently, so the least this can do is say so.
        val link by (handle?.link ?: TerminalHandle.EMPTY_LINK).collectAsState()
        val linkText = when {
            link.offline -> "link down · reconnecting"
            link.measured -> "link ${link.rttMs} ms · ${link.fps} fps"
            else -> null
        }
        linkText?.let {
            Text(
                it,
                color = if (link.offline) Accent else Muted,
                fontFamily = MonoFont,
                fontSize = 10.sp,
            )
        }
    }
}
