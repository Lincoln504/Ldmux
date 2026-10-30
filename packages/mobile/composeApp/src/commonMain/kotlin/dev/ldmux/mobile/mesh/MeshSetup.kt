package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.ConfigStore
import dev.ldmux.mobile.LdmuxConfig
import dev.ldmux.mobile.parseScannedPairing

/** The three things a mesh attach needs, each of which can be missing on its own. */
enum class MeshCredential { SessionKeys, GatewayToken, HostRadioKey }

/**
 * What the route setup screen shows, as one pure value: which credentials are present, what the
 * saved route would do right now, and which routes the user may choose. The screen draws this
 * and decides nothing.
 */
data class MeshSetupView(
    val present: Set<MeshCredential>,
    /** Missing credentials, in the order the user should supply them. */
    val missing: List<MeshCredential>,
    /** What a mesh route would resolve to with these credentials on this device. */
    val meshResolution: MeshRouteResolution,
    /** Routes the picker offers. A route that cannot work is absent, not greyed in by default. */
    val choosable: List<MeshRoute>,
    /** Whether a mesh route could be offered at all on this platform (false on iOS: the section is hidden). */
    val meshExists: Boolean,
)

/** The setup view for [config] given what secure storage holds. Pure: pass the facts in. */
fun meshSetupView(
    config: LdmuxConfig,
    state: MeshCredentialState,
    platform: MobilePlatform = dev.ldmux.mobile.mesh.currentPlatform,
    sessionBuilt: Boolean = MESH_SESSION_BUILT && sessionLibAvailable(),
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): MeshSetupView {
    val present = buildSet {
        if (state.hasSessionKeys) add(MeshCredential.SessionKeys)
        if (state.hasToken) add(MeshCredential.GatewayToken)
        if (state.hasPeer) add(MeshCredential.HostRadioKey)
    }
    // The order is the order of the ceremony: the ldmux code first (the gateway token is refused
    // without it), then MeshMore's code. The radio key rides on the ldmux code too.
    val missing = listOf(MeshCredential.SessionKeys, MeshCredential.HostRadioKey, MeshCredential.GatewayToken)
        .filter { it !in present }
    val resolution = resolveRoute(
        MeshRoute.Mesh, platform,
        hasToken = state.hasToken, hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
        sessionBuilt = sessionBuilt, terminalBuilt = terminalBuilt,
    )
    val exists = MeshRoute.Mesh in availableRoutes(platform)
    val choosable = buildList {
        // The tailnet is choosable only when there is a daemon to reach: a mesh-only device has none.
        if (config.isPaired) add(MeshRoute.Tailnet)
        if (exists && resolution is MeshRouteResolution.Use) add(MeshRoute.Mesh)
    }
    return MeshSetupView(present, missing, resolution, choosable, exists)
}

/**
 * [config] with [route] chosen, or null when the picker must not save it: a route that cannot run
 * is never persisted, so the saved route is always one that was usable when it was chosen.
 */
fun chooseRoute(config: LdmuxConfig, route: MeshRoute, view: MeshSetupView): LdmuxConfig? =
    if (route in view.choosable) config.copy(route = route) else null

/** Why a scanned gateway code was not taken. */
enum class GatewayScanProblem {
    /** Not a MeshMore gateway code (neither the structured payload nor a bare token). */
    NotAGatewayCode,

    /** No session keys yet: scanning the ldmux code comes first, and a token alone would be a half-pairing. */
    NeedsSessionPairing,

    /** Secure storage refused the write. */
    StoreFailed,
}

sealed interface GatewayScan {
    data class Saved(val config: LdmuxConfig) : GatewayScan
    data class Refused(val problem: GatewayScanProblem) : GatewayScan
}

/**
 * Take a scanned MeshMore gateway code: refused without a session pairing, otherwise the token goes
 * to secure storage and the endpoint (when it is not the default) into [config]. It writes the
 * token **last**, so a failure leaves no half-state.
 */
fun applyGatewayScan(text: String, config: LdmuxConfig, keys: MeshSessionKeyStore, tokens: MeshTokenStore): GatewayScan {
    val code = parseGatewayCode(text) ?: return GatewayScan.Refused(GatewayScanProblem.NotAGatewayCode)
    val haveKeys = runCatching { keys.load() }.getOrNull() != null
    if (!haveKeys) return GatewayScan.Refused(GatewayScanProblem.NeedsSessionPairing)
    try {
        tokens.save(code.token)
    } catch (_: Exception) {
        return GatewayScan.Refused(GatewayScanProblem.StoreFailed)
    }
    val stored = if (code.endpoint == MeshGatewayEndpoint.DEFAULT) "" else code.endpoint.toString()
    return GatewayScan.Saved(config.copy(meshGateway = stored))
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

fun gatewayScanLine(p: GatewayScanProblem): String = when (p) {
    GatewayScanProblem.NotAGatewayCode -> "not a MeshMore gateway code"
    GatewayScanProblem.NeedsSessionPairing -> "scan the ldmux pairing code first (ldmux code --mesh-only)"
    GatewayScanProblem.StoreFailed -> "could not store the token"
}

fun meshPairingLine(p: MeshPairingProblem): String = when (p) {
    MeshPairingProblem.NotAnLdmuxCode -> "not an ldmux code"
    MeshPairingProblem.NoMeshKeys -> "that code has no mesh keys (use ldmux code --mesh-only)"
    MeshPairingProblem.LibraryUnavailable -> "the session library is not available on this device"
    MeshPairingProblem.StoreFailed -> "could not store the keys"
}

/** One credential as a settings line. */
fun meshCredentialLine(c: MeshCredential, present: Boolean): String {
    val name = when (c) {
        MeshCredential.SessionKeys -> "session keys"
        MeshCredential.GatewayToken -> "MeshMore gateway token"
        MeshCredential.HostRadioKey -> "host radio key"
    }
    return "${if (present) "✓" else "✗"} $name"
}
