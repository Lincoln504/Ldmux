package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The shape every settings row has: a label in its own column, then the value or the control.
 *
 * **Why a column and not a sentence.** The rows used to be a label and a value side by side with
 * nothing between them — `tailnet  debian  unpair`, `radio  WIOld  AA:BB:CC  preset  change` — so
 * the label and the value read as one run of words and the eye had no place to stop. A person
 * looking for "which machine am I paired with" had to parse the whole line to find out where the
 * label ended. A fixed label column puts every value on one vertical line, which is what makes a
 * list of settings scannable: the labels are read once, down the left, and the values are compared
 * down the right.
 *
 * The column is [LABEL_WIDTH] wide, which fits the longest label in the app (`airtime`, `channel`,
 * `controls`) with room to spare, and it is a fixed width rather than a weight so the values line
 * up across sections instead of drifting with each row's longest label.
 */
internal val LABEL_WIDTH = 92.dp

/** A titled block: the heading, a rule under it, then its rows. */
@Composable
internal fun SettingsSection(title: String, content: @Composable () -> Unit) {
    Column(Modifier.fillMaxWidth()) {
        Text(
            title,
            color = Dim,
            fontSize = 11.sp,
            fontFamily = MonoFont,
            modifier = Modifier.padding(bottom = 6.dp),
        )
        SettingsDivider()
        Column(Modifier.fillMaxWidth()) { content() }
    }
}

/** The hairline between rows and under a heading. `Line` is the theme's divider colour. */
@Composable
internal fun SettingsDivider() {
    Box(Modifier.fillMaxWidth().height(1.dp).background(Line))
}

/**
 * One setting: the label in its column, then whatever sets or shows it.
 *
 * [content] is a `RowScope`, so a row can put a value and a button on the same line and let the
 * value take the space between them (`Modifier.weight(1f)`), which is how a long machine name
 * ellipsises instead of pushing its button off the screen.
 */
@Composable
internal fun SettingsRow(
    label: String,
    modifier: Modifier = Modifier,
    content: @Composable RowScope.() -> Unit,
) {
    Row(
        modifier
            .fillMaxWidth()
            .padding(vertical = LocalTouch.current.rowPad),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            label,
            color = Dim,
            fontSize = 12.sp,
            maxLines = 1,
            modifier = Modifier.width(LABEL_WIDTH),
        )
        Row(
            Modifier.weight(1f),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
            content = content,
        )
    }
}

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
    color: androidx.compose.ui.graphics.Color = Fg,
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
 * A line under a row, aligned with the values rather than with the labels.
 *
 * It is the row's consequence or its reason ("this also forgets the mesh keys and the radio"), so
 * it belongs to the value it explains, not to the label column.
 */
@Composable
internal fun SettingsNote(text: String, color: androidx.compose.ui.graphics.Color = Muted) {
    Row(Modifier.fillMaxWidth()) {
        Box(Modifier.width(LABEL_WIDTH))
        Text(text, color = color, fontSize = 11.sp, modifier = Modifier.weight(1f))
    }
}
