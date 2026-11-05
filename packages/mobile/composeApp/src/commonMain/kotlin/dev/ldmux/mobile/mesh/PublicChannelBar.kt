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
import androidx.compose.ui.text.font.FontFamily
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

/**
 * Public-channel mode's one control, drawn over the bottom of the terminal.
 *
 * **Why a bar and not a key.** The mode's whole risk is silent failure: typing that goes nowhere,
 * or a screen that has stopped following the shell while still looking live. A glyph in a key row
 * cannot say "12 characters are waiting and this screen is 4 updates behind"; a labelled bar can,
 * and it stays in one place so the user's thumb learns it. The bar is also the mode's own
 * indicator — while it is drawn, the mode is on.
 *
 * **Off is a state of the row, not a different row.** The same three things are drawn either way
 * — the status line, the sync button and the mode's own switch — and the ones that cannot act while
 * the mode is off are dimmed rather than removed. It used to collapse to a lone `hold input`
 * button, which hid the sync button in exactly the state where a user looking for it is most
 * likely to be (the mode was off and they did not know), and made the row's one remaining control
 * jump from the left edge to the right when the mode came on.
 *
 * [sync] is the standalone control the mode exists for. It sends everything waiting **and** draws
 * the shell's answer — one button, because the link is asynchronous: sending queues records and the
 * answer arrives as the host's next screen. A second "receive" button would duplicate the wait.
 */
