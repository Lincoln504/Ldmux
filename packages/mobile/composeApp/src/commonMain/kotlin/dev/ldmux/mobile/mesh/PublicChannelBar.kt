package dev.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
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
 * Off, it collapses to a single `hold input` button, so entering the mode is one tap from the
 * terminal and leaving it is the same tap plus the decision the bar asks for.
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
    /** The buffer is being drained right now: the button says so and cannot be pressed again. */
    flushing: Boolean,
    updates: Int,
    onToggle: () -> Unit,
    onSync: () -> Unit,
    onDiscard: () -> Unit,
    modifier: Modifier = Modifier,
) {
    if (!on) {
        // Forced mode is always on; this branch is the opt-in one only.
        // The way in. A labelled control rather than an icon: "hold input" is the whole feature in
        // two words, and a symbol for "hold" would have to be learned.
        Box(
            modifier
                .padding(6.dp)
                .clip(RadiusMedium)
                .background(Surface)
                .border(1.dp, Edge, RadiusMedium)
                .clickable(role = Role.Button, onClick = onToggle)
                .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad)
                .semantics {
                    contentDescription =
                        "hold input: keep typing on this phone and send it only when you press sync"
                },
        ) {
            Text("hold input", color = Dim, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
        }
        return
    }

    // **"View N behind" is the point of the freeze, so it cannot be shadowed by "nothing held".**
    // It used to be tested after the empty summary, so a frozen view with an empty buffer reported
    // "nothing held" and never how stale it was — which is the one thing the bar is for once the
    // buffer is empty. Ordered so the sentence is always true and always says the most useful part.
    val status = when {
        flushing -> "sending…"
        awaiting -> "waiting for the answer"
        updates > 0 && summary.isEmpty() -> "view $updates behind"
        updates > 0 -> "$summary · view $updates behind"
        summary.isEmpty() -> "nothing held"
        else -> summary
    }
    Column(
        modifier
            .padding(6.dp)
            .widthIn(max = 260.dp)
            .clip(RadiusMedium)
            .background(Surface)
            .border(1.dp, Edge, RadiusMedium)
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("public channel", color = Fg, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
            Text(status, color = if (awaiting || flushing) Warn else Muted, fontSize = 11.sp, fontFamily = FontFamily.Monospace)
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            // The standalone sync button. Filled, because it is the thing to press; disabled only
            // while a send is out, so the user can always press it again when a wait goes quiet.
            Box(
                Modifier
                    .clip(RadiusMedium)
                    // Disabled only while the buffer is actually draining. A wait for the answer
                    // can last seconds, and a user who typed more during it must be able to send
                    // it without waiting for the deadline.
                    .background(if (flushing) Line else Accent)
                    .clickable(role = Role.Button, enabled = !flushing, onClick = onSync)
                    .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad)
                    .semantics {
                        contentDescription = if (flushing) {
                            "sending: waiting for the shell's screen"
                        } else if (summary.isEmpty()) {
                            "sync: show the shell's newest screen"
                        } else {
                            "sync: send $summary, then show the shell's screen"
                        }
                    },
            ) {
                // `show` when nothing is held: with an empty buffer the button does not send,
                // it draws the newest screen the host sent, and a control should say what it does.
                Text(
                    when {
                        flushing -> "sending…"
                        summary.isEmpty() -> "show"
                        else -> "sync"
                    },
                    color = if (flushing) Dim else Bg,
                    fontSize = 13.sp,
                )
            }
            // **Discard stays live while a wait is running.** It used to be disabled for the whole
            // 20 s wait, so on a dead link the only control that could clear held text was greyed
            // out exactly when the user most wanted it. Nothing is lost by allowing it: a discard
            // while sending leaves the piece already on the air and drops the rest, which is what
            // "discard" means; a discard while only waiting leaves nothing in flight at all.
            MiniTextButton(onClick = onDiscard, enabled = summary.isNotEmpty()) {
                Text("discard", color = if (summary.isNotEmpty()) Dim else Muted, fontSize = 11.sp)
            }
        }
        // Turning the mode off is a decision about what is waiting, so the bar says which one it
        // is: leaving with text held sends it (a mode that can be left while silently dropping what
        // you typed is worse than one that sends on the way out).
        //
        // On a public channel there is no off: every keystroke would be a broadcast, and this is
        // the only supported way to type there. The line says why instead of offering a toggle.
        if (forced) {
            Text("the radio is on a public channel", color = Muted, fontSize = 11.sp)
        } else {
            MiniTextButton(onClick = onToggle) {
                Text(
                    if (summary.isEmpty()) "leave public channel mode" else "send what is held and leave",
                    color = Dim,
                    fontSize = 11.sp,
                )
            }
        }
    }
}
