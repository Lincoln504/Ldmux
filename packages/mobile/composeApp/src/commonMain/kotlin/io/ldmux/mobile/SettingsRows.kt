package io.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.toggleable
import androidx.compose.ui.semantics.Role
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * Settings rows, as a list of statements rather than a table.
 *
 * **There is no label column any more.** Every row used to be a fixed 92 dp label band with the
 * value pushed to a second band on its right, so the screen read as two columns: a noun down the
 * left and an answer down the right, with the eye crossing a gap on every line. That is a form's
 * shape, and it costs a third of a phone's width to a blank gutter. What the screen actually holds
 * is a list of *statements* — "route: over tailscale", "radio: WIOld, change", "text size: 12,
 * minus plus" — and a statement reads left to right on one line.
 *
 * So a row is now a title, an optional one-line summary under it (the value, or why the setting
 * matters), and the control at the trailing edge. Rows that hold a choice put the choice under the
 * title instead, because two buttons beside a sentence on a 360 dp phone is where the text starts
 * truncating.
 *
 * **The spacing is deliberate.** Rows are 14 dp apart vertically and a section starts 24 dp below
 * the one before it. The old screen packed them at 6 dp, which is a density that reads as a table
 * cell: the eye cannot tell where one setting ends and the next begins, and on a touch screen the
 * gap between two tap targets is part of the control.
 */

/** A titled block: the heading, a rule under it, then its rows. */
@Composable
internal fun SettingsSection(title: String, content: @Composable ColumnScope.() -> Unit) {
    Column(Modifier.fillMaxWidth()) {
        // **The header carries the hierarchy, so the spacing does not have to.** Uppercase at one
        // size down from a row's label, spaced out, in the brightest dim the theme has: it reads
        // as a label on a group rather than as the first row of one.
        Text(
            title.uppercase(),
            color = Dim,
            fontSize = 10.sp,
            letterSpacing = 1.2.sp,
            fontFamily = MonoFont,
            modifier = Modifier.padding(bottom = 8.dp),
        )
        SettingsDivider()
        Column(Modifier.fillMaxWidth(), content = content)
    }
}

/** The hairline between rows and under a heading. `Line` is the theme's divider colour. */
@Composable
internal fun SettingsDivider() {
    Box(Modifier.fillMaxWidth().height(1.dp).background(Line))
}

/**
 * One setting: a title, an optional summary line, and whatever sets or shows it.
 *
 * [trailing] is drawn at the row's end and aligned with the title's first line, so a value and a
 * button sit on the row's own line rather than in a second column. Give it a `Modifier.weight`
 * only when the value is the row's whole point; a trailing control should size to its content or
 * the title beside it starts truncating.
 */
