package io.ldmux.mobile.mesh

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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import kotlinx.coroutines.delay
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.ldmux.mobile.Accent
import io.ldmux.mobile.Bg
import io.ldmux.mobile.Dim
import io.ldmux.mobile.Fg
import io.ldmux.mobile.Edge
import io.ldmux.mobile.Line
import io.ldmux.mobile.LocalTouch
import io.ldmux.mobile.MiniTextButton
import io.ldmux.mobile.Muted
import io.ldmux.mobile.RadiusMedium
import io.ldmux.mobile.Surface
import io.ldmux.mobile.Warn
import io.ldmux.mobile.MonoFont

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
 * in a per-screen row next to the keys. This bar is drawn only while the mode is batch, so
 * its presence *is* the mode's indicator, and there is no "leave" button: leaving is changing the
 * setting.
 *
 * [sync] sends everything waiting **and** draws the shell's answer — one button, because the link
 * is asynchronous: sending queues records and the answer arrives as the host's next screen. A
 * second "receive" button would duplicate the wait.
 */
/** The two numbers the status line is made of: how far, and how long since anything came back. */
internal data class LinkEvidence(val hops: Int?, val answeredAgoMs: Long?)

/**
 * **What the link has actually said, in one sentence.**
 *
 * One place, because the bar, its tooltip and the "waiting for shell screen" line all say it: two
 * vocabularies for the same fact is how a screen ends up contradicting its own tooltip. "direct" is
 * said rather than "0 hops" because zero repeaters is the good case, not an absence.
 */
internal fun linkEvidenceText(hops: Int?, answeredAgoMs: Long?): String {
    val where = hops?.let { if (it <= 0) "direct" else "$it hop${if (it == 1) "" else "s"}" }
    val back = when {
        answeredAgoMs == null -> "nothing back yet"
        answeredAgoMs < 2_000L -> "answered just now"
        else -> "answered ${answeredAgoMs / 1000} s ago"
    }
    return if (where == null) back else "$where · $back"
}

@Composable
internal fun PublicChannelBar(
    summary: String,
    /** A send is out and its answer has not arrived. */
    awaiting: Boolean,
    /** The deadline passed with no answer. Changes what the bar says, not what it draws. */
    reported: Boolean,
    /** The buffer is being drained right now: the button says so and cannot be pressed again. */
    flushing: Boolean,
    onSync: () -> Unit,
    modifier: Modifier = Modifier,
    /** A newer screen is being kept unread: `show` would draw it. */
    held: Boolean = false,
    /**
     * What the link has actually said, for the line that says what this mode is doing.
     *
     * **"sent · …" is a claim about the caller; "nothing back yet" is a fact about the air.** At range the difference is the whole diagnosis: an answer that is coming takes
     * `2 × hops × airtime`, and a person watching a still screen cannot tell that from a link that
     * has stopped carrying anything. The two numbers are cheap — the hop count the link already
     * holds, and how long since a record last arrived — and they turn the status from a spinner into
     * a reading.
     */
    hops: Int? = null,
    /** Milliseconds since a record last arrived from the host; null when nothing ever has. */
    answeredAgoMs: Long? = null,
    /**
     * The same evidence, read afresh on this bar's own ticker.
     *
     * A lambda rather than the two numbers, because the numbers change every second: reading them
     * here recomposes this row alone, where passing them from the page would recompose the terminal
     * above it — a full redraw of a 46x17 surface to advance one digit.
     */
    evidence: (() -> String)? = null,
) {
    // **A row of the terminal's chrome, not a card floating over the shell.** Portrait puts it
    // where the key pad begins — the space above the keyboard — and landscape puts it at the
    // **top** of the side panel's scrolling area. Either way it never covers a row of the shell's
    // output and never moves with the view's scroll.
    //
    // **The status is the feature.** "sending…", then "sent · direct · nothing back yet", then
    // "12 characters": the only place the mode says what it is doing. The evidence half is a fact
    // about the link and stays until the answer arrives, because the answer is still coming — the
    // deadline reports, it does not cancel. A live region: it changes without the
    // user touching the control that caused it, so a reader would otherwise read it once and
    // never hear that the send finished or that the answer arrived. Polite: it waits for the
    // current utterance rather than interrupting.
    // **No count of screens.** The bar used to say "view 3 behind": a number nobody can act on,
    // counting screens the host had already sent and this phone had decided not to draw. What the
    // operator needs from this line is what the button will do next, which is the four states below.
    // **The air's own words, and they move.** "waiting for answer" is a claim about the caller;
    // "nothing back for 12 s" is a fact about the link, and at range the difference between an
    // answer that is coming (`2 × hops × airtime`) and a link that has stopped carrying anything is
    // the whole diagnosis. Re-read once a second only while this bar is talking about a send.
    var ticked by remember { mutableStateOf(LinkEvidence(hops, answeredAgoMs)) }
    val reading: () -> String = evidence ?: { linkEvidenceText(hops, answeredAgoMs) }
    LaunchedEffect(awaiting, reported, evidence) {
        if (!awaiting && !reported) return@LaunchedEffect
        while (true) {
            ticked = LinkEvidence(hops, answeredAgoMs)
            delay(1_000)
        }
    }
    // The lambda wins when given one: it reads the live link, not the values from composition.
    val said = if (evidence != null) reading() else linkEvidenceText(ticked.hops, ticked.answeredAgoMs)
    val status = when {
        flushing -> "sending…"
        // **A deadline with the evidence beside it.** "link busy or down" was two guesses; this says
        // how long nothing has come back, which is the only fact the phone has.
        reported -> "no answer · $said"
        awaiting -> "sent · $said"
        // **"Nothing typed", not "nothing held".** The line sits beside a button that says `show`,
        // and the mode may still be holding a screen the host sent: what is empty here is the
        // typed buffer, which is the only thing this summary counts.
        //
        // **A held screen is said out loud.** `show` with nothing typed did nothing anyone could
        // see, and the two states it can be in (the host has sent nothing since the last paint, or
        // there is a newer screen waiting) looked identical: both read "nothing typed", so the one
        // press that would have shown the output of a command already on the host had no
        // advertisement at all.
        summary.isEmpty() && held -> "output waiting · show"
        summary.isEmpty() -> "nothing typed"
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

    // **One control, always live.** It is dimmed while a send is out, but pressing it still does
    // something: it re-arms the wait and re-drains, which is what a user means when they press the
    // one button while nothing is coming back. Disabling it made the only control on the page dead
    // for up to the deadline (20 s) whenever the link stalled — with no other way to ask again.
    val syncEnabled = true
    val syncButton: @Composable (Modifier) -> Unit = { m ->
        Box(
            m
                .clip(RadiusMedium)
                .background(if (syncEnabled) Accent else Line)
                .clickable(role = Role.Button, enabled = syncEnabled, onClick = onSync)
                .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad)
                .semantics {
                    contentDescription = when {
                        flushing -> "sending: waiting for shell screen"
                        awaiting || reported -> "sync: $said"
                        summary.isEmpty() && held -> "show: draw the output waiting"
                        summary.isEmpty() -> "sync: show newest screen"
                        else -> "sync: send $summary, show screen"
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
    // button in one row leave the status ≈4 characters. Narrow stacks: the status on its
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
