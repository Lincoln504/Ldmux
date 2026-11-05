package dev.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.Bg
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.Edge
import dev.ldmux.mobile.Line
import dev.ldmux.mobile.LocalTouch
import dev.ldmux.mobile.MiniTextButton
import dev.ldmux.mobile.Muted
import dev.ldmux.mobile.RadiusMedium
import dev.ldmux.mobile.Surface
import dev.ldmux.mobile.Warn
import dev.ldmux.mobile.MonoFont

/**
 * Manual sync mode's one control: what is held, and the button that releases it.
 *
 * **Why a bar and not a key.** The mode's whole risk is silent failure: typing that goes nowhere,
 * or a screen that has stopped following the shell while still looking live. A glyph in a key row
 * cannot say "12 characters are waiting and this screen is 4 updates behind"; a labelled bar can,
 * and it stays in one place so the user's thumb learns it.
 *
 * **The mode is not switched here.** It is a property of the link — live, or held until sync — and
 * it is chosen in Settings beside the channel, because a connection-level decision does not belong
 * in a per-screen row next to the keys. This bar is drawn only while the mode is manual sync, so
 * its presence *is* the mode's indicator, and there is no "leave" button: leaving is changing the
 * setting.
 *
 * [sync] sends everything waiting **and** draws the shell's answer — one button, because the link
 * is asynchronous: sending queues records and the answer arrives as the host's next screen. A
 * second "receive" button would duplicate the wait.
 */
@Composable
internal fun PublicChannelBar(
    summary: String,
    /** A send is out and its answer has not arrived. */
    awaiting: Boolean,
    /** The deadline passed with no answer. Changes what the bar says, not what it draws. */
    reported: Boolean,
    /** The buffer is being drained right now: the button says so and cannot be pressed again. */
    flushing: Boolean,
    updates: Int,
    onSync: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // **A row of the terminal's chrome, not a card floating over the shell.** Portrait puts it
    // where the key pad begins — the space above the keyboard — and landscape puts it at the
    // **top** of the side panel's scrolling area. Either way it never covers a row of the shell's
    // output and never moves with the view's scroll.
    //
    // **The status is the feature.** "sending…" then "waiting for the answer" then "12
    // characters" is the only place the mode says what it is doing. "no answer yet" is a fact
    // about the last press and stays until the answer arrives, because the answer is still
    // coming — the deadline reports, it does not cancel. A live region: it changes without the
    // user touching the control that caused it, so a reader would otherwise read it once and
    // never hear that the send finished or that the answer arrived. Polite: it waits for the
    // current utterance rather than interrupting.
    val status = when {
        flushing -> "sending…"
        reported -> "no answer yet: the link may be busy or down"
        awaiting -> "waiting for the answer"
        updates > 0 && summary.isEmpty() -> "view $updates behind"
        updates > 0 -> "$summary · view $updates behind"
        summary.isEmpty() -> "nothing held"
        else -> summary
    }
    val statusColor = if (awaiting || flushing) Warn else Muted

    /**
     * The status, in as many lines as the shape allows. Portrait has the whole width and a single
     * line reads as a ticker; the landscape panel has 166 dp, where "no answer yet: the link may be
     * busy or down" is three lines. Wrapping beats truncating here, because the sentence is the
     * only place the mode says what went wrong.
     */
    val statusText: @Composable (Modifier, Int) -> Unit = { m, lines ->
        Text(
            status,
            color = statusColor,
            fontSize = 11.sp,
            fontFamily = MonoFont,
            maxLines = lines,
            overflow = TextOverflow.Ellipsis,
            modifier = m.semantics { liveRegion = LiveRegionMode.Polite },
        )
    }

    // The standalone sync button. Filled, because it is the thing to press; dimmed while a send
    // is out, so the user can always press it again when a wait goes quiet.
    val syncEnabled = !flushing
    val syncButton: @Composable (Modifier) -> Unit = { m ->
        Box(
            m
                .clip(RadiusMedium)
                .background(if (syncEnabled) Accent else Line)
                .clickable(role = Role.Button, enabled = syncEnabled, onClick = onSync)
                .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad)
                .semantics {
                    contentDescription = when {
                        flushing -> "sending: waiting for the shell's screen"
                        summary.isEmpty() -> "sync: show the shell's newest screen"
                        else -> "sync: send $summary, then show the shell's screen"
                    }
                },
        ) {
            // `show` when nothing is held: with an empty buffer the button does not send, it draws
            // the newest screen the host sent, and a control should say what it does.
            Text(
                when {
                    flushing -> "sending…"
                    summary.isEmpty() -> "show"
                    else -> "sync"
                },
                color = if (syncEnabled) Bg else Dim,
                fontSize = 13.sp,
                maxLines = 1,
            )
        }
    }

    // **Two shapes, because the row lives in two widths.** Portrait gives it the whole screen
    // under the terminal; landscape gives it the side panel's 190 dp, where a status line and a
    // button in one row leave the status about four characters. Narrow stacks: the status on its
    // own lines, then the button full width.
    BoxWithConstraints(
        modifier
            .fillMaxWidth()
            .background(Surface)
            .padding(horizontal = 8.dp, vertical = LocalTouch.current.pad),
    ) {
        val narrow = maxWidth < 300.dp
        if (narrow) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                statusText(Modifier.fillMaxWidth(), 2)
                syncButton(Modifier.fillMaxWidth())
            }
        } else {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                statusText(Modifier.weight(1f), 1)
                syncButton(Modifier)
            }
        }
    }
}
