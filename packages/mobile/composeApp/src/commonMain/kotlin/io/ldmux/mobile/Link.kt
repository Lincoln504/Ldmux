package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshEnrollOutcome
import io.ldmux.mobile.mesh.MeshEnrollment
import io.ldmux.mobile.mesh.MeshRoute
import io.ldmux.mobile.mesh.MeshRouteProblem
import io.ldmux.mobile.mesh.meshProblemLabel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow

/**
 * The state of the app's link to its daemon, as one value.
 *
 * There used to be a `connected` boolean and nothing else, so "the token was
 * refused", "the machine is off" and "not paired at all" all drew as the same empty
 * list under the same host name, and the only sentence anywhere was "not connected
 * to the host", shown after trying to open a shell.
 */
enum class LinkState {
    /** No pairing: nothing to connect to. */
    Unpaired,
    /** Paired, and the first attempt since pairing (or launch) is under way. */
    Connecting,
    Online,
    /** Paired, and the machine cannot be reached. Retried with backoff. */
    Unreachable,
    /**
     * The daemon answered and refused the token: the pairing is stale (the token
     * was rotated). Not retried on a timer — nothing about waiting will fix it —
     * only on refresh or a new pairing.
     */
    Rejected,
    /**
     * Paired, but the chosen route cannot run: the session library is missing, the terminal
     * profile is not built in, or a credential is missing. Nothing is opened and nothing falls
     * back to the tailnet: the user chose mesh, so the reason is shown
     * (`LdmuxConfig.routeProblem`).
     */
    RouteUnavailable,
}

/**
 * What a failed connection means, from the daemon's answer to a plain `GET /ws`.
 *
 * The WebSocket handshake failing says nothing about why; its exception is a
 * library's sentence about a status code. The daemon answers the same path without
 * upgrading, with the same checks, so one request tells the cases apart by status
 * alone: 401 is a refused token. 400 means the token was accepted (and the upgrade
 * failed for some other reason, which is transient).
 *
 * **A connection with nothing listening is its own sentence**, and the only one of these the
 * person can fix on the other machine: the daemon is not running. It was `unreachable`, which
 * sent them checking a network that was working ([`probeLink`]).
 */
internal fun linkStateFromProbe(answer: ProbeAnswer): LinkState = when (answer) {
    is ProbeAnswer.Answer -> if (answer.status == 401) LinkState.Rejected else LinkState.Unreachable
    // Both are "down, retry": the reason is the line's business ([linkReasonLabel]), not the
    // state's, because the retry ladder and the walk home are the same for either.
    ProbeAnswer.Refused -> LinkState.Unreachable
    ProbeAnswer.NoAnswer -> LinkState.Unreachable
}

/** The one line under a link that is down, from the probe's own answer. */
internal fun linkReasonLabel(answer: ProbeAnswer?): String? = when (answer) {
    ProbeAnswer.Refused -> "daemon not running"
    else -> null
}

/** The launcher's line under the title: what it is paired with, and how that is going. */
fun linkLabel(
    state: LinkState,
    config: LdmuxConfig,
    /** Why the saved route cannot run, when the caller has read the real credentials. */
    problem: MeshRouteProblem? = config.routeProblem(),
    /** Why the last attempt failed, when the probe could say: [`linkReasonLabel`]. */
    reason: String? = null,
): String = when (state) {
    LinkState.Unpaired -> "unpaired"
    LinkState.Online -> config.pairedName
    LinkState.Connecting -> "${config.pairedName} · connecting"
    LinkState.Unreachable -> reason?.let { "${config.pairedName} · $it" } ?: "${config.pairedName} · unreachable"
    LinkState.Rejected -> "${config.pairedName} · rejected"
    LinkState.RouteUnavailable ->
        "${config.pairedName} · ${problem?.let(::meshProblemLabel) ?: "mesh route unavailable"}"
}
/** The link a config starts in: unpaired, blocked on its route, or connecting. */
fun initialLink(config: LdmuxConfig): LinkState = when {
    // A mesh-only device has no tailnet pairing, and that is not "unpaired": the mesh route has its
    // own credentials. It is blocked on its route (shown by `routeProblem`) or, if it can run, the
    // mesh screen replaces this surface.
    config.route == MeshRoute.Mesh -> LinkState.RouteUnavailable
    !config.isPaired -> LinkState.Unpaired
    config.endpoint == null -> LinkState.RouteUnavailable
    else -> LinkState.Connecting
}