@Composable
internal fun PublicChannelBar(
    on: Boolean,
    /** The mode is not the user's choice here: the radio is on a public channel. */
    forced: Boolean = false,
    summary: String,
    /** A send is out and its answer has not arrived. */
    awaiting: Boolean,
    /** The deadline passed with no answer. Changes what the bar says, not what it draws. */
    reported: Boolean,
    /** The buffer is being drained right now: the button says so and cannot be pressed again. */
    flushing: Boolean,
    updates: Int,
    onToggle: () -> Unit,
    onSync: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // **A row of the terminal's chrome, not a card floating over the shell.** Portrait puts it
    // where the key pad begins — the space above the keyboard — and landscape puts it at the
    // **top** of the side panel's scrolling area (asked for by name, 2026-11-04: it used to sit at
    // the panel's foot, where a control the user presses while reading belongs below everything
    // else, not at the end of a scroll nobody reaches). Either way it never covers a row of the
    // shell's output and never moves with the view's scroll. A floating box over the terminal
    // covered the bottom-right of the screen, which is where a shell's last line and the cursor
    // usually are.
    //
    // **Every control is always drawn; one that cannot act is dimmed, never removed.**
    //
    // This row used to collapse to a single `hold input` button when the mode was off, so the
    // status line and the sync button vanished and came back — and a user told to press sync
    // found no sync button because the mode happened to be off. A row whose members come and go
    // also moves the ones that remain, so the thumb has to re-learn it after every tap. Off is a
    // state of the row, not a different row: the status says what the mode is not doing, and the
    // controls that need the mode are dim.
    //
    // **The status is the feature.** "sending…" then "waiting for the answer" then "12
    // characters" is the only place the mode says what it is doing. "no answer yet" is a fact
    // about the last press and stays until the answer arrives, because the answer is still
    // coming — the deadline reports, it does not cancel. A live region: it changes without the
    // user touching the control that caused it, so a reader would otherwise read it once and
    // never hear that the send finished or that the answer arrived. Polite: it waits for the
    // current utterance rather than interrupting.
    val status = when {
        // Short on purpose: the row shares a phone's width with two buttons, and a status that
        // ellipsises says less than a shorter one that fits. "off" is the whole fact; the button
        // beside it says what turning it on does.
        !on -> "held input off"
        flushing -> "sending…"
        reported -> "no answer yet: the link may be busy or down"
        awaiting -> "waiting for the answer"
        updates > 0 && summary.isEmpty() -> "view $updates behind"
        updates > 0 -> "$summary · view $updates behind"
        summary.isEmpty() -> "nothing held"
        else -> summary
    }
    val statusColor = if (on && (awaiting || flushing)) Warn else Muted

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
            fontFamily = FontFamily.Monospace,
            maxLines = lines,
            overflow = TextOverflow.Ellipsis,
            modifier = m.semantics { liveRegion = LiveRegionMode.Polite },
        )
    }

    // The standalone sync button. Filled, because it is the thing to press; dimmed while a send
    // is out, so the user can always press it again when a wait goes quiet — and dimmed, not
    // hidden, while the mode is off, where there is nothing held to send or show.
    val syncEnabled = on && !flushing
    val syncButton: @Composable (Modifier) -> Unit = { m ->
        Box(
            m
                .clip(RadiusMedium)
                .background(if (syncEnabled) Accent else Line)
                .clickable(role = Role.Button, enabled = syncEnabled, onClick = onSync)
                .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad)
                .semantics {
                    contentDescription = when {
                        !on -> "sync unavailable: hold input is off"
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

    // **Leaving is one word, and there is no "discard".** A discard button was a second way to do
    // what leaving does (leaving sends), and its own case — "throw away what I typed" — is a thing
    // a user does by deleting the text they can see. On a public channel there is no leaving: every
    // keystroke there would be a broadcast, so the slot says why and stays dim.
    //
    // Leaving is the way out when the mode is on; when it is off the same slot is the way in, and
    // it is the row's one *primary* action.
    val modeControl: @Composable (Modifier) -> Unit = { m ->
        when {
            forced -> Box(m.padding(horizontal = 4.dp, vertical = LocalTouch.current.pad)) {
                Text("public channel", color = Muted, fontSize = 11.sp, fontFamily = FontFamily.Monospace, maxLines = 1)
            }
            on -> MiniTextButton(onClick = onToggle, modifier = m) {
                Text(
                    if (summary.isEmpty()) "leave" else "send & leave",
                    color = Dim,
                    fontSize = 11.sp,
                    maxLines = 1,
                )
            }
            else -> Box(
                // The way in. A labelled control rather than an icon: "hold input" is the whole
                // feature in two words, and a symbol for "hold" would have to be learned.
                m
                    .clip(RadiusMedium)
                    .border(1.dp, Edge, RadiusMedium)
                    .clickable(role = Role.Button, onClick = onToggle)
                    // 6 dp, not 10: in the panel's 190 dp the two buttons share ~395 px and "hold
                    // input" needs ~196 of them. Ten dp of padding each side clipped the last two
                    // letters to "hold inp" (measured on the device, 2026-11-04).
                    .padding(horizontal = 6.dp, vertical = LocalTouch.current.pad)
                    .semantics {
                        contentDescription =
                            "hold input: keep typing on this phone and send it only when you press sync"
                    },
            ) {
                Text(
                    "hold input",
                    color = Dim,
                    fontSize = 11.sp,
                    fontFamily = FontFamily.Monospace,
                    maxLines = 1,
                    softWrap = false,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }

    // **Two shapes, because the row lives in two widths.** Portrait gives it the whole screen
    // under the terminal; landscape gives it the side panel's 190 dp, where a status line and two
    // buttons in one row leave the status about four characters — measured on the device, the
    // status vanished entirely and "hold input" wrapped onto two lines. Narrow stacks: the status
    // on its own line, the two controls sharing the next.
    BoxWithConstraints(
        modifier
            .fillMaxWidth()
            .background(Surface)
            .padding(horizontal = 8.dp, vertical = LocalTouch.current.pad),
    ) {
        val narrow = maxWidth < 300.dp
        if (narrow) {
            // **Stacked, not squeezed.** The panel's content width is 166 dp. "hold input" is ten
            // monospace characters and needs ~220 px of it; beside `show` there are 215 px, so any
            // split of one line truncates it — measured on the device 2026-11-04, an even split
            // drew "hold ir", a 1.3 share drew "hold in…". A full-width control per line is the
            // shape that fits, and it gives each control a thumb-sized target instead of two
            // halves of one.
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                statusText(Modifier.fillMaxWidth(), 2)
                syncButton(Modifier.fillMaxWidth())
                modeControl(Modifier.fillMaxWidth())
            }
        } else {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                statusText(Modifier.weight(1f), 1)
                syncButton(Modifier)
                modeControl(Modifier)
            }
        }
    }
}
