package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.selection.selectable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.ui.unit.sp

/**
 * A button that carries the accent, for the single primary action.
 *
 * [enabled] is here because the alternative was an `onClick` that quietly did nothing:
 * the radio dialog's `apply` had no disabled state at all, so before the radio answered
 * it was the brightest control on the screen and a tap on it produced silence.
 */
@Composable
fun AccentBtn(
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    Box(
        modifier
            .clip(RadiusMedium)
            .background(if (enabled) Accent else Surface)
            .heightIn(min = LocalTouch.current.min)
            .clickable(role = Role.Button, enabled = enabled, onClick = onClick)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = if (enabled) Bg else Dim, fontSize = 13.sp)
    }
}

/**
 * One of a set of mutually exclusive options, in a row.
 *
 * **The selected option is a filled shape, not only a brighter colour.** The settings
 * screen carried five of these pairs — tailnet/radio, left/right, compact/comfortable,
 * open/metered, and the radio's regions — and every one of them said "you are here" by
 * painting the chosen label with `Accent` and the rest with `Dim`. Three problems come
 * with that, and all three are the same problem:
 *  - a screen reader has nothing to read, because the only difference is a colour it
 *    never sees (`selectable` puts the state in the semantics tree);
 *  - a colour-blind eye gets no second signal at all;
 *  - a **button whose label is the state** ("open") cannot say whether tapping makes it
 *    open or tells you it is open, which is why these read as answers rather than
 *    controls.
 *
 * A filled background and an outline under the chosen label fix the first two without
 * adding a mark to every row, and the outline is the vocabulary the theme already names
 * for "the outline of a control that is chosen". `selected` in the semantics tree lets the
 * label stay a value.
 */
@Composable
internal fun OptionButton(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
) {
    Box(
        Modifier
            .clip(RadiusMedium)
            // `Line` rather than `Surface`: this control is used both on the screen's own
            // background and inside a modal card, whose background *is* `Surface` — a
            // `Surface` fill would be invisible in exactly half the places it is used.
            // The chosen option carries an outline as well, which is the vocabulary the
            // theme already names for "the outline of a control that is chosen".
            .background(if (selected) Line else Color.Transparent)
            .border(1.dp, if (selected) Chosen else Color.Transparent, RadiusMedium)
            .selectable(selected = selected, role = Role.RadioButton, onClick = onClick)
            .heightIn(min = LocalTouch.current.min)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = if (selected) Fg else Dim, fontSize = 12.sp, maxLines = 1)
    }
}

/**
 * A one-line answer to something that just happened, and whether it went the right way.
 *
 * The radio dialog put every outcome through one `String?` drawn in [Warn], so a
 * **success** ("applied") arrived in the colour the app reserves for a fault. A status
 * line that cannot tell good news from bad is worse than no line: the user learns to
 * distrust the colour, and the one failure that matters looks like the last success.
 */
internal data class Notice(val text: String, val ok: Boolean)

@Composable
internal fun NoticeLine(notice: Notice?) {
    if (notice == null) return
    Text(notice.text, color = if (notice.ok) Muted else Warn, fontSize = 11.sp)
}

/**
 * The destructive twin of [OutlinedBtn]: the same shape, in [Danger].
 *
 * Shape as the second action and colour as the warning, because a filled red button
 * would be the brightest thing on a modal that exists to ask a question.
 */
@Composable
fun DangerBtn(label: String, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .clip(RadiusMedium)
            .border(1.dp, Line, RadiusMedium)
            .heightIn(min = LocalTouch.current.min)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = Danger, fontSize = 13.sp)
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
internal fun MiniTextButton(
    onClick: () -> Unit,
    enabled: Boolean = true,
    modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit,
) {
    TextButton(
        onClick = onClick,
        enabled = enabled,
        // A caller's modifier comes first so `weight` decides the width and the height
        // stays the app's own: a button that is wider than its label is a button that
        // lines up with the one beside it.
        modifier = modifier.height(LocalTouch.current.min),
        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 0.dp),
        content = content,
    )
}
