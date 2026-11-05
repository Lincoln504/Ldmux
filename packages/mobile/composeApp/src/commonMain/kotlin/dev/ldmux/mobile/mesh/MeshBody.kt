package dev.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.Muted

/**
 * What the mesh screen says under the header when there is no list to draw.
 *
 * [line] is the short fact, always drawn. [detail] is the second sentence, in the muted colour:
 * what the screen is doing about it, and only then what to check. The split is deliberate — a
 * state the app is already recovering from is not an error, and drawing a three-clause diagnosis
 * at full brightness made a reconnect that was working look like a broken link ("makes them feel
 * like it's actually broken", 2026-11-05). The refresh square in the header is the retry for all
 * of these, so it is not repeated as a button.
 */
class MeshBody(val line: String, val detail: String? = null)

/** Null for the states that draw something else: the waits (with their seconds) and the lists. */
fun meshBodyFor(view: MeshTermView): MeshBody? = when (view) {
    MeshTermView.Idle, MeshTermView.Handshaking, MeshTermView.ProfileMissing, MeshTermView.Listing -> null
    is MeshTermView.Live -> null
    is MeshTermView.Choose ->
        if (view.entries.isEmpty()) MeshBody("the host listed no shells: start one there, then refresh") else null
    MeshTermView.NoShells -> MeshBody("the host is running no shell: start one there, then refresh")
    is MeshTermView.HostClosed -> MeshBody(
        when (view.reason) {
            null -> "the host ended the session: refresh to connect again"
            MeshTerm.Closed.NOT_FOUND -> "that shell is gone: refresh to see what the host has"
            MeshTerm.Closed.AMBIGUOUS -> "the host runs more than one shell: refresh to choose"
            MeshTerm.Closed.EXITED -> "the shell exited: refresh to see what the host has"
            MeshTerm.Closed.UNAVAILABLE -> "the host could not read the shell's screen: refresh to try again"
            MeshTerm.Closed.VERSION -> "the host speaks another terminal version: update the older of the two"
            else -> "the host closed the terminal (${view.reason}): refresh to connect again"
        },
    )
    is MeshTermView.LinkDown -> when (view.link) {
        MeshLinkState.NeedsRadio -> MeshBody("choose the radio in Settings")
        MeshLinkState.NeedsPermission -> MeshBody("Bluetooth permission is off for LDMUX")
        MeshLinkState.NoPath -> MeshBody(
            "the host's stored path is not usable",
            "the screen retries by itself; refresh to try now, or move closer to the host radio",
        )
        MeshLinkState.Idle, MeshLinkState.Connecting, MeshLinkState.Bound ->
            MeshBody("connecting to the radio")
        MeshLinkState.RadioUnavailable -> MeshBody(
            "the radio is not connected",
            "check that it is on and in range; this screen keeps retrying",
        )
        // `Refused` comes from the radio's own error codes (not a contact, unsupported
        // command, busy), none of which is the L3 pin. Saying "check that the host is paired"
        // sent the user to the pairing screen for a radio problem.
        MeshLinkState.Refused -> MeshBody(
            "the radio refused that record",
            "that is usually a busy radio or a path that has not resolved yet; this screen retries by itself",
        )
        // **A dropped link is not a diagnosis.** Three causes are possible and only one is
        // distance, so the old line asserted "it is off or out of range" on the first failed
        // record — before the app had retried once. The short line says what happened; the detail
        // says the screen is working on it, and lists what to check only if it never connects.
        MeshLinkState.SessionFailed -> MeshBody(
            "the link dropped",
            "this screen keeps retrying by itself; refresh to try now. If it never connects: the host is off, both radios are on different presets, or this phone is not paired (on the host: ldmux code --mesh)",
        )
        MeshLinkState.Closed -> MeshBody(
            "the link closed",
            "refresh to connect again; the screen is already trying",
        )
    }
}

/** The sentence under the header when the mesh route cannot run at all. */
fun meshRouteBodyFor(problem: MeshRouteProblem?): MeshBody? = when (problem) {
    null -> null
    MeshRouteProblem.NeedsSessionKeys -> MeshBody("scan the ldmux code in Settings to pair the mesh route")
    // Re-scanning cannot fix this one: the code that was scanned simply has no host radio
    // key in it, so the same scan produces the same state. The cure is on the host.
    MeshRouteProblem.NeedsPeer -> MeshBody(
        "the code has no host radio key: print a new one on the host with `ldmux code --mesh` (see `ldmux mesh status`)",
    )
    MeshRouteProblem.NotOnThisPlatform -> MeshBody("the mesh route is not available on this device")
    MeshRouteProblem.SessionNotBuilt -> MeshBody("the session library did not load on this device")
    MeshRouteProblem.TerminalNotBuilt -> MeshBody("the terminal is unavailable on this device")
}

@Composable
internal fun MeshBodyBlock(body: MeshBody, modifier: Modifier = Modifier) {
    Column(modifier) {
        Text(body.line, color = Fg, fontSize = 13.sp)
        body.detail?.let {
            Text(it, color = Muted, fontSize = 11.sp, modifier = Modifier.padding(top = 4.dp))
        }
    }
}
