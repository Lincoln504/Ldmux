package io.ldmux.mobile.mesh

import io.ldmux.mobile.ConfigStore
import io.ldmux.mobile.parseScannedPairing

/** The two things a mesh attach needs, each of which can be missing on its own. */
enum class MeshCredential { SessionKeys, HostRadioKey }

/**
 * What the route setup screen shows, as one pure value: which credentials are present, what the
 * saved route would do right now, and whether the route exists here. The screen draws this and
 * decides nothing.
 */
data class MeshSetupView(
    val present: Set<MeshCredential>,
    /** What a mesh route would resolve to with these credentials on this device. */
    val meshResolution: MeshRouteResolution,
    /** Whether a mesh route could be offered at all on this platform (false on iOS: the section is hidden). */
    val meshExists: Boolean,
)

/** The setup view given what secure storage holds. Pure: pass the facts in. */
fun meshSetupView(
    state: MeshCredentialState,
    platform: MobilePlatform = io.ldmux.mobile.mesh.currentPlatform,
    sessionBuilt: Boolean = MESH_SESSION_BUILT && sessionLibAvailable(),
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): MeshSetupView {
    val present = buildSet {
        if (state.hasSessionKeys) add(MeshCredential.SessionKeys)
        if (state.hasPeer) add(MeshCredential.HostRadioKey)
    }
    val resolution = resolveRoute(
        MeshRoute.Mesh, platform,
        hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
        sessionBuilt = sessionBuilt, terminalBuilt = terminalBuilt,
    )
    val exists = MeshRoute.Mesh in availableRoutes(platform)
    return MeshSetupView(present, resolution, exists)
}

/** Why a scanned ldmux code did not give the mesh route its keys. */
enum class MeshPairingProblem { NotAnLdmuxCode, NoMeshKeys, LibraryUnavailable, StoreFailed }

sealed interface MeshPairingScan {
    data class Enrolled(val meshOnly: Boolean) : MeshPairingScan
    data class Refused(val problem: MeshPairingProblem) : MeshPairingScan
}

/**
 * Take a scanned ldmux pairing code for the mesh route. Either kind works (`--mesh`, with the
 * tailnet pairing, or `--mesh-only`); a code with no mesh keys is refused here, since this entry
 * point is for the mesh route. The tailnet pairing in a full code is applied as it always was.
 */
fun applyMeshPairingScan(text: String, store: ConfigStore): MeshPairingScan {
    val p = parseScannedPairing(text) ?: return MeshPairingScan.Refused(MeshPairingProblem.NotAnLdmuxCode)
    if (p.mesh == null) return MeshPairingScan.Refused(MeshPairingProblem.NoMeshKeys)
    return when (store.pair(p)) {
        MeshEnrollOutcome.Enrolled -> MeshPairingScan.Enrolled(meshOnly = p.isMeshOnly)
        MeshEnrollOutcome.LibraryUnavailable -> MeshPairingScan.Refused(MeshPairingProblem.LibraryUnavailable)
        MeshEnrollOutcome.StoreFailed -> MeshPairingScan.Refused(MeshPairingProblem.StoreFailed)
        MeshEnrollOutcome.NoMeshKeys -> MeshPairingScan.Refused(MeshPairingProblem.NoMeshKeys)
    }
}

fun meshPairingLine(p: MeshPairingProblem): String = when (p) {
    MeshPairingProblem.NotAnLdmuxCode -> "not an ldmux code"
    MeshPairingProblem.NoMeshKeys -> "that code has no mesh keys (use ldmux code --mesh-only)"
    MeshPairingProblem.LibraryUnavailable -> "the session library is not available on this device"
    MeshPairingProblem.StoreFailed -> "could not store the keys"
}

/** One credential as a settings line. */
fun meshCredentialName(c: MeshCredential): String = when (c) {
    MeshCredential.SessionKeys -> "session keys"
    MeshCredential.HostRadioKey -> "host LoRa radio key"
}

fun meshCredentialLine(c: MeshCredential): String = "\u2717 " + meshCredentialName(c)
