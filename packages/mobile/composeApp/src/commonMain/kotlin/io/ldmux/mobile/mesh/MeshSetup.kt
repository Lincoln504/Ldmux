package io.ldmux.mobile.mesh

import io.ldmux.mobile.Pairing

/** The two things a mesh attach needs, each of which can be missing on its own. */
enum class MeshCredential { SessionKeys, HostRadioKey }

/**
 * What the route setup screen shows, as one pure value: which credentials are present and what the
 * saved route would do right now. The screen draws this and decides nothing.
 */
data class MeshSetupView(
    val present: Set<MeshCredential>,
    /** What a mesh route would resolve to with these credentials on this device. */
    val meshResolution: MeshRouteResolution,
)

/** The setup view given what secure storage holds. Pure: pass the facts in. */
fun meshSetupView(
    state: MeshCredentialState,
    sessionBuilt: Boolean = MESH_SESSION_BUILT && sessionLibAvailable(),
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): MeshSetupView {
    val present = buildSet {
        if (state.hasSessionKeys) add(MeshCredential.SessionKeys)
        if (state.hasPeer) add(MeshCredential.HostRadioKey)
    }
    val resolution = resolveRoute(
        MeshRoute.Mesh,
        hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
        sessionBuilt = sessionBuilt, terminalBuilt = terminalBuilt,
    )
    return MeshSetupView(present, resolution)
}

/** One credential as a settings line. */
fun meshCredentialName(c: MeshCredential): String = when (c) {
    MeshCredential.SessionKeys -> "keys"
    MeshCredential.HostRadioKey -> "host radio key"
}


/**
 * What a finished pairing should say, when the code is not the whole answer.
 *
 * A pairing is one gesture with three different endings that used to look identical: the
 * tailnet changed (the header says so by itself), the radio was enrolled but the app is still
 * on the tailnet route (nothing on screen changes), or the keys could not be stored (the
 * pairing looks complete and the radio route never works). The last two are the ones a user
 * cannot discover without being told, and the middle one is the common case on a first run
 * with `ldmux code --mesh-only`.
 *
 * [radioKeysStored] is null when there is nothing to ask: a build with no radio route, or a
 * code that carried no mesh keys.
 */
data class PairingNote(
    val text: String,
    /** Whether the note comes with the one press that finishes the job: use the radio route. */
    val useRadio: Boolean = false,
)

fun pairingNote(
    p: Pairing,
    radioKeysStored: Boolean?,
    route: MeshRoute,
    hasMeshRoute: Boolean,
): PairingNote? = when {
    // A code that is only keys, on a build that has no radio: nothing was paired and nothing
    // can be, and the honest line is that the code cannot be used here.
    p.isMeshOnly && !hasMeshRoute -> PairingNote("no radio route in this build")
    // The tailnet half landed and the radio half did not. Saying "paired" alone would promise a
    // route whose keys are not in the Keystore, and the radio row would then say "unpaired".
    p.mesh != null && radioKeysStored == false ->
        PairingNote("paired · radio keys not stored · scan again or re-pair")
    // Keys stored, route still the tailnet: the app stays where it was, so this is the only
    // sentence that can tell the user the radio half worked.
    p.isMeshOnly && route != MeshRoute.Mesh -> PairingNote("radio paired · app still on tailnet", useRadio = true)
    p.isMeshOnly -> PairingNote("radio paired")
    else -> null
}

/** Whether the one `unpair` verb has anything to forget. */
fun canUnpair(isPaired: Boolean, radioPaired: Boolean): Boolean = isPaired || radioPaired
