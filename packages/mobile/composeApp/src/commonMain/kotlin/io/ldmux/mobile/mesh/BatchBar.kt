package io.ldmux.mobile.mesh

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
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
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
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
import io.ldmux.mobile.Line
import io.ldmux.mobile.LocalTouch
import io.ldmux.mobile.MonoFont
import io.ldmux.mobile.Muted
import io.ldmux.mobile.RadiusMedium
import io.ldmux.mobile.Surface
import io.ldmux.mobile.Warn
import kotlinx.coroutines.delay

/**
 * What this page's command is doing, as one value.
 *
 * **Four facts, not one spinner.** `Send` puts one message on the air; the host writes it to the
 * pane, waits for the pane to settle and answers with exactly one screen. So the states are:
 * nothing out, out and unanswered, answered, and the deadline passed with nothing back. The old
 * model had eleven of these because it was tracking a freeze, a drain and a queue; v15 has no
 * freeze and no queue (`docs/RADIO-PLAN.md` §7).
 */
internal enum class BatchState {
    /** Nothing is out. `Send` sends the draft, `Refresh` asks for the screen. */
    Idle,

    /** The message is being handed to the radio. */
    Sending,

    /** It is on the air and the host has not answered yet. */
    Waiting,

    /** The host answered: its screen arrived. */
    Settled,

    /** The deadline passed with no answer. The answer may still come; this says it is late. */
    NoAnswer,
}

/** The two numbers the status line is made of: how far, and how long since anything came back. */
internal data class LinkEvidence(val hops: Int?, val answeredAgoMs: Long?)

/**
 * **What the link has actually said, in one sentence.**
 *
 * One place, because the bar, its tooltip and the "waiting for shell screen" line all say it: two
 * vocabularies for the same fact is how a screen ends up contradicting its own tooltip. "direct" is
 * said rather than "0 hops" because zero repeaters is the good case, not an absence.
 */
internal fun linkEvidenceText(
    hops: Int?,
    answeredAgoMs: Long?,
    exchangeMs: Long? = null,
): String {
    val where = hops?.let { if (it <= 0) "direct" else "$it hop${if (it == 1) "" else "s"}" }
    val back = when {
        answeredAgoMs == null -> "nothing back yet"
        answeredAgoMs < 2_000L -> "answered just now"
        else -> "answered ${answeredAgoMs / 1000} s ago"
    }
    // **The price of one exchange, because that is the number that decides whether typing is
    // possible.** A record and its answer each cross the whole route, so one exchange is
    // `2 × hops` records: 4 s at a neighbour, 34 s at twenty-eight hops. Without it the operator
    // reads "28 hops" and has no way to know that the terminal is unusable there rather than slow.
    val price = exchangeMs?.takeIf { it > 0L && (hops ?: 0) > 0 }
        ?.let { "~${it / 1000}s/exchange" }
    return listOfNotNull(where, price, back).joinToString(" · ")
}

/**
 * Batch mode's two controls: what is typed, and the press that sends it.
 *
 * **Why a bar and not a key.** The mode's whole risk is silent failure: typing that goes nowhere, or
 * a press whose answer never comes. A glyph in a key row cannot say "12 characters, waiting for the
 * answer"; a labelled bar can, and it stays in one place so the user's thumb learns it.
 *
 * **`Send` and `Refresh` are separate because they do different things.** `Send` puts the draft on
 * the air and never touches what is not typed; `Refresh` asks for the screen and **never touches the
 * draft**, so looking at a shell can never run what is typed in it. One button that did both was the
 * old `sync`, and it is why a `show` had to be argued about at all.
 */