@Composable
internal fun SettingsRow(
    label: String,
    summary: String? = null,
    modifier: Modifier = Modifier,
    trailing: @Composable RowScope.() -> Unit = {},
) {
    Row(
        modifier
            .fillMaxWidth()
            .padding(vertical = SettingsRowPad),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(label, color = Fg, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (!summary.isNullOrEmpty()) {
                Spacer(Modifier.height(2.dp))
                Text(
                    summary,
                    color = Dim,
                    fontSize = 12.sp,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            content = trailing,
        )
    }
}

/**
 * A setting whose control is a set of choices, drawn under the title.
 *
 * Two option buttons beside a title on a phone leaves the title about ten characters before it
 * ellipsises, and a truncated label is what the label is for. So the choices get their own line,
 * left-aligned with the title, which is also where a desktop settings app puts a segmented control.
 */
@Composable
internal fun SettingsChoice(
    label: String,
    summary: String? = null,
    modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit,
) {
    Column(
        modifier
            .fillMaxWidth()
            .padding(vertical = SettingsRowPad),
    ) {
        Text(label, color = Fg, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (!summary.isNullOrEmpty()) {
            Spacer(Modifier.height(2.dp))
            Text(summary, color = Dim, fontSize = 12.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        Spacer(Modifier.height(10.dp))
        Row(
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            content = content,
        )
    }
}

/**
 * A setting that is simply on or off.
 *
 * The whole row is the target, not the box: a 16 dp tick is the smallest thing on the screen, and
 * a settings row that only responds within it is a control that misses. The box itself is drawn
 * the way the launcher draws its own, so a tick means one thing in this app.
 */
@Composable
internal fun SettingsToggle(
    label: String,
    checked: Boolean,
    summary: String? = null,
    onChange: (Boolean) -> Unit,
) {
    Row(
        Modifier
            .fillMaxWidth()
            .toggleable(value = checked, role = Role.Checkbox, onValueChange = onChange)
            .padding(vertical = SettingsRowPad),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(label, color = Fg, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
            if (!summary.isNullOrEmpty()) {
                Spacer(Modifier.height(2.dp))
                Text(summary, color = Dim, fontSize = 12.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
            }
        }
        Box(
            Modifier
                .size(LocalTouch.current.check)
                .clip(RadiusSmall)
                .background(if (checked) Accent else Color.Transparent)
                .border(1.5.dp, if (checked) Chosen else Dim, RadiusSmall),
            contentAlignment = Alignment.Center,
        ) {
            if (checked) IconCheck(Bg, box = LocalTouch.current.check - 6.dp)
        }
    }
}

/** Vertical padding inside one row. The touch floor still comes from the control itself. */
internal val SettingsRowPad = 8.dp

/** The gap between two sections. The break is the header's job, not a large blank. */
internal val SettingsSectionGap = 20.dp


/**
 * A value in a row: the thing the label names.
 *
 * One line by default, because a name is a name and a wrapped name makes the row two rows. A row
 * whose value is a *measurement* (`914.25 MHz  500.0 kHz  SF8  CR5`, `0.0 s of 36 s this hour`)
 * asks for two lines instead: truncating a fact to `914.25 MHz  500.0 kHz  SF8 …` hides the part
 * the user opened the screen to read, and the row has no control that would justify the cut.
 */
@Composable
internal fun SettingsValue(
    text: String,
    modifier: Modifier = Modifier,
    color: Color = Fg,
    maxLines: Int = 1,
) {
    Text(
        text,
        color = color,
        fontSize = 13.sp,
        maxLines = maxLines,
        overflow = TextOverflow.Ellipsis,
        modifier = modifier,
    )
}

/**
 * A line under a row, explaining what the row does or what it costs.
 *
 * **A dash, not an indent.** The line used to be pushed right by one character, which read as a
 * hanging indent with nothing to hang from: the eye saw a column that was not there, and the
 * line looked like a mistake rather than a remark. A leading dash says "this belongs to the row
 * above" in the space of one character, and it is how the rest of the app writes an aside.
 */
@Composable
internal fun SettingsNote(text: String, color: Color = Muted) {
    Text(
        "- " + text,
        color = color,
        fontSize = 11.sp,
        modifier = Modifier.fillMaxWidth().padding(bottom = SettingsRowPad),
    )
}

/**
 * A sub-flow that opens **under its own row**, not over the screen.
 *
 * This replaces [ModalCard] for settings sub-flows, and the reason is a layout fact rather
 * than a taste: `ModalCard` sizes itself with `fillMaxSize`, but these flows are composed
 * inside the settings screen's `verticalScroll`, where the height constraint is infinite.
 * `fillMaxHeight` cannot resolve against infinity, so the card took its content's height
 * and drew a scrim-coloured block at the end of the list — below every other row, not under
 * the one that was tapped.
 *
 * A panel is the honest shape for these flows anyway: choosing a radio, reading its tuple and
 * writing a preset are edits to one row, and an editor that covers the row it edits makes the
 * user remember which row it was. Expanding in place keeps the row, its value and its editor
 * on one line of sight, and the rows below move down rather than being hidden.
 *
 * The back gesture still closes it, because a panel is the topmost thing the user opened and
 * back must mean "close this", not "leave the app".
 */
@Composable
internal fun SettingsPanel(
    title: String,
    onCollapse: () -> Unit,
    content: @Composable ColumnScope.() -> Unit,
) {
    BackToLauncher(enabled = true) { onCollapse() }
    Column(
        Modifier
            .fillMaxWidth()
            .padding(bottom = SettingsRowPad)
            .clip(RadiusLarge)
            .background(Surface)
            .padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Text(
                title,
                color = Fg,
                fontSize = 14.sp,
                fontWeight = androidx.compose.ui.text.font.FontWeight.SemiBold,
            )
            Spacer(Modifier.weight(1f))
            MiniTextButton(onClick = onCollapse) { Text("done", color = Accent, fontSize = 12.sp) }
        }
        content()
    }
}