/**
 * The one owner of the config: load, save, and tell everyone.
 *
 * Three places used to write it — `App()` held it, and two copies of the settings
 * dialog's `onSave` each saved it and reconnected the client — so a change had to be
 * made correctly in all three. This is where pairing and unpairing happen too, and
 * what the rest of the app observes.
 */
class ConfigStore(
    initial: LdmuxConfig,
    /** Where a pairing's mesh keys go (null: this build keeps none). Never part of the config. */
    private val mesh: MeshEnrollment? = null,
    private val persist: (LdmuxConfig) -> Unit,
) {
    private val _config = MutableStateFlow(initial)
    val config: StateFlow<LdmuxConfig> = _config

    fun update(next: LdmuxConfig) {
        if (next == _config.value) return
        persist(next)
        _config.value = next
    }

    /**
     * Apply a scanned pairing. A tailnet pairing replaces the daemon's address and token as it
     * always did. A **mesh-only** pairing ([Pairing.isMeshOnly]) enrols the mesh keys and touches
     * nothing of the tailnet pairing: it has no daemon to offer, and it must not erase one.
     * Returns what happened to the mesh keys.
     */
    fun pair(p: Pairing): MeshEnrollOutcome {
        val previous = _config.value.host
        // Keys first: a pairing that carried them and could not store them must not look complete.
        val outcome = mesh?.onPaired(previous, p) ?: MeshEnrollOutcome.NoMeshKeys
        if (p.isMeshOnly) {
            // Only the display name, and only if this device has none: the machine's name is the
            // tailnet pairing's, and a mesh-only code must not rename it.
            val c = _config.value
            if (c.pairName.isEmpty() && c.host.isEmpty() && p.name.isNotEmpty()) update(c.copy(pairName = p.name))
        } else {
            update(_config.value.paired(p))
        }
        // **What the desktop's radio is on, kept for later.** The panel compares it with this
        // phone's radio and offers to match, which turns "get both radios on the same tuple" from
        // comparing two screens by eye into one press — and a mismatch into a sentence instead of a
        // link that never comes up. A code without it clears the old value rather than keeping a
        // stale one: the desktop may since have been retuned.
        // The width is written as a fifth number, because it is part of the channel: a phone that
        // matches the frequency and keeps its own width cannot follow a route a public repeater
        // built. Kept as text so the settings row can show the host's channel without a migration.
        val preset = p.mesh?.hostPreset
        val text = preset?.let {
            "${(it.frequencyMhz * 1000).toInt()},${(it.bandwidthKhz * 1000).toInt()}," +
                "${it.spreadingFactor},${it.codingRate},${it.pathHashSize}"
        } ?: ""
        if (text != _config.value.meshHostPreset) update(_config.value.copy(meshHostPreset = text))
        return outcome
    }

    /** Forget the machine: the tailnet pairing and, with it, the mesh keys and the selected radio. */
    fun unpair() {
        mesh?.onUnpaired()
        update(_config.value.unpaired().withoutMesh())
    }

    /**
     * Forget **only** the mesh pairing: the session keys and the selected radio. The tailnet
     * pairing is untouched. If the mesh route was the saved route it goes
     * back to the tailnet, so the saved route is never one whose credentials were just deleted.
     */
    fun unpairMesh() {
        mesh?.onUnpaired()
        update(_config.value.withoutMesh())
    }

    private fun LdmuxConfig.withoutMesh(): LdmuxConfig =
        copy(meshRadio = "", meshRadioName = "", route = MeshRoute.Tailnet)
}

/** How long the "why did that fail" request may take before the answer is "unreachable". */
internal const val PROBE_TIMEOUT_MS = 3_000L