@Composable
internal fun BatchBar(
    /** The draft, as the bar shows it: "12 characters", "2 keys and 3 characters". */
    summary: String,
    state: BatchState,
    onSend: () -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
    /**
     * What the link has actually said, for the line that says what this press is doing.
     *
     * **"sent" is a claim about the caller; "nothing back yet" is a fact about the air.** At range
     * the difference is the whole diagnosis: an answer that is coming takes `2 × hops × airtime`,
     * and a person watching a still screen cannot tell that from a link that has stopped carrying
     * anything.
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
    // A row of the terminal's chrome, not a card floating over the shell: it never covers a row of
    // the shell's output and never moves with the view's scroll.
    //
    // **The status is the feature**, and a live region: it changes without the user touching the
    // control that caused it, so a reader would otherwise read it once and never hear that the send
    // finished or that the answer arrived. Polite: it waits for the current utterance.
    var ticked by remember { mutableStateOf(LinkEvidence(hops, answeredAgoMs)) }
    val reading: () -> String = evidence ?: { linkEvidenceText(hops, answeredAgoMs) }
    LaunchedEffect(state, evidence) {
        if (state != BatchState.Waiting && state != BatchState.NoAnswer) return@LaunchedEffect
        while (true) {
            ticked = LinkEvidence(hops, answeredAgoMs)
            delay(1_000)
        }
    }
    val said = if (evidence != null) reading() else linkEvidenceText(ticked.hops, ticked.answeredAgoMs)
    val status = when (state) {
        BatchState.Sending -> "sending…"
        BatchState.Waiting -> "waiting for the answer · $said"
        BatchState.NoAnswer -> "no answer yet · link busy or down · $said"
        BatchState.Settled -> "done · $said"
        BatchState.Idle -> if (summary.isEmpty()) "nothing typed" else summary
    }
    val statusColor = when (state) {
        BatchState.Waiting, BatchState.Sending -> Warn
        BatchState.NoAnswer -> Warn
        else -> Muted
    }

    /**
     * The status, in as many lines as the shape allows. Portrait has the whole width and a single
     * line reads as a ticker; the landscape panel is narrow, where the sentence is three lines.
     * Wrapping beats truncating here, because the sentence is the only place the mode says what went
     * wrong.
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

    // **`Send` is dimmed while the draft is empty, and does nothing then** — there is nothing to
    // send, and a press that sends nothing is a press that lies. `Refresh` is always live: asking
    // for the screen is always meaningful, and it is the one control that must work when the link
    // is slow.
    val sendEnabled = state != BatchState.Sending && summary.isNotEmpty()
    val sendButton: @Composable (Modifier) -> Unit = { m ->
        Box(
            m
                .clip(RadiusMedium)
                .background(if (sendEnabled) Accent else Line)
                .clickable(role = Role.Button, enabled = sendEnabled, onClick = onSend)
                .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad)
                .semantics {
                    contentDescription = when {
                        state == BatchState.Sending -> "sending"
                        summary.isEmpty() -> "send: nothing typed"
                        else -> "send $summary"
                    }
                },
        ) {
            Text(
                if (state == BatchState.Sending) "sending…" else "send",
                color = if (sendEnabled) Bg else Dim,
                fontSize = 13.sp,
                maxLines = 1,
            )
        }
    }
    val refreshButton: @Composable (Modifier) -> Unit = { m ->
        Box(
            m
                .clip(RadiusMedium)
                .background(Surface)
                .clickable(role = Role.Button, onClick = onRefresh)
                .padding(horizontal = 12.dp, vertical = LocalTouch.current.pad)
                .semantics { contentDescription = "refresh: ask for the screen, and never send the draft" },
        ) {
            Text("refresh", color = Fg, fontSize = 13.sp, maxLines = 1)
        }
    }

    // **Two shapes, because the row lives in two widths.** Portrait gives it the whole screen under
    // the terminal; landscape gives it the side panel, where a status line and two buttons in one row
    // leave the status a few characters. Narrow stacks: the status on its own lines, then the buttons
    // full width.
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
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    sendButton(Modifier.weight(1f))
                    refreshButton(Modifier.weight(1f))
                }
            }
        } else {
            Row(
                Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                statusText(Modifier.weight(1f), 1)
                sendButton(Modifier)
                refreshButton(Modifier)
            }
        }
    }
}

/**
 * One exchange over a route of `hops` repeaters, in milliseconds: a record out and its answer back.
 *
 * `2 × hops × airtime` is the floor, and it is the number that decides whether a terminal is usable
 * on this route at all: a 172-byte record at the public tuple is 603 ms, so one exchange is 4 s at a
 * neighbour and 34 s at twenty-eight hops. Null when the preset is unknown, so the line says what it
 * knows and never invents a price.
 */
internal fun exchangeMs(hops: Int?, preset: RadioPreset?): Long? {
    val h = hops ?: return null
    if (h <= 0 || preset == null) return null
    val airtime = RadioAirtime.timeOnAirMs(preset, MeshTerm.RECORD_PAYLOAD + 29)
    if (!airtime.isFinite() || airtime <= 0.0) return null
    return (2.0 * h * airtime).toLong()
}
