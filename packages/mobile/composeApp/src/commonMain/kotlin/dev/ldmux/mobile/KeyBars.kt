package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.pager.PagerState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.CoroutineScope

@Composable
internal fun PortraitBar(
    handle: TerminalHandle?,
    pages: List<Session>,
    pagerState: PagerState,
    scope: CoroutineScope,
    keyboardUp: Boolean,
    onToggleKeyboard: () -> Unit,
    onMenu: () -> Unit,
    menuExpanded: Boolean,
    onMenuDismiss: () -> Unit,
    onLauncher: () -> Unit,
    modifiers: Modifiers,
    onSticky: (StickyKey) -> Unit,
    press: (String) -> Unit,
    // The shell on screen, by id (see `shownSessionId` in App).
    shownId: String?,
    // Names the shell the arrows are moving to (see `ShellHeader`).
    onShow: (String) -> Unit,
) {
    Column(
        Modifier.fillMaxWidth().background(Surface).padding(horizontal = 6.dp, vertical = LocalTouch.current.pad),
        verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
    ) {
        // Where you are, and how to move, at the top of the bar: the same row the
        // side panel shows, because moving between shells must not mean leaving
        // them. Navigation at one end, keys in the middle, actions at the other.
        ShellHeader(
            pages,
            pagerState,
            scope,
            onLauncher,
            currentId = shownId,
            onShow = onShow,
            showMenu = false,
            dense = true,
        )

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
                AppMenu(menuExpanded, onMenuDismiss, onLauncher)
            }
        }
        // The keyboard, up or down: one toggle, a plain keyboard glyph, lit while
        // the keypad is on screen. It is not a rotation: the screen stays where it
        // is, and the keypad is the only thing that moves.
        val keyboardHere: @Composable RowScope.() -> Unit = {
            IconBtn(
                onClick = onToggleKeyboard,
                label = if (keyboardUp) "hide keyboard" else "show keyboard",
                modifier = Modifier.weight(1f),
            ) {
                IconKeyboard(if (keyboardUp) Accent else Dim, box = LocalTouch.current.icon)
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
        // Home/End and the page keys have no soft-keyboard equivalent and no
        // gesture, so they need a row of their own here. Landscape keeps its own
        // copy in the panel; neither mode has a scrollback that can stand in for
        // PageUp/PageDown inside a full-screen program.
        PortraitKeyRow(
            listOf(
                "Home" to { press("Home") },
                "End" to { press("End") },
                "PgUp" to { press("PageUp") },
                "PgDn" to { press("PageDown") },
            ),
        )
        PortraitKeyRow(
            // From `Keys.kt`, so the list the user sees is the list the C/d guard
            // checks. It used to be written here, and the guard read a constant this
            // call site had stopped using.
            portraitControlKeys(onSticky, press),
            leading = keyboardHere,
            active = listOf(modifiers.ctrl, modifiers.alt, false, false, false, false, false),
        )
        // No microphone: portrait has the keyboard, and the keyboard has its own.
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

@Composable
internal fun NavArrow(glyph: String, enabled: Boolean, dense: Boolean = false, onClick: () -> Unit) {
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
internal fun KeyBtn(
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

/**
 * One row of keys sharing the width between them.
 *
 * `weight(1f)` each rather than sized to their own text: a row of equally wide
 * targets is easier to hit without looking, and it reads as a set — which is what
 * `C-c`, `|`, `-` and `/` are, and what a ragged column of chips was not.
 */
@Composable
internal fun KeyRow(keys: List<Pair<String, () -> Unit>>, active: List<Boolean> = emptyList()) {
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
