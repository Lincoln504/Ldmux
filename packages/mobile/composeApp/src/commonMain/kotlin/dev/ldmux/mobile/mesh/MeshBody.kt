package dev.ldmux.mobile.mesh

import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Fg

/**
 * What the mesh screen says under the header when there is no list to draw: one sentence for what
 * is happening. Every state that is not a wait or a list lands here, so a dead link or a closed
 * shell is a sentence and not a black rectangle. The refresh square in the header is the retry for
 * all of them, so it is not repeated as a button.
 */
class MeshBody(val line: String)

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
        MeshLinkState.NoPath -> MeshBody("the host's stored path is not usable for this record: refresh the host contact or move closer")
        MeshLinkState.Idle, MeshLinkState.Connecting, MeshLinkState.Bound ->
            MeshBody("connecting to the radio")
        MeshLinkState.RadioUnavailable -> MeshBody("the radio is not connected: check that it is on and in range")
        // `Refused` comes from the radio's own error codes (not a contact, unsupported
        // command, busy), none of which is the L3 pin. Saying "check that the host is paired"
        // sent the user to the pairing screen for a radio problem.
        MeshLinkState.Refused -> MeshBody("the radio refused the record: it is busy, or this radio is not a contact of the host's")
        // Three causes, and only one of them is distance. A host that does not know this
        // phone answers nothing at all — a pin mismatch ends before its half of the
        // handshake — so "off or out of range" was the wrong half of the story.
        MeshLinkState.SessionFailed -> MeshBody("the host did not answer: it is off or out of range, both radios are on different presets, or it does not know this phone. On the host: ldmux code --mesh")
        MeshLinkState.Closed -> MeshBody("the link closed: refresh to connect again")
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
    Text(body.line, color = Fg, fontSize = 13.sp, modifier = modifier)
}
