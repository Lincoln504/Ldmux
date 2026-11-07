package io.ldmux.mobile.mesh

/**
 * The credentials a mesh attach needs, all read from secure storage.
 *
 * The MeshMore gateway token is gone. The radio route needs the L3 session keys
 * and the host radio public key; the phone owns the BLE link directly.
 */
class MeshCredentials(val keys: MeshSessionKeys, val hostRadio: ByteArray)

/** Why there are no [MeshCredentials]: what resolveRoute should be told. */
data class MeshCredentialState(val hasSessionKeys: Boolean, val hasPeer: Boolean) {
    val complete: Boolean get() = hasSessionKeys && hasPeer
}

/** What is stored, without ever throwing: a broken Keystore reads as "needs pairing". */
fun readMeshCredentials(keys: MeshSessionKeyStore): Pair<MeshCredentialState, MeshCredentials?> {
    val k = runCatching { keys.load() }.getOrNull()
    val state = MeshCredentialState(hasSessionKeys = k != null, hasPeer = k?.hostRadio != null)
    val creds = if (k != null && k.hostRadio != null) MeshCredentials(k, k.hostRadio) else null
    return state to creds
}

fun meshTermLabel(view: MeshTermView): String = when (view) {
    MeshTermView.Idle -> "mesh · not attached"
    is MeshTermView.LinkDown -> meshLinkLabel(view.link)
    // "Can take minutes" was the old truth (a single msg1 and a five-minute timeout). A handshake
    // is three small records and is now retried every six seconds, so the honest label is the
    // state, not a warning about how long it might take.
    MeshTermView.Handshaking -> "mesh · securing link"
    MeshTermView.ProfileMissing -> "mesh · linked, no terminal yet"
    MeshTermView.Listing -> "mesh · listing shells"
    // **The state, not an action.** `open on the desktop` was the label for the button that takes
    // the shell back, printed where the header names where the shell *is*. The overlay under it
    // already says `in use on X`; the header said something else about the same fact.
    is MeshTermView.HeldBy -> "mesh · in use on radio link"
    is MeshTermView.Choose -> "mesh · choose a shell"
    MeshTermView.NoShells -> "mesh · host runs no shell"
    is MeshTermView.Live -> when {
        view.resyncing -> "mesh · resyncing screen"
        !view.painted -> "mesh · attached, no screen yet"
        view.stalled -> "mesh · waiting for send budget"
        else -> "mesh · live (reduced)"
    }
    is MeshTermView.HostClosed -> when (view.reason) {
        null -> "mesh · host ended session"
        MeshTerm.Closed.NOT_FOUND -> "mesh · no such shell"
        MeshTerm.Closed.EXITED -> "mesh · shell exited"
        MeshTerm.Closed.UNAVAILABLE -> "mesh · host could not read screen"
        MeshTerm.Closed.VERSION -> "mesh · host speaks another version"
        else -> "mesh · host closed terminal (${view.reason})"
    }
}

/**
 * What is wrong with a saved mesh route right now, read from secure storage; null when it can
 * run. Never throws. The launcher uses it so the line under the title names the real next step
 * (scan a code, choose a radio) and not a guess.
 */
fun currentMeshResolution(route: MeshRoute): MeshRouteResolution {
    val (state, _) = readMeshCredentials(meshSessionKeyStore())
    return resolveRoute(
        route,
        hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
    )
}

/** Everything the mesh screen needs: the credentials, the library, and the selected radio. */
class MeshScreenPlan(
    val creds: MeshCredentials,
    val lib: SessionLib,
    /** The BLE address selected for this install; empty means no radio has been chosen. */
    val radioAddress: String = "",
)

/**
 * What to show the mesh terminal with, or null when the saved route is not mesh or cannot run (in
 * which case the caller shows the normal app and its stated reason). **Not gated on the tailnet
 * pairing**: a mesh-only device has no daemon host or token and is still a complete mesh pairing.
 * Tailnet configs return null without touching secure storage.
 */
fun meshScreenFor(config: io.ldmux.mobile.LdmuxConfig): MeshScreenPlan? {
    if (config.route != MeshRoute.Mesh) return null
    return meshScreenPlan(config, meshSessionKeyStore(), loadSessionLib())
}

/** [meshScreenFor] with its inputs passed in, so the rule is testable without a Keystore. */
fun meshScreenPlan(
    config: io.ldmux.mobile.LdmuxConfig,
    keys: MeshSessionKeyStore,
    lib: SessionLib?,
    terminalBuilt: Boolean = MESH_TERMINAL_BUILT,
): MeshScreenPlan? {
    if (config.route != MeshRoute.Mesh) return null
    val (state, creds) = readMeshCredentials(keys)
    if (lib == null) return null
    val resolution = resolveRoute(
        config.route,
        hasPeer = state.hasPeer, hasSessionKeys = state.hasSessionKeys,
        sessionBuilt = MESH_SESSION_BUILT, terminalBuilt = terminalBuilt,
    )
    return if (resolution is MeshRouteResolution.Use && creds != null) {
        MeshScreenPlan(creds, lib, config.meshRadio)
    } else null
}
