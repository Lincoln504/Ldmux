package dev.ldmux.mobile.mesh

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.ldmux.mobile.Accent
import dev.ldmux.mobile.Dim
import dev.ldmux.mobile.Fg
import dev.ldmux.mobile.MiniTextButton
import dev.ldmux.mobile.Warn
import kotlinx.coroutines.launch

/**
 * What the mesh screen says under the header when there is no list to draw: one sentence for what
 * is happening, and whether the one helpful action is to ask MeshMore for a token.
 *
 * **The body is never emptier than the header.** Every state that is not a wait or a list lands
 * here, so a dead link or a closed shell is a sentence and not a black rectangle. The refresh
 * square in the header is the retry for all of them, so it is not repeated as a button.
 */
class MeshBody(
    val line: String,
    val handoff: Boolean,
    /** Whether asking MeshMore must replace the stored token, because that token is known to be dead. */
    val replaceStale: Boolean = false,
)

/** Null for the states that draw something else: the waits (with their seconds) and the lists. */
fun meshBodyFor(view: MeshTermView, endpoint: MeshGatewayEndpoint? = null): MeshBody? = when (view) {
    MeshTermView.Idle, MeshTermView.Handshaking, MeshTermView.ProfileMissing, MeshTermView.Listing -> null
    is MeshTermView.Live -> null
    is MeshTermView.Choose ->
        if (view.entries.isEmpty()) MeshBody("the host listed no shells: start one there, then refresh", false) else null
    MeshTermView.NoShells -> MeshBody("the host is running no shell: start one there, then refresh", false)
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
        handoff = false,
    )
    is MeshTermView.LinkDown -> when (view.link) {
        MeshLinkState.NeedsToken -> MeshBody("no MeshMore token yet", handoff = true)
        MeshLinkState.Idle, MeshLinkState.Connecting, MeshLinkState.Bound ->
            MeshBody("connecting to MeshMore", false)
        MeshLinkState.RadioUnavailable -> MeshBody("MeshMore's radio is not connected: connect it in MeshMore", false)
        MeshLinkState.GatewayBusy -> MeshBody("another app is using the MeshMore gateway: close it, then refresh", false)
        MeshLinkState.ProtocolMismatch -> MeshBody("MeshMore speaks another gateway version: update one of the apps", false)
        MeshLinkState.Unreachable -> MeshBody(
            "nothing on ${endpoint ?: MeshGatewayEndpoint.DEFAULT}: open MeshMore and turn on " +
                "Connected apps, or pair again if it took another port",
            handoff = true,
        )
        MeshLinkState.Refused -> MeshBody(
            "MeshMore or the host refused the link: get a new token, or pair again",
            handoff = true,
            replaceStale = true,
        )
        MeshLinkState.SessionFailed -> MeshBody("the host stopped answering: it may be off or out of range. Trying again.", false)
        MeshLinkState.Closed -> MeshBody("the link closed: refresh to connect again", false)
    }
}

/** The sentence under the header when the mesh route cannot run at all, and whether a token is the cure. */
fun meshRouteBodyFor(problem: MeshRouteProblem?): MeshBody? = when (problem) {
    null -> null
    MeshRouteProblem.NeedsToken -> MeshBody("no MeshMore token yet", handoff = true)
    MeshRouteProblem.NeedsSessionKeys -> MeshBody("scan the ldmux code in Settings to pair the mesh route", false)
    MeshRouteProblem.NeedsPeer -> MeshBody("the host radio's key is missing: scan the ldmux code in Settings", false)
    MeshRouteProblem.NotOnThisPlatform -> MeshBody("the mesh route is not available on this device", false)
    MeshRouteProblem.SessionNotBuilt -> MeshBody("the session library did not load on this device", false)
    MeshRouteProblem.TerminalNotBuilt -> MeshBody("shells over mesh are not built yet", false)
}

/**
 * What a handoff attempt looks like to the screens: the last plain-words result (null after a
 * success or before any attempt), and the one way to try again. [run]'s argument says the stored
 * token has just failed, which is the only time it may be replaced.
 */
class MeshHandoffUi(
    private val noteOf: () -> String?,
    val run: suspend (replaceStale: Boolean) -> String?,
) {
    /**
     * The last result, read at draw time.
     *
     * A property rather than a constructor field so this object's identity can be stable: it used
     * to be rebuilt on every recomposition, and the screen's automatic attempt keyed on it, so every
     * result restarted the effect and asked again — a loop at frame rate.
     */
    val note: String? get() = noteOf()

    /**
     * The automatic attempt, **once per app run**.
     *
     * Held here, on an object that outlives the connection, because the connection is rebuilt when
     * a handoff lands (the config changes), which would reset any guard remembered beside it and
     * start the cycle again.
     */
    private var autoDone = false
    fun claimAuto(): Boolean {
        if (autoDone) return false
        autoDone = true
        return true
    }
}

/** One sentence, the last handoff result when there is one, and the one action that helps. */
@Composable
internal fun MeshBodyBlock(body: MeshBody, handoff: MeshHandoffUi?, replaceStale: Boolean, modifier: Modifier = Modifier) {
    val scope = rememberCoroutineScope()
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<String?>(null) }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Text(body.line, color = Fg, fontSize = 13.sp)
        (result ?: handoff?.note)?.let { Text(it, color = Warn, fontSize = 11.sp, fontFamily = FontFamily.Monospace) }
        if (body.handoff && handoff != null) {
            MiniTextButton(
                enabled = !busy,
                onClick = {
                    busy = true
                    scope.launch {
                        result = handoff.run(replaceStale || body.replaceStale)
                        busy = false
                    }
                },
            ) { Text(if (busy) "asking MeshMore" else "get token from MeshMore", color = Accent, fontSize = 12.sp, maxLines = 1) }
        }
        if (body.handoff && handoff == null) Text("use Settings to enter the MeshMore code", color = Dim, fontSize = 11.sp)
    }
}
