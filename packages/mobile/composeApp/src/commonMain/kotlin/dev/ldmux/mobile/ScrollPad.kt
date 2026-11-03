package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The three scroll controls, floating over the terminal.
 *
 * A terminal on a phone has no scrollbar and no wheel, and a drag is the only way up the
 * history — which is right for reading, and poor for the two things a terminal actually needs:
 * stepping back a few lines while watching live output, and getting to the bottom from anywhere.
 * These are those two, and the way home:
 *
 *  - `↑` one step up, `↓` one step down: small, so a tap is a nudge rather than a jump;
 *  - `⇓` all the way to the live bottom, drawn as a double arrow so it reads as "the end" and
 *    not as a bigger down.
 *
 * They are **always present**, in both routes, at the top end of the terminal: a control that
 * appears only after the user has already scrolled is a control they had to discover a gesture
 * to find. Dragging, flinging and the natural scrollback are untouched underneath.
 */
@Composable
internal fun ScrollPad(
    onUp: () -> Unit,
    onDown: () -> Unit,
    onBottom: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier.padding(6.dp),
        verticalArrangement = Arrangement.spacedBy(4.dp),
        horizontalAlignment = Alignment.End,
    ) {
        PadButton("scroll up", "\u2191", onUp)
        PadButton("scroll down", "\u2193", onDown)
        PadButton("scroll to the bottom", "\u21d3", onBottom)
    }
}

@Composable
private fun PadButton(label: String, glyph: String, onClick: () -> Unit) {
    val size = LocalTouch.current.min
    Box(
        Modifier
            .size(size)
            .clip(RadiusMedium)
            // Translucent rather than solid: the rows underneath are the reason the user is
            // looking at the screen, and a control that hides text is a control they scroll to
            // avoid.
            .background(Surface.copy(alpha = 0.72f))
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        Text(glyph, color = Dim, fontSize = 15.sp, fontFamily = FontFamily.Monospace)
    }
}
