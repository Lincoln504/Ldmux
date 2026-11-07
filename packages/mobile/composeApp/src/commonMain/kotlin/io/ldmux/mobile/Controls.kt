package io.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.interaction.collectIsPressedAsState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.selection.selectable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay

/// How long a repeatable button must be held before it starts repeating.
private const val REPEAT_DELAY_MS = 400L

/// How often it steps while held: fast enough to cross the text-size range in about two seconds,
/// slow enough that a held thumb lands on a size rather than overshooting it.
private const val REPEAT_EVERY_MS = 90L

/**
 * A button that carries the accent, for the single primary action.
 *
 * [enabled] is here because the alternative was an `onClick` that quietly did nothing:
 * the radio dialog's `apply` had no disabled state at all, so before the radio answered
 * it was the brightest control on the screen and a tap on it produced silence.
 */
/**
 * The second action, deliberately not a filled button.
 *
 * Two filled buttons of different widths would read as "one primary, one less
 * primary", which is exactly right — but two of the *same* fill just reads as a
 * bar split in half. An outline says "this is the other thing you can do" without
 * competing for the eye.
 */
@Composable
fun OutlinedBtn(
    label: String,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
    onClick: () -> Unit,
) {
    Box(
        modifier
            .clip(RadiusMedium)
            .border(1.dp, Edge, RadiusMedium)
            .heightIn(min = LocalTouch.current.min)
            .clickable(role = Role.Button, enabled = enabled, onClick = onClick)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = if (enabled) Fg else Dim, fontSize = 13.sp)
    }
}

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
            // Disabled fills with `Line`, not `Surface`: `ModalCard`'s background *is* `Surface`,
            // so a disabled button inside a dialog lost its shape entirely and only its dim label
            // floated. `OptionButton` already documents and fixes this; the two buttons beside it
            // had the same bug and kept it.
            .background(if (enabled) Accent else Line)
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
    /**
     * False where the option exists but cannot be chosen right now (a public channel forces
     * batch, so `live` is not available there). Drawn dim and inert rather than removed:
     * an option that vanishes reads as one the app does not have, and the row's other option
     * would move under the thumb.
     */
    enabled: Boolean = true,
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
            // **Every option is outlined, not only the chosen one.** A borderless label beside a
            // bordered one reads as text beside a button, so the unchosen options looked like
            // captions and the row looked like it had one control and some words. The chosen
            // option keeps its stronger outline; the rest keep a hairline so they are visibly
            // pressable.
            .border(1.dp, if (selected) Chosen else Line, RadiusMedium)
            .selectable(selected = selected, enabled = enabled, role = Role.RadioButton, onClick = onClick)
            .heightIn(min = LocalTouch.current.min)
            .padding(horizontal = 10.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            label,
            color = when {
                !enabled -> Dim.copy(alpha = 0.45f)
                selected -> Fg
                else -> Dim
            },
            fontSize = 12.sp,
            maxLines = 1,
            // An option label longer than the row used to end mid-character, which reads as a
            // rendering fault rather than as a name that did not fit.
            overflow = TextOverflow.Ellipsis,
        )
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
    Text(
        notice.text,
        color = if (notice.ok) Muted else Warn,
        fontSize = 11.sp,
        // **A refusal is announced, not just drawn.** The line clears itself after four seconds,
        // which is exactly the case a screen reader misses: it reads what it is focused on, and this
        // is a plain `Text` that appears on top of whatever that was. A live region makes the
        // refusal audible the moment it arrives; polite rather than assertive, because a keystroke
        // being held is not an emergency.
        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
    )
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
            // `Edge`, not `Line`: a destructive button whose edge cannot be seen is a label in a
            // red nobody can press with confidence.
            .border(1.dp, Edge, RadiusMedium)
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
    /**
     * Whether this draws as a button.
     *
     * True for a verb on a row (`change`, `scan`, `unpair`, `edit`), because a bare accent label
     * beside another label reads as text. False for the one action that is a *link* rather than a
     * control, `ldmux.io` in the settings footer: a website address with a control's outline
     * claims the weight of a setting.
     */
    bordered: Boolean = true,
    /**
     * Repeat while held, for a control that walks a range rather than performing a verb.
     *
     * The text size is 7 to 28 in single steps, which was twenty-one taps from one end to the other
     * — a control nobody reaches the far side of. A hold repeats after a short beat, so the far end
     * is one gesture. Off by default: `unpair` and `close` must not repeat because a held thumb is
     * not a command.
     */
    repeat: Boolean = false,
    content: @Composable RowScope.() -> Unit,
) {
    val source = remember { MutableInteractionSource() }
    if (repeat) {
        val pressed by source.collectIsPressedAsState()
        // `onClick` is a lambda rebuilt every recomposition, so it is read through the latest value
        // rather than captured once: a stale capture would step from the value the button had when
        // the gesture began and the range would never advance.
        val step by rememberUpdatedState(onClick)
        LaunchedEffect(pressed, enabled) {
            if (!pressed || !enabled) return@LaunchedEffect
            // A beat before repeating, so a tap that happens to be a little slow is still one step.
            delay(REPEAT_DELAY_MS)
            while (true) {
                step()
                delay(REPEAT_EVERY_MS)
            }
        }
    }
    TextButton(
        onClick = onClick,
        enabled = enabled,
        interactionSource = source,
        // A caller's modifier comes first so `weight` decides the width and the height
        // stays the app's own: a button that is wider than its label is a button that
        // lines up with the one beside it.
        // Square-ish, not just short: the height floor was set and no width floor was, so a
        // one-glyph `-`/`+` measured about 30x36 dp even in the comfortable density — under
        // the 48x48 dp every platform guidance asks for, on a control whose whole job is to be
        // hit. `widthIn` uses the same floor as the height so the two cannot drift apart.
        // **A verb on a row is a button, so it is drawn as one.** These were bare accent text
        // beside a label (`change`, `scan`, `unpair`, `edit`) and read as links inside a list of
        // statements; the hairline is what makes them look like something to press.
        modifier = modifier
            // **A minimum, not a height.** A fixed height clipped its own label once the system
            // font scale grew — the button stayed 48 dp while the glyph inside it needed more, and
            // the label was cut in half. `heightIn` keeps the touch target and lets the row grow
            // with the text, which is the whole point of a font-scale setting.
            .heightIn(min = LocalTouch.current.min)
            .widthIn(min = LocalTouch.current.min)
            .then(
                if (bordered) {
                    Modifier.clip(RadiusMedium).border(1.dp, Line, RadiusMedium)
                } else {
                    Modifier
                },
            ),
        contentPadding = PaddingValues(horizontal = 10.dp, vertical = 0.dp),
        content = content,
    )
}
